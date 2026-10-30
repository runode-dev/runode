//! 经 SSH 连的远端主机：每台一条 `Link`，接到那台机器上的宿主（`runode --ssh-bridge`，见 `bridge`）。
//! 本机那条（`host_client::link`）照旧，这里另管远端的。
//!
//! 每台主机一个 `remote-host` 线程（`supervise`）管连接：要用时连上；主连接（ControlMaster）没开着就先开，
//! 要认证时经 askpass 问桌面（`askpass`）；远端没装这个构建就装上（`deploy`）；连上后 ssh 子进程的 stdin、
//! stdout 和一对本地 socket 互搬，`Link` 接在这对 socket 的一头上。断了按退避阶梯（`RETRY_DELAYS`）重连，
//! 认证没过、主机密钥对不上这类要用户来办的不自己重试，等 `retry_now`。
//!
//! 连接的状态（`Status`）变了、连上了、要问用户，都经 `events` 告诉界面。

mod askpass;
pub(crate) mod deploy;
pub(crate) mod forward;
pub(crate) mod upload;

use std::{
    collections::{HashMap, HashSet},
    fmt,
    fs::File,
    io::{self, BufRead as _, BufReader, Read, Write},
    net::Shutdown,
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Condvar, LazyLock, Mutex, PoisonError},
    thread,
    time::{Duration, Instant},
};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use runode_protocol::{ClientMsg, SessionId};
use runode_ssh::{
    Destination, Ssh,
    bridge::{Start, read_start},
};

use super::{Link, Timeouts, UnixStream, build};

pub use askpass::Ask;

/// 一个终端、一个工作区在哪台机器上。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HostId {
    /// 这台机器（`host_client::link`）。
    Local,
    /// 经 SSH 连的主机，按用户填的连接目标（`Destination` 的写法）认。
    Ssh(String),
}

impl HostId {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    /// 侧栏、提示里显示的主机名；本机为空。
    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::Ssh(name) => Some(name),
        }
    }
}

/// 到 `host` 的那条连接；远端的头一回用时开始连。
pub fn link_of(host: &HostId) -> Link {
    match host {
        HostId::Local => super::link().clone(),
        HostId::Ssh(name) => remote(name).map_or_else(|| super::link().clone(), |remote| remote.link.clone()),
    }
}

/// 远端主机 `name`（`Destination` 的写法）；认不出这个写法时为空。头一回取时开始连。
pub fn remote(name: &str) -> Option<Arc<RemoteHost>> {
    let mut hosts = HOSTS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(host) = hosts.get(name) {
        return Some(host.clone());
    }
    let dest = Destination::parse(name)?;
    let host = Arc::new(RemoteHost::new(name.to_owned(), dest));
    hosts.insert(name.to_owned(), host.clone());
    drop(hosts);
    host.start();
    Some(host)
}

/// 已经有的远端主机 `name`；没有时不新建，也不开始连。
pub fn find(name: &str) -> Option<Arc<RemoteHost>> {
    HOSTS.lock().unwrap_or_else(PoisonError::into_inner).get(name).cloned()
}

/// 忘掉主机 `name`：断开，让主连接退出，不再列在 `remotes` 里；之后再用它时从头连。会阻塞。
pub fn forget(name: &str) {
    let Some(host) = HOSTS.lock().unwrap_or_else(PoisonError::into_inner).remove(name) else {
        return;
    };
    host.state().forgotten = true;
    host.disconnect();
    emit(RemoteEvent::Forgotten(name.to_owned()));
}

/// 已经连过（或正连着）的远端主机。
pub fn remotes() -> Vec<Arc<RemoteHost>> {
    let hosts = HOSTS.lock().unwrap_or_else(PoisonError::into_inner);
    let mut hosts: Vec<_> = hosts.values().cloned().collect();
    hosts.sort_by(|a, b| a.name.cmp(&b.name));
    hosts
}

/// 远端主机上发生的、界面要知道的事。只能取一次。
pub fn events() -> Option<UnboundedReceiver<RemoteEvent>> {
    EVENTS.1.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// 把配置里宿主关心的部分（`messages`）告诉各台远端主机的宿主，之后新连的主机也照它。
pub fn configure(messages: Vec<ClientMsg>) {
    for host in remotes() {
        for message in &messages {
            host.link.send(message.clone());
        }
    }
    *HOST_OPTIONS.lock().unwrap_or_else(PoisonError::into_inner) = messages;
}

/// 怎么调 ssh，跟着配置走。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshSettings {
    /// ssh 程序。
    pub program: PathBuf,
    /// 主连接空闲多久后退出（秒）。
    pub persist_secs: u32,
}

impl Default for SshSettings {
    fn default() -> Self {
        Self { program: PathBuf::from("ssh"), persist_secs: runode_ssh::DEFAULT_PERSIST_SECS }
    }
}

/// 换上配置里 ssh 的设置，之后的连接按它。
pub fn set_ssh_settings(settings: SshSettings) {
    *SETTINGS.lock().unwrap_or_else(PoisonError::into_inner) = settings;
}

pub(crate) fn ssh() -> Ssh {
    let settings = SETTINGS.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let dirs = runode_paths::Dirs::from_env();
    // Windows 自带的 OpenSSH 不支持 ControlMaster。
    let control = if cfg!(unix) && dirs.create_ssh_control_dir().is_ok() { dirs.ssh_control_path() } else { None };
    Ssh { program: settings.program, control, persist_secs: settings.persist_secs }
}

/// 界面要知道的事。
#[derive(Debug)]
pub enum RemoteEvent {
    /// 这台主机的连接状态变了，见 `RemoteHost::status`。
    Status(String),
    /// 连上了这台主机（第一次或断开后又连上），`alive` 是那时它的宿主里还活着的会话。
    Connected { host: String, alive: HashSet<SessionId> },
    /// ssh 要问用户。
    Ask(Ask),
    /// 这台主机被忘掉了（`forget`）。
    Forgotten(String),
}

/// 连接的状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// 没在连，也没要连。
    Idle,
    /// 正连着，做到了哪一步。
    Connecting(Phase),
    Connected(Connected),
    /// 断了或没连上，`retry_at` 时再试，这是第 `attempt` 次。
    Waiting {
        attempt: u32,
        retry_at: Instant,
        error: String,
    },
    /// 要用户来办才能再连（认证没过、主机密钥变了、平台不支持、装不上），等 `retry_now`。
    Stopped {
        error: String,
        auth: bool,
    },
}

/// 连上了的远端宿主。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connected {
    /// 远端宿主的构建。
    pub build: String,
    /// 远端宿主不是这个构建（升级时那边还连着装旧版本的桌面）。
    pub mismatch: bool,
    /// 远端宿主跟着 ssh 的登录会话走，最后一个连接断开时可能被系统结束，见 `runode_ssh::bridge::Opening`。
    pub session_bound: bool,
}

/// 连接做到了哪一步。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// 连 ssh、认证。
    Authenticating,
    /// 看远端装没装这个构建。
    Probing,
    /// 准备要装上去的 runode（下载），`done`/`total` 字节。
    Downloading { done: u64, total: u64 },
    /// 把 runode 传到远端，`done`/`total` 字节。
    Uploading { done: u64, total: u64 },
    /// 起桥接、连远端的宿主。
    Starting,
}

/// 第几次重连前等多久；用完了按最后一个。
const RETRY_DELAYS: [Duration; 6] = [
    Duration::ZERO,
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];
/// 重连的退避用完以后每隔这么久试一次。
const RETRY_LAST: Duration = Duration::from_secs(60);
/// 连上撑过这么久算稳了，下次断开从头退避。
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// 连着时每隔这么久问一次宿主还在不在。
const HEARTBEAT: Duration = Duration::from_secs(15);
/// 心跳最多等这么久回话，等不到按断开处理。
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
/// 重连上以后列会话最多等这么久。
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

static HOSTS: LazyLock<Mutex<HashMap<String, Arc<RemoteHost>>>> = LazyLock::new(Mutex::default);
/// 配置里宿主关心的部分（`SetTheme`、`SetOptions`），见 `configure`。
static HOST_OPTIONS: Mutex<Vec<ClientMsg>> = Mutex::new(Vec::new());
static SETTINGS: LazyLock<Mutex<SshSettings>> = LazyLock::new(Mutex::default);
#[allow(clippy::type_complexity)]
static EVENTS: LazyLock<(UnboundedSender<RemoteEvent>, Mutex<Option<UnboundedReceiver<RemoteEvent>>>)> =
    LazyLock::new(|| {
        let (tx, rx) = unbounded();
        (tx, Mutex::new(Some(rx)))
    });

fn emit(event: RemoteEvent) {
    let _ = EVENTS.0.unbounded_send(event);
}

/// 一台远端主机。
pub struct RemoteHost {
    /// 用户填的连接目标，原样。
    pub name: String,
    pub dest: Destination,
    pub link: Link,
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    status: Status,
    /// 要连着；`disconnect` 后为假，supervisor 等到再要连。
    wanted: bool,
    /// 不等退避，马上再试。
    hurry: bool,
    /// 连着时的 ssh 子进程，断开（`disconnect`）时结束它。
    child: Option<u32>,
    /// 被 `forget` 了，supervisor 不再等着要连，退出。
    forgotten: bool,
}

impl fmt::Debug for RemoteHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteHost").field("name", &self.name).finish_non_exhaustive()
    }
}

impl RemoteHost {
    fn new(name: String, dest: Destination) -> Self {
        let link = Link::with_timeouts(build(), Timeouts::REMOTE);
        // `Link` 记着最近一次的，连上时补发。
        for message in HOST_OPTIONS.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            link.send(message.clone());
        }
        Self {
            name,
            dest,
            link,
            state: Mutex::new(State {
                status: Status::Idle,
                wanted: true,
                hurry: false,
                child: None,
                forgotten: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn status(&self) -> Status {
        self.state().status.clone()
    }

    fn set_status(&self, status: Status) {
        let mut state = self.state();
        if state.status == status {
            return;
        }
        state.status = status;
        drop(state);
        emit(RemoteEvent::Status(self.name.clone()));
    }

    fn start(self: &Arc<Self>) {
        let host = self.clone();
        let spawned = thread::Builder::new().name("remote-host".into()).spawn(move || host.supervise());
        if let Err(err) = spawned {
            tracing::error!("cannot start watching {}: {err}", self.name);
        }
    }

    /// 不等退避马上再连；停着的（要用户来办的）也再试一次，这次可以问用户。
    pub fn retry_now(&self) {
        let mut state = self.state();
        state.wanted = true;
        state.hurry = true;
        self.changed.notify_all();
    }

    /// 断开，让主连接退出，不再自己重连，直到 `retry_now`。远端的会话照常跑着。会阻塞。
    pub fn disconnect(&self) {
        let mut state = self.state();
        state.wanted = false;
        state.hurry = false;
        if let Some(pid) = state.child.take() {
            crate::subprocess::kill(pid);
        }
        self.changed.notify_all();
        drop(state);
        // 主连接上开的别的通道也要断。
        self.close_master();
    }

    /// 让主连接（ControlMaster）退出；不用主连接时什么都不做。会阻塞。
    pub fn close_master(&self) {
        if ssh().control.is_none() {
            return;
        }
        let mut command = ssh().exit(&self.dest);
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        crate::subprocess::hide_window(&mut command);
        let _ = command.status();
    }

    /// 管这台主机的连接，见模块说明。
    fn supervise(self: Arc<Self>) {
        let mut attempt = 0u32;
        loop {
            if !self.state().wanted {
                self.set_status(Status::Idle);
            }
            let interactive = {
                let mut state = self.state();
                while !state.wanted {
                    if state.forgotten {
                        return;
                    }
                    state = self.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
                }
                std::mem::take(&mut state.hurry) || attempt == 0
            };
            let started = Instant::now();
            let failure = match self.connect_once(interactive) {
                Ok(running) => {
                    self.watch(running);
                    if started.elapsed() >= STABLE_AFTER {
                        attempt = 0;
                    }
                    None
                }
                Err(failure) => Some(failure),
            };
            if !self.state().wanted {
                self.set_status(Status::Idle);
                continue;
            }
            let error = match failure {
                Some(Failure::Stop { error, auth }) => {
                    tracing::warn!("connecting to {} needs the user: {error}", self.name);
                    self.set_status(Status::Stopped { error, auth });
                    self.wait_for_retry();
                    attempt = 0;
                    continue;
                }
                Some(Failure::Retry(error)) => error,
                None => "lost the connection".to_owned(),
            };
            let delay = RETRY_DELAYS.get(attempt as usize).copied().unwrap_or(RETRY_LAST);
            attempt += 1;
            tracing::info!("reconnecting to {} in {delay:?} (attempt {attempt}): {error}", self.name);
            self.set_status(Status::Waiting { attempt, retry_at: Instant::now() + delay, error });
            self.sleep(delay);
        }
    }

    /// 等用户要再试（`retry_now`）或断开（`disconnect`）。
    fn wait_for_retry(&self) {
        let mut state = self.state();
        while state.wanted && !state.hurry {
            state = self.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// 等 `delay`，被 `retry_now`、`disconnect` 叫醒时提前回来。
    fn sleep(&self, delay: Duration) {
        let deadline = Instant::now() + delay;
        let mut state = self.state();
        while state.wanted && !state.hurry {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            state = self.changed.wait_timeout(state, deadline - now).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// 连一次：开主连接、看远端装没装、需要时装上、起桥接，把 `Link` 接上去。
    fn connect_once(self: &Arc<Self>, interactive: bool) -> Result<Running, Failure> {
        self.set_status(Status::Connecting(Phase::Authenticating));
        let ssh = ssh();
        let log = self.log_file();
        if ssh.control.is_some() {
            self.open_master(&ssh, interactive, &log)?;
        }
        let dir = deploy::install_dir();
        let mut deployed = false;
        loop {
            self.set_status(Status::Connecting(if deployed { Phase::Starting } else { Phase::Probing }));
            let mut command =
                ssh.exec(&self.dest, &runode_ssh::bootstrap::connect_command(&dir), ssh.control.is_some());
            if ssh.control.is_none() {
                self.ask_through(&mut command, interactive);
            }
            let mut child = spawn_piped(&mut command, &log)?;
            let stdout = child.stdout.take().ok_or_else(|| Failure::Retry("ssh gave no output".into()))?;
            let mut stdout = BufReader::with_capacity(256 << 10, stdout);
            let start = read_start(&mut stdout).map_err(|err| Failure::Retry(format!("cannot read from ssh: {err}")));
            match start {
                Ok(Start::Bridge(opening)) => {
                    tracing::info!("bridged to {}: {opening:?}", self.name);
                    let pid = child.id();
                    self.state().child = Some(pid);
                    let running = Running::start(child, stdout, &self.link).map_err(|err| {
                        crate::subprocess::kill(pid);
                        Failure::Retry(format!("cannot talk to the host on {}: {err}", self.name))
                    })?;
                    let mismatch = opening.build != build().0;
                    let connected = Connected { build: opening.build, mismatch, session_bound: opening.session_bound };
                    self.connected(connected);
                    return Ok(running);
                }
                Ok(Start::Failed(failure)) => {
                    let _ = child.wait();
                    return Err(Failure::Retry(format!("{}: {}", failure.code, failure.message)));
                }
                Ok(Start::Probe(platform)) => {
                    let status = child.wait().map_err(|err| Failure::Retry(err.to_string()))?;
                    let Some(platform) = platform.filter(|_| status.code() == Some(runode_ssh::bootstrap::PROBE_EXIT))
                    else {
                        return Err(classify(&log, status.code()));
                    };
                    if deployed {
                        return Err(Failure::Stop {
                            error: "runode did not start after installing it".into(),
                            auth: false,
                        });
                    }
                    deploy::install(self, &ssh, &platform, &dir, &log)?;
                    deployed = true;
                }
                Err(failure) => {
                    let _ = child.wait();
                    return Err(failure);
                }
            }
        }
    }

    /// 连上了：记下状态，告诉界面那边还活着的会话，断开着的终端接回来。
    fn connected(&self, connected: Connected) {
        self.set_status(Status::Connected(connected));
        let alive = match self.link.list_sessions(LIST_TIMEOUT) {
            Ok(sessions) => sessions.into_iter().filter(|session| !session.exited).map(|session| session.id).collect(),
            Err(err) => {
                tracing::warn!("cannot list the sessions on {}: {err}", self.name);
                HashSet::new()
            }
        };
        emit(RemoteEvent::Connected { host: self.name.clone(), alive });
    }

    /// 连着时一直等到断开：ssh 退出、桥接断了，或者心跳等不到回话。
    fn watch(&self, mut running: Running) {
        loop {
            match running.child.try_wait() {
                Ok(Some(status)) => {
                    tracing::info!("ssh to {} exited: {status}", self.name);
                    break;
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!("cannot wait for ssh to {}: {err}", self.name);
                    break;
                }
            }
            if !self.state().wanted || !self.link.connected() {
                break;
            }
            if running.since_beat.elapsed() >= HEARTBEAT {
                running.since_beat = Instant::now();
                if !self.link.flush(HEARTBEAT_TIMEOUT) {
                    tracing::warn!("{} did not answer the heartbeat", self.name);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(250));
        }
        running.stop();
        self.state().child = None;
    }

    /// 开主连接（ControlMaster）：已经开着就复用。`interactive` 时要认证可以问用户，否则不问，要认证就停下等用户。
    fn open_master(&self, ssh: &Ssh, interactive: bool, log: &std::path::Path) -> Result<(), Failure> {
        let mut check = ssh.check(&self.dest);
        check.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        crate::subprocess::isolate(&mut check);
        if check.status().is_ok_and(|status| status.success()) {
            return Ok(());
        }
        let mut master = ssh.master(&self.dest, !interactive);
        if interactive {
            self.ask_through(&mut master, true);
        }
        let stderr = File::create(log).map_err(|err| Failure::Retry(format!("cannot write the ssh log: {err}")))?;
        master.stdin(Stdio::null()).stdout(Stdio::null()).stderr(stderr);
        crate::subprocess::isolate(&mut master);
        let status = master.status().map_err(|err| Failure::Stop {
            error: format!("cannot run {}: {err}", ssh.program.display()),
            auth: false,
        })?;
        if status.success() { Ok(()) } else { Err(classify(log, status.code())) }
    }

    /// 让 `command` 起的 ssh 要问用户时经 askpass 问桌面；`interactive` 为假时不问。
    fn ask_through(&self, command: &mut Command, interactive: bool) {
        if !interactive {
            command.args(["-o", "BatchMode=yes"]);
            return;
        }
        if let Some((socket, token)) = askpass::register(&self.name)
            && let Ok(exe) = askpass::program()
        {
            runode_ssh::askpass::configure(command, &exe, &socket, &token);
        }
    }

    fn log_file(&self) -> PathBuf {
        let dirs = runode_paths::Dirs::from_env();
        let dir = dirs.ssh_log_dir().unwrap_or_else(std::env::temp_dir);
        let _ = std::fs::create_dir_all(&dir);
        let name: String = self
            .name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') { c } else { '_' })
            .collect();
        dir.join(format!("{name}.log"))
    }
}

/// 一次连接为什么没连上。
#[derive(Debug)]
pub(crate) enum Failure {
    /// 过一会儿再试可能就好了。
    Retry(String),
    /// 要用户来办；`auth` 是认证的问题。
    Stop { error: String, auth: bool },
}

/// 按 ssh 的 stderr（写在 `log` 里）和退出码说清为什么没连上。
fn classify(log: &std::path::Path, code: Option<i32>) -> Failure {
    let stderr = std::fs::read_to_string(log).unwrap_or_default();
    let failure = runode_ssh::Failure::of(&stderr);
    let error = match &failure {
        runode_ssh::Failure::Other(last) if last.is_empty() => format!("ssh exited with {code:?}"),
        runode_ssh::Failure::Other(last) => last.clone(),
        other => {
            format!("{other:?}: {}", stderr.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("").trim())
        }
    };
    if failure.retryable() { Failure::Retry(error) } else { Failure::Stop { error, auth: true } }
}

/// 起 `command`：stdin、stdout 接管道，stderr 写进 `log`，自成一个进程组。
fn spawn_piped(command: &mut Command, log: &std::path::Path) -> Result<Child, Failure> {
    let stderr = File::create(log).map_err(|err| Failure::Retry(format!("cannot write the ssh log: {err}")))?;
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(stderr);
    crate::subprocess::isolate(command);
    command.spawn().map_err(|err| Failure::Stop { error: format!("cannot run ssh: {err}"), auth: false })
}

/// 连着的一次：ssh 子进程，和接在它与 `Link` 之间的一对 socket。
struct Running {
    child: Child,
    /// 本地这对 socket 里搬字节的那一头。
    theirs: UnixStream,
    since_beat: Instant,
}

impl Running {
    /// 起两个搬字节的线程，把 `Link` 接在另一头上。
    fn start(mut child: Child, stdout: BufReader<impl Read + Send + 'static>, link: &Link) -> io::Result<Self> {
        let stdin = child.stdin.take().ok_or_else(|| io::Error::other("ssh has no stdin"))?;
        let (ours, theirs) = runode_host::socket_pair()?;
        let to_link = theirs.try_clone()?;
        let from_link = theirs.try_clone()?;
        thread::Builder::new().name("ssh-pump-out".into()).spawn(move || {
            pump_out(stdout, &to_link);
            let _ = to_link.shutdown(Shutdown::Both);
        })?;
        thread::Builder::new().name("ssh-pump-in".into()).spawn(move || pump_in(&from_link, stdin))?;
        link.connect(ours).map_err(|err| io::Error::other(err.to_string()))?;
        Ok(Self { child, theirs, since_beat: Instant::now() })
    }

    /// 断开：关掉 socket（`Link` 读到结尾，各会话收到 `Lost`），结束 ssh。
    fn stop(mut self) {
        let _ = self.theirs.shutdown(Shutdown::Both);
        if self.child.try_wait().ok().flatten().is_none() {
            crate::subprocess::kill(self.child.id());
        }
        let _ = self.child.wait();
    }
}

/// ssh 的 stdout 搬进 socket，读到结尾为止。
fn pump_out(mut from: BufReader<impl Read>, mut to: &UnixStream) {
    // 读开场白时多读进缓冲的先写出去。
    let buffered = from.buffer().to_vec();
    from.consume(buffered.len());
    if to.write_all(&buffered).is_err() {
        return;
    }
    let mut from = from.into_inner();
    let mut buffer = vec![0; 256 << 10];
    loop {
        match from.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if to.write_all(&buffer[..n]).is_err() {
                    return;
                }
            }
        }
    }
}

/// socket 搬到 ssh 的 stdin，每块都 flush 出去；读到结尾就关掉 stdin，远端的桥接随之退出。
fn pump_in(mut from: &UnixStream, mut to: ChildStdin) {
    let mut buffer = vec![0; 256 << 10];
    loop {
        match from.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if to.write_all(&buffer[..n]).and_then(|()| to.flush()).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use futures::StreamExt as _;

    use super::{RemoteEvent, Status, events, forward, remote, ssh, upload};

    fn wait_for(host: &super::RemoteHost, timeout: Duration, done: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + timeout;
        loop {
            let status = host.status();
            if done(&status) || Instant::now() >= deadline {
                return status;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// 对着真的 SSH 主机连：`RUNODE_TEST_SSH_HOST` 是连接目标（要免密），`RUNODE_SSH_BINARY` 指向那台机器上
    /// 跑得起来的 runode。连上、列会话，掐断 ssh 后自己重连上。
    #[test]
    #[ignore = "needs an ssh host"]
    fn connects_and_reconnects_to_a_real_host() {
        let name = std::env::var("RUNODE_TEST_SSH_HOST").expect("RUNODE_TEST_SSH_HOST");
        let mut events = events().expect("events");
        let host = remote(&name).expect("a destination");
        let status = wait_for(&host, Duration::from_secs(600), |status| {
            matches!(status, Status::Connected(_) | Status::Stopped { .. })
        });
        assert!(matches!(status, Status::Connected(_)), "not connected: {status:?}");
        host.link.list_sessions(Duration::from_secs(10)).expect("list sessions");

        let mut connected = || {
            futures::executor::block_on(async {
                while let Some(event) = events.next().await {
                    if let RemoteEvent::Connected { host, alive } = event {
                        return Some((host, alive));
                    }
                }
                None
            })
        };
        assert!(connected().is_some());
        let pid = host.state().child.expect("ssh is running");
        crate::subprocess::kill(pid);
        let reconnected = connected();
        assert_eq!(reconnected.map(|(host, _)| host), Some(name));
        assert!(matches!(host.status(), Status::Connected(_)));

        // 两个同名的文件各传到一个子目录里，目录连同里面的一起传。
        let local = std::env::temp_dir().join(format!("rn-upload-{}", std::process::id()));
        for dir in ["a", "b", "c/inner"] {
            std::fs::create_dir_all(local.join(dir)).unwrap();
        }
        std::fs::write(local.join("a/same.txt"), "from a").unwrap();
        std::fs::write(local.join("b/same.txt"), "from b").unwrap();
        std::fs::write(local.join("c/inner/deep.txt"), "deep").unwrap();
        let files = vec![local.join("a/same.txt"), local.join("b/same.txt"), local.join("c")];
        let paths = upload::upload(&host, upload::Upload::Files(files)).expect("upload");
        assert_ne!(paths[0], paths[1]);
        let read = |path: &str| {
            let output = ssh().exec(&host.dest, &format!("cat {path}"), true).output().unwrap();
            String::from_utf8(output.stdout).unwrap()
        };
        assert_eq!((read(&paths[0]), read(&paths[1])), ("from a".into(), "from b".into()));
        assert_eq!(read(&format!("{}/inner/deep.txt", paths[2])), "deep");
        let bytes = upload::Upload::Bytes { name: "shot.png".into(), bytes: b"png".to_vec() };
        let shot = upload::upload(&host, bytes).expect("upload bytes");
        assert_eq!(read(&shot[0]), "png");
        let _ = std::fs::remove_dir_all(&local);

        // 远端只在自己那里听的端口，经转发在本机打得开。
        let serve = "cd /tmp && (python3 -m http.server 18765 --bind 127.0.0.1 > /dev/null 2>&1 & echo $!)";
        let output = ssh().exec(&host.dest, serve, true).output().unwrap();
        let server = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        std::thread::sleep(Duration::from_secs(1));
        let url = forward::local_url(&host, "http://localhost:18765/").expect("forward");
        assert_eq!(forward::local_url(&host, "http://localhost:18765/").unwrap(), url);
        let port: u16 = url.trim_start_matches("http://localhost:").trim_end_matches('/').parse().unwrap();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        std::io::Write::write_all(&mut stream, b"GET / HTTP/1.0\r\n\r\n").unwrap();
        let mut answer = String::new();
        std::io::Read::read_to_string(&mut stream, &mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.0 200"), "{answer}");
        let _ = ssh().exec(&host.dest, &format!("kill {server}"), true).output();

        host.disconnect();
        host.close_master();
    }
}
