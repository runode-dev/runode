//! socket 上的前端：`Host::listen` 在一个 Unix socket 上等别的进程连上来，每条连接按
//! `runode_protocol` 的帧收发，读和写各一个线程。
//!
//! 读的线程先等 `Hello`，回 `Welcome`（协议版本对不上时回 `Incompatible` 后断开），之后把控制
//! 消息和输入帧转给宿主。写的线程按到达的先后把帧写出去：连上的会话在自己的线程里把事件交给
//! 这条连接的 `Outbox`，一个会话的帧在连接上的先后就是它发生的先后。前端读得太慢、积压超过
//! `OUTBOX_LIMIT` 时，那个会话不再往这条连接发输出，改发 `Resync`，前端重新 `Attach`。
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
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow};
use runode_protocol::{
    AttachMode, BuildId, ClientMsg, Frame, FrameError, FrameKind, GoodbyeReason, HostMsg, PROTOCOL_VERSION, SessionId,
    SessionInfo, read_frame, write_frame,
};
use runode_shared_types::grid::GridSize;

use crate::{
    Client, Host, HostEvent, Sink, SpawnOptions,
    session::{Inbox, Screen, Subscribe},
};

/// 快照分成这么大的帧发，不必一帧装下整份（上限见 `runode_protocol::MAX_PAYLOAD`）。
const SNAPSHOT_CHUNK: usize = 1 << 20;
/// 一条连接最多积压这么多字节还没写出去，再多就让输出最多的会话改发 `Resync`。
const OUTBOX_LIMIT: usize = 32 << 20;
/// 等会话线程回话（列会话、读屏幕）的最长时间。
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// 接受连接出错（比如文件描述符用完了）后等一会儿再接，免得空转。
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

impl Host {
    /// 在 `socket` 上监听别的进程来的前端，之后在后台线程里一直接受连接。
    ///
    /// `lock` 是同一目录里的锁文件：拿到它的进程才监听，另一个 runode 已经在监听时返回错误。
    /// 拿到锁以后，`socket` 上已有的文件只能是上次没清掉的，删掉重建。`build` 是这次构建的
    /// 标识，前端的一样时才给快照，见 `AttachMode`。
    pub fn listen(&self, socket: &Path, lock: &Path, build: BuildId) -> Result<()> {
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
        let snapshot_format = runode_terminal::host_session::snapshot_format().unwrap_or_else(|err| {
            tracing::warn!("cannot tell the snapshot format: {err}");
            0
        });
        let server = Arc::new(Server { host: self.clone(), build, snapshot_format });
        thread::Builder::new()
            .name("host-listener".into())
            .spawn(move || {
                // 锁跟着监听的线程，一直拿着。
                let _lock = lock;
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            let server = server.clone();
                            let spawned = thread::Builder::new()
                                .name("host-connection".into())
                                .spawn(move || server.serve(stream));
                            if let Err(err) = spawned {
                                tracing::warn!("failed to start a connection thread: {err}");
                            }
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

    /// 所有会话在 `SessionList` 里的样子，按标识排好。先问遍所有会话线程再一起等回话。
    fn session_list(&self) -> Vec<SessionInfo> {
        let replies: Vec<_> = self
            .shared
            .handles()
            .into_iter()
            .filter_map(|(_, handle)| {
                let (reply, info) = mpsc::channel();
                handle.send(Inbox::Info(reply)).then_some(info)
            })
            .collect();
        replies.into_iter().filter_map(|info| info.recv_timeout(REPLY_TIMEOUT).ok()).collect()
    }
}

/// 打开并锁住锁文件，拿不到时返回错误。进程退出时锁自动放开。
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

struct Server {
    host: Host,
    build: BuildId,
    snapshot_format: u16,
}

impl Server {
    /// 一条连接从头到尾：起写的线程，在这个线程里读，读完了断开连着的会话。写的线程等所有
    /// 会话都放开这条连接、积压的帧写完后自己结束。
    fn serve(&self, stream: UnixStream) {
        if !same_user(&stream) {
            tracing::warn!("refused a connection from another user");
            return;
        }
        let write_half = match stream.try_clone() {
            Ok(stream) => stream,
            Err(err) => {
                tracing::warn!("failed to set up a connection: {err}");
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        let out = Outbox { tx, queued: Arc::default() };
        let queued = out.queued.clone();
        if let Err(err) = thread::Builder::new()
            .name("host-connection-writer".into())
            .spawn(move || write_out(write_half, &rx, &queued))
        {
            tracing::warn!("failed to start a connection writer: {err}");
            return;
        }
        let mut connection = Connection {
            server: self,
            client: self.host.connect_in_process(),
            out,
            snapshots: false,
            channels: HashMap::new(),
            next_channel: 1,
        };
        connection.run(&mut BufReader::new(&stream));
        connection.detach_all();
    }
}

/// 一条连接上等着写出去的帧。可以随意克隆，各份往同一个写的线程送。
#[derive(Clone)]
struct Outbox {
    tx: mpsc::Sender<Out>,
    /// 已经交给写的线程、还没写出去的载荷字节数。
    queued: Arc<AtomicUsize>,
}

struct Out {
    kind: FrameKind,
    channel: u32,
    payload: Arc<[u8]>,
}

impl Outbox {
    /// 交一帧给写的线程；连接已经断了时返回 false。
    fn push(&self, kind: FrameKind, channel: u32, payload: Arc<[u8]>) -> bool {
        self.queued.fetch_add(payload.len(), Ordering::Relaxed);
        self.tx.send(Out { kind, channel, payload }).is_ok()
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

    fn backed_up(&self) -> bool {
        self.queued.load(Ordering::Relaxed) > OUTBOX_LIMIT
    }
}

/// 写的线程：有帧就写，一批写完再刷出去。写不出去（对面断了）时关掉连接，读的那边也随之结束。
fn write_out(stream: UnixStream, frames: &mpsc::Receiver<Out>, queued: &AtomicUsize) {
    let mut writer = BufWriter::with_capacity(64 << 10, &stream);
    let mut write_all = || -> Result<(), FrameError> {
        while let Ok(first) = frames.recv() {
            let mut next = Some(first);
            while let Some(out) = next {
                write_frame(&mut writer, out.kind, out.channel, &out.payload)?;
                queued.fetch_sub(out.payload.len(), Ordering::Relaxed);
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
struct Connection<'a> {
    server: &'a Server,
    /// 这条连接在宿主那里的身份：`Detach` 按它找到这条连接的订阅。
    client: Client,
    out: Outbox,
    /// 前端解得了快照：自己说能解，构建也和宿主一样。
    snapshots: bool,
    /// 连着的会话，按通道。
    channels: HashMap<u32, SessionId>,
    next_channel: u32,
}

impl Connection<'_> {
    fn run(&mut self, reader: &mut BufReader<&UnixStream>) {
        let hello = match read_frame(reader) {
            Ok(Some(frame)) if frame.kind == FrameKind::Control => frame.message::<ClientMsg>().ok(),
            Ok(_) => None,
            Err(err) => {
                tracing::debug!("connection closed before hello: {err}");
                return;
            }
        };
        let Some(ClientMsg::Hello { protocol, build, caps, .. }) = hello else {
            self.goodbye("the first message must be hello");
            return;
        };
        if protocol != PROTOCOL_VERSION {
            self.out.control(&HostMsg::Incompatible {
                protocol: PROTOCOL_VERSION,
                build: self.server.build.clone(),
                reason: format!("the host speaks protocol {PROTOCOL_VERSION}, the client {protocol}"),
            });
            return;
        }
        self.snapshots = caps.snapshot && build == self.server.build;
        self.out.control(&HostMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            build: self.server.build.clone(),
            host_pid: std::process::id(),
            snapshot_format: self.server.snapshot_format,
        });
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
                    Some(&id) => self.client.input(id, frame.payload),
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
                self.out.control(&HostMsg::SessionList { sessions: self.server.host.session_list() });
            }
            ClientMsg::Spawn { req, size, cwd, integration } => {
                let options = SpawnOptions { size, cwd, integration, start: true, shell: None, settings: None };
                match self.client.spawn_with(options, false) {
                    Ok(id) => self.out.control(&HostMsg::Spawned { req, id }),
                    Err(err) => {
                        self.out.control(&HostMsg::Error { req: Some(req), id: None, message: format!("{err:#}") })
                    }
                };
            }
            ClientMsg::Attach { id, size, mode } => self.attach(id, size, mode),
            ClientMsg::Detach { id } => {
                self.forget(id);
                self.client.send(message);
            }
            ClientMsg::Kill { id } => {
                self.forget(id);
                self.client.send(message);
            }
            ClientMsg::ReadScreen { id, lines } => self.read_screen(id, lines),
            ClientMsg::Handoff | ClientMsg::Shutdown { .. } => self.error(
                None,
                None,
                "the host runs inside the runode app and cannot hand off or shut down on its own".into(),
            ),
            ClientMsg::Unknown => self.error(None, None, "unknown message".into()),
            // 桌面那份 VT 在 `HostMsg::ThemeApplied` 处套的是桌面自己的配置，不是宿主套的那份；
            // 别的进程改了主题，两份 VT 就分叉了。主题和选项现在只跟着桌面的配置走。
            ClientMsg::SetTheme { .. } | ClientMsg::SetOptions { .. } => {
                self.error(None, None, "the theme and options follow the runode app's config".into())
            }
            ClientMsg::Resize { .. } | ClientMsg::Focus { .. } | ClientMsg::ClearScreen { .. } => {
                self.client.send(message)
            }
        }
    }

    /// 连上会话。已经连着的先断开再重新连，前端收到 `Resync` 后就是这样重新连上的。
    fn attach(&mut self, id: SessionId, size: Option<GridSize>, mode: AttachMode) {
        if self.forget(id) {
            self.client.send(ClientMsg::Detach { id });
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
            let attached = HostMsg::Attached { id, channel, size: screen.size, mode: screen.mode, meta: screen.meta };
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
        let subscribe = Subscribe { connection: self.client.connection, size, mode, start };
        if self.client.shared.deliver(id, Inbox::Subscribe(subscribe)) {
            self.channels.insert(channel, id);
        } else {
            self.error(None, Some(id), format!("no session {id}"));
        }
    }

    fn read_screen(&self, id: SessionId, lines: Option<u32>) {
        let (reply, text) = mpsc::channel();
        if !self.client.shared.deliver(id, Inbox::ReadScreen { lines, reply }) {
            self.error(None, Some(id), format!("no session {id}"));
            return;
        }
        match text.recv_timeout(REPLY_TIMEOUT) {
            Ok(Ok(text)) => {
                self.out.control(&HostMsg::ScreenText { id, text });
            }
            Ok(Err(err)) => self.error(None, Some(id), format!("failed to read the screen: {err:#}")),
            Err(_) => self.error(None, Some(id), format!("session {id} did not answer")),
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
            self.client.send(ClientMsg::Detach { id });
        }
    }

    fn error(&self, req: Option<u32>, id: Option<SessionId>, message: String) {
        self.out.control(&HostMsg::Error { req, id, message });
    }

    fn goodbye(&self, message: &str) {
        self.out.control(&HostMsg::Goodbye { reason: GoodbyeReason::Error { message: message.into() } });
    }
}

/// 一个会话往这条连接发事件的 `Sink`。`MetaOnly` 的前端没有 VT，只要状态，不要输出和改 VT 的
/// 标记。
fn session_sink(id: SessionId, channel: u32, meta_only: bool, out: Outbox) -> Sink {
    Box::new(move |event| match event {
        HostEvent::Output(_) if meta_only => true,
        HostEvent::Output(data) => {
            if out.backed_up() {
                out.control(&HostMsg::Resync { id, reason: "the client reads too slowly".into() });
                return false;
            }
            out.push(FrameKind::Output, channel, data)
        }
        HostEvent::Msg(message) => {
            if meta_only && matches!(*message, HostMsg::Resized { .. } | HostMsg::ThemeApplied { .. }) {
                return true;
            }
            out.control(&message)
        }
    })
}
