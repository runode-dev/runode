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
//! 短命的线程等回话、交给 `Outbox`，同一条连接上之后的输入照常转发。开会话还在读的线程里办
//! （开伪终端、启动 shell 要几毫秒）。
//!
//! 能连上 socket 就能读写所有终端：socket 放在只有自己能进的目录里（见调用方建目录的方式），
//! 连上来的进程也要是同一个用户的。

use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{self, BufReader, BufWriter, Write as _},
    net::Shutdown,
    os::unix::{
        fs::{OpenOptionsExt as _, PermissionsExt as _},
        io::AsRawFd as _,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use runode_protocol::{
    AttachMode, ClientKind, ClientMsg, Frame, FrameError, FrameKind, GoodbyeReason, HostMsg, PROTOCOL_VERSION,
    SessionId, SessionInfo, read_frame, write_frame,
};
use runode_shared_types::{grid::GridSize, input::parse_keys, session::DriveAction};
use runode_terminal::pty;

use crate::{
    Host, Shared, SpawnOptions, Stopped,
    session::{Drive, Event, EventSink, Inbox, Screen, Subscribe},
};

/// 快照分成这么大的帧发，不必一帧装下整份（上限见 `runode_protocol::MAX_PAYLOAD`）。
const SNAPSHOT_CHUNK: usize = 1 << 20;
/// 一条连接最多积压这么多字节还没写出去，再多就让输出最多的会话改发 `Resync`。
const OUTBOX_LIMIT: usize = 32 << 20;
/// 等会话线程回话（列会话、读屏幕）的最长时间。
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
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
    pub(crate) standalone: bool,
    /// 要退出了，为什么。
    pub(crate) stop: Option<Stopped>,
    /// 最近一次有连接连上或者断开的时刻，空闲从这时起算。
    pub(crate) activity_at: Instant,
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
            activity_at: Instant::now(),
        }
    }
}

impl Peers {
    pub(crate) fn connection_count(&self) -> usize {
        self.connections.len()
    }
}

struct Peer {
    out: Option<Outbox>,
}

/// 转给界面、还没回话的一条请求。
struct Pending {
    /// 转给了哪条界面的连接；只认它回的话。
    desktop: u64,
    /// 发请求的那条连接，回话交给它。
    origin: Outbox,
    req: u32,
}

/// `Host::listen` 开着的 socket。丢掉时放开锁，socket 文件由退出的一方删。
pub(crate) struct Listening {
    pub(crate) socket: PathBuf,
    _lock: File,
}

impl Host {
    /// 在 `socket` 上监听别的进程来的前端，之后在后台线程里一直接受连接，直到宿主退出（见
    /// `Host::run_until_idle`）。
    ///
    /// `lock` 是同一目录里的锁文件：拿到它的进程才监听，另一个宿主已经在监听时返回错误。
    /// 拿到锁以后，`socket` 上已有的文件只能是上次没清掉的，删掉重建。
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
        // 之后开的 shell 里的命令行连这个 socket，开发版和装好的版本同时开着时也不会连错。
        self.set_env(runode_protocol::ENV_SOCKET, socket.as_os_str());
        self.shared.peers().listening = Some(Listening { socket: socket.into(), _lock: lock });
        let shared = self.shared.clone();
        thread::Builder::new()
            .name("host-listener".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            // 判断还接不接和登记连接在同一把锁里，空闲退出时不会漏掉刚连上的。
                            let Some(id) = register(&shared) else {
                                drop(stream);
                                continue;
                            };
                            start_serving(&shared, id, stream, true);
                        }
                        Err(err) => {
                            tracing::warn!("failed to accept a connection: {err}");
                            thread::sleep(ACCEPT_BACKOFF);
                        }
                    }
                }
            })
            .context("failed to start the listener thread")?;
        Ok(())
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

/// 连接结束：撤掉登记。它是界面的话，转给它还没回话的请求都回一句没办成。
fn unregister(shared: &Shared, id: u64) {
    let mut peers = shared.peers();
    peers.connections.remove(&id);
    peers.desktops.retain(|&desktop| desktop != id);
    let orphaned: Vec<u64> =
        peers.pending.iter().filter(|(_, pending)| pending.desktop == id).map(|(&ui, _)| ui).collect();
    for ui in orphaned {
        if let Some(pending) = peers.pending.remove(&ui) {
            pending.origin.control(&HostMsg::Error {
                req: Some(pending.req),
                id: None,
                message: "the runode window went away before answering".into(),
            });
        }
    }
    peers.activity_at = Instant::now();
    drop(peers);
    shared.peers_changed.notify_all();
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
        for peer in peers.connections.values() {
            if let Some(out) = &peer.out {
                out.control(&HostMsg::Goodbye { reason: GoodbyeReason::Shutdown });
                out.close();
            }
        }
        drop(peers);
        self.peers_changed.notify_all();
        true
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
fn same_user(stream: &UnixStream) -> bool {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: 描述符来自 `stream`，两个输出参数指向本地变量。
    let ok = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } == 0;
    // SAFETY: 没有参数，总是成功。
    ok && uid == unsafe { libc::geteuid() }
}

/// 把 socket 的收发缓冲设成 `SOCKET_BUFFER`；设不了时记日志，照常用。
fn set_buffers(stream: &UnixStream) {
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
        // 登记之后、写线程起好之前宿主要退出了：这条连接也收 `Goodbye`。
        if peers.stop == Some(Stopped::Shutdown) {
            out.control(&HostMsg::Goodbye { reason: GoodbyeReason::Shutdown });
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
        snapshots: false,
        channels: HashMap::new(),
        next_channel: 1,
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
struct Outbox {
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
}

impl Outbox {
    /// 交一帧给写的线程；连接已经断了时返回 false。
    fn push(&self, kind: FrameKind, channel: u32, payload: Arc<[u8]>) -> bool {
        self.queued.fetch_add(payload.len(), Ordering::Relaxed);
        self.tx.send(Out::Frame { kind, channel, payload }).is_ok()
    }

    fn control(&self, message: &HostMsg) -> bool {
        match Frame::control(message) {
            Ok(frame) => self.push(FrameKind::Control, 0, frame.payload.into()),
            Err(err) => {
                tracing::warn!("failed to encode {message:?}: {err}");
                true
            }
        }
    }

    /// 已经交给写线程的帧写完后断开连接，之后交的不再写。
    fn close(&self) {
        let _ = self.tx.send(Out::Close);
    }

    fn backed_up(&self) -> bool {
        self.queued.load(Ordering::Relaxed) > OUTBOX_LIMIT
    }
}

/// 写的线程：有帧就写，一批写完再刷出去。写不出去（对面断了）或者要断开时关掉连接，读的那边
/// 也随之结束。
fn write_out(stream: UnixStream, frames: &mpsc::Receiver<Out>, queued: &AtomicUsize) {
    let mut writer = BufWriter::with_capacity(WRITE_BUFFER, &stream);
    let mut write_all = || -> Result<(), FrameError> {
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
                        return Ok(());
                    }
                }
                next = frames.try_recv().ok();
            }
            writer.flush()?;
        }
        Ok(())
    };
    if let Err(err) = write_all() {
        tracing::debug!("connection closed while writing: {err}");
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
    /// 前端解得了快照：自己说能解，构建也和宿主一样。
    snapshots: bool,
    /// 连着的会话，按通道。
    channels: HashMap<u32, SessionId>,
    next_channel: u32,
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
        let Some(ClientMsg::Hello { protocol, build, caps, client, session }) = hello else {
            self.goodbye("the first message must be hello");
            return;
        };
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
        self.snapshots = caps.snapshot && build == self.shared.build;
        // 登记界面和回 `Welcome` 在同一把锁里做：界面收到 `Welcome` 后，别的连接转来的请求一定
        // 交给它；别的连接也只有在 `Welcome` 排进 `Outbox` 之后才看得到它，请求不会抢在前面。
        let mut peers = self.shared.peers();
        if client == ClientKind::Desktop {
            peers.desktops.push(self.id);
        }
        self.out.control(&HostMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            build: self.shared.build.clone(),
            host_pid: std::process::id(),
            snapshot_format: self.shared.snapshot_format,
        });
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
                        if !self.shared.deliver(id, Inbox::Input(frame.payload)) {
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

    fn handle(&mut self, message: ClientMsg) {
        match message {
            ClientMsg::Hello { .. } => self.error(None, None, "already said hello".into()),
            ClientMsg::ListSessions => {
                let replies = self.shared.ask_sessions();
                let out = self.out.clone();
                answer_later("host-list-sessions", move || {
                    out.control(&HostMsg::SessionList { sessions: collect_sessions(replies) });
                });
            }
            ClientMsg::Spawn { req, size, cwd, integration, start, shell, settings, env } => {
                let options = SpawnOptions { size, cwd, integration, start, shell, settings };
                match self.shared.spawn(options, env) {
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
                self.shared.send(id, Inbox::Detach { connection: self.id });
            }
            ClientMsg::Kill { id } => {
                self.drive(id, DriveAction::Kill);
                self.forget(id);
                self.shared.kill(id);
            }
            ClientMsg::Resize { id, size } => self.shared.send(id, Inbox::Resize(size)),
            ClientMsg::ClearScreen { id } => {
                self.drive(id, DriveAction::ClearScreen);
                self.shared.send(id, Inbox::ClearScreen);
            }
            ClientMsg::SetTheme { settings } => self.shared.set_theme(settings),
            // 记不记命令历史是用户在 app 里的设置，别的程序不能改。
            ClientMsg::SetOptions { record_history } => {
                if self.kind == ClientKind::Desktop {
                    self.shared.record_history.store(record_history, Ordering::Relaxed);
                } else {
                    self.error(None, None, "only the runode app changes the host's options".into());
                }
            }
            // 通知在界面那边发，宿主不用知道哪个会话被看着。
            ClientMsg::Focus { .. } => {}
            ClientMsg::ReadScreen { id, lines, command } => self.read_screen(id, lines, command),
            ClientMsg::SendKeys { req, id, keys } => {
                let parsed: Result<Vec<_>, _> = keys.iter().map(|key| parse_keys(key)).collect();
                match parsed {
                    Ok(keys) => self.deliver_done(req, id, DriveAction::Keys, Inbox::Keys(keys.concat())),
                    Err(err) => self.error(Some(req), Some(id), err.to_string()),
                }
            }
            ClientMsg::Paste { req, id, text } => self.deliver_done(req, id, DriveAction::Paste, Inbox::Paste(text)),
            ClientMsg::Open { req, .. } | ClientMsg::Reveal { req, .. } | ClientMsg::Layout { req } => {
                self.to_ui(req, message);
            }
            ClientMsg::UiReply { ui, reply } => self.ui_reply(ui, *reply),
            ClientMsg::Shutdown { kill_sessions: true } => {
                self.channels.clear();
                self.shared.kill_all();
                self.shared.shut_down();
            }
            ClientMsg::Shutdown { kill_sessions: false } | ClientMsg::Handoff => {
                self.error(None, None, "the host cannot hand its sessions over yet".into());
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
            peers.pending.insert(ui, Pending { desktop, origin: self.out.clone(), req });
            drop(peers);
            if !out.control(&HostMsg::UiRequest { ui, request: Box::new(request) }) {
                // 界面的连接刚断开：它的读线程撤掉登记时会替还在等的请求回话；已经撤掉了的话
                // 这条请求是登记之后才加的，在这里回。
                let pending = self.shared.peers().pending.remove(&ui);
                if let Some(pending) = pending {
                    pending.origin.control(&HostMsg::Error {
                        req: Some(pending.req),
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

    /// 界面回话：转给发请求的一方。只认被转去的那个界面回的；对不上的（请求已经回过、不是转给
    /// 这条连接的）丢掉。
    fn ui_reply(&self, ui: u64, reply: HostMsg) {
        let pending = {
            let mut peers = self.shared.peers();
            match peers.pending.get(&ui) {
                Some(pending) if pending.desktop == self.id => peers.pending.remove(&ui),
                _ => None,
            }
        };
        match pending {
            Some(pending) => {
                pending.origin.control(&reply);
            }
            None => tracing::debug!("dropped a reply to unknown ui request {ui}"),
        }
    }

    /// 连上会话。已经连着的先断开再重新连，前端收到 `Resync` 后就是这样重新连上的；
    /// `MetaOnly` 和看屏幕之间来回切换也是这样。`Detach` 和之后的 `Subscribe` 按先后送到会话
    /// 线程，旧通道的帧都在新的 `Attached` 之前，新通道的帧都在它之后。
    fn attach(&mut self, id: SessionId, size: Option<GridSize>, mode: AttachMode) {
        if self.forget(id) {
            self.shared.send(id, Inbox::Detach { connection: self.id });
        }
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
        let subscribe = Subscribe { connection: self.id, size, mode, start, desktop };
        if self.shared.deliver(id, Inbox::Subscribe(subscribe)) {
            self.channels.insert(channel, id);
        } else {
            self.error(None, Some(id), format!("no session {id}"));
        }
    }

    /// 读屏幕，`command` 给了时读倒数第几条命令的输出：请求在这里送到会话线程（排在这条连接
    /// 之前送去的输入后面），回话另起线程等。
    fn read_screen(&self, id: SessionId, lines: Option<u32>, command: Option<u32>) {
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
        });
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
            self.shared.send(id, Inbox::Detach { connection: self.id });
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

    /// 列会话、读屏幕要等会话线程回话，等的时候同一条连接上的输入照常转发：这里有个会话的线程
    /// 卡在一个不返回的 `Subscribe::start` 里，答不了话。
    #[test]
    fn waiting_for_answers_does_not_hold_up_input() {
        let host = Host::new(BuildId("test".into()));
        let options = |shell: &str| SpawnOptions {
            size: SIZE,
            cwd: None,
            integration: IntegrationMode::Off,
            start: true,
            shell: Some(shell.into()),
            settings: None,
        };
        let stuck = host.shared.spawn(options("/bin/cat"), Vec::new()).unwrap();
        let (release, released) = mpsc::channel::<()>();
        let (entered, stuck_now) = mpsc::channel();
        let start = Box::new(move |_: Screen| {
            let _ = entered.send(());
            // 卡住会话线程，直到测试放开（丢掉发送的一端）。
            let _ = released.recv();
            None
        });
        let subscribe = Subscribe { connection: 0, size: None, mode: AttachMode::MetaOnly, start, desktop: false };
        assert!(host.shared.deliver(stuck, Inbox::Subscribe(subscribe)));
        stuck_now.recv_timeout(WAIT).unwrap();

        let mut stream = host.connect_pair().unwrap();
        let frames = frames(&stream);
        send(
            &mut stream,
            &ClientMsg::Hello {
                protocol: PROTOCOL_VERSION,
                build: BuildId("test".into()),
                client: ClientKind::Cli,
                caps: Caps::default(),
                session: None,
            },
        );
        assert!(matches!(message(&frames), HostMsg::Welcome { .. }));
        let other = host.shared.spawn(options("/bin/cat"), Vec::new()).unwrap();
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
}
