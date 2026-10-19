//! 桌面这一侧连终端宿主的客户端：宿主管着每个会话的 PTY 和权威的那份 VT（见 `runode_host`），
//! 桌面经一条连接（`Link`）按 `runode_protocol` 和它说话。宿主平常单独一个进程跑（`runode --host`），
//! 连不上也拉不起来、或者 socket 被别的宿主占着时才跑在 app 进程里（`Host::connect_pair` 的一对
//! socket），见 `launch::choose_mode`。单独跑的宿主是别的构建时（app 升级了），先让这个构建的新宿主
//! 接手它的会话，见 `handoff`。配置项 `terminal-host` 只管退出时会话留不留下（`quit_keeps_sessions`）。
//!
//! 跑在 app 里的宿主不把会话交出去：shell 是 app 的子进程、算在 app 名下（macOS 的责任进程），app
//! 一退出，交出去的终端就连不上局域网（No route to host）。单独跑的宿主交出会话后等它的 shell 都
//! 结束了才退出，见 `host_process`。
//!
//! 启动时 `start` 在后台线程里读配置、定模式、连上宿主，主线程第一次用 `link` 时等它连好，读到的
//! 配置交给主线程当第一份生效的配置（`take_config`）。
//! 主题和剪贴板的规矩跟着配置走，见 `configure`；别的进程经宿主请界面办的事见 `serve_ui`；
//! 连接断了以后用 `reconnect` 重新连上。

mod handoff;
mod launch;
mod link;

use std::{
    path::{Path, PathBuf},
    sync::{
        Condvar, LazyLock, Mutex, OnceLock, PoisonError,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{App, PromptLevel};
use runode_config::Config;
use runode_host::{BuildId, ClientMsg, Host};
use runode_protocol::{HandoffRefusal, SessionInfo, TaskSource, WindowLayout};

pub use handoff::{HandoffFailure, HandoffStatus, READY_BY};
use launch::{Choice, Probe};
#[cfg(test)]
pub use link::Attached;
pub use link::{ConnectError, Link, LinkEvent, Output, Screen, SpawnOptions, UiTicket};

/// 宿主现在怎么跑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 跑在 app 进程里，app 退出时会话跟着结束，见模块文档。
    InProcess,
    /// 单独一个进程（`runode --host`），app 退出后会话还在。配置项 `terminal-host` 关着时退出时让它
    /// 连会话一起退出（发 `Shutdown { kill_sessions: true }`）。
    Standalone,
}

/// 列会话最多等这么久。
const LIST_TIMEOUT: Duration = Duration::from_secs(2);
/// 看板的请求最多等这么久：宿主办一条要连跑两条 git 命令，比单条 git 的超时长，否则办成了这边却
/// 先超时，晚到的回话（比如开好的会话）也丢了。
const TODO_TIMEOUT: Duration = Duration::from_secs(250);
/// 让留下的宿主退出后，最多等这么久拿到它放开的锁、开自己的 socket。
const LISTEN_RETRY: Duration = Duration::from_secs(2);
/// 另一个新 app 正在让新宿主接手（`HandoffRefusal::Busy`）时，等这么久再探一次：多半会探到它
/// 拉起的、和这边同一个构建的新宿主。
const BUSY_RETRY: Duration = Duration::from_secs(1);
/// 最多这么多次。
const BUSY_TRIES: u32 = 5;
/// 交接后有终端的回滚历史没带过来时发的通知的标识。
const DEGRADED_TAG: &str = "runode-handoff-degraded";

fn build() -> BuildId {
    BuildId(env!("RUNODE_BUILD").into())
}

/// 全进程共用的连接。
static LINK: LazyLock<Link> = LazyLock::new(|| Link::new(build()));
/// `start` 的后台线程连好了（连没连上都算）。
static READY: Mutex<bool> = Mutex::new(false);
static READY_CHANGED: Condvar = Condvar::new();
static MODE: Mutex<Mode> = Mutex::new(Mode::InProcess);
/// 跑在 app 里的宿主，用到时才建。
static IN_PROCESS: OnceLock<Host> = OnceLock::new();
/// 配置项 `terminal-host`：退出后保留会话，跟着配置走，见 `quit_keeps_sessions`。
static KEEP_SESSIONS: AtomicBool = AtomicBool::new(false);
/// 跑在 app 里的宿主的远程访问，第一次要开时才建，见 `configure`。
static REMOTE: Mutex<Option<runode_remote::Service>> = Mutex::new(None);
/// `start` 的后台线程读到的配置，和读之前取的配置文件的修改时间，见 `take_config`。
static LOADED_CONFIG: Mutex<Option<(Config, crate::config::Stamps)>> = Mutex::new(None);
/// 要在界面上告诉用户的事（比如旧版本的宿主还活着），按发生的先后，见 `take_notice`。
static NOTICE: Mutex<Vec<Notice>> = Mutex::new(Vec::new());

/// 要在界面上告诉用户的宿主的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// socket 上的宿主协议对不上（多半是旧版本的还活着），这次跑在 app 里，命令行连不上 app。用户
    /// 要结束旧宿主却没结束成时也用它，界面上说的一样。
    Incompatible(String),
    /// socket 上是另一个 runode app 进程里的宿主：这次跑在 app 里、不开 socket，命令行连到的是
    /// 那个 app。
    OtherApp,
    /// 要单独一个进程的宿主，却连不上也拉不起来，这次跑在 app 里，退出时会话跟着结束。
    Unreachable(String),
    /// 升级时没能让新宿主接手旧宿主的会话（拒绝、出错、超时、新宿主崩溃）：旧宿主连同它的
    /// `sessions` 个会话（协议对不上时不知道几个）照常跑着，这次跑在 app 里、不开 socket。问用户
    /// 留着它还是结束它。
    HandoffFailed { reason: String, sessions: Option<usize> },
    /// 交接成了，但 `count` 个终端退成了重放，回滚历史没带过来。不用用户做什么，不弹框。
    HandoffDegraded { count: usize },
    /// 旧版本的 app 还开着：它连着旧宿主、旧宿主不交（`HandoffRefusal::DesktopConnected`），或者
    /// 用户要结束的旧宿主其实跑在它的进程里（见 `launch::Ended::NotAHost`）。这次跑在 app 里、
    /// 不开 socket，请用户先退出旧版本。
    OldAppRunning,
    /// 用户要结束旧宿主，socket 上却已经是这个构建的宿主了（交接其实成了，或者另一个同版本的
    /// app 让它接手了）：没结束它，会话在它那里；这个 app 已经跑着自己的宿主，下次打开时连上它。
    AlreadyUpgraded,
    /// 连上的单独宿主是别的构建（`host` 是它报的构建号）：交接时又被别的版本接手了之类。终端照常
    /// 能用，提醒用户退出后重新打开，让这个构建的宿主接手。
    BuildMismatch { host: String },
}

fn notify(notice: Notice) {
    NOTICE.lock().unwrap_or_else(PoisonError::into_inner).push(notice);
}

/// 启动时在后台线程 `host-connect` 里：读配置、定宿主怎么跑、连上它，再做 `then`（比如提前拉起
/// 第一个 shell）。主线程第一次调 `link` 时等它连好。读到的配置留一份，主线程用 `take_config`
/// 取去当第一份生效的配置，不用再读一遍。
pub fn start(then: impl FnOnce(&Config) + Send + 'static) {
    let spawned = thread::Builder::new().name("host-connect".into()).spawn(move || {
        let ready = MarkReady;
        // 还不知道系统外观，先按深色读；主线程拿到时外观不一样、主题又跟着外观走就重读，见
        // `Config::fits_appearance`。配置有问题时在这里报告。
        let before = crate::config::watch_stamp(&Config::default());
        let config = Config::load(true);
        KEEP_SESSIONS.store(config.terminal_host, Ordering::Relaxed);
        establish();
        *LOADED_CONFIG.lock().unwrap_or_else(PoisonError::into_inner) = Some((config.clone(), before));
        drop(ready);
        then(&config);
    });
    if let Err(err) = spawned {
        tracing::warn!("failed to start the host-connect thread, connecting in place: {err}");
        let _ready = MarkReady;
        establish();
    }
}

/// 丢掉时（包括后台线程 panic 时）放行等着 `link` 的主线程。
struct MarkReady;

impl Drop for MarkReady {
    fn drop(&mut self) {
        *READY.lock().unwrap_or_else(PoisonError::into_inner) = true;
        READY_CHANGED.notify_all();
    }
}

/// `start` 的后台线程读到的配置，和读之前按默认配置要盯的文件取的修改时间（交给
/// `config::seen_after`），只给一次；`start` 还没连好时等它，没读（后台线程没起来）时为 `None`。
pub fn take_config() -> Option<(Config, crate::config::Stamps)> {
    link();
    LOADED_CONFIG.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// 到宿主的连接；`start` 还没连好时等它。连不上时返回的连接没连着，用起来什么都不做。
pub fn link() -> &'static Link {
    let mut ready = READY.lock().unwrap_or_else(PoisonError::into_inner);
    while !*ready {
        ready = READY_CHANGED.wait(ready).unwrap_or_else(PoisonError::into_inner);
    }
    &LINK
}

/// 宿主现在怎么跑；`start` 还没定下时等它，同 `link`。
pub fn mode() -> Mode {
    link();
    *MODE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 退出时要不要把会话留下：配置项 `terminal-host`，设置里改了马上算数，不用重启。宿主跑在 app 里时
/// 留不下，见模块文档。
pub fn quit_keeps_sessions() -> bool {
    KEEP_SESSIONS.load(Ordering::Relaxed)
}

/// 取走要在界面上告诉用户的事，按发生的先后，只给一次。
pub fn take_notice() -> Vec<Notice> {
    std::mem::take(&mut *NOTICE.lock().unwrap_or_else(PoisonError::into_inner))
}

/// 把配置里宿主关心的部分告诉它：主题（各个会话在输出流里标出换主题的位置，视图到那里再换）、
/// 不把命令记进历史文件，以及终端里的程序读写剪贴板的规矩。重连后 `Link` 自己补发。
///
/// 宿主跑在 app 里时，远程访问的监听也开在 app 里，在这里按配置开关；单独跑的宿主自己读配置，
/// 见 `remote_access`。
pub fn configure(config: &Config) {
    let link = link();
    link.send(ClientMsg::SetTheme { settings: config.term_settings() });
    link.send(ClientMsg::SetOptions { record_history: false, clipboard: config.clipboard_access() });
    KEEP_SESSIONS.store(config.terminal_host, Ordering::Relaxed);
    let in_app = mode() == Mode::InProcess;
    let port = if in_app { crate::remote_access::wanted_port(config) } else { None };
    let mut remote = REMOTE.lock().unwrap_or_else(PoisonError::into_inner);
    if remote.is_none()
        && port.is_some()
        && let Some(host) = IN_PROCESS.get()
    {
        *remote = crate::remote_access::service(host);
    }
    if let Some(service) = remote.as_ref() {
        service.set(port, config.remote_access_name.clone());
        service.set_push(crate::remote_access::push_settings(config));
    }
}

/// 宿主转给界面去办的请求（`runode open`、`runode focus` 这类），带着回话用的编号，办完了用
/// `Link::ui_reply` 回话。只能取一次。
pub fn serve_ui() -> Option<UnboundedReceiver<(UiTicket, ClientMsg)>> {
    link().ui_requests()
}

/// 看板变了、或者换了一条连接要重新问看板，见 `Link::todos_changed`。只能取一次。
pub fn todos_changed() -> Option<UnboundedReceiver<()>> {
    link().todos_changed()
}

/// 和宿主的连接断了以后重新连上：跑在 app 里时重开一对 socket；单独一个进程时重连它的 socket，
/// 没有就重新拉起，拉不起来时改成跑在 app 里。重连期间最多卡住调用的线程约 2 秒。已经连着时什么都
/// 不做。之前的会话要重新 `attach`。
pub fn reconnect() -> Result<()> {
    let link = link();
    if link.connected() {
        return Ok(());
    }
    let mode = match mode() {
        Mode::InProcess => in_process(false),
        Mode::Standalone => {
            let socket = socket_path().ok_or_else(|| anyhow!("no place for the host socket"))?;
            let exe = std::env::current_exe()?;
            match launch::connect_or_launch(link, &socket, &exe) {
                Ok(()) => Mode::Standalone,
                Err(ConnectError::NotStandalone) => {
                    tracing::warn!("another runode app took the host socket, running the host in this app");
                    in_process(false)
                }
                // 卡住、握手超时也报 `Io`：socket 还连得上就先不当它没了，报错等下次再连，免得改在 app 里另起
                // 宿主后，原来的会话再也连不回。连着 `STUCK_TRIES` 次都这样就当它卡死了，照样改在 app 里跑，
                // 不然重开永远没反应。
                Err(err) if socket_gone(&socket) || STUCK.fetch_add(1, Ordering::Relaxed) + 1 >= STUCK_TRIES => {
                    tracing::info!("the host is gone or stuck, running the host in the app: {err}");
                    in_process(true)
                }
                Err(err) => return Err(err.into()),
            }
        }
    };
    *MODE.lock().unwrap_or_else(PoisonError::into_inner) = mode;
    STUCK.store(0, Ordering::Relaxed);
    if link.connected() { Ok(()) } else { Err(anyhow!("could not reconnect to the host")) }
}

/// `reconnect` 连着几次连上了 socket 却握不了手，见那里。
static STUCK: AtomicU32 = AtomicU32::new(0);
const STUCK_TRIES: u32 = 3;

/// 没有宿主在 `socket` 上听了。
fn socket_gone(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_err()
}

/// 单独跑的宿主上除了这个 app 还有别的桌面连着：开关关着时退出只结束这个 app 窗口里的会话，不让宿主
/// 退出，免得连别人的会话一起结束。最多卡住调用的线程 `launch::probe` 的超时；问不到时当作没有。
pub fn other_desktops() -> bool {
    if mode() != Mode::Standalone {
        return false;
    }
    let Some(socket) = socket_path() else { return false };
    // 探的是命令行的连接，连着的桌面里有这个 app 自己。
    let ours = usize::from(link().connected());
    matches!(launch::probe(&socket, &build()), Probe::Running { desktops, .. } if desktops > ours)
}

/// 宿主里所有的会话。
pub fn list_sessions() -> Result<Vec<SessionInfo>> {
    link().list_sessions(LIST_TIMEOUT)
}

/// 宿主手里的布局，见 `Link::held_layout`。
pub fn held_layout() -> Result<Vec<WindowLayout>> {
    link().held_layout(LIST_TIMEOUT)
}

/// 宿主在 `dir` 里列出的命令，见 `ClientMsg::ListTasks`。会等宿主回话，别在主线程上调。
pub fn list_tasks(dir: PathBuf) -> Result<Vec<TaskSource>> {
    link().list_tasks(dir, LIST_TIMEOUT)
}

/// 发一条看板的请求（`ClientMsg::ListTodos` 等），等宿主回话；宿主不认得这些消息时直接报错，见
/// `Link::host_knows_todos`。开始任务要建工作树，大仓库上要一会儿，所以等得久些。会等宿主回话，别在
/// 主线程上调。
pub fn ask_todos(make: impl FnOnce(u32) -> ClientMsg) -> Result<runode_protocol::HostMsg> {
    let link = link();
    if !link.host_knows_todos() {
        return Err(anyhow!(rust_i18n::t!("kanban.host_too_old").into_owned()));
    }
    link.request(make, TODO_TIMEOUT)
}

/// 宿主的 socket 放在哪；建不了 `run/` 目录或者路径太长时为 `None`。
fn socket_path() -> Option<PathBuf> {
    let dirs = runode_paths::Dirs::from_env();
    match dirs.create_runtime_dir() {
        Ok(_) => dirs.host_socket_file(),
        Err(err) => {
            tracing::warn!("no place for the host socket: {err}");
            None
        }
    }
}

/// 定下宿主怎么跑、连上它，见 `launch::choose_mode`。
fn establish() {
    let build = build();
    let socket = socket_path();
    let mut busy = 0;
    let mode = loop {
        let probe = socket.as_deref().map_or(Probe::Absent, |socket| launch::probe(socket, &build));
        let choice = launch::choose_mode(&probe, &build);
        tracing::info!("host: {probe:?}, so {choice:?}");
        if probe == Probe::OtherApp {
            tracing::warn!("another runode app runs the host on the socket, so this one runs its own without a socket");
            notify(Notice::OtherApp);
        }
        break match (choice, socket.as_deref()) {
            (Choice::InProcess, _) => in_process(false),
            (Choice::Keep, Some(socket)) => {
                let connected = launch::connect(&LINK, socket).or_else(|err| match err {
                    ConnectError::Incompatible(_) | ConnectError::NotStandalone => Err(err),
                    // 宿主在跑却连不上（比如卡住了，或者刚好在退出）：拉起新的；它还拿着锁时新拉起的抢不到、
                    // 自己退出，等它放开后再拉。
                    err => {
                        tracing::warn!("failed to connect to the running host, starting one: {err}");
                        launch_host(socket)
                    }
                });
                match connected {
                    Ok(()) => Mode::Standalone,
                    Err(err) => fall_back(err),
                }
            }
            (Choice::Launch, Some(socket)) => match launch_host(socket) {
                Ok(()) => Mode::Standalone,
                Err(err) => fall_back(err),
            },
            (Choice::HandOver, Some(socket)) => {
                let sessions = match &probe {
                    Probe::Running { sessions, .. } => Some(*sessions),
                    _ => None,
                };
                match hand_over(socket, sessions) {
                    Some(mode) => mode,
                    None if busy < BUSY_TRIES => {
                        busy += 1;
                        thread::sleep(BUSY_RETRY);
                        continue;
                    }
                    None => {
                        let reason = "another runode kept upgrading the host".to_owned();
                        tracing::warn!("{reason}, running the host in the app without a socket");
                        notify(Notice::HandoffFailed { reason, sessions });
                        in_process(false)
                    }
                }
            }
            (_, None) => in_process(false),
        };
    };
    if mode == Mode::Standalone
        && let Some(theirs) = LINK.host_build()
        && theirs != build
    {
        tracing::warn!("connected to a host of build {theirs:?}, this app is {build:?}");
        notify(Notice::BuildMismatch { host: theirs.0 });
    }
    *MODE.lock().unwrap_or_else(PoisonError::into_inner) = mode;
}

/// 让这个构建的新宿主接手 `socket` 上旧宿主的会话（见 `handoff::hand_over`），成了就连上它。
/// `sessions` 是旧宿主有几个会话，协议对不上时不知道。另一个新 app 正在交接时返回 `None`，由
/// 调用方过一会儿重新探。
///
/// 没交成时旧宿主连同会话照常跑着，这次跑在 app 里、不开 socket（锁在它手里），弹框问用户；只有
/// 旧宿主没有会话时，没什么可丢的，让它退出、拉起这个构建的。
fn hand_over(socket: &Path, sessions: Option<usize>) -> Option<Mode> {
    let started = Instant::now();
    let outcome = match std::env::current_exe() {
        Ok(exe) => handoff::hand_over(&exe, handoff::Timing::default()),
        Err(err) => handoff::Outcome::Failed(format!("cannot tell where this app is: {err}")),
    };
    tracing::info!("handoff: {outcome:?} after {:?}", started.elapsed());
    let failed = |reason: String| {
        // 交接要等上几十秒，其间旧宿主上可能开了新会话（手机、命令行），结束前重探一次。卡住、握手超时时
        // `probe` 也报 `Absent`，socket 还连得上就不当它没了。
        let still_empty = || match launch::probe(socket, &build()) {
            Probe::Running { sessions: 0, .. } => true,
            Probe::Absent => socket_gone(socket),
            _ => false,
        };
        if sessions == Some(0) && still_empty() {
            tracing::warn!("the handoff failed ({reason}); the old host has no sessions, so replacing it");
            if let Err(err) = launch::retire(socket, &build()) {
                tracing::warn!("failed to stop the old host: {err}");
            }
            return match launch_host(socket) {
                Ok(()) => Mode::Standalone,
                Err(err) => fall_back(err),
            };
        }
        tracing::warn!("the handoff failed, running the host in the app without a socket: {reason}");
        notify(Notice::HandoffFailed { reason, sessions });
        in_process(false)
    };
    let mode = match outcome {
        handoff::Outcome::TookOver { sessions, replayed } => {
            tracing::info!("the new host took over {sessions} sessions, {replayed} of them without scrollback");
            // 接手的是这个构建的新宿主；对不上时（又被别的版本接手了）照样连，记一笔。
            if let Probe::Running { build: theirs, .. } = launch::probe(socket, &build())
                && theirs != build()
            {
                tracing::warn!("expected the new host to be build {:?}, found {theirs:?}", build());
            }
            match launch::connect(&LINK, socket) {
                Ok(()) => {
                    if replayed > 0 {
                        notify(Notice::HandoffDegraded { count: replayed });
                    }
                    Mode::Standalone
                }
                Err(err) => fall_back(err),
            }
        }
        handoff::Outcome::Refused(HandoffRefusal::Busy) => return None,
        handoff::Outcome::Refused(HandoffRefusal::DesktopConnected) => {
            tracing::warn!("an older runode app is still connected to the old host, running the host in the app");
            notify(Notice::OldAppRunning);
            in_process(false)
        }
        handoff::Outcome::Refused(HandoffRefusal::NotStandalone) => {
            tracing::warn!("the host on the socket now runs inside another runode app, running this one's in the app");
            notify(Notice::OtherApp);
            in_process(false)
        }
        handoff::Outcome::Refused(reason) => failed(format!("the old host refused to hand over: {reason:?}")),
        handoff::Outcome::Failed(reason) => failed(reason),
    };
    Some(mode)
}

/// 用户在弹框里选了结束旧宿主（连同它的会话）：在后台线程里结束它（见 `launch::end_old_host`），
/// 之后这个 app 里的宿主开 socket，让命令行连得上。对面不是单独的宿主进程时不结束它，改请用户先
/// 退出那个 app（`Notice::OldAppRunning`）。
///
/// 动手前再探一次：旧宿主已经退了的直接开 socket；socket 上已经是这个构建的宿主时不能结束它，
/// 那会结束刚接过去的会话，只告诉用户（`Notice::AlreadyUpgraded`）。这个 app 不改连过去：窗口里的
/// 终端都在 app 自己的宿主里。
fn end_old_host(cx: &mut gpui::AsyncApp) {
    let (tx, rx) = futures::channel::oneshot::channel();
    let spawned = thread::Builder::new().name("end-old-host".into()).spawn(move || {
        let Some(socket) = socket_path() else { return };
        let ended = match launch::probe(&socket, &build()) {
            Probe::Running { build: theirs, .. } if theirs == build() => {
                tracing::warn!("the host on the socket is already this build, so not ending it");
                let _ = tx.send(Notice::AlreadyUpgraded);
                return;
            }
            // 弹框期间旧宿主自己退了：不用结束谁，直接开 socket。旧宿主卡住、握手超时时 `probe` 也报
            // `Absent`，那时 socket 还连得上，照常去结束它。
            Probe::Absent if socket_gone(&socket) => {
                tracing::info!("the old host is already gone");
                Ok(launch::Ended::Ended)
            }
            // 探完到动手之间旧宿主也可能刚好退了，结束它就会出错：socket 连不上了的不算失败。
            _ => launch::end_old_host(&socket, &build()).or_else(|err| {
                if !socket_gone(&socket) {
                    return Err(err);
                }
                tracing::info!("the old host went away while ending it: {err}");
                Ok(launch::Ended::Ended)
            }),
        };
        match ended {
            Ok(launch::Ended::Ended) => {
                tracing::info!("the old host and its sessions are gone");
                if let Some(host) = IN_PROCESS.get() {
                    listen_in_app(host);
                }
            }
            Ok(launch::Ended::NotAHost) => {
                tracing::warn!("the old host runs inside an older runode app, asking to quit it instead");
                let _ = tx.send(Notice::OldAppRunning);
            }
            Err(err) => {
                tracing::warn!("failed to end the old host: {err}");
                // 旧宿主多半还活着、这个 app 也没开 socket，和协议对不上时一样：终端跑在 app 里，命令行
                // 连不上它们，请用户退出旧的或结束它的宿主进程后重开。
                let _ = tx.send(Notice::Incompatible(err.to_string()));
            }
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("failed to start ending the old host: {err}");
        return;
    }
    cx.spawn(async move |cx| {
        if let Ok(notice) = rx.await {
            notify(notice);
            cx.update(show_notice);
        }
    })
    .detach();
}

/// 连上 socket 上单独一个进程的宿主，没有就拉起一个，见 `launch::connect_or_launch`。
fn launch_host(socket: &Path) -> Result<(), ConnectError> {
    std::env::current_exe().map_err(ConnectError::Io).and_then(|exe| launch::connect_or_launch(&LINK, socket, &exe))
}

/// 单独一个进程的宿主连不上也拉不起来（`err`）：这次跑在 app 里，告诉用户。协议对不上、socket 上
/// 是另一个 app 的宿主时锁在别人手里，不开 socket。
fn fall_back(err: ConnectError) -> Mode {
    match err {
        ConnectError::Incompatible(reason) => {
            tracing::warn!("an incompatible host took the socket, running the host in the app: {reason}");
            notify(Notice::Incompatible(reason));
            in_process(false)
        }
        ConnectError::NotStandalone => {
            tracing::warn!("another runode app runs the host on the socket, running this one's in the app");
            notify(Notice::OtherApp);
            in_process(false)
        }
        err => {
            tracing::warn!("failed to reach or start the host process, running it in the app: {err}");
            notify(Notice::Unreachable(err.to_string()));
            in_process(true)
        }
    }
}

/// 让宿主之后启动的 shell 能直接敲 `runode`：`runode_cli::ENV_BIN` 指向这个可执行文件，它所在的
/// 目录追加到 `PATH` 末尾。设在环境里而不是只在 shell 集成里定义函数，是因为 agent 跑命令时另起
/// 非交互的 shell，不加载集成脚本，但继承环境。追加在末尾，不盖过用户自己装的同名程序。
pub(crate) fn expose_cli(host: &Host) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if let Some(path) = exe.parent().and_then(|dir| path_with(std::env::var_os("PATH"), dir)) {
        host.set_env("PATH", path);
    }
    host.set_env(runode_cli::ENV_BIN, exe);
}

/// `path` 末尾追加 `dir` 后的 `PATH`；已经在里面时原样返回，拼不出来（目录名里有分隔符）时为 `None`。
fn path_with(path: Option<std::ffi::OsString>, dir: &Path) -> Option<std::ffi::OsString> {
    let mut dirs: Vec<PathBuf> = path.as_deref().map(|path| std::env::split_paths(path).collect()).unwrap_or_default();
    if !dirs.iter().any(|existing| existing == dir) {
        dirs.push(dir.to_path_buf());
    }
    std::env::join_paths(dirs).ok()
}

/// 宿主跑在 app 里：建好它（已经建过就用原来的），`listen` 时先开 socket（之后启动的 shell 才知道
/// 命令行该连哪里），再经一对 socket 连上。
fn in_process(listen: bool) -> Mode {
    let mut created = false;
    let host = IN_PROCESS.get_or_init(|| {
        created = true;
        let host = Host::new(build());
        expose_cli(&host);
        host
    });
    if listen && created {
        listen_in_app(host);
    }
    match host.connect_pair() {
        Ok(stream) => {
            if let Err(err) = LINK.connect(stream) {
                tracing::error!("failed to connect to the host in the app: {err}");
            }
        }
        Err(err) => tracing::error!("failed to connect to the host in the app: {err}"),
    }
    Mode::InProcess
}

/// 在 runode 自己的 `run/` 目录里开宿主的 socket，让命令行这类别的进程连上来。刚让留下的宿主
/// 退出时，它放开锁要一会儿，最多等 `LISTEN_RETRY`。开不了时记一笔日志，app 照常用。
fn listen_in_app(host: &Host) {
    let dirs = runode_paths::Dirs::from_env();
    let deadline = Instant::now() + LISTEN_RETRY;
    loop {
        match listen(host, &dirs) {
            Ok(socket) => {
                tracing::info!("host listening on {}", socket.display());
                return;
            }
            Err(err) if Instant::now() >= deadline => {
                tracing::warn!("the host is not listening for other processes: {err:#}");
                return;
            }
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// 建好 runode 自己的 `run/` 目录，拿着宿主的锁在宿主的 socket 上开监听，再打开看板，返回 socket 的
/// 路径。跑在 app 里的宿主（`listen_in_app`）和单独跑的宿主（`host_process::run`）都这样开。
pub fn listen(host: &Host, dirs: &runode_paths::Dirs) -> Result<PathBuf> {
    dirs.create_runtime_dir()?;
    let socket = dirs.host_socket_file().ok_or_else(|| anyhow!("the socket path is too long"))?;
    let lock = dirs.host_lock_file().ok_or_else(|| anyhow!("no place for the host lock"))?;
    host.listen(&socket, &lock)?;
    // 拿着锁的宿主只有一个，看板和定时也只由它办。
    if let Some(db) = dirs.todos_db() {
        host.serve_todos(db);
    }
    Ok(socket)
}

/// 有要告诉用户的宿主的事（见 `Notice`）时，在最前面的窗口上弹框说一声；交接没成时弹框问用户
/// 留着旧宿主（默认）还是结束它。回滚历史没带过来这种不用做决定的事发系统通知，不弹框。同一次
/// 启动里攒了几件要弹框的事时只弹最后那件，系统通知照发。
pub fn show_notice(cx: &mut App) {
    let mut last = None;
    for notice in take_notice() {
        match notice {
            Notice::HandoffDegraded { count } => report_degraded(count, cx),
            notice => last = Some(notice),
        }
    }
    let Some(notice) = last else { return };
    // `end`：问用户留着旧宿主还是结束它。
    let (title, detail, end) = match notice {
        Notice::Incompatible(reason) => (
            rust_i18n::t!("host.incompatible_title"),
            rust_i18n::t!("host.incompatible_detail", reason = reason),
            false,
        ),
        Notice::OtherApp => (rust_i18n::t!("host.other_app_title"), rust_i18n::t!("host.other_app_detail"), false),
        Notice::Unreachable(reason) => {
            (rust_i18n::t!("host.unreachable_title"), rust_i18n::t!("host.unreachable_detail", reason = reason), false)
        }
        Notice::OldAppRunning => (rust_i18n::t!("host.old_app_title"), rust_i18n::t!("host.old_app_detail"), false),
        Notice::AlreadyUpgraded => {
            (rust_i18n::t!("host.already_upgraded_title"), rust_i18n::t!("host.already_upgraded_detail"), false)
        }
        Notice::HandoffFailed { reason, sessions } => {
            let detail = match sessions {
                Some(count) => rust_i18n::t!("host.handoff_failed_detail", count = count, reason = reason),
                None => rust_i18n::t!("host.handoff_failed_detail_uncounted", reason = reason),
            };
            (rust_i18n::t!("host.handoff_failed_title"), detail, true)
        }
        Notice::BuildMismatch { host } => (
            rust_i18n::t!("host.build_mismatch_title"),
            rust_i18n::t!("host.build_mismatch_detail", host = host, app = build().0),
            false,
        ),
        Notice::HandoffDegraded { .. } => return,
    };
    let Some(window) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else {
        return;
    };
    let ok = rust_i18n::t!("host.ok");
    let (keep, end_old) = (rust_i18n::t!("host.keep_old"), rust_i18n::t!("host.end_old"));
    // 默认（第一个）按钮留着旧宿主。
    let answers: Vec<&str> = if end { vec![&keep, &end_old] } else { vec![&ok] };
    let answer =
        window.update(cx, |_, window, cx| window.prompt(PromptLevel::Warning, &title, Some(&detail), &answers, cx));
    if let Ok(answer) = answer {
        cx.spawn(async move |cx| {
            if end && answer.await == Ok(1) {
                end_old_host(cx);
            }
        })
        .detach();
    }
}

/// 交接后 `count` 个终端的回滚历史没带过来：发一条系统通知。不在 .app 里时通知中心用不了，只记
/// 日志。
fn report_degraded(count: usize, cx: &mut App) {
    tracing::warn!("the scrollback of {count} terminals was not carried over in the handoff");
    #[cfg(target_os = "macos")]
    if crate::about::in_app_bundle() {
        cx.show_system_notification(gpui::SystemNotification {
            tag: DEGRADED_TAG.into(),
            title: rust_i18n::t!("host.degraded_title").into_owned().into(),
            body: rust_i18n::t!("host.degraded_detail", count = count).into_owned().into(),
            actions: Vec::new(),
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (cx, DEGRADED_TAG);
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, path::Path};

    use super::path_with;

    #[test]
    fn the_runode_directory_goes_last_on_the_path() {
        let path =
            path_with(Some(OsString::from("/usr/bin:/bin")), Path::new("/Applications/Runode.app/Contents/MacOS"));
        assert_eq!(path, Some(OsString::from("/usr/bin:/bin:/Applications/Runode.app/Contents/MacOS")));
    }

    #[test]
    fn a_path_that_has_it_already_stays_as_it_is() {
        let path = path_with(Some(OsString::from("/opt/runode:/usr/bin")), Path::new("/opt/runode"));
        assert_eq!(path, Some(OsString::from("/opt/runode:/usr/bin")));
    }

    #[test]
    fn without_a_path_it_is_the_only_entry() {
        assert_eq!(path_with(None, Path::new("/opt/runode")), Some(OsString::from("/opt/runode")));
    }
}
