//! agent 停下来等回答、任务板上的任务做完时给登记过的手机推一条通知（线上格式和整个流程见
//! `runode_protocol::push`）。
//!
//! `Pusher` 跟着监听活在同一个进程里：开着推送、监听开着、设备表里至少有一条登记时，它像命令行一样
//! 经 `Connect` 连上宿主（`ClientKind::Cli`），每 `POLL_INTERVAL` 要一次会话列表，宿主认得任务板
//! （`Welcome` 的 `todos`）时接着要一次任务表（`ListTodos`），交给 `Rounds` 决定什么时候推，要读屏幕时
//! 发 `ReadScreen`；要发的推送交给发送线程（`send`）。两份回话谁先到不一定（宿主另起线程回），
//! `Rounds` 等同一轮的两份都到了才看任务，任务表按请求编号认是不是这一轮的；问过 `ListTodos` 后宿主
//! 发来的 `TodosChanged` 不理，下一轮照常要。不用
//! `Attach`：只看状态的连接也会让会话的 `SessionInfo::clients` 加一，桌面靠它为零回收后台已退出的会话。
//! 不满足条件时断开这条连接，单独跑的宿主照常能因空闲退出。
//!
//! 推送的路线按登记的 bundle id 定：等于配置的 `ApnsKey::bundle` 时用那把密钥直连 APNs，等于官方 App
//! 的 `OFFICIAL_BUNDLE` 而这个构建带着中转（`RELAY_URL`）时经中转服务，别的跳过（每个 bundle 记一次日志）。
//!
//! 发出去的通知撤不回，答完的由手机自己清掉。宿主说它交接给了新版本（`GoodbyeReason::Handoff`）以后
//! 这边不再连，推送由新宿主进程里的 `Pusher` 接着办。

mod apns;
mod curl;
mod probe;
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
    BuildId, Caps, ClientKind, ClientMsg, Frame, FrameKind, GoodbyeReason, HostMsg, PROTOCOL_VERSION,
    push::{ApnsEnv, Notification, OFFICIAL_BUNDLE, RELAY_URL},
    read_frame, write_frame,
};

pub use self::probe::{TestPush, send_test_push};
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
    /// 中转服务的基址；这个构建没带中转时为 `None`，见 `RELAY_URL`。
    pub relay: Option<String>,
}

impl Default for PushSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            text: true,
            delay: Duration::from_secs(10),
            direct: None,
            relay: RELAY_URL.map(str::to_owned),
        }
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
    rounds: Rounds,
    registrations: Vec<PushRegistration>,
    /// curl 能不能用，第一次要推时看一次。
    curl_ok: Option<bool>,
    /// 交给发送线程的密钥和中转基址，变了才再交。
    sent_settings: Option<(Option<ApnsKey>, Option<String>)>,
    /// 宿主交接给了新版本，不再连。
    handed_off: bool,
    /// 下次要会话列表、读设备表的时刻。
    next_poll: Instant,
    /// 连不上宿主的次数：第一次记警告，之后只记调试日志。
    connect_failures: u32,
    /// 记过日志的、推不了的 bundle id。
    skipped: HashSet<String>,
    /// 这一轮 `ListTodos` 的请求编号。
    todo_req: u32,
    /// 要过会话列表、宿主还没回。会话列表不带请求编号，宿主在单独的线程里等各会话回话（会话线程忙时
    /// 要等好几秒，比两轮之间还久），没回之前不开新的一轮：不然晚到的那份会和下一轮的任务表配成一对，
    /// 或者比下一轮的那份还晚到，把会话的状态往回改。
    listing: bool,
}

impl Poller {
    fn new(dirs: Dirs, connect: Connect, curl: Curl, sender: Sender) -> Self {
        Self {
            dirs,
            connect,
            curl,
            sender,
            host: None,
            rounds: Rounds::new(PushSettings::default().delay, true),
            registrations: Vec::new(),
            curl_ok: None,
            sent_settings: None,
            handed_off: false,
            next_poll: Instant::now(),
            connect_failures: 0,
            skipped: HashSet::new(),
            todo_req: 0,
            listing: false,
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
            // 关了推送或停了监听时马上清掉登记，排着没发的通知不等到下一轮才放掉。
            let poll = now >= self.next_poll
                || (!self.registrations.is_empty() && !(settings.enabled && listening == Listening::On));
            if poll {
                self.next_poll = now + POLL_INTERVAL;
                let registrations = if settings.enabled && listening == Listening::On {
                    // 设备表一时读不了（文件坏了、文件描述符用完）不等于手机都注销了：
                    // 照旧用上一次读到的登记，排着的通知也不丢，下一轮再读。
                    devices::push_registrations(&self.dirs)
                        .inspect_err(|err| tracing::warn!("cannot read push registrations: {err}"))
                        .ok()
                } else {
                    Some(Vec::new())
                };
                if let Some(registrations) = registrations {
                    if registrations != self.registrations {
                        // 关了推送、注销了或者换了 token 的手机，排着还没发出去的通知不发了。
                        let kept = registrations
                            .iter()
                            .map(|registration| (registration.device_id, registration.token.clone()));
                        self.sender.keep(kept.collect());
                    }
                    self.registrations = registrations;
                }
            }
            if !self.active(&settings, listening) {
                self.host = None;
                let state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
                if state.quit || state.settings != settings || state.listening != listening {
                    // 放开锁的这会儿设置变了，通知已经错过，马上再看一遍。
                } else if !settings.enabled || listening != Listening::On {
                    // 不读设备表，没有要定时做的事，只等 `set_settings`、`set_listening` 或丢掉 `Pusher` 叫醒。
                    drop(shared.changed.wait(state).unwrap_or_else(PoisonError::into_inner));
                } else {
                    let wait = self.next_poll.saturating_duration_since(Instant::now());
                    drop(shared.changed.wait_timeout(state, wait).unwrap_or_else(PoisonError::into_inner));
                }
                continue;
            }
            let rounds = &mut self.rounds;
            rounds.set_options(settings.delay, settings.text);
            if self.host.is_none() {
                match HostLink::connect(&self.connect) {
                    Ok(link) => {
                        self.host = Some(link);
                        self.connect_failures = 0;
                        self.listing = false;
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
            // 上一轮的会话列表还没回时这一轮不要，等它回来，见 `listing`。
            let ask = poll && !self.listing;
            if ask {
                self.todo_req = self.todo_req.wrapping_add(1);
                rounds.new_round();
                self.listing = true;
            }
            let req = self.todo_req;
            if ask
                && self.host.as_ref().is_some_and(|link| {
                    link.send(&ClientMsg::ListSessions).is_err()
                        || (link.todos && link.send(&ClientMsg::ListTodos { req }).is_err())
                })
            {
                self.host = None;
                continue;
            }
            let Some(link) = &self.host else { continue };
            let wait = self.next_poll.saturating_duration_since(Instant::now());
            let now = Instant::now();
            let actions = match link.messages.recv_timeout(wait) {
                Ok(HostMsg::SessionList { sessions }) => {
                    self.listing = false;
                    let mut actions = rounds.observe(&sessions, now);
                    if link.todos {
                        actions.extend(rounds.todo_sessions(&sessions, now, crate::now_unix()));
                    }
                    actions
                }
                // 上一轮晚到的任务表不要，免得盖掉这一轮的。
                Ok(HostMsg::Todos { req: answered, todos, .. }) if answered == req => {
                    rounds.set_todos(todos, now, crate::now_unix())
                }
                Ok(HostMsg::ScreenText { id, text, .. }) => rounds.screen(id, Some(&text)),
                Ok(HostMsg::Error { id: Some(id), .. }) => rounds.screen(id, None),
                // 宿主不给会话列表（连接积压着、等回话的请求太多）：这一轮没有了，下一轮再要。
                Ok(HostMsg::Error { req: None, id: None, .. }) => {
                    self.listing = false;
                    Vec::new()
                }
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

    /// 现在该不该连着宿主推：开着推送、监听开着、有推得出去的登记、curl 能用、宿主没交接走。
    fn active(&mut self, settings: &PushSettings, listening: Listening) -> bool {
        let routable = self.registrations.iter().any(|registration| route(registration, settings).is_some());
        if !settings.enabled || listening != Listening::On || !routable || self.handed_off {
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
                Action::Notify(id, meta, lines) => {
                    let targets = self.targets(settings, |registration| {
                        Notification::new(registration.machine.clone(), &registration.machine_name, id, &meta, &lines)
                    });
                    if !targets.is_empty() {
                        self.sender.notify(targets);
                    }
                }
                Action::TodoDone(id, title) => {
                    let targets = self.targets(settings, |registration| {
                        Notification::todo_done(registration.machine.clone(), &registration.machine_name, id, &title)
                    });
                    if !targets.is_empty() {
                        self.sender.notify(targets);
                    }
                }
                Action::Cancel(id) => self.sender.cancel(id),
            }
        }
    }

    /// 一条通知推给哪些手机；`notification` 给每台手机拼它那份，带着它自己给这台电脑编的标识和名字。
    fn targets(
        &mut self,
        settings: &PushSettings,
        notification: impl Fn(&PushRegistration) -> Notification,
    ) -> Vec<Target> {
        let mut targets = Vec::new();
        for registration in &self.registrations {
            let Some(route) = route(registration, settings) else {
                if self.skipped.insert(registration.bundle.clone()) {
                    let why = if registration.bundle == OFFICIAL_BUNDLE {
                        "this build has no push relay"
                    } else {
                        "no APNs key is configured for it"
                    };
                    tracing::info!("not pushing to the app {} ({:?}): {why}", registration.bundle, registration.env);
                }
                continue;
            };
            targets.push(Target {
                route,
                device: registration.device_id,
                token: registration.token.clone(),
                notification: notification(registration),
            });
        }
        targets
    }
}

/// 登记 `registration` 走哪条路线，见模块文档。
fn route(registration: &PushRegistration, settings: &PushSettings) -> Option<Route> {
    if registration.env == ApnsEnv::Unknown {
        return None;
    }
    let via = if settings.direct.as_ref().is_some_and(|key| key.bundle == registration.bundle) {
        Via::Direct
    } else if registration.bundle == OFFICIAL_BUNDLE && settings.relay.is_some() {
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
    /// 宿主认得任务板（`Welcome` 的 `todos`）；旧宿主不认得，对 `ListTodos` 回 `Error`，不发。
    todos: bool,
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
        let mut link = Self { stream, messages, todos: false };
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
            Ok(HostMsg::Welcome { todos, .. }) => {
                link.todos = todos;
                Ok(link)
            }
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
