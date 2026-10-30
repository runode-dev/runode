//! 连上宿主的 socket：握手，发控制消息和输入，按超时等宿主的消息。
//!
//! 读放在单独的线程里，读到的控制消息经 channel 交过来，等消息时能按截止时刻放弃，不会把一帧
//! 读到一半。命令行只看状态，输出帧和快照帧直接丢掉。

use std::{
    cell::{Cell, RefCell},
    collections::{HashSet, VecDeque},
    io::ErrorKind,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::net::UnixStream;

use anyhow::{Context as _, Result, anyhow, bail};
use runode_protocol::{
    BuildId, Caps, ClientKind, ClientMsg, Frame, FrameKind, GoodbyeReason, HostMsg, PROTOCOL_VERSION, SessionId,
    WindowLayout, read_frame, write_frame,
};
#[cfg(windows)]
use uds_windows::UnixStream;

use crate::Env;

/// 等宿主回话的最长时间；`wait` 等 agent 不受它限制。
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// 宿主断开了连接（`Goodbye`，或者连接直接断了）。
const CLOSED: &str = "the runode app closed the connection";

/// 宿主正把会话交给新版本的宿主（`Goodbye { reason: GoodbyeReason::Handoff }`），这条连接断了。
/// 过一会儿重新连上就是新宿主，见 `is_upgrading`。
#[derive(Debug)]
pub(crate) struct Upgrading;

impl std::fmt::Display for Upgrading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the runode host is being upgraded; run the command again")
    }
}

impl std::error::Error for Upgrading {}

/// 错误是不是宿主在升级（`Upgrading`）。
pub(crate) fn is_upgrading(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| cause.is::<Upgrading>())
}

/// 宿主说 `Goodbye` 时的错误：在升级时是 `Upgrading`，别的原因（含认不出的）一律当作连接断了。
fn goodbye(reason: &GoodbyeReason) -> anyhow::Error {
    match reason {
        GoodbyeReason::Handoff => Upgrading.into(),
        _ => anyhow!(CLOSED),
    }
}

/// 拉起宿主 `<exe> --host`，等到 `socket` 连得上，最多到 `deadline`。宿主自己一个进程组，这个终端里的
/// Ctrl+C 不传给它。拉起的宿主退出了（多半是锁在同时拉起的另一个宿主手里）也照等到 `deadline`，
/// 那一个开了监听就用它，到时还连不上才报退出码。
fn launch_host(exe: &std::path::Path, socket: &std::path::Path, deadline: Instant) -> Result<UnixStream> {
    let mut command = std::process::Command::new(exe);
    command
        .arg("--host")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().with_context(|| format!("cannot start the runode host {}", exe.display()))?;
    let mut exited = None;
    loop {
        if let Ok(stream) = UnixStream::connect(socket) {
            return Ok(stream);
        }
        if exited.is_none() {
            exited = child.try_wait().ok().flatten();
        }
        if Instant::now() >= deadline {
            match exited {
                Some(status) => {
                    bail!("the runode host exited ({status}) before listening on {}; see its log", socket.display())
                }
                None => bail!("the runode host did not listen on {} in time", socket.display()),
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// `probe` 问到的正在跑的宿主。
pub(crate) struct Running {
    pub(crate) build: BuildId,
    /// 协议对不上（`Incompatible`）时宿主不报进程号，为 `None`。
    pub(crate) pid: Option<u32>,
    /// 单独一个进程跑，不在某个 app 的进程里；协议对不上时不知道，当作是。
    pub(crate) standalone: bool,
}

/// 问 socket 上有没有宿主在跑、是哪个构建：握完手就断开，协议对不上也算在跑。socket 不在或连接被拒
/// （宿主没在听）时为 `None`，别的连不上（比如没有权限）报错。
pub(crate) fn probe(env: &Env) -> Result<Option<Running>> {
    let Some(socket) = env.socket.as_deref() else { return Ok(None) };
    if !socket.exists() {
        return Ok(None);
    }
    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("cannot connect to {}", socket.display())),
    };
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    let hello = Frame::control(&ClientMsg::Hello {
        protocol: PROTOCOL_VERSION,
        build: BuildId(env.build.clone()),
        client: ClientKind::Cli,
        caps: Caps::default(),
        session: None,
        device: None,
    })?;
    write_frame(&mut stream, hello.kind, hello.channel, &hello.payload)?;
    loop {
        let frame = read_frame(&mut stream)?.ok_or_else(|| anyhow!(CLOSED))?;
        if frame.kind != FrameKind::Control {
            continue;
        }
        return match frame.message::<HostMsg>()? {
            HostMsg::Welcome { build, host_pid, standalone, .. } => {
                Ok(Some(Running { build, pid: Some(host_pid), standalone }))
            }
            HostMsg::Incompatible { build, .. } => Ok(Some(Running { build, pid: None, standalone: true })),
            other => bail!("unexpected answer to hello: {}", kind(&other)),
        };
    }
}

pub(crate) struct Connection {
    stream: UnixStream,
    messages: mpsc::Receiver<HostMsg>,
    /// 等回话时跳过去的会话事件（状态、命令结束这些），按到达的先后，`next` 先交出它们：先连上
    /// 会话再发请求的命令，等回话期间会话变了的话不会漏掉。
    pending: RefCell<VecDeque<HostMsg>>,
    /// 等回话时见过 `Exited` 的会话，见 `exited`。
    exited: RefCell<HashSet<SessionId>>,
    /// 等回话时见过 `Resync` 的会话，见 `lost`。
    lost: RefCell<HashSet<SessionId>>,
    /// 下一个请求的编号。
    next_req: Cell<u32>,
    /// 宿主认得看板的几条消息（`Welcome` 的 `todos`）。
    pub(crate) todos: bool,
}

impl Connection {
    /// 连上宿主并握手；连不上、给了 `Env::launch_host` 时先拉起宿主。
    pub(crate) fn open(env: &Env) -> Result<Self> {
        Self::connect(env, Instant::now() + REPLY_TIMEOUT, true)
    }

    /// 同 `open`，但握手最多等到 `deadline`，也不拉起宿主：补全这类不能久等的用它。
    pub(crate) fn open_by(env: &Env, deadline: Instant) -> Result<Self> {
        Self::connect(env, deadline, false)
    }

    fn connect(env: &Env, mut deadline: Instant, launch: bool) -> Result<Self> {
        let socket = env.socket.as_deref().ok_or_else(|| anyhow!("no place to find the runode app's socket"))?;
        let stream = match (UnixStream::connect(socket), env.launch_host.as_deref().filter(|_| launch)) {
            (Ok(stream), _) => stream,
            (Err(_), Some(exe)) => {
                let stream = launch_host(exe, socket, deadline)?;
                // 拉起花掉的时间不算进握手的期限。
                deadline = Instant::now() + REPLY_TIMEOUT;
                stream
            }
            (Err(err), None) => {
                return Err(err)
                    .with_context(|| format!("cannot reach the runode app at {}; is it running?", socket.display()));
            }
        };
        let mut reader = stream.try_clone()?;
        let (tx, messages) = mpsc::channel();
        thread::Builder::new().name("runode-reader".into()).spawn(move || {
            while let Ok(Some(frame)) = read_frame(&mut reader) {
                if frame.kind != FrameKind::Control {
                    continue;
                }
                let Ok(message) = frame.message::<HostMsg>() else { continue };
                if tx.send(message).is_err() {
                    break;
                }
            }
        })?;
        let mut connection = Self {
            stream,
            messages,
            pending: RefCell::default(),
            exited: RefCell::default(),
            lost: RefCell::default(),
            next_req: Cell::new(1),
            todos: false,
        };
        connection.send(&ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            build: BuildId(env.build.clone()),
            client: ClientKind::Cli,
            // 用不着 shell 集成报告的 PATH 和函数名，没变时不必再收。
            caps: Caps { shell_report: true, ..Caps::default() },
            // 宿主据此记下是哪个终端里的程序在操作别的终端，见 `SessionMeta::driver`。
            session: env.session.as_deref().and_then(|own| own.parse().ok()),
            // 命令行不决定会话的尺寸，用不着设备名。
            device: None,
        })?;
        match connection.reply_by(deadline)? {
            HostMsg::Welcome { todos, .. } => {
                connection.todos = todos;
                Ok(connection)
            }
            HostMsg::Incompatible { reason, .. } => bail!("the runode app speaks another protocol: {reason}"),
            other => bail!("unexpected answer to hello: {}", kind(&other)),
        }
    }

    pub(crate) fn send(&self, message: &ClientMsg) -> Result<()> {
        let frame = Frame::control(message)?;
        write_frame(&mut &self.stream, frame.kind, frame.channel, &frame.payload)
            .map_err(|err| anyhow!("lost the runode app: {err}"))
    }

    pub(crate) fn input(&self, channel: u32, data: &[u8]) -> Result<()> {
        write_frame(&mut &self.stream, FrameKind::Input, channel, data)
            .map_err(|err| anyhow!("lost the runode app: {err}"))
    }

    /// 下一条消息，到 `deadline` 还没有时为 `None`；为 `None` 的 `deadline` 一直等。宿主说
    /// `Goodbye` 时是错误，见 `goodbye`。
    pub(crate) fn next(&self, deadline: Option<Instant>) -> Result<Option<HostMsg>> {
        if let Some(message) = self.pending.borrow_mut().pop_front() {
            return Ok(Some(message));
        }
        let received = match deadline {
            Some(at) => match self.messages.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(message) => Ok(message),
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
                Err(mpsc::RecvTimeoutError::Disconnected) => Err(()),
            },
            None => self.messages.recv().map_err(|_| ()),
        };
        match received {
            Ok(HostMsg::Goodbye { reason }) => Err(goodbye(&reason)),
            Ok(message) => Ok(Some(message)),
            Err(()) => Err(anyhow!(CLOSED)),
        }
    }

    /// 回话：跳过 `Meta` 这类随时会插进来的会话事件，宿主报错时返回错误。
    pub(crate) fn reply(&self) -> Result<HostMsg> {
        self.reply_by(Instant::now() + REPLY_TIMEOUT)
    }

    /// 同 `reply`，但给了 `deadline` 时不等过它：按用户给的超时等的命令用它。
    pub(crate) fn reply_before(&self, deadline: Option<Instant>) -> Result<HostMsg> {
        let by = Instant::now() + REPLY_TIMEOUT;
        self.reply_by(deadline.map_or(by, |at| at.min(by)))
    }

    /// 同 `reply`，但等到 `deadline`：宿主要办好一阵子的请求（`StartTodo` 可能先跑 `git worktree add`）用它。
    pub(crate) fn reply_by(&self, deadline: Instant) -> Result<HostMsg> {
        self.answer(deadline)?.map_err(|message| anyhow!(message))
    }

    /// 回话，宿主报的错放在里层，由调用方决定怎么办。跳过的事件里有会话结束（`Exited`）、宿主摘掉了
    /// 订阅（`Resync`）时记下来。
    /// 宿主说 `Goodbye` 时是外层的错误，见 `goodbye`。
    fn answer(&self, deadline: Instant) -> Result<Result<HostMsg, String>> {
        loop {
            // 不从 `pending` 取：那里的都是已经跳过的事件。
            let message = match self.messages.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(message) => message,
                Err(mpsc::RecvTimeoutError::Timeout) => bail!("the runode app did not answer"),
                Err(mpsc::RecvTimeoutError::Disconnected) => bail!(CLOSED),
            };
            match message {
                HostMsg::Exited { id, .. } => {
                    self.exited.borrow_mut().insert(id);
                    self.pending.borrow_mut().push_back(message);
                }
                HostMsg::Meta { .. }
                | HostMsg::CommandFinished { .. }
                | HostMsg::Bell { .. }
                | HostMsg::Notify { .. }
                | HostMsg::Resized { .. }
                | HostMsg::ResizeWindow { .. }
                | HostMsg::ThemeApplied { .. }
                | HostMsg::SnapshotEnd { .. } => self.pending.borrow_mut().push_back(message),
                HostMsg::Resync { id, .. } => {
                    self.lost.borrow_mut().insert(id);
                    self.pending.borrow_mut().push_back(message);
                }
                // 问过 `Layout` 以后 app 的布局一变就来一条 `LayoutChanged`，问过 `ListTodos` 以后看板
                // 一变就来一条 `TodosChanged`，都不是回话。
                HostMsg::UiRequest { .. } | HostMsg::LayoutChanged | HostMsg::TodosChanged | HostMsg::Unknown => {}
                HostMsg::Error { message, .. } => return Ok(Err(message)),
                HostMsg::Goodbye { reason } => return Err(goodbye(&reason)),
                message => return Ok(Ok(message)),
            }
        }
    }

    /// 等回话时见过这个会话结束了。
    pub(crate) fn exited(&self, id: SessionId) -> bool {
        self.exited.borrow().contains(&id)
    }

    /// 等回话时见过宿主摘掉了这个会话的订阅（`Resync`）：之后它的状态和结束都收不到了。
    pub(crate) fn lost(&self, id: SessionId) -> bool {
        self.lost.borrow().contains(&id)
    }

    /// 丢掉等回话时跳过的事件：一遍遍读屏幕的命令用不着它们，免得越攒越多。
    pub(crate) fn drop_pending(&self) {
        self.pending.borrow_mut().clear();
    }

    /// 新请求的编号。
    pub(crate) fn req(&self) -> u32 {
        let req = self.next_req.get();
        self.next_req.set(req.wrapping_add(1).max(1));
        req
    }

    /// 发一个只回 `Done` 的请求（`SendKeys`、`Paste` 这类），等它办完。
    pub(crate) fn request_done(&self, message: &ClientMsg) -> Result<()> {
        self.send(message)?;
        match self.reply()? {
            HostMsg::Done { .. } => Ok(()),
            other => bail!("unexpected answer: {}", kind(&other)),
        }
    }

    /// app 里各个终端摆在哪；app 没开窗口、回答不了时为 `None`。
    pub(crate) fn layout(&self) -> Result<Option<Vec<WindowLayout>>> {
        let req = self.req();
        self.send(&ClientMsg::Layout { req })?;
        match self.answer(Instant::now() + REPLY_TIMEOUT)? {
            Ok(HostMsg::Layout { windows, .. }) => Ok(Some(windows)),
            Ok(other) => bail!("unexpected answer: {}", kind(&other)),
            Err(_) => Ok(None),
        }
    }
}

impl Drop for Connection {
    /// 关掉连接，读的线程随之结束，宿主那边也知道这条连接没了。
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// 消息的种类，报错时用。
pub(crate) fn kind(message: &HostMsg) -> String {
    serde_json::to_value(message)
        .ok()
        .and_then(|value| value.get("type").and_then(|kind| kind.as_str()).map(str::to_owned))
        .unwrap_or_else(|| "message".into())
}
