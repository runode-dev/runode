//! 桌面这头：每台 SSH 主机一个 `runode --ssh-project`，经主连接起，按需重起。请求带编号，回话由读线程按
//! 编号交给等着的一方，几个请求可以同时在路上。主机连上以后才起：经主连接时不用再认证，没有主连接
//! （Windows）时这条 ssh 自己认证，要问用户时经 askpass 问。

use std::{
    collections::HashMap,
    io::{BufReader, Read, Write},
    process::Stdio,
    sync::{
        Arc, LazyLock, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use super::wire::{self, Call, Reply, Request, Response};
use crate::host_client::remote::{self, Status, deploy};

/// 等回话时每隔这么久看一次要不要停。
const CANCEL_POLL: Duration = Duration::from_millis(50);

static CLIENTS: LazyLock<Mutex<HashMap<String, Arc<Client>>>> = LazyLock::new(Default::default);

type Pending = Mutex<HashMap<u64, mpsc::Sender<(Reply, Vec<u8>)>>>;

pub(crate) struct Client {
    name: String,
    conn: Mutex<Option<Arc<Conn>>>,
}

struct Conn {
    input: Mutex<Box<dyn Write + Send>>,
    pending: Arc<Pending>,
    next: AtomicU64,
    closed: Arc<AtomicBool>,
}

impl Client {
    /// 主机 `name` 的那一个。
    pub(crate) fn of(name: &str) -> Arc<Self> {
        let mut clients = CLIENTS.lock().unwrap_or_else(PoisonError::into_inner);
        clients
            .entry(name.to_owned())
            .or_insert_with(|| Arc::new(Self { name: name.to_owned(), conn: Mutex::default() }))
            .clone()
    }

    /// 忘掉主机 `name` 的那一个；还在用它的请求照样收到回话或断开。
    pub(crate) fn forget(name: &str) {
        CLIENTS.lock().unwrap_or_else(PoisonError::into_inner).remove(name);
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// 经 `input`、`output` 和同一个进程里的项目服务说话的一个，不经 ssh，测试用。
    #[cfg(test)]
    pub(super) fn over(
        name: &str,
        input: impl Write + Send + 'static,
        output: impl Read + Send + 'static,
    ) -> Arc<Self> {
        let conn = Conn::start(name, Box::new(input), output, || {}).expect("start the reader");
        Arc::new(Self { name: name.to_owned(), conn: Mutex::new(Some(conn)) })
    }

    /// 发一个请求，等回话，最多等 `timeout`。会阻塞。
    pub(super) fn call(&self, call: Call, timeout: Duration) -> Result<(Reply, Vec<u8>), String> {
        self.call_with(call, &[], timeout, None)
    }

    /// 发一个请求，等回话，`cancel` 置位时不再等。会阻塞。
    pub(super) fn call_cancellable(
        &self,
        call: Call,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<(Reply, Vec<u8>), String> {
        self.call_with(call, &[], timeout, Some(cancel))
    }

    /// 发一个后面跟着 `content` 的请求，等回话，最多等 `timeout`；给了 `cancel` 时它一置位就不再等，并让远端
    /// 也停下（`Call::Cancel`），远端随后交回的结果丢掉。会阻塞。
    pub(super) fn call_with(
        &self,
        call: Call,
        content: &[u8],
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<(Reply, Vec<u8>), String> {
        let conn = self.conn()?;
        let id = conn.next.fetch_add(1, Ordering::Relaxed);
        let (sender, answer) = mpsc::channel();
        conn.pending.lock().unwrap_or_else(PoisonError::into_inner).insert(id, sender);
        let written = wire::write(
            &mut *conn.input.lock().unwrap_or_else(PoisonError::into_inner),
            &Request { id, call, blob: content.len() },
            content,
        );
        if let Err(err) = written {
            conn.closed.store(true, Ordering::Relaxed);
            conn.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            return Err(format!("lost the connection to {}: {err}", self.name));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let wait = if cancel.is_some() { left.min(CANCEL_POLL) } else { left };
            match answer.recv_timeout(wait) {
                Ok((Reply::Failed(error), _)) => return Err(error),
                Ok(reply) => return Ok(reply),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("lost the connection to {}", self.name));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let cancelled = cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed));
            if !cancelled && !left.is_zero() {
                continue;
            }
            conn.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            if !cancelled {
                return Err(format!("{} did not answer in time", self.name));
            }
            let stop = Request { id: conn.next.fetch_add(1, Ordering::Relaxed), call: Call::Cancel { id }, blob: 0 };
            if wire::write(&mut *conn.input.lock().unwrap_or_else(PoisonError::into_inner), &stop, &[]).is_err() {
                conn.closed.store(true, Ordering::Relaxed);
            }
            return Err("cancelled".into());
        }
    }

    /// 连着的那一个；没有或断了时起一个新的。
    fn conn(&self) -> Result<Arc<Conn>, String> {
        let mut slot = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(conn) = slot.as_ref().filter(|conn| !conn.closed.load(Ordering::Relaxed)) {
            return Ok(conn.clone());
        }
        let host = remote::find(&self.name).ok_or_else(|| format!("no such host: {}", self.name))?;
        if !matches!(host.status(), Status::Connected(_)) {
            return Err(format!("not connected to {}", self.name));
        }
        let script = runode_ssh::bootstrap::project_command(&deploy::install_dir());
        let mut command = host.exec(&remote::ssh(), &script, true, false);
        command.stderr(Stdio::null());
        crate::subprocess::hide_window(&mut command);
        let pipes = runode_ssh::process::Pipes { stdin: true, stdout: true, stderr: false };
        let piped = runode_ssh::process::spawn(&mut command, pipes).map_err(|err| format!("cannot run ssh: {err}"))?;
        let mut child = piped.child;
        let (Some(input), Some(output)) = (piped.stdin, piped.stdout) else {
            return Err("ssh gave no pipes".into());
        };
        let conn = Conn::start(&self.name, Box::new(input), output, move || {
            let _ = child.wait();
        })?;
        *slot = Some(conn.clone());
        Ok(conn)
    }
}

impl Conn {
    /// 经 `input` 发请求，起读线程从 `output` 读回话、按编号交给等着的一方；读到结尾时调 `finished`。
    fn start(
        name: &str,
        input: Box<dyn Write + Send>,
        output: impl Read + Send + 'static,
        finished: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Self>, String> {
        let pending = Arc::<Pending>::default();
        let closed = Arc::new(AtomicBool::new(false));
        let reader = {
            let (pending, closed, name) = (pending.clone(), closed.clone(), name.to_owned());
            move || {
                let mut output = BufReader::new(output);
                loop {
                    let response = match wire::read_line::<Response>(&mut output) {
                        Ok(Some(response)) => response,
                        Ok(None) => break,
                        Err(err) => {
                            tracing::warn!("bad answer from the project service on {name}: {err}");
                            break;
                        }
                    };
                    let Ok(blob) = wire::read_blob(&mut output, response.blob) else {
                        break;
                    };
                    let waiting = pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&response.id);
                    if let Some(waiting) = waiting {
                        let _ = waiting.send((response.reply, blob));
                    }
                }
                // 丢掉等着的一方，它们马上收到断开。
                closed.store(true, Ordering::Relaxed);
                pending.lock().unwrap_or_else(PoisonError::into_inner).clear();
                finished();
            }
        };
        thread::Builder::new()
            .name("project-client".into())
            .spawn(reader)
            .map_err(|err| format!("cannot start a thread: {err}"))?;
        Ok(Arc::new(Self { input: Mutex::new(input), pending, next: AtomicU64::new(0), closed }))
    }
}
