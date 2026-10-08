//! 连接上的前端：别的进程经 `Host::listen` 开的 Unix socket 连上来，同一个进程里的桌面经
//! `Host::connect_pair` 拿一对 socket 的一端，两种连接走同一个 `serve`，按 `runode_protocol` 的帧
//! 收发，读和写各一个线程。
//!
//! 读的线程先等 `Hello`，回 `Welcome`（协议版本对不上时回 `Incompatible` 后断开），之后把控制
//! 消息和输入帧转给宿主。写的线程按到达的先后把帧写出去：连上的会话在自己的线程里把事件交给
//! 这条连接的 `Outbox`，一个会话的帧在连接上的先后就是它发生的先后。前端读得太慢、积压超过
//! `OUTBOX_LIMIT` 时，那个会话不再往这条连接发输出，改发 `Resync`，前端重新 `Attach`。转给
//! 连接的输出里，shell 集成的报告抹掉了内容（见 `ReportRedactor`），报告带的口令不出宿主。
//!
//! 读的线程不等会话线程回话：列会话、读屏幕先在读的线程里把请求按先后送到会话线程，再起一个
//! 短命的线程等回话、交给 `Outbox`，同一条连接上之后的输入照常转发。一条连接上这样在等的请求
//! 最多 `MAX_WAITING` 个，再多的当场回 `Error`，前端狂发也堆不起线程。开会话、列目录
//! （`ClientMsg::ListDirs`）和列项目命令（`ClientMsg::ListProjectTasks`）还在读的线程里办（开伪终端、
//! 启动 shell 要几毫秒，读一次目录项、几个小文件更快）。
//!
//! 能连上 socket 就能读写所有终端：socket 放在只有自己能进的目录里（见调用方建目录的方式），
//! 连上来的进程也要是同一个用户的。

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{self, BufReader, BufWriter, Read as _, Write as _},
    net::Shutdown,
    os::unix::{
        fs::{OpenOptionsExt as _, PermissionsExt as _},
        io::AsRawFd as _,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use runode_protocol::{
    AttachMode, ClientKind, ClientMsg, Frame, FrameError, FrameKind, GoodbyeReason, HANDOFF_FORMAT, HostMsg,
    PROTOCOL_VERSION, SessionId, SessionInfo, git::GitRequest, read_frame, write_frame,
};
use runode_shared_types::{grid::GridSize, input::parse_keys, session::DriveAction};
use runode_terminal::pty;

use crate::{
    Host, Shared, SpawnOptions, Stopped, browse,
    git::{GitWorker, Job},
    handoff, project_tasks,
    session::{Drive, Event, EventSink, Inbox, Screen, Subscribe},
};

/// 快照分成这么大的帧发，不必一帧装下整份（上限见 `runode_protocol::MAX_PAYLOAD`）。
const SNAPSHOT_CHUNK: usize = 1 << 20;
/// 一条连接最多积压这么多字节还没写出去，再多就让输出最多的会话改发 `Resync`。
const OUTBOX_LIMIT: usize = 32 << 20;
/// 等会话线程回话（列会话、读屏幕）的最长时间。
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// 一条连接上最多同时有这么多列会话、读屏幕、读写 git 的请求在等回话，见 `Waiting`。
const MAX_WAITING: usize = 16;
/// 接受连接出错（比如文件描述符用完了）后等一会儿再接，免得空转。
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
/// 每条连接 socket 的收发缓冲。macOS 上默认只有 8 KiB，刷屏的输出一块就塞满，读写两边来回
/// 切换的次数多到吞吐上不去；内核允许的上限（`kern.ipc.maxsockbuf`）是 8 MiB。
const SOCKET_BUFFER: libc::c_int = 4 << 20;
/// 写的线程一次最多攒这么多字节再写出去。
const WRITE_BUFFER: usize = 256 << 10;

/// 连着的前端、转给界面还没回话的请求，以及监听和退出的状态。
pub(crate) struct Peers {
    /// 连着的连接，按编号；`out` 在连接的写线程起好之前为 `None`。
    connections: HashMap<u64, Peer>,
    /// 说自己是 `ClientKind::Desktop` 的连接，按 `Hello` 的先后；最后一个是现在的界面。
    desktops: Vec<u64>,
    /// 转给界面、还没回话的请求，按 `HostMsg::UiRequest::ui`。
    pending: HashMap<u64, Pending>,
    next_ui: u64,
    /// 还接新连接；退出时在判断要退出的同一把锁里置为 false。
    pub(crate) accepting: bool,
    /// `Host::listen` 开的 socket 和拿着的锁。
    pub(crate) listening: Option<Listening>,
    /// 宿主单独一个进程在跑，见 `Host::run_until_idle`：收到 `Shutdown` 时连宿主一起退出。
    /// `HostMsg::Welcome::standalone` 报的就是它。
    pub(crate) standalone: bool,
    /// 要退出了，为什么。
    pub(crate) stop: Option<Stopped>,
    /// 正在把会话交给新宿主：那条连接的编号，见 `handoff::give`。这期间新来的连接收到
    /// `Goodbye { Handoff }`，也不开新会话。
    pub(crate) handoff: Option<u64>,
    /// 跑在 app 里的宿主要在 app 退出时交出会话，见 `Host::yield_on_quit`。
    pub(crate) yielding: bool,
    /// 最近一次有连接连上或者断开的时刻，空闲从这时起算。
    pub(crate) activity_at: Instant,
    /// 没有会话也没有连接时也不算空闲，见 `Host::set_stay_up`。
    pub(crate) stay_up: bool,
}

impl Default for Peers {
    fn default() -> Self {
        Self {
            connections: HashMap::new(),
            desktops: Vec::new(),
            pending: HashMap::new(),
            next_ui: 1,
            accepting: true,
            listening: None,
            standalone: false,
            stop: None,
            handoff: None,
            yielding: false,
            activity_at: Instant::now(),
            stay_up: false,
        }
    }
}

impl Peers {
    pub(crate) fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// 有桌面的界面连着。
    pub(crate) fn has_desktop(&self) -> bool {
        !self.desktops.is_empty()
    }

    /// 给 `except` 以外的连接都发 `Goodbye`，写完后断开；`spare_desktops` 时桌面的界面也不发。
    /// 写线程还没起好的连接起好时自己发，见 `serve`。
    pub(crate) fn say_goodbye(&self, except: Option<u64>, spare_desktops: bool, reason: &GoodbyeReason) {
        for (&id, peer) in &self.connections {
            if Some(id) != except
                && !(spare_desktops && self.desktops.contains(&id))
                && let Some(out) = &peer.out
            {
                out.control(&HostMsg::Goodbye { reason: reason.clone() });
                out.close();
            }
        }
    }
}

struct Peer {
    out: Option<Outbox>,
}

/// 转给界面、还没回话的一条请求。
struct Pending {
    /// 转给了哪条界面的连接；只认它回的话。
    desktop: u64,
    /// 回话交给谁。
    asker: Asker,
}

/// 请界面办事的一方。
enum Asker {
    /// 别的连接发来的请求（`Open`、`Reveal`、`Layout`）。
    Client {
        /// 发请求的那条连接的编号；它断开时这条请求跟着撤掉。
        from: u64,
        /// 发请求的那条连接，回话交给它。
        origin: Outbox,
        req: u32,
    },
    /// 会话自己的请求（读写剪贴板），回话交给会话线程（`Inbox::UiAnswer`）。
    Session(SessionId),
}

/// 会话线程请界面办事的一头，见 `Shared::ask_ui`。拿着宿主的弱引用：
/// 会话线程不该让宿主留着不放，宿主没了时什么都办不成。
#[derive(Clone, Default)]
pub(crate) struct UiPort(Weak<Shared>);

impl UiPort {
    pub(crate) fn new(shared: Weak<Shared>) -> Self {
        Self(shared)
    }

    /// 见 `Shared::ask_ui`。
    pub(crate) fn ask(&self, session: SessionId, preferred: Option<u64>, request: ClientMsg) -> Option<u64> {
        self.0.upgrade()?.ask_ui(session, preferred, request)
    }
}

/// `Host::listen` 开着的 socket。丢掉时放开锁，socket 文件由退出的一方删。
pub(crate) struct Listening {
    pub(crate) socket: PathBuf,
    /// 监听的 socket，和接受连接的线程共用；交接时把它的描述符交给新宿主。
    pub(crate) listener: Arc<UnixListener>,
    /// 锁住了的锁文件，交接时连同描述符交给新宿主。
    pub(crate) lock: File,
    /// 让接受连接的线程停下、接着接或者结束。
    pub(crate) control: Arc<ListenControl>,
}

/// 接受连接的线程该做什么，见 `ListenControl`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenState {
    Accepting,
    /// 不接，来的连接排在 backlog 里：交接期间，连接留给接手的新宿主或者回滚后的自己。
    Paused,
    /// 结束线程，之后不再变。只关自己这份描述符，不 `shutdown` 监听的 socket：交接后新宿主
    /// 用的是同一个打开的 socket。
    Stopped,
}

/// 管接受连接的线程：线程 `poll` 监听的 socket 和一根唤醒管道，改状态时往管道里写一个字节
/// 叫醒它。
pub(crate) struct ListenControl {
    /// 线程接连接时一直拿着它，所以 `pause`、`stop` 返回之后线程不会再接。
    state: Mutex<ListenState>,
    wake: io::PipeWriter,
}

impl ListenControl {
    /// 不再接新连接，返回时已经不会再接，见 `ListenState::Paused`。
    pub(crate) fn pause(&self) {
        self.set(ListenState::Paused);
    }

    /// 接着接新连接，先接 backlog 里排着的。
    pub(crate) fn resume(&self) {
        self.set(ListenState::Accepting);
    }

    /// 结束接受连接的线程，见 `ListenState::Stopped`。
    pub(crate) fn stop(&self) {
        self.set(ListenState::Stopped);
    }

    fn set(&self, to: ListenState) {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if *state != ListenState::Stopped {
                *state = to;
            }
        }
        if let Err(err) = (&self.wake).write_all(&[1]) {
            tracing::warn!("failed to wake the listener thread: {err}");
        }
    }
}

/// 接受连接的线程：按 `control` 的状态 `poll` 唤醒管道（停着时只等它）和监听的 socket，有连接
/// 就接下来、登记、起线程服务。
fn accept_loop(shared: &Arc<Shared>, listener: &UnixListener, control: &ListenControl, wake: &io::PipeReader) {
    loop {
        let state = *control.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state == ListenState::Stopped {
            return;
        }
        let mut fds = [
            libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: listener.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let count: libc::nfds_t = if state == ListenState::Accepting { 2 } else { 1 };
        // SAFETY: `fds` 是本地数组，`count` 不超过它的长度；两个描述符在这次调用期间都开着。
        if unsafe { libc::poll(fds.as_mut_ptr(), count, -1) } < 0 {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                tracing::warn!("failed to wait for connections: {err}");
                thread::sleep(ACCEPT_BACKOFF);
            }
            continue;
        }
        if fds[0].revents != 0 {
            let mut drained = [0u8; 64];
            let _ = (&*wake).read(&mut drained);
        }
        if count < 2 || fds[1].revents == 0 {
            continue;
        }
        let (accepted, failed) = {
            let state = control.state.lock().unwrap_or_else(PoisonError::into_inner);
            if *state != ListenState::Accepting {
                continue;
            }
            accept_ready(listener)
        };
        for stream in accepted {
            // 判断还接不接和登记连接在同一把锁里，空闲退出时不会漏掉刚连上的。
            let Some(id) = register(shared) else {
                drop(stream);
                continue;
            };
            start_serving(shared, id, stream, true);
        }
        if failed {
            thread::sleep(ACCEPT_BACKOFF);
        }
    }
}

/// 把 backlog 里排着的连接都接下来；出错（比如文件描述符用完了）时停下，第二项为 true。
fn accept_ready(listener: &UnixListener) -> (Vec<UnixStream>, bool) {
    let mut accepted = Vec::new();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // macOS 上接到的连接沿用监听的 socket 的 `O_NONBLOCK`，连接一律按阻塞的读写。
                match stream.set_nonblocking(false) {
                    Ok(()) => accepted.push(stream),
                    Err(err) => tracing::warn!("failed to set up a connection: {err}"),
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return (accepted, false),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                tracing::warn!("failed to accept a connection: {err}");
                return (accepted, true);
            }
        }
    }
}

impl Host {
    /// 在 `socket` 上监听别的进程来的前端，之后在后台线程里一直接受连接，直到宿主退出（见
    /// `Host::run_until_idle`）。
    ///
    /// `lock` 是同一目录里的锁文件：拿到它的进程才监听，另一个宿主已经在监听时返回错误。
    /// 拿到锁以后，`socket` 上已有的文件只能是上次没清掉的，删掉重建。宿主退出时删掉 `socket`
    /// （交接给新宿主时不删）。这个宿主已经在监听时返回错误。
    pub fn listen(&self, socket: &Path, lock: &Path) -> Result<()> {
        let lock = lock_exclusively(lock)?;
        match std::fs::remove_file(socket) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => {
                return Err(err).with_context(|| format!("failed to remove the stale socket {}", socket.display()));
            }
            _ => {}
        }
        let listener =
            UnixListener::bind(socket).with_context(|| format!("failed to listen on {}", socket.display()))?;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        self.start_listening(listener, lock, socket, false)?;
        // 之后开的 shell 里的命令行连这个 socket，开发版和装好的版本同时开着时也不会连错。
        self.set_env(runode_protocol::ENV_SOCKET, socket.as_os_str());
        Ok(())
    }

    /// 在已经开好的 `listener` 上接受别的进程来的前端，同 `Host::listen`，但不设 shell 的环境变量。
    /// `lock` 是锁住了的锁文件（`flock` 锁的是打开的文件，交接时连同描述符一起交过来的仍锁着），
    /// 丢掉时放开；`socket` 是 `listener` 的路径。`paused` 时接受连接的线程先停着（连接排在
    /// backlog 里），等 `ListenControl::resume`。
    pub(crate) fn start_listening(
        &self,
        listener: UnixListener,
        lock: File,
        socket: &Path,
        paused: bool,
    ) -> Result<Arc<ListenControl>> {
        if self.shared.peers().listening.is_some() {
            return Err(anyhow!("the host is already listening"));
        }
        // 非阻塞：接受连接的线程先 `poll` 再接，`poll` 之后没接到（别的进程抢先接走了，或者交接
        // 前后两个宿主共用这个打开的 socket）时回去接着等，不卡在 `accept` 里叫不停。
        listener.set_nonblocking(true).context("failed to make the listener non-blocking")?;
        let listener = Arc::new(listener);
        let (wake_rx, wake_tx) = io::pipe().context("failed to create the listener's wake pipe")?;
        let state = if paused { ListenState::Paused } else { ListenState::Accepting };
        let control = Arc::new(ListenControl { state: Mutex::new(state), wake: wake_tx });
        let shared = self.shared.clone();
        let thread_listener = listener.clone();
        let thread_control = control.clone();
        thread::Builder::new()
            .name("host-listener".into())
            .spawn(move || accept_loop(&shared, &thread_listener, &thread_control, &wake_rx))
            .context("failed to start the listener thread")?;
        self.shared.peers().listening =
            Some(Listening { socket: socket.into(), listener, lock, control: control.clone() });
        Ok(control)
    }

    /// 在同一个进程里连上宿主：开一对互相连着的 socket，宿主这边起和 socket 上一样的连接，返回
    /// 另一端。之后按 `runode_protocol` 说话，先发 `Hello`。两端的收发缓冲都设好了。宿主在退出时
    /// 返回错误。
    pub fn connect_pair(&self) -> io::Result<UnixStream> {
        let (ours, theirs) = UnixStream::pair()?;
        set_buffers(&theirs);
        let Some(id) = register(&self.shared) else {
            return Err(io::Error::other("the host is shutting down"));
        };
        if !start_serving(&self.shared, id, ours, false) {
            return Err(io::Error::other("failed to start a connection thread"));
        }
        Ok(theirs)
    }
}

/// 登记一条新连接，返回它的编号；宿主不再接新连接时返回 `None`。
fn register(shared: &Shared) -> Option<u64> {
    let mut peers = shared.peers();
    if !peers.accepting {
        return None;
    }
    let id = shared.next_connection();
    peers.connections.insert(id, Peer { out: None });
    peers.activity_at = Instant::now();
    Some(id)
}

/// 起线程服务登记好的连接 `id`；起不了时撤掉登记，返回 false。
fn start_serving(shared: &Arc<Shared>, id: u64, stream: UnixStream, check_peer: bool) -> bool {
    let thread_shared = shared.clone();
    let spawned = thread::Builder::new()
        .name("host-connection".into())
        .spawn(move || serve(&thread_shared, id, stream, check_peer));
    if let Err(err) = spawned {
        tracing::warn!("failed to start a connection thread: {err}");
        unregister(shared, id);
        return false;
    }
    true
}

/// 连接结束：撤掉登记。它是界面的话，转给它还没回话的请求都回一句没办成（会话的请求交回会话
/// 线程）；它发出去、界面还没回话的请求撤掉，回话没人收了。
fn unregister(shared: &Shared, id: u64) {
    let mut peers = shared.peers();
    peers.connections.remove(&id);
    peers.desktops.retain(|&desktop| desktop != id);
    peers.pending.retain(|_, pending| !matches!(pending.asker, Asker::Client { from, .. } if from == id));
    let orphaned: Vec<u64> =
        peers.pending.iter().filter(|(_, pending)| pending.desktop == id).map(|(&ui, _)| ui).collect();
    let gone = || "the runode window went away before answering".to_owned();
    let mut sessions = Vec::new();
    for ui in orphaned {
        match peers.pending.remove(&ui).map(|pending| pending.asker) {
            Some(Asker::Client { origin, req, .. }) => {
                origin.control(&HostMsg::Error { req: Some(req), id: None, message: gone() });
            }
            Some(Asker::Session(session)) => sessions.push((session, ui)),
            None => {}
        }
    }
    peers.activity_at = Instant::now();
    drop(peers);
    shared.peers_changed.notify_all();
    for (session, ui) in sessions {
        let reply = Box::new(HostMsg::Error { req: None, id: Some(session), message: gone() });
        shared.deliver(session, Inbox::UiAnswer { ui, reply });
    }
}

impl Shared {
    /// 要退出了（收到 `Shutdown`）。宿主单独一个进程在跑时，给所有连接发 `Goodbye`、写完后断开，
    /// 由 `Host::run_until_idle` 收尾，返回 true；跑在 app 进程里时什么都不做，返回 false。
    fn shut_down(&self) -> bool {
        let mut peers = self.peers();
        if !peers.standalone {
            return false;
        }
        peers.stop.get_or_insert(Stopped::Shutdown);
        peers.accepting = false;
        peers.say_goodbye(None, false, &GoodbyeReason::Shutdown);
        drop(peers);
        self.peers_changed.notify_all();
        true
    }

    /// 会话 `session` 请界面办一件事（`request`），返回这条请求的编号，界面的回话经
    /// `Inbox::UiAnswer` 带着它交回会话线程。交给 `preferred` 这条界面的连接，它不是（或者不再是）
    /// 界面时交给最近连上的界面；没有界面连着时返回 `None`。
    fn ask_ui(&self, session: SessionId, preferred: Option<u64>, request: ClientMsg) -> Option<u64> {
        let mut peers = self.peers();
        let desktop =
            preferred.filter(|connection| peers.desktops.contains(connection)).or(peers.desktops.last().copied())?;
        let out = peers.connections.get(&desktop).and_then(|peer| peer.out.clone())?;
        let ui = peers.next_ui;
        peers.next_ui += 1;
        peers.pending.insert(ui, Pending { desktop, asker: Asker::Session(session) });
        drop(peers);
        if out.control(&HostMsg::UiRequest { ui, request: Box::new(request) }) {
            return Some(ui);
        }
        // 界面的连接刚断开：撤掉这条请求（读线程撤掉登记时已经替它回过话的话，那条回话会话线程
        // 认不出，丢掉）。
        self.peers().pending.remove(&ui);
        None
    }

    /// 所有会话在 `SessionList` 里的样子：在调用的线程里把请求按先后送到各个会话线程，返回等
    /// 回话的一端，见 `collect_sessions`。
    fn ask_sessions(&self) -> Vec<mpsc::Receiver<SessionInfo>> {
        self.handles()
            .into_iter()
            .filter_map(|(_, handle)| {
                let (reply, info) = mpsc::channel();
                handle.send(Inbox::Info(reply)).then_some(info)
            })
            .collect()
    }
}

/// 等 `Shared::ask_sessions` 的回话，最多等到 `REPLY_TIMEOUT`；没回话的会话不列。
fn collect_sessions(replies: Vec<mpsc::Receiver<SessionInfo>>) -> Vec<SessionInfo> {
    let deadline = Instant::now() + REPLY_TIMEOUT;
    replies
        .into_iter()
        .filter_map(|info| info.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok())
        .collect()
}

/// 一条连接上在等会话线程回话的请求数，上限 `MAX_WAITING`。
#[derive(Clone, Default)]
struct Waiting(Arc<AtomicUsize>);

impl Waiting {
    /// 再等一个；已经到上限时返回 `None`。返回的 `WaitSlot` 丢掉时减回去。
    fn enter(&self) -> Option<WaitSlot> {
        self.0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < MAX_WAITING).then_some(n + 1))
            .ok()
            .map(|_| WaitSlot(self.0.clone()))
    }
}

/// `Waiting` 里的一个名额，回完话（或者没起成线程、丢掉了）时放开。
pub(crate) struct WaitSlot(Arc<AtomicUsize>);

impl Drop for WaitSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 起一个短命的线程等回话，不占着读的线程；起不了时就地等。
fn answer_later(name: &str, answer: impl FnOnce() + Send + 'static) {
    let answer = Arc::new(std::sync::Mutex::new(Some(answer)));
    let deferred = answer.clone();
    let spawned = thread::Builder::new().name(name.into()).spawn(move || {
        if let Some(answer) = deferred.lock().ok().and_then(|mut answer| answer.take()) {
            answer();
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("failed to start a reply thread, answering in place: {err}");
        if let Some(answer) = answer.lock().ok().and_then(|mut answer| answer.take()) {
            answer();
        }
    }
}

/// 打开并锁住锁文件，拿不到时返回错误。进程退出（或者丢掉返回的文件）时锁自动放开。
fn lock_exclusively(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to open the lock {}", path.display()))?;
    // SAFETY: 描述符来自上面打开的文件，在这次调用期间一直有效。
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let err = io::Error::last_os_error();
        return Err(if err.kind() == io::ErrorKind::WouldBlock {
            anyhow!("another runode already holds {}", path.display())
        } else {
            anyhow!("failed to lock {}: {err}", path.display())
        });
    }
    Ok(file)
}

/// 连上来的进程是不是和自己同一个用户。
pub fn same_user(stream: &UnixStream) -> bool {
    // SAFETY: 没有参数，总是成功。
    peer_uid(stream) == Some(unsafe { libc::geteuid() })
}

#[cfg(not(target_os = "linux"))]
fn peer_uid(stream: &UnixStream) -> Option<libc::uid_t> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: 描述符来自 `stream`，两个输出参数指向本地变量。
    (unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0).then_some(uid)
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> Option<libc::uid_t> {
    // SAFETY: `ucred` 是纯数据，全零合法。
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: 描述符来自 `stream`；输出参数指向本地变量，长度如实给出。
    let ok = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len)
    } == 0;
    ok.then_some(cred.uid)
}

/// 把 socket 的收发缓冲设成 `SOCKET_BUFFER`；设不了时记日志，照常用。
pub fn set_buffers(stream: &UnixStream) {
    for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        let size = SOCKET_BUFFER;
        // SAFETY: 描述符来自 `stream`；值指向本地变量，长度是它的大小。
        let result = unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                (&raw const size).cast(),
                std::mem::size_of_val(&size) as libc::socklen_t,
            )
        };
        if result != 0 {
            tracing::debug!("failed to set a socket buffer: {}", io::Error::last_os_error());
        }
    }
}

/// 一条连接从头到尾：起写的线程，在这个线程里读，读完了断开连着的会话、撤掉登记。写的线程等
/// 所有会话都放开这条连接、积压的帧写完后自己结束。`check_peer` 时只接同一个用户的进程。
fn serve(shared: &Arc<Shared>, id: u64, stream: UnixStream, check_peer: bool) {
    pty::set_current_thread_interactive();
    if check_peer && !same_user(&stream) {
        tracing::warn!("refused a connection from another user");
        unregister(shared, id);
        return;
    }
    set_buffers(&stream);
    let Some(out) = start_writer(&stream) else {
        unregister(shared, id);
        return;
    };
    {
        let mut peers = shared.peers();
        // 登记之后、写线程起好之前宿主要退出了，或者开始把会话交给新宿主了：这条连接也收
        // `Goodbye`。
        let reason = match peers.stop {
            Some(Stopped::Shutdown) => Some(GoodbyeReason::Shutdown),
            Some(Stopped::Handoff) => Some(GoodbyeReason::Handoff),
            _ if peers.handoff.is_some() => Some(GoodbyeReason::Handoff),
            _ => None,
        };
        if let Some(reason) = reason {
            out.control(&HostMsg::Goodbye { reason });
            out.close();
        }
        if let Some(peer) = peers.connections.get_mut(&id) {
            peer.out = Some(out.clone());
        }
    }
    let mut connection = Connection {
        shared: shared.clone(),
        id,
        out,
        kind: ClientKind::Unknown,
        by: None,
        device: None,
        snapshots: false,
        channels: HashMap::new(),
        next_channel: 1,
        waiting: Waiting::default(),
        git: None,
    };
    connection.run(&mut BufReader::new(&stream));
    connection.detach_all();
    unregister(shared, id);
}

/// 起这条连接的写线程，返回往它送帧的 `Outbox`。
fn start_writer(stream: &UnixStream) -> Option<Outbox> {
    let write_half = match stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!("failed to set up a connection: {err}");
            return None;
        }
    };
    let (tx, rx) = mpsc::channel();
    let out = Outbox { tx, queued: Arc::default() };
    let queued = out.queued.clone();
    let spawned = thread::Builder::new().name("host-connection-writer".into()).spawn(move || {
        pty::set_current_thread_interactive();
        write_out(write_half, &rx, &queued);
    });
    match spawned {
        Ok(_) => Some(out),
        Err(err) => {
            tracing::warn!("failed to start a connection writer: {err}");
            None
        }
    }
}

/// 一条连接上等着写出去的帧。可以随意克隆，各份往同一个写的线程送。
#[derive(Clone)]
pub(crate) struct Outbox {
    tx: mpsc::Sender<Out>,
    /// 已经交给写的线程、还没写出去的载荷字节数。
    queued: Arc<AtomicUsize>,
}

enum Out {
    Frame {
        kind: FrameKind,
        channel: u32,
        payload: Arc<[u8]>,
    },
    /// 之前的帧写完后断开连接。
    Close,
    /// 之前的帧写完后写的线程结束，但不断开连接，写完时往里发一声：交接时之后由交出会话的线程
    /// 直接在 socket 上发描述符消息，见 `handoff::give`。
    Detach(mpsc::Sender<()>),
}

impl Outbox {
    /// 交一帧给写的线程；连接已经断了时返回 false。
    fn push(&self, kind: FrameKind, channel: u32, payload: Arc<[u8]>) -> bool {
        self.queued.fetch_add(payload.len(), Ordering::Relaxed);
        self.tx.send(Out::Frame { kind, channel, payload }).is_ok()
    }

    pub(crate) fn control(&self, message: &HostMsg) -> bool {
        match Frame::control(message) {
            Ok(frame) => self.push(FrameKind::Control, 0, frame.payload.into()),
            Err(err) => {
                tracing::warn!("failed to encode {message:?}: {err}");
                true
            }
        }
    }

    /// 已经交给写线程的帧写完后断开连接，之后交的不再写。
    pub(crate) fn close(&self) {
        let _ = self.tx.send(Out::Close);
    }

    /// 已经交给写线程的帧写完后写线程结束，连接留着，之后交的不再写；返回的一端在写完时收到
    /// 一声，写不出去（对面断了）时断开。
    pub(crate) fn detach(&self) -> mpsc::Receiver<()> {
        let (done, finished) = mpsc::channel();
        let _ = self.tx.send(Out::Detach(done));
        finished
    }

    fn backed_up(&self) -> bool {
        self.queued.load(Ordering::Relaxed) > OUTBOX_LIMIT
    }
}

/// 写的线程：有帧就写，一批写完再刷出去。写不出去（对面断了）或者要断开时关掉连接，读的那边
/// 也随之结束；`Out::Detach` 时写完就结束，不断开。
fn write_out(stream: UnixStream, frames: &mpsc::Receiver<Out>, queued: &AtomicUsize) {
    let mut writer = BufWriter::with_capacity(WRITE_BUFFER, &stream);
    let mut write_all = || -> Result<Option<mpsc::Sender<()>>, FrameError> {
        while let Ok(first) = frames.recv() {
            let mut next = Some(first);
            while let Some(out) = next {
                match out {
                    Out::Frame { kind, channel, payload } => {
                        write_frame(&mut writer, kind, channel, &payload)?;
                        queued.fetch_sub(payload.len(), Ordering::Relaxed);
                    }
                    Out::Close => {
                        writer.flush()?;
                        return Ok(None);
                    }
                    Out::Detach(done) => {
                        writer.flush()?;
                        return Ok(Some(done));
                    }
                }
                next = frames.try_recv().ok();
            }
            writer.flush()?;
        }
        Ok(None)
    };
    match write_all() {
        Ok(Some(done)) => {
            let _ = done.send(());
            return;
        }
        Ok(None) => {}
        Err(err) => tracing::debug!("connection closed while writing: {err}"),
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// 一条连接上读的那边的状态。
struct Connection {
    shared: Arc<Shared>,
    /// 这条连接的编号：`Detach` 按它找到这条连接的订阅，转给界面的请求按它认回话的一方。
    id: u64,
    out: Outbox,
    /// `Hello` 里说的前端种类。
    kind: ClientKind,
    /// `Hello` 里说的前端所在的会话，记谁在操作会话时用，见 `drive`。
    by: Option<SessionId>,
    /// `Hello` 里报的设备名，尺寸归这条连接时告诉别的前端，见 `HostMsg::SizeOwner`。
    device: Option<String>,
    /// 前端解得了快照：自己说能解，构建也和宿主一样。
    snapshots: bool,
    /// 连着的会话，按通道。
    channels: HashMap<u32, SessionId>,
    next_channel: u32,
    /// 在等回话的列会话、读屏幕、读写 git 的请求。
    waiting: Waiting,
    /// 办 git 请求的工作线程，第一次要时才起。
    git: Option<GitWorker>,
}

impl Connection {
    fn run(&mut self, reader: &mut BufReader<&UnixStream>) {
        let hello = match read_frame(reader) {
            Ok(Some(frame)) if frame.kind == FrameKind::Control => frame.message::<ClientMsg>().ok(),
            Ok(_) => None,
            Err(err) => {
                tracing::debug!("connection closed before hello: {err}");
                return;
            }
        };
        let Some(ClientMsg::Hello { protocol, build, caps, client, session, device }) = hello else {
            self.goodbye("the first message must be hello");
            return;
        };
        // 接手的新宿主不比对协议版本，永远如此，见 `runode_protocol::message` 的模块文档。
        if client == ClientKind::Successor {
            self.kind = client;
            let standalone = self.shared.peers().standalone;
            self.out.control(&self.welcome(standalone));
            self.successor(reader, build);
            return;
        }
        if protocol != PROTOCOL_VERSION {
            self.out.control(&HostMsg::Incompatible {
                protocol: PROTOCOL_VERSION,
                build: self.shared.build.clone(),
                reason: format!("the host speaks protocol {PROTOCOL_VERSION}, the client {protocol}"),
            });
            return;
        }
        self.kind = client;
        self.by = session;
        self.device = device;
        self.snapshots = caps.snapshot && build == self.shared.build;
        // 登记界面和回 `Welcome` 在同一把锁里做：界面收到 `Welcome` 后，别的连接转来的请求一定
        // 交给它；别的连接也只有在 `Welcome` 排进 `Outbox` 之后才看得到它，请求不会抢在前面。
        let mut peers = self.shared.peers();
        if client == ClientKind::Desktop {
            peers.desktops.push(self.id);
        }
        self.out.control(&self.welcome(peers.standalone));
        drop(peers);
        loop {
            let frame = match read_frame(reader) {
                Ok(Some(frame)) => frame,
                Ok(None) => return,
                Err(err) => {
                    tracing::debug!("connection closed: {err}");
                    return;
                }
            };
            match frame.kind {
                FrameKind::Control => match frame.message::<ClientMsg>() {
                    Ok(message) => self.handle(message),
                    Err(err) => self.error(None, None, format!("unreadable message: {err}")),
                },
                FrameKind::Input => match self.channels.get(&frame.channel) {
                    // 会话已经没了（比如别的前端结束了它）：通道跟着作废。
                    Some(&id) => {
                        self.drive(id, DriveAction::Input);
                        let input = Inbox::Input { connection: self.id, data: frame.payload };
                        if !self.shared.deliver(id, input) {
                            self.channels.remove(&frame.channel);
                        }
                    }
                    None => tracing::debug!("input for unknown channel {}", frame.channel),
                },
                FrameKind::Output | FrameKind::Snapshot => {
                    self.goodbye("clients do not send output or snapshots");
                    return;
                }
            }
        }
    }

    fn welcome(&self, standalone: bool) -> HostMsg {
        HostMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            build: self.shared.build.clone(),
            host_pid: std::process::id(),
            snapshot_format: self.shared.snapshot_format,
            standalone,
            handoff: HANDOFF_FORMAT,
        }
    }

    /// 接手的新宿主（构建是 `build`）连上来：只认 `ClientMsg::Handoff`，交给 `handoff::give`，之后
    /// 这条连接归它。
    fn successor(&self, reader: &mut BufReader<&UnixStream>, build: runode_protocol::BuildId) {
        let message = match read_frame(reader) {
            Ok(Some(frame)) if frame.kind == FrameKind::Control => frame.message::<ClientMsg>().ok(),
            Ok(_) => None,
            Err(err) => {
                tracing::debug!("the successor went away before asking for the handoff: {err}");
                return;
            }
        };
        match message {
            Some(ClientMsg::Handoff { min_format, max_format }) => {
                let asked = handoff::Asked { formats: min_format..=max_format, build };
                handoff::give(&self.shared, self.id, &self.out, reader, &asked);
            }
            _ => self.goodbye("a successor must ask for the handoff"),
        }
    }

    fn handle(&mut self, message: ClientMsg) {
        match message {
            ClientMsg::Hello { .. } => self.error(None, None, "already said hello".into()),
            ClientMsg::ListSessions => {
                let Some(slot) = self.waiting.enter() else {
                    self.error(None, None, "too many requests are waiting for an answer".into());
                    return;
                };
                let replies = self.shared.ask_sessions();
                let out = self.out.clone();
                answer_later("host-list-sessions", move || {
                    out.control(&HostMsg::SessionList { sessions: collect_sessions(replies) });
                    drop(slot);
                });
            }
            ClientMsg::Spawn { req, size, cwd, integration, start, shell, settings } => {
                let options = SpawnOptions { size, cwd, integration, start, shell, settings };
                match self.shared.spawn(options) {
                    Ok(id) => self.out.control(&HostMsg::Spawned { req, id }),
                    Err(err) => {
                        self.out.control(&HostMsg::Error { req: Some(req), id: None, message: format!("{err:#}") })
                    }
                };
            }
            ClientMsg::Start { id, integration } => {
                if !self.shared.deliver(id, Inbox::Start { integration }) {
                    self.error(None, Some(id), format!("no session {id}"));
                }
            }
            ClientMsg::Attach { id, size, mode } => self.attach(id, size, mode),
            ClientMsg::Detach { id } => {
                self.forget(id);
                self.shared.deliver(id, Inbox::Detach { connection: self.id });
            }
            ClientMsg::Kill { id } => {
                self.drive(id, DriveAction::Kill);
                self.forget(id);
                self.shared.kill(id);
            }
            ClientMsg::Resize { id, size } => {
                self.shared.deliver(id, Inbox::Resize { connection: self.id, size });
            }
            ClientMsg::ClearScreen { id } => {
                self.drive(id, DriveAction::ClearScreen);
                self.shared.deliver(id, Inbox::ClearScreen);
            }
            ClientMsg::SetTheme { settings } => self.shared.set_theme(settings),
            // 记不记命令历史是用户在 app 里的设置，别的程序不能改。
            ClientMsg::SetOptions { record_history, clipboard } => {
                if self.kind == ClientKind::Desktop {
                    self.shared.record_history.store(record_history, Ordering::Relaxed);
                    self.shared.set_clipboard(clipboard);
                } else {
                    self.error(None, None, "only the runode app changes the host's options".into());
                }
            }
            // 获得焦点算一次交互，可能轮到这条连接决定尺寸；失去焦点只是不在看了，不让出。通知在
            // 界面那边发，宿主不用知道哪个会话被看着。
            ClientMsg::Focus { id, focused: true } => {
                self.shared.deliver(id, Inbox::Focus { connection: self.id });
            }
            ClientMsg::Focus { focused: false, .. } => {}
            ClientMsg::ReadScreen { id, lines, command } => self.read_screen(id, lines, command),
            ClientMsg::SendKeys { req, id, keys } => {
                let parsed: Result<Vec<_>, _> = keys.iter().map(|key| parse_keys(key)).collect();
                match parsed {
                    Ok(keys) => self.deliver_done(req, id, DriveAction::Keys, Inbox::Keys(keys.concat())),
                    Err(err) => self.error(Some(req), Some(id), err),
                }
            }
            ClientMsg::Paste { req, id, text } => self.deliver_done(req, id, DriveAction::Paste, Inbox::Paste(text)),
            ClientMsg::Open { req, .. }
            | ClientMsg::OpenWorkspace { req, .. }
            | ClientMsg::RenameWorkspace { req, .. }
            | ClientMsg::Reveal { req, .. }
            | ClientMsg::Layout { req } => {
                self.to_ui(req, message);
            }
            ClientMsg::ListDirs { req, path } => match browse::list_dirs(path) {
                Ok(browse::Listing { path, dirs, truncated }) => {
                    self.out.control(&HostMsg::Dirs { req, path, dirs, truncated });
                }
                Err(message) => self.error(Some(req), None, message),
            },
            ClientMsg::ListProjectTasks { req, dir } => match project_tasks::list(&dir) {
                Ok(sources) => {
                    self.out.control(&HostMsg::ProjectTasks { req, dir, sources });
                }
                Err(message) => self.error(Some(req), None, message),
            },
            ClientMsg::Git { req, id, request } => self.git(req, id, request),
            ClientMsg::UiReply { ui, reply } => self.ui_reply(ui, *reply),
            // 读写剪贴板是宿主替会话里的程序请界面办的，前端不能直接要。
            ClientMsg::WriteClipboard { .. } | ClientMsg::ReadClipboard { .. } => {
                self.error(None, None, "only the host asks the runode window to use the clipboard".into());
            }
            ClientMsg::Shutdown { kill_sessions: true } => {
                self.channels.clear();
                self.shared.kill_all();
                self.shared.shut_down();
            }
            ClientMsg::Shutdown { kill_sessions: false } => {
                self.error(None, None, "the host cannot hand its sessions over yet".into());
            }
            // 交接只在 `Hello` 里说自己是 `ClientKind::Successor` 的连接上谈，见 `successor`。
            ClientMsg::Handoff { .. }
            | ClientMsg::HandoffReady
            | ClientMsg::HandoffAbort { .. }
            | ClientMsg::HandoffDone => {
                self.error(None, None, "only a successor host can take the sessions over".into());
            }
            // 推送只有手机经远程访问登记，由监听方按过了门禁的那台设备记下，不到宿主这里。
            ClientMsg::PushRegister { req, .. } => {
                self.error(Some(req), None, "only a remote device registers for push through remote access".into());
            }
            ClientMsg::Unknown => self.error(None, None, "unknown message".into()),
        }
    }

    /// 把要界面办的请求转给现在的界面（最近连上的 `ClientKind::Desktop` 连接）。没有界面连接
    /// 时回一句没办成。
    fn to_ui(&self, req: u32, request: ClientMsg) {
        let mut peers = self.shared.peers();
        if let Some(&desktop) = peers.desktops.last()
            && let Some(out) = peers.connections.get(&desktop).and_then(|peer| peer.out.clone())
        {
            let ui = peers.next_ui;
            peers.next_ui += 1;
            let asker = Asker::Client { from: self.id, origin: self.out.clone(), req };
            peers.pending.insert(ui, Pending { desktop, asker });
            drop(peers);
            if !out.control(&HostMsg::UiRequest { ui, request: Box::new(request) }) {
                // 界面的连接刚断开：它的读线程撤掉登记时会替还在等的请求回话；已经撤掉了的话
                // 这条请求是登记之后才加的，在这里回。
                let pending = self.shared.peers().pending.remove(&ui);
                if let Some(Pending { asker: Asker::Client { origin, req, .. }, .. }) = pending {
                    origin.control(&HostMsg::Error {
                        req: Some(req),
                        id: None,
                        message: "the runode window went away before answering".into(),
                    });
                }
            }
            return;
        }
        drop(peers);
        self.error(Some(req), None, "there is no runode window to do this in".into());
    }

    /// 界面回话：转给发请求的一方，会话自己的请求交回会话线程。只认被转去的那个界面回的；对不上的
    /// （请求已经回过、不是转给这条连接的、发请求的一方已经断开的）丢掉。界面读不懂请求（读成
    /// `ClientMsg::Unknown`）时回的 `Error` 不知道 `req`，这里补上原请求的，发请求的一方才认得出是
    /// 哪条没办成。
    fn ui_reply(&self, ui: u64, reply: HostMsg) {
        let pending = {
            let mut peers = self.shared.peers();
            match peers.pending.get(&ui) {
                Some(pending) if pending.desktop == self.id => peers.pending.remove(&ui),
                _ => None,
            }
        };
        match pending.map(|pending| pending.asker) {
            Some(Asker::Client { origin, req, .. }) => {
                let reply = match reply {
                    HostMsg::Error { req: None, id, message } => HostMsg::Error { req: Some(req), id, message },
                    reply => reply,
                };
                origin.control(&reply);
            }
            Some(Asker::Session(session)) => {
                self.shared.deliver(session, Inbox::UiAnswer { ui, reply: Box::new(reply) });
            }
            None => tracing::debug!("dropped a reply to unknown ui request {ui}"),
        }
    }

    /// 连上会话。已经连着的换一个新通道重新连，前端收到 `Resync` 后就是这样重新连上的；
    /// `MetaOnly` 和看屏幕之间来回切换也是这样。不先发 `Detach`：会话线程收到同一条连接的
    /// `Subscribe` 时换掉旧的订阅，尺寸归属的状态（见 `Runner::subscribe`）留着；旧通道的帧都在
    /// 新的 `Attached` 之前，新通道的帧都在它之后。
    fn attach(&mut self, id: SessionId, size: Option<GridSize>, mode: AttachMode) {
        self.forget(id);
        let mode = match mode {
            AttachMode::Snapshot if !self.snapshots => AttachMode::VtReplay,
            mode => mode,
        };
        let channel = self.next_channel;
        self.next_channel = self.next_channel.checked_add(1).unwrap_or(1);
        let out = self.out.clone();
        let start = Box::new(move |screen: Screen| {
            let meta_only = screen.mode == AttachMode::MetaOnly;
            let attached = HostMsg::Attached {
                id,
                channel,
                size: screen.size,
                mode: screen.mode,
                meta: screen.meta,
                settings: Some(screen.settings),
            };
            if !out.control(&attached) {
                return None;
            }
            if !meta_only {
                for chunk in screen.data.chunks(SNAPSHOT_CHUNK) {
                    out.push(FrameKind::Snapshot, channel, chunk.into());
                }
                out.control(&HostMsg::SnapshotEnd { id });
            }
            Some(session_sink(id, channel, meta_only, out))
        });
        let desktop = self.kind == ClientKind::Desktop;
        let device = self.device.clone();
        let subscribe = Subscribe { connection: self.id, size, mode, start, desktop, device };
        if self.shared.deliver(id, Inbox::Subscribe(subscribe)) {
            self.channels.insert(channel, id);
        } else {
            self.error(None, Some(id), format!("no session {id}"));
        }
    }

    /// 读屏幕，`command` 给了时读倒数第几条命令的输出：请求在这里送到会话线程（排在这条连接
    /// 之前送去的输入后面），回话另起线程等。
    fn read_screen(&self, id: SessionId, lines: Option<u32>, command: Option<u32>) {
        let Some(slot) = self.waiting.enter() else {
            self.error(None, Some(id), "too many requests are waiting for an answer".into());
            return;
        };
        let (reply, text) = mpsc::channel();
        if !self.shared.deliver(id, Inbox::ReadScreen { lines, command, reply }) {
            self.error(None, Some(id), format!("no session {id}"));
            return;
        }
        let out = self.out.clone();
        answer_later("host-read-screen", move || {
            let message = match text.recv_timeout(REPLY_TIMEOUT) {
                Ok(Ok((text, truncated))) => HostMsg::ScreenText { id, text, truncated },
                // 读命令输出时的错误（没有 shell 集成这类）本身就是给读的一方看的说明。
                Ok(Err(err)) if command.is_some() => {
                    HostMsg::Error { req: None, id: Some(id), message: format!("{err:#}") }
                }
                Ok(Err(err)) => {
                    HostMsg::Error { req: None, id: Some(id), message: format!("failed to read the screen: {err:#}") }
                }
                Err(_) => HostMsg::Error { req: None, id: Some(id), message: format!("session {id} did not answer") },
            };
            out.control(&message);
            drop(slot);
        });
    }

    /// 在会话 `id` 所在的仓库里办 git 请求：向会话线程要它的目录，连同请求交给这条连接的 git
    /// 工作线程，见 `git` 模块。
    fn git(&mut self, req: u32, id: SessionId, request: GitRequest) {
        let Some(slot) = self.waiting.enter() else {
            self.error(Some(req), None, "too many requests are waiting for an answer".into());
            return;
        };
        let (reply, info) = mpsc::channel();
        if !self.shared.deliver(id, Inbox::Info(reply)) {
            self.error(Some(req), None, format!("no session {id}"));
            return;
        }
        if self.git.is_none() {
            match GitWorker::start() {
                Ok(worker) => self.git = Some(worker),
                Err(err) => {
                    self.error(Some(req), None, format!("failed to start git: {err}"));
                    return;
                }
            }
        }
        let job = Job { req, id, request, info, out: self.out.clone(), slot };
        if let Some(worker) = &self.git
            && let Err(job) = worker.submit(job)
        {
            self.git = None;
            self.error(Some(job.req), None, "the git worker stopped".into());
        }
    }

    /// 这条连接要对会话做 `action`，先告诉会话谁在操作它（见 `SessionMeta::driver`）：桌面的
    /// 界面上是用户自己，清掉记录（会话没被标过时 `Handle::send` 直接丢掉，不进收件箱）；别的
    /// 前端记下来。只有会改会话的操作才调。
    fn drive(&self, id: SessionId, action: DriveAction) {
        let drive = (self.kind != ClientKind::Desktop).then_some(Drive { by: self.by, action });
        self.shared.deliver(id, Inbox::Driven(drive));
    }

    /// 把一件只回 `Done` 的事（发控制键、粘贴）交给会话，先记下谁在操作它。会话线程按到达的先后
    /// 处理，在这之后发来的输入不会抢到前面。
    fn deliver_done(&self, req: u32, id: SessionId, action: DriveAction, message: Inbox) {
        self.drive(id, action);
        if self.shared.deliver(id, message) {
            self.out.control(&HostMsg::Done { req });
        } else {
            self.error(Some(req), Some(id), format!("no session {id}"));
        }
    }

    /// 不再记着这个会话的通道，之后它的输入帧不再转发；原来连着时返回 true。
    fn forget(&mut self, id: SessionId) -> bool {
        let before = self.channels.len();
        self.channels.retain(|_, attached| *attached != id);
        self.channels.len() != before
    }

    /// 连接结束：断开所有连着的会话，它们放开这条连接后写的线程就结束了。
    fn detach_all(&mut self) {
        for (_, id) in self.channels.drain() {
            self.shared.deliver(id, Inbox::Detach { connection: self.id });
        }
    }

    fn error(&self, req: Option<u32>, id: Option<SessionId>, message: String) {
        self.out.control(&HostMsg::Error { req, id, message });
    }

    fn goodbye(&self, message: &str) {
        self.out.control(&HostMsg::Goodbye { reason: GoodbyeReason::Error { message: message.into() } });
    }
}

/// 一个会话往这条连接发事件的 `EventSink`。`MetaOnly` 的前端没有 VT，只要状态（含 `Bell`），
/// 不要输出和改 VT 的标记。
fn session_sink(id: SessionId, channel: u32, meta_only: bool, out: Outbox) -> EventSink {
    Box::new(move |event| match event {
        Event::Output(_) if meta_only => true,
        Event::Output(data) => {
            if out.backed_up() {
                out.control(&HostMsg::Resync { id, reason: "the client reads too slowly".into() });
                return false;
            }
            out.push(FrameKind::Output, channel, data)
        }
        Event::Msg(message) => {
            if meta_only && matches!(*message, HostMsg::Resized { .. } | HostMsg::ThemeApplied { .. }) {
                return true;
            }
            out.control(&message)
        }
    })
}

#[cfg(test)]
mod tests {
    use runode_protocol::{BuildId, Caps};
    use runode_shared_types::shell::IntegrationMode;

    use super::*;

    const SIZE: GridSize = GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 };
    const WAIT: Duration = Duration::from_secs(10);

    fn send(stream: &mut UnixStream, message: &ClientMsg) {
        let frame = Frame::control(message).unwrap();
        write_frame(stream, frame.kind, 0, &frame.payload).unwrap();
    }

    /// 后台线程把读到的帧交过来，测试按超时等。
    fn frames(stream: &UnixStream) -> mpsc::Receiver<Frame> {
        let mut reader = stream.try_clone().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut reader) {
                if tx.send(frame).is_err() {
                    break;
                }
            }
        });
        rx
    }

    fn message(frames: &mpsc::Receiver<Frame>) -> HostMsg {
        loop {
            let frame = frames.recv_timeout(WAIT).expect("timed out");
            if frame.kind == FrameKind::Control {
                return frame.message().unwrap();
            }
        }
    }

    fn options(shell: &str) -> SpawnOptions {
        SpawnOptions {
            size: SIZE,
            cwd: None,
            integration: IntegrationMode::Off,
            start: true,
            shell: Some(shell.into()),
            settings: None,
        }
    }

    /// 经 `Host::connect_pair` 连上、按 `client` 握手，返回连接和读到的帧。
    fn greet(host: &Host, client: ClientKind) -> (UnixStream, mpsc::Receiver<Frame>) {
        let mut stream = host.connect_pair().unwrap();
        let frames = frames(&stream);
        send(
            &mut stream,
            &ClientMsg::Hello {
                protocol: PROTOCOL_VERSION,
                build: BuildId("test".into()),
                client,
                caps: Caps::default(),
                session: None,
                device: None,
            },
        );
        assert!(matches!(message(&frames), HostMsg::Welcome { .. }));
        (stream, frames)
    }

    /// 开一个线程卡在不返回的 `Subscribe::start` 里、答不了话的会话；丢掉返回的发送端时放开。
    fn stuck_session(host: &Host) -> (SessionId, mpsc::Sender<()>) {
        let stuck = host.shared.spawn(options("/bin/cat")).unwrap();
        let (release, released) = mpsc::channel::<()>();
        let (entered, stuck_now) = mpsc::channel();
        let start = Box::new(move |_: Screen| {
            let _ = entered.send(());
            // 卡住会话线程，直到测试放开（丢掉发送的一端）。
            let _ = released.recv();
            None
        });
        let subscribe =
            Subscribe { connection: 0, size: None, mode: AttachMode::MetaOnly, start, desktop: false, device: None };
        assert!(host.shared.deliver(stuck, Inbox::Subscribe(subscribe)));
        stuck_now.recv_timeout(WAIT).unwrap();
        (stuck, release)
    }

    /// 列会话、读屏幕要等会话线程回话，等的时候同一条连接上的输入照常转发：这里有个会话的线程
    /// 卡在一个不返回的 `Subscribe::start` 里，答不了话。
    #[test]
    fn waiting_for_answers_does_not_hold_up_input() {
        let host = Host::new(BuildId("test".into()));
        let (stuck, release) = stuck_session(&host);
        let (mut stream, frames) = greet(&host, ClientKind::Cli);
        let other = host.shared.spawn(options("/bin/cat")).unwrap();
        send(&mut stream, &ClientMsg::Attach { id: other, size: None, mode: AttachMode::VtReplay });
        let channel = loop {
            if let HostMsg::Attached { channel, .. } = message(&frames) {
                break channel;
            }
        };
        send(&mut stream, &ClientMsg::ListSessions);
        send(&mut stream, &ClientMsg::ReadScreen { id: stuck, lines: None, command: None });
        write_frame(&mut stream, FrameKind::Input, channel, b"through\r").unwrap();
        let mut output = Vec::new();
        while !output.windows(7).any(|w| w == b"through") {
            let frame = frames.recv_timeout(WAIT).expect("timed out");
            match frame.kind {
                FrameKind::Output if frame.channel == channel => output.extend_from_slice(&frame.payload),
                FrameKind::Control => assert!(
                    !matches!(frame.message().unwrap(), HostMsg::SessionList { .. } | HostMsg::ScreenText { .. }),
                    "answered while a session was stuck"
                ),
                _ => {}
            }
        }
        drop(release);
        let mut answers = Vec::new();
        while answers.len() < 2 {
            match message(&frames) {
                HostMsg::SessionList { sessions } => {
                    assert_eq!(sessions.len(), 2);
                    answers.push("list");
                }
                HostMsg::ScreenText { id, .. } => {
                    assert_eq!(id, stuck);
                    answers.push("read");
                }
                _ => {}
            }
        }
        host.shared.kill_all();
    }

    /// 一条连接上在等回话的请求有上限：超出的当场回 `Error`，回完话的名额放开，之后的请求照常。
    #[test]
    fn too_many_waiting_requests_are_refused() {
        let host = Host::new(BuildId("test".into()));
        let (stuck, release) = stuck_session(&host);
        let (mut stream, frames) = greet(&host, ClientKind::Cli);
        for _ in 0..=MAX_WAITING {
            send(&mut stream, &ClientMsg::ReadScreen { id: stuck, lines: None, command: None });
        }
        match message(&frames) {
            HostMsg::Error { id: Some(id), message, .. } => {
                assert_eq!(id, stuck);
                assert!(message.contains("too many"), "{message}");
            }
            other => panic!("expected the extra request to be refused: {other:?}"),
        }
        drop(release);
        let mut answered = 0;
        while answered < MAX_WAITING {
            match message(&frames) {
                HostMsg::ScreenText { id, .. } if id == stuck => answered += 1,
                other => panic!("unexpected {other:?}"),
            }
        }
        send(&mut stream, &ClientMsg::ListSessions);
        assert!(matches!(message(&frames), HostMsg::SessionList { .. }));
        host.shared.kill_all();
    }

    /// 发请求的连接断开后，它转给界面、界面还没回话的请求跟着撤掉；界面之后再回话也只是丢掉。
    #[test]
    fn requests_from_a_closed_connection_are_forgotten() {
        let host = Host::new(BuildId("test".into()));
        let (mut desktop, desktop_frames) = greet(&host, ClientKind::Desktop);
        let (mut cli, _cli_frames) = greet(&host, ClientKind::Cli);
        send(&mut cli, &ClientMsg::Reveal { req: 1, id: SessionId(9) });
        let HostMsg::UiRequest { ui, .. } = message(&desktop_frames) else { panic!("expected a ui request") };
        assert_eq!(host.shared.peers().pending.len(), 1);
        cli.shutdown(Shutdown::Both).unwrap();
        let deadline = Instant::now() + WAIT;
        while !host.shared.peers().pending.is_empty() {
            assert!(Instant::now() < deadline, "the request outlived its connection");
            thread::sleep(Duration::from_millis(5));
        }
        send(&mut desktop, &ClientMsg::UiReply { ui, reply: Box::new(HostMsg::Done { req: 1 }) });
        send(&mut desktop, &ClientMsg::ListSessions);
        assert!(matches!(message(&desktop_frames), HostMsg::SessionList { .. }));
    }

    /// 处理消息时 panic 的会话（这里是连着的前端的 `EventSink` 一收到输出就 panic）：前端先收到
    /// 带着会话标识的 `Error`，再收到 `Exited`，会话线程随后结束。
    #[test]
    fn a_panicking_session_tells_its_front_end() {
        let host = Host::new(BuildId("test".into()));
        let id = host.shared.spawn(options("/bin/cat")).unwrap();
        let (tx, rx) = mpsc::channel();
        let start = Box::new(move |_: Screen| {
            let sink: EventSink = Box::new(move |event| match event {
                Event::Output(_) => panic!("the sink cannot take output"),
                Event::Msg(message) => tx.send(*message).is_ok(),
            });
            Some(sink)
        });
        let subscribe =
            Subscribe { connection: 0, size: None, mode: AttachMode::VtReplay, start, desktop: false, device: None };
        assert!(host.shared.deliver(id, Inbox::Subscribe(subscribe)));
        assert!(host.shared.deliver(id, Inbox::Input { connection: 0, data: b"boom\r".to_vec() }));
        let next = || loop {
            match rx.recv_timeout(WAIT).expect("timed out") {
                HostMsg::Meta { .. } | HostMsg::Resized { .. } | HostMsg::SizeOwner { .. } => {}
                message => return message,
            }
        };
        match next() {
            HostMsg::Error { req: None, id: Some(errored), message } => {
                assert_eq!(errored, id);
                assert!(message.contains("crashed"), "{message}");
            }
            other => panic!("expected an error first: {other:?}"),
        }
        assert!(matches!(next(), HostMsg::Exited { id: exited, .. } if exited == id));
        let deadline = Instant::now() + WAIT;
        while host.shared.deliver(id, Inbox::ClearScreen) {
            assert!(Instant::now() < deadline, "the session thread kept running");
            thread::sleep(Duration::from_millis(5));
        }
        host.shared.kill_all();
    }
}
