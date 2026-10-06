//! 桌面和宿主之间的那条连接：一条 Unix socket 承载所有会话，按 `runode_protocol` 的帧说话。
//! 宿主跑在 app 里时是 `Host::connect_pair` 给的一端，单独一个进程时是连它的 socket。
//!
//! 写：调用方（主线程、提前拉起 shell 的线程）在锁里直接写帧，每帧写完就刷出去，不经别的线程
//! 转手。socket 设了发送超时（`WRITE_TIMEOUT`），宿主卡住、一直写不出去时按连接断开处理。
//!
//! 读：一个 `host-link-reader` 线程读帧，按会话分发成 `LinkEvent`：输出帧按通道找到会话；快照帧
//! 拼好到 `SnapshotEnd` 作为一份 `Screen` 交出去；带 `req` 的回话交给等着的调用方；
//! `UiRequest` 交给界面（见 `Link::ui_requests`）；连接断开时给每个会话发 `LinkEvent::Lost`。
//!
//! 连上一个会话（`attach`）时先登记再发 `Attach`。发出 `Attach` 到收到对应的 `Attached` 之间，
//! 这个会话的输出和标记一律丢掉：宿主按先后处理同一条连接上的消息，旧订阅的帧都排在新的
//! `Attached` 前面，新订阅从一份新的屏幕开始，旧的帧用不上。只有 `Exited` 留着，等这次连上（或者
//! 没连成）之后再交出去：会话正好在这时被结束（比如重连时撞上 `runode kill`）的话，新的订阅连不上，
//! 视图只能靠它知道会话没了。
//!
//! 宿主编的快照格式（`HostMsg::Welcome::snapshot_format`）和这边解得了的不一样时，要快照一律改要
//! VT 重放。
//!
//! 换主题和改选项（`SetTheme`、`SetOptions`）记着最近一次的，重连后补发。

use std::{
    collections::{HashMap, VecDeque},
    io::{self, BufReader, BufWriter, Write as _},
    net::Shutdown,
    os::unix::{io::AsRawFd as _, net::UnixStream},
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Result, anyhow};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use runode_protocol::{
    AttachMode, BuildId, Caps, ClientKind, ClientMsg, Frame, FrameError, FrameKind, GoodbyeReason, HostMsg,
    PROTOCOL_VERSION, SessionId, SessionInfo, read_frame, write_frame,
};
use runode_shared_types::{grid::GridSize, session::SessionMeta, settings::TermSettings, shell::IntegrationMode};

/// 写一帧最多等这么久，超时按连接断开处理。
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// 握手（`Hello` 到 `Welcome`）最多等这么久。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// 开会话最多等这么久的回话。
const SPAWN_TIMEOUT: Duration = Duration::from_secs(5);
/// 读线程的缓冲：刷屏时一次读进一大块，少几次系统调用。
const READ_BUFFER: usize = 256 << 10;
/// 写的缓冲：帧头和小的载荷攒成一次写；更大的载荷直接写。
const WRITE_BUFFER: usize = 64 << 10;
/// socket 的收发缓冲。macOS 上默认只有 8 KiB，刷屏的输出一块就塞满；经 `Host::connect_pair` 拿到的
/// 一端宿主已经设好，连 socket 的这里自己设。
const SOCKET_BUFFER: libc::c_int = 4 << 20;

/// 宿主给的一个会话的样子，来自 `HostMsg::Attached`。
#[derive(Clone, Debug, PartialEq)]
pub struct Attached {
    pub id: SessionId,
    /// 这个会话的帧在这条连接上用的通道。
    pub channel: u32,
    pub size: GridSize,
    /// 宿主实际给的，要快照、构建又不一样时退成 `VtReplay`。
    pub mode: AttachMode,
    pub meta: SessionMeta,
    /// 宿主那份 VT 现在套着的主题；`VtReplay` 时按它新建界面的 VT。
    pub settings: Option<TermSettings>,
}

/// 连上会话时宿主给的一份屏幕：`Attached` 和拼好的快照（或 VT 重放）。`MetaOnly` 时 `data` 为空。
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    pub attached: Attached,
    pub data: Vec<u8>,
}

/// 一个会话收到的一件事，按发生的先后。
#[derive(Debug)]
pub enum LinkEvent {
    /// 连上（或重新连上）了，从这份屏幕接着喂之后的输出。
    Screen(Screen),
    /// PTY 的输出。
    Output(Vec<u8>),
    /// 这个会话的控制消息：`Resized`、`ThemeApplied`、`Meta`、`CommandFinished`、`Bell`、`Exited`、
    /// `Resync`，以及带着它的 `Error`。
    Msg(HostMsg),
    /// 和宿主的连接断了，之后不会再有这个会话的事件；重连后要重新 `attach`。
    Lost,
}

/// 新开一个会话，对应 `ClientMsg::Spawn`。
#[derive(Clone, Debug)]
pub struct SpawnOptions {
    pub size: GridSize,
    /// shell 从哪个目录开始，`None` 时从家目录。
    pub cwd: Option<PathBuf>,
    pub integration: IntegrationMode,
    /// 现在就启动 shell；为 false 时等 `ClientMsg::Start`。
    pub start: bool,
    /// 要启动的程序，`None` 时用用户的 `$SHELL`。
    pub shell: Option<String>,
    /// 宿主还没收到过 `SetTheme` 时这个会话一开始套的主题。
    pub settings: Option<TermSettings>,
    /// 启动 shell 时另外设的环境变量。
    pub env: Vec<(String, String)>,
}

/// 连宿主没连成的原因。
#[derive(Debug)]
pub enum ConnectError {
    /// 宿主说协议版本对不上，多半是旧版本的宿主还活着。
    Incompatible(String),
    /// 要连单独一个进程的宿主，socket 上的却跑在另一个 app 里（`Welcome::standalone` 为假），见
    /// `Link::connect_standalone`。
    NotStandalone,
    /// 还没回 `Welcome` 就断开了，多半撞上它正因空闲退出，过一会儿再试。
    Closed,
    Io(io::Error),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Incompatible(reason) => write!(f, "the host speaks another protocol: {reason}"),
            Self::NotStandalone => write!(f, "the host runs inside another runode app"),
            Self::Closed => write!(f, "the host closed the connection before saying welcome"),
            Self::Io(err) => write!(f, "failed to talk to the host: {err}"),
        }
    }
}

impl std::error::Error for ConnectError {}

/// 到宿主的连接。可以随意克隆，各份是同一条连接；断了以后可以用 `connect` 接上新的一条，
/// 之前拿到的各份照常用。
#[derive(Clone)]
pub struct Link {
    inner: Arc<Inner>,
}

struct Inner {
    build: BuildId,
    state: Mutex<State>,
    /// 现在这条连接的写的一端。锁的先后：拿着 `state` 时可以再拿它，反过来不行。
    writer: Mutex<Option<Writer>>,
    ui: UnboundedSender<(u64, ClientMsg)>,
    ui_requests: Mutex<Option<UnboundedReceiver<(u64, ClientMsg)>>>,
    next_req: AtomicU32,
}

struct Writer {
    /// 是第几条连接，见 `State::generation`。
    generation: u64,
    stream: BufWriter<UnixStream>,
}

#[derive(Default)]
struct State {
    /// 接上过几条连接；读线程只处理自己那一条的断开。
    generation: u64,
    connected: bool,
    sessions: HashMap<SessionId, Route>,
    /// 连着的会话，按通道。
    channels: HashMap<u32, SessionId>,
    /// 等回话的调用方，按 `req`。
    replies: HashMap<u32, mpsc::Sender<HostMsg>>,
    /// 等 `SessionList` 的调用方，按发请求的先后。
    lists: VecDeque<mpsc::Sender<Vec<SessionInfo>>>,
    /// 最近一次换的主题和选项，重连后补发。
    theme: Option<TermSettings>,
    record_history: Option<bool>,
    /// 现在这条连接上的宿主编的快照这边解得了，见 `snapshots_usable`；为假时要快照改要 VT 重放。
    snapshots: bool,
}

/// 一个连着（或正连着）的会话。
struct Route {
    events: UnboundedSender<LinkEvent>,
    /// 发出去、还没等到 `Attached`（或者对应的 `Error`）的 `Attach` 有几个；不为 0 时这个会话的帧
    /// 一律丢掉。
    attaching: u32,
    /// 现在的通道；`attaching` 不为 0 时为 `None`。
    channel: Option<u32>,
    /// 收到了 `Attached`，快照还在拼。
    assembling: Option<(Attached, Vec<u8>)>,
    /// 通道还不知道时攒着的输入，收到 `Attached` 时按先后写出去。
    queued: Vec<Vec<u8>>,
    /// `attach_now` 的调用方等着的第一份屏幕；没连成时给它错误。
    first_screen: Option<mpsc::Sender<Result<Screen, String>>>,
    /// 正连着时收到的 `Exited`，等这次连上（或者没连成）后交出去，见 `Route::settle`。
    exited: Option<HostMsg>,
}

impl Route {
    fn new(events: UnboundedSender<LinkEvent>) -> Self {
        Self {
            events,
            attaching: 0,
            channel: None,
            assembling: None,
            queued: Vec::new(),
            first_screen: None,
            exited: None,
        }
    }

    /// 连上（或者没连成）了：交出正连着时留下的 `Exited`。返回收的一方还在不在。
    fn settle(&mut self) -> bool {
        match self.exited.take() {
            Some(exited) if self.attaching == 0 => self.deliver(LinkEvent::Msg(exited)),
            exited => {
                self.exited = exited;
                true
            }
        }
    }

    /// 交给会话一件事，返回收的一方还在不在。
    fn deliver(&mut self, event: LinkEvent) -> bool {
        match event {
            LinkEvent::Screen(screen) if self.first_screen.is_some() => {
                self.first_screen.take().is_some_and(|first| first.send(Ok(screen)).is_ok())
            }
            event => self.events.unbounded_send(event).is_ok(),
        }
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn writer(&self) -> MutexGuard<'_, Option<Writer>> {
        self.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 写一帧并刷出去。没连着时返回错误；写不出去（对面断了、超时）时关掉连接，读线程随之
    /// 读到结尾，给各个会话发 `Lost`。
    fn write(&self, kind: FrameKind, channel: u32, payload: &[u8]) -> io::Result<()> {
        let mut guard = self.writer();
        let Some(writer) = guard.as_mut() else {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "not connected to the host"));
        };
        let written = write_frame(&mut writer.stream, kind, channel, payload)
            .map_err(|err| match err {
                FrameError::Io(err) => err,
                err => io::Error::other(err.to_string()),
            })
            .and_then(|()| writer.stream.flush());
        if let Err(err) = &written {
            tracing::warn!("lost the host while writing: {err}");
            let _ = writer.stream.get_ref().shutdown(Shutdown::Both);
            *guard = None;
        }
        written
    }

    fn control(&self, message: &ClientMsg) -> io::Result<()> {
        let frame = Frame::control(message).map_err(io::Error::other)?;
        self.write(FrameKind::Control, 0, &frame.payload)
    }

    /// 宿主收到 `Shutdown` 后结束了所有会话、说了 `Goodbye`：它不等会话线程就发 `Goodbye`，各会话的
    /// `Exited` 多半赶不上，这里替每个会话补一条 `Exited`，视图按会话结束处理，不当成断开。之后的
    /// 断开（`lost`）不再有会话要通知。
    fn ended_all(&self, generation: u64) {
        let mut state = self.state();
        if state.generation != generation {
            return;
        }
        for (id, mut route) in state.sessions.drain() {
            route.deliver(LinkEvent::Msg(HostMsg::Exited { id, status: None }));
        }
        state.channels.clear();
    }

    /// 连接 `generation` 断了：每个会话收到 `Lost`，等回话的都放弃。已经换了新连接时什么都不做。
    fn lost(&self, generation: u64) {
        let mut state = self.state();
        if state.generation != generation || !state.connected {
            return;
        }
        state.connected = false;
        for (_, mut route) in state.sessions.drain() {
            // 已经知道会话结束了的，先说结束了。
            if let Some(exited) = route.exited.take() {
                route.deliver(LinkEvent::Msg(exited));
            }
            route.deliver(LinkEvent::Lost);
        }
        state.channels.clear();
        state.replies.clear();
        state.lists.clear();
        drop(state);
        let mut writer = self.writer();
        if writer.as_ref().is_some_and(|writer| writer.generation == generation)
            && let Some(writer) = writer.take()
        {
            let _ = writer.stream.get_ref().shutdown(Shutdown::Both);
        }
        tracing::warn!("lost the connection to the host");
    }
}

impl Link {
    /// 还没连上宿主的一条连接，接着用 `connect`。`build` 是这次构建的标识，和宿主的一样时宿主
    /// 给快照。
    pub fn new(build: BuildId) -> Self {
        let (ui, ui_requests) = unbounded();
        let inner = Inner {
            build,
            state: Mutex::default(),
            writer: Mutex::new(None),
            ui,
            ui_requests: Mutex::new(Some(ui_requests)),
            next_req: AtomicU32::new(1),
        };
        Self { inner: Arc::new(inner) }
    }

    /// 在 `stream` 上和宿主握手（以桌面界面的身份），成了就换上这条连接，起读线程，补发记着的
    /// 主题和选项。原来连着的连接先断开，上面的会话收到 `Lost`。
    pub fn connect(&self, stream: UnixStream) -> Result<(), ConnectError> {
        self.connect_to(stream, false)
    }

    /// 同 `connect`，但只连单独一个进程的宿主：握手时宿主说自己跑在某个 app 里（那个 app 才是它的
    /// 界面）时不换上这条连接，返回 `ConnectError::NotStandalone`。
    pub fn connect_standalone(&self, stream: UnixStream) -> Result<(), ConnectError> {
        self.connect_to(stream, true)
    }

    fn connect_to(&self, stream: UnixStream, standalone_only: bool) -> Result<(), ConnectError> {
        set_buffers(&stream);
        stream.set_write_timeout(Some(WRITE_TIMEOUT)).map_err(ConnectError::Io)?;
        let welcome = handshake(&stream, &self.inner.build)?;
        if standalone_only && !welcome.standalone {
            let _ = stream.shutdown(Shutdown::Both);
            return Err(ConnectError::NotStandalone);
        }
        let host_pid = welcome.host_pid;
        let snapshots = snapshots_usable(welcome.snapshot_format, local_snapshot_format());
        if !snapshots {
            tracing::warn!(
                "the host encodes snapshots in format {}, which this build cannot read; attaching with VT replays",
                welcome.snapshot_format
            );
        }
        let reader = stream.try_clone().map_err(ConnectError::Io)?;
        let generation = {
            let mut state = self.inner.state();
            let old = state.generation;
            drop(state);
            self.inner.lost(old);
            state = self.inner.state();
            state.generation += 1;
            state.connected = true;
            state.snapshots = snapshots;
            *self.inner.writer() =
                Some(Writer { generation: state.generation, stream: BufWriter::with_capacity(WRITE_BUFFER, stream) });
            state.generation
        };
        let inner = self.inner.clone();
        let spawned = thread::Builder::new().name("host-link-reader".into()).spawn(move || {
            runode_terminal::pty::set_current_thread_interactive();
            read_loop(&inner, generation, reader);
            inner.lost(generation);
        });
        if let Err(err) = spawned {
            self.inner.lost(generation);
            return Err(ConnectError::Io(err));
        }
        let (theme, record_history) = {
            let state = self.inner.state();
            (state.theme.clone(), state.record_history)
        };
        if let Some(settings) = theme {
            let _ = self.inner.control(&ClientMsg::SetTheme { settings });
        }
        if let Some(record_history) = record_history {
            let _ = self.inner.control(&ClientMsg::SetOptions { record_history });
        }
        tracing::info!("connected to the host (pid {host_pid})");
        Ok(())
    }

    /// 断开现在的连接，各个会话收到 `Lost`。
    #[allow(dead_code)]
    pub fn close(&self) {
        let generation = self.inner.state().generation;
        self.inner.lost(generation);
    }

    /// 现在连着宿主。
    pub fn connected(&self) -> bool {
        self.inner.state().connected
    }

    /// 发一条控制消息，不等回话；没连着或写不出去时记一笔日志。`SetTheme`、`SetOptions` 记下来，
    /// 重连后补发；`Kill`、`Detach` 顺带忘掉这个会话，同 `kill`、`detach`。连会话用 `attach`。
    pub fn send(&self, message: ClientMsg) {
        match &message {
            ClientMsg::SetTheme { settings } => self.inner.state().theme = Some(settings.clone()),
            ClientMsg::SetOptions { record_history } => self.inner.state().record_history = Some(*record_history),
            ClientMsg::Kill { id } | ClientMsg::Detach { id } => self.forget(*id),
            _ => {}
        }
        if let Err(err) = self.inner.control(&message) {
            tracing::debug!("dropped {message:?}: {err}");
        }
    }

    /// 把输入写给会话里的程序。还没等到 `Attached`、不知道通道时先攒着，到时按先后写出去；
    /// 没连着这个会话时丢掉。
    pub fn input(&self, id: SessionId, data: &[u8]) {
        let channel = {
            let mut state = self.inner.state();
            let Some(route) = state.sessions.get_mut(&id) else {
                tracing::debug!("dropped input for session {id}, which is not attached");
                return;
            };
            match route.channel {
                Some(channel) => channel,
                None => {
                    route.queued.push(data.to_vec());
                    return;
                }
            }
        };
        if let Err(err) = self.inner.write(FrameKind::Input, channel, data) {
            tracing::debug!("dropped input for session {id}: {err}");
        }
    }

    /// 新开一个会话，等宿主回话（最多 `SPAWN_TIMEOUT`），返回它的标识。开好的会话不会自动连上，
    /// 接着 `attach`。
    pub fn spawn(&self, options: SpawnOptions) -> Result<SessionId> {
        let SpawnOptions { size, cwd, integration, start, shell, settings, env } = options;
        let req = self.inner.next_req.fetch_add(1, Ordering::Relaxed);
        let reply = self.expect_reply(req)?;
        let spawn = ClientMsg::Spawn { req, size, cwd, integration, start, shell, settings, env };
        if let Err(err) = self.inner.control(&spawn) {
            self.inner.state().replies.remove(&req);
            return Err(anyhow!("failed to ask the host for a terminal: {err}"));
        }
        match reply.recv_timeout(SPAWN_TIMEOUT) {
            Ok(HostMsg::Spawned { id, .. }) => Ok(id),
            Ok(HostMsg::Error { message, .. }) => Err(anyhow!(message)),
            Ok(other) => Err(anyhow!("unexpected answer from the host: {other:?}")),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // 之后才到的 `Spawned` 没人等，读线程见了就结束那个会话。
                self.inner.state().replies.remove(&req);
                Err(anyhow!("the host did not answer in time"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!("lost the connection to the host")),
        }
    }

    /// 连上会话，返回收它的事件的一端，第一件是 `LinkEvent::Screen`（连不上时是带着它的
    /// `LinkEvent::Msg(HostMsg::Error)`）。这个会话原来连着的话，原来那一端不再收到事件。
    /// `size` 是视图的尺寸，宿主按它改会话的尺寸；为 `None` 时不改。
    #[allow(dead_code)]
    pub fn attach(&self, id: SessionId, size: Option<GridSize>, mode: AttachMode) -> UnboundedReceiver<LinkEvent> {
        let (events, rx) = unbounded();
        self.start_attach(id, size, mode, Some(Route::new(events)));
        rx
    }

    /// 已经连着的会话重新连一次（比如收到 `Resync`、在只看状态和看屏幕之间切换），事件照旧交给
    /// 原来那一端，下一件是新的 `LinkEvent::Screen`。没连着这个会话时返回 false。
    pub fn reattach(&self, id: SessionId, size: Option<GridSize>, mode: AttachMode) -> bool {
        self.start_attach(id, size, mode, None)
    }

    /// 连上会话并等第一份屏幕，最多等 `timeout`；之后的事件交给返回的一端。
    pub fn attach_now(
        &self,
        id: SessionId,
        size: Option<GridSize>,
        mode: AttachMode,
        timeout: Duration,
    ) -> Result<(Screen, UnboundedReceiver<LinkEvent>)> {
        let (events, rx) = unbounded();
        let (first, screen) = mpsc::channel();
        let mut route = Route::new(events);
        route.first_screen = Some(first);
        if !self.start_attach(id, size, mode, Some(route)) {
            return Err(anyhow!("not connected to the host"));
        }
        match screen.recv_timeout(timeout) {
            Ok(Ok(screen)) => Ok((screen, rx)),
            Ok(Err(message)) => {
                self.forget(id);
                Err(anyhow!(message))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.detach(id);
                Err(anyhow!("the host did not send session {id} in time"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!("lost the connection to the host")),
        }
    }

    /// 登记（`route` 为 `None` 时沿用原来的登记）再发 `Attach`。没连着、或者要沿用却没有登记时
    /// 返回 false。
    fn start_attach(&self, id: SessionId, size: Option<GridSize>, mut mode: AttachMode, route: Option<Route>) -> bool {
        {
            let mut state = self.inner.state();
            let snapshots = state.snapshots;
            if !state.connected {
                if let Some(mut route) = route {
                    route.deliver(LinkEvent::Lost);
                }
                return false;
            }
            let attaching = state.sessions.get(&id).map_or(0, |old| old.attaching);
            let route = match route {
                Some(mut route) => {
                    // 原来那个 `Attach` 的 `Attached` 还会到，要算着它，不然会被当成这次的。
                    route.attaching = attaching;
                    state.sessions.insert(id, route);
                    state.sessions.get_mut(&id)
                }
                None => state.sessions.get_mut(&id),
            };
            let Some(route) = route else {
                return false;
            };
            mode = attach_mode(mode, snapshots);
            route.attaching += 1;
            route.assembling = None;
            if let Some(channel) = route.channel.take() {
                state.channels.remove(&channel);
            }
        }
        if let Err(err) = self.inner.control(&ClientMsg::Attach { id, size, mode }) {
            tracing::debug!("failed to attach session {id}: {err}");
        }
        true
    }

    /// 不再看这个会话，会话照旧跑着。
    pub fn detach(&self, id: SessionId) {
        self.send(ClientMsg::Detach { id });
    }

    /// 结束会话。
    pub fn kill(&self, id: SessionId) {
        self.send(ClientMsg::Kill { id });
    }

    /// 忘掉这个会话的登记，之后它的帧丢掉。
    fn forget(&self, id: SessionId) {
        let mut state = self.inner.state();
        if let Some(route) = state.sessions.remove(&id)
            && let Some(channel) = route.channel
        {
            state.channels.remove(&channel);
        }
    }

    /// 宿主里所有的会话，最多等 `timeout`。
    pub fn list_sessions(&self, timeout: Duration) -> Result<Vec<SessionInfo>> {
        let (tx, rx) = mpsc::channel();
        {
            let mut state = self.inner.state();
            if !state.connected {
                return Err(anyhow!("not connected to the host"));
            }
            state.lists.push_back(tx);
        }
        self.inner.control(&ClientMsg::ListSessions).map_err(|err| anyhow!("failed to ask the host: {err}"))?;
        rx.recv_timeout(timeout).map_err(|err| match err {
            mpsc::RecvTimeoutError::Timeout => anyhow!("the host did not list its sessions in time"),
            mpsc::RecvTimeoutError::Disconnected => anyhow!("lost the connection to the host"),
        })
    }

    /// 等宿主读完之前发的所有消息，最多等 `timeout`：发一个 `ListSessions` 等它回话，宿主按先后
    /// 处理同一条连接上的消息。宿主读完后断开（比如收到 `Shutdown` 后发了 `Goodbye`）也算。
    /// 超时返回 false。
    pub fn flush(&self, timeout: Duration) -> bool {
        match self.list_sessions(timeout) {
            Ok(_) => true,
            Err(_) => !self.connected(),
        }
    }

    /// 回宿主转来的 `HostMsg::UiRequest`，`ui` 是那条请求的编号。
    pub fn ui_reply(&self, ui: u64, reply: HostMsg) {
        self.send(ClientMsg::UiReply { ui, reply: Box::new(reply) });
    }

    /// 宿主转给界面去办的请求（`Open`、`Reveal`、`Layout`），带着回话要用的编号。只能取一次，
    /// 第二次返回 `None`；取走之前到的请求攒着。
    pub fn ui_requests(&self) -> Option<UnboundedReceiver<(u64, ClientMsg)>> {
        self.inner.ui_requests.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    fn expect_reply(&self, req: u32) -> Result<mpsc::Receiver<HostMsg>> {
        let (tx, rx) = mpsc::channel();
        let mut state = self.inner.state();
        if !state.connected {
            return Err(anyhow!("not connected to the host"));
        }
        state.replies.insert(req, tx);
        Ok(rx)
    }
}

/// `HostMsg::Welcome` 里这边要的。
struct Welcome {
    host_pid: u32,
    snapshot_format: u16,
    standalone: bool,
}

/// 以桌面界面的身份握手，返回宿主的 `Welcome`。
fn handshake(stream: &UnixStream, build: &BuildId) -> Result<Welcome, ConnectError> {
    let hello = ClientMsg::Hello {
        protocol: PROTOCOL_VERSION,
        build: build.clone(),
        client: ClientKind::Desktop,
        caps: Caps { snapshot: true, vt_replay: true },
        session: None,
    };
    let frame = Frame::control(&hello).map_err(|err| ConnectError::Io(io::Error::other(err)))?;
    let mut writer = stream;
    write_frame(&mut writer, frame.kind, 0, &frame.payload).map_err(frame_error)?;
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).map_err(ConnectError::Io)?;
    // 不经缓冲读：缓冲会把握手之后的帧也读走。
    let mut reader = stream;
    let answer = loop {
        match read_frame(&mut reader).map_err(frame_error)? {
            None => return Err(ConnectError::Closed),
            Some(frame) if frame.kind == FrameKind::Control => match frame.message::<HostMsg>() {
                Ok(HostMsg::Welcome { host_pid, snapshot_format, standalone, .. }) => {
                    break Ok(Welcome { host_pid, snapshot_format, standalone });
                }
                Ok(HostMsg::Incompatible { reason, .. }) => break Err(ConnectError::Incompatible(reason)),
                Ok(HostMsg::Goodbye { .. }) => break Err(ConnectError::Closed),
                Ok(other) => tracing::debug!("unexpected message before welcome: {other:?}"),
                Err(err) => tracing::debug!("unreadable message before welcome: {err}"),
            },
            Some(_) => {}
        }
    };
    stream.set_read_timeout(None).map_err(ConnectError::Io)?;
    answer
}

/// 这个构建的界面 VT 解得了的快照格式，算一次；算不出来时为 `None`。
fn local_snapshot_format() -> Option<u16> {
    static FORMAT: OnceLock<Option<u16>> = OnceLock::new();
    *FORMAT.get_or_init(|| {
        runode_terminal::host_session::snapshot_format()
            .inspect_err(|err| tracing::warn!("cannot tell which snapshot format this build reads: {err}"))
            .ok()
    })
}

/// 宿主编的快照（格式 `host`）这边（格式 `ours`）解不解得了：格式一样才行，这边说不出自己的格式
/// 时按解不了。
fn snapshots_usable(host: u16, ours: Option<u16>) -> bool {
    ours == Some(host)
}

/// 实际向宿主要的屏幕：解不了宿主的快照（`snapshots` 为假）时把 `Snapshot` 换成 `VtReplay`，免得
/// 拿到一份解不了的快照、建不出视图；别的照旧。
fn attach_mode(requested: AttachMode, snapshots: bool) -> AttachMode {
    match requested {
        AttachMode::Snapshot if !snapshots => AttachMode::VtReplay,
        mode => mode,
    }
}

fn frame_error(err: FrameError) -> ConnectError {
    match err {
        FrameError::Io(err) => ConnectError::Io(err),
        FrameError::Truncated => ConnectError::Closed,
        err => ConnectError::Io(io::Error::other(err.to_string())),
    }
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

/// 读线程：读到连接断开（或者宿主说 `Goodbye`）为止。
fn read_loop(inner: &Inner, generation: u64, stream: UnixStream) {
    let mut reader = BufReader::with_capacity(READ_BUFFER, stream);
    loop {
        let frame = match read_frame(&mut reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => return,
            Err(err) => {
                tracing::debug!("the host connection broke: {err}");
                return;
            }
        };
        match frame.kind {
            FrameKind::Output => output(inner, frame.channel, frame.payload),
            FrameKind::Snapshot => snapshot(inner, frame.channel, &frame.payload),
            FrameKind::Control => match frame.message::<HostMsg>() {
                Ok(HostMsg::Goodbye { reason }) => {
                    tracing::info!("the host said goodbye: {reason:?}");
                    if reason == GoodbyeReason::Shutdown {
                        inner.ended_all(generation);
                    }
                    return;
                }
                Ok(message) => dispatch(inner, message),
                Err(err) => tracing::debug!("unreadable message from the host: {err}"),
            },
            FrameKind::Input => tracing::debug!("the host sent an input frame"),
        }
    }
}

/// 一块输出：交给这个通道的会话；通道不认识（旧的订阅、已经不看了）时丢掉。
fn output(inner: &Inner, channel: u32, data: Vec<u8>) {
    let mut state = inner.state();
    let Some(&id) = state.channels.get(&channel) else { return };
    let alive = match state.sessions.get_mut(&id) {
        Some(route) if route.attaching == 0 && route.assembling.is_none() => route.deliver(LinkEvent::Output(data)),
        _ => true,
    };
    if !alive {
        drop_route(&mut state, id);
    }
}

fn snapshot(inner: &Inner, channel: u32, data: &[u8]) {
    let mut state = inner.state();
    let Some(&id) = state.channels.get(&channel) else { return };
    if let Some(route) = state.sessions.get_mut(&id)
        && let Some((_, assembled)) = &mut route.assembling
    {
        assembled.extend_from_slice(data);
    }
}

/// 收的一方不要了：忘掉这个会话。视图丢掉时自己发 `Detach` 或 `Kill`，这里不发。
fn drop_route(state: &mut State, id: SessionId) {
    if let Some(route) = state.sessions.remove(&id)
        && let Some(channel) = route.channel
    {
        state.channels.remove(&channel);
    }
}

/// 一条控制消息：回话交给等着的调用方，会话的消息交给那个会话。
fn dispatch(inner: &Inner, message: HostMsg) {
    let mut state = inner.state();
    match message {
        HostMsg::Spawned { req, id } => match state.replies.remove(&req) {
            Some(reply) => {
                let _ = reply.send(HostMsg::Spawned { req, id });
            }
            None => {
                // 等的一方已经超时走了，没人要这个会话。
                drop(state);
                tracing::debug!("ending session {id}, spawned after its caller gave up");
                let _ = inner.control(&ClientMsg::Kill { id });
            }
        },
        HostMsg::SessionList { sessions } => {
            // 等的一方超时走了，它的位置还排在队里：跳过这些，交给下一个还在等的。回话按先后
            // 到，交出去的可能是前一个请求的，那也只早一点。
            let mut sessions = sessions;
            while let Some(reply) = state.lists.pop_front() {
                match reply.send(sessions) {
                    Ok(()) => break,
                    Err(mpsc::SendError(back)) => sessions = back,
                }
            }
        }
        HostMsg::UiRequest { ui, request } => {
            drop(state);
            if inner.ui.unbounded_send((ui, *request)).is_err() {
                let reply = HostMsg::Error { req: None, id: None, message: "the runode app is quitting".into() };
                let _ = inner.control(&ClientMsg::UiReply { ui, reply: Box::new(reply) });
            }
        }
        HostMsg::Attached { id, channel, size, mode, meta, settings } => {
            let Some(route) = state.sessions.get_mut(&id) else {
                // 等着的时候不看了（`Detach`、`Kill` 已经跟在 `Attach` 后面发出去了）。
                return;
            };
            route.attaching = route.attaching.saturating_sub(1);
            if route.attaching > 0 {
                // 之前那个 `Attach` 的，新的还在后面。
                return;
            }
            route.channel = Some(channel);
            let queued = std::mem::take(&mut route.queued);
            let attached = Attached { id, channel, size, mode, meta, settings };
            let alive = if mode == AttachMode::MetaOnly {
                route.deliver(LinkEvent::Screen(Screen { attached, data: Vec::new() })) && route.settle()
            } else {
                route.assembling = Some((attached, Vec::new()));
                true
            };
            state.channels.insert(channel, id);
            if !alive {
                drop_route(&mut state, id);
                return;
            }
            // 拿着 `state` 写：别的线程要等这里写完才看得到通道，之后的输入排在攒着的后面。
            for data in queued {
                if inner.write(FrameKind::Input, channel, &data).is_err() {
                    break;
                }
            }
        }
        HostMsg::SnapshotEnd { id } => {
            let Some(route) = state.sessions.get_mut(&id) else { return };
            if let Some((attached, data)) = route.assembling.take()
                && !(route.deliver(LinkEvent::Screen(Screen { attached, data })) && route.settle())
            {
                drop_route(&mut state, id);
            }
        }
        HostMsg::Error { req: Some(req), id, message } if state.replies.contains_key(&req) => {
            if let Some(reply) = state.replies.remove(&req) {
                let _ = reply.send(HostMsg::Error { req: Some(req), id, message });
            }
        }
        HostMsg::Error { req, id: Some(id), message } => {
            let Some(route) = state.sessions.get_mut(&id) else {
                tracing::debug!("error from the host about session {id}: {message}");
                return;
            };
            // 正连着时的错误当成这次没连成（多半是会话已经没了）。
            if route.attaching > 0 {
                route.attaching -= 1;
                if let Some(first) = route.first_screen.take() {
                    let _ = first.send(Err(message));
                    return;
                }
            }
            if !(route.deliver(LinkEvent::Msg(HostMsg::Error { req, id: Some(id), message })) && route.settle()) {
                drop_route(&mut state, id);
            }
        }
        HostMsg::Error { message, .. } => tracing::warn!("the host reported an error: {message}"),
        message => {
            let Some(id) = session_of(&message) else {
                tracing::debug!("ignored a message from the host: {message:?}");
                return;
            };
            let alive = match state.sessions.get_mut(&id) {
                Some(route) if route.attaching == 0 => route.deliver(LinkEvent::Msg(message)),
                // 正连着时旧订阅的消息用不上：新的屏幕和 `Attached` 带着最新的状态。只有会话结束了
                // 要留着：新的订阅可能连不上（会话已经没了），视图只能靠它知道。
                Some(route) if matches!(message, HostMsg::Exited { .. }) => {
                    route.exited = Some(message);
                    true
                }
                _ => true,
            };
            if !alive {
                drop_route(&mut state, id);
            }
        }
    }
}

/// 属于某个会话、要交给它的消息是哪个会话的。
fn session_of(message: &HostMsg) -> Option<SessionId> {
    match message {
        HostMsg::Resized { id, .. }
        | HostMsg::ThemeApplied { id, .. }
        | HostMsg::Meta { id, .. }
        | HostMsg::CommandFinished { id, .. }
        | HostMsg::Resync { id, .. }
        | HostMsg::Exited { id, .. }
        | HostMsg::Bell { id }
        | HostMsg::ScreenText { id, .. } => Some(*id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        time::{Duration, Instant},
    };

    use runode_host::Host;
    use runode_terminal::session::Session;

    use super::*;

    const WAIT: Duration = Duration::from_secs(10);
    const SIZE: GridSize = GridSize { cols: 40, rows: 6, cell_width_px: 8, cell_height_px: 16 };
    const BUILD: &str = "link-test";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rnl-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 写一个当 shell 用的脚本；登录 shell 多带的参数它不看。
    fn script(dir: &Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt as _;

        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn connected(host: &Host) -> Link {
        let link = Link::new(BuildId(BUILD.into()));
        link.connect(host.connect_pair().unwrap()).unwrap();
        link
    }

    fn spawn(link: &Link, shell: String) -> SessionId {
        link.spawn(SpawnOptions {
            size: SIZE,
            cwd: None,
            integration: IntegrationMode::Off,
            start: true,
            shell: Some(shell),
            settings: None,
            env: Vec::new(),
        })
        .unwrap()
    }

    fn next(rx: &mut UnboundedReceiver<LinkEvent>) -> LinkEvent {
        let deadline = Instant::now() + WAIT;
        loop {
            match rx.try_recv() {
                Ok(event) => return event,
                Err(futures::channel::mpsc::TryRecvError::Closed) => panic!("the link dropped the session"),
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
                Err(_) => panic!("timed out waiting for an event"),
            }
        }
    }

    /// 等到输出里出现 `needle`，跳过别的事件。
    fn wait_for_output(rx: &mut UnboundedReceiver<LinkEvent>, needle: &[u8]) {
        let mut seen = Vec::new();
        loop {
            if let LinkEvent::Output(data) = next(rx) {
                seen.extend_from_slice(&data);
                if seen.windows(needle.len()).any(|w| w == needle) {
                    return;
                }
            }
        }
    }

    #[test]
    fn attaching_assembles_a_snapshot_the_view_can_decode() {
        let dir = temp_dir("snapshot");
        let host = Host::new(BuildId(BUILD.into()));
        let link = connected(&host);
        let id = spawn(&link, script(&dir, "hello.sh", "printf 'ready\\n'\nexec /bin/cat"));
        // 先等输出出来，快照里才有它。
        let mut probe = link.attach(id, None, AttachMode::MetaOnly);
        assert!(matches!(next(&mut probe), LinkEvent::Screen(Screen { data, .. }) if data.is_empty()));
        let deadline = Instant::now() + WAIT;
        let (screen, _rx) = loop {
            let (screen, rx) = link.attach_now(id, Some(SIZE), AttachMode::Snapshot, WAIT).unwrap();
            assert_eq!(screen.attached.mode, AttachMode::Snapshot);
            assert_eq!(screen.attached.id, id);
            let session = Session::from_snapshot(&screen.data, Box::new(|_| {})).unwrap();
            if session.screen_text().unwrap_or_default().contains("ready") || Instant::now() > deadline {
                break (screen, rx);
            }
            thread::sleep(Duration::from_millis(20));
        };
        let session = Session::from_snapshot(&screen.data, Box::new(|_| {})).unwrap();
        assert!(session.screen_text().unwrap_or_default().contains("ready"), "{:?}", session.screen_text());
        link.kill(id);
    }

    #[test]
    fn input_and_output_go_through_the_session_channel() {
        let dir = temp_dir("echo");
        let host = Host::new(BuildId(BUILD.into()));
        let link = connected(&host);
        let id = spawn(&link, script(&dir, "cat.sh", "exec /bin/cat"));
        let mut rx = link.attach(id, None, AttachMode::Snapshot);
        // 还没等到 `Attached` 就打字：攒着，知道通道后写出去。
        link.input(id, b"early\r");
        let LinkEvent::Screen(screen) = next(&mut rx) else { panic!("the first event is the screen") };
        assert!(screen.attached.channel > 0);
        wait_for_output(&mut rx, b"early");
        link.input(id, b"later\r");
        wait_for_output(&mut rx, b"later");
        link.kill(id);
    }

    #[test]
    fn two_sessions_get_their_own_output() {
        let dir = temp_dir("two");
        let host = Host::new(BuildId(BUILD.into()));
        let link = connected(&host);
        let a = spawn(&link, script(&dir, "a.sh", "exec /bin/cat"));
        let b = spawn(&link, script(&dir, "b.sh", "exec /bin/cat"));
        let (_, mut rx_a) = link.attach_now(a, None, AttachMode::Snapshot, WAIT).unwrap();
        let (_, mut rx_b) = link.attach_now(b, None, AttachMode::Snapshot, WAIT).unwrap();
        link.input(a, b"apple\r");
        link.input(b, b"banana\r");
        wait_for_output(&mut rx_a, b"apple");
        wait_for_output(&mut rx_b, b"banana");
        // 各自只收到自己的。
        link.input(a, b"zzz\r");
        wait_for_output(&mut rx_a, b"zzz");
        while let Ok(event) = rx_b.try_recv() {
            if let LinkEvent::Output(data) = event {
                assert!(!data.windows(3).any(|w| w == b"zzz"));
            }
        }
        link.kill(a);
        link.kill(b);
    }

    #[test]
    fn attaching_a_missing_session_fails() {
        let host = Host::new(BuildId(BUILD.into()));
        let link = connected(&host);
        let id = SessionId(42);
        assert!(link.attach_now(id, None, AttachMode::Snapshot, WAIT).is_err());
        let mut rx = link.attach(id, None, AttachMode::Snapshot);
        assert!(matches!(next(&mut rx), LinkEvent::Msg(HostMsg::Error { id: Some(got), .. }) if got == id));
    }

    #[test]
    fn ui_requests_go_to_the_desktop_and_replies_come_back() {
        let host = Host::new(BuildId(BUILD.into()));
        let link = connected(&host);
        let mut requests = link.ui_requests().unwrap();
        assert!(link.ui_requests().is_none());
        // 命令行那一方经另一对 socket 连上来，请界面切到某个会话。
        let mut cli = host.connect_pair().unwrap();
        let send = |stream: &mut UnixStream, message: &ClientMsg| {
            let frame = Frame::control(message).unwrap();
            write_frame(stream, frame.kind, 0, &frame.payload).unwrap();
        };
        let receive = |stream: &mut UnixStream| loop {
            let frame = read_frame(stream).unwrap().unwrap();
            if frame.kind == FrameKind::Control {
                return frame.message::<HostMsg>().unwrap();
            }
        };
        let hello = ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            build: BuildId(BUILD.into()),
            client: ClientKind::Cli,
            caps: Caps::default(),
            session: None,
        };
        send(&mut cli, &hello);
        assert!(matches!(receive(&mut cli), HostMsg::Welcome { .. }));
        let id = SessionId(7);
        send(&mut cli, &ClientMsg::Reveal { req: 3, id });
        let deadline = Instant::now() + WAIT;
        let (ui, request) = loop {
            match requests.try_recv() {
                Ok(request) => break request,
                _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
                _ => panic!("no ui request"),
            }
        };
        assert_eq!(request, ClientMsg::Reveal { req: 3, id });
        link.ui_reply(ui, HostMsg::Done { req: 3 });
        assert_eq!(receive(&mut cli), HostMsg::Done { req: 3 });
    }

    #[test]
    fn frames_between_attach_and_attached_are_dropped() {
        let link = Link::new(BuildId(BUILD.into()));
        let id = SessionId(1);
        let (events, mut rx) = unbounded();
        {
            let mut state = link.inner.state();
            state.connected = true;
            let mut route = Route::new(events);
            route.channel = Some(1);
            state.sessions.insert(id, route);
            state.channels.insert(1, id);
        }
        output(&link.inner, 1, b"live".to_vec());
        assert!(matches!(rx.try_recv(), Ok(LinkEvent::Output(data)) if data == b"live"));
        // 重新连上：这之后、新的 `Attached` 之前，旧订阅的输出和标记都不要。
        assert!(link.reattach(id, None, AttachMode::Snapshot));
        output(&link.inner, 1, b"stale".to_vec());
        dispatch(&link.inner, HostMsg::Resized { id, size: SIZE });
        dispatch(&link.inner, HostMsg::Meta { id, meta: SessionMeta::default() });
        assert!(rx.try_recv().is_err(), "nothing reaches the view while attaching");
        dispatch(
            &link.inner,
            HostMsg::Attached {
                id,
                channel: 2,
                size: SIZE,
                mode: AttachMode::Snapshot,
                meta: SessionMeta::default(),
                settings: None,
            },
        );
        snapshot(&link.inner, 2, b"snap");
        snapshot(&link.inner, 2, b"shot");
        output(&link.inner, 1, b"old channel".to_vec());
        dispatch(&link.inner, HostMsg::SnapshotEnd { id });
        let LinkEvent::Screen(screen) = rx.try_recv().unwrap() else { panic!("expected the screen") };
        assert_eq!(screen.data, b"snapshot");
        assert_eq!(screen.attached.channel, 2);
        output(&link.inner, 2, b"new".to_vec());
        assert!(matches!(rx.try_recv(), Ok(LinkEvent::Output(data)) if data == b"new"));
        dispatch(&link.inner, HostMsg::Bell { id });
        assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Bell { .. }))));
    }

    #[test]
    fn two_attaches_in_a_row_wait_for_the_second_attached() {
        let link = Link::new(BuildId(BUILD.into()));
        link.inner.state().connected = true;
        let id = SessionId(1);
        let mut rx = link.attach(id, None, AttachMode::MetaOnly);
        assert!(link.reattach(id, None, AttachMode::MetaOnly));
        let attached = |channel| HostMsg::Attached {
            id,
            channel,
            size: SIZE,
            mode: AttachMode::MetaOnly,
            meta: SessionMeta::default(),
            settings: None,
        };
        dispatch(&link.inner, attached(1));
        dispatch(&link.inner, HostMsg::Meta { id, meta: SessionMeta::default() });
        assert!(rx.try_recv().is_err());
        dispatch(&link.inner, attached(2));
        let LinkEvent::Screen(screen) = rx.try_recv().unwrap() else { panic!("expected the screen") };
        assert_eq!(screen.attached.channel, 2);
    }

    /// 连着、已经连上会话 `id`（通道 1）的 `Link`，返回收这个会话事件的一端。
    fn attached_link(id: SessionId) -> (Link, UnboundedReceiver<LinkEvent>) {
        let link = Link::new(BuildId(BUILD.into()));
        let (events, rx) = unbounded();
        let mut state = link.inner.state();
        state.connected = true;
        let mut route = Route::new(events);
        route.channel = Some(1);
        state.sessions.insert(id, route);
        state.channels.insert(1, id);
        drop(state);
        (link, rx)
    }

    /// 重新连上时撞上会话被结束（比如 `runode kill`）：旧订阅的 `Exited` 先到、新的 `Attach` 因为
    /// 会话没了回 `Error`。视图先收到 `Error`，再收到留下的 `Exited`，能关掉。
    #[test]
    fn an_exit_while_reattaching_reaches_the_view() {
        let id = SessionId(1);
        let (link, mut rx) = attached_link(id);
        assert!(link.reattach(id, None, AttachMode::Snapshot));
        dispatch(&link.inner, HostMsg::Meta { id, meta: SessionMeta::default() });
        dispatch(&link.inner, HostMsg::Exited { id, status: None });
        assert!(rx.try_recv().is_err(), "nothing reaches the view while attaching");
        dispatch(&link.inner, HostMsg::Error { req: None, id: Some(id), message: format!("no session {id}") });
        assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Error { id: Some(got), .. })) if got == id));
        assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Exited { id: got, .. })) if got == id));
        assert!(rx.try_recv().is_err());
    }

    /// 正连着时留下的 `Exited` 跟在新的屏幕后面交出去，快照和只看状态都一样；连接断了时先交它再
    /// 交 `Lost`。
    #[test]
    fn an_exit_while_attaching_follows_the_new_screen() {
        let id = SessionId(1);
        let (link, mut rx) = attached_link(id);
        assert!(link.reattach(id, None, AttachMode::Snapshot));
        dispatch(&link.inner, HostMsg::Exited { id, status: None });
        let attached = |channel, mode| HostMsg::Attached {
            id,
            channel,
            size: SIZE,
            mode,
            meta: SessionMeta::default(),
            settings: None,
        };
        dispatch(&link.inner, attached(2, AttachMode::Snapshot));
        snapshot(&link.inner, 2, b"snap");
        assert!(rx.try_recv().is_err(), "the exit waits for the screen");
        dispatch(&link.inner, HostMsg::SnapshotEnd { id });
        assert!(matches!(rx.try_recv(), Ok(LinkEvent::Screen(screen)) if screen.data == b"snap"));
        assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Exited { .. }))));

        assert!(link.reattach(id, None, AttachMode::MetaOnly));
        dispatch(&link.inner, HostMsg::Exited { id, status: None });
        dispatch(&link.inner, attached(3, AttachMode::MetaOnly));
        assert!(matches!(rx.try_recv(), Ok(LinkEvent::Screen(_))));
        assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Exited { .. }))));

        assert!(link.reattach(id, None, AttachMode::MetaOnly));
        dispatch(&link.inner, HostMsg::Exited { id, status: None });
        link.close();
        assert!(matches!(rx.try_recv(), Ok(LinkEvent::Msg(HostMsg::Exited { .. }))));
        assert!(matches!(rx.try_recv(), Ok(LinkEvent::Lost)));
    }

    #[test]
    fn snapshots_need_the_same_format() {
        assert!(snapshots_usable(1, Some(1)));
        assert!(!snapshots_usable(2, Some(1)));
        assert!(!snapshots_usable(0, None));
        assert_eq!(attach_mode(AttachMode::Snapshot, true), AttachMode::Snapshot);
        assert_eq!(attach_mode(AttachMode::Snapshot, false), AttachMode::VtReplay);
        assert_eq!(attach_mode(AttachMode::VtReplay, true), AttachMode::VtReplay);
        assert_eq!(attach_mode(AttachMode::MetaOnly, false), AttachMode::MetaOnly);
    }

    /// 宿主编的快照格式和这边的对不上：要快照时改要 VT 重放，宿主给的也是重放，不至于拿到解不了的
    /// 快照。
    #[test]
    fn a_host_with_another_snapshot_format_gets_asked_for_replays() {
        let (asked, asked_rx) = mpsc::channel();
        let (ours, theirs) = UnixStream::pair().unwrap();
        thread::spawn(move || {
            let mut stream = theirs;
            let _hello = read_frame(&mut stream).unwrap().unwrap();
            let welcome = HostMsg::Welcome {
                protocol: PROTOCOL_VERSION,
                build: BuildId(BUILD.into()),
                host_pid: 1,
                snapshot_format: local_snapshot_format().unwrap() + 1,
                standalone: true,
            };
            let frame = Frame::control(&welcome).unwrap();
            write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
            loop {
                let frame = read_frame(&mut stream).unwrap().unwrap();
                if let Ok(ClientMsg::Attach { mode, .. }) = frame.message::<ClientMsg>() {
                    asked.send(mode).unwrap();
                    return;
                }
            }
        });
        let link = Link::new(BuildId(BUILD.into()));
        link.connect(ours).unwrap();
        let _rx = link.attach(SessionId(1), Some(SIZE), AttachMode::Snapshot);
        assert_eq!(asked_rx.recv_timeout(WAIT).unwrap(), AttachMode::VtReplay);
    }

    /// 一个按脚本说话的宿主：握手后由 `then` 接着处理这条连接。
    fn fake_host(then: impl FnOnce(UnixStream) + Send + 'static) -> Link {
        let (ours, theirs) = UnixStream::pair().unwrap();
        thread::spawn(move || {
            let mut stream = theirs;
            let hello = read_frame(&mut stream).unwrap().unwrap();
            assert!(matches!(
                hello.message::<ClientMsg>().unwrap(),
                ClientMsg::Hello { client: ClientKind::Desktop, .. }
            ));
            let welcome = HostMsg::Welcome {
                protocol: PROTOCOL_VERSION,
                build: BuildId(BUILD.into()),
                host_pid: 1,
                snapshot_format: 1,
                standalone: true,
            };
            let frame = Frame::control(&welcome).unwrap();
            write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
            then(stream);
        });
        let link = Link::new(BuildId(BUILD.into()));
        link.connect(ours).unwrap();
        link
    }

    /// 读到 `Attach` 为止。
    fn wait_for_attach(stream: &mut UnixStream) {
        loop {
            let frame = read_frame(stream).unwrap().unwrap();
            if matches!(frame.message::<ClientMsg>(), Ok(ClientMsg::Attach { .. })) {
                return;
            }
        }
    }

    #[test]
    fn every_session_hears_when_the_host_goes_away() {
        let (attached, attached_rx) = mpsc::channel();
        let link = fake_host(move |mut stream| {
            wait_for_attach(&mut stream);
            wait_for_attach(&mut stream);
            attached.send(()).unwrap();
            // 断开。
        });
        let mut a = link.attach(SessionId(1), None, AttachMode::Snapshot);
        let mut b = link.attach(SessionId(2), None, AttachMode::Snapshot);
        attached_rx.recv_timeout(WAIT).unwrap();
        assert!(matches!(next(&mut a), LinkEvent::Lost));
        assert!(matches!(next(&mut b), LinkEvent::Lost));
        let deadline = Instant::now() + WAIT;
        while link.connected() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(!link.connected());
        assert!(
            link.spawn(SpawnOptions {
                size: SIZE,
                cwd: None,
                integration: IntegrationMode::Off,
                start: true,
                shell: None,
                settings: None,
                env: Vec::new(),
            })
            .is_err()
        );
        assert!(matches!(next(&mut link.attach(SessionId(3), None, AttachMode::Snapshot)), LinkEvent::Lost));
    }

    #[test]
    fn a_shutdown_goodbye_ends_every_session() {
        let link = fake_host(|mut stream| {
            wait_for_attach(&mut stream);
            let goodbye = HostMsg::Goodbye { reason: runode_protocol::GoodbyeReason::Shutdown };
            let frame = Frame::control(&goodbye).unwrap();
            write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
        });
        let id = SessionId(1);
        let mut rx = link.attach(id, None, AttachMode::Snapshot);
        assert!(matches!(next(&mut rx), LinkEvent::Msg(HostMsg::Exited { id: got, .. }) if got == id));
    }

    #[test]
    fn theme_and_options_are_sent_again_after_reconnecting() {
        let host = Host::new(BuildId(BUILD.into()));
        let link = connected(&host);
        let settings = TermSettings { scrollback_limit: 3 << 20, ..TermSettings::default() };
        link.send(ClientMsg::SetTheme { settings: settings.clone() });
        link.send(ClientMsg::SetOptions { record_history: false });
        let (seen, seen_rx) = mpsc::channel();
        let (ours, theirs) = UnixStream::pair().unwrap();
        thread::spawn(move || {
            let mut stream = theirs;
            let _hello = read_frame(&mut stream).unwrap().unwrap();
            let welcome = HostMsg::Welcome {
                protocol: PROTOCOL_VERSION,
                build: BuildId(BUILD.into()),
                host_pid: 1,
                snapshot_format: 1,
                standalone: true,
            };
            let frame = Frame::control(&welcome).unwrap();
            write_frame(&mut stream, frame.kind, 0, &frame.payload).unwrap();
            for _ in 0..2 {
                let frame = read_frame(&mut stream).unwrap().unwrap();
                seen.send(frame.message::<ClientMsg>().unwrap()).unwrap();
            }
        });
        link.connect(ours).unwrap();
        assert_eq!(seen_rx.recv_timeout(WAIT).unwrap(), ClientMsg::SetTheme { settings });
        assert_eq!(seen_rx.recv_timeout(WAIT).unwrap(), ClientMsg::SetOptions { record_history: false });
    }

    /// 基准：经 `Link` 的按键到回显延迟和 `cat` 大文件的吞吐，宿主跑在 app 里（一对 socket）和单独
    /// 一个进程（`runode --host`，要先构建出 runode 本身）各一份。手动跑：
    ///
    /// ```sh
    /// cargo build --release -p runode
    /// cargo test --release -p runode bench_ -- --ignored --nocapture --test-threads=1
    /// ```
    ///
    /// `RUNODE_LATENCY_N` 改回显的次数（默认 1 万），`RUNODE_THROUGHPUT_MB` 改文件大小（默认 100）。
    mod bench {
        use std::process::{Child, Command};

        use futures::StreamExt as _;

        use super::*;

        const LATENCY_SIZE: GridSize = GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 };
        const THROUGHPUT_SIZE: GridSize = GridSize { cols: 200, rows: 50, cell_width_px: 8, cell_height_px: 16 };
        const DONE: &[u8] = b"RUNODE-DONE";

        #[allow(clippy::print_stderr)]
        fn report(line: &str) {
            eprintln!("{line}");
        }

        /// 宿主在哪里跑；单独一个进程的那个在丢掉时结束。跑在 app 里的拿着它，基准跑完前不丢掉。
        enum Where {
            InApp(#[allow(dead_code)] Host),
            Process(Child, PathBuf),
        }

        impl Drop for Where {
            fn drop(&mut self) {
                if let Self::Process(child, dir) = self {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_dir_all(dir);
                }
            }
        }

        fn in_app() -> (Where, Link) {
            let host = Host::new(BuildId(BUILD.into()));
            let link = connected(&host);
            (Where::InApp(host), link)
        }

        /// 拉起 `runode --host`，socket 放在临时的配置目录里，经 socket 连上。
        fn process(name: &str) -> (Where, Link) {
            let exe = std::env::current_exe().unwrap();
            let runode = exe.parent().unwrap().parent().unwrap().join("runode");
            assert!(runode.exists(), "build runode first: {}", runode.display());
            let config = PathBuf::from(format!("/tmp/rnb-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&config);
            std::fs::create_dir_all(&config).unwrap();
            let child = Command::new(&runode).arg("--host").env("XDG_CONFIG_HOME", &config).spawn().unwrap();
            let dirs = runode_paths::Dirs {
                config: Some(config.clone()),
                data: Some(config.join("runode")),
                ..Default::default()
            };
            let socket = dirs.host_socket_file().unwrap();
            let deadline = Instant::now() + WAIT;
            // 单独一个进程的宿主和 app 的构建一样，不然给不了快照。
            let link = Link::new(BuildId(env!("RUNODE_BUILD").into()));
            loop {
                if let Ok(stream) = UnixStream::connect(&socket)
                    && link.connect(stream).is_ok()
                {
                    break;
                }
                assert!(Instant::now() < deadline, "the host process did not come up");
                thread::sleep(Duration::from_millis(5));
            }
            (Where::Process(child, config), link)
        }

        fn iterations() -> usize {
            std::env::var("RUNODE_LATENCY_N").ok().and_then(|n| n.parse().ok()).unwrap_or(10_000)
        }

        fn spawn_at(link: &Link, shell: String, size: GridSize) -> SessionId {
            link.spawn(SpawnOptions {
                size,
                cwd: None,
                integration: IntegrationMode::Off,
                start: true,
                shell: Some(shell),
                settings: None,
                env: Vec::new(),
            })
            .unwrap()
        }

        fn output(rx: &mut UnboundedReceiver<LinkEvent>) -> Vec<u8> {
            loop {
                match futures::executor::block_on(rx.next()).expect("the link dropped the session") {
                    LinkEvent::Output(data) => return data,
                    LinkEvent::Lost => panic!("lost the host"),
                    _ => {}
                }
            }
        }

        fn latency(name: &str, (_host, link): (Where, Link)) {
            let dir = temp_dir(&format!("bench-echo-{name}"));
            let id = spawn_at(&link, script(&dir, "echo.sh", "stty raw -echo\nexec /bin/cat -u"), LATENCY_SIZE);
            let (_, mut rx) = link.attach_now(id, None, AttachMode::Snapshot, WAIT).unwrap();
            thread::sleep(Duration::from_millis(500));
            link.input(id, b"!");
            while !output(&mut rx).contains(&b'!') {}
            let mut samples = Vec::with_capacity(iterations());
            for i in 0..iterations() {
                let byte = b'a' + (i % 26) as u8;
                let start = Instant::now();
                link.input(id, &[byte]);
                while !output(&mut rx).contains(&byte) {}
                samples.push(start.elapsed());
            }
            link.kill(id);
            samples.sort();
            let pct = |p: f64| samples[((samples.len() as f64 * p) as usize).min(samples.len() - 1)];
            report(&format!(
                "link {name} echo n={} p50={:?} p90={:?} p99={:?} max={:?}",
                samples.len(),
                pct(0.5),
                pct(0.9),
                pct(0.99),
                samples[samples.len() - 1]
            ));
        }

        fn throughput(name: &str, (_host, link): (Where, Link)) {
            let dir = temp_dir(&format!("bench-cat-{name}"));
            let mb: usize = std::env::var("RUNODE_THROUGHPUT_MB").ok().and_then(|n| n.parse().ok()).unwrap_or(100);
            let data = dir.join("data.txt");
            let line: String = (0..99).map(|i| char::from(b'!' + (i % 90) as u8)).collect::<String>() + "\n";
            std::fs::write(&data, line.repeat(mb * 1024 * 1024 / line.len())).unwrap();
            let body =
                format!("stty -echo\nread go\ncat {}\nprintf 'RUNODE-%s\\n' DONE\nexec /bin/cat", data.display());
            let id = spawn_at(&link, script(&dir, "cat.sh", &body), THROUGHPUT_SIZE);
            let (screen, mut rx) = link.attach_now(id, None, AttachMode::Snapshot, WAIT).unwrap();
            let mut session = Session::from_snapshot(&screen.data, Box::new(|_| {})).unwrap();
            thread::sleep(Duration::from_millis(500));
            let start = Instant::now();
            link.input(id, b"go\r");
            let (mut tail, mut total) = (Vec::new(), 0);
            loop {
                let chunk = output(&mut rx);
                session.feed(&chunk);
                total += chunk.len();
                tail.extend_from_slice(&chunk);
                if tail.windows(DONE.len()).any(|w| w == DONE) {
                    break;
                }
                let keep = tail.len().saturating_sub(DONE.len());
                tail.drain(..keep);
            }
            let elapsed = start.elapsed();
            link.kill(id);
            let _ = std::fs::remove_dir_all(dir);
            report(&format!(
                "link {name} cat {mb} MiB: {total} bytes in {elapsed:?} = {:.1} MiB/s",
                total as f64 / 1048576.0 / elapsed.as_secs_f64()
            ));
        }

        #[test]
        #[ignore = "测量延迟，手动跑"]
        fn bench_echo_latency_in_app() {
            latency("in-app", in_app());
        }

        #[test]
        #[ignore = "测量延迟，手动跑；要先构建 runode"]
        fn bench_echo_latency_host_process() {
            latency("host-process", process("echo"));
        }

        #[test]
        #[ignore = "测量吞吐，手动跑"]
        fn bench_cat_throughput_in_app() {
            throughput("in-app", in_app());
        }

        #[test]
        #[ignore = "测量吞吐，手动跑；要先构建 runode"]
        fn bench_cat_throughput_host_process() {
            throughput("host-process", process("cat"));
        }
    }
}
