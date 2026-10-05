//! 一个会话的线程：`HostSession` 在这个线程里建好，从不离开它（它不是 `Send`）。线程之间只传
//! `Send` 的数据：PTY 的读线程交来的输出、前端的请求，以及发给前端的 `HostEvent`。
//!
//! 收件箱是一个 `std::sync::mpsc` 的 channel，PTY 输出和前端的请求都排在里面，按到达的先后
//! 处理。PTY 输出另外限了量：读线程最多积压 `PTY_BACKLOG_BYTES` 字节，再多就等会话线程处理掉
//! 一些，程序输出得比宿主的 VT 处理得快时，被拖慢的是程序，不是内存。前端的请求不限量，发的
//! 一方从不等。没有事的时候按 agent 识别和前台进程轮询要的时刻醒来。

use std::{
    any::Any,
    ffi::OsString,
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
use runode_shared_types::{grid::GridSize, session::SessionMeta, settings::TermSettings, shell::IntegrationMode};
use runode_terminal::{
    history,
    host_session::{HostSession, ReportRedactor},
    pty::{self, Pty, PtyEvent},
};

use crate::{Attached, HostEvent, Sink, SpawnOptions};

/// PTY 读线程最多积压这么多字节的输出，再多就等会话线程处理：64 块读满的缓冲（每块 64 KiB）。
/// 按字节而不按块数算：程序不停输出时一次只读到 1 KiB 左右，按块数限的话积压不了多少，会话
/// 线程稍一耽搁读线程就得停下。
const PTY_BACKLOG_BYTES: usize = 64 * 64 * 1024;
/// 重新读取前台进程的间隔：不输出的程序（比如 `sleep`）启动后，标签名也能跟上。
const FOREGROUND_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// 有输出时重读前台进程的最短间隔：大量输出时不必每块都做几次系统调用，推迟的那次到点补上。
const FOREGROUND_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
/// 还没有前端连上时攒着的事件最多这么多字节的输出，再多就不攒了，之后的连接会失败。
const BACKLOG_LIMIT: usize = 16 << 20;

/// 会话线程收到的消息。
pub(crate) enum Inbox {
    Pty(PtyEvent),
    Attach {
        connection: u64,
        sink: Sink,
        reply: mpsc::Sender<Result<Attached>>,
    },
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
    /// socket 上的前端连上来，见 `Subscribe`。
    Subscribe(Subscribe),
    /// 要这个会话在 `SessionList` 里的一项。
    Info(mpsc::Sender<SessionInfo>),
    /// 读屏幕底部的文字，见 `HostSession::screen_text`。
    ReadScreen {
        lines: Option<u32>,
        reply: mpsc::Sender<Result<String>>,
    },
    Kill,
}

/// socket 上的前端连上一个会话。和进程内的 `Inbox::Attach` 不同，不补发攒着的事件，而是
/// 当场给一份现在的屏幕（快照或 VT 重放），之后的事件接着它；一个会话能这样连上任意多次。
pub(crate) struct Subscribe {
    pub(crate) connection: u64,
    /// 前端视图的尺寸，先按它改会话的尺寸，屏幕按改好的尺寸给。
    pub(crate) size: Option<GridSize>,
    /// 要什么样的屏幕；`Snapshot` 编不出来时退成 `VtReplay`。
    pub(crate) mode: AttachMode,
    /// 在会话线程里调用，交给它当前的屏幕，返回之后收事件的 `Sink`；为 `None` 时不连了。
    /// 它和之后的事件在同一个线程里按先后发生，前端收到的屏幕和输出之间不会漏也不会重。
    pub(crate) start: Box<dyn FnOnce(Screen) -> Option<Sink> + Send>,
}

/// `Subscribe` 时会话当前的样子。
pub(crate) struct Screen {
    pub(crate) size: GridSize,
    pub(crate) meta: SessionMeta,
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
}

impl Handle {
    /// 发一条消息，会话线程已经结束时返回 false。
    pub(crate) fn send(&self, message: Inbox) -> bool {
        if matches!(message, Inbox::Kill) {
            self.killed.store(true, Ordering::Release);
        }
        self.inbox.send(message).is_ok()
    }
}

/// 开会话：在调用的线程里打开伪终端（`start` 时连 shell 一起启动），错误当场返回；再起会话线程，
/// 等它把 `HostSession` 建好。`env` 是启动 shell 时另外设的环境变量。
pub(crate) fn spawn(
    id: SessionId,
    options: SpawnOptions,
    settings: TermSettings,
    env: Vec<(String, OsString)>,
    record_history: Arc<AtomicBool>,
    keep_backlog: bool,
) -> Result<Handle> {
    let (inbox, rx) = mpsc::channel();
    let credits = Arc::new(Credits::default());
    let sink: pty::PtySink = {
        let inbox = inbox.clone();
        let credits = credits.clone();
        Box::new(move |event| credits.acquire(output_len(&event)) && inbox.send(Inbox::Pty(event)).is_ok())
    };
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
            Runner::new(id, session, options, settings, credits, record_history, keep_backlog).run(&rx, &killed_flag);
        })
        .context("failed to start the session thread")?;
    created.recv().map_err(|_| anyhow!("the session thread ended while starting"))??;
    Ok(Handle { inbox, killed })
}

/// 会话线程里的状态。
struct Runner {
    id: SessionId,
    session: HostSession,
    /// 连着的前端。
    subscribers: Vec<Subscriber>,
    /// 给 socket 上的前端的输出抹掉 shell 集成报告的内容，从会话开出来起每块输出都经过它。
    redactor: ReportRedactor,
    /// 还没有前端连上过时攒着的事件，第一个连上的前端先收到它们；为 `None` 时已经连上过，或者
    /// 攒得太多放弃了（`backlog_lost`）。
    backlog: Option<Vec<HostEvent>>,
    backlog_bytes: usize,
    backlog_lost: bool,
    /// 会话是 socket 上开的，从来不攒，见 `Client::spawn_with`。
    unbuffered: bool,
    /// 会话开出来时 VT 的尺寸和主题，攒着的事件要从这样一份 VT 喂起。
    created: (GridSize, TermSettings),
    /// `Inbox::Start` 时启动的程序，见 `SpawnOptions::shell`。
    shell: Option<String>,
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
        options: SpawnOptions,
        settings: TermSettings,
        credits: Arc<Credits>,
        record_history: Arc<AtomicBool>,
        keep_backlog: bool,
    ) -> Self {
        let now = Instant::now();
        let mut runner = Self {
            id,
            session,
            subscribers: Vec::new(),
            redactor: ReportRedactor::new(),
            backlog: keep_backlog.then(Vec::new),
            backlog_bytes: 0,
            backlog_lost: false,
            unbuffered: !keep_backlog,
            created: (options.size, settings),
            shell: options.shell,
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
                return;
            }
            // 处理一条消息时 panic 的话，`HostSession` 可能停在半路，不能再往下处理；告诉前端会话
            // 没了，免得视图一直停在最后一屏不动。
            match panic::catch_unwind(AssertUnwindSafe(|| self.step(message))) {
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
            self.emit(HostEvent::msg(HostMsg::Exited { id: self.id, status: None }));
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
        // 发给前端时又 panic 的（比如出事的正是前端的 `Sink`），也只能到此为止。
        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
            self.emit(HostEvent::msg(error));
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
                self.emit(HostEvent::msg(HostMsg::Exited { id: self.id, status: None }));
            }
            Inbox::Attach { connection, sink, reply } => {
                let _ = reply.send(self.attach(connection, sink));
            }
            Inbox::Detach { connection } => self.subscribers.retain(|s| s.connection != connection),
            Inbox::Start { integration } => {
                if let Err(err) = self.session.start(self.shell.as_deref(), integration) {
                    tracing::error!("failed to start terminal session: {err:#}");
                    self.exited = true;
                    self.emit(HostEvent::msg(HostMsg::Exited { id: self.id, status: None }));
                    return;
                }
                self.refresh_foreground(Instant::now());
            }
            Inbox::Input(data) => self.session.write(data),
            Inbox::Resize(size) => {
                if self.session.resize(size) {
                    self.emit(HostEvent::msg(HostMsg::Resized { id: self.id, size }));
                }
            }
            Inbox::ClearScreen => {
                self.clear_pending = true;
                self.try_clear();
            }
            Inbox::Theme(settings) => {
                if self.session.apply_theme(&settings) {
                    let settings = (*settings).clone();
                    self.emit(HostEvent::msg(HostMsg::ThemeApplied { id: self.id, settings }));
                }
            }
            Inbox::Subscribe(subscribe) => self.subscribe(subscribe),
            Inbox::Info(reply) => {
                let _ = reply.send(SessionInfo {
                    id: self.id,
                    size: self.session.size(),
                    meta: self.session.meta(),
                    clients: u32::try_from(self.subscribers.len()).unwrap_or(u32::MAX),
                    exited: self.exited,
                });
            }
            Inbox::ReadScreen { lines, reply } => {
                let _ = reply.send(self.session.screen_text(lines));
            }
            Inbox::Kill => {}
        }
    }

    /// socket 上的前端连上来：按它的尺寸改好会话，给它当前的屏幕，之后的事件接着发给它。
    fn subscribe(&mut self, Subscribe { connection, size, mode, start }: Subscribe) {
        if let Some(size) = size
            && self.session.resize(size)
        {
            self.emit(HostEvent::msg(HostMsg::Resized { id: self.id, size }));
        }
        let (mode, data) = match mode {
            AttachMode::MetaOnly => (mode, Vec::new()),
            AttachMode::Snapshot => match self.session.snapshot() {
                Ok(data) => (mode, data),
                Err(err) => {
                    tracing::debug!("session {} falls back to a VT replay: {err}", self.id);
                    (AttachMode::VtReplay, self.replay())
                }
            },
            AttachMode::VtReplay => (mode, self.replay()),
        };
        let screen = Screen { size: self.session.size(), meta: self.session.meta(), mode, data };
        let Some(mut sink) = start(screen) else { return };
        // shell 已经退出了：`Exited` 只在退出那一刻发过一次，晚连上的前端在这里补上，免得一直等。
        if self.exited && !sink(HostEvent::msg(HostMsg::Exited { id: self.id, status: None })) {
            return;
        }
        // 别的进程拿不到 shell 集成报告的口令，见 `ReportRedactor`。
        self.subscribers.push(Subscriber { connection, sink, redacted: true });
    }

    fn replay(&self) -> Vec<u8> {
        self.session.vt_replay().unwrap_or_else(|err| {
            tracing::warn!("session {} cannot replay its screen: {err:#}", self.id);
            Vec::new()
        })
    }

    /// 一块 PTY 输出：先原样转给前端，再喂宿主的 VT，然后处理它带来的变化。
    fn output(&mut self, data: &Arc<[u8]>) {
        self.emit(HostEvent::Output(data.clone()));
        self.session.feed(data);
        let record = self.record_history.load(Ordering::Relaxed);
        for entry in self.session.take_commands() {
            let command =
                FinishedCommand { cmd: entry.cmd.clone(), cwd: entry.cwd.clone(), exit: entry.exit, ts: entry.ts };
            if record {
                history::record(entry);
            }
            self.emit(HostEvent::msg(HostMsg::CommandFinished { id: self.id, command }));
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
            self.emit(HostEvent::Output(bytes.into()));
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
            let event = HostEvent::msg(HostMsg::Meta { id: self.id, meta });
            // 状态不攒：连上时直接给最新的。
            self.subscribers.retain_mut(|s| (s.sink)(event.clone()));
        }
    }

    fn attach(&mut self, connection: u64, mut sink: Sink) -> Result<Attached> {
        let Some(backlog) = self.backlog.take() else {
            return Err(anyhow!(if self.backlog_lost {
                "too much output before the session was attached"
            } else if self.unbuffered {
                "the session was opened over the socket and has no earlier output to replay"
            } else {
                "the session is already attached"
            }));
        };
        let alive = backlog.into_iter().all(&mut sink);
        if alive {
            // 进程内的桌面原样收，报告由宿主这份 VT 认，界面那份不看。
            self.subscribers.push(Subscriber { connection, sink, redacted: false });
        }
        self.backlog_bytes = 0;
        let (size, settings) = self.created.clone();
        Ok(Attached { size, settings, meta: self.session.meta(), started: self.session.started() })
    }

    /// 把一件事发给连着的前端；还没有前端连上过时攒起来。输出要先经过 `redactor`：它跟着整条
    /// 输出流走，不管这时有没有 socket 上的前端。
    fn emit(&mut self, event: HostEvent) {
        let redacted = match &event {
            HostEvent::Output(data) => self.redactor.redact(data).map(|data| HostEvent::Output(data.into())),
            HostEvent::Msg(_) => None,
        };
        if let Some(backlog) = &mut self.backlog {
            if let HostEvent::Output(data) = &event {
                self.backlog_bytes += data.len();
            }
            if self.backlog_bytes > BACKLOG_LIMIT {
                tracing::warn!("session {} produced too much output before it was attached", self.id);
                self.backlog = None;
                self.backlog_lost = true;
            } else {
                backlog.push(event.clone());
            }
        }
        self.subscribers.retain_mut(|subscriber| match &redacted {
            // 整块都是报告的内容，抹完什么都不剩，不用发。
            Some(HostEvent::Output(data)) if subscriber.redacted && data.is_empty() => true,
            Some(redacted) if subscriber.redacted => (subscriber.sink)(redacted.clone()),
            _ => (subscriber.sink)(event.clone()),
        });
    }
}

/// 一个连着的前端。
struct Subscriber {
    /// 连接的编号，`Inbox::Detach` 按它找。
    connection: u64,
    sink: Sink,
    /// 收抹掉了 shell 集成报告内容的输出：socket 上别的进程的前端。
    redacted: bool,
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
