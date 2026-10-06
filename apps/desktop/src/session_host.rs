//! 桌面和终端宿主之间：宿主管着每个会话的 PTY 和权威的那份 VT（见 `runode_host`），桌面经一条
//! 连接（`Link`）按 `runode_protocol` 和它说话。宿主跑在 app 进程里（`Host::connect_pair` 的一对
//! socket），或者单独一个进程（`runode --host`，app 退出后会话还在），由配置项 `terminal-host`
//! 在启动时定下，见 `launch::choose_mode`。单独跑的宿主是别的构建时（app 升级了），先让这个构建
//! 的新宿主接手它的会话，见 `handoff`。
//!
//! 启动时 `start` 在后台线程里读配置、定模式、连上宿主，主线程第一次用 `link` 时等它连好，读到的
//! 配置交给主线程当第一份生效的配置（`take_config`）。
//! 主题和要不要记命令历史跟着配置走，见 `configure`；别的进程经宿主请界面办的事见 `serve_ui`；
//! 连接断了以后用 `reconnect` 重新连上。

mod handoff;
mod launch;
mod link;

use std::{
    path::{Path, PathBuf},
    sync::{Condvar, LazyLock, Mutex, OnceLock, PoisonError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{App, PromptLevel};
use runode_config::Config;
use runode_host::{BuildId, ClientMsg, Host};
use runode_protocol::{HandoffRefusal, SessionInfo};

pub use handoff::{HandoffFailure, HandoffStatus, READY_BY};
use launch::{Choice, Probe};
// `Attached` 给视图状态机（重新连上、只看状态）用。
#[allow(unused_imports)]
pub use link::{Attached, ConnectError, Link, LinkEvent, Screen, SpawnOptions};

/// 宿主现在怎么跑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 跑在 app 进程里，app 退出时会话跟着结束。
    InProcess,
    /// 单独一个进程（`runode --host`），app 退出后会话还在。`end_on_quit` 时配置项 `terminal-host`
    /// 已经关了，这次是把上次留下的会话接回来：这次退出时要让它连会话一起退出（发
    /// `Shutdown { kill_sessions: true }`），下次启动就跑在 app 里。
    Standalone { end_on_quit: bool },
}

/// 列会话最多等这么久。
const LIST_TIMEOUT: Duration = Duration::from_secs(2);
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
/// `start` 的后台线程读到的配置，见 `take_config`。
static LOADED_CONFIG: Mutex<Option<Config>> = Mutex::new(None);
/// 要在界面上告诉用户的事（比如旧版本的宿主还活着），见 `take_notice`。
static NOTICE: Mutex<Option<Notice>> = Mutex::new(None);

/// 要在界面上告诉用户的宿主的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// socket 上的宿主协议对不上（多半是旧版本的还活着），这次跑在 app 里，命令行连不上 app。
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
    /// socket 上的旧宿主太老，不会交接：同上，问用户留着它还是结束它。
    PreHandoff,
    /// 交接成了，但 `count` 个终端退成了重放，回滚历史没带过来。不用用户做什么，不弹框。
    HandoffDegraded { count: usize },
    /// 旧版本的 app 还开着：它连着旧宿主、旧宿主不交（`HandoffRefusal::DesktopConnected`），或者
    /// 用户要结束的旧宿主其实跑在它的进程里（见 `launch::Ended::NotAHost`）。这次跑在 app 里、
    /// 不开 socket，请用户先退出旧版本。
    OldAppRunning,
    /// 用户要结束旧宿主，socket 上却已经是这个构建的宿主了（交接其实成了，或者另一个同版本的
    /// app 让它接手了）：没结束它，会话在它那里；这个 app 已经跑着自己的宿主，下次打开时连上它。
    AlreadyUpgraded,
}

fn notify(notice: Notice) {
    *NOTICE.lock().unwrap_or_else(PoisonError::into_inner) = Some(notice);
}

/// 启动时在后台线程 `host-connect` 里：读配置、定宿主怎么跑、连上它，再做 `then`（比如提前拉起
/// 第一个 shell）。主线程第一次调 `link` 时等它连好。读到的配置留一份，主线程用 `take_config`
/// 取去当第一份生效的配置，不用再读一遍。
pub fn start(then: impl FnOnce(&Config) + Send + 'static) {
    let spawned = thread::Builder::new().name("host-connect".into()).spawn(move || {
        let ready = MarkReady;
        // 还不知道系统外观，先按深色读；主线程拿到时外观不一样、主题又跟着外观走就重读，见
        // `Config::fits_appearance`。配置有问题时在这里报告。
        let config = Config::load(true);
        establish(config.terminal_host);
        *LOADED_CONFIG.lock().unwrap_or_else(PoisonError::into_inner) = Some(config.clone());
        drop(ready);
        then(&config);
    });
    if let Err(err) = spawned {
        tracing::warn!("failed to start the host-connect thread, connecting in place: {err}");
        let _ready = MarkReady;
        establish(false);
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

/// `start` 的后台线程读到的配置，只给一次；`start` 还没连好时等它，没读（后台线程没起来）时为
/// `None`。
pub fn take_config() -> Option<Config> {
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

/// 取走要在界面上告诉用户的事，只给一次。
pub fn take_notice() -> Option<Notice> {
    NOTICE.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// 把配置里宿主关心的部分告诉它：主题（各个会话在输出流里标出换主题的位置，视图到那里再换）
/// 和要不要把命令记进历史文件。和宿主现在的一样时它什么都不做；重连后 `Link` 自己补发。
pub fn configure(config: &Config) {
    let link = link();
    link.send(ClientMsg::SetTheme { settings: config.term_settings() });
    link.send(ClientMsg::SetOptions { record_history: config.command_suggestions });
}

/// 宿主转给界面去办的请求（`runode open`、`runode focus` 这类），带着回话用的编号，办完了用
/// `Link::ui_reply` 回话。只能取一次。
pub fn serve_ui() -> Option<UnboundedReceiver<(u64, ClientMsg)>> {
    link().ui_requests()
}

/// 和宿主的连接断了以后重新连上：跑在 app 里时重开一对 socket；单独一个进程时重连它的 socket，
/// 没有就重新拉起（`terminal-host` 已经关了、只是接回上次的会话时，改成跑在 app 里）。重连期间
/// 最多卡住调用的线程约 2 秒。已经连着时什么都不做。之前的会话要重新 `attach`。
// 宿主断开后「在原目录重开」用上它之前先放着。
#[allow(dead_code)]
pub fn reconnect() -> Result<()> {
    let link = link();
    if link.connected() {
        return Ok(());
    }
    let mode = match mode() {
        Mode::InProcess => in_process(false),
        Mode::Standalone { end_on_quit } => {
            let socket = socket_path().ok_or_else(|| anyhow!("no place for the host socket"))?;
            let connected = if end_on_quit {
                launch::connect(link, &socket)
            } else {
                let exe = std::env::current_exe()?;
                launch::connect_or_launch(link, &socket, &exe)
            };
            match connected {
                Ok(()) => Mode::Standalone { end_on_quit },
                Err(ConnectError::NotStandalone) => {
                    tracing::warn!("another runode app took the host socket, running the host in this app");
                    in_process(false)
                }
                Err(err) if end_on_quit => {
                    tracing::info!("the leftover host is gone, running the host in the app: {err}");
                    in_process(true)
                }
                Err(err) => return Err(err.into()),
            }
        }
    };
    *MODE.lock().unwrap_or_else(PoisonError::into_inner) = mode;
    if link.connected() { Ok(()) } else { Err(anyhow!("could not reconnect to the host")) }
}

/// 宿主里所有的会话。
pub fn list_sessions() -> Result<Vec<SessionInfo>> {
    link().list_sessions(LIST_TIMEOUT)
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

/// 按配置项 `terminal-host` 定下宿主怎么跑、连上它。
fn establish(terminal_host: bool) {
    let build = build();
    let socket = socket_path();
    let mut busy = 0;
    let mode = loop {
        let probe = socket.as_deref().map_or(Probe::Absent, |socket| launch::probe(socket, &build));
        let choice = launch::choose_mode(terminal_host, &probe, &build);
        tracing::info!("host: {probe:?} with terminal-host = {terminal_host}, so {choice:?}");
        if probe == Probe::OtherApp {
            tracing::warn!("another runode app runs the host on the socket, so this one runs its own without a socket");
            notify(Notice::OtherApp);
        }
        break match (choice, socket.as_deref()) {
            (Choice::InProcess { listen }, _) => in_process(listen),
            (Choice::PreHandoff, _) => {
                tracing::warn!("a host too old to hand over is running, so the host runs in the app without a socket");
                notify(Notice::PreHandoff);
                in_process(false)
            }
            (Choice::Retire, Some(socket)) => {
                if let Err(err) = launch::retire(socket, &build) {
                    tracing::warn!("failed to stop the leftover host: {err}");
                }
                in_process(true)
            }
            (Choice::Keep { end_on_quit }, Some(socket)) => {
                let connected = launch::connect(&LINK, socket).or_else(|err| match err {
                    ConnectError::Incompatible(_) | ConnectError::NotStandalone => Err(err),
                    // 开关关着，只是来接上次留下的会话：不为它另拉起宿主。
                    err if end_on_quit => Err(err),
                    // 宿主在跑却连不上（比如卡住了，或者刚好在退出）：照开关开着时的办法，连不上就拉起
                    // 新的；它还拿着锁时新拉起的抢不到、自己退出，等它放开后再拉。
                    err => {
                        tracing::warn!("failed to connect to the running host, starting one: {err}");
                        launch_host(socket)
                    }
                });
                match connected {
                    Ok(()) => Mode::Standalone { end_on_quit },
                    Err(err) => fall_back(err),
                }
            }
            (Choice::Launch, Some(socket)) => match launch_host(socket) {
                Ok(()) => Mode::Standalone { end_on_quit: false },
                Err(err) => fall_back(err),
            },
            (Choice::HandOver { end_on_quit }, Some(socket)) => {
                let sessions = match &probe {
                    Probe::Running { sessions, .. } => Some(*sessions),
                    _ => None,
                };
                match hand_over(socket, end_on_quit, sessions) {
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
    *MODE.lock().unwrap_or_else(PoisonError::into_inner) = mode;
}

/// 让这个构建的新宿主接手 `socket` 上旧宿主的会话（见 `handoff::hand_over`），成了就连上它。
/// `sessions` 是旧宿主有几个会话，协议对不上时不知道。另一个新 app 正在交接时返回 `None`，由
/// 调用方过一会儿重新探。
///
/// 没交成时旧宿主连同会话照常跑着，这次跑在 app 里、不开 socket（锁在它手里），弹框问用户；只有
/// 开关开着、旧宿主又没有会话时，没什么可丢的，让它退出、拉起这个构建的。
fn hand_over(socket: &Path, end_on_quit: bool, sessions: Option<usize>) -> Option<Mode> {
    let started = Instant::now();
    let outcome = match std::env::current_exe() {
        Ok(exe) => handoff::hand_over(&exe, handoff::Timing::default()),
        Err(err) => handoff::Outcome::Failed(format!("cannot tell where this app is: {err}")),
    };
    tracing::info!("handoff: {outcome:?} after {:?}", started.elapsed());
    let failed = |reason: String| {
        if sessions == Some(0) && !end_on_quit {
            tracing::warn!("the handoff failed ({reason}); the old host has no sessions, so replacing it");
            if let Err(err) = launch::retire(socket, &build()) {
                tracing::warn!("failed to stop the old host: {err}");
            }
            return match launch_host(socket) {
                Ok(()) => Mode::Standalone { end_on_quit: false },
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
                    Mode::Standalone { end_on_quit }
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
        handoff::Outcome::PreHandoff => {
            tracing::warn!("the old host cannot hand over, running the host in the app without a socket");
            notify(Notice::PreHandoff);
            in_process(false)
        }
        handoff::Outcome::Refused(reason) => failed(format!("the old host refused to hand over: {reason:?}")),
        handoff::Outcome::Failed(reason) => failed(reason),
    };
    Some(mode)
}

/// 用户在弹框里选了结束旧宿主（连同它的会话）：在后台线程里结束它（`terminate` 时不管它说
/// 什么协议，直接发 SIGTERM，见 `launch::terminate`；否则见 `launch::end_old_host`），之后
/// 这个 app 里的宿主开 socket，让命令行连得上。对面不是单独的宿主进程（宿主跑在旧版本的 app
/// 里）时不结束它，改请用户先退出旧版本（`Notice::OldAppRunning`）。
///
/// 动手前再探一次：弹框之后 socket 上可能已经换成这个构建的宿主（交接比这边等的久、最后成了，
/// 或者另一个同版本的 app 让新宿主接手了），结束它就把刚接过去的会话全结束了。这时不结束，告诉
/// 用户（`Notice::AlreadyUpgraded`）。不在运行中把这个 app 改连过去：窗口里的终端都在 app 自己
/// 的宿主里，换了连接它们就断了。
fn end_old_host(terminate: bool, cx: &mut gpui::AsyncApp) {
    let (tx, rx) = futures::channel::oneshot::channel();
    let spawned = thread::Builder::new().name("end-old-host".into()).spawn(move || {
        let Some(socket) = socket_path() else { return };
        if let Probe::Running { build: theirs, .. } = launch::probe(&socket, &build())
            && theirs == build()
        {
            tracing::warn!("the host on the socket is already this build, so not ending it");
            let _ = tx.send(Notice::AlreadyUpgraded);
            return;
        }
        let ended = if terminate { launch::terminate(&socket) } else { launch::end_old_host(&socket, &build()) };
        match ended {
            Ok(launch::Ended::Ended) => {
                tracing::info!("ended the old host and its sessions");
                if let Some(host) = IN_PROCESS.get() {
                    listen_in_app(host);
                }
            }
            Ok(launch::Ended::NotAHost) => {
                tracing::warn!("the old host runs inside an older runode app, asking to quit it instead");
                let _ = tx.send(Notice::OldAppRunning);
            }
            Err(err) => tracing::warn!("failed to end the old host: {err}"),
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

/// 宿主跑在 app 里：建好它（已经建过就用原来的），`listen` 时先开 socket（之后启动的 shell 才知道
/// 命令行该连哪里），再经一对 socket 连上。
fn in_process(listen: bool) -> Mode {
    let mut created = false;
    let host = IN_PROCESS.get_or_init(|| {
        created = true;
        let host = Host::new(build());
        // 之后启动的 shell 里有 `runode_cli::ENV_BIN`，指向这个可执行文件，没把 runode 放进 PATH 也能用命令行。
        if let Ok(exe) = std::env::current_exe() {
            host.set_env(runode_cli::ENV_BIN, exe);
        }
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
        let result = dirs.create_runtime_dir().map_err(anyhow::Error::from).and_then(|_| {
            let socket = dirs.host_socket_file().ok_or_else(|| anyhow!("the socket path is too long"))?;
            let lock = dirs.host_lock_file().ok_or_else(|| anyhow!("no place for the host lock"))?;
            host.listen(&socket, &lock)?;
            tracing::info!("host listening on {}", socket.display());
            Ok(())
        });
        match result {
            Ok(()) => return,
            Err(err) if Instant::now() >= deadline => {
                tracing::warn!("the host is not listening for other processes: {err:#}");
                return;
            }
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// 有要告诉用户的宿主的事（见 `Notice`）时，在最前面的窗口上弹框说一声；交接没成时弹框问用户
/// 留着旧宿主（默认）还是结束它。回滚历史没带过来这种不用做决定的事发系统通知，不弹框。
pub fn show_notice(cx: &mut App) {
    let Some(notice) = take_notice() else { return };
    // 选了「结束旧会话」时怎么结束：`Some(true)` 直接发 SIGTERM，`Some(false)` 先试着让它自己退出。
    let (title, detail, end) = match notice {
        Notice::Incompatible(reason) => {
            (rust_i18n::t!("host.incompatible_title"), rust_i18n::t!("host.incompatible_detail", reason = reason), None)
        }
        Notice::OtherApp => (rust_i18n::t!("host.other_app_title"), rust_i18n::t!("host.other_app_detail"), None),
        Notice::Unreachable(reason) => {
            (rust_i18n::t!("host.unreachable_title"), rust_i18n::t!("host.unreachable_detail", reason = reason), None)
        }
        Notice::OldAppRunning => (rust_i18n::t!("host.old_app_title"), rust_i18n::t!("host.old_app_detail"), None),
        Notice::AlreadyUpgraded => {
            (rust_i18n::t!("host.already_upgraded_title"), rust_i18n::t!("host.already_upgraded_detail"), None)
        }
        Notice::PreHandoff => {
            (rust_i18n::t!("host.pre_handoff_title"), rust_i18n::t!("host.pre_handoff_detail"), Some(true))
        }
        Notice::HandoffFailed { reason, sessions } => {
            let detail = match sessions {
                Some(count) => rust_i18n::t!("host.handoff_failed_detail", count = count, reason = reason),
                None => rust_i18n::t!("host.handoff_failed_detail_uncounted", reason = reason),
            };
            (rust_i18n::t!("host.handoff_failed_title"), detail, Some(false))
        }
        Notice::HandoffDegraded { count } => {
            report_degraded(count, cx);
            return;
        }
    };
    let Some(window) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else {
        return;
    };
    let ok = rust_i18n::t!("host.ok");
    let (keep, end_old) = (rust_i18n::t!("host.keep_old"), rust_i18n::t!("host.end_old"));
    // 默认（第一个）按钮留着旧宿主。
    let answers: Vec<&str> = if end.is_some() { vec![&keep, &end_old] } else { vec![&ok] };
    let answer =
        window.update(cx, |_, window, cx| window.prompt(PromptLevel::Warning, &title, Some(&detail), &answers, cx));
    if let Ok(answer) = answer {
        cx.spawn(async move |cx| {
            if let (Some(terminate), Ok(1)) = (end, answer.await) {
                end_old_host(terminate, cx);
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
