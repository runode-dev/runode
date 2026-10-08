//! agent 停下来等回答时推 Live Activity 到登记过的手机上（线上格式和整个流程见
//! `runode_protocol::push`）。
//!
//! `Pusher` 跟着监听活在同一个进程里：开着推送、监听开着、设备表里至少有一条登记时，它像命令行一样
//! 经 `Connect` 连上宿主（`ClientKind::Cli`），每 `POLL_INTERVAL` 要一次会话列表，交给 `Rounds`
//! 决定什么时候起、刷新、收起，要读屏幕时发 `ReadScreen`；要发的推送交给发送线程（`send`）。不用
//! `Attach`：只看状态的连接也会让会话的 `SessionInfo::clients` 加一，桌面靠它为零回收后台已退出的会话。
//! 不满足条件时断开这条连接，单独跑的宿主照常能因空闲退出。
//!
//! 推送的路线按登记的 bundle id 定：等于配置的 `ApnsKey::bundle` 时用那把密钥直连 APNs，等于官方 App
//! 的 `OFFICIAL_BUNDLE` 时经中转服务，别的跳过（每个 bundle 记一次日志）。
//!
//! 关了推送、关了远程访问、登记都删光了时，开着的回合都收起。监听只是暂时没开成（端口被占、交接时
//! 旧宿主还没放手）时不收起，回合留在频道文件里。宿主说它交接给了新版本（`GoodbyeReason::Handoff`）
//! 以后这边不再连，开着的回合由新宿主进程里的 `Pusher` 从频道文件接着办。

mod apns;
mod curl;
mod rounds;
mod send;
#[cfg(test)]
mod tests;

use std::{
    collections::HashSet,
    io,
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, PoisonError, mpsc},
    thread,
    time::{Duration, Instant},
};

use runode_paths::Dirs;
use runode_protocol::{
    BuildId, Caps, ClientKind, ClientMsg, Frame, FrameKind, GoodbyeReason, HostMsg, PROTOCOL_VERSION, SessionId,
    push::{ActivityAttributes, ApnsEnv, OFFICIAL_BUNDLE, RELAY_URL},
    read_frame, write_frame,
};

use self::{
    apns::{Route, Via},
    curl::Curl,
    rounds::{Action, Rounds},
    send::{Sender, Target},
};
use crate::{
    Connect,
    devices::{self, PushRegistration},
};

/// 隔多久要一次会话列表、看一眼设备表。
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 读屏幕时要最后这么多行，从里面挑有字的几行：选项和按键提示不要，窄屏下它们折成好几行也还够读到
/// 上面的问题和命令。
const SCREEN_LINES: u32 = 30;
/// 连上宿主后最多等这么久的 `Welcome`。
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// 推送的设置，来自配置（`remote-access-push` 这几项）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushSettings {
    /// 推不推。
    pub enabled: bool,
    /// 带不带屏幕上的文字（问题和选项）。
    pub text: bool,
    /// agent 等回答过了这么久还在等才推。
    pub delay: Duration,
    /// 直连 APNs 用的密钥；没配时为 `None`，只能经中转推官方 App。
    pub direct: Option<ApnsKey>,
    /// 中转服务的基址。
    pub relay: String,
}

impl Default for PushSettings {
    fn default() -> Self {
        Self { enabled: true, text: true, delay: Duration::from_secs(10), direct: None, relay: RELAY_URL.to_owned() }
    }
}

/// 直连 APNs 用的密钥：Apple 开发者后台下载的 .p8 文件、它的 key id、开发者账号的 team id，以及用它
/// 推哪个 App（bundle id）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApnsKey {
    pub key_file: PathBuf,
    pub key_id: String,
    pub team_id: String,
    pub bundle: String,
}

/// 监听现在怎样，由 `Service` 告诉 `Pusher`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Listening {
    /// 关着远程访问。
    Off,
    /// 要开着，还没开成。
    Starting,
    On,
}

/// 推送的后台线程，见模块文档。丢掉时线程自己结束，不等它。
pub(crate) struct Pusher {
    shared: Arc<Shared>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    settings: PushSettings,
    listening: Listening,
    quit: bool,
}

impl Pusher {
    /// 起后台线程。设置先按默认的，等 `set_settings`；监听先当关着，等 `set_listening`。
    pub(crate) fn start(dirs: Dirs, connect: Connect) -> io::Result<Self> {
        Self::start_with(dirs, connect, Curl::default())
    }

    fn start_with(dirs: Dirs, connect: Connect, curl: Curl) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State { settings: PushSettings::default(), listening: Listening::Off, quit: false }),
            changed: Condvar::new(),
        });
        let sender = Sender::start(dirs.clone(), curl.clone())?;
        let thread_shared = shared.clone();
        thread::Builder::new().name("remote-access-poll".into()).spawn(move || {
            Poller::new(dirs, connect, curl, sender).run(&thread_shared);
        })?;
        Ok(Self { shared })
    }

    pub(crate) fn set_settings(&self, settings: PushSettings) {
        self.update(|state| {
            let changed = state.settings != settings;
            state.settings = settings;
            changed
        });
    }

    pub(crate) fn set_listening(&self, listening: Listening) {
        self.update(|state| std::mem::replace(&mut state.listening, listening) != listening);
    }

    fn update(&self, change: impl FnOnce(&mut State) -> bool) {
        let mut state = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if change(&mut state) {
            self.shared.changed.notify_all();
        }
    }
}

impl Drop for Pusher {
    fn drop(&mut self) {
        self.update(|state| {
            state.quit = true;
            true
        });
    }
}

/// 轮询宿主的线程。
struct Poller {
    dirs: Dirs,
    connect: Connect,
    curl: Curl,
    sender: Sender,
    host: Option<HostLink>,
    /// 第一次要推时才从频道文件读出上次没收起的回合。
    rounds: Option<Rounds>,
    registrations: Vec<PushRegistration>,
    /// curl 能不能用，第一次要推时看一次。
    curl_ok: Option<bool>,
    /// 交给发送线程的密钥和中转基址，变了才再交。
    sent_settings: Option<(Option<ApnsKey>, String)>,
    /// 宿主交接给了新版本，不再连。
    handed_off: bool,
    /// 下次要会话列表、读设备表的时刻。
    next_poll: Instant,
    /// 连不上宿主的次数：第一次记警告，之后只记调试日志。
    connect_failures: u32,
    /// 记过日志的、推不了的 bundle id。
    skipped: HashSet<String>,
}

impl Poller {
    fn new(dirs: Dirs, connect: Connect, curl: Curl, sender: Sender) -> Self {
        Self {
            dirs,
            connect,
            curl,
            sender,
            host: None,
            rounds: None,
            registrations: Vec::new(),
            curl_ok: None,
            sent_settings: None,
            handed_off: false,
            next_poll: Instant::now(),
            connect_failures: 0,
            skipped: HashSet::new(),
        }
    }

    fn run(&mut self, shared: &Shared) {
        loop {
            let (settings, listening) = {
                let state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
                if state.quit {
                    return;
                }
                (state.settings.clone(), state.listening)
            };
            let wanted = (settings.direct.clone(), settings.relay.clone());
            if self.sent_settings.as_ref() != Some(&wanted) {
                self.sender.settings(wanted.0.clone(), wanted.1.clone());
                self.sent_settings = Some(wanted);
            }
            let now = Instant::now();
            let poll = now >= self.next_poll;
            if poll {
                self.next_poll = now + POLL_INTERVAL;
                self.registrations = if settings.enabled && listening == Listening::On {
                    devices::push_registrations(&self.dirs).unwrap_or_else(|err| {
                        tracing::warn!("cannot read push registrations: {err}");
                        Vec::new()
                    })
                } else {
                    Vec::new()
                };
            }
            if !self.active(&settings, listening) {
                self.host = None;
                // 监听只是暂时没开成时回合留着，见模块文档。
                if listening != Listening::Starting
                    && !self.handed_off
                    && let Some(rounds) = &mut self.rounds
                {
                    let actions = rounds.end_all();
                    self.dispatch(&settings, actions);
                }
                let state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
                if !state.quit {
                    let wait = self.next_poll.saturating_duration_since(Instant::now());
                    drop(shared.changed.wait_timeout(state, wait).unwrap_or_else(PoisonError::into_inner));
                }
                continue;
            }
            let restored = if self.rounds.is_none() { Some(self.sender.load()) } else { None };
            let rounds = self
                .rounds
                .get_or_insert_with(|| Rounds::new(restored.unwrap_or_default(), settings.delay, settings.text));
            rounds.set_options(settings.delay, settings.text);
            if self.host.is_none() {
                match HostLink::connect(&self.connect) {
                    Ok(link) => {
                        self.host = Some(link);
                        self.connect_failures = 0;
                        rounds.reconnected();
                        self.next_poll = Instant::now();
                        continue;
                    }
                    Err(err) => {
                        if self.connect_failures == 0 {
                            tracing::warn!("cannot reach the host to watch for agents waiting for an answer: {err}");
                        } else {
                            tracing::debug!("still cannot reach the host: {err}");
                        }
                        self.connect_failures += 1;
                        let state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
                        if !state.quit {
                            drop(
                                shared
                                    .changed
                                    .wait_timeout(state, POLL_INTERVAL)
                                    .unwrap_or_else(PoisonError::into_inner),
                            );
                        }
                        continue;
                    }
                }
            }
            if poll && self.host.as_ref().is_some_and(|link| link.send(&ClientMsg::ListSessions).is_err()) {
                self.host = None;
                continue;
            }
            let Some(link) = &self.host else { continue };
            let wait = self.next_poll.saturating_duration_since(Instant::now());
            let now = Instant::now();
            let actions = match link.messages.recv_timeout(wait) {
                Ok(HostMsg::SessionList { sessions }) => rounds.observe(&sessions, now),
                Ok(HostMsg::ScreenText { id, text, .. }) => rounds.screen(id, Some(&text), now),
                Ok(HostMsg::Error { id: Some(id), .. }) => rounds.screen(id, None, now),
                Ok(HostMsg::Goodbye { reason }) => {
                    if reason == GoodbyeReason::Handoff {
                        tracing::info!("the host handed its sessions over, leaving the pushes to the new host");
                        self.handed_off = true;
                    }
                    self.host = None;
                    Vec::new()
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => Vec::new(),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    tracing::debug!("lost the host connection used for pushes");
                    self.host = None;
                    Vec::new()
                }
            };
            self.dispatch(&settings, actions);
        }
    }

    /// 现在该不该连着宿主推：开着推送、监听开着、有登记、curl 能用、宿主没交接走。
    fn active(&mut self, settings: &PushSettings, listening: Listening) -> bool {
        if !settings.enabled || listening != Listening::On || self.registrations.is_empty() || self.handed_off {
            return false;
        }
        *self.curl_ok.get_or_insert_with(|| {
            self.curl
                .check()
                .map_err(|err| tracing::warn!("push notifications are off: curl is needed to reach APNs ({err})"))
                .is_ok()
        })
    }

    fn dispatch(&mut self, settings: &PushSettings, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::ReadScreen(id) => {
                    let read = ClientMsg::ReadScreen { id, lines: Some(SCREEN_LINES), command: None };
                    if self.host.as_ref().is_some_and(|link| link.send(&read).is_err()) {
                        self.host = None;
                    }
                }
                Action::Start(id, content) => {
                    let groups = self.groups(settings, id);
                    if !groups.is_empty() {
                        self.sender.open(id, groups, content);
                    }
                }
                Action::Update(id, content) => self.sender.update(id, content),
                Action::End(id, content) => self.sender.end(id, content),
            }
        }
    }

    /// 会话 `session` 推给哪些手机，按路线分组。
    fn groups(&mut self, settings: &PushSettings, session: SessionId) -> Vec<(Route, Vec<Target>)> {
        let mut groups: Vec<(Route, Vec<Target>)> = Vec::new();
        for registration in &self.registrations {
            let Some(route) = route(registration, settings) else {
                if self.skipped.insert(registration.bundle.clone()) {
                    tracing::info!(
                        "not pushing to the app {} ({:?}): no APNs key is configured for it",
                        registration.bundle,
                        registration.env
                    );
                }
                continue;
            };
            let target = Target {
                device: registration.device_id,
                token: registration.token.clone(),
                attributes: ActivityAttributes::new(registration.machine.clone(), &registration.machine_name, session),
            };
            match groups.iter_mut().find(|(known, _)| *known == route) {
                Some((_, targets)) => targets.push(target),
                None => groups.push((route, vec![target])),
            }
        }
        groups
    }
}

/// 登记 `registration` 走哪条路线，见模块文档。
fn route(registration: &PushRegistration, settings: &PushSettings) -> Option<Route> {
    if registration.env == ApnsEnv::Unknown {
        return None;
    }
    let via = if settings.direct.as_ref().is_some_and(|key| key.bundle == registration.bundle) {
        Via::Direct
    } else if registration.bundle == OFFICIAL_BUNDLE {
        Via::Relay
    } else {
        return None;
    };
    Some(Route { env: registration.env, via })
}

/// 到宿主的一条连接：读的一半在单独的线程里，把控制消息送到 `messages`。丢掉时断开，读的线程随之
/// 结束。
struct HostLink {
    stream: UnixStream,
    messages: mpsc::Receiver<HostMsg>,
}

impl HostLink {
    fn connect(connect: &Connect) -> io::Result<Self> {
        let stream = connect()?;
        let mut reader = stream.try_clone()?;
        let (tx, messages) = mpsc::channel();
        thread::Builder::new().name("remote-access-poll-read".into()).spawn(move || {
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
        let link = Self { stream, messages };
        link.send(&ClientMsg::Hello {
            protocol: PROTOCOL_VERSION,
            // 只在要快照时比对构建，这里不要。
            build: BuildId(String::new()),
            client: ClientKind::Cli,
            caps: Caps::default(),
            session: None,
            device: None,
        })?;
        match link.messages.recv_timeout(HELLO_TIMEOUT) {
            Ok(HostMsg::Welcome { .. }) => Ok(link),
            Ok(HostMsg::Incompatible { reason, .. }) => Err(io::Error::other(format!("the host refused: {reason}"))),
            Ok(_) => Err(io::Error::other("unexpected answer to hello")),
            Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "the host did not answer hello")),
        }
    }

    fn send(&self, message: &ClientMsg) -> io::Result<()> {
        let frame = Frame::control(message).map_err(io::Error::other)?;
        write_frame(&mut &self.stream, frame.kind, frame.channel, &frame.payload).map_err(io::Error::other)
    }
}

impl Drop for HostLink {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}
