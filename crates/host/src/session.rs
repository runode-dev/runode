//! 一个会话的线程：`HostSession` 在这个线程里建好，从不离开它（它不是 `Send`）。线程之间只传
//! `Send` 的数据：PTY 的读线程交来的输出、前端的请求，以及发给前端的 `Event`。
//!
//! 收件箱是一个 `std::sync::mpsc` 的 channel，PTY 输出和前端的请求都排在里面，按到达的先后
//! 处理。PTY 输出另外限了量：读线程最多积压 `PTY_BACKLOG_BYTES` 字节，再多就等会话线程处理掉
//! 一些，程序输出得比宿主的 VT 处理得快时，被拖慢的是程序，不是内存。前端的请求不限量，发的
//! 一方从不等。没有事的时候按 agent 识别和前台进程轮询要的时刻醒来。
//!
//! 宿主升级时会话交给新宿主（见 `handoff`，只在 Unix 上）：旧宿主这边 `Inbox::Prepare` 让会话停下来交出状态、
//! 冻结，`Inbox::Release` 提交（交出 PTY、线程结束）、`Inbox::Resume` 回滚；新宿主这边 `adopt`
//! 接手，PTY 停在闸门上，`Inbox::Open` 提交后打开闸门，`Inbox::Release` 不接手了原样交回去。
//!
//! 程序读写剪贴板（OSC 52）的请求按配置办，要桌面办的交给最近和这个会话交互过的桌面，见 `clipboard`。

use std::{
    any::Any,
    ffi::OsString,
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use runode_protocol::{AttachMode, HostMsg, SessionId, SessionInfo};

use runode_terminal::{
    host_session::{HostSession, ReportRedactor},
    pty::{self, Pty, PtyEvent},
};
use runode_types::{
    agent::AgentState,
    clipboard::ClipboardAccess,
    grid::GridSize,
    input::KeyChord,
    session::{DriveAction, SessionMeta},
    settings::TermSettings,
    shell::IntegrationMode,
};

use crate::{SpawnOptions, server::UiPort};

mod clipboard;
#[cfg(unix)]
mod handoff;
mod sizing;

pub(crate) use clipboard::CLIPBOARD_READ_PATIENCE;
#[cfg(unix)]
pub(crate) use handoff::{Adopted, Adopting, Exported, Prepared, Unwritten, adopt};
use sizing::Viewer;

/// PTY 读线程最多积压这么多字节的输出，再多就等会话线程处理。按字节而不按块数算：程序不停输出
/// 时一次只读到 1 KiB 左右，按块数限的话积压不了多少，会话线程稍一耽搁读线程就得停下。也不能
/// 太大：前端的输入和改尺寸跟输出排在同一个收件箱里，刷屏时 Ctrl-C 要等前面积压的输出都处理完
/// 才写给程序。积压的输出按 `COALESCE_BYTES` 合成大块处理，这个量够会话线程短暂耽搁时读线程
/// 不停下。
const PTY_BACKLOG_BYTES: usize = 1024 * 1024;
/// 会话线程一次最多把这么多字节相邻的 PTY 输出合成一块处理，见 `coalesce`。处理一块时不看
/// 轮询的时刻和 `Handle::killed`，所以块的大小也是它们最多被耽搁多久：发布构建里 VT 喂一整块
/// 很快；开发构建的 libghostty 是 Zig 的 Debug 构建，喂得极慢，前台进程和结束会话都跟着拖住，
/// 所以开发构建合得小。
const COALESCE_BYTES: usize = if cfg!(debug_assertions) { 4 * 1024 } else { 64 * 1024 };
/// 重新读取前台进程的间隔：不输出的程序（比如 `sleep`）启动后，标签名也能跟上。
const FOREGROUND_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// `Inbox::TypeAtPrompt` 最多等提示符这么久；没有 shell 集成的 shell 不报提示符，到时就写。
const TYPE_AT_PROMPT_PATIENCE: Duration = Duration::from_secs(5);
/// 注入了 shell 集成的 shell 迟早会报提示符，多等一会儿：配置跑得慢（比如启动时问要不要更新）时，
/// 早写出去的命令会被配置里的程序读走。不一直等，免得集成没生效（配置里 exec 成了别的 shell）时
/// 命令永远打不出去。
const TYPE_AT_INTEGRATED_PROMPT_PATIENCE: Duration = Duration::from_secs(60);
/// 有输出时重读前台进程的最短间隔：大量输出时不必每块都做几次系统调用，推迟的那次到点补上。
const FOREGROUND_REFRESH_INTERVAL: Duration = Duration::from_millis(50);

/// 会话发给一个连着的前端的一件事，同一个会话的按发生的先后。
#[derive(Clone, Debug)]
pub(crate) enum Event {
    /// PTY 的输出，或者宿主为清屏插进输出流的字节。
    Output(Arc<[u8]>),
    /// 控制消息。装在盒子里：输出最常见，每件事挪动时不必带着最大那种消息的大小。
    Msg(Box<HostMsg>),
}

impl Event {
    pub(crate) fn msg(message: HostMsg) -> Self {
        Self::Msg(Box::new(message))
    }
}

/// 收一个会话的 `Event` 的一方，在会话的线程里调用，不能阻塞。返回 false 表示不再要了，之后
/// 不再调用。
pub(crate) type EventSink = Box<dyn FnMut(Event) -> bool + Send>;

/// 会话线程收到的消息。
pub(crate) enum Inbox {
    Pty(PtyEvent),
    Detach {
        connection: u64,
    },
    Start {
        integration: IntegrationMode,
    },
    /// 连接 `connection` 发来的输入帧，也算它的一次交互，见 `Runner::interact`。
    Input {
        connection: u64,
        data: Vec<u8>,
    },
    /// 连接 `connection` 的视图尺寸变了，见 `Runner::request_size`。
    Resize {
        connection: u64,
        size: GridSize,
    },
    /// 连接 `connection` 上这个会话获得了焦点（`ClientMsg::Focus { focused: true }`），算一次交互。
    Focus {
        connection: u64,
    },
    ClearScreen,
    Theme(Arc<TermSettings>),
    /// 连接上的前端连上来，见 `Subscribe`。
    Subscribe(Subscribe),
    /// 要这个会话在 `SessionList` 里的一项。
    Info(mpsc::Sender<SessionInfo>),
    /// 读屏幕底部的文字（见 `HostSession::screen_text`），`command` 给了时改读倒数第几条命令的
    /// 输出（见 `HostSession::command_output`）。回的是文字和开头是否已经被挤出回滚历史。
    ReadScreen {
        lines: Option<u32>,
        command: Option<u32>,
        reply: mpsc::Sender<Result<(String, bool)>>,
    },
    /// 按宿主 VT 当前的模式编好这些键写给程序，见 `HostSession::encode_keys`。
    Keys(Vec<KeyChord>),
    /// 按宿主 VT 当前的模式把这段文字当粘贴写给程序，见 `HostSession::encode_paste`。
    Paste(String),
    /// 等 shell 出了提示符（shell 集成报告了提示符所在的目录）再把这些字节写给程序，最多等
    /// `TYPE_AT_PROMPT_PATIENCE`（注入了集成的等 `TYPE_AT_INTEGRATED_PROMPT_PATIENCE`）。看板在新开的会话里打 agent 的命令用：shell 还在读配置时打进去的字
    /// 可能被配置里的程序读走。
    TypeAtPrompt(Vec<u8>),
    /// 谁最近在操作这个会话，见 `SessionMeta::driver`：`Some` 是别的终端里的程序经 socket 做了
    /// 一件事，排在那件事前面；`None` 是用户在界面里打了字，清掉记录。
    Driven(Option<Drive>),
    Kill,
    /// 交接（旧宿主）：停下读 PTY、处理完已经读出来的输出，交出会话的状态，之后冻结，见
    /// `Runner::hand_over`。
    #[cfg(unix)]
    Prepare(mpsc::Sender<Prepared>),
    /// 交出 PTY（不结束 shell），回交出时还没写进 PTY 的输入，线程随即结束：旧宿主在交接提交时
    /// 发，新宿主不接手了时发（PTY 停在闸门上，原样交回）。
    #[cfg(unix)]
    Release(mpsc::Sender<Unwritten>),
    /// 交接回滚（旧宿主）：接着读 PTY，按先后重放冻结期间存下的请求。
    #[cfg(unix)]
    Resume,
    /// 交接提交了（新宿主）：先把旧宿主没写进 PTY 的这些输入排进写队列，再打开读写线程的闸门；
    /// 还在等提示符的从这时起重新等，同 `TypeAtPrompt`。
    #[cfg(unix)]
    Open(Unwritten),
    /// 读写剪贴板的规矩改了，见 `ClientMsg::SetOptions`。
    Clipboard(ClipboardAccess),
    /// 界面回了会话经 `UiPort::ask` 请它办的事，`ui` 是那条请求的编号；界面没回就断开了时是
    /// `HostMsg::Error`。
    UiAnswer {
        ui: u64,
        reply: Box<HostMsg>,
    },
}

/// 开会话和接手会话共用的设置。
pub(crate) struct Setup {
    pub(crate) id: SessionId,
    /// VT 一开始套的主题。
    pub(crate) settings: TermSettings,
    /// 启动 shell 时另外设的全部环境变量（宿主的和 `runode_protocol::ENV_SESSION`）。
    pub(crate) env: Vec<(String, OsString)>,
    /// 请界面办事（读写剪贴板）的一头。
    pub(crate) ui: UiPort,
    /// 一开始的读写剪贴板的规矩，之后改了经 `Inbox::Clipboard` 送来。
    pub(crate) clipboard: ClipboardAccess,
    /// 读剪贴板最多等界面多久，见 `Host::set_clipboard_read_patience`。
    pub(crate) read_patience: Duration,
    /// 会话线程拿着它，线程结束（`Pty` 丢掉、shell 杀掉）时才放开，见 `Threads`。
    pub(crate) alive: Alive,
}

/// 还没结束的会话线程（连正在开的）有几个。shell 是会话线程丢掉 `Pty` 时杀的，`Inbox::Kill` 只是叫它
/// 结束；宿主要退出时等它们都结束再走（`Host::run_until_idle`），不然进程先退出，忽略 SIGHUP 的 shell
/// 只收到内核那一声 SIGHUP，活了下来。
#[derive(Default)]
pub(crate) struct Threads {
    live: Mutex<usize>,
    ended: Condvar,
}

impl Threads {
    /// 记上一个会话线程，丢掉返回的 `Alive` 时算它结束。
    pub(crate) fn enter(self: &Arc<Self>) -> Alive {
        *self.live.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        Alive(self.clone())
    }

    /// 等所有会话线程结束，最多等到 `deadline`，返回还没结束的个数。
    pub(crate) fn wait_until(&self, deadline: Instant) -> usize {
        let mut live = self.live.lock().unwrap_or_else(PoisonError::into_inner);
        while *live > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            live = self.ended.wait_timeout(live, left).map_or_else(|err| err.into_inner().0, |(live, _)| live);
        }
        *live
    }

    /// 测试用：现在记着几个。
    #[cfg(all(test, unix))]
    pub(crate) fn live(&self) -> usize {
        *self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `Threads` 里记着的一个会话线程，丢掉时算它结束。会话线程里第一个放进局部变量，最后一个丢掉，
/// 排在 `Runner`（和它的 `Pty`）后面。
pub(crate) struct Alive(Arc<Threads>);

impl Drop for Alive {
    fn drop(&mut self) {
        *self.0.live.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        self.0.ended.notify_all();
    }
}

/// 别的终端里的程序对会话做了什么，见 `Inbox::Driven`。
pub(crate) struct Drive {
    /// 发消息的程序所在的会话，见 `ClientMsg::Hello::session`。
    pub(crate) by: Option<SessionId>,
    pub(crate) action: DriveAction,
}

/// 连接上的前端连上一个会话：当场给一份现在的屏幕（快照或 VT 重放），之后的事件接着它；一个
/// 会话能这样连上任意多次。
pub(crate) struct Subscribe {
    /// 连上来的连接；它已经连着这个会话时，新的订阅换掉旧的。
    pub(crate) connection: u64,
    /// 前端视图的尺寸：要屏幕时算一次交互，先按尺寸归属改会话的尺寸，屏幕按改好的尺寸给；只看
    /// 状态时只在还没有 owner 时改。见 `Runner::subscribe`。
    pub(crate) size: Option<GridSize>,
    /// 要什么样的屏幕；`Snapshot` 编不出来时退成 `VtReplay`。
    pub(crate) mode: AttachMode,
    /// 在会话线程里调用，交给它当前的屏幕，返回之后收事件的 `EventSink`；为 `None` 时不连了。
    /// 它和之后的事件在同一个线程里按先后发生，前端收到的屏幕和输出之间不会漏也不会重。
    pub(crate) start: Box<dyn FnOnce(Screen) -> Option<EventSink> + Send>,
    /// 是桌面的界面连上来，`SessionInfo::claimed` 据此算。
    pub(crate) desktop: bool,
    /// 连接在 `Hello` 里报的设备名，见 `HostMsg::SizeOwner::owner`。
    pub(crate) device: Option<String>,
}

/// `Subscribe` 时会话当前的样子。
pub(crate) struct Screen {
    pub(crate) size: GridSize,
    pub(crate) meta: SessionMeta,
    /// 宿主那份 VT 现在套着的主题。
    pub(crate) settings: TermSettings,
    /// 实际给的屏幕，见 `HostMsg::Attached::mode`。
    pub(crate) mode: AttachMode,
    /// 快照或 VT 重放的字节；`MetaOnly` 时为空。
    pub(crate) data: Vec<u8>,
}

/// 往会话线程发消息的一端。
#[derive(Clone)]
pub(crate) struct Handle {
    inbox: mpsc::Sender<Inbox>,
    /// 前端要结束会话。`Inbox::Kill` 排在积压的输出后面，会话线程处理每条消息前先看这个，
    /// 不用等积压的输出（最多 `PTY_BACKLOG_BYTES`）都喂完。
    killed: Arc<AtomicBool>,
    /// 发过 `Inbox::Driven(Some(..))`、之后还没清过：桌面每次打字都要清一下，没被标过时
    /// `Inbox::Driven(None)` 不必进收件箱。改它和把 `Inbox::Driven` 排进收件箱在同一把锁里：几份克隆在
    /// 不同线程里一个标、一个清时，它和收件箱里最后一条 `Driven` 说的一样，不会记着标过却当成没标。
    driven: Arc<Mutex<bool>>,
    /// 会话线程写的 `Runner::desktop`。
    desktop: Arc<AtomicU64>,
}

impl Handle {
    /// 发一条消息，会话线程已经结束时返回 false。没被标过的会话收到 `Inbox::Driven(None)` 时
    /// 什么都不发，返回 true。
    pub(crate) fn send(&self, message: Inbox) -> bool {
        match &message {
            Inbox::Kill => self.killed.store(true, Ordering::Release),
            Inbox::Driven(drive) => {
                let mut driven = self.driven.lock().unwrap_or_else(PoisonError::into_inner);
                if drive.is_none() && !*driven {
                    return true;
                }
                *driven = drive.is_some();
                return self.inbox.send(message).is_ok();
            }
            _ => {}
        }
        self.inbox.send(message).is_ok()
    }

    /// 这个会话要界面办的事交给哪条桌面的连接（`Runner::ui_connection`）；没有桌面连着它时为 `None`。
    /// 会话线程处理完每条消息后才更新，刚断开的连接可能还在这里。
    pub(crate) fn desktop(&self) -> Option<u64> {
        Some(self.desktop.load(Ordering::Relaxed)).filter(|&connection| connection != 0)
    }

    /// 这个登记项是不是拿着 `killed` 的那个会话线程的。
    pub(crate) fn owns(&self, killed: &Arc<AtomicBool>) -> bool {
        Arc::ptr_eq(&self.killed, killed)
    }

    /// 测试用：发到 `inbox` 的一端，没有会话线程，由测试自己收、自己回话。
    #[cfg(all(test, unix))]
    pub(crate) fn detached(inbox: mpsc::Sender<Inbox>) -> Self {
        Self { inbox, killed: Arc::default(), driven: Arc::default(), desktop: Arc::default() }
    }
}

/// 开会话：在调用的线程里打开伪终端（`start` 时连 shell 一起启动），错误当场返回；再起会话线程，
/// 等它把 `HostSession` 建好。
pub(crate) fn spawn(setup: Setup, options: SpawnOptions) -> Result<Handle> {
    let Setup { id, settings, env, ui, clipboard, read_patience, alive } = setup;
    let (inbox, rx) = mpsc::channel();
    let credits = Arc::new(Credits::default());
    let sink = pty_sink(&inbox, &credits);
    let mut pty = Pty::open(options.size, sink)?;
    for (key, value) in env {
        pty.set_env(key, value);
    }
    pty.set_shell_features(&settings);
    if options.start {
        pty.start(options.shell.as_deref(), options.cwd.as_deref(), options.integration)?;
    }
    let (ready, created) = mpsc::channel();
    let killed = Arc::new(AtomicBool::new(false));
    let killed_flag = killed.clone();
    let desktop = Arc::new(AtomicU64::new(0));
    let runner_desktop = desktop.clone();
    thread::Builder::new()
        .name(format!("session-{id}"))
        .spawn(move || {
            // 最后丢掉，排在 `Pty` 后面，见 `Alive`。
            let _alive = alive;
            pty::set_current_thread_interactive();
            // 线程结束时放开读线程，免得它一直等着积压的块被处理。
            let _close = CloseOnDrop(credits.clone());
            let session = match HostSession::new(options.size, pty, options.cwd.as_deref(), &settings) {
                Ok(session) => session,
                Err(err) => {
                    let _ = ready.send(Err(err));
                    return;
                }
            };
            let _ = ready.send(Ok(()));
            let mut runner = Runner::new(id, session, options.shell, settings, credits);
            runner.ui = ui;
            runner.desktop = runner_desktop;
            runner.set_clipboard(clipboard);
            runner.read_patience = read_patience;
            runner.run(&rx, &killed_flag);
        })
        .context("failed to start the session thread")?;
    created.recv().map_err(|_| anyhow!("the session thread ended while starting"))??;
    Ok(Handle { inbox, killed, driven: Arc::default(), desktop })
}

/// 读线程交 PTY 输出的 `PtySink`：先记上字节数（积压太多就等），再排进收件箱。
fn pty_sink(inbox: &mpsc::Sender<Inbox>, credits: &Arc<Credits>) -> pty::PtySink {
    let inbox = inbox.clone();
    let credits = credits.clone();
    Box::new(move |event| credits.acquire(output_len(&event)) && inbox.send(Inbox::Pty(event)).is_ok())
}

/// 会话线程里的状态。
struct Runner {
    id: SessionId,
    session: HostSession,
    subscribers: Vec<Subscriber>,
    /// 带着屏幕连着的连接，有资格决定尺寸，见 `Viewer`。
    viewers: Vec<Viewer>,
    /// 决定会话尺寸的连接（`viewers` 里的一个）：最近交互过的那个。还没有谁交互过、或者没有
    /// `viewers` 时为空，这时谁的 `Inbox::Resize` 都照旧应用，只有一个前端时和没有尺寸归属一样。
    owner: Option<u64>,
    /// 交互的计数，给 `Viewer::last_active` 编号。
    interactions: u64,
    /// 给前端的输出抹掉 shell 集成报告的内容，从会话开出来起每块输出都经过它。
    redactor: ReportRedactor,
    /// 宿主那份 VT 现在套着的主题：最近一次 `Inbox::Theme` 的，还没有过时是开出来时的。
    settings: Arc<TermSettings>,
    /// `Inbox::Start` 时启动的程序，见 `SpawnOptions::shell`。
    shell: Option<String>,
    credits: Arc<Credits>,
    /// 前端要清屏，等 VT 回到 ground 再清。
    clear_pending: bool,
    exited: bool,
    /// 下次按 `FOREGROUND_POLL_INTERVAL` 重读前台进程的时刻；shell 还没启动时没有前台进程可读，
    /// 为 `None`，不为它醒来，启动后再排上。
    next_poll: Option<Instant>,
    /// 上次因为有输出而重读前台进程的时刻，以及推迟到的那次。
    foreground_read_at: Instant,
    foreground_due: Option<Instant>,
    /// 请界面办事的一头，见 `Setup::ui`。
    ui: UiPort,
    /// `ui_connection` 的结果，0 表示没有；每处理完一条消息写一次，别的线程经 `Handle::desktop` 读。
    desktop: Arc<AtomicU64>,
    /// 读写剪贴板的规矩，见 `set_clipboard`。
    clipboard: ClipboardAccess,
    /// 读剪贴板最多等界面多久，见 `Setup::read_patience`。
    read_patience: Duration,
    /// 在等界面回的那个读剪贴板的请求，见 `clipboard`。
    clipboard_read: Option<clipboard::PendingRead>,
    /// 上次公布的 agent 状态和前台是不是 shell，见 `publish_meta`。
    board_state: Option<(Option<AgentState>, bool)>,
    /// 等着提示符出来再写的字节和收到请求的时刻，见 `Inbox::TypeAtPrompt`、`type_ahead_by`。
    type_ahead: Option<(Vec<u8>, Instant)>,
}

impl Runner {
    fn new(
        id: SessionId,
        session: HostSession,
        shell: Option<String>,
        settings: TermSettings,
        credits: Arc<Credits>,
    ) -> Self {
        let now = Instant::now();
        let mut runner = Self {
            id,
            session,
            subscribers: Vec::new(),
            viewers: Vec::new(),
            owner: None,
            interactions: 0,
            redactor: ReportRedactor::new(),
            settings: Arc::new(settings),
            shell,
            credits,
            clear_pending: false,
            exited: false,
            next_poll: None,
            foreground_read_at: now,
            foreground_due: None,
            ui: UiPort::default(),
            desktop: Arc::default(),
            clipboard: ClipboardAccess::default(),
            read_patience: CLIPBOARD_READ_PATIENCE,
            clipboard_read: None,
            board_state: None,
            type_ahead: None,
        };
        // shell 已经在起始目录里跑起来了，不等第一次输出，前端一连上就有名字。
        runner.refresh_foreground(now);
        runner
    }

    fn run(mut self, inbox: &mpsc::Receiver<Inbox>, killed: &Arc<AtomicBool>) {
        // 合并输出时多取出来的那条消息，下一轮先办它，见 `coalesce`。
        let mut stash = None;
        loop {
            let message = match stash.take() {
                Some(message) => Some(message),
                None => match self.wait(inbox) {
                    Some(message) => message,
                    None => return,
                },
            };
            if killed.load(Ordering::Acquire) {
                self.ended();
                self.answer_pending(message, inbox);
                return;
            }
            let message = match message {
                Some(Inbox::Pty(PtyEvent::Output(data))) => {
                    Some(Inbox::Pty(PtyEvent::Output(coalesce(data, inbox, &mut stash))))
                }
                message => message,
            };
            // 处理一条消息时 panic 的话，`HostSession` 可能停在半路，不能再往下处理；告诉前端会话
            // 没了，免得视图一直停在最后一屏不动。
            let handled = panic::catch_unwind(AssertUnwindSafe(|| match message {
                #[cfg(unix)]
                Some(Inbox::Prepare(reply)) => self.hand_over(&reply, inbox, killed),
                message => self.step(message),
            }));
            match handled {
                Ok(true) => {}
                Ok(false) => return,
                Err(panic) => {
                    self.crashed(panic.as_ref());
                    // 线程没了，登记项留着的话宿主不再空闲退出，任务和定时也一直当它还在跑。
                    self.ui.forget(self.id, killed);
                    return;
                }
            }
        }
    }

    /// 处理一条消息（`None` 表示到了该醒的时刻），返回会话是否还要接着跑。
    fn step(&mut self, message: Option<Inbox>) -> bool {
        match message {
            Some(Inbox::Kill) => {
                self.ended();
                return false;
            }
            #[cfg(unix)]
            Some(Inbox::Release(reply)) => {
                let _ = reply.send(self.release(Vec::new()));
                return false;
            }
            Some(message) => self.handle(message),
            None => {}
        }
        // 输出一直不断时收件箱总有消息，`wait` 等不到超时；到点的轮询在这里补上，不然前台进程
        // 和 agent 状态要等输出停了才更新。
        if self.deadline().is_some_and(|at| Instant::now() >= at) {
            self.tick();
        }
        self.type_at_prompt();
        self.publish_meta();
        self.desktop.store(self.ui_connection().unwrap_or(0), Ordering::Relaxed);
        true
    }

    /// 这个会话要界面办的事交给哪条桌面的连接：连着这个会话的桌面里最近交互过的（尺寸归属里的交互，见
    /// `Runner::interact`）；都没交互过时取最近连上这个会话的；没有桌面连着这个会话时为 `None`。
    fn ui_connection(&self) -> Option<u64> {
        let last_active = |connection: u64| {
            self.viewers.iter().find(|viewer| viewer.connection == connection).map_or(0, |viewer| viewer.last_active)
        };
        self.subscribers
            .iter()
            .filter(|subscriber| subscriber.desktop)
            .max_by_key(|subscriber| last_active(subscriber.connection))
            .map(|subscriber| subscriber.connection)
    }

    /// 等着的 `Inbox::TypeAtPrompt` 到时候了就写出去：提示符出来了，或者等够了。
    fn type_at_prompt(&mut self) {
        let Some(by) = self.type_ahead_by() else { return };
        if Instant::now() < by && self.session.meta().prompt_cwd.is_none() {
            return;
        }
        if let Some((data, _)) = self.type_ahead.take() {
            self.session.write(data);
        }
    }

    /// 等着的 `Inbox::TypeAtPrompt` 最晚什么时候写出去；shell 还没启动时不写，等待从 `Inbox::Start` 起算。
    /// 注入集成的事可能在请求之后才发生，所以每次现查。
    fn type_ahead_by(&self) -> Option<Instant> {
        if !self.session.started() {
            return None;
        }
        let (_, since) = self.type_ahead.as_ref()?;
        let integrated = self.session.has_shell_integration();
        Some(*since + if integrated { TYPE_AT_INTEGRATED_PROMPT_PATIENCE } else { TYPE_AT_PROMPT_PATIENCE })
    }

    /// 会话结束了（前端要结束它，或者处理消息时 panic）：还没报告过退出的，告诉连着的前端
    /// （比如经 socket 等着 agent 的命令行）；叫结束的那一方多半已经不收了。报告过的也告诉宿主一声，
    /// 任务卡片上打开终端的按钮要收起。
    fn ended(&mut self) {
        if self.exited {
            self.ui.todo_session_changed(self.id, true);
        } else {
            self.report_exit();
        }
    }

    /// shell 退出了：告诉连着的前端，也告诉宿主，任务会话的卡片要换列。
    fn report_exit(&mut self) {
        self.exited = true;
        self.emit(Event::msg(HostMsg::Exited { id: self.id, status: None }));
        self.ui.todo_session_changed(self.id, true);
    }

    /// 被结束时还排在收件箱里的消息（`first` 是已经取出来的那些：`run` 里取出的那条，或者交接冻结期间存下的
    /// 请求）：积压的输出和别的请求都丢掉，
    /// 等着回话的一方随之收到断开；只有前端连上来（`Subscribe`）要回，不然它收不到
    /// `Attached`，干等到超时。照常给它现在的屏幕，`subscribe` 见会话已经结束，接着补发 `Exited`；
    /// 不按它的尺寸改会话。
    ///
    /// 宿主先把会话从登记表里拿掉、再置上 `Handle::killed` 的标记，`Subscribe` 又是经登记表送来
    /// 的，所以看到标记时，所有送得到的 `Subscribe` 都已经在收件箱里了。
    fn answer_pending(&mut self, first: impl IntoIterator<Item = Inbox>, inbox: &mpsc::Receiver<Inbox>) {
        for message in first.into_iter().chain(std::iter::from_fn(|| inbox.try_recv().ok())) {
            match message {
                Inbox::Subscribe(subscribe) => self.subscribe(Subscribe { size: None, ..subscribe }),
                Inbox::Kill => return,
                _ => {}
            }
        }
    }

    /// 处理消息时 panic 了：记日志，告诉前端出了错、会话没了。
    fn crashed(&mut self, panic: &(dyn Any + Send)) {
        let message = panic
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        tracing::error!("session {} panicked: {message}", self.id);
        let error = HostMsg::Error { req: None, id: Some(self.id), message: format!("the session crashed: {message}") };
        // 发给前端时又 panic 的（比如出事的正是前端的 `EventSink`），也只能到此为止。
        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
            self.emit(Event::msg(error));
            self.ended();
        }));
    }

    /// 等下一条消息，到了该醒的时刻还没有时为 `Some(None)`；收件箱断开了为 `None`。
    fn wait(&self, inbox: &mpsc::Receiver<Inbox>) -> Option<Option<Inbox>> {
        Some(match self.deadline() {
            Some(at) => match inbox.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(message) => Some(message),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            },
            None => Some(inbox.recv().ok()?),
        })
    }

    /// 下次没有消息也要醒来的时刻；shell 还没启动时和退出以后不再轮询。
    fn deadline(&self) -> Option<Instant> {
        if self.exited {
            return None;
        }
        let type_ahead = self.type_ahead_by();
        [self.next_poll, self.foreground_due, self.session.agent_deadline(), self.clipboard_deadline(), type_ahead]
            .into_iter()
            .flatten()
            .min()
    }

    fn handle(&mut self, message: Inbox) {
        match message {
            Inbox::Pty(PtyEvent::Output(data)) => {
                self.output(&data);
                self.credits.release(data.len());
            }
            Inbox::Pty(PtyEvent::Exited) => self.report_exit(),
            Inbox::Detach { connection } => {
                self.subscribers.retain(|s| s.connection != connection);
                self.forget_viewer(connection);
            }
            Inbox::Start { integration } => {
                if let Err(err) = self.session.start(self.shell.as_deref(), integration) {
                    tracing::error!("failed to start terminal session: {err:#}");
                    self.report_exit();
                    return;
                }
                let now = Instant::now();
                if let Some((_, since)) = &mut self.type_ahead {
                    *since = now;
                }
                self.refresh_foreground(now);
            }
            // 先轮到它决定尺寸（要改的话，`Resized` 排在这次输入的回显前面），再写给程序。
            Inbox::Input { connection, data } => {
                self.interact(connection);
                self.session.write(data);
            }
            Inbox::Resize { connection, size } => self.request_size(connection, size),
            Inbox::Focus { connection } => self.interact(connection),
            Inbox::ClearScreen => {
                self.clear_pending = true;
                self.try_clear();
            }
            Inbox::Theme(settings) => {
                self.settings = settings.clone();
                if self.session.apply_theme(&settings) {
                    let settings = (*settings).clone();
                    self.emit(Event::msg(HostMsg::ThemeApplied { id: self.id, settings }));
                }
            }
            Inbox::Subscribe(subscribe) => self.subscribe(subscribe),
            // 列会话的（推送每两秒轮询一次、桌面的后台会话列表、命令行、手机）都不读 shell 的
            // PATH 和函数名，函数名可能有几百个，不带；要它们的界面从 `Attached` 和 `Meta` 拿。
            Inbox::Info(reply) => {
                let _ = reply.send(SessionInfo {
                    id: self.id,
                    size: self.session.size(),
                    meta: SessionMeta { shell_path: None, shell_names: Arc::default(), ..self.session.meta() },
                    clients: u32::try_from(self.subscribers.len()).unwrap_or(u32::MAX),
                    claimed: self.subscribers.iter().any(|s| s.desktop),
                    exited: self.exited,
                    size_owner: self.owner.and_then(|owner| self.device_of(owner)),
                });
            }
            Inbox::ReadScreen { lines, command, reply } => {
                let text = match command {
                    Some(n) => self.session.command_output(n),
                    None => self.session.screen_text(lines).map(|text| (text, false)),
                };
                let _ = reply.send(text);
            }
            Inbox::Keys(keys) => match self.session.encode_keys(&keys) {
                Ok(bytes) if !bytes.is_empty() => self.session.write(bytes),
                Ok(_) => {}
                Err(err) => tracing::warn!("session {} cannot encode keys: {err:#}", self.id),
            },
            Inbox::Paste(text) => match self.session.encode_paste(&text) {
                Ok(bytes) if !bytes.is_empty() => self.session.write(bytes),
                Ok(_) => {}
                Err(err) => tracing::warn!("session {} cannot encode a paste: {err:#}", self.id),
            },
            Inbox::TypeAtPrompt(data) => self.type_ahead = Some((data, Instant::now())),
            Inbox::Driven(Some(Drive { by, action })) => {
                let at_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX));
                self.session.drive(by.map(|by| by.to_string()), action, at_ms);
            }
            Inbox::Driven(None) => self.session.clear_driver(),
            #[cfg(unix)]
            Inbox::Open(Unwritten { now, at_prompt }) => {
                if !now.is_empty() {
                    self.session.write(now);
                }
                if !at_prompt.is_empty() {
                    self.type_ahead = Some((at_prompt, Instant::now()));
                }
                if let Err(err) = self.session.resume_reading() {
                    tracing::error!("session {} cannot read its pty after the handoff: {err:#}", self.id);
                }
            }
            Inbox::Clipboard(clipboard) => self.set_clipboard(clipboard),
            Inbox::UiAnswer { ui, reply } => self.ui_answered(ui, *reply),
            // 交接以外的时候收到的：`Prepare` 在 `run` 里办，丢掉回话的一端，等的一方当会话没了；
            // `Release` 在 `step` 里办；没有冻结着时 `Resume` 没什么可做。
            Inbox::Kill => {}
            #[cfg(unix)]
            Inbox::Prepare(_) | Inbox::Release(_) | Inbox::Resume => {}
        }
    }

    /// 连接上的前端连上来：按尺寸归属改好会话的尺寸，给它当前的屏幕，之后的事件接着发给它。
    ///
    /// 这条连接已经连着时（收到 `Resync` 后重新连上、在只看状态和看屏幕之间切换），新的订阅换掉
    /// 旧的：旧订阅的事件都在新屏幕之前。要屏幕的连接成为 `Viewer`，原来就是的沿用它记着的尺寸和
    /// 交互先后（`Resync` 后重新连上不带尺寸，不算交互，不抢也不丢 owner）；带尺寸的算一次交互。
    /// 改成只看状态的不再有资格，是 owner 的话让给别人。带着屏幕的订阅在 `Attached` 之后补发一次
    /// 当前的 `SizeOwner`：前端在 `Attached` 之前收到的会话消息都丢掉了。
    fn subscribe(&mut self, Subscribe { connection, size, mode, start, desktop, device }: Subscribe) {
        self.subscribers.retain(|s| s.connection != connection);
        if mode == AttachMode::MetaOnly {
            self.forget_viewer(connection);
            if let Some(size) = size {
                self.request_size(connection, size);
            }
        } else {
            if !self.viewers.iter().any(|v| v.connection == connection) {
                self.viewers.push(Viewer { connection, device, requested: None, last_active: 0 });
            }
            if let Some(size) = size {
                self.request_size(connection, size);
                self.interact(connection);
            }
        }
        // 屏幕给的是之后收抹过的输出的前端：输出流正停在一条报告里时，宿主这份 VT 的续接里
        // 带着口令，见 `HostSession::redacted_snapshot`。
        let (mode, data) = match mode {
            AttachMode::MetaOnly => (mode, Vec::new()),
            AttachMode::Snapshot => match self.session.redacted_snapshot(&self.redactor) {
                Ok(data) => (mode, data),
                Err(err) => {
                    tracing::debug!("session {} falls back to a VT replay: {err}", self.id);
                    (AttachMode::VtReplay, self.replay())
                }
            },
            AttachMode::VtReplay => (mode, self.replay()),
        };
        let settings = (*self.settings).clone();
        let screen = Screen { size: self.session.size(), meta: self.session.meta(), settings, mode, data };
        let Some(mut sink) = start(screen) else { return };
        // shell 已经退出了：`Exited` 只在退出那一刻发过一次，晚连上的前端在这里补上，免得一直等。
        if self.exited && !sink(Event::msg(HostMsg::Exited { id: self.id, status: None })) {
            return;
        }
        if let Some(owner) = self.size_owner_for(connection)
            && !sink(Event::msg(owner))
        {
            return;
        }
        // 连接上的前端一律收抹过的输出，见 `ReportRedactor`：别的进程拿不到 shell 集成报告的
        // 口令。桌面经 `Host::connect_pair` 连上来时也一样：它那份 VT 不认这些报告（标题、目录、
        // shell 的名字都从 `Meta` 来），抹过的流喂出来的状态和宿主的一样。
        self.subscribers.push(Subscriber { connection, sink, desktop });
    }

    fn replay(&self) -> Vec<u8> {
        self.session.redacted_vt_replay(&self.redactor).unwrap_or_else(|err| {
            tracing::warn!("session {} cannot replay its screen: {err:#}", self.id);
            Vec::new()
        })
    }

    /// 一块 PTY 输出：先原样转给前端，再喂宿主的 VT，然后处理它带来的变化。这块输出里响了铃的，
    /// 紧跟着它发 `Bell`，要求发通知的发 `Notify`：界面那份 VT 降成只看状态（后台标签）时靠它们。
    /// 要求改窗口尺寸的同样紧跟着发 `ResizeWindow`：界面那份 VT 重放屏幕时会再认一遍，所以只认宿主这边的。
    fn output(&mut self, data: &Arc<[u8]>) {
        self.emit(Event::Output(data.clone()));
        self.session.feed(data);
        if self.session.take_bell() {
            self.emit(Event::msg(HostMsg::Bell { id: self.id }));
        }
        for (title, body) in self.session.take_notifications() {
            self.emit(Event::msg(HostMsg::Notify { id: self.id, title, body }));
        }
        if let Some((rows, columns)) = self.session.take_resize_window() {
            self.emit(Event::msg(HostMsg::ResizeWindow { id: self.id, rows, columns }));
        }
        for command in self.session.take_commands() {
            self.emit(Event::msg(HostMsg::CommandFinished { id: self.id, command }));
        }
        for request in self.session.take_clipboard() {
            self.clipboard(request);
        }
        self.try_clear();
        // 进出目录、启动或退出程序时通常都有输出，顺带重读前台进程：离上次读满了间隔就读，
        // 不满就定在满的时刻；已经定了、到点了也读。
        let now = Instant::now();
        let due = self.foreground_due.unwrap_or(self.foreground_read_at + FOREGROUND_REFRESH_INTERVAL);
        if now >= due {
            self.refresh_foreground(now);
        } else {
            self.foreground_due = Some(due);
        }
    }

    /// 前端要清屏、VT 又在 ground 时清屏，清屏写进 VT 的字节当成一段输出发给前端。
    fn try_clear(&mut self) {
        if !self.clear_pending || !self.session.at_ground() {
            return;
        }
        self.clear_pending = false;
        if let Some(bytes) = self.session.clear_screen() {
            self.emit(Event::Output(bytes.into()));
        }
    }

    /// 到了该醒的时刻（没有消息时，或者处理完一条消息时已经过了点）：轮询前台进程，判断 agent
    /// 状态。
    fn tick(&mut self) {
        let now = Instant::now();
        if self.foreground_due.is_some_and(|due| now >= due) || self.next_poll.is_some_and(|at| now >= at) {
            self.refresh_foreground(now);
        }
        if self.session.agent_deadline().is_some_and(|at| now >= at) {
            self.session.poll_agent();
        }
        self.clipboard_tick(now);
    }

    /// 重读前台进程，排好下一次轮询；shell 还没启动时不排，见 `next_poll`。
    fn refresh_foreground(&mut self, now: Instant) {
        self.session.refresh_foreground();
        self.foreground_read_at = now;
        self.foreground_due = None;
        self.next_poll = self.session.started().then_some(now + FOREGROUND_POLL_INTERVAL);
    }

    /// 对外公布的状态变了就发给前端。
    fn publish_meta(&mut self) {
        if let Some(meta) = self.session.take_meta() {
            // 看板按这两样给任务分列，变了才告诉宿主；标题这类常变的不算。
            let state = (meta.agent.as_ref().map(|agent| agent.state), meta.foreground_is_shell);
            if self.board_state.replace(state) != Some(state) {
                self.ui.todo_session_changed(self.id, false);
            }
            let event = Event::msg(HostMsg::Meta { id: self.id, meta, shell: None });
            // 状态不攒：连上时直接给最新的。
            self.subscribers.retain_mut(|s| (s.sink)(event.clone()));
        }
    }

    /// 把一件事发给连着的前端。输出先经过 `redactor` 抹掉 shell 集成报告的内容（报告带的口令不出
    /// 宿主）：它跟着整条输出流走，没有前端连着时也要经过。
    fn emit(&mut self, event: Event) {
        let event = match event {
            Event::Output(data) => match self.redactor.redact(&data) {
                // 整块都是报告的内容，抹完什么都不剩，不用发。
                Some(redacted) if redacted.is_empty() => return,
                Some(redacted) => Event::Output(redacted.into()),
                None => Event::Output(data),
            },
            event => event,
        };
        self.subscribers.retain_mut(|subscriber| (subscriber.sink)(event.clone()));
    }
}

/// 一个连着的前端。
struct Subscriber {
    /// 连接的编号，`Inbox::Detach` 按它找。
    connection: u64,
    sink: EventSink,
    /// 是桌面的界面，见 `SessionInfo::claimed`。
    desktop: bool,
}

/// 读线程交来、会话线程还没处理完的输出有多少字节：读线程每交一块先记上（超过
/// `PTY_BACKLOG_BYTES` 就等），会话线程处理完一块再减掉。没超过时只碰一个原子计数，不加锁。
#[derive(Default)]
struct Credits {
    bytes: AtomicUsize,
    closed: AtomicBool,
    lock: Mutex<()>,
    drained: Condvar,
}

impl Credits {
    /// 记上 `len` 字节，积压太多就等；会话已经结束时返回 false。
    fn acquire(&self, len: usize) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        if self.bytes.fetch_add(len, Ordering::AcqRel) + len <= PTY_BACKLOG_BYTES {
            return true;
        }
        let mut guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        while self.bytes.load(Ordering::Acquire) > PTY_BACKLOG_BYTES && !self.closed.load(Ordering::Acquire) {
            guard = self.drained.wait(guard).unwrap_or_else(PoisonError::into_inner);
        }
        !self.closed.load(Ordering::Acquire)
    }

    /// 处理完了 `len` 字节。从超过上限降到上限以内时叫醒等着的读线程；它判断和睡下都在锁里，
    /// 这里拿着锁叫，不会错过。
    fn release(&self, len: usize) {
        let before = self.bytes.fetch_sub(len, Ordering::AcqRel);
        if before > PTY_BACKLOG_BYTES && before - len <= PTY_BACKLOG_BYTES {
            let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
            self.drained.notify_all();
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.drained.notify_all();
    }
}

/// 把收件箱里紧跟在 `first` 后面排着的 PTY 输出接到它后面，合起来最多 `COALESCE_BYTES`，好让
/// 刷屏时一次喂 VT、一次发帧，不必每个小块（常常只有十几个字节）都走一遍 `Runner::output`。
/// 取到的第一条别的消息，或者接上就超了的那块输出，放进 `stash` 由下一轮先办，先后不变。
fn coalesce(first: Arc<[u8]>, inbox: &mpsc::Receiver<Inbox>, stash: &mut Option<Inbox>) -> Arc<[u8]> {
    let mut merged: Option<Vec<u8>> = None;
    let mut len = first.len();
    while len < COALESCE_BYTES {
        let Ok(message) = inbox.try_recv() else { break };
        match message {
            Inbox::Pty(PtyEvent::Output(data)) if len + data.len() <= COALESCE_BYTES => {
                merged.get_or_insert_with(|| first.to_vec()).extend_from_slice(&data);
                len += data.len();
            }
            message => {
                *stash = Some(message);
                break;
            }
        }
    }
    merged.map_or(first, Arc::from)
}

fn output_len(event: &PtyEvent) -> usize {
    match event {
        PtyEvent::Output(data) => data.len(),
        PtyEvent::Exited => 0,
    }
}

struct CloseOnDrop(Arc<Credits>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: GridSize = GridSize { cols: 20, rows: 4, cell_width_px: 8, cell_height_px: 16 };
    /// 测试里当 shell 用的程序：一直开着读输入，不起子进程，所以前台总是它自己。认不出的 shell 会被加上
    /// `-l`，macOS 的 cat 认它，GNU cat 不认，别的 Unix 上用 `tests/fixtures/cat`。Windows 上没有 `cat`，用 cmd。
    const CAT: &str = if cfg!(windows) {
        "cmd.exe"
    } else if cfg!(target_os = "macos") {
        "/bin/cat"
    } else {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cat")
    };

    /// `CAT` 当 shell 的会话线程状态，`start` 时已经启动。
    pub(super) fn runner(start: bool) -> Runner {
        let mut pty = Pty::open(SIZE, Box::new(|_| true)).unwrap();
        if start {
            pty.start(Some(CAT), None, IntegrationMode::Off).unwrap();
        }
        let session = HostSession::new(SIZE, pty, None, &TermSettings::default()).unwrap();
        Runner::new(SessionId(1), session, Some(CAT.into()), TermSettings::default(), Arc::default())
    }

    #[test]
    fn unstarted_sessions_do_not_wake_up_until_started() {
        let mut runner = runner(false);
        assert_eq!(runner.deadline(), None, "an unstarted session has nothing to poll");
        // 没到点的醒来（比如别的消息）也不该排上轮询。
        runner.tick();
        assert_eq!(runner.deadline(), None);
        let before = Instant::now();
        runner.handle(Inbox::Start { integration: IntegrationMode::Off });
        assert!(runner.session.started());
        let deadline = runner.deadline().expect("a started session polls its foreground");
        assert!(deadline >= before + FOREGROUND_POLL_INTERVAL && deadline <= Instant::now() + FOREGROUND_POLL_INTERVAL);
    }

    /// 排在一起的输出合成一块，夹着别的消息时各自成块、先后不变；接上就超了 `COALESCE_BYTES`
    /// 的那块另起一块。
    #[test]
    fn adjacent_output_is_coalesced_in_order() {
        let (tx, rx) = mpsc::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let sink: EventSink = Box::new(move |event| {
            sink_seen.lock().unwrap().push(event);
            true
        });
        let start = Box::new(move |_| Some(sink));
        let subscribe =
            Subscribe { connection: 1, size: None, mode: AttachMode::MetaOnly, start, desktop: false, device: None };
        let output = |data: &[u8]| Inbox::Pty(PtyEvent::Output(data.into()));
        let big = vec![b'x'; COALESCE_BYTES];
        for message in [
            Inbox::Subscribe(subscribe),
            output(b"a"),
            output(b"b"),
            Inbox::Resize { connection: 2, size: GridSize { cols: 30, ..SIZE } },
            output(b"c"),
            output(&big),
            output(b"d"),
            output(b"e"),
        ] {
            tx.send(message).unwrap();
        }
        drop(tx);
        runner(true).run(&rx, &Arc::new(AtomicBool::new(false)));
        let seen: Vec<Vec<u8>> = seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                Event::Output(data) => Some(data.to_vec()),
                Event::Msg(message) if matches!(**message, HostMsg::Resized { .. }) => Some(b"<resized>".to_vec()),
                Event::Msg(_) => None,
            })
            .collect();
        let expected = [b"ab".to_vec(), b"<resized>".to_vec(), b"c".to_vec(), big, b"de".to_vec()];
        assert_eq!(seen, expected);
    }

    /// 没启动的 shell 不会被等超时的输入抢先写进 PTY，免得绕过提示符检测被启动脚本读走。
    #[test]
    fn typing_at_the_prompt_waits_for_the_shell_to_start() {
        let mut runner = runner(false);
        let long_ago = Instant::now().checked_sub(TYPE_AT_PROMPT_PATIENCE * 2).unwrap();
        runner.type_ahead = Some((b"claude\r".to_vec(), long_ago));
        assert_eq!(runner.deadline(), None);
        runner.type_at_prompt();
        assert!(runner.type_ahead.is_some(), "written before the shell started");
        let before = Instant::now();
        runner.handle(Inbox::Start { integration: IntegrationMode::Off });
        let (_, since) = runner.type_ahead.as_ref().expect("still waiting for the prompt");
        assert!(*since >= before, "the wait starts when the shell starts");
    }

    #[test]
    fn started_sessions_poll_the_foreground() {
        let before = Instant::now();
        let runner = runner(true);
        let deadline = runner.deadline().expect("a started session polls its foreground");
        assert!(deadline >= before + FOREGROUND_POLL_INTERVAL && deadline <= Instant::now() + FOREGROUND_POLL_INTERVAL);
    }
}
