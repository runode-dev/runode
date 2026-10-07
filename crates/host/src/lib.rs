//! 管终端会话的宿主：每个会话的 PTY 和权威的那份 VT（`HostSession`）都在这里，一个会话一个
//! 线程。前端（桌面的界面、命令行，以后的 TUI）连上来，收 PTY 输出和状态，发输入和请求。
//!
//! 前端一律经一条连接按 `runode_protocol` 的帧和消息说话，见 `server`：别的进程连 `Host::listen`
//! 开的 Unix socket，同一个进程里的桌面用 `Host::connect_pair` 拿到的一对 socket 的一端，两条路
//! 走的是同一套代码。宿主可以跑在 app 进程里，也可以单独一个进程（`runode --host`，见
//! `Host::run_until_idle` 和 `launch`）。单独跑的宿主升级时，新版本的宿主经同一个 socket 接过
//! 所有会话和 socket 本身，shell 不中断（`Host::take_over`，见 `handoff`）。
//!
//! 每个会话的线程按到达的先后处理 PTY 输出和前端的请求：输出先转给连着的前端，再喂宿主的
//! VT；改 VT 状态的请求（改尺寸、换主题、清屏）在输出流里插一条标记（`HostMsg::Resized`、
//! `HostMsg::ThemeApplied`）或者一段输出，前端在同一个位置做同样的事，两份 VT 才不会分叉。
//!
//! 宿主不管窗口：要界面办的请求（`Open`、`OpenWorkspace`、`Reveal`、`Layout`）包成 `HostMsg::UiRequest` 转给
//! 登记为界面的那条连接（`Hello` 里说自己是 `ClientKind::Desktop` 的），界面用 `ClientMsg::UiReply`
//! 回话，宿主再原样转回发请求的一方。会话里的程序读写剪贴板（OSC 52）也这样请界面办，回话交回
//! 会话线程，见 `session` 的 `clipboard`。
//!
//! 前端也能请宿主在某个会话所在的仓库里读写 git（`ClientMsg::Git`），手机靠它管电脑上的仓库，
//! 见 `git`。手机新建工作区时一级级浏览电脑上的目录，也由宿主列出来（`ClientMsg::ListDirs`），
//! 见 `browse`；会话卡片上列的 Makefile 目标和 package.json 的 scripts 也是（`ClientMsg::ListProjectTasks`），
//! 见 `project_tasks`。

mod browse;
mod git;
mod handoff;
mod idle;
mod launch;
mod project_tasks;
mod server;
mod session;

use std::{
    collections::HashMap,
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
pub use handoff::{GIVE_READY_WINDOW, TAKE_OVER_AFTER_READY, TakeOverError, TakeOverOptions, TakeOverReport};
pub use idle::Stopped;
pub use launch::{STATUS_FD, Successor, launch, launch_successor};
pub use runode_protocol::{BuildId, ClientMsg, HandoffRefusal, HostMsg, Placement, SessionId};
use runode_shared_types::{clipboard::ClipboardAccess, grid::GridSize, settings::TermSettings, shell::IntegrationMode};

/// 新开一个会话。
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
    /// 宿主还没收到过 `ClientMsg::SetTheme` 时这个会话的 VT 一开始套的主题，比如配置还没
    /// 加载完就提前拉起的 shell 用自己读到的配置。收到过时一律用宿主当前的主题。之后 `SetTheme`
    /// 到了，和这个不一样时照常换。
    pub settings: Option<TermSettings>,
}

/// 宿主本身。可以随意克隆，各份是同一个宿主。
#[derive(Clone)]
pub struct Host {
    shared: Arc<Shared>,
}

struct Shared {
    /// 自己的弱引用，交给会话线程请界面办事用，见 `server::UiPort`。
    me: Weak<Shared>,
    /// 这次构建的标识，前端的一样时才给快照，见 `AttachMode`。
    build: BuildId,
    /// 这个构建编的快照的格式版本，见 `HostMsg::Welcome`。
    snapshot_format: u16,
    registry: Mutex<Registry>,
    /// 要不要把 shell 集成报告的命令记进历史文件，见 `ClientMsg::SetOptions`。
    record_history: Arc<AtomicBool>,
    /// 下一条连接的编号。
    next_connection: AtomicU64,
    /// 之后启动的 shell 另外设的环境变量，见 `Host::set_env`。
    env: Mutex<Vec<(String, OsString)>>,
    /// 连着的前端、转给界面的请求，以及监听和退出的状态，见 `server::Peers`。锁的先后：拿着它时
    /// 可以再拿 `registry`，反过来不行。
    peers: Mutex<server::Peers>,
    /// 有连接断开、或者要退出时通知，`Host::run_until_idle` 等在它上面。
    peers_changed: Condvar,
    /// 交出会话时给新宿主多久收下会话、回 `HandoffReady`，见 `Host::set_handoff_deadline`。
    handoff_deadline: Mutex<Duration>,
    /// 读剪贴板最多等界面多久，见 `Host::set_clipboard_read_patience`。
    clipboard_patience: Mutex<Duration>,
}

/// 会话、主题和剪贴板的规矩放在同一把锁下：新会话加进来和换主题、改规矩不会互相错过，见
/// `Shared::spawn`。
#[derive(Default)]
struct Registry {
    sessions: HashMap<SessionId, session::Handle>,
    /// 新会话套的主题，见 `ClientMsg::SetTheme`。
    settings: Arc<TermSettings>,
    /// 收到过几次 `SetTheme`。
    theme_generation: u64,
    /// 会话里的程序读写剪贴板的规矩，见 `ClientMsg::SetOptions`。改了就发给每个会话
    /// （`Inbox::Clipboard`），会话线程自己记着一份，不必每块输出都来拿锁。
    clipboard: ClipboardAccess,
}

impl Shared {
    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn peers(&self) -> MutexGuard<'_, server::Peers> {
        self.peers.lock().unwrap_or_else(PoisonError::into_inner)
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

    fn next_connection(&self) -> u64 {
        self.next_connection.fetch_add(1, Ordering::Relaxed)
    }

    /// 新开一个会话，返回它的标识。`extra_env` 是这一个会话另外设的环境变量，盖过 `Host::set_env` 设的同名变量，但盖不了
    /// `runode_protocol::ENV_SESSION`。
    ///
    /// 交接给新宿主期间不开新会话（交出去的会话已经定了，新开的会跟着这个宿主一起退出）。
    fn spawn(&self, options: SpawnOptions, extra_env: Vec<(String, String)>) -> Result<SessionId> {
        let id = SessionId::random()?;
        let (settings, generation) = {
            let registry = self.registry();
            let settings = match &options.settings {
                Some(settings) if registry.theme_generation == 0 => settings.clone(),
                _ => (*registry.settings).clone(),
            };
            (settings, registry.theme_generation)
        };
        let setup = self.setup(id, settings, extra_env);
        let clipboard = setup.clipboard;
        // 开伪终端、启动 shell 要几毫秒，不占着锁。
        let handle = session::spawn(setup, options)?;
        // 锁的先后：拿着 `peers` 再拿 `registry`；交接开始时在同一把锁里定下要交的会话。
        let peers = self.peers();
        if peers.handoff.is_some() {
            drop(peers);
            handle.send(session::Inbox::Kill);
            anyhow::bail!("the host is being upgraded; try again in a moment");
        }
        let mut registry = self.registry();
        // 这期间换过主题的话，那次换主题没赶上这个会话，补上。
        if registry.theme_generation != generation {
            handle.send(session::Inbox::Theme(registry.settings.clone()));
        }
        // 剪贴板的规矩也一样。
        if registry.clipboard != clipboard {
            handle.send(session::Inbox::Clipboard(registry.clipboard));
        }
        registry.sessions.insert(id, handle);
        Ok(id)
    }

    /// 会话 `id` 的设置：主题是 `settings`；启动 shell 时设宿主的环境变量（见 `Host::set_env`），
    /// 同名的由 `extra_env` 盖过，再加上 `runode_protocol::ENV_SESSION`。
    fn setup(&self, id: SessionId, settings: TermSettings, extra_env: Vec<(String, String)>) -> session::Setup {
        let mut env = self.env.lock().unwrap_or_else(PoisonError::into_inner).clone();
        for (key, value) in &extra_env {
            env.retain(|(k, _)| k != key);
            env.push((key.clone(), value.into()));
        }
        env.retain(|(k, _)| k != runode_protocol::ENV_SESSION);
        env.push((runode_protocol::ENV_SESSION.into(), id.to_string().into()));
        let ui = server::UiPort::new(self.me.clone());
        let clipboard = self.registry().clipboard;
        let read_patience = *self.clipboard_patience.lock().unwrap_or_else(PoisonError::into_inner);
        let record_history = self.record_history.clone();
        session::Setup { id, settings, env, extra_env, record_history, ui, clipboard, read_patience }
    }

    /// 改剪贴板的规矩，告诉每个会话；和现在的一样时什么都不做。
    fn set_clipboard(&self, clipboard: ClipboardAccess) {
        let mut registry = self.registry();
        if registry.clipboard == clipboard {
            return;
        }
        registry.clipboard = clipboard;
        for handle in registry.sessions.values() {
            handle.send(session::Inbox::Clipboard(clipboard));
        }
    }

    /// 结束会话：先从登记表里拿掉，再叫它的线程结束，见 `Runner::answer_pending`。没有这个会话
    /// 时返回 false。
    fn kill(&self, id: SessionId) -> bool {
        let handle = self.registry().sessions.remove(&id);
        handle.is_some_and(|handle| {
            handle.send(session::Inbox::Kill);
            true
        })
    }

    /// 结束所有会话。
    fn kill_all(&self) {
        let handles: Vec<_> = self.registry().sessions.drain().map(|(_, handle)| handle).collect();
        for handle in handles {
            handle.send(session::Inbox::Kill);
        }
    }

    /// 换主题，之后新开的会话也用它。
    fn set_theme(&self, settings: TermSettings) {
        let settings = Arc::new(settings);
        let mut registry = self.registry();
        registry.settings = settings.clone();
        registry.theme_generation += 1;
        for handle in registry.sessions.values() {
            handle.send(session::Inbox::Theme(settings.clone()));
        }
    }
}

impl Host {
    /// 一个还没有会话的宿主：主题是默认的，记命令历史。`build` 是这次构建的标识，连上来的前端
    /// 的构建一样时才给快照，见 `AttachMode`；快照的格式版本也在这里算好。
    pub fn new(build: BuildId) -> Self {
        let snapshot_format = runode_terminal::host_session::snapshot_format().unwrap_or_else(|err| {
            tracing::warn!("cannot tell the snapshot format: {err}");
            0
        });
        let shared = |me: &Weak<Shared>| Shared {
            me: me.clone(),
            build,
            snapshot_format,
            registry: Mutex::default(),
            record_history: Arc::new(AtomicBool::new(true)),
            next_connection: AtomicU64::new(1),
            env: Mutex::default(),
            peers: Mutex::default(),
            peers_changed: Condvar::new(),
            handoff_deadline: Mutex::new(handoff::DEFAULT_DEADLINE),
            clipboard_patience: Mutex::new(session::CLIPBOARD_READ_PATIENCE),
        };
        Self { shared: Arc::new_cyclic(shared) }
    }

    /// 交出会话时给接手的新宿主多久收下会话（发送也算在内）、回 `HandoffReady`，默认 20 秒，
    /// 到期就杀掉它、回滚。改了它，`GIVE_READY_WINDOW` 就不准了；测试用来缩短。
    pub fn set_handoff_deadline(&self, deadline: Duration) {
        *self.shared.handoff_deadline.lock().unwrap_or_else(PoisonError::into_inner) = deadline;
    }

    /// 之后开的会话里的程序读剪贴板时最多等界面多久（多半在等用户点询问框），过了就回程序一个空的
    /// 剪贴板，默认 30 秒；已经开着的会话不受影响。测试用来缩短。
    pub fn set_clipboard_read_patience(&self, patience: Duration) {
        *self.shared.clipboard_patience.lock().unwrap_or_else(PoisonError::into_inner) = patience;
    }

    /// 之后启动的每个 shell 都设上这个环境变量，同名的换掉；已经启动的不受影响。每个 shell
    /// 另外还有自己的 `runode_protocol::ENV_SESSION`。
    pub fn set_env(&self, key: &str, value: impl Into<OsString>) {
        let mut env = self.shared.env.lock().unwrap_or_else(PoisonError::into_inner);
        env.retain(|(k, _)| k != key);
        env.push((key.into(), value.into()));
    }
}
