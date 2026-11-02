//! 配对过的电脑：别的电脑上开着远程访问的 Runode，经 `runode_remote::client` 连过去当一台远端主机用。每台一条
//! `Link`，接在 `client::dial` 交回的 socket 上，和那台电脑的宿主按 `ClientKind::Desktop` 说话。那台电脑上本来就
//! 跑着 Runode，不用装；构建不同时照样连（`Connected::mismatch`）。
//!
//! 每台一个 `paired-host` 线程（`supervise`）管连接，和 SSH 主机一样：要用时连，断了按 `RETRY_DELAYS` 退避重连，
//! 走完了还连不上就停下，这台电脑睡醒或网络变了时再试；那台电脑撤销了配对、证书变了这类要用户来办的不自己重试。
//! 停下的都等 `retry_now`。连接的状态变了、连上了，经 `hosts::events` 告诉界面。
//!
//! 读写项目、端口转发和远程桌面各自另拨一条连接（`PairedHost::open_channel`、`PairedHost::dial`），过了门禁后
//! 说各自的协议。断开这台（`disconnect`）或终端那条连接断了时，登记过的（`PairedHost::hold`）跟着关掉；端口转发
//! 的监听（`forward`）在断开这台时关。

pub(crate) mod forward;

use std::{
    collections::{HashMap, HashSet},
    net::Shutdown,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, PoisonError},
    thread,
    time::{Duration, Instant},
};

use runode_protocol::{
    ClientMsg, Frame, FrameKind, HostMsg, read_frame,
    remote::{ChannelHello, ChannelRefusal, ChannelReply, RejectReason},
    write_frame,
};
use runode_remote::client::{self, DialError, Dialed, PairedMachine};

use super::{
    ConnectError, Link, Timeouts, UnixStream, build,
    hosts::{
        self, Connected, HEARTBEAT, HEARTBEAT_TIMEOUT, HostId, LIST_TIMEOUT, Phase, RETRY_DELAYS, RemoteEvent,
        STABLE_AFTER, Status, emit,
    },
};

/// 认系统时问的、只有 macOS 上才有的目录。
const MACOS_ONLY_DIR: &str = "/System/Library/CoreServices";
/// 让那台电脑撤销这台设备（`ClientMsg::Unpair`）最多等这么久。
const UNPAIR_TIMEOUT: Duration = Duration::from_secs(10);
/// 开专用连接时等那边回 `ChannelReply` 最多这么久。
const CHANNEL_TIMEOUT: Duration = Duration::from_secs(10);

static HOSTS: LazyLock<Mutex<HashMap<String, Arc<PairedHost>>>> = LazyLock::new(Mutex::default);
/// 配对过的电脑，头一回要用时读 `client::list_machines`，之后配对、忘掉、连上时跟着改。
static MACHINES: Mutex<Option<Vec<PairedMachine>>> = Mutex::new(None);

fn dirs() -> runode_paths::Dirs {
    runode_paths::Dirs::from_env()
}

/// 配对过的电脑，按配对的先后。
pub fn machines() -> Vec<PairedMachine> {
    let mut machines = MACHINES.lock().unwrap_or_else(PoisonError::into_inner);
    machines.get_or_insert_with(|| client::list_machines(&dirs())).clone()
}

/// 配对过的、标识是 `id` 的那台。
pub fn machine(id: &str) -> Option<PairedMachine> {
    machines().into_iter().find(|machine| machine.id == id)
}

/// 记下这次连上（或刚配对）的 `machine`：那台电脑报的主机名、地址可能变了。
fn remember(machine: &PairedMachine) {
    let mut machines = MACHINES.lock().unwrap_or_else(PoisonError::into_inner);
    let machines = machines.get_or_insert_with(|| client::list_machines(&dirs()));
    match machines.iter_mut().find(|known| known.id == machine.id) {
        Some(known) => *known = machine.clone(),
        None => machines.push(machine.clone()),
    }
}

/// 配对过的电脑 `id`；没配对过时为空。头一回取时开始连。
pub fn remote(id: &str) -> Option<Arc<PairedHost>> {
    let mut hosts = HOSTS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(host) = hosts.get(id) {
        return Some(host.clone());
    }
    machine(id)?;
    let host = Arc::new(PairedHost::new(id.to_owned()));
    hosts.insert(id.to_owned(), host.clone());
    drop(hosts);
    host.start();
    hosts::watch_network_once();
    Some(host)
}

/// 已经有的配对的电脑 `id`；没有时不新建，也不开始连。
pub fn find(id: &str) -> Option<Arc<PairedHost>> {
    HOSTS.lock().unwrap_or_else(PoisonError::into_inner).get(id).cloned()
}

/// 这次运行里连过（或正连着）的配对的电脑，按标识排好。
pub fn remotes() -> Vec<Arc<PairedHost>> {
    let hosts = HOSTS.lock().unwrap_or_else(PoisonError::into_inner);
    let mut hosts: Vec<_> = hosts.values().cloned().collect();
    hosts.sort_by(|a, b| a.id.cmp(&b.id));
    hosts
}

/// 断开 `id` 那台，不再列在 `remotes` 里；配对的记录留着，之后再用它时从头连。
pub fn forget(id: &str) {
    let Some(host) = HOSTS.lock().unwrap_or_else(PoisonError::into_inner).remove(id) else {
        return;
    };
    host.state().forgotten = true;
    host.disconnect();
    emit(RemoteEvent::Forgotten(HostId::Paired(id.to_owned())));
}

/// 解除和 `id` 那台的配对：连着时先请那台电脑撤销这台设备（`ClientMsg::Unpair`），再断开、删掉这边的记录和设备
/// 私钥。那边没撤销成（没连着、没回话）时这边照样忘掉，交回那边没撤销成的原因，界面据此提醒用户去那台电脑上撤销。
/// 会阻塞。
pub fn unpair(id: &str) -> Result<(), UnpairError> {
    let remote = find(id).filter(|host| matches!(host.status(), Status::Connected(_)));
    let told = match remote {
        Some(host) => match host.link.request(|req| ClientMsg::Unpair { req }, UNPAIR_TIMEOUT) {
            Ok(HostMsg::Done { .. }) => Ok(()),
            Ok(other) => Err(UnpairError::NotRevoked(format!("unexpected answer: {other:?}"))),
            Err(err) => Err(UnpairError::NotRevoked(err.to_string())),
        },
        None => Err(UnpairError::NotRevoked("not connected".into())),
    };
    forget(id);
    client::forget_machine(&dirs(), id).map_err(UnpairError::Local)?;
    if let Some(machines) = MACHINES.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        machines.retain(|machine| machine.id != id);
    }
    emit(RemoteEvent::Forgotten(HostId::Paired(id.to_owned())));
    told
}

/// 解除配对没办全。
#[derive(Debug)]
pub enum UnpairError {
    /// 这边忘掉了，那台电脑那边没撤销成（没连着、没回话），带着原因。
    NotRevoked(String),
    /// 这边的记录没删掉。
    Local(std::io::Error),
}

/// 用配对链接 `uri` 配对，这边叫 `device_name`，成了就记下这台电脑、把这条连接接到它的 `Link` 上。交回它的标识。
/// 会阻塞。
pub fn pair(uri: &runode_protocol::remote::PairingUri, device_name: &str) -> Result<String, DialError> {
    let dialed = client::pair(&dirs(), uri, device_name)?;
    let id = dialed.machine.id.clone();
    remember(&dialed.machine);
    // 刚配对的连接交给 supervisor 下一次连时用，省一次拨号。
    let (host, new) = {
        let mut hosts = HOSTS.lock().unwrap_or_else(PoisonError::into_inner);
        match hosts.get(&id) {
            Some(host) => (host.clone(), false),
            None => {
                let host = Arc::new(PairedHost::new(id.clone()));
                hosts.insert(id.clone(), host.clone());
                (host, true)
            }
        }
    };
    *host.handed.lock().unwrap_or_else(PoisonError::into_inner) = Some(dialed);
    host.retry_now();
    if new {
        host.start();
        hosts::watch_network_once();
    }
    Ok(id)
}

/// 睡醒或网络变了：叫正在重连和重连放弃了的电脑马上再试（`PairedHost::nudge`）。
pub(super) fn nudge_all() {
    for host in remotes() {
        host.nudge();
    }
}

/// 一台配对的电脑。
pub struct PairedHost {
    /// `PairedMachine::id`。
    pub id: String,
    pub link: Link,
    state: Mutex<State>,
    changed: Condvar,
    /// 连着时最近一次心跳来回花的时间。
    latency: Mutex<Option<Duration>>,
    /// 刚配对时 `pair` 交来的连接，supervisor 第一次连时直接用它。
    handed: Mutex<Option<Dialed>>,
    /// 登记过的专用连接（`hold`），断开时一起关掉。
    channels: Mutex<Vec<UnixStream>>,
}

/// 专用连接开不了。
#[derive(Debug)]
pub enum ChannelError {
    Dial(DialError),
    /// 那边拒绝了，带着它说的原因。
    Refused(ChannelRefusal),
    /// 连接断了，或者那边回的读不懂。
    Broken(String),
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dial(err) => match classify(err) {
                Failure::Retry(text) | Failure::Stop(text) => f.write_str(&text),
            },
            Self::Refused(ChannelRefusal::BuildMismatch { build }) => {
                f.write_str(&rust_i18n::t!("paired.build_mismatch", build = build))
            }
            Self::Refused(ChannelRefusal::Unavailable { message }) => f.write_str(message),
            Self::Refused(other) => write!(f, "the other computer refused: {other:?}"),
            Self::Broken(reason) => f.write_str(reason),
        }
    }
}

struct State {
    status: Status,
    /// 要连着；`disconnect` 后为假，supervisor 等到再要连。
    wanted: bool,
    /// 不等退避，马上再试。
    hurry: bool,
    /// 睡醒或网络变了，不等退避马上再试（`nudge`）。
    nudged: bool,
    /// 重连的退避走完了停在 `Status::Stopped`，网络变了可以再试。
    gave_up: bool,
    /// 被 `forget` 了，supervisor 不再等着要连，退出。
    forgotten: bool,
    /// 连着时 `Link` 用的 socket 的另一份，断开（`disconnect`）时关掉它。
    stream: Option<UnixStream>,
    /// 那台电脑是 Windows，连上认出来以后记着。
    windows: bool,
}

/// 一次连接为什么没连上。
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// 过一会儿再试可能就好了。
    Retry(String),
    /// 要用户来办（重新配对、升级）。
    Stop(String),
}

impl std::fmt::Debug for PairedHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairedHost").field("id", &self.id).finish_non_exhaustive()
    }
}

impl PairedHost {
    fn new(id: String) -> Self {
        let link = Link::with_timeouts(build(), Timeouts::REMOTE);
        hosts::send_host_options(&link);
        Self {
            id,
            link,
            state: Mutex::new(State {
                status: Status::Idle,
                wanted: !hosts::holding_new_hosts(),
                hurry: false,
                nudged: false,
                gave_up: false,
                forgotten: false,
                stream: None,
                windows: false,
            }),
            changed: Condvar::new(),
            latency: Mutex::new(None),
            handed: Mutex::new(None),
            channels: Mutex::default(),
        }
    }

    /// 另拨一条过了门禁的连接，调用方在上面发第一帧（`ScreenHello` 这类）。连着时才拨。会阻塞。
    pub fn dial(&self) -> Result<UnixStream, ChannelError> {
        if !matches!(self.status(), Status::Connected(_)) {
            return Err(ChannelError::Broken(format!("not connected to the paired computer {}", self.id)));
        }
        client::dial(&dirs(), &self.id).map(|dialed| dialed.stream).map_err(ChannelError::Dial)
    }

    /// 开一条专用连接：拨号过门禁后发 `hello`、等那边回话，`ChannelReady` 后交回两边原样搬字节的 socket。会阻塞。
    pub fn open_channel(&self, hello: &ChannelHello) -> Result<UnixStream, ChannelError> {
        let broken = |err: &dyn std::fmt::Display| ChannelError::Broken(err.to_string());
        let mut stream = self.dial()?;
        let frame = Frame::control(hello).map_err(|err| broken(&err))?;
        write_frame(&mut stream, FrameKind::Control, 0, &frame.payload).map_err(|err| broken(&err))?;
        stream.set_read_timeout(Some(CHANNEL_TIMEOUT)).map_err(|err| broken(&err))?;
        // 只读这一帧，不能多读：`ChannelReady` 之后的字节是专用连接自己的。
        let reply = read_frame(&mut stream).map_err(|err| broken(&err))?;
        stream.set_read_timeout(None).map_err(|err| broken(&err))?;
        match reply.map(|frame| frame.message::<ChannelReply>()) {
            Some(Ok(ChannelReply::ChannelReady)) => Ok(stream),
            Some(Ok(ChannelReply::ChannelRefused { reason })) => Err(ChannelError::Refused(reason)),
            Some(Ok(ChannelReply::Unknown)) => Err(ChannelError::Refused(ChannelRefusal::Unknown)),
            Some(Err(err)) => Err(broken(&err)),
            None => Err(ChannelError::Broken("the other computer closed the connection".into())),
        }
    }

    /// 登记专用连接 `stream`：断开这台或终端那条连接断了时关掉它。
    pub fn hold(&self, stream: &UnixStream) {
        match stream.try_clone() {
            Ok(stream) => self.channels.lock().unwrap_or_else(PoisonError::into_inner).push(stream),
            Err(err) => tracing::warn!("cannot keep track of a connection to the paired computer {}: {err}", self.id),
        }
    }

    fn close_channels(&self) {
        for stream in self.channels.lock().unwrap_or_else(PoisonError::into_inner).drain(..) {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn host_id(&self) -> HostId {
        HostId::Paired(self.id.clone())
    }

    pub fn status(&self) -> Status {
        self.state().status.clone()
    }

    /// 连着时最近一次心跳到那台电脑的宿主来回花的时间；没连着、还没测过时为空。
    pub fn latency(&self) -> Option<Duration> {
        *self.latency.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 那台电脑是 Windows；还没连上过时不算。
    pub fn is_windows(&self) -> bool {
        self.state().windows
    }

    fn set_status(&self, status: Status) {
        let mut state = self.state();
        if state.status == status {
            return;
        }
        state.status = status;
        drop(state);
        emit(RemoteEvent::Status(self.host_id()));
    }

    fn start(self: &Arc<Self>) {
        let host = self.clone();
        let spawned = thread::Builder::new().name("paired-host".into()).spawn(move || host.supervise());
        if let Err(err) = spawned {
            tracing::error!("cannot start watching the paired computer {}: {err}", self.id);
        }
    }

    /// 不等退避马上再连；停着的（要用户来办的）也再试一次。停着、没在连的当场改成在连，调用方紧接着读到的不是
    /// 上回停下的状态。
    pub fn retry_now(&self) {
        let mut state = self.state();
        state.wanted = true;
        state.hurry = true;
        let woken = !state.forgotten && matches!(state.status, Status::Stopped { .. } | Status::Idle);
        self.wake(state, woken);
    }

    /// 睡醒或网络变了：正在等着重连的、重连放弃了的不等退避马上再试，退避从头算。要用户来办的、断开了的不动。
    fn nudge(&self) {
        let mut state = self.state();
        let stopped = matches!(state.status, Status::Stopped { .. });
        if !state.wanted || state.forgotten || !matches!(state.status, Status::Waiting { .. }) && !state.gave_up {
            return;
        }
        state.nudged = true;
        self.wake(state, stopped);
    }

    /// 叫醒 supervisor；`show` 时当场把状态改成在连。
    fn wake(&self, mut state: MutexGuard<'_, State>, show: bool) {
        if show {
            state.status = Status::Connecting(Phase::Authenticating);
        }
        self.changed.notify_all();
        drop(state);
        if show {
            emit(RemoteEvent::Status(self.host_id()));
        }
    }

    /// 断开，不再自己重连，直到 `retry_now`。那台电脑上的会话照常跑着。
    pub fn disconnect(&self) {
        let mut state = self.state();
        state.wanted = false;
        state.hurry = false;
        state.nudged = false;
        if let Some(stream) = state.stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
        self.changed.notify_all();
        drop(state);
        self.close_channels();
        forward::forget(&self.id);
    }

    /// 管这台电脑的连接：要用时连，断了按 `RETRY_DELAYS` 退避重连，走完了或要用户来办的停下等 `retry_now`。
    fn supervise(self: Arc<Self>) {
        let mut attempt = 0u32;
        loop {
            if !self.state().wanted {
                self.set_status(Status::Idle);
            }
            {
                let mut state = self.state();
                while !state.wanted {
                    if state.forgotten {
                        return;
                    }
                    state = self.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
                }
                state.hurry = false;
                if std::mem::take(&mut state.nudged) {
                    attempt = 0;
                }
            }
            let started = Instant::now();
            let failure = match self.connect_once() {
                Ok(()) => {
                    self.watch();
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
                Some(Failure::Stop(error)) => {
                    tracing::warn!("connecting to the paired computer {} needs the user: {error}", self.id);
                    self.set_status(Status::Stopped { error, auth: false });
                    self.wait_for_retry();
                    attempt = 0;
                    continue;
                }
                Some(Failure::Retry(error)) => error,
                None => rust_i18n::t!("paired.lost").into_owned(),
            };
            let Some(&delay) = RETRY_DELAYS.get(attempt as usize) else {
                tracing::warn!("gave up reconnecting to the paired computer {} after {attempt} attempts", self.id);
                self.state().gave_up = true;
                self.set_status(Status::Stopped { error, auth: false });
                self.wait_for_retry();
                self.state().gave_up = false;
                attempt = 0;
                continue;
            };
            attempt += 1;
            tracing::info!("reconnecting to the paired computer {} in {delay:?} (attempt {attempt}): {error}", self.id);
            self.set_status(Status::Waiting { attempt, retry_at: Instant::now() + delay, error });
            self.sleep(delay);
        }
    }

    /// 等用户要再试（`retry_now`）、睡醒或网络变了（`nudge`），或断开（`disconnect`）。
    fn wait_for_retry(&self) {
        let mut state = self.state();
        while state.wanted && !state.hurry && !state.nudged {
            state = self.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// 等 `delay`，被 `retry_now`、`nudge`、`disconnect` 叫醒时提前回来。
    fn sleep(&self, delay: Duration) {
        let deadline = Instant::now() + delay;
        let mut state = self.state();
        while state.wanted && !state.hurry && !state.nudged {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            state = self.changed.wait_timeout(state, deadline - now).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// 连一次：拨号过门禁，把 `Link` 接上去，问清那边的家目录和系统。
    fn connect_once(&self) -> Result<(), Failure> {
        self.set_status(Status::Connecting(Phase::Authenticating));
        let handed = self.handed.lock().unwrap_or_else(PoisonError::into_inner).take();
        let dialed = match handed {
            Some(dialed) => dialed,
            None => client::dial(&dirs(), &self.id).map_err(|err| classify(&err))?,
        };
        remember(&dialed.machine);
        tracing::info!("dialed the paired computer {} at {:?}", self.id, dialed.addr);
        self.set_status(Status::Connecting(Phase::Starting));
        let stream = dialed.stream;
        let theirs = stream.try_clone().map_err(|err| Failure::Retry(err.to_string()))?;
        {
            let mut state = self.state();
            // 拨号的时候用户断开或忘掉了这台：不接上，不然它连上后照样报 `RemoteEvent::Connected`。
            if !state.wanted {
                let _ = theirs.shutdown(Shutdown::Both);
                return Err(Failure::Retry("disconnected while connecting".into()));
            }
            state.stream = Some(theirs);
            self.link.set_windows_paths(state.windows);
        }
        if let Err(err) = self.link.connect(stream) {
            self.drop_stream();
            // 宿主的协议版本对不上时再连也一样，要两边升级到同一版本。
            return Err(match err {
                ConnectError::Incompatible(reason) => {
                    Failure::Stop(rust_i18n::t!("paired.protocol", reason = reason).into_owned())
                }
                err => Failure::Retry(err.to_string()),
            });
        }
        let build = self.link.host_build().map(|build| build.0).unwrap_or_default();
        let (home, os) = self.probe_system();
        let windows = os == "windows";
        self.state().windows = windows;
        self.link.set_windows_paths(windows);
        let mismatch = build != super::build().0;
        self.connected(Connected { build, mismatch, session_bound: false, home, os });
        Ok(())
    }

    /// 那台电脑的家目录和系统（`Connected::os` 的写法）：家目录按 `ClientMsg::ListDirs` 不给目录时列的那个认，带
    /// 盘符的是 Windows；别的看有没有只有 macOS 上才有的目录。问不到时家目录为空、系统不明。
    fn probe_system(&self) -> (Option<PathBuf>, String) {
        let list =
            |path: Option<PathBuf>| match self.link.request(|req| ClientMsg::ListDirs { req, path }, LIST_TIMEOUT) {
                Ok(HostMsg::Dirs { path, .. }) => Some(path),
                _ => None,
            };
        let Some(home) = list(None) else {
            tracing::warn!("cannot find the home directory on the paired computer {}", self.id);
            return (None, String::new());
        };
        let os = if has_drive(&home) {
            "windows"
        } else if list(Some(PathBuf::from(MACOS_ONLY_DIR))).is_some() {
            "macos"
        } else {
            "linux"
        };
        (Some(home), os.to_owned())
    }

    /// 连上了：记下状态，告诉界面那边还活着的会话，断开着的终端接回来。
    fn connected(&self, connected: Connected) {
        self.set_status(Status::Connected(connected));
        let alive: HashSet<_> = match self.link.list_sessions(LIST_TIMEOUT) {
            Ok(sessions) => sessions.into_iter().filter(|session| !session.exited).map(|session| session.id).collect(),
            Err(err) => {
                tracing::warn!("cannot list the sessions on the paired computer {}: {err}", self.id);
                HashSet::new()
            }
        };
        emit(RemoteEvent::Connected { host: self.host_id(), alive });
    }

    /// 连着时一直等到断开：那边关了连接，或者心跳等不到回话。
    fn watch(&self) {
        let mut since_beat = Instant::now();
        loop {
            if !self.state().wanted || !self.link.connected() {
                break;
            }
            // 连上后马上测一次延迟，之后每隔 `HEARTBEAT` 一次。
            if since_beat.elapsed() >= HEARTBEAT || self.latency().is_none() {
                since_beat = Instant::now();
                if !self.link.flush(HEARTBEAT_TIMEOUT) {
                    tracing::warn!("the paired computer {} did not answer the heartbeat", self.id);
                    break;
                }
                *self.latency.lock().unwrap_or_else(PoisonError::into_inner) = Some(since_beat.elapsed());
            }
            thread::sleep(Duration::from_millis(250));
        }
        *self.latency.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.drop_stream();
        self.close_channels();
    }

    /// 关掉 `Link` 用的 socket：`Link` 读到结尾，各会话收到 `Lost`。
    fn drop_stream(&self) {
        if let Some(stream) = self.state().stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}

/// 路径以盘符开头（`C:/Users/me`），是 Windows 上的。
fn has_drive(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// 按拨号的失败定下要不要自己再试，原因写给人看。
fn classify(err: &DialError) -> Failure {
    let text = |key: &str| rust_i18n::t!(key).into_owned();
    match err {
        DialError::Rejected(RejectReason::UnknownDevice) => Failure::Stop(text("paired.revoked")),
        DialError::Rejected(RejectReason::BadSignature) => Failure::Stop(text("paired.bad_signature")),
        DialError::Rejected(RejectReason::RateLimited) => Failure::Retry(text("paired.rate_limited")),
        DialError::Rejected(RejectReason::Disabled) => Failure::Retry(text("paired.disabled")),
        DialError::Rejected(RejectReason::PairingInvalid | RejectReason::Unknown) => {
            Failure::Retry(text("paired.rejected"))
        }
        DialError::FingerprintMismatch => Failure::Stop(text("paired.fingerprint")),
        DialError::Expired => Failure::Stop(text("paired.expired")),
        DialError::Protocol(reason) => Failure::Stop(rust_i18n::t!("paired.protocol", reason = reason).into_owned()),
        DialError::Unreachable { last } => {
            Failure::Retry(rust_i18n::t!("paired.unreachable", reason = last.as_str()).into_owned())
        }
        DialError::Io(err) => Failure::Retry(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use runode_protocol::remote::RejectReason;
    use runode_remote::client::DialError;

    use super::{Failure, classify, has_drive};

    /// 撤销了配对、证书变了、协议不认识的要用户来办，停下；连不上、限速、一时办不了的自己再试。
    #[test]
    fn dial_failures_stop_only_when_the_user_has_to_act() {
        let stops = |err: DialError| matches!(classify(&err), Failure::Stop(_));
        assert!(stops(DialError::Rejected(RejectReason::UnknownDevice)));
        assert!(stops(DialError::Rejected(RejectReason::BadSignature)));
        assert!(stops(DialError::FingerprintMismatch));
        assert!(stops(DialError::Protocol("gate version 2".into())));
        assert!(!stops(DialError::Unreachable { last: "timed out".into() }));
        assert!(!stops(DialError::Rejected(RejectReason::RateLimited)));
        assert!(!stops(DialError::Rejected(RejectReason::Disabled)));
        assert!(!stops(DialError::Rejected(RejectReason::Unknown)));
        assert!(!stops(DialError::Io(std::io::Error::other("reset"))));
    }

    #[test]
    fn windows_homes_start_with_a_drive() {
        assert!(has_drive(Path::new("C:/Users/me")));
        assert!(has_drive(Path::new("d:\\work")));
        assert!(!has_drive(Path::new("/Users/me")));
        assert!(!has_drive(Path::new("/home/me")));
        assert!(!has_drive(Path::new("")));
    }
}
