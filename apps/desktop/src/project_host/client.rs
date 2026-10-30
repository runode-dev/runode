//! 桌面这头：每台 SSH 主机一个 `runode --ssh-project`，经主连接起，按需重起。请求带编号，回话由读线程按
//! 编号交给等着的一方，几个请求可以同时在路上。主机还没连上时不起，免得在这里另外问用户要密码。

use std::{
    collections::HashMap,
    io::BufReader,
    process::{ChildStdin, Stdio},
    sync::{
        Arc, LazyLock, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use super::wire::{self, Call, Reply, Request, Response};
use crate::host_client::remote::{self, Status, deploy};

static CLIENTS: LazyLock<Mutex<HashMap<String, Arc<Client>>>> = LazyLock::new(Default::default);

type Pending = Mutex<HashMap<u64, mpsc::Sender<(Reply, Vec<u8>)>>>;

pub(crate) struct Client {
    name: String,
    conn: Mutex<Option<Arc<Conn>>>,
}

struct Conn {
    input: Mutex<ChildStdin>,
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

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// 发一个请求，等回话，最多等 `timeout`。会阻塞。
    pub(super) fn call(&self, call: Call, timeout: Duration) -> Result<(Reply, Vec<u8>), String> {
        self.call_with(call, &[], timeout)
    }

    /// 发一个后面跟着 `content` 的请求，等回话。会阻塞。
    pub(super) fn call_with(&self, call: Call, content: &[u8], timeout: Duration) -> Result<(Reply, Vec<u8>), String> {
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
        match answer.recv_timeout(timeout) {
            Ok((Reply::Failed(error), _)) => Err(error),
            Ok(reply) => Ok(reply),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                conn.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
                Err(format!("{} did not answer in time", self.name))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(format!("lost the connection to {}", self.name)),
        }
    }

    /// 连着的那一个；没有或断了时起一个新的。
    fn conn(&self) -> Result<Arc<Conn>, String> {
        let mut slot = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(conn) = slot.as_ref().filter(|conn| !conn.closed.load(Ordering::Relaxed)) {
            return Ok(conn.clone());
        }
        let host = remote::remote(&self.name).ok_or_else(|| format!("no such host: {}", self.name))?;
        if !matches!(host.status(), Status::Connected(_)) {
            return Err(format!("not connected to {}", self.name));
        }
        let ssh = remote::ssh();
        let script = runode_ssh::bootstrap::project_command(&deploy::install_dir());
        let mut command = ssh.exec(&host.dest, &script, true);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        crate::subprocess::hide_window(&mut command);
        let mut child = command.spawn().map_err(|err| format!("cannot run ssh: {err}"))?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("ssh gave no pipes".into());
        };
        let pending = Arc::<Pending>::default();
        let closed = Arc::new(AtomicBool::new(false));
        let reader = {
            let (pending, closed, name) = (pending.clone(), closed.clone(), self.name.clone());
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
                let _ = child.wait();
            }
        };
        thread::Builder::new()
            .name("project-client".into())
            .spawn(reader)
            .map_err(|err| format!("cannot start a thread: {err}"))?;
        let conn = Arc::new(Conn { input: Mutex::new(input), pending, next: AtomicU64::new(0), closed });
        *slot = Some(conn.clone());
        Ok(conn)
    }
}
