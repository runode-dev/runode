//! 一条连接上收到的消息怎么办：登记连接、握手，把请求转给会话、界面或另起的线程，等回话的请求限量，
//! 连接断开时撤掉登记。

use std::{
    collections::HashMap,
    io::BufReader,
    net::Shutdown,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};

#[cfg(unix)]
use libc::SO_SNDBUF;
use runode_protocol::{
    AttachMode, ClientKind, ClientMsg, ERROR_BACKED_UP, ERROR_NO_SESSION, FrameKind, GoodbyeReason, HANDOFF_FORMAT,
    HostMsg, MAX_FIND_COMMANDS, PROTOCOL_VERSION, SessionId, SessionInfo, ShellReport, git::GitRequest, read_frame,
};
use runode_terminal::pty;
use runode_types::{grid::GridSize, input::parse_key_list, session::DriveAction};
#[cfg(windows)]
use windows_sys::Win32::Networking::WinSock::SO_SNDBUF;

#[cfg(unix)]
use super::listen::set_buffer;
#[cfg(windows)]
use super::windows::set_buffer;
use super::{
    Asker, Peer, Phase, REPLY_TIMEOUT, ToUi, UnixStream,
    outbox::{MOBILE_OUTBOX_LIMIT, Outbox, session_sink, start_writer},
    same_user, set_buffers,
};
use crate::{
    Shared, SpawnOptions, Stopped, browse,
    git::{GitWorker, Job},
    session::{Drive, Inbox, Screen, Subscribe},
};

/// 快照分成这么大的帧发，不必一帧装下整份（上限见 `runode_protocol::MAX_PAYLOAD`）。
const SNAPSHOT_CHUNK: usize = 1 << 20;
/// 一条连接上最多同时有这么多列会话、读屏幕的请求在等回话，读写 git 的请求、列目录和列命令的
/// 请求各另有这么多，见 `Waiting`。
pub(super) const MAX_WAITING: usize = 16;
/// 等回话的请求已经有 `MAX_WAITING` 条时回的错误。
const ERROR_TOO_MANY_WAITING: &str = "too many requests are waiting for an answer";
/// 转给界面的请求还没回话、界面的连接就断了时回的错误。
const ERROR_WINDOW_GONE: &str = "the runode window went away before answering";
/// 前端给的终端行列数最多这么大，再大的开会话、连上、改尺寸都回 `Error`：几万见方的网格会让
/// 会话线程卡住、内存涨到 GB 级。
const MAX_GRID: u16 = 2000;
/// 手机连接的发送缓冲：发出去的积压在这里也有一份，同样为了回显不排在一大段旧输出后面。
const MOBILE_SOCKET_BUFFER: i32 = 256 << 10;

/// 登记一条新连接，返回它的编号；宿主不再接新连接时返回 `None`。
pub(super) fn register(shared: &Shared) -> Option<u64> {
    let mut peers = shared.peers();
    if !peers.accepting {
        return None;
    }
    let id = shared.next_connection();
    peers.connections.insert(id, Peer { out: None, watches_layout: false, watches_todos: false });
    peers.activity_at = Instant::now();
    Some(id)
}

/// 起线程服务登记好的连接 `id`；起不了时撤掉登记，返回 false。
pub(super) fn start_serving(shared: &Arc<Shared>, id: u64, stream: UnixStream, check_peer: bool) -> bool {
    let thread_shared = shared.clone();
    let spawned = thread::Builder::new()
        .name("host-connection".into())
        .spawn(move || serve(&thread_shared, id, stream, check_peer));
    if let Err(err) = spawned {
        tracing::warn!("failed to start a connection thread: {err}");
        unregister(shared, id);
        return false;
    }
    true
}

/// 连接结束：撤掉登记。它是界面的话，转给它还没回话的请求都回一句没办成（会话的请求交回会话
/// 线程）；它发出去、界面还没回话的请求撤掉，回话没人收了。
fn unregister(shared: &Shared, id: u64) {
    let mut peers = shared.peers();
    peers.connections.remove(&id);
    peers.desktops.retain(|&desktop| desktop != id);
    peers.pending.retain(|_, pending| !matches!(pending.asker, Asker::Client { from, .. } if from == id));
    let orphaned: Vec<u64> =
        peers.pending.iter().filter(|(_, pending)| pending.desktop == id).map(|(&ui, _)| ui).collect();
    let gone = || ERROR_WINDOW_GONE.to_owned();
    let mut sessions = Vec::new();
    for ui in orphaned {
        match peers.pending.remove(&ui).map(|pending| pending.asker) {
            Some(Asker::Client { origin, req, .. }) => {
                origin.control(&HostMsg::Error { req: Some(req), id: None, message: gone() });
            }
            Some(Asker::Session(session)) => sessions.push((session, ui)),
            None => {}
        }
    }
    peers.activity_at = Instant::now();
    drop(peers);
    shared.peers_changed.notify_all();
    for (session, ui) in sessions {
        let reply = Box::new(HostMsg::Error { req: None, id: Some(session), message: gone() });
        shared.deliver(session, Inbox::UiAnswer { ui, reply });
    }
}

/// 一条连接从头到尾：起写的线程，在这个线程里读，读完了断开连着的会话、撤掉登记。写的线程等
/// 所有会话都放开这条连接、积压的帧写完后自己结束。`check_peer` 时只接同一个用户的进程。
fn serve(shared: &Arc<Shared>, id: u64, stream: UnixStream, check_peer: bool) {
    pty::set_current_thread_interactive();
    if check_peer && !same_user(&stream) {
        tracing::warn!("refused a connection from another user");
        unregister(shared, id);
        return;
    }
    set_buffers(&stream);
    let Some(out) = start_writer(&stream) else {
        unregister(shared, id);
        return;
    };
    {
        let mut peers = shared.peers();
        // 登记之后、写线程起好之前宿主要退出了，或者开始把会话交给新宿主了：这条连接也收
        // `Goodbye`。
        let reason = match peers.phase {
            Phase::Stopping(Stopped::Shutdown) => Some(GoodbyeReason::Shutdown),
            Phase::HandingOver | Phase::Stopping(Stopped::Handoff) => Some(GoodbyeReason::Handoff),
            Phase::Serving | Phase::Stopping(Stopped::Idle) => None,
        };
        if let Some(reason) = reason {
            out.control(&HostMsg::Goodbye { reason });
            out.close();
        }
        if let Some(peer) = peers.connections.get_mut(&id) {
            peer.out = Some(out.clone());
        }
    }
    let mut connection = Connection {
        shared: shared.clone(),
        id,
        out,
        kind: ClientKind::Unknown,
        by: None,
        device: None,
        snapshots: false,
        shell_report: false,
        channels: HashMap::new(),
        next_channel: 1,
        waiting: Waiting::default(),
        git_waiting: Waiting::default(),
        files_waiting: Waiting::default(),
        spawn_waiting: Waiting::default(),
        git: None,
        closing: false,
    };
    connection.run(&mut BufReader::new(&stream));
    connection.detach_all();
    unregister(shared, id);
}

impl Shared {
    /// 所有会话在 `SessionList` 里的样子：在调用的线程里把请求按先后送到各个会话线程，返回等
    /// 回话的一端，见 `collect_sessions`。
    fn ask_sessions(&self) -> Vec<mpsc::Receiver<SessionInfo>> {
        self.handles()
            .into_iter()
            .filter_map(|(_, handle)| {
                let (reply, info) = mpsc::channel();
                handle.send(Inbox::Info(reply)).then_some(info)
            })
            .collect()
    }
}

/// 等 `Shared::ask_sessions` 的回话，最多等到 `REPLY_TIMEOUT`；没回话的会话不列。
fn collect_sessions(replies: Vec<mpsc::Receiver<SessionInfo>>) -> Vec<SessionInfo> {
    let deadline = Instant::now() + REPLY_TIMEOUT;
    replies
        .into_iter()
        .filter_map(|info| info.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok())
        .collect()
}

/// 一条连接上在等会话线程回话的请求数，上限 `MAX_WAITING`。
#[derive(Clone, Default)]
struct Waiting(Arc<AtomicUsize>);

impl Waiting {
    /// 再等一个；已经到上限时返回 `None`。返回的 `WaitSlot` 丢掉时减回去。
    fn enter(&self) -> Option<WaitSlot> {
        self.0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < MAX_WAITING).then_some(n + 1))
            .ok()
            .map(|_| WaitSlot(self.0.clone()))
    }
}

/// `Waiting` 里的一个名额，回完话（或者没起成线程、丢掉了）时放开。
pub(crate) struct WaitSlot(Arc<AtomicUsize>);

impl Drop for WaitSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 起一个短命的线程等回话，不占着读的线程；起不了时就地等。
fn answer_later(name: &str, answer: impl FnOnce() + Send + 'static) {
    let answer = Arc::new(std::sync::Mutex::new(Some(answer)));
    let deferred = answer.clone();
    let spawned = thread::Builder::new().name(name.into()).spawn(move || {
        if let Some(answer) = deferred.lock().ok().and_then(|mut answer| answer.take()) {
            answer();
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("failed to start a reply thread, answering in place: {err}");
        if let Some(answer) = answer.lock().ok().and_then(|mut answer| answer.take()) {
            answer();
        }
    }
}

/// 一条连接上读的那边的状态。
struct Connection {
    shared: Arc<Shared>,
    /// 这条连接的编号：`Detach` 按它找到这条连接的订阅，转给界面的请求按它认回话的一方。
    id: u64,
    out: Outbox,
    /// `Hello` 里说的前端种类。
    kind: ClientKind,
    /// `Hello` 里说的前端所在的会话，记谁在操作会话时用，见 `drive`。
    by: Option<SessionId>,
    /// `Hello` 里报的设备名，尺寸归这条连接时告诉别的前端，见 `HostMsg::SizeOwner`。
    device: Option<String>,
    /// 前端解得了快照：自己说能解，构建也和宿主一样。
    snapshots: bool,
    /// 前端说了 `Caps::shell_report`：shell 集成报告的东西拆出来单独发，没变时不发。
    shell_report: bool,
    /// 连着的会话，按通道。
    channels: HashMap<u32, SessionId>,
    next_channel: u32,
    /// 在等回话的列会话、读屏幕的请求。
    waiting: Waiting,
    /// 在排队或在办的 git 请求，和 `waiting` 分开算：推送、拉取卡在网络上时不占列会话、读屏幕的名额。
    git_waiting: Waiting,
    /// 在另起的线程里读文件系统的请求（列目录、列命令），也和 `waiting` 分开算，见 `read_files`。
    files_waiting: Waiting,
    /// 在另起的线程里开的会话，和 `files_waiting` 同理另算。
    spawn_waiting: Waiting,
    /// 办 git 请求的工作线程，第一次要时才起。
    git: Option<GitWorker>,
    /// 这条连接发来的 `Shutdown` 让宿主退出了：之后的请求都不办，读的线程结束。
    closing: bool,
}

impl Connection {
    fn run(&mut self, reader: &mut BufReader<&UnixStream>) {
        let hello = match read_frame(reader) {
            Ok(Some(frame)) if frame.kind == FrameKind::Control => frame.message::<ClientMsg>().ok(),
            Ok(_) => None,
            Err(err) => {
                tracing::debug!("connection closed before hello: {err}");
                return;
            }
        };
        let Some(ClientMsg::Hello { protocol, build, caps, client, session, device }) = hello else {
            self.goodbye("the first message must be hello");
            return;
        };
        // 接手的新宿主不比对协议版本，永远如此，见 `runode_protocol::message` 的模块文档。
        #[cfg(unix)]
        if client == ClientKind::Successor {
            self.kind = client;
            let peers = self.shared.peers();
            let welcome = self.welcome(peers.standalone, peers.desktop_count());
            drop(peers);
            self.out.control(&welcome);
            self.successor(reader, build);
            return;
        }
        // Windows 上的 ConPTY 交不出去，升级时会话跟着重启。
        #[cfg(not(unix))]
        if client == ClientKind::Successor {
            self.goodbye("this host cannot hand over its sessions");
            return;
        }
        if protocol != PROTOCOL_VERSION {
            self.out.control(&HostMsg::Incompatible {
                protocol: PROTOCOL_VERSION,
                build: self.shared.build.clone(),
                reason: format!("the host speaks protocol {PROTOCOL_VERSION}, the client {protocol}"),
            });
            return;
        }
        self.kind = client;
        if client == ClientKind::Mobile {
            self.out.limit = MOBILE_OUTBOX_LIMIT;
            set_buffer(reader.get_ref(), SO_SNDBUF, MOBILE_SOCKET_BUFFER);
        }
        self.by = session;
        self.device = device;
        self.snapshots = caps.snapshot && build == self.shared.build;
        self.shell_report = caps.shell_report;
        // 登记界面和回 `Welcome` 在同一把锁里做：界面收到 `Welcome` 后，别的连接转来的请求一定
        // 交给它；别的连接也只有在 `Welcome` 排进 `Outbox` 之后才看得到它，请求不会抢在前面。
        let mut peers = self.shared.peers();
        self.out.control(&self.welcome(peers.standalone, peers.desktop_count()));
        if client == ClientKind::Desktop {
            peers.desktops.push(self.id);
        }
        drop(peers);
        loop {
            let frame = match read_frame(reader) {
                Ok(Some(frame)) => frame,
                Ok(None) => return,
                Err(err) => {
                    tracing::debug!("connection closed: {err}");
                    return;
                }
            };
            match frame.kind {
                // 回话写不出去，再办只会往积压上加：不发 `Goodbye`（也排在积压后面），直接断开，写的
                // 线程随之出错退出，排着的帧跟着丢掉。
                FrameKind::Control if self.out.overflowing() => {
                    tracing::warn!("closing connection {}: the client stopped reading its replies", self.id);
                    let _ = reader.get_ref().shutdown(Shutdown::Both);
                    return;
                }
                FrameKind::Control => match frame.message::<ClientMsg>() {
                    Ok(message) => {
                        self.handle(message);
                        if self.closing {
                            // `Goodbye` 和之前的回话可能还在写线程里：等它写完、断开再走。走了就撤掉登记，最后一条
                            // 连接撤掉后 `Host::run_until_idle` 不再等，进程随之退出，没写出去的就丢了。写线程在
                            // `Goodbye` 后面的 `Out::Close` 处结束，排在后面的这一声随之断开。最多等得和
                            // `run_until_idle` 等各条连接一样久。
                            let _ = self.out.detach().recv_timeout(crate::idle::DRAIN_TIMEOUT);
                            return;
                        }
                    }
                    Err(err) => self.error(None, None, format!("unreadable message: {err}")),
                },
                FrameKind::Input => match self.channels.get(&frame.channel) {
                    // 会话已经没了（比如别的前端结束了它）：通道跟着作废。
                    Some(&id) => {
                        self.drive(id, DriveAction::Input);
                        let input = Inbox::Input { connection: self.id, data: frame.payload };
                        if !self.shared.deliver(id, input) {
                            self.channels.remove(&frame.channel);
                        }
                    }
                    None => tracing::debug!("input for unknown channel {}", frame.channel),
                },
                FrameKind::Output | FrameKind::Snapshot | FrameKind::Video | FrameKind::Audio => {
                    self.goodbye("clients do not send output, snapshots, video or audio");
                    return;
                }
            }
        }
    }

    fn welcome(&self, standalone: bool, desktops: u32) -> HostMsg {
        HostMsg::Welcome {
            protocol: PROTOCOL_VERSION,
            build: self.shared.build.clone(),
            host_pid: std::process::id(),
            snapshot_format: self.shared.snapshot_format,
            standalone,
            handoff: HANDOFF_FORMAT,
            keep_awake: true,
            todos: true,
            shells: true,
            desktops,
        }
    }

    /// 接手的新宿主（构建是 `build`）连上来：只认 `ClientMsg::Handoff`，交给 `handoff::give`，之后
    /// 这条连接归它。
    #[cfg(unix)]
    fn successor(&self, reader: &mut BufReader<&UnixStream>, build: runode_protocol::BuildId) {
        let message = match read_frame(reader) {
            Ok(Some(frame)) if frame.kind == FrameKind::Control => frame.message::<ClientMsg>().ok(),
            Ok(_) => None,
            Err(err) => {
                tracing::debug!("the successor went away before asking for the handoff: {err}");
                return;
            }
        };
        match message {
            Some(ClientMsg::Handoff { min_format, max_format }) => {
                let asked = crate::handoff::Asked { formats: min_format..=max_format, build };
                crate::handoff::give(&self.shared, self.id, &self.out, reader, &asked);
            }
            _ => self.goodbye("a successor must ask for the handoff"),
        }
    }

    fn handle(&mut self, message: ClientMsg) {
        match message {
            ClientMsg::Hello { .. } => self.error(None, None, "already said hello".into()),
            ClientMsg::ListSessions => {
                if self.out.backed_up() {
                    self.error(None, None, ERROR_BACKED_UP.into());
                    return;
                }
                let Some(slot) = self.waiting.enter() else {
                    self.error(None, None, ERROR_TOO_MANY_WAITING.into());
                    return;
                };
                let replies = self.shared.ask_sessions();
                let out = self.out.clone();
                answer_later("host-list-sessions", move || {
                    out.control(&HostMsg::SessionList { sessions: collect_sessions(replies) });
                    drop(slot);
                });
            }
            ClientMsg::Spawn { req, size, cwd, fallback_cwd, integration, start, shell, settings, resume } => {
                if let Err(message) = check_size(size) {
                    self.error(Some(req), None, message);
                    return;
                }
                let shared = self.shared.clone();
                self.read_files_in(&self.spawn_waiting, req, "host-spawn", move || {
                    // 备用目录也不在时交给 `Pty::start`，它从家目录开。
                    let cwd = match (cwd, fallback_cwd) {
                        (Some(cwd), Some(fallback)) if !cwd.is_dir() => Some(fallback),
                        (cwd, _) => cwd,
                    };
                    let options = SpawnOptions { size, cwd, integration, start, shell, settings };
                    let id = shared.spawn(options).map_err(|err| format!("{err:#}"))?;
                    if let Some(command) = resume.as_ref().and_then(runode_agent::conversation::resume_command) {
                        shared.deliver(id, Inbox::TypeAtPrompt(format!("{command}\r").into_bytes()));
                    }
                    Ok(HostMsg::Spawned { req, id })
                });
            }
            ClientMsg::Start { id, integration } => {
                if !self.shared.deliver(id, Inbox::Start { integration }) {
                    self.error(None, Some(id), format!("{ERROR_NO_SESSION} {id}"));
                }
            }
            ClientMsg::Attach { id, size, mode } => self.attach(id, size, mode),
            ClientMsg::Detach { id } => {
                self.forget(id);
                self.shared.deliver(id, Inbox::Detach { connection: self.id });
            }
            ClientMsg::Kill { id } => {
                self.drive(id, DriveAction::Kill);
                self.forget(id);
                self.shared.kill(id);
            }
            ClientMsg::Resize { id, size } => {
                if let Err(message) = check_size(size) {
                    self.error(None, Some(id), message);
                    return;
                }
                self.shared.deliver(id, Inbox::Resize { connection: self.id, size });
            }
            ClientMsg::ClearScreen { id } => {
                self.drive(id, DriveAction::ClearScreen);
                self.shared.deliver(id, Inbox::ClearScreen);
            }
            ClientMsg::SetTheme { settings } => self.shared.set_theme(settings),
            // 读写剪贴板的规矩是用户在 app 里的设置，只认说自己是界面的连接。种类是自报的，这只防
            // 命令行之类误发，不防同一个用户的程序冒充界面，见模块文档。
            ClientMsg::SetOptions { clipboard, .. } => {
                if self.kind == ClientKind::Desktop {
                    self.shared.set_clipboard(clipboard);
                } else {
                    self.error(None, None, "only the runode app changes the host's options".into());
                }
            }
            // 获得焦点算一次交互，可能轮到这条连接决定尺寸；失去焦点只是不在看了，不让出。通知在
            // 界面那边发，宿主不用知道哪个会话被看着。
            ClientMsg::Focus { id, focused: true } => {
                self.shared.deliver(id, Inbox::Focus { connection: self.id });
            }
            ClientMsg::Focus { focused: false, .. } => {}
            ClientMsg::ReadScreen { id, lines, command } => self.read_screen(id, lines, command),
            ClientMsg::SendKeys { req, id, keys } => match parse_key_list(&keys) {
                Ok(keys) => self.deliver_done(req, id, DriveAction::Keys, Inbox::Keys(keys)),
                Err(err) => self.error(Some(req), Some(id), err),
            },
            ClientMsg::Paste { req, id, text } => self.deliver_done(req, id, DriveAction::Paste, Inbox::Paste(text)),
            ClientMsg::Open { req, .. }
            | ClientMsg::OpenWorkspace { req, .. }
            | ClientMsg::RenameWorkspace { req, .. }
            | ClientMsg::Reveal { req, .. }
            | ClientMsg::MovePane { req, .. }
            | ClientMsg::SwapPanes { req, .. } => {
                self.to_ui(req, message);
            }
            ClientMsg::Layout { req } => {
                // 界面自己问的是宿主手里的布局：恢复窗口前接回没开着时宿主办的事，见 `workspaces`。
                if self.kind == ClientKind::Desktop {
                    self.out.control(&HostMsg::Layout { req, windows: self.shared.held_layout() });
                    return;
                }
                if let Some(peer) = self.shared.peers().connections.get_mut(&self.id) {
                    peer.watches_layout = true;
                }
                self.to_ui(req, message);
            }
            ClientMsg::LayoutChanged { windows } => {
                if self.kind == ClientKind::Desktop {
                    let mut peers = self.shared.peers();
                    if let Some(windows) = windows {
                        peers.layout = windows;
                    }
                    peers.layout_changed();
                } else {
                    self.error(None, None, "only the runode app reports its layout".into());
                }
            }
            ClientMsg::ListDirs { req, path } => self.read_files(req, "host-list-dirs", move || {
                let browse::Listing { path, dirs, truncated } = browse::list_dirs(path)?;
                Ok(HostMsg::Dirs { req, path, dirs, truncated })
            }),
            ClientMsg::ListTasks { req, dir } => self.read_files(req, "host-list-tasks", move || {
                let sources = runode_tasks::list(&dir)?;
                Ok(HostMsg::Tasks { req, dir, sources })
            }),
            ClientMsg::ListShells { req } => self.read_files(req, "host-list-shells", move || {
                Ok(HostMsg::Shells { req, shells: runode_terminal::shells::available() })
            }),
            ClientMsg::FindCommands { req, path, programs } => {
                if programs.len() > MAX_FIND_COMMANDS {
                    self.error(Some(req), None, format!("at most {MAX_FIND_COMMANDS} commands at a time"));
                    return;
                }
                self.read_files(req, "host-find-commands", move || {
                    Ok(HostMsg::Commands {
                        req,
                        found: runode_terminal::shells::find_commands(path.as_deref(), &programs),
                    })
                });
            }
            ClientMsg::Git { req, id, request } => self.git(req, id, request),
            ClientMsg::ListTodos { req } => {
                if let Some(peer) = self.shared.peers().connections.get_mut(&self.id) {
                    peer.watches_todos = true;
                }
                self.todos(req, "host-list-todos", move |shared| shared.list_todos(req));
            }
            ClientMsg::AddTodo { req, spec } => {
                self.todos(req, "host-add-todo", move |shared| shared.add_todo(req, &spec))
            }
            ClientMsg::UpdateTodo { req, id, spec, done } => {
                self.todos(req, "host-update-todo", move |shared| shared.update_todo(req, &id, spec.as_ref(), done));
            }
            ClientMsg::RemoveTodo { req, id } => {
                self.todos(req, "host-remove-todo", move |shared| shared.remove_todo(req, &id));
            }
            ClientMsg::StartTodo { req, id } => self.todos(req, "host-start-todo", move |shared| {
                shared.start_todo(&id).map(|session| HostMsg::Opened { req, id: session })
            }),
            ClientMsg::ResumeTodo { req, id } => self.todos(req, "host-resume-todo", move |shared| {
                shared.resume_todo(&id).map(|session| HostMsg::Opened { req, id: session })
            }),
            ClientMsg::ListDoneTodos { req, schedule, offset } => {
                self.todos(req, "host-list-done-todos", move |shared| {
                    shared.list_done_todos(req, schedule.as_deref(), offset)
                })
            }
            ClientMsg::GetTodo { req, id } => {
                self.todos(req, "host-get-todo", move |shared| shared.get_todo(req, &id));
            }
            ClientMsg::MoveTodo { req, id, before } => {
                self.todos(req, "host-move-todo", move |shared| shared.move_todo(req, &id, before.as_deref()));
            }
            ClientMsg::AddSchedule { req, spec, when } => {
                self.todos(req, "host-add-schedule", move |shared| shared.add_schedule(req, &spec, &when));
            }
            ClientMsg::UpdateSchedule { req, id, spec, when } => {
                self.todos(req, "host-update-schedule", move |shared| {
                    shared.update_schedule(req, &id, spec.as_ref(), when.as_ref())
                })
            }
            ClientMsg::RemoveSchedule { req, id } => {
                self.todos(req, "host-remove-schedule", move |shared| shared.remove_schedule(req, &id));
            }
            ClientMsg::UiReply { ui, reply } => self.ui_reply(ui, *reply),
            // 读写剪贴板是宿主替会话里的程序请界面办的，前端不能直接要。
            ClientMsg::WriteClipboard { .. } | ClientMsg::ReadClipboard { .. } => {
                self.error(None, None, "only the host asks the runode window to use the clipboard".into());
            }
            // 宿主要退出时这条连接上之后的请求都不办（同一次读进缓冲里的 `Spawn` 也是），见 `Connection::run`。
            ClientMsg::Shutdown { kill_sessions: true } => {
                self.channels.clear();
                self.shared.kill_all();
                self.closing = self.shared.shut_down();
            }
            ClientMsg::Shutdown { kill_sessions: false } => {
                self.error(None, None, "the host cannot hand its sessions over yet".into());
            }
            // 交接只在 `Hello` 里说自己是 `ClientKind::Successor` 的连接上谈，见 `successor`。
            ClientMsg::Handoff { .. }
            | ClientMsg::HandoffReady
            | ClientMsg::HandoffAbort { .. }
            | ClientMsg::HandoffDone => {
                self.error(None, None, "only a successor host can take the sessions over".into());
            }
            // 推送只有手机经远程访问登记，由监听方按过了门禁的那台设备记下，不到宿主这里。
            ClientMsg::PushRegister { req, .. } => {
                self.error(Some(req), None, "only a remote device registers for push through remote access".into());
            }
            ClientMsg::KeepAwake => {}
            ClientMsg::Unknown => self.error(None, None, "unknown message".into()),
        }
    }

    /// 把要界面办的请求转给现在的界面（最近连上的 `ClientKind::Desktop` 连接）。没有界面连接
    /// 时宿主自己办，见 `headless`。
    fn to_ui(&self, req: u32, request: ClientMsg) {
        let asker = Asker::Client { from: self.id, origin: self.out.clone(), req };
        match self.shared.send_to_ui(None, asker, request) {
            ToUi::Sent(_) => {}
            ToUi::NoDesktop(request) => self.headless(req, request),
            ToUi::Gone(Some(Asker::Client { origin, req, .. })) => {
                origin.control(&HostMsg::Error { req: Some(req), id: None, message: ERROR_WINDOW_GONE.into() });
            }
            ToUi::Gone(_) => {}
        }
    }

    /// 没有界面连着时办要界面办的请求：布局、开终端、新建工作区、改名照宿主手里的布局办，见
    /// `workspaces`；切到终端、挪分屏没有窗口可办，回一句没办成。开终端要等会话线程、stat 目录，和读文件系统的
    /// 请求一样另起线程办。
    fn headless(&self, req: u32, request: ClientMsg) {
        let shared = self.shared.clone();
        match request {
            ClientMsg::Layout { req } => {
                self.out.control(&HostMsg::Layout { req, windows: shared.held_layout() });
            }
            ClientMsg::RenameWorkspace { req, window, workspace, name } => {
                let reply = shared.rename_workspace(req, window, workspace, &name);
                self.out.control(&reply.unwrap_or_else(|message| HostMsg::Error { req: Some(req), id: None, message }));
            }
            ClientMsg::OpenWorkspace { req, dir, name, .. } => {
                self.read_files(req, "host-open-workspace", move || shared.open_workspace(req, dir, name));
            }
            ClientMsg::Open { req, near, cwd, .. } => {
                self.read_files(req, "host-open", move || shared.open_tab(req, near, cwd));
            }
            _ => self.error(Some(req), None, "there is no runode window to do this in".into()),
        }
    }

    /// 界面回话：转给发请求的一方，会话自己的请求交回会话线程。只认被转去的那个界面回的；对不上的
    /// （请求已经回过、不是转给这条连接的、发请求的一方已经断开的）丢掉。界面读不懂请求（读成
    /// `ClientMsg::Unknown`）时回的 `Error` 不知道 `req`，这里补上原请求的，发请求的一方才认得出是
    /// 哪条没办成。
    fn ui_reply(&self, ui: u64, reply: HostMsg) {
        let pending = {
            let mut peers = self.shared.peers();
            match peers.pending.get(&ui) {
                Some(pending) if pending.desktop == self.id => peers.pending.remove(&ui),
                _ => None,
            }
        };
        match pending.map(|pending| pending.asker) {
            Some(Asker::Client { origin, req, .. }) => {
                let reply = match reply {
                    HostMsg::Error { req: None, id, message } => HostMsg::Error { req: Some(req), id, message },
                    reply => reply,
                };
                origin.control(&reply);
            }
            Some(Asker::Session(session)) => {
                self.shared.deliver(session, Inbox::UiAnswer { ui, reply: Box::new(reply) });
            }
            None => tracing::debug!("dropped a reply to unknown ui request {ui}"),
        }
    }

    /// 连上会话。已经连着的换一个新通道重新连，前端收到 `Resync` 后就是这样重新连上的；
    /// `MetaOnly` 和看屏幕之间来回切换也是这样。不先发 `Detach`：会话线程收到同一条连接的
    /// `Subscribe` 时换掉旧的订阅，尺寸归属的状态（见 `Runner::subscribe`）留着；旧通道的帧都在
    /// 新的 `Attached` 之前，新通道的帧都在它之后。
    fn attach(&mut self, id: SessionId, size: Option<GridSize>, mode: AttachMode) {
        if let Err(message) = size.map_or(Ok(()), check_size) {
            self.error(None, Some(id), message);
            return;
        }
        if self.out.backed_up() {
            self.error(None, Some(id), ERROR_BACKED_UP.into());
            return;
        }
        self.forget(id);
        let mode = match mode {
            AttachMode::Snapshot if !self.snapshots => AttachMode::VtReplay,
            mode => mode,
        };
        let channel = self.next_channel;
        self.next_channel = self.next_channel.checked_add(1).unwrap_or(1);
        let out = self.out.clone();
        let shell_report = self.shell_report;
        let start = Box::new(move |screen: Screen| {
            let meta_only = screen.mode == AttachMode::MetaOnly;
            let mut meta = screen.meta;
            let shell = shell_report.then(|| ShellReport::take(&mut meta));
            let attached = HostMsg::Attached {
                id,
                channel,
                size: screen.size,
                mode: screen.mode,
                meta,
                settings: Some(screen.settings),
                shell: shell.clone(),
            };
            if !out.control(&attached) {
                return None;
            }
            if !meta_only {
                for chunk in screen.data.chunks(SNAPSHOT_CHUNK) {
                    out.push(FrameKind::Snapshot, channel, chunk.into());
                }
                out.control(&HostMsg::SnapshotEnd { id });
            }
            Some(session_sink(id, channel, meta_only, shell, out))
        });
        let desktop = self.kind == ClientKind::Desktop;
        let device = self.device.clone();
        let subscribe = Subscribe { connection: self.id, size, mode, start, desktop, device };
        if self.shared.deliver(id, Inbox::Subscribe(subscribe)) {
            self.channels.insert(channel, id);
        } else {
            self.error(None, Some(id), format!("{ERROR_NO_SESSION} {id}"));
        }
    }

    /// 读屏幕，`command` 给了时读倒数第几条命令的输出：请求在这里送到会话线程（排在这条连接
    /// 之前送去的输入后面），回话另起线程等。
    fn read_screen(&self, id: SessionId, lines: Option<u32>, command: Option<u32>) {
        if self.out.backed_up() {
            self.error(None, Some(id), ERROR_BACKED_UP.into());
            return;
        }
        let Some(slot) = self.waiting.enter() else {
            self.error(None, Some(id), ERROR_TOO_MANY_WAITING.into());
            return;
        };
        let (reply, text) = mpsc::channel();
        if !self.shared.deliver(id, Inbox::ReadScreen { lines, command, reply }) {
            self.error(None, Some(id), format!("{ERROR_NO_SESSION} {id}"));
            return;
        }
        let out = self.out.clone();
        answer_later("host-read-screen", move || {
            let message = match text.recv_timeout(REPLY_TIMEOUT) {
                Ok(Ok((text, truncated))) => HostMsg::ScreenText { id, text, truncated },
                // 读命令输出时的错误（没有 shell 集成这类）本身就是给读的一方看的说明。
                Ok(Err(err)) if command.is_some() => {
                    HostMsg::Error { req: None, id: Some(id), message: format!("{err:#}") }
                }
                Ok(Err(err)) => {
                    HostMsg::Error { req: None, id: Some(id), message: format!("failed to read the screen: {err:#}") }
                }
                Err(_) => HostMsg::Error { req: None, id: Some(id), message: format!("session {id} did not answer") },
            };
            out.control(&message);
            drop(slot);
        });
    }

    /// 另起线程办一件要读文件系统的请求（列目录、列命令，没有界面时开终端），占 `files_waiting`
    /// 的一个名额：断掉的网络挂载上 stat、读目录会一直卡住，卡住的不能是读线程。`answer` 的 `Err` 回成带着
    /// `req` 的 `Error`。
    fn read_files(&self, req: u32, name: &str, answer: impl FnOnce() -> Result<HostMsg, String> + Send + 'static) {
        self.read_files_in(&self.files_waiting, req, name, answer);
    }

    /// 同 `read_files`，占 `slots` 的名额。
    fn read_files_in(
        &self,
        slots: &Waiting,
        req: u32,
        name: &str,
        answer: impl FnOnce() -> Result<HostMsg, String> + Send + 'static,
    ) {
        let Some(slot) = slots.enter() else {
            self.error(Some(req), None, ERROR_TOO_MANY_WAITING.into());
            return;
        };
        let out = self.out.clone();
        answer_later(name, move || {
            let message = answer().unwrap_or_else(|message| HostMsg::Error { req: Some(req), id: None, message });
            out.control(&message);
            drop(slot);
        });
    }

    /// 另起线程办一件看板的请求，见 `todos`：要读写库、stat 目录，开始任务时还要跑 git、开会话。
    fn todos(&self, req: u32, name: &str, answer: impl FnOnce(&Shared) -> Result<HostMsg, String> + Send + 'static) {
        let shared = self.shared.clone();
        self.read_files(req, name, move || answer(&shared));
    }

    /// 在会话 `id` 所在的仓库里办 git 请求：向会话线程要它的目录，连同请求交给这条连接的 git
    /// 工作线程，见 `git` 模块。
    fn git(&mut self, req: u32, id: SessionId, request: GitRequest) {
        if self.out.backed_up() {
            self.error(Some(req), None, ERROR_BACKED_UP.into());
            return;
        }
        let Some(slot) = self.git_waiting.enter() else {
            self.error(Some(req), None, ERROR_TOO_MANY_WAITING.into());
            return;
        };
        let (reply, info) = mpsc::channel();
        if !self.shared.deliver(id, Inbox::Info(reply)) {
            self.error(Some(req), None, format!("{ERROR_NO_SESSION} {id}"));
            return;
        }
        if self.git.is_none() {
            match GitWorker::start() {
                Ok(worker) => self.git = Some(worker),
                Err(err) => {
                    self.error(Some(req), None, format!("failed to start git: {err}"));
                    return;
                }
            }
        }
        let job = Job { req, id, request, info, out: self.out.clone(), slot };
        if let Some(worker) = &self.git
            && let Err(job) = worker.submit(job)
        {
            self.git = None;
            self.error(Some(job.req), None, "the git worker stopped".into());
        }
    }

    /// 这条连接要对会话做 `action`，先告诉会话谁在操作它（见 `SessionMeta::driver`）：桌面、
    /// 手机、TUI 的界面上是用户自己（手机上的滚动和点按也发输入帧），清掉记录（会话没被标过时
    /// `Handle::send` 直接丢掉，不进收件箱）；别的前端记下来。只有会改会话的操作才调。
    fn drive(&self, id: SessionId, action: DriveAction) {
        let by_user = matches!(self.kind, ClientKind::Desktop | ClientKind::Mobile | ClientKind::Tui);
        let drive = (!by_user).then_some(Drive { by: self.by, action });
        self.shared.deliver(id, Inbox::Driven(drive));
    }

    /// 把一件只回 `Done` 的事（发控制键、粘贴）交给会话，先记下谁在操作它。会话线程按到达的先后
    /// 处理，在这之后发来的输入不会抢到前面。
    fn deliver_done(&self, req: u32, id: SessionId, action: DriveAction, message: Inbox) {
        self.drive(id, action);
        if self.shared.deliver(id, message) {
            self.out.control(&HostMsg::Done { req });
        } else {
            self.error(Some(req), Some(id), format!("{ERROR_NO_SESSION} {id}"));
        }
    }

    /// 不再记着这个会话的通道，之后它的输入帧不再转发；原来连着时返回 true。
    fn forget(&mut self, id: SessionId) -> bool {
        let before = self.channels.len();
        self.channels.retain(|_, attached| *attached != id);
        self.channels.len() != before
    }

    /// 连接结束：断开所有连着的会话，它们放开这条连接后写的线程就结束了。
    fn detach_all(&mut self) {
        for (_, id) in self.channels.drain() {
            self.shared.deliver(id, Inbox::Detach { connection: self.id });
        }
    }

    fn error(&self, req: Option<u32>, id: Option<SessionId>, message: String) {
        self.out.control(&HostMsg::Error { req, id, message });
    }

    fn goodbye(&self, message: &str) {
        self.out.control(&HostMsg::Goodbye { reason: GoodbyeReason::Error { message: message.into() } });
    }
}

/// 前端给的尺寸行列都不超过 `MAX_GRID` 时为 `Ok`。
fn check_size(size: GridSize) -> Result<(), String> {
    if size.cols > MAX_GRID || size.rows > MAX_GRID {
        return Err(format!("{}x{} is larger than {MAX_GRID}x{MAX_GRID}", size.cols, size.rows));
    }
    Ok(())
}
