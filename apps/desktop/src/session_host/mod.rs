//! 桌面和终端宿主之间：宿主管着每个会话的 PTY 和权威的那份 VT（见 `runode_host`），桌面经一条
//! 连接（`Link`）按 `runode_protocol` 和它说话。宿主跑在 app 进程里（`Host::connect_pair` 的一对
//! socket），或者单独一个进程（`runode --host`，app 退出后会话还在），由配置项 `terminal-host`
//! 在启动时定下，见 `launch::choose_mode`。
//!
//! 启动时 `start` 在后台线程里读配置、定模式、连上宿主，主线程第一次用 `link` 时等它连好。
//! 主题和要不要记命令历史跟着配置走，见 `configure`；别的进程经宿主请界面办的事见 `serve_ui`；
//! 连接断了以后用 `reconnect` 重新连上。

mod launch;
mod link;

use std::{
    path::PathBuf,
    sync::{Condvar, LazyLock, Mutex, OnceLock, PoisonError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{App, PromptLevel};
use runode_config::Config;
use runode_host::{BuildId, ClientMsg, Host};
use runode_protocol::SessionInfo;

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
/// 要在界面上告诉用户的事（比如旧版本的宿主还活着），见 `take_notice`。
static NOTICE: Mutex<Option<Notice>> = Mutex::new(None);

/// 要在界面上告诉用户的宿主的事。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// socket 上的宿主协议对不上（多半是旧版本的还活着），这次跑在 app 里，命令行连不上 app。
    Incompatible(String),
}

/// 启动时在后台线程 `host-connect` 里：读配置、定宿主怎么跑、连上它，再做 `then`（比如提前拉起
/// 第一个 shell）。主线程第一次调 `link` 时等它连好。
pub fn start(then: impl FnOnce(&Config) + Send + 'static) {
    let spawned = thread::Builder::new().name("host-connect".into()).spawn(move || {
        let ready = MarkReady;
        // 配置有问题时由主线程加载配置时报告，这里不重复。
        let config =
            tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || Config::load(true));
        establish(config.terminal_host);
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
    let probe = socket.as_deref().map_or(Probe::Absent, |socket| launch::probe(socket, &build));
    let choice = launch::choose_mode(terminal_host, &probe);
    tracing::info!("host: {probe:?} with terminal-host = {terminal_host}, so {choice:?}");
    if let Probe::Incompatible(reason) = &probe {
        tracing::warn!("an incompatible host is running, so the host runs in the app without a socket: {reason}");
        *NOTICE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Notice::Incompatible(reason.clone()));
    }
    let mode = match (choice, socket) {
        (Choice::InProcess { listen }, _) => in_process(listen),
        (Choice::Retire, Some(socket)) => {
            if let Err(err) = launch::retire(&socket, &build) {
                tracing::warn!("failed to stop the leftover host: {err}");
            }
            in_process(true)
        }
        (Choice::Keep { end_on_quit }, Some(socket)) => match launch::connect(&LINK, &socket) {
            Ok(()) => Mode::Standalone { end_on_quit },
            Err(err) => {
                tracing::warn!("failed to connect to the running host, running it in the app: {err}");
                in_process(true)
            }
        },
        (Choice::Launch, Some(socket)) => {
            let launched = std::env::current_exe()
                .map_err(ConnectError::Io)
                .and_then(|exe| launch::connect_or_launch(&LINK, &socket, &exe));
            match launched {
                Ok(()) => Mode::Standalone { end_on_quit: false },
                Err(ConnectError::Incompatible(reason)) => {
                    tracing::warn!("an incompatible host took the socket, running the host in the app: {reason}");
                    *NOTICE.lock().unwrap_or_else(PoisonError::into_inner) = Some(Notice::Incompatible(reason));
                    in_process(false)
                }
                Err(err) => {
                    tracing::warn!("failed to start the host process, running it in the app: {err}");
                    in_process(true)
                }
            }
        }
        (_, None) => in_process(false),
    };
    *MODE.lock().unwrap_or_else(PoisonError::into_inner) = mode;
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

/// 有要告诉用户的宿主的事（见 `Notice`）时，在最前面的窗口上弹框说一声。
pub fn show_notice(cx: &mut App) {
    let Some(Notice::Incompatible(reason)) = take_notice() else { return };
    let Some(window) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else {
        return;
    };
    let title = rust_i18n::t!("host.incompatible_title");
    let detail = rust_i18n::t!("host.incompatible_detail", reason = reason);
    let answer = window.update(cx, |_, window, cx| {
        window.prompt(PromptLevel::Warning, &title, Some(&detail), &[&*rust_i18n::t!("host.ok")], cx)
    });
    if let Ok(answer) = answer {
        cx.spawn(async move |_| {
            let _ = answer.await;
        })
        .detach();
    }
}
