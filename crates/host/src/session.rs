//! 一个会话的线程：`HostSession` 在这个线程里建好，从不离开它（它不是 `Send`）。线程之间只传
//! `Send` 的数据：PTY 的读线程交来的输出、前端的请求，以及发给前端的 `HostEvent`。
//!
//! 收件箱是一个 `std::sync::mpsc` 的 channel，PTY 输出和前端的请求都排在里面，按到达的先后
//! 处理。PTY 输出另外限了量：读线程最多积压 `PTY_BACKLOG_BYTES` 字节，再多就等会话线程处理掉
//! 一些，程序输出得比宿主的 VT 处理得快时，被拖慢的是程序，不是内存。前端的请求不限量，发的
//! 一方从不等。没有事的时候按 agent 识别和前台进程轮询要的时刻醒来。

use std::{
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use runode_protocol::{FinishedCommand, HostMsg, SessionId};
use runode_shared_types::{grid::GridSize, settings::TermSettings, shell::IntegrationMode};
use runode_terminal::{
    history,
    host_session::HostSession,
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
    Attach { connection: u64, sink: Sink, reply: mpsc::Sender<Result<Attached>> },
    Detach { connection: u64 },
    Start { integration: IntegrationMode },
    Input(Vec<u8>),
    Resize(GridSize),
    ClearScreen,
    Theme(Arc<TermSettings>),
    Kill,
}

/// 往会话线程发消息的一端。
pub(crate) struct Handle {
    inbox: mpsc::Sender<Inbox>,
}

impl Handle {
    /// 发一条消息，会话线程已经结束时返回 false。
    pub(crate) fn send(&self, message: Inbox) -> bool {
        self.inbox.send(message).is_ok()
    }
}

/// 开会话：在调用的线程里打开伪终端（`start` 时连 shell 一起启动），错误当场返回；再起会话线程，
/// 等它把 `HostSession` 建好。
pub(crate) fn spawn(
    id: SessionId,
    options: SpawnOptions,
    settings: TermSettings,
    record_history: Arc<AtomicBool>,
) -> Result<Handle> {
    let (inbox, rx) = mpsc::channel();
    let credits = Arc::new(Credits::default());
    let sink: pty::PtySink = {
        let inbox = inbox.clone();
        let credits = credits.clone();
        Box::new(move |event| credits.acquire(output_len(&event)) && inbox.send(Inbox::Pty(event)).is_ok())
    };
    let pty = if options.start {
        Pty::spawn(options.size, options.shell.as_deref(), options.cwd.as_deref(), options.integration, sink)?
    } else {
        Pty::open(options.size, sink)?
    };
    let (ready, created) = mpsc::channel();
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
            Runner::new(id, session, options, settings, credits, record_history).run(&rx);
        })
        .context("failed to start the session thread")?;
    created.recv().map_err(|_| anyhow!("the session thread ended while starting"))??;
    Ok(Handle { inbox })
}

/// 有大量输出时会话线程处理完一块后空转等下一块的最长时间。
const STREAMING_SPIN: Duration = Duration::from_micros(50);

/// 空转着等下一条消息，最多 `STREAMING_SPIN`；没等到时为 `None`，收件箱断开了也是 `None`，
/// 留给接着睡下等的那一步发现。
fn spin_recv(inbox: &mpsc::Receiver<Inbox>) -> Option<Option<Inbox>> {
    let until = Instant::now() + STREAMING_SPIN;
    loop {
        match inbox.try_recv() {
            Ok(message) => return Some(Some(message)),
            Err(mpsc::TryRecvError::Disconnected) => return None,
            Err(mpsc::TryRecvError::Empty) if Instant::now() >= until => return None,
            Err(mpsc::TryRecvError::Empty) => std::hint::spin_loop(),
        }
    }
}

/// 会话线程里的状态。
struct Runner {
    id: SessionId,
    session: HostSession,
    /// 连着的前端，按连接的编号。
    subscribers: Vec<(u64, Sink)>,
    /// 还没有前端连上过时攒着的事件，第一个连上的前端先收到它们；为 `None` 时已经连上过，或者
    /// 攒得太多放弃了（`backlog_lost`）。
    backlog: Option<Vec<HostEvent>>,
    backlog_bytes: usize,
    backlog_lost: bool,
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
    ) -> Self {
        let now = Instant::now();
        let mut runner = Self {
            id,
            session,
            subscribers: Vec::new(),
            backlog: Some(Vec::new()),
            backlog_bytes: 0,
            backlog_lost: false,
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

    fn run(mut self, inbox: &mpsc::Receiver<Inbox>) {
        let mut streaming = false;
        loop {
            // 程序正在大量输出时下一块多半马上就到，先空转等一小会儿：线程不睡下，读线程交下一块时
            // 就不用叫醒它，省掉每块一次的系统调用。
            let spun = if streaming { spin_recv(inbox) } else { None };
            let message = match spun {
                Some(message) => Some(message),
                None => self.wait(inbox),
            };
            let Some(message) = message else {
                return;
            };
            streaming = matches!(message, Some(Inbox::Pty(PtyEvent::Output(_))));
            match message {
                Some(Inbox::Kill) => return,
                Some(message) => self.handle(message),
                None => self.tick(),
            }
            self.publish_meta();
        }
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
            Inbox::Detach { connection } => self.subscribers.retain(|(c, _)| *c != connection),
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
                    self.emit(HostEvent::msg(HostMsg::ThemeApplied { id: self.id }));
                }
            }
            Inbox::Kill => {}
        }
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
        // 进出目录、启动或退出程序时通常都有输出，顺带重读前台进程。
        let now = Instant::now();
        if self.foreground_due.is_none() {
            let due = self.foreground_read_at + FOREGROUND_REFRESH_INTERVAL;
            if now >= due {
                self.refresh_foreground(now);
            } else {
                self.foreground_due = Some(due);
            }
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

    /// 没有消息、到了该醒的时刻：轮询前台进程，判断 agent 状态。
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
            self.subscribers.retain_mut(|(_, sink)| sink(event.clone()));
        }
    }

    fn attach(&mut self, connection: u64, mut sink: Sink) -> Result<Attached> {
        let Some(backlog) = self.backlog.take() else {
            return Err(anyhow!(if self.backlog_lost {
                "too much output before the session was attached"
            } else {
                "the session is already attached"
            }));
        };
        let alive = backlog.into_iter().all(&mut sink);
        if alive {
            self.subscribers.push((connection, sink));
        }
        self.backlog_bytes = 0;
        let (size, settings) = self.created.clone();
        Ok(Attached { size, settings, meta: self.session.meta(), started: self.session.started() })
    }

    /// 把一件事发给连着的前端；还没有前端连上过时攒起来。
    fn emit(&mut self, event: HostEvent) {
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
        self.subscribers.retain_mut(|(_, sink)| sink(event.clone()));
    }
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
