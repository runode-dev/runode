//! 交出会话的一方（旧宿主），见 `handoff` 的模块文档。在新宿主那条连接的读线程里跑：
//! 检查、清场、让会话停下来交出状态（`Inbox::Prepare`）、发出、等新宿主回话，然后提交或者回滚。

use std::{
    collections::HashMap,
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
    AtPrompt, BuildId, ClientMsg, FrameError, FrameKind, GoodbyeReason, HANDOFF_FORMAT, HandoffPart, HandoffPartError,
    HandoffRefusal, HostMsg, ReportToken, RunningCommand, SessionId, encode_part, read_frame,
};
use runode_terminal::{fd_passing, host_session::SessionExport, pty::PtyHandoff};
use runode_types::shell::IntegrationMode;

use super::{exits_in_grace, kill_peer, ms, peer_pid, running};
use crate::{
    Shared, SpawnOptions, Stopped,
    server::{ListenControl, Outbox},
    session::{self, Adopted, Exported, Handle, Inbox, Prepared, Unwritten},
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
    let Ready { handed, gone } = match prepare(&giving.handles) {
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
    // 接受连接的一方读到的对端进程号一直准，见 `peer_pid`；提交后它没确认时要靠它确认它不在了。
    let successor = peer_pid(stream);
    // 发出会话和等 `HandoffReady` 合起来最多 `deadline`：新宿主卡住不读时发送也会卡住，到期时
    // 看门狗杀掉它、断开连接，发送随之失败。
    let deadline = *shared.handoff_deadline.lock().unwrap_or_else(PoisonError::into_inner);
    let until = Instant::now() + deadline;
    let sent = match Watchdog::arm(stream, until) {
        Ok(watchdog) => {
            let sent = send_parts(shared, stream, &giving, handed.iter());
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
    // 复制的 master 都发出去了，这边的那份留到新宿主确认接手（`HandoffDone`）：提交时会话交出自己
    // 那份，新宿主这时死掉的话只剩它握着 PTY，见 `reclaim`。
    let kept = handed;
    let handed: Vec<SessionId> = kept.iter().map(|(id, _)| *id).collect();
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
    let Released { pending, late } = release(&giving.handles, &handed, RELEASE_TIMEOUT);
    {
        let mut registry = shared.registry();
        for id in &handed {
            registry.sessions.remove(id);
        }
    }
    let pending_bytes: usize = pending.iter().map(|(_, input)| input.now.len() + input.at_prompt.len()).sum();
    // 有会话没按时交出 PTY 时不发 `Commit`：它的写线程可能还在写，没写出去的输入也还没交回来，新宿主这时
    // 打开闸门就是两边一起写同一个 PTY。新宿主等不到 `Commit` 会放弃，下面 `reclaim` 把会话接回来。
    let commit_sent = late.is_empty() && {
        let sent = commit_part(&pending).map_err(|err| io::Error::other(err.to_string())).and_then(|data| {
            stream.set_write_timeout(Some(DONE_TIMEOUT))?;
            fd_passing::send_with_fds(stream, &data, &[])
        });
        if let Err(err) = &sent {
            tracing::error!("failed to send the commit to the new host: {err}");
        }
        sent.is_ok()
    };
    let done = commit_sent && wait_for_done(stream, reader);
    if !done && reclaim(shared, Unconfirmed { kept, pending, late, successor, commit_sent }) {
        giving.roll_back(shared, started, "the new host did not confirm taking the sessions over; took them back");
        return;
    }
    // shell 已经退出的会话不交，结束掉；回滚时照旧留着，所以等确认交出去了才结束。
    for id in &gone {
        shared.kill(*id);
    }
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
        if done { "" } else { "; the new host did not confirm and could not be stopped" },
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
    // 要接手的和自己是同一个构建：另一个新 app 抢先让这个构建的新宿主接手了，这是它拉起的宿主，
    // 后来的 app 的探测已经过时。按正在交接回话，那边过一会儿重新探，会连上这里。
    if asked.build == shared.build {
        return Err(HandoffRefusal::Busy);
    }
    if peers.has_desktop() {
        return Err(HandoffRefusal::DesktopConnected);
    }
    // 跑在 app 里的宿主不交：shell 是 app 的子进程、算在 app 名下（macOS 的责任进程），app 一退出，
    // 交出去的终端就连不上局域网（No route to host）。
    if !peers.standalone {
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
    peers.say_goodbye(Some(connection), &GoodbyeReason::Handoff);
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

/// 发 `HandoffPart::Host` 和各个会话的 `HandoffPart::Session`。交出的东西都留着，见 `reclaim`。
fn send_parts<'a>(
    shared: &Shared,
    stream: &UnixStream,
    giving: &Giving,
    handed: impl ExactSizeIterator<Item = &'a (SessionId, Box<Exported>)>,
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
        // 现在的宿主不记命令历史，也不看这一项。
        record_history: false,
        socket: giving.socket.clone(),
        clipboard,
        layout: shared.peers().layout.clone(),
    };
    send_part(stream, &host, &[], &[giving.listener.as_fd(), giving.lock.as_fd()])?;
    for (id, exported) in handed {
        let snapshot = exported.snapshot.as_deref().unwrap_or_default();
        let fds: Vec<BorrowedFd<'_>> = exported.master.iter().map(|fd| fd.as_fd()).collect();
        send_part(stream, &session_part(*id, exported), &[snapshot, &exported.replay], &fds)?;
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

/// 提交的 `HandoffPart::Commit`：各个会话没写进 PTY 的输入一块，还在等提示符的接在块末尾，长度记进
/// `at_prompt`。
pub(super) fn commit_part(pending: &[(SessionId, Unwritten)]) -> Result<Vec<u8>, HandoffPartError> {
    let pending_input = pending.iter().map(|(id, _)| *id).collect();
    let at_prompt = pending
        .iter()
        .filter(|(_, input)| !input.at_prompt.is_empty())
        .map(|(id, input)| AtPrompt { id: *id, len: u64::try_from(input.at_prompt.len()).unwrap_or(u64::MAX) })
        .collect();
    let blocks: Vec<Vec<u8>> =
        pending.iter().map(|(_, input)| [input.now.as_slice(), &input.at_prompt].concat()).collect();
    let blocks: Vec<&[u8]> = blocks.iter().map(Vec::as_slice).collect();
    encode_part(&HandoffPart::Commit { pending_input, at_prompt }, &blocks)
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

/// 等会话交出 PTY 的回话：交出时没写进 PTY 的输入。
type ReleaseReply = mpsc::Receiver<Unwritten>;

/// `release` 的结果。
struct Released {
    /// 交出了 PTY 的会话没写进 PTY 的输入，只列有的。
    pending: Vec<(SessionId, Unwritten)>,
    /// 没按时交出 PTY 的会话和等它回话的一端（线程已经结束了的不算没交：它的 PTY 随线程关掉了）。原来的
    /// 线程可能还握着 PTY，回滚时要先等它交出来，见 `settle`。
    late: Vec<(SessionId, ReleaseReply)>,
}

/// 提交：各个交出去的会话交出 PTY，最多等 `within`，返回它们没写进 PTY 的输入和没按时交的。
fn release(handles: &[(SessionId, Handle)], handed: &[SessionId], within: Duration) -> Released {
    let waiting: Vec<_> = handles
        .iter()
        .filter(|(id, _)| handed.contains(id))
        .filter_map(|(id, handle)| {
            let (reply, released) = mpsc::channel();
            handle.send(Inbox::Release(reply)).then_some((*id, released))
        })
        .collect();
    let deadline = Instant::now() + within;
    let mut released = Released { pending: Vec::new(), late: Vec::new() };
    for (id, reply) in waiting {
        match reply.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(input) if !input.is_empty() => released.pending.push((id, input)),
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::error!("session {id} did not release its pty in time");
                released.late.push((id, reply));
            }
            // 会话刚被结束（交接期间收到 `Kill`），线程没交就走了。
            Err(mpsc::RecvTimeoutError::Disconnected) => tracing::warn!("session {id} ended instead of releasing"),
        }
    }
    released
}

/// 提交后等新宿主回 `HandoffDone`，最多等 `DONE_TIMEOUT`。等到了返回 true；没回就断开（新宿主
/// 总是先回 `HandoffDone` 再断开，见 `Host::take_over`）、超时或者说了别的都是 false。
fn wait_for_done(stream: &UnixStream, reader: &mut BufReader<&UnixStream>) -> bool {
    if stream.set_read_timeout(Some(DONE_TIMEOUT)).is_err() {
        return false;
    }
    match read_frame(reader) {
        Ok(Some(frame)) if frame.kind == FrameKind::Control => {
            matches!(frame.message::<ClientMsg>(), Ok(ClientMsg::HandoffDone))
        }
        Ok(_) | Err(_) => false,
    }
}

/// 提交以后新宿主没确认接手时 `reclaim` 要的，各项见 `give`。
struct Unconfirmed {
    /// 交出去的会话的状态和这边留着的那份 master。
    kept: Vec<(SessionId, Box<Exported>)>,
    /// 按时交出了 PTY 的会话没写进 PTY 的输入。
    pending: Vec<(SessionId, Unwritten)>,
    /// 没按时交出 PTY 的会话，见 `Released::late`。
    late: Vec<(SessionId, ReleaseReply)>,
    /// 新宿主的进程号。
    successor: Option<libc::pid_t>,
    /// 发出了 `Commit`。
    commit_sent: bool,
}

/// 提交以后新宿主没确认接手（见 `wait_for_done`）：多半是它死了，或者没收到 `Commit`（包括这边有会话没按时
/// 交出 PTY、没发 `Commit`）、自己放弃了，
/// 这时 PTY 只剩这边留着的那份 master（`kept`）。先杀掉它（`successor`）、等它不在了，再用留着的
/// master 和交出去的状态把会话接回来，和新宿主接手一样（`session::adopt`），返回 true。认不出、
/// 杀不掉新宿主时不接回、返回 false：两个宿主一起读写同一个 PTY 比丢掉更糟。没按时交出 PTY 的会话
/// （`late`）原来的线程可能还握着 PTY、写线程还在写，再等它交出来（`settle`）才接回；还是不交的不接回，
/// 理由同上。
///
/// 接回的屏幕停在交出时，之后的输出还在 PTY 里；新宿主打开闸门后读走的那些丢了。没写进 PTY 的
/// 输入（`pending`）只在 `Commit` 没发出去（`commit_sent` 为假）时写回，还在等提示符的接着等：发出去了
/// 的话新宿主可能已经写了，再写一遍就重了。
fn reclaim(shared: &Shared, unconfirmed: Unconfirmed) -> bool {
    let Unconfirmed { kept, pending, late, successor, commit_sent } = unconfirmed;
    // SAFETY: 没有参数，总是成功。
    let own = unsafe { libc::getpid() };
    let Some(pid) = successor.filter(|pid| *pid > 1 && *pid != own) else {
        tracing::error!("the new host did not confirm the handoff and cannot be told apart; leaving the sessions");
        return false;
    };
    // SAFETY: 只发信号。
    if running(pid) && unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
        tracing::warn!("failed to kill the new host {pid}: {}", io::Error::last_os_error());
    }
    if !exits_in_grace(pid) {
        tracing::error!("the new host {pid} did not confirm the handoff and is still running; leaving the sessions");
        return false;
    }
    let mut pending: HashMap<SessionId, Unwritten> = if commit_sent {
        if !pending.is_empty() {
            tracing::warn!("dropped input the new host may already have written to {} sessions", pending.len());
        }
        HashMap::new()
    } else {
        pending.into_iter().collect()
    };
    // 有会话没按时交出 PTY 时没发 `Commit`，晚交出来的输入也写回。
    let stuck = settle(late, &mut pending, Instant::now() + RELEASE_TIMEOUT);
    for (id, exported) in kept {
        if stuck.contains(&id) {
            tracing::error!("session {id} still holds its pty; not taking it back");
            continue;
        }
        match take_back(shared, id, *exported) {
            Ok(handle) => {
                shared.registry().sessions.insert(id, handle.clone());
                handle.send(Inbox::Open(pending.remove(&id).unwrap_or_default()));
            }
            Err(err) => tracing::error!("cannot take session {id} back: {err:#}"),
        }
    }
    true
}

/// 等没按时交出 PTY 的会话（`late`）交出来，最多等到 `until`：交出时没写进 PTY 的输入放进 `pending`（这些
/// 会话本来不在里面），返回到时还没交的。线程没交就结束了的不算没交，同 `release`。
fn settle(
    late: Vec<(SessionId, ReleaseReply)>,
    pending: &mut HashMap<SessionId, Unwritten>,
    until: Instant,
) -> Vec<SessionId> {
    let mut stuck = Vec::new();
    for (id, reply) in late {
        match reply.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(input) if !input.is_empty() => {
                pending.insert(id, input);
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => stuck.push(id),
            Err(mpsc::RecvTimeoutError::Disconnected) => tracing::warn!("session {id} ended instead of releasing"),
        }
    }
    stuck
}

/// 用交出去的状态和留着的 master 把会话 `id` 重新建起来，PTY 停在闸门上，等 `Inbox::Open`。还没
/// 启动 shell 的没有 PTY，和新宿主一样重开一个等 `Inbox::Start`。
fn take_back(shared: &Shared, id: SessionId, exported: Exported) -> anyhow::Result<Handle> {
    let Exported { export, master, pid, snapshot, replay, redactor, shell } = exported;
    let setup = shared.setup(id, export.settings.clone());
    let (Some(master), Some(pid)) = (master, pid) else {
        let options = SpawnOptions {
            size: export.size,
            cwd: export.start_dir,
            integration: IntegrationMode::Off,
            start: false,
            shell,
            settings: None,
        };
        return session::spawn(setup, options);
    };
    let handoff = PtyHandoff {
        master,
        pid,
        size: export.size,
        report_token: export.report_token.clone(),
        pending_input: Vec::new(),
    };
    let adopted = Adopted { handoff, export, snapshot, replay, redactor, shell };
    Ok(session::adopt(setup, adopted)?.finish()?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没写进 PTY 的输入：`now` 当场写，`at_prompt` 是还在等提示符的。
    fn unwritten(now: &[u8], at_prompt: &[u8]) -> Unwritten {
        Unwritten { now: now.to_vec(), at_prompt: at_prompt.to_vec() }
    }

    /// 一个会话：收到 `Inbox::Release` 后，`gate` 为空时当场回 `input`；不为空时等测试从 `gate` 放行才回，
    /// 测试丢掉放行的一端时不回就走。什么时候回全由测试定，不靠睡多久。
    fn session(gate: Option<mpsc::Receiver<()>>, input: Unwritten) -> (Handle, thread::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            while let Ok(message) = rx.recv() {
                let Inbox::Release(reply) = message else { continue };
                if gate.is_none_or(|gate| gate.recv().is_ok()) {
                    let _ = reply.send(input);
                }
                return;
            }
        });
        (Handle::detached(tx), thread)
    }

    /// 没按时交出 PTY 的会话不当作没有输入丢掉：回滚时再等它交出来，晚交的输入照样写回，还在等提示符的
    /// 照样等；到时还不交的报出来、不接回，免得原来的线程和接回的会话一起读写同一个 PTY。
    #[test]
    fn a_late_release_is_waited_for_before_taking_the_session_back() {
        let ids = [SessionId(1), SessionId(2), SessionId(3)];
        let (open_late, late_gate) = mpsc::channel();
        let (keep_stuck, stuck_gate) = mpsc::channel::<()>();
        let sessions = [
            session(None, unwritten(b"on time", b"")),
            session(Some(late_gate), unwritten(b"late", b"agent\r")),
            session(Some(stuck_gate), unwritten(b"", b"")),
        ];
        let handles: Vec<(SessionId, Handle)> =
            ids.iter().zip(&sessions).map(|(id, (handle, _))| (*id, handle.clone())).collect();
        let Released { pending, late } = release(&handles, &ids, Duration::from_millis(500));
        assert_eq!(pending, vec![(ids[0], unwritten(b"on time", b""))]);
        assert_eq!(late.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [ids[1], ids[2]]);

        // 第二个这时才交，第三个一直不交，直到看过结果。
        open_late.send(()).unwrap();
        let mut pending: HashMap<SessionId, Unwritten> = pending.into_iter().collect();
        let stuck = settle(late, &mut pending, Instant::now() + Duration::from_millis(500));
        assert_eq!(stuck, [ids[2]]);
        assert_eq!(pending.get(&ids[1]), Some(&unwritten(b"late", b"agent\r")));
        drop(keep_stuck);
        for (_, thread) in sessions {
            thread.join().unwrap();
        }
    }
}
