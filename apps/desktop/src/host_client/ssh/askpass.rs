//! askpass 的桌面这一头：开一个只有自己能进的 socket，ssh 调起的 runode（`runode_ssh::askpass`）连上来交
//! 提示，这里按口令（`register` 给每次连接的）认出是哪台主机，交给界面弹框（`RemoteEvent::Ask`），等用户答。

use std::{
    collections::{HashMap, VecDeque},
    io::{self, BufReader, Read as _},
    path::PathBuf,
    sync::{LazyLock, Mutex, OnceLock, PoisonError},
    thread,
};

use runode_types::random::random_hex;

use futures::channel::oneshot;
use runode_ssh::askpass::{Kind, Reply, Request, read_request, write_reply};

use super::{RemoteEvent, emit};

/// 一次询问，交给界面；答了（或者丢掉，算取消）时回给 ssh。
#[derive(Debug)]
pub struct Ask {
    /// 哪台主机在问。
    pub host: String,
    pub kind: Kind,
    pub prompt: String,
    reply: Option<oneshot::Sender<Reply>>,
    /// 询问该收起了：askpass 进程结束了（ssh 办完提示的事，或者等不到回答、放弃了）。
    pub dismissed: Option<oneshot::Receiver<()>>,
}

impl Ask {
    /// 回答；`None` 是取消。
    pub fn answer(mut self, reply: Reply) {
        if let Some(sender) = self.reply.take() {
            let _ = sender.send(reply);
        }
    }
}

impl Drop for Ask {
    fn drop(&mut self) {
        if let Some(sender) = self.reply.take() {
            let _ = sender.send(None);
        }
    }
}

struct Server {
    socket: PathBuf,
}

static SERVER: OnceLock<Option<Server>> = OnceLock::new();
static TOKENS: LazyLock<Mutex<Tokens>> = LazyLock::new(Mutex::default);

/// 最多记着这么多个口令，再发新的就忘掉最早发的：同时在问用户的 ssh 不会有这么多。
const MAX_TOKENS: usize = 64;

/// 发出去的口令，按口令记是哪台主机。
#[derive(Default)]
struct Tokens {
    hosts: HashMap<String, String>,
    /// 按发出的先后。
    order: VecDeque<String>,
}

impl Tokens {
    fn insert(&mut self, token: String, host: String) {
        if self.order.len() >= MAX_TOKENS
            && let Some(oldest) = self.order.pop_front()
        {
            self.hosts.remove(&oldest);
        }
        self.order.push_back(token.clone());
        self.hosts.insert(token, host);
    }

    fn host(&self, token: &str) -> Option<String> {
        self.hosts.get(token).cloned()
    }
}

/// 给 `host` 的一次连接要一个口令，返回 socket 的路径和口令；开不了 socket 时为空，ssh 只能不问用户。
pub(super) fn register(host: &str) -> Option<(PathBuf, String)> {
    let server = SERVER.get_or_init(start).as_ref()?;
    let token = random_hex::<16>().inspect_err(|err| tracing::warn!("cannot pick an askpass token: {err}")).ok()?;
    TOKENS.lock().unwrap_or_else(PoisonError::into_inner).insert(token.clone(), host.to_owned());
    Some((server.socket.clone(), token))
}

/// ssh 当 askpass 调的程序：这个可执行文件；Windows 上它是界面程序，stdout 不可靠，用旁边的控制台程序 `rn`。
pub(super) fn program() -> io::Result<PathBuf> {
    // 测试程序自己不认 askpass 的模式，测试指一个编好的 runode 或 `rn`。
    #[cfg(test)]
    if let Some(exe) = std::env::var_os("RUNODE_TEST_ASKPASS") {
        return Ok(exe.into());
    }
    let exe = runode_paths::runode_exe()?;
    #[cfg(windows)]
    let exe = Some(exe.with_file_name("rn.exe")).filter(|rn| rn.is_file()).unwrap_or(exe);
    Ok(exe)
}

#[cfg(unix)]
type Listener = std::os::unix::net::UnixListener;
#[cfg(windows)]
type Listener = uds_windows::UnixListener;

fn start() -> Option<Server> {
    let dirs = runode_paths::Dirs::from_env();
    let opened = dirs.create_runtime_dir().ok().and_then(|_| dirs.askpass_socket_file(std::process::id()));
    let Some(socket) = opened else {
        tracing::warn!("no place for the askpass socket; ssh cannot ask for passwords");
        return None;
    };
    let _ = std::fs::remove_file(&socket);
    let listener = match Listener::bind(&socket) {
        Ok(listener) => listener,
        Err(err) => {
            tracing::warn!("cannot open the askpass socket {}: {err}", socket.display());
            return None;
        }
    };
    let spawned = thread::Builder::new().name("ssh-askpass".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = thread::Builder::new().name("ssh-askpass-ask".into()).spawn(move || serve(stream));
        }
    });
    spawned.ok().map(|_| Server { socket })
}

#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type Stream = uds_windows::UnixStream;

fn serve(stream: Stream) {
    let request = match read_request(&mut BufReader::new(&stream)) {
        Ok(request) => request,
        Err(err) => {
            tracing::debug!("unreadable askpass request: {err}");
            return;
        }
    };
    let Request { token, kind, prompt } = request;
    let host = TOKENS.lock().unwrap_or_else(PoisonError::into_inner).host(&token);
    let Some(host) = host else {
        tracing::warn!("askpass request with an unknown token, refusing it");
        let _ = write_reply(&mut &stream, &None);
        return;
    };
    let (reply, answer) = oneshot::channel();
    let (dismiss, dismissed) = oneshot::channel();
    // ssh 办完提示的事、或者认证超时放弃了，就结束 askpass 进程，连接随之关掉；回答以后 askpass 读完回话也
    // 关掉，那时界面上已经收起了。
    if let Ok(mut watch) = stream.try_clone() {
        let _ = thread::Builder::new().name("ssh-askpass-watch".into()).spawn(move || {
            let _ = watch.read(&mut [0; 1]);
            let _ = dismiss.send(());
        });
    }
    emit(RemoteEvent::Ask(Ask { host, kind, prompt, reply: Some(reply), dismissed: Some(dismissed) }));
    let reply = futures::executor::block_on(answer).unwrap_or(None);
    let _ = write_reply(&mut &stream, &reply);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 记满了再发新的，最早发的口令认不出了，新的认得出。
    #[test]
    fn askpass_tokens_forget_the_oldest_past_the_limit() {
        let mut tokens = Tokens::default();
        for at in 0..=MAX_TOKENS {
            tokens.insert(format!("t{at}"), format!("host{at}"));
        }
        assert_eq!(tokens.host("t0"), None);
        assert_eq!(tokens.host("t1").as_deref(), Some("host1"));
        assert_eq!(tokens.host(&format!("t{MAX_TOKENS}")), Some(format!("host{MAX_TOKENS}")));
        assert_eq!(tokens.hosts.len(), MAX_TOKENS);
    }
}
