//! 管终端会话的宿主：每个会话的 PTY 和权威的那份 VT（`HostSession`）都在这里，一个会话一个
//! 线程。前端（桌面的界面、以后的命令行和 TUI）连上来，收 PTY 输出和状态，发输入和请求。
//!
//! 现在宿主跑在 app 进程里，桌面的界面用 `Host::connect_in_process` 拿到的 `Client` 经 channel
//! 和它说话，不经 socket，也不序列化；消息尽量直接用 `runode_protocol` 的类型（`ClientMsg`、
//! `HostMsg`），以后宿主搬到单独的进程时只换传输层。新开、连上、启动会话这几样要等回复的
//! 请求，进程内直接是 `Client` 的方法。
//!
//! 别的进程（以后的命令行、TUI）经 `Host::listen` 开的 Unix socket 连上来，按 `runode_protocol`
//! 的帧和消息说话，见 `server`。
//!
//! 每个会话的线程按到达的先后处理 PTY 输出和前端的请求：输出先原样转给连着的前端，再喂宿主
//! 的 VT；改 VT 状态的请求（改尺寸、换主题、清屏）在输出流里插一条标记（`HostMsg::Resized`、
//! `HostMsg::ThemeApplied`）或者一段输出，前端在同一个位置做同样的事，两份 VT 才不会分叉。

mod server;
mod session;

use std::{
    collections::HashMap,
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use anyhow::{Result, anyhow};
pub use runode_protocol::{BuildId, ClientMsg, HostMsg, Placement, SessionId};
use runode_shared_types::{grid::GridSize, session::SessionMeta, settings::TermSettings, shell::IntegrationMode};

/// 宿主发给前端的一件事，同一个会话的按发生的先后到达。
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    /// PTY 的输出，原样的字节；也可能是宿主为清屏插进输出流的字节，见 `ClientMsg::ClearScreen`。
    Output(Arc<[u8]>),
    /// 控制消息。装在盒子里：输出最常见，每件事挪动时不必带着最大那种消息的大小。
    Msg(Box<HostMsg>),
}

impl HostEvent {
    pub fn msg(message: HostMsg) -> Self {
        Self::Msg(Box::new(message))
    }
}

/// 收一个会话的 `HostEvent` 的一方，在会话的线程里调用，不能阻塞。返回 false 表示不再要了，
/// 之后不再调用。
pub type Sink = Box<dyn FnMut(HostEvent) -> bool + Send>;

/// 要 app 的界面去办的请求：`ClientMsg::Open`、`ClientMsg::Reveal`。宿主不管窗口，收到这样的
/// 请求就交给 `Host::set_ui` 登记的界面。界面办完了用 `reply` 回话；没回就丢掉时替它回一句
/// `HostMsg::Error`，发请求的一方不会白等。
pub struct UiRequest {
    pub message: ClientMsg,
    reply: Option<Box<dyn FnOnce(HostMsg) + Send>>,
}

impl UiRequest {
    pub(crate) fn new(message: ClientMsg, reply: Box<dyn FnOnce(HostMsg) + Send>) -> Self {
        Self { message, reply: Some(reply) }
    }

    /// 请求的编号，回话时带上。
    pub fn req(&self) -> Option<u32> {
        match self.message {
            ClientMsg::Open { req, .. } | ClientMsg::Reveal { req, .. } => Some(req),
            _ => None,
        }
    }

    pub fn reply(mut self, message: HostMsg) {
        if let Some(reply) = self.reply.take() {
            reply(message);
        }
    }

    /// 回一句没办成。
    pub fn fail(self, message: impl Into<String>) {
        let req = self.req();
        self.reply(HostMsg::Error { req, id: None, message: message.into() });
    }
}

impl Drop for UiRequest {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            reply(HostMsg::Error { req: self.req(), id: None, message: "the runode app dropped the request".into() });
        }
    }
}

/// 收 `UiRequest` 的界面，在发请求的连接的线程里调用，不能阻塞，自己转到界面的线程去办。
pub type UiHandler = Box<dyn Fn(UiRequest) + Send + Sync>;

/// 新开一个会话。
#[derive(Clone, Debug)]
pub struct SpawnOptions {
    pub size: GridSize,
    /// shell 从哪个目录开始，`None` 时从家目录。
    pub cwd: Option<PathBuf>,
    pub integration: IntegrationMode,
    /// 现在就启动 shell；为 false 时等 `Client::start`。
    pub start: bool,
    /// 要启动的程序，`None` 时用用户的 `$SHELL`。
    pub shell: Option<String>,
    /// 宿主还没收到过 `ClientMsg::SetTheme` 时这个会话的 VT 一开始套的主题，比如配置还没
    /// 加载完就提前拉起的 shell 用自己读到的配置。收到过时一律用宿主当前的主题。之后 `SetTheme`
    /// 到了，和这个不一样时照常换。
    pub settings: Option<TermSettings>,
}

/// 连上一个会话时宿主给的东西。前端按 `size` 和 `settings` 新建自己的 VT，接着按先后处理
/// `Sink` 收到的事件：会话开出来以后、连上之前的输出和标记会先补发，所以两份 VT 从同一个起点
/// 喂同样的字节。
#[derive(Clone, Debug, PartialEq)]
pub struct Attached {
    pub size: GridSize,
    pub settings: TermSettings,
    pub meta: SessionMeta,
    /// shell 已经启动了。
    pub started: bool,
}

/// 宿主本身。可以随意克隆，各份是同一个宿主。
#[derive(Clone, Default)]
pub struct Host {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    registry: Mutex<Registry>,
    /// 要不要把 shell 集成报告的命令记进历史文件，见 `ClientMsg::SetOptions`。
    record_history: Arc<AtomicBool>,
    /// 下一个进程内连接的编号。
    next_connection: AtomicU64,
    /// 之后启动的 shell 另外设的环境变量，见 `Host::set_env`。
    env: Mutex<Vec<(String, OsString)>>,
    /// 办 `UiRequest` 的界面，见 `Host::set_ui`。
    ui: Mutex<Option<Arc<UiHandler>>>,
}

/// 会话和主题放在同一把锁下：新会话加进来和换主题不会互相错过，见 `Client::spawn`。
#[derive(Default)]
struct Registry {
    sessions: HashMap<SessionId, session::Handle>,
    /// 新会话套的主题，见 `ClientMsg::SetTheme`。
    settings: Arc<TermSettings>,
    /// 收到过几次 `SetTheme`。
    theme_generation: u64,
}

impl Shared {
    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn send(&self, id: SessionId, message: session::Inbox) {
        self.deliver(id, message);
    }

    /// 往会话线程发消息；没有这个会话、或者它的线程已经结束时返回 false。
    fn deliver(&self, id: SessionId, message: session::Inbox) -> bool {
        self.registry().sessions.get(&id).is_some_and(|handle| handle.send(message))
    }

    /// 所有会话，按标识排好。
    fn handles(&self) -> Vec<(SessionId, session::Handle)> {
        let mut handles: Vec<_> = self.registry().sessions.iter().map(|(id, handle)| (*id, handle.clone())).collect();
        handles.sort_by_key(|(id, _)| *id);
        handles
    }
}

impl Host {
    /// 一个还没有会话的宿主：主题是默认的，记命令历史。
    pub fn new() -> Self {
        let host = Self::default();
        host.shared.record_history.store(true, Ordering::Relaxed);
        host
    }

    /// 在进程内连上宿主。
    pub fn connect_in_process(&self) -> Client {
        Client { shared: self.shared.clone(), connection: self.shared.next_connection.fetch_add(1, Ordering::Relaxed) }
    }

    /// 登记办 `UiRequest` 的界面，换掉之前登记的。
    pub fn set_ui(&self, handler: UiHandler) {
        *self.shared.ui.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(handler));
    }

    /// 把请求交给界面；没有登记界面时回一句没办成。
    fn to_ui(&self, request: UiRequest) {
        let handler = self.shared.ui.lock().unwrap_or_else(PoisonError::into_inner).clone();
        match handler {
            Some(handler) => handler(request),
            None => request.fail("there is no runode window to do this in"),
        }
    }

    /// 之后启动的每个 shell 都设上这个环境变量，同名的换掉；已经启动的不受影响。每个 shell
    /// 另外还有自己的 `runode_protocol::ENV_SESSION`。
    pub fn set_env(&self, key: &str, value: impl Into<OsString>) {
        let mut env = self.shared.env.lock().unwrap_or_else(PoisonError::into_inner);
        env.retain(|(k, _)| k != key);
        env.push((key.into(), value.into()));
    }
}

/// 进程内连到宿主的一个前端。克隆出来的是同一个连接。
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
    connection: u64,
}

impl Client {
    /// 新开一个会话，返回它的标识。伪终端开不了、`start` 时 shell 启动不了时返回错误。开好的
    /// 会话不会自动连上，接着 `attach`。
    pub fn spawn(&self, options: SpawnOptions) -> Result<SessionId> {
        self.spawn_with(options, true)
    }

    /// `keep_backlog` 为 false 时不攒连上之前的事件：socket 上开的会话连上时当场给屏幕，用不着。
    fn spawn_with(&self, options: SpawnOptions, keep_backlog: bool) -> Result<SessionId> {
        let id = SessionId::random()?;
        let (settings, generation) = {
            let registry = self.shared.registry();
            let settings = match &options.settings {
                Some(settings) if registry.theme_generation == 0 => settings.clone(),
                _ => (*registry.settings).clone(),
            };
            (settings, registry.theme_generation)
        };
        // 开伪终端、启动 shell 要几毫秒，不占着锁。
        let mut env = self.shared.env.lock().unwrap_or_else(PoisonError::into_inner).clone();
        env.push((runode_protocol::ENV_SESSION.into(), id.to_string().into()));
        let handle = session::spawn(id, options, settings, env, self.shared.record_history.clone(), keep_backlog)?;
        let mut registry = self.shared.registry();
        // 这期间换过主题的话，那次换主题没赶上这个会话，补上。
        if registry.theme_generation != generation {
            handle.send(session::Inbox::Theme(registry.settings.clone()));
        }
        registry.sessions.insert(id, handle);
        Ok(id)
    }

    /// 连上会话，之后它的事件交给 `sink`。现在一个会话只能连一次，第二次返回错误。
    pub fn attach(&self, id: SessionId, sink: Sink) -> Result<Attached> {
        let (reply, attached) = std::sync::mpsc::channel();
        let sent = self
            .shared
            .registry()
            .sessions
            .get(&id)
            .is_some_and(|handle| handle.send(session::Inbox::Attach { connection: self.connection, sink, reply }));
        if !sent {
            return Err(anyhow!("no session {id}"));
        }
        attached.recv().map_err(|_| anyhow!("session {id} ended before it was attached"))?
    }

    /// 启动 `SpawnOptions::start` 为 false 时开的会话的 shell。启动不了时宿主发 `HostMsg::Exited`。
    pub fn start(&self, id: SessionId, integration: IntegrationMode) {
        self.shared.send(id, session::Inbox::Start { integration });
    }

    /// 把输入写给会话里的程序，不等它写出去。
    pub fn input(&self, id: SessionId, data: Vec<u8>) {
        self.shared.send(id, session::Inbox::Input(data));
    }

    /// 发一条控制消息。进程内用不着的（`Hello`、`Spawn`、`Attach` 等，换成了上面的方法）记一笔
    /// 日志后忽略。
    pub fn send(&self, message: ClientMsg) {
        match message {
            ClientMsg::Resize { id, size } => self.shared.send(id, session::Inbox::Resize(size)),
            ClientMsg::ClearScreen { id } => self.shared.send(id, session::Inbox::ClearScreen),
            ClientMsg::Kill { id } => {
                if let Some(handle) = self.shared.registry().sessions.remove(&id) {
                    handle.send(session::Inbox::Kill);
                }
            }
            ClientMsg::Detach { id } => self.shared.send(id, session::Inbox::Detach { connection: self.connection }),
            ClientMsg::SetTheme { settings } => {
                let settings = Arc::new(settings);
                let mut registry = self.shared.registry();
                registry.settings = settings.clone();
                registry.theme_generation += 1;
                for handle in registry.sessions.values() {
                    handle.send(session::Inbox::Theme(settings.clone()));
                }
            }
            ClientMsg::SetOptions { record_history } => {
                self.shared.record_history.store(record_history, Ordering::Relaxed);
            }
            // 通知还在界面那边发，宿主不用知道哪个会话被看着。
            ClientMsg::Focus { .. } => {}
            other => tracing::debug!(?other, "message not used in process"),
        }
    }
}
