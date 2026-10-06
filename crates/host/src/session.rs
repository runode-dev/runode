//! 一个会话的线程：`HostSession` 在这个线程里建好，从不离开它（它不是 `Send`）。线程之间只传
//! `Send` 的数据：PTY 的读线程交来的输出、前端的请求，以及发给前端的 `Event`。
//!
//! 收件箱是一个 `std::sync::mpsc` 的 channel，PTY 输出和前端的请求都排在里面，按到达的先后
//! 处理。PTY 输出另外限了量：读线程最多积压 `PTY_BACKLOG_BYTES` 字节，再多就等会话线程处理掉
//! 一些，程序输出得比宿主的 VT 处理得快时，被拖慢的是程序，不是内存。前端的请求不限量，发的
//! 一方从不等。没有事的时候按 agent 识别和前台进程轮询要的时刻醒来。
//!
//! 宿主升级时会话交给新宿主（见 `handoff`）：旧宿主这边 `Inbox::Prepare` 让会话停下来交出状态、
//! 冻结，`Inbox::Release` 提交（交出 PTY、线程结束）、`Inbox::Resume` 回滚；新宿主这边 `adopt`
//! 接手，PTY 停在闸门上，`Inbox::Open` 提交后打开闸门，`Inbox::Release` 不接手了原样交回去。

use std::{
    any::Any,
    ffi::OsString,
    os::fd::OwnedFd,
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use runode_protocol::{AttachMode, FinishedCommand, HostMsg, SessionId, SessionInfo};
use runode_shared_types::{
    grid::GridSize,
    input::KeyChord,
    session::{DriveAction, SessionMeta},
    settings::TermSettings,
    shell::IntegrationMode,
};
use runode_terminal::{
    SnapshotError, history,
    host_session::{HostSession, ImportError, ImportScreen, RedactorState, ReportRedactor, SessionExport},
    pty::{self, Pty, PtyEvent, PtyHandoff},
};

use crate::SpawnOptions;

/// PTY 读线程最多积压这么多字节的输出，再多就等会话线程处理：64 块读满的缓冲（每块 64 KiB）。
/// 按字节而不按块数算：程序不停输出时一次只读到 1 KiB 左右，按块数限的话积压不了多少，会话
/// 线程稍一耽搁读线程就得停下。
const PTY_BACKLOG_BYTES: usize = 64 * 64 * 1024;
/// 重新读取前台进程的间隔：不输出的程序（比如 `sleep`）启动后，标签名也能跟上。
const FOREGROUND_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 有输出时重读前台进程的最短间隔：大量输出时不必每块都做几次系统调用，推迟的那次到点补上。
const FOREGROUND_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
/// 交接时 VT 停在一条编不进快照的序列中间（`SnapshotError::Unfinished`），最多这么久里让程序
/// 接着输出、再试；还不行就只交重放。
const SNAPSHOT_PATIENCE: Duration = Duration::from_secs(1);
/// 编不出快照时每次让程序接着输出这么久再停下来试。
const UNFINISHED_RETRY: Duration = Duration::from_millis(100);
/// 交接时等读线程停下的过程中，隔这么久看一次它停了没有。
const DRAIN_POLL: Duration = Duration::from_millis(2);

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
    Input(Vec<u8>),
    Resize(GridSize),
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
    /// 谁最近在操作这个会话，见 `SessionMeta::driver`：`Some` 是别的终端里的程序经 socket 做了
    /// 一件事，排在那件事前面；`None` 是用户在界面里打了字，清掉记录。
    Driven(Option<Drive>),
    Kill,
    /// 交接（旧宿主）：停下读 PTY、处理完已经读出来的输出，交出会话的状态，之后冻结，见
    /// `Runner::hand_over`。
    Prepare(mpsc::Sender<Prepared>),
    /// 交出 PTY（不结束 shell），回交出时还没写进 PTY 的输入，线程随即结束：旧宿主在交接提交时
    /// 发，新宿主不接手了时发（PTY 停在闸门上，原样交回）。
    Release(mpsc::Sender<Vec<u8>>),
    /// 交接回滚（旧宿主）：接着读 PTY，按先后重放冻结期间存下的请求。
    Resume,
    /// 交接提交了（新宿主）：先把旧宿主没写进 PTY 的这些输入排进写队列，再打开读写线程的闸门。
    Open(Vec<u8>),
}

/// 会话对 `Inbox::Prepare` 的回话。
pub(crate) enum Prepared {
    /// 不交：shell 已经退出了，或者会话正被结束。交接提交时结束它，回滚时照旧留着。
    Gone,
    /// 交不了（比如复制不了 PTY 的描述符），整个交接放弃。
    Failed(String),
    Ready(Box<Exported>),
}

/// 会话交给新宿主的东西，见 `runode_protocol::HandoffPart::Session`。
pub(crate) struct Exported {
    pub(crate) export: SessionExport,
    /// 复制的一份 PTY master，自己这份照常留着直到提交；还没启动 shell 时为空。
    pub(crate) master: Option<OwnedFd>,
    /// shell 的进程号；还没启动时为空。
    pub(crate) pid: Option<u32>,
    /// 宿主那份 VT 的原始快照（没抹口令）；编不出来时为空，新宿主只能用重放。
    pub(crate) snapshot: Option<Vec<u8>>,
    /// 重画当前屏幕的 VT 序列。
    pub(crate) replay: Vec<u8>,
    /// 给前端的输出流停在哪里，新宿主接着抹。
    pub(crate) redactor: RedactorState,
    /// `Inbox::Start` 时启动的程序。
    pub(crate) shell: Option<String>,
    /// 开会话时另外设的环境变量，还没启动的会话在新宿主里启动时用。
    pub(crate) extra_env: Vec<(String, String)>,
}

/// 交接过来、已经启动了 shell 的会话，见 `adopt`。
pub(crate) struct Adopted {
    pub(crate) handoff: PtyHandoff,
    pub(crate) export: SessionExport,
    /// 用它重建 VT；为空（或者解不开）时用 `replay`。
    pub(crate) snapshot: Option<Vec<u8>>,
    pub(crate) replay: Vec<u8>,
    pub(crate) redactor: RedactorState,
    pub(crate) shell: Option<String>,
}

/// 开会话和接手会话共用的设置。
pub(crate) struct Setup {
    pub(crate) id: SessionId,
    /// VT 一开始套的主题。
    pub(crate) settings: TermSettings,
    /// 启动 shell 时另外设的全部环境变量（宿主的、这个会话的和 `runode_protocol::ENV_SESSION`）。
    pub(crate) env: Vec<(String, OsString)>,
    /// 其中开会话时指定的那些，交接还没启动的会话时带给新宿主。
    pub(crate) extra_env: Vec<(String, String)>,
    pub(crate) record_history: Arc<AtomicBool>,
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
    pub(crate) connection: u64,
    /// 前端视图的尺寸，先按它改会话的尺寸，屏幕按改好的尺寸给。
    pub(crate) size: Option<GridSize>,
    /// 要什么样的屏幕；`Snapshot` 编不出来时退成 `VtReplay`。
    pub(crate) mode: AttachMode,
    /// 在会话线程里调用，交给它当前的屏幕，返回之后收事件的 `EventSink`；为 `None` 时不连了。
    /// 它和之后的事件在同一个线程里按先后发生，前端收到的屏幕和输出之间不会漏也不会重。
    pub(crate) start: Box<dyn FnOnce(Screen) -> Option<EventSink> + Send>,
    /// 是桌面的界面连上来，`SessionInfo::claimed` 据此算。
    pub(crate) desktop: bool,
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
    /// `Inbox::Driven(None)` 不必进收件箱。
    driven: Arc<AtomicBool>,
}

impl Handle {
    /// 发一条消息，会话线程已经结束时返回 false。没被标过的会话收到 `Inbox::Driven(None)` 时
    /// 什么都不发，返回 true。
    pub(crate) fn send(&self, message: Inbox) -> bool {
        match &message {
            Inbox::Kill => self.killed.store(true, Ordering::Release),
            Inbox::Driven(Some(_)) => self.driven.store(true, Ordering::Release),
            Inbox::Driven(None) if !self.driven.swap(false, Ordering::AcqRel) => return true,
            _ => {}
        }
        self.inbox.send(message).is_ok()
    }
}

/// 开会话：在调用的线程里打开伪终端（`start` 时连 shell 一起启动），错误当场返回；再起会话线程，
/// 等它把 `HostSession` 建好。
pub(crate) fn spawn(setup: Setup, options: SpawnOptions) -> Result<Handle> {
    let Setup { id, settings, env, extra_env, record_history } = setup;
    let (inbox, rx) = mpsc::channel();
    let credits = Arc::new(Credits::default());
    let sink = pty_sink(&inbox, &credits);
    let mut pty = Pty::open(options.size, sink)?;
    for (key, value) in env {
        pty.set_env(key, value);
    }
    if options.start {
        pty.start(options.shell.as_deref(), options.cwd.as_deref(), options.integration)?;
    }
    let (ready, created) = mpsc::channel();
    let killed = Arc::new(AtomicBool::new(false));
    let killed_flag = killed.clone();
    thread::Builder::new()
        .name(format!("session-{id}"))
        .spawn(move || {
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
            let mut runner = Runner::new(id, session, options.shell, settings, credits, record_history);
            runner.extra_env = extra_env;
            runner.run(&rx, &killed_flag);
        })
        .context("failed to start the session thread")?;
    created.recv().map_err(|_| anyhow!("the session thread ended while starting"))??;
    Ok(Handle { inbox, killed, driven: Arc::default() })
}

/// 读线程交 PTY 输出的 `PtySink`：先记上字节数（积压太多就等），再排进收件箱。
fn pty_sink(inbox: &mpsc::Sender<Inbox>, credits: &Arc<Credits>) -> pty::PtySink {
    let inbox = inbox.clone();
    let credits = credits.clone();
    Box::new(move |event| credits.acquire(output_len(&event)) && inbox.send(Inbox::Pty(event)).is_ok())
}

/// 接手交接过来的会话：PTY 停在闸门上（`Pty::adopt_paused`，不读不写），在会话线程里用快照
/// 重建 `HostSession`，快照没有或者解不开时用重放。不等重建完就返回，几个会话一起重建，见
/// `Adopting::finish`。闸门等 `Inbox::Open` 打开，不接手了发 `Inbox::Release`。
///
/// 失败时 PTY 已经原样交回、关掉了这边的描述符，shell 不受影响（交出方还拿着自己那份）。
/// PTY 在会话线程起好之后才经 channel 交过去：起不了线程时它还在这里，交回去，不会随着丢掉
/// 结束 shell。
pub(crate) fn adopt(setup: Setup, adopted: Adopted) -> Result<Adopting> {
    let Setup { id, settings: _, env: _, extra_env, record_history } = setup;
    let Adopted { handoff, export, snapshot, replay, redactor, shell } = adopted;
    let (inbox, rx) = mpsc::channel();
    let credits = Arc::new(Credits::default());
    let sink = pty_sink(&inbox, &credits);
    // 失败时交来的描述符随 `AdoptError` 丢掉，只关这边这份。
    let mut pty = Pty::adopt_paused(handoff, sink).map_err(|err| anyhow!("{err}"))?;
    let (give, take) = mpsc::channel::<Pty>();
    let (ready, created) = mpsc::channel();
    let killed = Arc::new(AtomicBool::new(false));
    let killed_flag = killed.clone();
    let spawned = thread::Builder::new().name(format!("session-{id}")).spawn(move || {
        pty::set_current_thread_interactive();
        let _close = CloseOnDrop(credits.clone());
        let Ok(pty) = take.recv() else { return };
        let settings = export.settings.clone();
        let (session, replayed) = match import(id, pty, snapshot.as_deref(), &replay, redactor, export) {
            Ok(imported) => imported,
            Err(err) => {
                let _ = ready.send(Err(err));
                return;
            }
        };
        let _ = ready.send(Ok(replayed));
        let mut runner = Runner::new(id, session, shell, settings, credits, record_history);
        runner.redactor = ReportRedactor::from_state(redactor);
        runner.extra_env = extra_env;
        runner.run(&rx, &killed_flag);
    });
    if let Err(err) = spawned {
        release(&mut pty);
        return Err(anyhow::Error::new(err).context("failed to start the session thread"));
    }
    if let Err(mpsc::SendError(mut pty)) = give.send(pty) {
        release(&mut pty);
        return Err(anyhow!("the session thread ended while starting"));
    }
    Ok(Adopting { handle: Handle { inbox, killed, driven: Arc::default() }, created })
}

/// 正在重建的接手来的会话，见 `adopt`。
pub(crate) struct Adopting {
    handle: Handle,
    created: mpsc::Receiver<Result<bool>>,
}

impl Adopting {
    /// 等会话线程把会话建好，返回它和它的屏幕是不是从重放重建的。失败时 PTY 已经原样交回。
    pub(crate) fn finish(self) -> Result<(Handle, bool)> {
        let replayed = self.created.recv().map_err(|_| anyhow!("the session thread ended while starting"))??;
        Ok((self.handle, replayed))
    }
}

/// 用快照（没有或者解不开时用重放）把交接过来的会话建起来，返回会话和是不是用的重放。都不成时
/// 把 PTY 原样交回去再报错。
fn import(
    id: SessionId,
    pty: Pty,
    snapshot: Option<&[u8]>,
    replay: &[u8],
    redactor: RedactorState,
    export: SessionExport,
) -> Result<(HostSession, bool)> {
    let pty = match snapshot {
        Some(snapshot) => match HostSession::import(pty, ImportScreen::Snapshot(snapshot), export.clone()) {
            Ok(session) => return Ok((session, false)),
            Err(err) => {
                let ImportError { pty, error } = *err;
                tracing::warn!("session {id} falls back to its VT replay: {error:#}");
                pty
            }
        },
        None => pty,
    };
    match HostSession::import(pty, ImportScreen::Replay { bytes: replay, redactor }, export) {
        Ok(session) => Ok((session, true)),
        Err(err) => {
            let ImportError { mut pty, error } = *err;
            release(&mut pty);
            Err(error.context(format!("failed to rebuild session {id}")))
        }
    }
}

/// 把接手来、还停着的 PTY 原样交回去：只关这边的描述符，不结束 shell。
fn release(pty: &mut Pty) {
    if let Err(err) = pty.release() {
        tracing::warn!("failed to give a pty back: {err:#}");
    }
}

/// 会话线程里的状态。
struct Runner {
    id: SessionId,
    session: HostSession,
    /// 连着的前端。
    subscribers: Vec<Subscriber>,
    /// 给前端的输出抹掉 shell 集成报告的内容，从会话开出来起每块输出都经过它。
    redactor: ReportRedactor,
    /// 宿主那份 VT 现在套着的主题：最近一次 `Inbox::Theme` 的，还没有过时是开出来时的。
    settings: Arc<TermSettings>,
    /// `Inbox::Start` 时启动的程序，见 `SpawnOptions::shell`。
    shell: Option<String>,
    /// 开会话时另外设的环境变量，交接还没启动的会话时带给新宿主，见 `Setup::extra_env`。
    extra_env: Vec<(String, String)>,
    credits: Arc<Credits>,
    record_history: Arc<AtomicBool>,
    /// 前端要清屏，等 VT 回到 ground 再清。
    clear_pending: bool,
    exited: bool,
    /// 下次按 `FOREGROUND_POLL_INTERVAL` 重读前台进程的时刻。
    next_poll: Instant,
    /// 上次因为有输出而重读前台进程的时刻，以及推迟到的那次。
    foreground_read_at: Instant,
    foreground_due: Option<Instant>,
}

impl Runner {
    fn new(
        id: SessionId,
        session: HostSession,
        shell: Option<String>,
        settings: TermSettings,
        credits: Arc<Credits>,
        record_history: Arc<AtomicBool>,
    ) -> Self {
        let now = Instant::now();
        let mut runner = Self {
            id,
            session,
            subscribers: Vec::new(),
            redactor: ReportRedactor::new(),
            settings: Arc::new(settings),
            shell,
            extra_env: Vec::new(),
            credits,
            record_history,
            clear_pending: false,
            exited: false,
            next_poll: now + FOREGROUND_POLL_INTERVAL,
            foreground_read_at: now,
            foreground_due: None,
        };
        // shell 已经在起始目录里跑起来了，不等第一次输出，前端一连上就有名字。
        runner.session.refresh_foreground();
        runner
    }

    fn run(mut self, inbox: &mpsc::Receiver<Inbox>, killed: &AtomicBool) {
        loop {
            let Some(message) = self.wait(inbox) else {
                return;
            };
            if killed.load(Ordering::Acquire) {
                self.ended();
                self.answer_pending(message, inbox);
                return;
            }
            // 处理一条消息时 panic 的话，`HostSession` 可能停在半路，不能再往下处理；告诉前端会话
            // 没了，免得视图一直停在最后一屏不动。
            let handled = panic::catch_unwind(AssertUnwindSafe(|| match message {
                Some(Inbox::Prepare(reply)) => self.hand_over(&reply, inbox, killed),
                message => self.step(message),
            }));
            match handled {
                Ok(true) => {}
                Ok(false) => return,
                Err(panic) => {
                    self.crashed(panic.as_ref());
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
        self.publish_meta();
        true
    }

    /// 会话结束了（前端要结束它，或者处理消息时 panic）：还没报告过退出的，告诉连着的前端
    /// （比如经 socket 等着 agent 的命令行）；叫结束的那一方多半已经不收了。
    fn ended(&mut self) {
        if !std::mem::replace(&mut self.exited, true) {
            self.emit(Event::msg(HostMsg::Exited { id: self.id, status: None }));
        }
    }

    /// 被结束时还排在收件箱里的消息（`first` 是已经取出来的那条）：积压的输出和别的请求都丢掉，
    /// 等着回话的一方随之收到断开；只有前端连上来（`Subscribe`）要回，不然它收不到
    /// `Attached`，干等到超时。照常给它现在的屏幕，`subscribe` 见会话已经结束，接着补发 `Exited`；
    /// 不按它的尺寸改会话。
    ///
    /// 宿主先把会话从登记表里拿掉、再置上 `Handle::killed` 的标记，`Subscribe` 又是经登记表送来
    /// 的，所以看到标记时，所有送得到的 `Subscribe` 都已经在收件箱里了。
    fn answer_pending(&mut self, first: Option<Inbox>, inbox: &mpsc::Receiver<Inbox>) {
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

    /// 下次没有消息也要醒来的时刻；shell 退出以后不再轮询。
    fn deadline(&self) -> Option<Instant> {
        if self.exited {
            return None;
        }
        [Some(self.next_poll), self.foreground_due, self.session.agent_deadline()].into_iter().flatten().min()
    }

    fn handle(&mut self, message: Inbox) {
        match message {
            Inbox::Pty(PtyEvent::Output(data)) => {
                self.output(&data);
                self.credits.release(data.len());
            }
            Inbox::Pty(PtyEvent::Exited) => {
                self.exited = true;
                self.emit(Event::msg(HostMsg::Exited { id: self.id, status: None }));
            }
            Inbox::Detach { connection } => self.subscribers.retain(|s| s.connection != connection),
            Inbox::Start { integration } => {
                if let Err(err) = self.session.start(self.shell.as_deref(), integration) {
                    tracing::error!("failed to start terminal session: {err:#}");
                    self.exited = true;
                    self.emit(Event::msg(HostMsg::Exited { id: self.id, status: None }));
                    return;
                }
                self.refresh_foreground(Instant::now());
            }
            Inbox::Input(data) => self.session.write(data),
            Inbox::Resize(size) => {
                if self.session.resize(size) {
                    self.emit(Event::msg(HostMsg::Resized { id: self.id, size }));
                }
            }
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
            Inbox::Info(reply) => {
                let _ = reply.send(SessionInfo {
                    id: self.id,
                    size: self.session.size(),
                    meta: self.session.meta(),
                    clients: u32::try_from(self.subscribers.len()).unwrap_or(u32::MAX),
                    claimed: self.subscribers.iter().any(|s| s.desktop),
                    exited: self.exited,
                    // 尺寸归属还没做。
                    size_owner: None,
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
            Inbox::Driven(Some(Drive { by, action })) => {
                let at_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX));
                self.session.drive(by.map(|by| by.to_string()), action, at_ms);
            }
            Inbox::Driven(None) => self.session.clear_driver(),
            Inbox::Open(pending) => {
                if !pending.is_empty() {
                    self.session.write(pending);
                }
                if let Err(err) = self.session.resume_reading() {
                    tracing::error!("session {} cannot read its pty after the handoff: {err:#}", self.id);
                }
            }
            // 交接以外的时候收到的：`Prepare` 在 `run` 里办，丢掉回话的一端，等的一方当会话没了；
            // `Release` 在 `step` 里办；没有冻结着时 `Resume` 没什么可做。
            Inbox::Kill | Inbox::Prepare(_) | Inbox::Release(_) | Inbox::Resume => {}
        }
    }

    /// 交接（`Inbox::Prepare`）：停下来交出状态（`prepare`），之后冻结，直到交接提交
    /// （`Inbox::Release`：交出 PTY，线程结束）或者回滚（`Inbox::Resume`）。返回会话是否还要接着跑。
    ///
    /// 冻结期间 PTY 不读，程序之后的输出留在 PTY 里，归提交后的新宿主或者回滚后的自己。前端改会话
    /// 的请求（输入、改尺寸、换主题、清屏、启动 shell、连上来等）存进 `Freeze::held`：回滚时按先后
    /// 重放；提交时其中的输入编好、接在没写出去的输入后面交给新宿主，别的丢掉（发请求的连接都已经
    /// 收到 `Goodbye`）。只读的请求（列会话、读屏幕）和断开照常办。不按时刻轮询 agent：读线程停着
    /// 没有输出，拖久了会把工作中的 agent 判成空闲。
    fn hand_over(
        &mut self,
        reply: &mpsc::Sender<Prepared>,
        inbox: &mpsc::Receiver<Inbox>,
        killed: &AtomicBool,
    ) -> bool {
        let mut freeze = Freeze::default();
        let prepared = self.prepare(inbox, killed, &mut freeze);
        let _ = reply.send(prepared);
        loop {
            if killed.load(Ordering::Acquire) {
                self.ended();
                self.answer_pending(None, inbox);
                return false;
            }
            match freeze.decision.take() {
                Some(Decision::Release(reply)) => {
                    let _ = reply.send(self.release(std::mem::take(&mut freeze.held)));
                    return false;
                }
                Some(Decision::Resume) => {
                    self.resume(std::mem::take(&mut freeze.held));
                    return true;
                }
                None => {}
            }
            let Ok(message) = inbox.recv() else { return false };
            self.frozen(message, &mut freeze);
        }
    }

    /// 交接的第一步：已经启动了 shell 的，叫停读线程，把它已经读出来的输出处理完，编原始快照
    /// （VT 停在编不进快照的序列中间时让程序再输出一会儿再试，最多 `SNAPSHOT_PATIENCE`）和重放，
    /// 复制一份 PTY master；还没启动的只交状态。期间别的消息按冻结的规矩办，见 `hand_over`。
    fn prepare(&mut self, inbox: &mpsc::Receiver<Inbox>, killed: &AtomicBool, freeze: &mut Freeze) -> Prepared {
        if self.exited {
            return Prepared::Gone;
        }
        let exported = |runner: &Self, snapshot, replay, master, pid| {
            Prepared::Ready(Box::new(Exported {
                export: runner.session.export(),
                master,
                pid,
                snapshot,
                replay,
                redactor: runner.redactor.state(),
                shell: runner.shell.clone(),
                extra_env: runner.extra_env.clone(),
            }))
        };
        if !self.session.export().started {
            return exported(self, None, Vec::new(), None, None);
        }
        self.session.stop_reading();
        let deadline = Instant::now() + SNAPSHOT_PATIENCE;
        let snapshot = loop {
            if !self.drain(inbox, killed, freeze) || self.exited {
                return Prepared::Gone;
            }
            match self.session.snapshot() {
                Ok(snapshot) => break Some(snapshot),
                Err(SnapshotError::Unfinished) if Instant::now() < deadline => {
                    if let Err(err) = self.session.resume_reading() {
                        tracing::warn!("session {} hands over a VT replay only: {err:#}", self.id);
                        self.session.stop_reading();
                        continue;
                    }
                    let until = (Instant::now() + UNFINISHED_RETRY).min(deadline);
                    while let Some(left) = until.checked_duration_since(Instant::now()) {
                        match inbox.recv_timeout(left) {
                            Ok(message) => self.frozen(message, freeze),
                            Err(mpsc::RecvTimeoutError::Timeout) => break,
                            Err(mpsc::RecvTimeoutError::Disconnected) => return Prepared::Gone,
                        }
                        if killed.load(Ordering::Acquire) {
                            return Prepared::Gone;
                        }
                    }
                    self.session.stop_reading();
                }
                Err(err) => {
                    tracing::warn!("session {} hands over a VT replay only: {err}", self.id);
                    break None;
                }
            }
        };
        let replay = self.session.vt_replay().unwrap_or_else(|err| {
            tracing::warn!("session {} cannot replay its screen: {err:#}", self.id);
            Vec::new()
        });
        match self.session.dup_master() {
            Ok(master) => exported(self, snapshot, replay, Some(master), self.session.shell_pid()),
            Err(err) => Prepared::Failed(format!("session {} cannot duplicate its pty: {err}", self.id)),
        }
    }

    /// 读线程叫停以后，把它已经交来的输出都处理完：等到 `pty_reader_finished`（之后读线程不会再
    /// 往收件箱里放东西），再取完收件箱里剩下的。别的消息按冻结的规矩办。会话被结束或者宿主没了
    /// 时返回 false。
    fn drain(&mut self, inbox: &mpsc::Receiver<Inbox>, killed: &AtomicBool, freeze: &mut Freeze) -> bool {
        loop {
            if killed.load(Ordering::Acquire) {
                return false;
            }
            let message = if self.session.pty_reader_finished() {
                match inbox.try_recv() {
                    Ok(message) => message,
                    Err(mpsc::TryRecvError::Empty) => return true,
                    Err(mpsc::TryRecvError::Disconnected) => return false,
                }
            } else {
                match inbox.recv_timeout(DRAIN_POLL) {
                    Ok(message) => message,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return false,
                }
            };
            self.frozen(message, freeze);
        }
    }

    /// 交接期间收到的一条消息，见 `hand_over`。
    fn frozen(&mut self, message: Inbox, freeze: &mut Freeze) {
        match message {
            Inbox::Pty(PtyEvent::Output(data)) => {
                self.output(&data);
                self.credits.release(data.len());
            }
            Inbox::Pty(PtyEvent::Exited) => {
                self.exited = true;
                self.emit(Event::msg(HostMsg::Exited { id: self.id, status: None }));
            }
            message @ (Inbox::Detach { .. } | Inbox::Info(_) | Inbox::ReadScreen { .. }) => self.handle(message),
            Inbox::Release(reply) => {
                freeze.decision.get_or_insert(Decision::Release(reply));
            }
            Inbox::Resume => {
                freeze.decision.get_or_insert(Decision::Resume);
            }
            // `Kill` 由 `Handle::killed` 的标记办；同时只会有一次交接。
            Inbox::Kill | Inbox::Prepare(_) => {}
            message => freeze.held.push(message),
        }
        self.publish_meta();
    }

    /// 交接提交了：交出 PTY（不结束 shell），返回交出时没写进 PTY 的输入，`held` 里存下的输入编好
    /// 接在后面。还没启动 shell 的会话没有 PTY 可交，只有存下的输入。
    fn release(&mut self, held: Vec<Inbox>) -> Vec<u8> {
        let mut pending = match self.session.release_pty() {
            // 丢掉交出来的 master 只关这边这份，新宿主手里还有一份。
            Ok(handoff) => handoff.pending_input,
            Err(err) => {
                tracing::debug!("session {} has no pty to release: {err:#}", self.id);
                Vec::new()
            }
        };
        for message in held {
            let bytes = match message {
                Inbox::Input(data) => Ok(data),
                Inbox::Keys(keys) => self.session.encode_keys(&keys),
                Inbox::Paste(text) => self.session.encode_paste(&text),
                _ => continue,
            };
            match bytes {
                Ok(bytes) => pending.extend_from_slice(&bytes),
                Err(err) => tracing::warn!("session {} drops input held during the handoff: {err:#}", self.id),
            }
        }
        pending
    }

    /// 交接回滚：接着读 PTY，按先后重放冻结期间存下的请求。
    fn resume(&mut self, held: Vec<Inbox>) {
        if let Err(err) = self.session.resume_reading() {
            tracing::error!("session {} cannot read its pty after the handoff was rolled back: {err:#}", self.id);
        }
        for message in held {
            self.handle(message);
        }
        self.publish_meta();
    }

    /// 连接上的前端连上来：按它的尺寸改好会话，给它当前的屏幕，之后的事件接着发给它。
    fn subscribe(&mut self, Subscribe { connection, size, mode, start, desktop }: Subscribe) {
        if let Some(size) = size
            && self.session.resize(size)
        {
            self.emit(Event::msg(HostMsg::Resized { id: self.id, size }));
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
    /// 紧跟着它发 `Bell`：界面那份 VT 降成只看状态（后台标签）时，响铃靠它。
    fn output(&mut self, data: &Arc<[u8]>) {
        self.emit(Event::Output(data.clone()));
        self.session.feed(data);
        if self.session.take_bell() {
            self.emit(Event::msg(HostMsg::Bell { id: self.id }));
        }
        let record = self.record_history.load(Ordering::Relaxed);
        for entry in self.session.take_commands() {
            let command =
                FinishedCommand { cmd: entry.cmd.clone(), cwd: entry.cwd.clone(), exit: entry.exit, ts: entry.ts };
            if record {
                history::record(entry);
            }
            self.emit(Event::msg(HostMsg::CommandFinished { id: self.id, command }));
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
        if self.foreground_due.is_some_and(|due| now >= due) || now >= self.next_poll {
            self.refresh_foreground(now);
        }
        if self.session.agent_deadline().is_some_and(|at| now >= at) {
            self.session.poll_agent();
        }
    }

    fn refresh_foreground(&mut self, now: Instant) {
        self.session.refresh_foreground();
        self.foreground_read_at = now;
        self.foreground_due = None;
        self.next_poll = now + FOREGROUND_POLL_INTERVAL;
    }

    /// 对外公布的状态变了就发给前端。
    fn publish_meta(&mut self) {
        if let Some(meta) = self.session.take_meta() {
            let event = Event::msg(HostMsg::Meta { id: self.id, meta });
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

/// 交接期间冻结着的会话存下的东西，见 `Runner::hand_over`。
#[derive(Default)]
struct Freeze {
    /// 冻结期间收到的、改会话的请求，按先后。
    held: Vec<Inbox>,
    /// 交接的结果：先到的那个算。
    decision: Option<Decision>,
}

enum Decision {
    Release(mpsc::Sender<Vec<u8>>),
    Resume,
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
