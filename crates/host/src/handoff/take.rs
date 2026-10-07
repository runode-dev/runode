//! 接手会话的一方（新宿主），入口是 `Host::take_over`，见 `handoff` 的模块文档。

use std::{
    collections::HashMap,
    fs::File,
    io,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering, mpsc},
    thread,
    time::{Duration, Instant},
};

use runode_protocol::{
    BuildId, Caps, ClientKind, ClientMsg, Frame, FrameKind, GoodbyeReason, HANDOFF_FORMAT, HandoffPart, HandoffRefusal,
    HostMsg, OLDEST_READABLE_HANDOFF_FORMAT, PROTOCOL_VERSION, SessionId, decode_part, read_frame, write_frame,
};
use runode_shared_types::{clipboard::ClipboardAccess, settings::TermSettings, shell::IntegrationMode};
use runode_terminal::{
    fd_passing, history,
    host_session::{RedactorState, SessionExport},
    pty::PtyHandoff,
};

use super::{TakeOverError, TakeOverOptions, TakeOverReport, ms, peer_pid, running};
use crate::{
    Host, Shared, SpawnOptions,
    server::ListenControl,
    session::{self, Adopted, Handle, Inbox},
};

/// 握手和收会话时，旧宿主一次最多这么久不说话。它让会话停下来最多花五秒多，大的回滚历史编码、
/// 发送也要一会儿。
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(25);
/// 发出 `HandoffReady` 后最多等旧宿主这么久发 `Commit`。旧宿主收到 `HandoffReady` 后只是让会话
/// 交出 PTY（`give::RELEASE_TIMEOUT`）、写出 `Commit`（`give::DONE_TIMEOUT`），要比这两个之和宽。
/// 算进 `TAKE_OVER_AFTER_READY`。
pub(super) const COMMIT_TIMEOUT: Duration = Duration::from_secs(30);
/// 旧宿主没发 `Commit` 就断开时，最多等它这么久退出完，再判断它是死了还是回滚了。
pub(super) const EXIT_GRACE: Duration = Duration::from_secs(2);
/// 不接手了时等各个会话交回 PTY 最多这么久。
pub(super) const RELEASE_TIMEOUT: Duration = Duration::from_secs(5);

const _: () = assert!(
    COMMIT_TIMEOUT.as_millis() > super::give::RELEASE_TIMEOUT.as_millis() + super::give::DONE_TIMEOUT.as_millis()
);

impl Host {
    /// 从 `socket` 上单独跑着的旧宿主手里接过所有会话、监听的 socket 和锁：连上去说自己是
    /// `ClientKind::Successor`，要它交接，收下会话（PTY 停着，屏幕按快照或者重放重建），回
    /// `HandoffReady`；旧宿主提交后打开各个会话的 PTY、开始在同一个 socket 上接受连接，回
    /// `HandoffDone`。之后照常 `Host::run_until_idle`。
    ///
    /// 这个 `Host` 要是新建的：还没有会话、没在监听。失败时旧宿主照旧跑着、会话都在它手里，这里
    /// 也不留任何状态（收下的 PTY 原样交回、监听的 socket 只关这边这份）。只有一种情况在
    /// `HandoffReady` 之后：旧宿主没发 `Commit` 就断开、但进程还在跑（它回滚了），返回 `Failed`；
    /// 它已经死了的话当作提交了，照样接手（它手里没写出去的输入丢了）。
    pub fn take_over(&self, socket: &Path, options: TakeOverOptions) -> Result<TakeOverReport, TakeOverError> {
        let started = Instant::now();
        {
            let peers = self.shared.peers();
            if peers.listening.is_some() || peers.handoff.is_some() {
                return Err(TakeOverError::Failed("this host is already listening".into()));
            }
        }
        if !self.shared.registry().sessions.is_empty() {
            return Err(TakeOverError::Failed("this host already has sessions".into()));
        }
        let stream = UnixStream::connect(socket)
            .map_err(|err| TakeOverError::Failed(format!("cannot connect to {}: {err}", socket.display())))?;
        stream
            .set_read_timeout(Some(RECEIVE_TIMEOUT))
            .map_err(|err| TakeOverError::Failed(format!("cannot set up the connection: {err}")))?;
        let (sessions, old_pid) = handshake(&self.shared, &stream)?;
        let begun_at = Instant::now();
        let our_format = options.snapshot_format.unwrap_or(self.shared.snapshot_format);
        let received = match receive(&self.shared, &stream, sessions, our_format) {
            Ok(received) => received,
            Err(reason) => return Err(abort(&stream, reason, started)),
        };
        let received_at = Instant::now();
        let (mut taken, listener, lock) = match received.finish() {
            Ok(taken) => taken,
            Err(reason) => return Err(abort(&stream, reason, started)),
        };
        let imported_at = Instant::now();
        // 接受连接的线程先起好、停着，提交以后才放开；在那之前来的连接排在 backlog 里。
        let control = match self.start_listening(listener, lock, &taken.socket, true) {
            Ok(control) => control,
            Err(err) => {
                release_all(&taken.handles);
                return Err(abort(&stream, format!("cannot listen on the socket: {err:#}"), started));
            }
        };
        // 拉起这边的一方等结果有时限：过了 `ready_by` 它已经当作失败了，不能再接手成功。
        if options.ready_by.is_some_and(|at| Instant::now() >= at) {
            self.stop_listening(&control);
            release_all(&taken.handles);
            return Err(abort(&stream, "took too long to get ready".into(), started));
        }
        if let Some(on_ready) = &options.on_ready {
            on_ready();
        }
        if let Err(err) = send(&stream, &ClientMsg::HandoffReady) {
            self.stop_listening(&control);
            release_all(&taken.handles);
            return Err(TakeOverError::Failed(format!("cannot tell the old host we are ready: {err}")));
        }

        // 提交点在旧宿主那边：等它的 `Commit`。
        let pending = match wait_for_commit(&stream, old_pid) {
            Ok(pending) => pending,
            Err(reason) => {
                self.stop_listening(&control);
                release_all(&taken.handles);
                tracing::warn!("the handoff failed after {} ms: {reason}", ms(started.elapsed()));
                return Err(TakeOverError::Failed(reason));
            }
        };
        let committed_at = Instant::now();
        self.set_env(runode_protocol::ENV_SOCKET, taken.socket.as_os_str());
        self.commit(&mut taken, pending);
        control.resume();
        if let Err(err) = send(&stream, &ClientMsg::HandoffDone) {
            tracing::debug!("cannot tell the old host the handoff is done: {err}");
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
        let report = TakeOverReport { sessions: taken.handles.len(), replayed: taken.replayed };
        tracing::info!(
            "took {} sessions over in {} ms ({} rebuilt from a VT replay{}; handshake and the old host getting \
             ready {} ms, receive {} ms, rebuild {} ms, wait for the commit {} ms, open {} ms)",
            report.sessions,
            ms(started.elapsed()),
            report.replayed.len(),
            if report.replayed.is_empty() { String::new() } else { format!(": {:?}", report.replayed) },
            ms(begun_at - started),
            ms(received_at - begun_at),
            ms(imported_at - received_at),
            ms(committed_at - imported_at),
            ms(committed_at.elapsed()),
        );
        Ok(report)
    }

    /// 旧宿主提交了：登记会话，套用旧宿主的主题和选项（剪贴板的规矩也交给各个会话，它们开出来时
    /// 拿的是默认的），把它没写进 PTY 的输入排进各个会话的写队列，打开各个会话的闸门。
    fn commit(&self, taken: &mut Taken, mut pending: HashMap<SessionId, Vec<u8>>) {
        let shared = &self.shared;
        shared.record_history.store(taken.record_history, Ordering::Relaxed);
        {
            let mut registry = shared.registry();
            if let Some(theme) = taken.theme.take() {
                registry.settings = Arc::new(theme);
                registry.theme_generation += 1;
            }
            registry.clipboard = taken.clipboard;
            for (id, handle) in &taken.handles {
                handle.send(Inbox::Clipboard(taken.clipboard));
                registry.sessions.insert(*id, handle.clone());
            }
        }
        for (id, handle) in &taken.handles {
            handle.send(Inbox::Open(pending.remove(id).unwrap_or_default()));
        }
        for id in pending.keys() {
            tracing::warn!("dropped input for session {id}, which was not handed over");
        }
    }

    /// 不接手了：停下接受连接的线程、撤掉监听，不碰 socket 文件（它还是旧宿主的）。
    fn stop_listening(&self, control: &ListenControl) {
        control.stop();
        self.shared.peers().listening = None;
    }
}

/// 握手：说自己是接手的新宿主，要旧宿主交接。返回要接手几个会话和旧宿主的进程号。
///
/// 旧宿主正在把会话交给别的新宿主时，这条连接收到的是 `Goodbye { Handoff }`（交接开始前连上、
/// 或者交接期间被接受的连接都这样）：和它回 `HandoffRefused { Busy }` 一样，当作别人正在交接。
fn handshake(shared: &Shared, stream: &UnixStream) -> Result<(u32, Option<libc::pid_t>), TakeOverError> {
    let failed = |what: &str, err: &dyn std::fmt::Display| TakeOverError::Failed(format!("{what}: {err}"));
    send(
        stream,
        &ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            build: BuildId(shared.build.0.clone()),
            caps: Caps::default(),
            client: ClientKind::Successor,
            session: None,
            device: None,
        },
    )
    .map_err(|err| failed("cannot greet the old host", &err))?;
    let old_pid = match receive_message(stream).map_err(|err| failed("no answer from the old host", &err))? {
        HostMsg::Welcome { handoff: 0, .. } => return Err(TakeOverError::PreHandoff),
        HostMsg::Welcome { host_pid, .. } => old_host_pid(stream, host_pid),
        // 协议 3 及更早的宿主不认识 `Successor`，按协议版本回 `Incompatible`。
        HostMsg::Incompatible { protocol, .. } if protocol <= 3 => return Err(TakeOverError::PreHandoff),
        HostMsg::Goodbye { reason: GoodbyeReason::Handoff } => {
            return Err(TakeOverError::Refused(HandoffRefusal::Busy));
        }
        other => return Err(TakeOverError::Failed(format!("unexpected answer to hello: {other:?}"))),
    };
    let handoff = ClientMsg::Handoff { min_format: OLDEST_READABLE_HANDOFF_FORMAT, max_format: HANDOFF_FORMAT };
    send(stream, &handoff).map_err(|err| failed("cannot ask for the handoff", &err))?;
    match receive_message(stream).map_err(|err| failed("no answer to the handoff", &err))? {
        HostMsg::HandoffRefused { reason } => Err(TakeOverError::Refused(reason)),
        HostMsg::Goodbye { reason: GoodbyeReason::Handoff } => Err(TakeOverError::Refused(HandoffRefusal::Busy)),
        HostMsg::HandoffBegin { format, sessions }
            if (OLDEST_READABLE_HANDOFF_FORMAT..=HANDOFF_FORMAT).contains(&format) =>
        {
            Ok((sessions, old_pid))
        }
        HostMsg::HandoffBegin { format, .. } => {
            let reason = format!("the old host writes handoff format {format}");
            let _ = send(stream, &ClientMsg::HandoffAbort { reason: reason.clone() });
            Err(TakeOverError::Failed(reason))
        }
        other => Err(TakeOverError::Failed(format!("unexpected answer to the handoff: {other:?}"))),
    }
}

/// 旧宿主的进程号：用它在 `Welcome` 里自报的 `reported`，收到 `Welcome` 以后再读一次连接对端
/// （`peer_pid`，这时它已经 accept 了这条连接）对照，对不上时记一笔、仍以自报的为准。连上时就读
/// 的对端进程号不准：监听的 socket 交接过以后，读到的是最早建它的那个宿主，见 `peer_pid`。
fn old_host_pid(stream: &UnixStream, reported: u32) -> Option<libc::pid_t> {
    let reported = libc::pid_t::try_from(reported).ok().filter(|pid| *pid > 0);
    let peer = peer_pid(stream);
    match (reported, peer) {
        (Some(reported), Some(peer)) if reported != peer => {
            tracing::warn!(
                "the old host says it is process {reported}, the connection says {peer}; going with {reported}"
            );
            Some(reported)
        }
        (Some(reported), _) => Some(reported),
        (None, peer) => peer,
    }
}

/// 收下的东西，会话还在重建。
struct Received {
    host: HostState,
    /// 已经启动了 shell 的会话在重建，没启动的已经建好。
    adopting: Vec<(SessionId, session::Adopting)>,
    rebuilt: Vec<(SessionId, Handle)>,
}

/// `HandoffPart::Host` 带来的。
struct HostState {
    listener: UnixListener,
    lock: File,
    socket: PathBuf,
    theme: Option<TermSettings>,
    record_history: bool,
    clipboard: ClipboardAccess,
}

/// 接手了的会话，PTY 都停在闸门上。
struct Taken {
    socket: PathBuf,
    theme: Option<TermSettings>,
    record_history: bool,
    clipboard: ClipboardAccess,
    handles: Vec<(SessionId, Handle)>,
    replayed: Vec<SessionId>,
}

impl Received {
    /// 等所有会话重建完，连同监听的 socket 和锁一起返回。有一个不成就把已经接手的都交回去，
    /// 返回原因。
    fn finish(self) -> Result<(Taken, UnixListener, File), String> {
        let Received { host, adopting, rebuilt } = self;
        let mut handles = rebuilt;
        let mut replayed = Vec::new();
        let mut failure = None;
        for (id, adopting) in adopting {
            match adopting.finish() {
                Ok((handle, from_replay)) => {
                    if from_replay {
                        replayed.push(id);
                    }
                    handles.push((id, handle));
                }
                Err(err) => {
                    failure.get_or_insert_with(|| format!("cannot take session {id} over: {err:#}"));
                }
            }
        }
        if let Some(reason) = failure {
            release_all(&handles);
            return Err(reason);
        }
        handles.sort_by_key(|(id, _)| *id);
        let HostState { listener, lock, socket, theme, record_history, clipboard } = host;
        Ok((Taken { socket, theme, record_history, clipboard, handles, replayed }, listener, lock))
    }
}

/// 收 `HandoffPart::Host` 和 `sessions` 条 `HandoffPart::Session`，一边收一边接手。出错时已经
/// 接手的都交回去，返回原因。
fn receive(shared: &Shared, stream: &UnixStream, sessions: u32, our_format: u16) -> Result<Received, String> {
    let (data, fds) = fd_passing::recv_with_fds(stream).map_err(|err| format!("cannot receive the host: {err}"))?;
    let (part, _) = decode_part(&data).map_err(|err| format!("cannot read the host: {err}"))?;
    let HandoffPart::Host {
        format,
        snapshot_format,
        sessions: count,
        theme,
        record_history,
        socket,
        build,
        clipboard,
        ..
    } = part
    else {
        return Err("the old host did not start with itself".into());
    };
    if !(OLDEST_READABLE_HANDOFF_FORMAT..=HANDOFF_FORMAT).contains(&format) || count != sessions {
        return Err(format!("the old host's handoff is inconsistent: format {format}, {count} of {sessions} sessions"));
    }
    let [listener, lock] = <[_; 2]>::try_from(fds)
        .map_err(|fds| format!("the old host sent {} descriptors with itself, expected 2", fds.len()))?;
    tracing::info!("taking {sessions} sessions over from host {} (snapshot format {snapshot_format})", build.0);
    let use_snapshots = snapshot_format == our_format && our_format != 0;
    let mut received = Received {
        host: HostState {
            listener: UnixListener::from(listener),
            lock: File::from(lock),
            socket,
            theme,
            record_history,
            clipboard,
        },
        adopting: Vec::new(),
        rebuilt: Vec::new(),
    };
    for _ in 0..sessions {
        if let Err(reason) = receive_session(shared, stream, use_snapshots, &mut received) {
            let Received { adopting, rebuilt, .. } = received;
            let mut handles = rebuilt;
            handles.extend(adopting.into_iter().filter_map(|(id, adopting)| Some((id, adopting.finish().ok()?.0))));
            release_all(&handles);
            return Err(reason);
        }
    }
    Ok(received)
}

/// 收一条 `HandoffPart::Session`，接手它：已经启动了的 PTY 停在闸门上、在会话线程里重建屏幕，
/// 没启动的另开一个伪终端重建。
fn receive_session(
    shared: &Shared,
    stream: &UnixStream,
    use_snapshots: bool,
    received: &mut Received,
) -> Result<(), String> {
    let (data, fds) = fd_passing::recv_with_fds(stream).map_err(|err| format!("cannot receive a session: {err}"))?;
    let (part, blocks) = decode_part(&data).map_err(|err| format!("cannot read a session: {err}"))?;
    let HandoffPart::Session {
        id,
        started,
        pid,
        size,
        report_token,
        settings,
        shell,
        start_dir,
        meta,
        prompt_reported,
        running,
        pending_shell_cwd,
        pending_command,
        redactor,
    } = part
    else {
        return Err("the old host sent something other than a session".into());
    };
    let mut setup = shared.setup(id, settings.clone());
    if !started {
        // 宿主的 `ENV_SOCKET` 提交时才设（失败时不留状态），这个会话启动 shell 时先带上。
        let socket = received.host.socket.as_os_str().to_owned();
        setup.env.retain(|(key, _)| key != runode_protocol::ENV_SOCKET);
        setup.env.push((runode_protocol::ENV_SOCKET.into(), socket));
        if !fds.is_empty() {
            return Err(format!("session {id} has not started but came with a pty"));
        }
        let options = SpawnOptions {
            size,
            cwd: start_dir,
            integration: IntegrationMode::Off,
            start: false,
            shell,
            settings: None,
        };
        let handle = session::spawn(setup, options).map_err(|err| format!("cannot rebuild session {id}: {err:#}"))?;
        received.rebuilt.push((id, handle));
        return Ok(());
    }
    let (Some(pid), Ok([master])) = (pid, <[_; 1]>::try_from(fds)) else {
        return Err(format!("session {id} came without its pty or shell"));
    };
    let report_token = report_token.map(|token| token.0);
    let export = SessionExport {
        meta: *meta,
        size,
        started,
        report_token: report_token.clone(),
        settings,
        start_dir,
        prompt_reported,
        running: running.map(|command| history::Entry {
            cmd: command.cmd,
            cwd: command.cwd,
            exit: None,
            ts: command.ts,
        }),
        pending_shell_cwd,
        pending_command,
    };
    let snapshot = blocks.first().filter(|snapshot| use_snapshots && !snapshot.is_empty()).map(|block| block.to_vec());
    let replay = blocks.get(1).map(|block| block.to_vec()).unwrap_or_default();
    let adopted = Adopted {
        handoff: PtyHandoff { master, pid, size, report_token, pending_input: Vec::new() },
        export,
        snapshot,
        replay,
        redactor: RedactorState { matched: redactor.matched, inside: redactor.inside },
        shell,
    };
    let adopting = session::adopt(setup, adopted).map_err(|err| format!("cannot take session {id} over: {err:#}"))?;
    received.adopting.push((id, adopting));
    Ok(())
}

/// `HandoffReady` 之前出错：告诉旧宿主不接手了（它随即回滚），返回失败。
fn abort(stream: &UnixStream, reason: String, started: Instant) -> TakeOverError {
    tracing::warn!("the handoff failed after {} ms: {reason}", ms(started.elapsed()));
    if let Err(err) = send(stream, &ClientMsg::HandoffAbort { reason: reason.clone() }) {
        tracing::debug!("cannot tell the old host to roll back: {err}");
    }
    TakeOverError::Failed(reason)
}

/// 等旧宿主的 `Commit`，返回它没写进 PTY 的输入。它没发就断开时：进程已经不在了当作提交了
/// （输入丢了），还在跑就是回滚了，返回原因。
fn wait_for_commit(stream: &UnixStream, old_pid: Option<libc::pid_t>) -> Result<HashMap<SessionId, Vec<u8>>, String> {
    let received = stream
        .set_read_timeout(Some(COMMIT_TIMEOUT))
        .and_then(|()| fd_passing::recv_with_fds(stream))
        .map_err(|err| err.to_string());
    let error = match received.map(|(data, _)| data) {
        Ok(data) => match decode_part(&data) {
            Ok((HandoffPart::Commit { pending_input }, blocks)) => {
                return Ok(pending_input.into_iter().zip(blocks).map(|(id, input)| (id, input.to_vec())).collect());
            }
            // 旧宿主只在提交时发东西：读不懂也是提交了。
            Ok((other, _)) => {
                tracing::error!("expected the commit from the old host, got {other:?}; taking the sessions anyway");
                return Ok(HashMap::new());
            }
            Err(err) => {
                tracing::error!("unreadable commit from the old host ({err}); taking the sessions anyway");
                return Ok(HashMap::new());
            }
        },
        Err(err) => err,
    };
    let Some(pid) = old_pid else {
        return Err(format!("the old host went away without committing ({error}) and its pid is unknown"));
    };
    let until = Instant::now() + EXIT_GRACE;
    while running(pid) {
        if Instant::now() >= until {
            return Err(format!("the old host {pid} rolled back without committing ({error})"));
        }
        thread::sleep(Duration::from_millis(10));
    }
    tracing::error!(
        "the old host {pid} died without committing ({error}); taking the sessions over, input it had not written yet is lost"
    );
    Ok(HashMap::new())
}

/// 不接手了：各个会话把 PTY 原样交回（只关这边的描述符）、线程结束，最多等 `RELEASE_TIMEOUT`。
fn release_all(handles: &[(SessionId, Handle)]) {
    let waiting: Vec<_> = handles
        .iter()
        .filter_map(|(id, handle)| {
            let (reply, released) = mpsc::channel();
            handle.send(Inbox::Release(reply)).then_some((*id, released))
        })
        .collect();
    let deadline = Instant::now() + RELEASE_TIMEOUT;
    for (id, released) in waiting {
        if let Err(err) = released.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            tracing::warn!("session {id} did not give its pty back: {err}");
        }
    }
}

fn send(stream: &UnixStream, message: &ClientMsg) -> io::Result<()> {
    let frame = Frame::control(message).map_err(|err| io::Error::other(err.to_string()))?;
    write_frame(&mut &*stream, frame.kind, 0, &frame.payload).map_err(|err| io::Error::other(err.to_string()))
}

/// 读下一条控制消息。不经缓冲地读，只读这一帧的字节：之后的描述符消息要用 `recvmsg` 收。
fn receive_message(stream: &UnixStream) -> io::Result<HostMsg> {
    match read_frame(&mut &*stream).map_err(|err| io::Error::other(err.to_string()))? {
        Some(frame) if frame.kind == FrameKind::Control => {
            frame.message().map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
        }
        Some(frame) => Err(io::Error::new(io::ErrorKind::InvalidData, format!("unexpected {:?} frame", frame.kind))),
        None => Err(io::ErrorKind::UnexpectedEof.into()),
    }
}
