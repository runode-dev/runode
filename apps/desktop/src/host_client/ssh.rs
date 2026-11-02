//! 经 SSH 连的远端主机：每台一条 `Link`，接到那台机器上的宿主（`runode --ssh-bridge`）。
//! 本机那条（`host_client::link`）照旧，这里另管远端的。
//!
//! 每台主机一个 `remote-host` 线程（`supervise`）管连接：要用时连上；主连接（ControlMaster）没开着就先开，
//! 要认证时经 askpass 问桌面（`askpass`）；远端没装这个构建就装上（`deploy`）；连上后 ssh 子进程的 stdin、
//! stdout 和一对本地 socket 互搬，`Link` 接在这对 socket 的一头上。断了按退避阶梯（`RETRY_DELAYS`）重连，
//! 阶梯走完还连不上（比如那台机器关了）就停下，这台电脑睡醒或网络变了时再试（`watch_network`）；认证没过、
//! 主机密钥对不上这类要用户来办的不自己重试。停下的都等 `retry_now`。
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
    io::{self, BufRead as _, BufReader, PipeWriter, Read, Write},
    net::Shutdown,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, LazyLock, Mutex, PoisonError, atomic::Ordering},
    thread,
    time::{Duration, Instant},
};

use runode_ssh::{
    Destination, Ssh,
    bootstrap::{self, Family},
    bridge::{self, Start, read_service, read_start},
    process::{Piped, Pipes},
};

use super::{
    Link, Timeouts, UnixStream, build,
    hosts::{
        self, Connected, HEARTBEAT, HEARTBEAT_TIMEOUT, HostId, LIST_TIMEOUT, Phase, RETRY_DELAYS, RemoteEvent,
        STABLE_AFTER, Status, emit,
    },
};

pub use askpass::Ask;

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
    hosts::watch_network_once();
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
    emit(RemoteEvent::Forgotten(HostId::Ssh(name.to_owned())));
}

/// 睡醒或网络变了：叫正在重连和重连放弃了的主机马上再试（`RemoteHost::nudge`）。
pub(super) fn nudge_all() {
    let hosts: Vec<_> = HOSTS.lock().unwrap_or_else(PoisonError::into_inner).values().cloned().collect();
    for host in hosts {
        host.nudge();
    }
}

/// 让各台主机的主连接退出，不等它们办完：app 退出时调，退出的收尾等不了多久。
pub fn close_masters() {
    let ssh = ssh();
    if ssh.control.is_none() {
        return;
    }
    for host in remotes() {
        let mut command = exit_command(&ssh, &host.dest);
        // 自成一个进程组，app 退出时不连带结束它。
        crate::subprocess::isolate(&mut command);
        if let Err(err) = command.spawn() {
            tracing::warn!("cannot close the connection to {}: {err}", host.name);
        }
    }
}

fn exit_command(ssh: &Ssh, dest: &Destination) -> Command {
    let mut command = ssh.exit(dest);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    command
}

/// 已经连过（或正连着）的远端主机。
pub fn remotes() -> Vec<Arc<RemoteHost>> {
    let hosts = HOSTS.lock().unwrap_or_else(PoisonError::into_inner);
    let mut hosts: Vec<_> = hosts.values().cloned().collect();
    hosts.sort_by(|a, b| a.name.cmp(&b.name));
    hosts
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

/// 这个 ssh 能不能经 askpass 问用户（`runode_ssh::supports_askpass`），每个程序只看一次版本；认不出版本（不是
/// OpenSSH）时当作能。
pub(crate) fn can_ask(ssh: &Ssh) -> bool {
    static KNOWN: LazyLock<Mutex<HashMap<PathBuf, bool>>> = LazyLock::new(Mutex::default);
    if let Some(&known) = KNOWN.lock().unwrap_or_else(PoisonError::into_inner).get(&ssh.program) {
        return known;
    }
    let mut command = ssh.version();
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let version =
        command.output().ok().and_then(|out| runode_ssh::parse_version(&String::from_utf8_lossy(&out.stderr)));
    let can = version.is_none_or(runode_ssh::supports_askpass);
    if !can {
        tracing::warn!("{} is OpenSSH {version:?}, which cannot ask for passwords", ssh.program.display());
    }
    KNOWN.lock().unwrap_or_else(PoisonError::into_inner).insert(ssh.program.clone(), can);
    can
}

pub(crate) fn ssh() -> Ssh {
    let settings = SETTINGS.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let dirs = runode_paths::Dirs::from_env();
    // Windows 自带的 OpenSSH 不支持 ControlMaster。
    let control = if cfg!(unix) && dirs.create_ssh_control_dir().is_ok() { dirs.ssh_control_path() } else { None };
    Ssh { program: settings.program, control, persist_secs: settings.persist_secs }
}

static HOSTS: LazyLock<Mutex<HashMap<String, Arc<RemoteHost>>>> = LazyLock::new(Mutex::default);
static SETTINGS: LazyLock<Mutex<SshSettings>> = LazyLock::new(Mutex::default);

/// 一台远端主机。
pub struct RemoteHost {
    /// 用户填的连接目标，原样。
    pub name: String,
    pub dest: Destination,
    pub link: Link,
    state: Mutex<State>,
    changed: Condvar,
    /// 连着时最近一次心跳来回花的时间。
    latency: Mutex<Option<Duration>>,
}

struct State {
    status: Status,
    /// 要连着；`disconnect` 后为假，supervisor 等到再要连。
    wanted: bool,
    /// 不等退避，马上再试，可以问用户。
    hurry: bool,
    /// 睡醒或网络变了，不等退避马上再试，不问用户（`nudge`）。
    nudged: bool,
    /// 重连的退避走完了停在 `Status::Stopped`，网络变了可以再试。
    gave_up: bool,
    /// 连着时的 ssh 子进程，断开（`disconnect`）时结束它。
    child: Option<u32>,
    /// 被 `forget` 了，supervisor 不再等着要连，退出。
    forgotten: bool,
    /// 远端命令的写法，认出是 Windows 以后记着，再连时不用先按 Unix 试。
    family: Family,
}

impl fmt::Debug for RemoteHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteHost").field("name", &self.name).finish_non_exhaustive()
    }
}

impl RemoteHost {
    fn new(name: String, dest: Destination) -> Self {
        let link = Link::with_timeouts(build(), Timeouts::REMOTE);
        hosts::send_host_options(&link);
        Self {
            name,
            dest,
            link,
            state: Mutex::new(State {
                status: Status::Idle,
                wanted: !hosts::holding_new_hosts(),
                hurry: false,
                nudged: false,
                gave_up: false,
                child: None,
                forgotten: false,
                family: Family::Unix,
            }),
            changed: Condvar::new(),
            latency: Mutex::new(None),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn id(&self) -> HostId {
        HostId::Ssh(self.name.clone())
    }

    pub fn status(&self) -> Status {
        self.state().status.clone()
    }

    /// 连着时最近一次心跳经 ssh 到远端宿主来回花的时间；没连着、还没测过时为空。
    pub fn latency(&self) -> Option<Duration> {
        *self.latency.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_status(&self, status: Status) {
        let mut state = self.state();
        if state.status == status {
            return;
        }
        state.status = status;
        drop(state);
        emit(RemoteEvent::Status(self.id()));
    }

    fn start(self: &Arc<Self>) {
        let host = self.clone();
        let spawned = thread::Builder::new().name("remote-host".into()).spawn(move || host.supervise());
        if let Err(err) = spawned {
            tracing::error!("cannot start watching {}: {err}", self.name);
        }
    }

    /// 不等退避马上再连；停着的（要用户来办的）也再试一次，这次可以问用户。停着、没在连的当场改成在连，
    /// 调用方紧接着读到的不是上回停下的状态。
    pub fn retry_now(&self) {
        let mut state = self.state();
        state.wanted = true;
        state.hurry = true;
        let woken = !state.forgotten && matches!(state.status, Status::Stopped { .. } | Status::Idle);
        self.wake(state, woken);
    }

    /// 睡醒或网络变了：正在等着重连的、重连放弃了的不等退避马上再试，退避从头算，这次不问用户。要用户来办的、
    /// 断开了的不动。
    fn nudge(&self) {
        let mut state = self.state();
        let stopped = matches!(state.status, Status::Stopped { .. });
        if !state.wanted || state.forgotten || !matches!(state.status, Status::Waiting { .. }) && !state.gave_up {
            return;
        }
        state.nudged = true;
        self.wake(state, stopped);
    }

    /// 叫醒 supervisor；`show` 时当场把状态改成在连，调用方紧接着读到的不是上回停下的状态。
    fn wake(&self, mut state: std::sync::MutexGuard<'_, State>, show: bool) {
        if show {
            state.status = Status::Connecting(Phase::Authenticating);
        }
        self.changed.notify_all();
        drop(state);
        if show {
            emit(RemoteEvent::Status(self.id()));
        }
    }

    /// 断开，让主连接退出，不再自己重连，直到 `retry_now`。远端的会话照常跑着。会阻塞。
    pub fn disconnect(&self) {
        let mut state = self.state();
        state.wanted = false;
        state.hurry = false;
        state.nudged = false;
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
        let ssh = ssh();
        if ssh.control.is_none() {
            return;
        }
        let mut command = exit_command(&ssh, &self.dest);
        let _ = command.status();
        forward::forget(&self.name);
    }

    /// 远端命令按哪种系统写，没认出是 Windows 时按 Unix。
    pub fn family(&self) -> Family {
        self.state().family
    }

    /// 在远端把宿主装成登录服务（`runode --ssh-service`）。连着时才办，会阻塞。
    pub fn install_service(&self) -> Result<bridge::Service, bridge::Failure> {
        let fail = |code: &str, message: String| bridge::Failure { code: code.into(), message };
        if !matches!(self.status(), Status::Connected(_)) {
            return Err(fail("disconnected", format!("not connected to {}", self.name)));
        }
        let ssh = ssh();
        let script = bootstrap::run_command(&deploy::install_dir(), bootstrap::Entry::Service);
        let mut command = self.exec(&ssh, &script, true, false);
        command.stdin(Stdio::null());
        let output =
            runode_ssh::process::output(&mut command).map_err(|err| fail("ssh", format!("cannot run ssh: {err}")))?;
        let read = read_service(&mut output.stdout.as_slice());
        read.map_err(|_| fail("ssh", format!("ssh exited with {:?}", output.status.code())))?
    }

    /// 删掉远端除了这个构建、`runode` 链接指着的和还在跑的以外的构建，交回删掉的个数和腾出的 KiB。连着时才办，
    /// 会阻塞。
    pub fn prune_builds(&self) -> Result<(usize, u64), String> {
        if !matches!(self.status(), Status::Connected(_)) {
            return Err(format!("not connected to {}", self.name));
        }
        let ssh = ssh();
        let script = self.family().prune_command(&deploy::install_dir());
        let mut command = self.exec(&ssh, &script, true, false);
        command.stdin(Stdio::null());
        let output = runode_ssh::process::output(&mut command).map_err(|err| format!("cannot run ssh: {err}"))?;
        if !output.status.success() {
            return Err(format!("ssh exited with {:?}", output.status.code()));
        }
        let pruned = bootstrap::parse_pruned(&String::from_utf8_lossy(&output.stdout));
        Ok((pruned.len(), pruned.iter().map(|(_, kib)| kib).sum()))
    }

    /// 管这台主机的连接：要用时连，断了按 `RETRY_DELAYS` 退避重连，走完了或要用户来办的停下等 `retry_now`。
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
                let hurry = std::mem::take(&mut state.hurry);
                let nudged = std::mem::take(&mut state.nudged);
                let interactive = hurry || attempt == 0 && !nudged;
                if nudged {
                    attempt = 0;
                }
                interactive
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
                Some(Failure::Stop { mut error, auth }) => {
                    if auth && !can_ask(&ssh()) {
                        error.push_str(
                            " (this ssh cannot ask for passwords, which needs OpenSSH 8.4 or later: use a key or ssh-agent)",
                        );
                    }
                    tracing::warn!("connecting to {} needs the user: {error}", self.name);
                    self.set_status(Status::Stopped { error, auth });
                    self.wait_for_retry();
                    attempt = 0;
                    continue;
                }
                Some(Failure::Retry(error)) => error,
                None => "lost the connection".to_owned(),
            };
            let Some(&delay) = RETRY_DELAYS.get(attempt as usize) else {
                tracing::warn!("gave up reconnecting to {} after {attempt} attempts: {error}", self.name);
                self.state().gave_up = true;
                self.set_status(Status::Stopped { error, auth: false });
                self.wait_for_retry();
                self.state().gave_up = false;
                attempt = 0;
                continue;
            };
            attempt += 1;
            tracing::info!("reconnecting to {} in {delay:?} (attempt {attempt}): {error}", self.name);
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

    /// 连一次：开主连接、看远端装没装、需要时装上、起桥接，把 `Link` 接上去。
    fn connect_once(self: &Arc<Self>, interactive: bool) -> Result<Running, Failure> {
        self.set_status(Status::Connecting(Phase::Authenticating));
        let ssh = ssh();
        let interactive = interactive && can_ask(&ssh);
        let log = self.log_file();
        if ssh.control.is_some() {
            self.open_master(&ssh, interactive, &log)?;
        }
        let dir = deploy::install_dir();
        let mut deployed = false;
        let mut family = self.family();
        // 按 Unix 没连上、改按 Windows 试时，Unix 那次的 ssh 日志和退出码：Windows 也不是时按它报错。
        let mut unix_failure: Option<(Vec<u8>, Option<i32>)> = None;
        loop {
            self.set_status(Status::Connecting(if deployed { Phase::Starting } else { Phase::Probing }));
            let script = family.connect_command(&dir);
            let mut command = self.exec(&ssh, &script, interactive, false);
            let piped = spawn_piped(&mut command, &log)?;
            let (mut child, stdin) = (piped.child, piped.stdin);
            let stdout = piped.stdout.ok_or_else(|| Failure::Retry("ssh gave no output".into()))?;
            let mut stdout = BufReader::with_capacity(256 << 10, stdout);
            let start = read_start(&mut stdout).map_err(|err| Failure::Retry(format!("cannot read from ssh: {err}")));
            match start {
                Ok(Start::Bridge(opening)) => {
                    tracing::info!("bridged to {}: {opening:?}", self.name);
                    let pid = child.id();
                    let mut state = self.state();
                    // 连的时候用户断开或忘掉了这台主机：不接上，不然它连上后照样报 `RemoteEvent::Connected`。
                    if !state.wanted {
                        drop(state);
                        crate::subprocess::kill(pid);
                        let _ = child.wait();
                        return Err(Failure::Retry(format!("disconnected from {} while connecting", self.name)));
                    }
                    state.child = Some(pid);
                    state.family = family;
                    drop(state);
                    self.link.set_windows_paths(family == Family::Windows);
                    let running = Running::start(child, stdin, stdout, &self.link).map_err(|err| {
                        self.state().child = None;
                        Failure::Retry(format!("cannot talk to the host on {}: {err}", self.name))
                    })?;
                    let mismatch = opening.build != build().0;
                    let home = (!opening.home.is_empty()).then(|| PathBuf::from(&opening.home));
                    let connected = Connected {
                        build: opening.build,
                        mismatch,
                        session_bound: opening.session_bound,
                        home,
                        os: opening.os,
                    };
                    self.connected(connected);
                    return Ok(running);
                }
                Ok(Start::Failed(failure)) => {
                    drop(stdin);
                    let _ = child.wait();
                    return Err(Failure::Retry(format!("{}: {}", failure.code, failure.message)));
                }
                Ok(Start::Probe(platform)) => {
                    // 远端脚本已经退出；Windows 的 ssh 还要等到 stdin 读完才退，不关就一直等下去。
                    drop(stdin);
                    let status = child.wait().map_err(|err| Failure::Retry(err.to_string()))?;
                    // 默认 shell 是 Windows PowerShell 时，它只按成败交回 0 或 1，Windows 上认不出 `PROBE_EXIT`；
                    // Windows 的脚本只在没装这个构建时打出平台信息，打出了就算。
                    let probed = family == Family::Windows || status.code() == Some(bootstrap::PROBE_EXIT);
                    let Some(platform) = platform.filter(|_| probed) else {
                        // 没打出平台信息、ssh 也没报连接或认证的错，多半是 Windows：cmd 把 Unix 脚本里的 `|`、`>`
                        // 当成管道和重定向，退出码也可能是 255，不能按退出码认。
                        let unix_log = std::fs::read(&log).unwrap_or_default();
                        let ssh_failed = !matches!(
                            runode_ssh::Failure::of(&String::from_utf8_lossy(&unix_log)),
                            runode_ssh::Failure::Other(_)
                        );
                        if family == Family::Unix && !deployed && !ssh_failed {
                            unix_failure = Some((unix_log, status.code()));
                            family = Family::Windows;
                            continue;
                        }
                        if let Some((unix_log, code)) = unix_failure {
                            let _ = std::fs::write(&log, unix_log);
                            return Err(classify(&log, code));
                        }
                        return Err(classify(&log, status.code()));
                    };
                    if deployed {
                        return Err(Failure::Stop {
                            error: "runode did not start after installing it".into(),
                            auth: false,
                        });
                    }
                    deploy::install(self, &ssh, &platform, &dir, &log, interactive)?;
                    deployed = true;
                }
                Err(failure) => {
                    // 读不到开场白时远端可能还在跑、等着 stdin，不结束它就一直等不到退出。
                    crate::subprocess::kill(child.id());
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
        emit(RemoteEvent::Connected { host: self.id(), alive });
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
            // 连上后马上测一次延迟，之后每隔 `HEARTBEAT` 一次。
            if running.since_beat.elapsed() >= HEARTBEAT || self.latency().is_none() {
                running.since_beat = Instant::now();
                if !self.link.flush(HEARTBEAT_TIMEOUT) {
                    tracing::warn!("{} did not answer the heartbeat", self.name);
                    break;
                }
                *self.latency.lock().unwrap_or_else(PoisonError::into_inner) = Some(running.since_beat.elapsed());
            }
            thread::sleep(Duration::from_millis(250));
        }
        *self.latency.lock().unwrap_or_else(PoisonError::into_inner) = None;
        running.stop();
        self.state().child = None;
    }

    /// 开主连接（ControlMaster）：已经开着就复用。`interactive` 时要认证可以问用户，否则不问，要认证就停下等用户。
    fn open_master(&self, ssh: &Ssh, interactive: bool, log: &Path) -> Result<(), Failure> {
        let mut check = ssh.check(&self.dest);
        check.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        crate::subprocess::isolate(&mut check);
        if check.status().is_ok_and(|status| status.success()) {
            return Ok(());
        }
        // 挂在上一条主连接上的转发跟着它没了。
        forward::forget(&self.name);
        let mut master = ssh.master(&self.dest, !interactive);
        if interactive {
            self.ask_through(&mut master);
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

    /// 在这台主机上跑 `script` 的 ssh，`compressed` 时传的数据压缩。经主连接时不用认证；不经主连接时自己
    /// 认证，`interactive` 而且这个 ssh 能问用户（`can_ask`）时经 askpass 问，否则不问，要认证就失败。
    pub(crate) fn exec(&self, ssh: &Ssh, script: &str, interactive: bool, compressed: bool) -> Command {
        let batch = ssh.control.is_some() || !interactive || !can_ask(ssh);
        let mut command = if compressed {
            ssh.exec_compressed(&self.dest, script, batch)
        } else {
            ssh.exec(&self.dest, script, batch)
        };
        if !batch {
            self.ask_through(&mut command);
        }
        command
    }

    /// 让 `command` 起的 ssh 要问用户时经 askpass 问桌面。
    fn ask_through(&self, command: &mut Command) {
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

/// ssh 自己出错时的退出码；别的退出码是远端命令的。
const SSH_ERROR_EXIT: i32 = 255;

/// 按 ssh 的 stderr（写在 `log` 里）和退出码说清为什么没连上。不是 ssh 自己出错时 stderr 里是远端脚本的话，
/// 不按 ssh 的话认：远端 `mkdir` 报的 `Permission denied` 不是认证没过。
fn classify(log: &std::path::Path, code: Option<i32>) -> Failure {
    let stderr = std::fs::read_to_string(log).unwrap_or_default();
    if code.is_some_and(|code| code != SSH_ERROR_EXIT) {
        return Failure::Retry(match runode_ssh::last_line(&stderr) {
            "" => format!("ssh exited with {code:?}"),
            line => line.to_owned(),
        });
    }
    let failure = runode_ssh::Failure::of(&stderr);
    let error = match &failure {
        runode_ssh::Failure::Other(last) if last.is_empty() => format!("ssh exited with {code:?}"),
        runode_ssh::Failure::Other(last) => last.clone(),
        other => format!("{other:?}: {}", runode_ssh::last_line(&stderr)),
    };
    if failure.retryable() { Failure::Retry(error) } else { Failure::Stop { error, auth: true } }
}

/// 起 `command`：stdin、stdout 接管道，stderr 写进 `log`，自成一个进程组。
fn spawn_piped(command: &mut Command, log: &std::path::Path) -> Result<Piped, Failure> {
    let stderr = File::create(log).map_err(|err| Failure::Retry(format!("cannot write the ssh log: {err}")))?;
    command.stderr(stderr);
    crate::subprocess::isolate(command);
    let pipes = Pipes { stdin: true, stdout: true, stderr: false };
    runode_ssh::process::spawn(command, pipes)
        .map_err(|err| Failure::Stop { error: format!("cannot run ssh: {err}"), auth: false })
}

/// 连着的一次：ssh 子进程，和接在它与 `Link` 之间的一对 socket。
struct Running {
    child: Child,
    /// 本地这对 socket 里搬字节的那一头。
    theirs: UnixStream,
    since_beat: Instant,
}

impl Running {
    /// 起两个搬字节的线程，把 `Link` 接在另一头上；出错时结束并收掉 ssh 子进程。
    fn start(
        mut child: Child,
        stdin: Option<PipeWriter>,
        stdout: BufReader<impl Read + Send + 'static>,
        link: &Link,
    ) -> io::Result<Self> {
        let started = Self::pump(stdin, stdout, link);
        match started {
            Ok(theirs) => Ok(Self { child, theirs, since_beat: Instant::now() }),
            Err(err) => {
                crate::subprocess::kill(child.id());
                let _ = child.wait();
                Err(err)
            }
        }
    }

    fn pump(
        stdin: Option<PipeWriter>,
        stdout: BufReader<impl Read + Send + 'static>,
        link: &Link,
    ) -> io::Result<UnixStream> {
        let stdin = stdin.ok_or_else(|| io::Error::other("ssh has no stdin"))?;
        let (ours, theirs) = runode_host::socket_pair()?;
        let to_link = theirs.try_clone()?;
        let from_link = theirs.try_clone()?;
        thread::Builder::new().name("ssh-pump-out".into()).spawn(move || {
            pump_out(stdout, &to_link);
            let _ = to_link.shutdown(Shutdown::Both);
        })?;
        thread::Builder::new().name("ssh-pump-in".into()).spawn(move || pump_in(&from_link, stdin))?;
        link.connect(ours).map_err(|err| io::Error::other(err.to_string()))?;
        Ok(theirs)
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

/// 还没写进 ssh 的 stdin 的输入最多攒这么多字节，再多就当作断线。
const MAX_UNSENT: usize = 64 << 20;

/// socket 搬到 ssh 的 stdin，每块都 flush 出去；读到结尾就关掉 stdin，远端的桥接随之退出。socket 一直读空，
/// 读到的先攒在内存里，另一个线程往 ssh 写：链路慢时 `Link` 写帧不会卡住调它的界面线程。攒的超过
/// `MAX_UNSENT`，或者写不进 ssh 时关掉 socket，`Link` 按断开处理。
fn pump_in(from: &UnixStream, to: PipeWriter) {
    let unsent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (chunks, pending) = std::sync::mpsc::channel::<Vec<u8>>();
    let writer = {
        let (unsent, socket) = (unsent.clone(), from.try_clone());
        thread::Builder::new().name("ssh-pump-write".into()).spawn(move || {
            if !write_chunks(pending, to, &unsent)
                && let Ok(socket) = socket
            {
                let _ = socket.shutdown(Shutdown::Both);
            }
        })
    };
    if writer.is_err() {
        return;
    }
    let mut from = from;
    let mut buffer = vec![0; 256 << 10];
    loop {
        match from.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if unsent.fetch_add(n, Ordering::Relaxed) + n > MAX_UNSENT {
                    tracing::warn!("ssh is not taking input; dropping the connection");
                    let _ = from.shutdown(Shutdown::Both);
                    return;
                }
                if chunks.send(buffer[..n].to_vec()).is_err() {
                    return;
                }
            }
        }
    }
}

/// 把 `pending` 里的一块块写进 ssh 的 stdin，写完一块从 `unsent` 里减掉；`pending` 关了（socket 读到结尾）
/// 时写完剩下的、关掉 stdin，返回 `true`，写不进去时返回 `false`。
fn write_chunks(
    pending: std::sync::mpsc::Receiver<Vec<u8>>,
    mut to: PipeWriter,
    unsent: &std::sync::atomic::AtomicUsize,
) -> bool {
    for chunk in pending {
        if to.write_all(&chunk).and_then(|()| to.flush()).is_err() {
            return false;
        }
        unsent.fetch_sub(chunk.len(), Ordering::Relaxed);
    }
    true
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use futures::StreamExt as _;

    use super::{HostId, RemoteEvent, Status, hosts, hosts::events, remote, ssh, upload};
    #[cfg(unix)]
    use super::{SshSettings, deploy, set_ssh_settings};

    type Alive = std::collections::HashSet<runode_protocol::SessionId>;

    /// 连上的主机和那时还活着的会话。`RemoteEvent` 一个进程只能取一次，由一个线程收着，几个测试共用；ssh 经
    /// askpass 问的当场代答（`answer`）。
    static CONNECTED: std::sync::LazyLock<std::sync::Mutex<std::sync::mpsc::Receiver<(HostId, Alive)>>> =
        std::sync::LazyLock::new(|| {
            let mut events = events().expect("events");
            let (connected, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                futures::executor::block_on(async {
                    while let Some(event) = events.next().await {
                        match event {
                            RemoteEvent::Connected { host, alive } => {
                                let _ = connected.send((host, alive));
                            }
                            RemoteEvent::Ask(ask) => answer(ask),
                            _ => {}
                        }
                    }
                })
            });
            std::sync::Mutex::new(receiver)
        });
    /// ssh 经 askpass 问过的。
    static ASKED: std::sync::Mutex<Vec<runode_ssh::askpass::Kind>> = std::sync::Mutex::new(Vec::new());

    /// 认主机指纹答是，要口令时答 `RUNODE_TEST_SSH_SECRET`（没设时取消）。
    fn answer(ask: super::askpass::Ask) {
        use runode_ssh::askpass::Kind;

        ASKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(ask.kind);
        let reply = match ask.kind {
            Kind::Confirm => Some("yes".to_owned()),
            Kind::Secret => std::env::var("RUNODE_TEST_SSH_SECRET").ok(),
            Kind::Notice => return,
        };
        ask.answer(reply);
    }

    /// 等到连上主机 `name` 的下一次 `RemoteEvent::Connected`，交回那时还活着的会话。
    fn next_connected(name: &str) -> Option<Alive> {
        let connected = CONNECTED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            let (host, alive) = connected.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok()?;
            if host == HostId::Ssh(name.to_owned()) {
                return Some(alive);
            }
        }
    }

    /// 经假的 ssh 连的「远端」：假的 ssh 把要在远端跑的命令换成在本机、以一个临时目录为家目录跑，主连接相关的
    /// 直接成功。丢掉时结束「远端」的宿主、删掉临时目录，测试中途失败也一样。
    #[cfg(unix)]
    struct FakeRemote {
        host: std::sync::Arc<super::RemoteHost>,
        home: std::path::PathBuf,
        root: std::path::PathBuf,
        /// 测试里见过的「远端」宿主，丢掉时一起结束。
        pids: std::cell::RefCell<Vec<u32>>,
    }

    #[cfg(unix)]
    impl FakeRemote {
        /// 主机名 `name`，连接要等 `RemoteHost::retry_now` 或第一次自己连。
        fn new(name: &str) -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            // 放在 /tmp 下：远端宿主的 socket 在这个家目录里，路径要放得进 `sun_path`。
            let root = std::path::PathBuf::from(format!("/tmp/rn-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let home = root.join("home");
            std::fs::create_dir_all(&home).unwrap();
            let fake = root.join("ssh");
            let script = format!(
                concat!(
                    "#!/bin/sh\n",
                    "for arg in \"$@\"; do case \"$arg\" in\n",
                    "  -V) echo OpenSSH_9.6p1 >&2; exit 0;;\n",
                    "  -M|-N|-O) exit 0;;\n",
                    "esac; done\n",
                    "eval \"remote=\\${{$#}}\"\n",
                    "HOME='{home}' exec /bin/sh -c \"$remote\"\n",
                ),
                home = home.display()
            );
            std::fs::write(&fake, script).unwrap();
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
            set_ssh_settings(SshSettings { program: fake, persist_secs: 60 });
            let host = remote(&format!("me@{name}")).expect("a destination");
            Self { host, home, root, pids: Default::default() }
        }

        /// 等到连上或停下，交回那时的状态，记下「远端」宿主。
        fn connected(&self) -> Status {
            let status = wait_for(&self.host, Duration::from_secs(120), |status| {
                matches!(status, Status::Connected(_) | Status::Stopped { .. })
            });
            self.pids.borrow_mut().extend(self.host.link.host_pid());
            status
        }

        fn spawn(&self) -> runode_protocol::SessionId {
            let options = crate::host_client::SpawnOptions {
                size: runode_types::grid::GridSize { cols: 80, rows: 24, cell_width_px: 8, cell_height_px: 16 },
                cwd: None,
                fallback_cwd: None,
                integration: runode_types::shell::IntegrationMode::Off,
                start: true,
                shell: Some("/bin/sh".into()),
                settings: None,
                resume: None,
            };
            self.host.link.spawn(options).expect("a session on the remote host")
        }
    }

    #[cfg(unix)]
    impl Drop for FakeRemote {
        fn drop(&mut self) {
            self.pids.borrow_mut().extend(self.host.link.host_pid());
            for &pid in self.pids.borrow().iter() {
                crate::subprocess::kill(pid);
            }
            self.host.disconnect();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// 睡醒、网络变了只叫醒在等重连和重连放弃了的主机，要用户来办的、断开了的不动；放弃了的当场显示在连。
    #[test]
    fn nudge_wakes_only_hosts_waiting_to_reconnect() {
        let nudged = |status: Status, gave_up: bool, wanted: bool| {
            let host = super::RemoteHost::new("me@nudge".into(), runode_ssh::Destination::parse("me@nudge").unwrap());
            {
                let mut state = host.state();
                state.status = status;
                state.gave_up = gave_up;
                state.wanted = wanted;
            }
            host.nudge();
            let state = host.state();
            (state.nudged, state.status.clone())
        };
        let waiting = Status::Waiting { attempt: 3, retry_at: Instant::now(), error: "timed out".into() };
        assert!(nudged(waiting.clone(), false, true).0);
        assert!(!nudged(waiting, false, false).0);
        let stopped = || Status::Stopped { error: "timed out".into(), auth: false };
        assert_eq!(nudged(stopped(), true, true), (true, Status::Connecting(super::Phase::Authenticating)));
        assert_eq!(nudged(stopped(), false, true), (false, stopped()));
        assert!(!nudged(Status::Idle, false, true).0);
    }

    /// ssh 一时不收输入时，写进 socket 的照样马上写完，等 ssh 收了再按原样、按先后交给它。
    #[test]
    fn input_is_taken_while_ssh_is_not_reading() {
        use std::io::{Read as _, Write as _};

        let (mut ours, theirs) = runode_host::socket_pair().unwrap();
        let (mut stdout, stdin) = std::io::pipe().unwrap();
        std::thread::spawn(move || super::pump_in(&theirs, stdin));
        ours.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        let data: Vec<u8> = (0..4 << 20).map(|n: u32| n as u8).collect();
        let started = Instant::now();
        ours.write_all(&data).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2), "writing waited for ssh");
        drop(ours);
        let mut got = Vec::new();
        stdout.read_to_end(&mut got).unwrap();
        assert!(got == data, "ssh got {} bytes out of order or short", got.len());
    }

    /// 第一次连上时装上 `RUNODE_SSH_BINARY` 指的 runode、起桥接和宿主；掐断 ssh 后自己重连，会话还在；断开再连时
    /// 直接进桥接，不再装一遍。`make test-ssh` 编好 runode 再跑它。
    #[cfg(unix)]
    #[test]
    #[ignore = "runs a built runode over a fake ssh; make test-ssh"]
    fn deploys_reconnects_and_keeps_sessions_over_a_fake_ssh() {
        assert!(std::env::var_os("RUNODE_SSH_BINARY").is_some(), "RUNODE_SSH_BINARY points at a built runode");
        let fake = FakeRemote::new("fake-ssh");
        let name = fake.host.name.clone();
        let status = fake.connected();
        assert!(matches!(status, Status::Connected(_)), "not connected: {status:?}");
        assert!(next_connected(&name).is_some());
        let installed = fake.home.join(runode_ssh::bootstrap::BIN_DIR).join(deploy::install_dir()).join("runode");
        let modified = || std::fs::metadata(&installed).and_then(|meta| meta.modified()).expect("installed runode");
        let first = modified();
        let session = fake.spawn();

        // 掐断 ssh：自己重连上，会话还在。
        crate::subprocess::kill(fake.host.state().child.expect("ssh is running"));
        let alive = next_connected(&name).expect("reconnected");
        assert!(alive.contains(&session), "{session} is gone after reconnecting: {alive:?}");

        // 断开再连：装过的不再装，直接进桥接，会话还在。
        fake.host.disconnect();
        wait_for(&fake.host, Duration::from_secs(10), |status| matches!(status, Status::Idle));
        fake.host.retry_now();
        let alive = next_connected(&name).expect("connected again");
        assert!(alive.contains(&session));
        assert_eq!(modified(), first);

        fake.host.link.kill(session);
    }

    /// 远端跑着别的构建的宿主时（升级了 app），连上时装上这个构建，由它的新宿主接手旧宿主的会话，会话不断。
    /// `RUNODE_TEST_OLD_BINARY` 是构建号不同的 runode，`make test-ssh` 编好它再跑。
    #[cfg(unix)]
    #[test]
    #[ignore = "runs two builds of runode over a fake ssh; make test-ssh"]
    fn hands_sessions_to_a_new_build_over_a_fake_ssh() {
        let new = std::env::var_os("RUNODE_SSH_BINARY").expect("RUNODE_SSH_BINARY points at a built runode");
        let old = std::env::var_os("RUNODE_TEST_OLD_BINARY").expect("RUNODE_TEST_OLD_BINARY points at another build");
        // 装哪个按这个环境变量（`deploy::source`），换的时候主机没在连，读它的线程都停着。
        let use_binary = |binary: &std::ffi::OsStr| {
            // SAFETY: 测试一个个跑（`--test-threads=1`），这时没有别的线程读环境变量。
            unsafe { std::env::set_var("RUNODE_SSH_BINARY", binary) };
        };
        use_binary(&old);
        let fake = FakeRemote::new("fake-ssh-handoff");
        let name = fake.host.name.clone();
        let status = fake.connected();
        let Status::Connected(connected) = &status else { panic!("not connected to the old build: {status:?}") };
        assert!(connected.mismatch, "the old build reported this build: {connected:?}");
        assert!(next_connected(&name).is_some());
        let old_pid = fake.host.link.host_pid().expect("the old host");
        let session = fake.spawn();

        fake.host.disconnect();
        wait_for(&fake.host, Duration::from_secs(10), |status| matches!(status, Status::Idle));
        use_binary(&new);
        fake.host.retry_now();
        let alive = next_connected(&name).expect("connected to the new build");
        let status = fake.connected();
        let Status::Connected(connected) = &status else { panic!("not connected to the new build: {status:?}") };
        assert!(!connected.mismatch, "the old host is still serving: {connected:?}");
        assert_ne!(fake.host.link.host_pid(), Some(old_pid), "no new host took over");
        assert!(alive.contains(&session), "{session} was lost in the handoff: {alive:?}");

        fake.host.link.kill(session);
    }

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

    /// 对着真的 SSH 主机连：`RUNODE_TEST_SSH_HOST` 是连接目标，`RUNODE_SSH_BINARY`（或按平台的那个）指向那台
    /// 机器上跑得起来的 runode。ssh 要问（认主机指纹、要口令）时经 `RUNODE_TEST_ASKPASS` 指的本机 runode 问，
    /// 指纹答 yes，口令答 `RUNODE_TEST_SSH_SECRET`。连上、列会话，掐断 ssh
    /// 后自己重连上，经项目服务读远端，传文件，有主连接、远端不是 Windows 时再试端口转发。
    #[test]
    #[ignore = "needs an ssh host"]
    fn connects_and_reconnects_to_a_real_host() {
        let name = std::env::var("RUNODE_TEST_SSH_HOST").expect("RUNODE_TEST_SSH_HOST");
        // 连的时候 ssh 就可能问，开始连之前先有人收着。
        std::sync::LazyLock::force(&CONNECTED);
        let host = remote(&name).expect("a destination");
        let status = wait_for(&host, Duration::from_secs(600), |status| {
            matches!(status, Status::Connected(_) | Status::Stopped { .. })
        });
        assert!(matches!(status, Status::Connected(_)), "not connected: {status:?}");
        host.link.list_sessions(Duration::from_secs(10)).expect("list sessions");

        assert!(next_connected(&name).is_some());
        let pid = host.state().child.expect("ssh is running");
        crate::subprocess::kill(pid);
        // 没有主连接时自己重连要重新认证，不问用户，要口令的就停下来；这时像用户那样点重试。
        if ssh().control.is_none() {
            let status = wait_for(&host, Duration::from_secs(120), |status| match status {
                Status::Connected(_) => host.state().child.is_some_and(|now| now != pid),
                Status::Stopped { auth, .. } => *auth,
                _ => false,
            });
            if matches!(status, Status::Stopped { .. }) {
                host.retry_now();
            }
        }
        assert!(next_connected(&name).is_some(), "did not reconnect");
        assert!(matches!(host.status(), Status::Connected(_)));

        // 文件树、Git 面板经项目服务读远端，没有主连接时它自己认证。
        let project = crate::project_host::ProjectHost::of(&super::HostId::Ssh(name.clone()));
        let root = std::path::PathBuf::from("/");
        let listed = project.scan(root.clone(), Vec::new(), runode_git::UntrackedCache::default(), false);
        assert!(
            listed.is_ok_and(|scan| scan.listings.iter().any(|(dir, entries)| *dir == root && entries.is_some())),
            "the project service did not answer"
        );

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
        let windows = host.family() == super::Family::Windows;
        let read = |path: &str| {
            let script = if windows {
                format!(r#"cmd /d /c "type {}""#, path.replace('/', "\\"))
            } else {
                format!("cat {path}")
            };
            let mut command = host.exec(&ssh(), &script, true, false);
            let output = runode_ssh::process::output(command.stdin(std::process::Stdio::null())).unwrap();
            String::from_utf8(output.stdout).unwrap()
        };
        assert_eq!((read(&paths[0]), read(&paths[1])), ("from a".into(), "from b".into()));
        assert_eq!(read(&format!("{}/inner/deep.txt", paths[2])), "deep");
        let bytes = upload::Upload::Bytes { name: "shot.png".into(), bytes: b"png".to_vec() };
        let shot = upload::upload(&host, bytes).expect("upload bytes");
        assert_eq!(read(&shot[0]), "png");
        let _ = std::fs::remove_dir_all(&local);

        if std::env::var_os("RUNODE_TEST_SSH_SECRET").is_some() {
            let asked = ASKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(asked.contains(&runode_ssh::askpass::Kind::Secret), "ssh never asked through askpass: {asked:?}");
        }
        // 远端只在自己那里听的端口，经转发在本机打得开；转发挂在主连接上，没有主连接时不试。起来听的服务用的是
        // Unix 远端的 python3，Windows 远端也不试。
        if ssh().control.is_none() || windows {
            host.disconnect();
            return;
        }
        let serve = "cd /tmp && (python3 -m http.server 18765 --bind 127.0.0.1 > /dev/null 2>&1 & echo $!)";
        let output = ssh().exec(&host.dest, serve, true).output().unwrap();
        let server = String::from_utf8(output.stdout).unwrap().trim().to_owned();
        std::thread::sleep(Duration::from_secs(1));
        let id = HostId::Ssh(name);
        let url = hosts::forward::local_url(&id, "http://localhost:18765/").expect("forward");
        assert_eq!(hosts::forward::local_url(&id, "http://localhost:18765/").unwrap(), url);
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
