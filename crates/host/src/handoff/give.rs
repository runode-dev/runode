//! 交出会话的一方（旧宿主），见 `handoff` 的模块文档。在新宿主那条连接的读线程里跑：
//! 检查、清场、让会话停下来交出状态（`Inbox::Prepare`）、发出、等新宿主回话，然后提交或者回滚。

use std::{
    fs::File,
    io::{self, BufReader},
    ops::RangeInclusive,
    os::{
        fd::{AsFd as _, BorrowedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{Arc, PoisonError, mpsc},
    thread,
    time::{Duration, Instant},
};

use runode_protocol::{
    BuildId, ClientMsg, FrameError, FrameKind, GoodbyeReason, HANDOFF_FORMAT, HandoffPart, HandoffRefusal, HostMsg,
    ReportToken, RunningCommand, SessionId, encode_part, read_frame,
};
use runode_terminal::{fd_passing, host_session::SessionExport};

use super::{kill_peer, ms};
use crate::{
    Shared, Stopped,
    server::{ListenControl, Outbox},
    session::{Exported, Handle, Inbox, Prepared},
};

/// 各个会话停下来交出状态最多等这么久；它们并行地停，每个最多花一秒多编快照。
pub(super) const PREPARE_TIMEOUT: Duration = Duration::from_secs(5);
/// 等写线程把 `HandoffBegin` 和之前的帧写完。
pub(super) const DETACH_TIMEOUT: Duration = Duration::from_secs(5);
/// 提交时等各个会话交出 PTY。新宿主等 `Commit` 的时限（`take::COMMIT_TIMEOUT`）要比它加上写
/// `Commit`（`DONE_TIMEOUT`）宽。
pub(super) const RELEASE_TIMEOUT: Duration = Duration::from_secs(5);
/// 写 `Commit` 最多这么久，发出后再等新宿主回 `HandoffDone` 也最多这么久。
pub(super) const DONE_TIMEOUT: Duration = Duration::from_secs(5);

/// 新宿主要接手（`ClientMsg::Handoff`）时说的。
pub(crate) struct Asked {
    /// 它读得了的交接格式。
    pub(crate) formats: RangeInclusive<u32>,
    /// 它的构建（`Hello` 里说的）。
    pub(crate) build: BuildId,
}

/// 新宿主要接手，见 `Asked`。`connection` 是这条连接的编号，`out` 是它的写队列，`reader` 读它。
/// 回滚时会话和监听都照旧，这条连接随后断开。
pub(crate) fn give(
    shared: &Arc<Shared>,
    connection: u64,
    out: &Outbox,
    reader: &mut BufReader<&UnixStream>,
    asked: &Asked,
) {
    let started = Instant::now();
    let giving = match begin(shared, connection, asked) {
        Ok(giving) => giving,
        Err(reason) => {
            tracing::info!("refused to hand the sessions over: {reason:?}");
            out.control(&HostMsg::HandoffRefused { reason });
            out.close();
            return;
        }
    };
    let ids: Vec<SessionId> = giving.handles.iter().map(|(id, _)| *id).collect();
    tracing::info!("handing {} sessions over to a new host", ids.len());

    // 各个会话并行地停下来、交出状态。
    let Ready { mut handed, gone } = match prepare(&giving.handles) {
        Ok(ready) => ready,
        Err(reason) => {
            out.control(&HostMsg::HandoffRefused { reason: HandoffRefusal::Unknown });
            out.close();
            giving.roll_back(shared, started, &format!("a session could not get ready: {reason}"));
            return;
        }
    };
    let prepared_at = Instant::now();
    let degraded = handed.iter().filter(|(_, e)| e.export.started && e.snapshot.is_none()).count();

    // 发出：`HandoffBegin` 走写线程，写完后写线程退出，之后在 socket 上直接发描述符消息。
    let sessions = u32::try_from(handed.len()).unwrap_or(u32::MAX);
    out.control(&HostMsg::HandoffBegin { format: HANDOFF_FORMAT, sessions });
    if out.detach().recv_timeout(DETACH_TIMEOUT).is_err() {
        giving.roll_back(shared, started, "the new host's connection broke before the handoff began");
        return;
    }
    let stream = *reader.get_ref();
    // 发出会话和等 `HandoffReady` 合起来最多 `deadline`：新宿主卡住不读时发送也会卡住，到期时
    // 看门狗杀掉它、断开连接，发送随之失败。
    let deadline = *shared.handoff_deadline.lock().unwrap_or_else(PoisonError::into_inner);
    let until = Instant::now() + deadline;
    let sent = match Watchdog::arm(stream, until) {
        Ok(watchdog) => {
            let sent = send_parts(shared, stream, &giving, handed.iter_mut());
            if watchdog.disarm() {
                Err(io::Error::new(io::ErrorKind::TimedOut, format!("the new host took more than {deadline:?}")))
            } else {
                sent
            }
        }
        Err(err) => Err(err),
    };
    if let Err(err) = sent {
        giving.roll_back(shared, started, &format!("failed to send the sessions: {err}"));
        return;
    }
    // 复制的 master 都发出去了，这边的关掉。
    let handed: Vec<SessionId> = handed.into_iter().map(|(id, _)| id).collect();
    let sent_at = Instant::now();

    match wait_for_ready(stream, reader, until) {
        Reply::Ready => {}
        Reply::Abort(reason) => {
            giving.roll_back(shared, started, &format!("the new host gave up: {reason}"));
            return;
        }
        Reply::Closed(reason) => {
            giving.roll_back(shared, started, &format!("the new host went away: {reason}"));
            return;
        }
        Reply::TimedOut => {
            kill_peer(stream);
            giving.roll_back(shared, started, &format!("the new host did not get ready within {deadline:?}"));
            return;
        }
    }
    let ready_at = Instant::now();

    // 提交：从这里起会话归新宿主。
    let pending = release(&giving.handles, &handed);
    {
        let mut registry = shared.registry();
        for id in &handed {
            registry.sessions.remove(id);
        }
    }
    // shell 已经退出的会话不交，结束掉。
    for id in &gone {
        shared.kill(*id);
    }
    let pending_bytes: usize = pending.iter().map(|(_, input)| input.len()).sum();
    let ids: Vec<SessionId> = pending.iter().map(|(id, _)| *id).collect();
    let blocks: Vec<&[u8]> = pending.iter().map(|(_, input)| input.as_slice()).collect();
    let sent = encode_part(&HandoffPart::Commit { pending_input: ids }, &blocks)
        .map_err(|err| io::Error::other(err.to_string()))
        .and_then(|data| {
            stream.set_write_timeout(Some(DONE_TIMEOUT))?;
            fd_passing::send_with_fds(stream, &data, &[])
        });
    if let Err(err) = sent {
        tracing::error!("failed to send the commit to the new host: {err}");
    }
    let done = wait_for_done(stream, reader);
    {
        let mut peers = shared.peers();
        peers.stop = Some(Stopped::Handoff);
        peers.accepting = false;
    }
    giving.control.stop();
    shared.peers_changed.notify_all();
    tracing::info!(
        "handed {} sessions over in {} ms ({} sent as a VT replay only, {} exited ones ended, {} bytes of input \
         not written yet; prepare {} ms, send {} ms, wait for ready {} ms, commit {} ms){}",
        handed.len(),
        ms(started.elapsed()),
        degraded,
        gone.len(),
        pending_bytes,
        ms(prepared_at - started),
        ms(sent_at - prepared_at),
        ms(ready_at - sent_at),
        ms(ready_at.elapsed()),
        if done { "" } else { "; the new host did not confirm" },
    );
}

/// 正在交出的会话和监听的 socket。
struct Giving {
    control: Arc<ListenControl>,
    /// 交给新宿主的监听 socket 和锁（各是复制的一份描述符，和自己用的是同一个打开的文件）。
    listener: Arc<UnixListener>,
    lock: File,
    socket: PathBuf,
    /// 开始交接时的全部会话，按标识排好。
    handles: Vec<(SessionId, Handle)>,
}

impl Giving {
    /// 回滚：各个会话接着读、重放冻结期间存下的请求，接着接受连接。
    fn roll_back(&self, shared: &Shared, started: Instant, why: &str) {
        for (_, handle) in &self.handles {
            handle.send(Inbox::Resume);
        }
        shared.peers().handoff = None;
        self.control.resume();
        shared.peers_changed.notify_all();
        tracing::warn!("the handoff was rolled back after {} ms: {why}", ms(started.elapsed()));
    }
}

/// 检查能不能交，能交就清场：记下正在交接、停下接受连接、给别的连接发 `Goodbye`，定下要交的
/// 会话。都在同一把锁里，之后新开的会话（见 `Shared::spawn`）和新连上的连接（见 `serve`）都
/// 看得到正在交接。
fn begin(shared: &Shared, connection: u64, asked: &Asked) -> Result<Giving, HandoffRefusal> {
    let mut peers = shared.peers();
    // 跑在 app 里、app 要退出了（`Host::yield_on_quit`）：来接手的是这个 app 刚拉起的同构建
    // 宿主，连着的界面就是这个 app 自己，下面三条都不拦。
    let yielding = peers.yielding;
    // 要接手的和自己是同一个构建：另一个新 app 抢先让这个构建的新宿主接手了，这是它拉起的宿主，
    // 后来的 app 的探测已经过时。按正在交接回话，那边过一会儿重新探，会连上这里。
    if asked.build == shared.build && !yielding {
        return Err(HandoffRefusal::Busy);
    }
    if peers.has_desktop() && !yielding {
        return Err(HandoffRefusal::DesktopConnected);
    }
    if !peers.standalone && !yielding {
        return Err(HandoffRefusal::NotStandalone);
    }
    if peers.handoff.is_some() || peers.stop.is_some() {
        return Err(HandoffRefusal::Busy);
    }
    if !asked.formats.contains(&HANDOFF_FORMAT) {
        return Err(HandoffRefusal::UnsupportedFormat { writes: HANDOFF_FORMAT });
    }
    let Some(listening) = &peers.listening else {
        // 单独跑的宿主总是在监听；没有 socket 可交时当作不是单独跑的。
        return Err(HandoffRefusal::NotStandalone);
    };
    let lock = listening.lock.try_clone().map_err(|err| {
        tracing::warn!("cannot hand the lock over: {err}");
        HandoffRefusal::Unknown
    })?;
    let control = listening.control.clone();
    let listener = listening.listener.clone();
    let socket = listening.socket.clone();
    peers.handoff = Some(connection);
    // 返回时接受连接的线程已经不会再接，之后连上来的排在 backlog 里，留给新宿主。
    control.pause();
    // app 退出前交出会话时，它自己的界面不用走：交接没成时它还得接着用这些会话。
    peers.say_goodbye(Some(connection), yielding, &GoodbyeReason::Handoff);
    let handles = shared.handles();
    Ok(Giving { control, listener, lock, socket, handles })
}

/// 各个会话停下来以后。
struct Ready {
    /// 要交出去的会话和它们交出的东西。
    handed: Vec<(SessionId, Box<Exported>)>,
    /// 不交的：shell 已经退出了。
    gone: Vec<SessionId>,
}

/// 让各个会话停下来交出状态，并行地等它们回话。会话线程已经结束了的不在结果里。有会话交不了
/// 或者超时时返回原因，整个交接放弃。
fn prepare(handles: &[(SessionId, Handle)]) -> Result<Ready, String> {
    let waiting: Vec<_> = handles
        .iter()
        .filter_map(|(id, handle)| {
            let (reply, prepared) = mpsc::channel();
            handle.send(Inbox::Prepare(reply)).then_some((*id, prepared))
        })
        .collect();
    let deadline = Instant::now() + PREPARE_TIMEOUT;
    let mut ready = Ready { handed: Vec::with_capacity(waiting.len()), gone: Vec::new() };
    for (id, reply) in waiting {
        match reply.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Prepared::Ready(exported)) => ready.handed.push((id, exported)),
            Ok(Prepared::Gone) => ready.gone.push(id),
            Ok(Prepared::Failed(reason)) => return Err(reason),
            Err(mpsc::RecvTimeoutError::Timeout) => return Err(format!("session {id} did not answer in time")),
            // 线程已经结束了：会话刚被结束，不交也不用管。
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
    }
    Ok(ready)
}

/// 发 `HandoffPart::Host` 和各个会话的 `HandoffPart::Session`。
fn send_parts<'a>(
    shared: &Shared,
    stream: &UnixStream,
    giving: &Giving,
    handed: impl ExactSizeIterator<Item = &'a mut (SessionId, Box<Exported>)>,
) -> io::Result<()> {
    let (theme, clipboard) = {
        let registry = shared.registry();
        ((registry.theme_generation > 0).then(|| (*registry.settings).clone()), registry.clipboard)
    };
    let host = HandoffPart::Host {
        format: HANDOFF_FORMAT,
        build: shared.build.clone(),
        snapshot_format: shared.snapshot_format,
        sessions: u32::try_from(handed.len()).unwrap_or(u32::MAX),
        theme,
        record_history: shared.record_history.load(std::sync::atomic::Ordering::Relaxed),
        socket: giving.socket.clone(),
        clipboard,
    };
    send_part(stream, &host, &[], &[giving.listener.as_fd(), giving.lock.as_fd()])?;
    for (id, exported) in handed {
        let snapshot = exported.snapshot.take().unwrap_or_default();
        let replay = std::mem::take(&mut exported.replay);
        let master = exported.master.take();
        let fds: Vec<BorrowedFd<'_>> = master.iter().map(|fd| fd.as_fd()).collect();
        send_part(stream, &session_part(*id, exported), &[&snapshot, &replay], &fds)?;
    }
    Ok(())
}

fn send_part(stream: &UnixStream, part: &HandoffPart, blocks: &[&[u8]], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    let data = encode_part(part, blocks).map_err(|err| io::Error::other(err.to_string()))?;
    fd_passing::send_with_fds(stream, &data, fds)
}

/// 会话交出的状态写成 `HandoffPart::Session`。
fn session_part(id: SessionId, exported: &Exported) -> HandoffPart {
    let SessionExport {
        meta,
        size,
        started,
        report_token,
        settings,
        start_dir,
        prompt_reported,
        running,
        pending_shell_cwd,
        pending_command,
    } = exported.export.clone();
    HandoffPart::Session {
        id,
        started,
        pid: exported.pid,
        size,
        report_token: report_token.map(ReportToken),
        settings,
        shell: exported.shell.clone(),
        start_dir,
        meta: Box::new(meta),
        prompt_reported,
        running: running.map(|entry| RunningCommand { cmd: entry.cmd, cwd: entry.cwd, ts: entry.ts }),
        pending_shell_cwd,
        pending_command,
        redactor: runode_protocol::RedactorState {
            matched: exported.redactor.matched,
            inside: exported.redactor.inside,
        },
    }
}

/// 新宿主对交过去的会话怎么说。
enum Reply {
    Ready,
    Abort(String),
    Closed(String),
    TimedOut,
}

/// 等新宿主回 `HandoffReady` 或者 `HandoffAbort`，最多等到 `until`。
fn wait_for_ready(stream: &UnixStream, reader: &mut BufReader<&UnixStream>, until: Instant) -> Reply {
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Reply::TimedOut;
        }
        if let Err(err) = stream.set_read_timeout(Some(left)) {
            return Reply::Closed(format!("cannot wait for it: {err}"));
        }
        match read_frame(reader) {
            Ok(Some(frame)) if frame.kind == FrameKind::Control => match frame.message::<ClientMsg>() {
                Ok(ClientMsg::HandoffReady) => return Reply::Ready,
                Ok(ClientMsg::HandoffAbort { reason }) => return Reply::Abort(reason),
                Ok(other) => return Reply::Closed(format!("unexpected {other:?}")),
                Err(err) => return Reply::Closed(format!("unreadable message: {err}")),
            },
            Ok(Some(frame)) => return Reply::Closed(format!("unexpected {:?} frame", frame.kind)),
            Ok(None) => return Reply::Closed("the connection closed".into()),
            Err(FrameError::Io(err)) if matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(err) => return Reply::Closed(err.to_string()),
        }
    }
}

/// 发会话时的看门狗：到期还没撤掉就杀掉新宿主、断开连接，卡在发送上的那边随之失败。
struct Watchdog {
    disarm: mpsc::Sender<()>,
    thread: thread::JoinHandle<bool>,
}

impl Watchdog {
    fn arm(stream: &UnixStream, until: Instant) -> io::Result<Self> {
        let stream = stream.try_clone()?;
        let (disarm, disarmed) = mpsc::channel();
        let thread = thread::Builder::new().name("handoff-watchdog".into()).spawn(move || {
            match disarmed.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    kill_peer(&stream);
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    true
                }
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => false,
            }
        })?;
        Ok(Self { disarm, thread })
    }

    /// 撤掉，返回它是不是已经到期动手了。
    fn disarm(self) -> bool {
        let _ = self.disarm.send(());
        self.thread.join().unwrap_or(true)
    }
}

/// 提交：各个交出去的会话交出 PTY，返回它们没写进 PTY 的输入（只列有的）。没按时回话的当作没有。
fn release(handles: &[(SessionId, Handle)], handed: &[SessionId]) -> Vec<(SessionId, Vec<u8>)> {
    let waiting: Vec<_> = handles
        .iter()
        .filter(|(id, _)| handed.contains(id))
        .filter_map(|(id, handle)| {
            let (reply, released) = mpsc::channel();
            handle.send(Inbox::Release(reply)).then_some((*id, released))
        })
        .collect();
    let deadline = Instant::now() + RELEASE_TIMEOUT;
    let mut pending = Vec::new();
    for (id, released) in waiting {
        match released.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(input) if !input.is_empty() => pending.push((id, input)),
            Ok(_) => {}
            Err(err) => tracing::warn!("session {id} did not release its pty: {err}"),
        }
    }
    pending
}

/// 提交后等新宿主回 `HandoffDone`，它断开也算；最多等 `DONE_TIMEOUT`。等到了返回 true。
fn wait_for_done(stream: &UnixStream, reader: &mut BufReader<&UnixStream>) -> bool {
    if stream.set_read_timeout(Some(DONE_TIMEOUT)).is_err() {
        return false;
    }
    match read_frame(reader) {
        Ok(Some(frame)) if frame.kind == FrameKind::Control => {
            matches!(frame.message::<ClientMsg>(), Ok(ClientMsg::HandoffDone))
        }
        Ok(None) => true,
        Ok(Some(_)) | Err(_) => false,
    }
}
