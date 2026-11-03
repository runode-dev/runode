//! 交接时会话这一头：旧宿主上停下来交出状态、冻结，提交时交出 PTY、回滚时接着跑（`Runner::hand_over`）；
//! 新宿主上接手交过来的 PTY、用快照或重放重建会话（`adopt`）。

use std::{
    os::fd::OwnedFd,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use runode_protocol::SessionId;
use runode_terminal::{
    SnapshotError,
    host_session::{HostSession, ImportError, ImportScreen, RedactorState, ReportRedactor, SessionExport},
    pty::{self, Pty, PtyHandoff},
};

use super::{CloseOnDrop, Credits, Handle, Inbox, Runner, Setup, pty_sink};

/// 交接时 VT 停在一条编不进快照的序列中间（`SnapshotError::Unfinished`），最多这么久里让程序
/// 接着输出、再试；还不行就只交重放。
const SNAPSHOT_PATIENCE: Duration = Duration::from_secs(1);
/// 编不出快照时每次让程序接着输出这么久再停下来试。
const UNFINISHED_RETRY: Duration = Duration::from_millis(100);
/// 交接时等读线程停下的过程中，隔这么久看一次它停了没有。
const DRAIN_POLL: Duration = Duration::from_millis(2);

/// 会话交出 PTY 时还没写进 PTY 的输入。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Unwritten {
    /// 接手的一方当场写的。
    pub(crate) now: Vec<u8>,
    /// 还在等提示符的 `Inbox::TypeAtPrompt`：接手的一方接着等，不当场写。空的就是没有。
    pub(crate) at_prompt: Vec<u8>,
}

impl Unwritten {
    pub(crate) fn is_empty(&self) -> bool {
        self.now.is_empty() && self.at_prompt.is_empty()
    }
}

/// 会话对 `Inbox::Prepare` 的回话。
pub(crate) enum Prepared {
    /// 不交：shell 已经退出了，或者会话正被结束。交接提交时结束它，回滚时照旧留着。
    Gone,
    /// 交不了（比如复制不了 PTY 的描述符），整个交接放弃。
    Failed(String),
    Ready(Box<Exported>),
}

/// 会话交给新宿主的东西。
pub(crate) struct Exported {
    pub(crate) export: SessionExport,
    /// 复制的一份 PTY master，自己这份照常留着直到提交；还没启动 shell 时为空。交出方发出去以后
    /// 还留着它，直到新宿主确认接手。
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
}

/// 交接过来、已经启动了 shell 的会话。
pub(crate) struct Adopted {
    pub(crate) handoff: PtyHandoff,
    pub(crate) export: SessionExport,
    /// 用它重建 VT；为空（或者解不开）时用 `replay`。
    pub(crate) snapshot: Option<Vec<u8>>,
    pub(crate) replay: Vec<u8>,
    pub(crate) redactor: RedactorState,
    pub(crate) shell: Option<String>,
}

/// 接手交接过来的会话：PTY 停在闸门上（`Pty::adopt_paused`，不读不写），在会话线程里用快照
/// 重建 `HostSession`，快照没有或者解不开时用重放。不等重建完就返回，几个会话一起重建，见
/// `Adopting::finish`。闸门等 `Inbox::Open` 打开，不接手了发 `Inbox::Release`。
///
/// 失败时 PTY 已经原样交回、关掉了这边的描述符，shell 不受影响（交出方还拿着自己那份）。
/// PTY 在会话线程起好之后才经 channel 交过去：起不了线程时它还在这里，用 `Pty::release` 明着交回。
pub(crate) fn adopt(setup: Setup, adopted: Adopted) -> Result<Adopting> {
    let Setup { id, settings: _, env: _, ui, clipboard, read_patience, alive } = setup;
    let Adopted { handoff, export, snapshot, replay, redactor, shell } = adopted;
    // 交出时记着谁在操作它的，接手后照样记着：用户再打字时要清掉。
    let driven = Arc::new(Mutex::new(export.meta.driver.is_some()));
    let (inbox, rx) = mpsc::channel();
    let credits = Arc::new(Credits::default());
    let sink = pty_sink(&inbox, &credits);
    // 失败时交来的描述符随 `AdoptError` 丢掉，只关这边这份。
    let mut pty = Pty::adopt_paused(handoff, sink).map_err(|err| anyhow!("{err}"))?;
    let (give, take) = mpsc::channel::<Pty>();
    let (ready, created) = mpsc::channel();
    let killed = Arc::new(AtomicBool::new(false));
    let killed_flag = killed.clone();
    let desktop = Arc::new(AtomicU64::new(0));
    let runner_desktop = desktop.clone();
    let spawned = thread::Builder::new().name(format!("session-{id}")).spawn(move || {
        // 最后丢掉，排在 `Pty` 后面。
        let _alive = alive;
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
        let mut runner = Runner::new(id, session, shell, settings, credits);
        runner.redactor = ReportRedactor::from_state(redactor);
        runner.ui = ui;
        runner.desktop = runner_desktop;
        runner.set_clipboard(clipboard);
        runner.read_patience = read_patience;
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
    Ok(Adopting { handle: Handle { inbox, killed, driven, desktop }, created })
}

/// 正在重建的接手来的会话。
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

impl Runner {
    /// 交接（`Inbox::Prepare`）：停下来交出状态（`prepare`），之后冻结，直到交接提交
    /// （`Inbox::Release`：交出 PTY，线程结束）或者回滚（`Inbox::Resume`）。返回会话是否还要接着跑。
    ///
    /// 冻结期间 PTY 不读，程序之后的输出留在 PTY 里，归提交后的新宿主或者回滚后的自己。前端改会话
    /// 的请求（输入、改尺寸、换主题、清屏、启动 shell、连上来等）存进 `Freeze::held`：回滚时按先后
    /// 重放；提交时其中的输入编好、接在没写出去的输入后面交给新宿主，别的丢掉（发请求的连接都已经
    /// 收到 `Goodbye`）。只读的请求（列会话、读屏幕）照常办；断开当场放开订阅，它让出的尺寸归属
    /// （会改尺寸）也存下回滚时重放。不按时刻轮询 agent：读线程停着没有输出，拖久了会把工作中的
    /// agent 判成空闲。
    pub(super) fn hand_over(
        &mut self,
        reply: &mpsc::Sender<Prepared>,
        inbox: &mpsc::Receiver<Inbox>,
        killed: &AtomicBool,
    ) -> bool {
        let mut freeze = Freeze::default();
        let prepared = self.prepare(inbox, killed, &mut freeze);
        let _ = reply.send(prepared);
        loop {
            // 冻结期间存下的 `Subscribe` 也要回，排在收件箱里的前面。
            if killed.load(Ordering::Acquire) {
                self.ended();
                self.answer_pending(std::mem::take(&mut freeze.held), inbox);
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
    /// 复制一份 PTY master；还没启动的只交状态。期间别的消息照冻结时的规矩办。
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

    /// 处理交接期间收到的一条消息。
    fn frozen(&mut self, message: Inbox, freeze: &mut Freeze) {
        match message {
            message @ (Inbox::Pty(_) | Inbox::Info(_) | Inbox::ReadScreen { .. }) => self.handle(message),
            // 断开的连接当场放开（它的 `Outbox` 跟着订阅丢掉）；它让出的尺寸归属会改尺寸，和别的
            // 请求一样存下，回滚时重放。
            Inbox::Detach { connection } => {
                self.subscribers.retain(|s| s.connection != connection);
                freeze.held.push(Inbox::Detach { connection });
            }
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
    /// 接在后面；还在等提示符的 `TypeAtPrompt`（`held` 里的后来的替掉先前的）单独交出，
    /// 接手的一方接着等。还没启动 shell 的会话没有 PTY 可交，只有存下的输入。
    pub(super) fn release(&mut self, held: Vec<Inbox>) -> Unwritten {
        // 在等桌面回话的读剪贴板请求新宿主接不过去（它不知道在等什么，也不会替它计时），程序会一直等：交出
        // PTY 之前回它空的，回话写不出去的话随没写出去的输入一起交给新宿主。冻结期间存下的界面回话随
        // `held` 丢掉。
        if let Some(read) = self.clipboard_read.take() {
            tracing::info!("session {} answered a clipboard read with nothing: the host is handing over", self.id);
            self.finish_read(read, None);
        }
        let mut now = match self.session.release_pty() {
            // 丢掉交出来的 master 只关这边这份，新宿主手里还有一份。
            Ok(handoff) => handoff.pending_input,
            Err(err) => {
                tracing::debug!("session {} has no pty to release: {err:#}", self.id);
                Vec::new()
            }
        };
        let mut at_prompt = self.type_ahead.take().map(|(data, _)| data).unwrap_or_default();
        for message in held {
            let bytes = match message {
                Inbox::Input { data, .. } => Ok(data),
                Inbox::TypeAtPrompt(data) => {
                    at_prompt = data;
                    continue;
                }
                Inbox::Keys(keys) => self.session.encode_keys(&keys),
                Inbox::Paste(text) => self.session.encode_paste(&text),
                _ => continue,
            };
            match bytes {
                Ok(bytes) => now.extend_from_slice(&bytes),
                Err(err) => tracing::warn!("session {} drops input held during the handoff: {err:#}", self.id),
            }
        }
        Unwritten { now, at_prompt }
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
}

/// 交接期间冻结着的会话存下的东西。
#[derive(Default)]
struct Freeze {
    /// 冻结期间收到的、改会话的请求，按先后。
    held: Vec<Inbox>,
    /// 交接的结果：先到的那个算。
    decision: Option<Decision>,
}

enum Decision {
    Release(mpsc::Sender<Unwritten>),
    Resume,
}

#[cfg(test)]
mod tests {
    use runode_protocol::AttachMode;

    use super::*;
    use crate::session::{Screen, Subscribe, tests::runner};

    /// 交接冻结期间被结束：冻结时存下的 `Subscribe` 也要回，前端才收得到 `Attached`（和随后的 `Exited`），
    /// 不干等到超时。
    #[test]
    fn a_session_killed_while_frozen_answers_held_subscribes() {
        const WAIT: Duration = Duration::from_secs(10);
        let (tx, rx) = mpsc::channel();
        let killed = Arc::new(AtomicBool::new(false));
        let thread_killed = killed.clone();
        // `HostSession` 不是 `Send`，在会话的线程里建。
        let session = thread::spawn(move || runner(true).run(&rx, &thread_killed));
        let (prepared, ready) = mpsc::channel();
        tx.send(Inbox::Prepare(prepared)).unwrap();
        assert!(matches!(ready.recv_timeout(WAIT).unwrap(), Prepared::Ready(_)));
        let (started, screen) = mpsc::channel();
        let start = Box::new(move |_: Screen| {
            let _ = started.send(());
            None
        });
        let subscribe =
            Subscribe { connection: 1, size: None, mode: AttachMode::MetaOnly, start, desktop: false, device: None };
        tx.send(Inbox::Subscribe(subscribe)).unwrap();
        // 冻结期间照常回读状态的请求：回来了，排在它前面的 `Subscribe` 就已经存下了。
        let (info, answered) = mpsc::channel();
        tx.send(Inbox::Info(info)).unwrap();
        answered.recv_timeout(WAIT).unwrap();
        killed.store(true, Ordering::Release);
        tx.send(Inbox::Kill).unwrap();
        screen.recv_timeout(WAIT).expect("the held subscribe was not answered");
        session.join().unwrap();
    }

    /// 交出 PTY 时还在等提示符的 `TypeAtPrompt`（冻结期间存下的替掉先前的）不混进当场写的输入；接手的一方
    /// `Open` 后接着等提示符，不当场写。
    #[test]
    fn input_waiting_for_the_prompt_is_handed_over_apart() {
        let mut giver = runner(false);
        giver.handle(Inbox::TypeAtPrompt(b"old\r".to_vec()));
        let held = vec![
            Inbox::Input { connection: 1, data: b"ls\r".to_vec() },
            Inbox::TypeAtPrompt(b"agent\r".to_vec()),
            Inbox::Input { connection: 1, data: b"q".to_vec() },
        ];
        let unwritten = giver.release(held);
        assert_eq!(unwritten, Unwritten { now: b"ls\rq".to_vec(), at_prompt: b"agent\r".to_vec() });
        assert!(giver.type_ahead.is_none());

        let mut taker = runner(false);
        let before = Instant::now();
        taker.handle(Inbox::Open(unwritten));
        let (data, since) = taker.type_ahead.as_ref().expect("still waiting for the prompt");
        assert_eq!(data, b"agent\r");
        assert!(*since >= before, "the wait starts over when the session is taken over");
    }
}
