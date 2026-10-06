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
//! `Attached` 前面，新订阅从一份新的屏幕开始，旧的帧用不上。
//!
//! 换主题和改选项（`SetTheme`、`SetOptions`）记着最近一次的，重连后补发。

use std::{
    collections::{HashMap, VecDeque},
    io::{self, BufReader, BufWriter, Write as _},
    net::Shutdown,
    os::unix::{io::AsRawFd as _, net::UnixStream},
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
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
    /// 还没回 `Welcome` 就断开了，多半撞上它正因空闲退出，过一会儿再试。
    Closed,
    Io(io::Error),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Incompatible(reason) => write!(f, "the host speaks another protocol: {reason}"),
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
    host_pid: Option<u32>,
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
}

impl Route {
    fn new(events: UnboundedSender<LinkEvent>) -> Self {
        Self { events, attaching: 0, channel: None, assembling: None, queued: Vec::new(), first_screen: None }
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
        state.host_pid = None;
        for (_, mut route) in state.sessions.drain() {
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
        set_buffers(&stream);
        stream.set_write_timeout(Some(WRITE_TIMEOUT)).map_err(ConnectError::Io)?;
        let host_pid = handshake(&stream, &self.inner.build)?;
        let reader = stream.try_clone().map_err(ConnectError::Io)?;
        let generation = {
            let mut state = self.inner.state();
            let old = state.generation;
            drop(state);
            self.inner.lost(old);
            state = self.inner.state();
            state.generation += 1;
            state.connected = true;
            state.host_pid = Some(host_pid);
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
    pub fn close(&self) {
        let generation = self.inner.state().generation;
        self.inner.lost(generation);
    }

    /// 现在连着宿主。
    pub fn connected(&self) -> bool {
        self.inner.state().connected
    }

    /// 连着的宿主的进程号。
    pub fn host_pid(&self) -> Option<u32> {
        self.inner.state().host_pid
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
    fn start_attach(&self, id: SessionId, size: Option<GridSize>, mode: AttachMode, route: Option<Route>) -> bool {
        {
            let mut state = self.inner.state();
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

/// 以桌面界面的身份握手，返回宿主的进程号。
fn handshake(stream: &UnixStream, build: &BuildId) -> Result<u32, ConnectError> {
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
                Ok(HostMsg::Welcome { host_pid, .. }) => break Ok(host_pid),
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
            if let Some(reply) = state.lists.pop_front() {
                let _ = reply.send(sessions);
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
                route.deliver(LinkEvent::Screen(Screen { attached, data: Vec::new() }))
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
                && !route.deliver(LinkEvent::Screen(Screen { attached, data }))
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
            if !route.deliver(LinkEvent::Msg(HostMsg::Error { req, id: Some(id), message })) {
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
                // 正连着时旧订阅的消息用不上：新的屏幕和 `Attached` 带着最新的状态。
                Some(route) if route.attaching == 0 => route.deliver(LinkEvent::Msg(message)),
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
