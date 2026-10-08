//! 发推送的线程：`Rounds` 决定了的事（起、刷新、收起）在这里拆成一个个 HTTPS 请求排队发，失败的按
//! 情况退避重试、换环境或者放弃。一个会话的请求按先后发：先建频道再起 Live Activity，收起完了再删
//! 频道；不同会话之间互不耽误，一个会话在退避时别的会话照发。
//!
//! 正用着的频道（每个会话在每条路线上一个）和推出去的最后内容记在 `remote_access_push_channels_file`
//! 里，删了频道才去掉；监听方重启后 `load` 读回来，由 `Rounds` 决定接着用还是收起。
//!
//! 出错时：
//! - 没拿到响应、5xx、429：隔 2、10、30 秒（之后都是 30 秒，429 带了 `Retry-After` 时至少等那么久）
//!   再试；起 Live Activity、建频道和刷新最多试到排进队一分钟，收起和删频道最多五分钟。
//! - push-to-start token 不认（410、400 `BadDeviceToken`）：可能是登记的环境不对，换另一个环境（另建
//!   一个频道）再起一次；成了把登记的环境改过来，还不认就删掉这条登记。
//! - 直连的密钥不对（403 `InvalidProviderToken`）：记错误，直连的都不发了，直到配置换了密钥。
//! - 别的错误：记下来，这件事不办了。

use std::{
    collections::HashSet,
    io,
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use runode_paths::Dirs;
use runode_protocol::{
    SessionId,
    push::{ActivityAttributes, ActivityContent, ActivityEvent},
    remote::DeviceId,
};
use serde::{Deserialize, Serialize};

use super::{
    ApnsKey,
    apns::{self, Call, DirectKey, Outcome, Route, Via},
    curl::Curl,
};
use crate::{
    devices,
    files::{no_home, read_json, write_json},
};

/// 起 Live Activity、建频道、刷新最多试这么久，过了就不发了：等回答的事过时了。
const SHORT_DEADLINE: Duration = Duration::from_secs(60);
/// 收起和删频道最多试这么久：不收起的话手机上的 Live Activity 一直挂到过时。
const LONG_DEADLINE: Duration = Duration::from_secs(5 * 60);
/// 第几次失败后隔多久再试，之后都按最后一项。
const BACKOFF: [Duration; 3] = [Duration::from_secs(2), Duration::from_secs(10), Duration::from_secs(30)];
/// 频道文件的格式版本。
const FILE_VERSION: u32 = 1;

/// 推给一台手机：哪台设备的哪个 push-to-start token，带着这台手机的 attributes。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) device: DeviceId,
    pub(crate) token: String,
    pub(crate) attributes: ActivityAttributes,
}

/// 交给发送线程的事。
enum Command {
    /// 直连的密钥（`None` 是没配）和中转服务的基址。
    Settings(Option<ApnsKey>, String),
    /// 读频道文件（只在第一次读），回上次没收起的会话和推出去的最后内容。
    Load(mpsc::Sender<Vec<(SessionId, ActivityContent)>>),
    /// 起 Live Activity：每条路线建一个频道，再推给路线上的每台手机。
    Open { session: SessionId, groups: Vec<(Route, Vec<Target>)>, content: ActivityContent },
    /// 刷新或者收起会话所有的频道。
    Broadcast { session: SessionId, event: ActivityEvent, content: ActivityContent },
}

/// 发送线程的把手。丢掉时线程办完手上那个请求就结束，不等排着的：没办完的收起下次启动时再办，见
/// 模块文档。
pub(crate) struct Sender {
    tx: mpsc::Sender<Command>,
}

impl Sender {
    pub(crate) fn start(dirs: Dirs, curl: Curl) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let mut worker = Worker {
            dirs,
            curl,
            direct_config: None,
            direct: None,
            direct_broken: false,
            relay: runode_protocol::push::RELAY_URL.to_owned(),
            queue: Vec::new(),
            open: Vec::new(),
            loaded: false,
        };
        thread::Builder::new().name("remote-access-push".into()).spawn(move || worker.run(&rx))?;
        Ok(Self { tx })
    }

    pub(crate) fn settings(&self, direct: Option<ApnsKey>, relay: String) {
        let _ = self.tx.send(Command::Settings(direct, relay));
    }

    /// 上次没收起的会话和推出去的最后内容，见 `Command::Load`。
    pub(crate) fn load(&self) -> Vec<(SessionId, ActivityContent)> {
        let (reply, answer) = mpsc::channel();
        if self.tx.send(Command::Load(reply)).is_err() {
            return Vec::new();
        }
        answer.recv().unwrap_or_default()
    }

    pub(crate) fn open(&self, session: SessionId, groups: Vec<(Route, Vec<Target>)>, content: ActivityContent) {
        let _ = self.tx.send(Command::Open { session, groups, content });
    }

    pub(crate) fn update(&self, session: SessionId, content: ActivityContent) {
        let _ = self.tx.send(Command::Broadcast { session, event: ActivityEvent::Update, content });
    }

    pub(crate) fn end(&self, session: SessionId, content: ActivityContent) {
        let _ = self.tx.send(Command::Broadcast { session, event: ActivityEvent::End, content });
    }
}

/// 一个会话正用着的频道和推出去的最后内容，写进频道文件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OpenSession {
    session: SessionId,
    content: ActivityContent,
    channels: Vec<OpenChannel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OpenChannel {
    route: Route,
    /// APNs 给的频道 id。
    channel: String,
}

#[derive(Default, Serialize, Deserialize)]
struct ChannelsFile {
    version: u32,
    sessions: Vec<OpenSession>,
}

/// 排着的一个请求。
struct Queued {
    session: SessionId,
    work: Work,
    /// 这之前不发：在退避。
    not_before: Instant,
    /// 过了这个时刻还没办成就放弃。
    deadline: Instant,
    /// 失败了几次。
    failures: usize,
}

#[derive(Clone, Debug)]
enum Work {
    CreateChannel {
        route: Route,
        content: ActivityContent,
    },
    Start {
        route: Route,
        target: Target,
        content: ActivityContent,
        other_env: bool,
    },
    /// 刷新或者收起，轮到时拆成会话每个频道的 `BroadcastTo`（收起时再加上删频道）。
    Broadcast {
        event: ActivityEvent,
        content: ActivityContent,
    },
    BroadcastTo {
        route: Route,
        channel: String,
        event: ActivityEvent,
        content: ActivityContent,
    },
    DeleteChannel {
        route: Route,
        channel: String,
    },
}

impl Work {
    fn deadline(&self) -> Duration {
        match self {
            Self::Broadcast { event: ActivityEvent::End, .. }
            | Self::BroadcastTo { event: ActivityEvent::End, .. }
            | Self::DeleteChannel { .. } => LONG_DEADLINE,
            _ => SHORT_DEADLINE,
        }
    }

    /// 收起时还排着就不用办了：起 Live Activity 和刷新。
    fn moot_after_end(&self) -> bool {
        match self {
            Self::CreateChannel { .. } | Self::Start { .. } => true,
            Self::Broadcast { event, .. } | Self::BroadcastTo { event, .. } => *event == ActivityEvent::Update,
            Self::DeleteChannel { .. } => false,
        }
    }

    fn route(&self) -> Option<Route> {
        match self {
            Self::CreateChannel { route, .. }
            | Self::Start { route, .. }
            | Self::BroadcastTo { route, .. }
            | Self::DeleteChannel { route, .. } => Some(*route),
            Self::Broadcast { .. } => None,
        }
    }
}

fn queued(session: SessionId, work: Work, now: Instant) -> Queued {
    Queued { session, deadline: now + work.deadline(), work, not_before: now, failures: 0 }
}

struct Worker {
    dirs: Dirs,
    curl: Curl,
    /// 配置里的密钥，变了才重读。
    direct_config: Option<ApnsKey>,
    /// 读好的密钥；没配或者读不了时为 `None`。
    direct: Option<DirectKey>,
    /// APNs 说密钥不对，配置换了密钥之前直连的都不发。
    direct_broken: bool,
    relay: String,
    queue: Vec<Queued>,
    open: Vec<OpenSession>,
    /// 读过频道文件了。
    loaded: bool,
}

impl Worker {
    fn run(&mut self, rx: &mpsc::Receiver<Command>) {
        loop {
            let now = Instant::now();
            let (ready, wake) = self.next(now);
            let command = match (ready, wake) {
                (Some(_), _) => rx.try_recv().map_err(|err| match err {
                    mpsc::TryRecvError::Empty => RecvTimeoutError::Timeout,
                    mpsc::TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
                }),
                (None, Some(wake)) => rx.recv_timeout(wake.saturating_duration_since(now)),
                (None, None) => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match command {
                Ok(command) => self.command(command),
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(index) = ready {
                        self.work(index);
                    }
                }
            }
        }
    }

    /// 能办的第一个请求（每个会话只看排在最前面的那个），和没有能办的时下次什么时候有。
    fn next(&self, now: Instant) -> (Option<usize>, Option<Instant>) {
        let mut seen = HashSet::new();
        let mut wake: Option<Instant> = None;
        for (index, item) in self.queue.iter().enumerate() {
            if !seen.insert(item.session) {
                continue;
            }
            if item.not_before <= now {
                return (Some(index), None);
            }
            wake = Some(wake.map_or(item.not_before, |wake| wake.min(item.not_before)));
        }
        (None, wake)
    }

    fn command(&mut self, command: Command) {
        let now = Instant::now();
        match command {
            Command::Settings(direct, relay) => {
                self.relay = relay;
                if direct != self.direct_config {
                    self.direct_broken = false;
                    self.direct = direct.as_ref().and_then(|key| {
                        DirectKey::load(&key.key_file, &key.key_id, &key.team_id, &key.bundle)
                            .map_err(|err| tracing::warn!("cannot push straight to APNs: {err}"))
                            .ok()
                    });
                    self.direct_config = direct;
                }
            }
            Command::Load(reply) => {
                if !self.loaded {
                    self.loaded = true;
                    match self.read_file() {
                        Ok(open) => self.open = open,
                        Err(err) => tracing::warn!("cannot read the push channels in use: {err}"),
                    }
                }
                let _ = reply.send(self.open.iter().map(|open| (open.session, open.content.clone())).collect());
            }
            Command::Open { session, groups, content } => {
                for (route, targets) in groups {
                    self.queue.push(queued(session, Work::CreateChannel { route, content: content.clone() }, now));
                    for target in targets {
                        let work = Work::Start { route, target, content: content.clone(), other_env: false };
                        self.queue.push(queued(session, work, now));
                    }
                }
            }
            Command::Broadcast { session, event, content } => {
                // 新内容盖过还没发的旧内容；收起时还没起来的不用起了。
                self.queue.retain(|item| {
                    item.session != session
                        || match event {
                            ActivityEvent::End => !item.work.moot_after_end(),
                            _ => !matches!(&item.work, Work::Broadcast { event: ActivityEvent::Update, .. }),
                        }
                });
                self.queue.push(queued(session, Work::Broadcast { event, content }, now));
            }
        }
    }

    /// 办排在 `index` 的请求。
    fn work(&mut self, index: usize) {
        let session = self.queue[index].session;
        if let Work::Broadcast { event, content } = &self.queue[index].work {
            let (event, content) = (*event, content.clone());
            let item = self.queue.remove(index);
            self.expand(index, session, event, content, item.deadline);
            return;
        }
        let route = self.queue[index].work.route();
        if route.is_some_and(|route| route.via == Via::Direct) && (self.direct.is_none() || self.direct_broken) {
            tracing::debug!("not pushing to session {session}: no usable APNs key");
            let item = self.queue.remove(index);
            self.finished(&item);
            return;
        }
        let work = self.queue[index].work.clone();
        let result = self.send(session, &work);
        match result {
            Outcome::Done(channel) => {
                let item = self.queue.remove(index);
                self.succeeded(&item, channel);
                self.finished(&item);
            }
            Outcome::Retry { after, why } => self.retry(index, after, &why),
            Outcome::ExpiredJwt => {
                if let Some(key) = &mut self.direct {
                    key.forget_jwt();
                }
                self.retry(index, Some(Duration::ZERO), "the APNs token expired");
            }
            Outcome::BadToken(why) => {
                let item = self.queue.remove(index);
                self.bad_token(index, item, &why);
            }
            Outcome::BadKey(why) => {
                tracing::error!(
                    "APNs refused the configured key ({why}); not pushing straight to APNs until it changes"
                );
                self.direct_broken = true;
                let item = self.queue.remove(index);
                self.finished(&item);
            }
            Outcome::Failed(why) => {
                tracing::warn!("push for session {session} failed: {why}");
                let item = self.queue.remove(index);
                self.finished(&item);
            }
        }
    }

    /// 发一个请求，读回结果。
    fn send(&mut self, session: SessionId, work: &Work) -> Outcome {
        let channel_of = |route: Route| self.channel(session, route).map(str::to_owned);
        let (route, owned_channel) = match work {
            Work::Start { route, .. } => (*route, channel_of(*route)),
            Work::CreateChannel { route, .. } => (*route, None),
            Work::BroadcastTo { route, .. } | Work::DeleteChannel { route, .. } => (*route, None),
            Work::Broadcast { .. } => return Outcome::Done(None),
        };
        let call = match work {
            Work::CreateChannel { route, .. } => {
                if self.channel(session, *route).is_some() {
                    return Outcome::Done(None);
                }
                Call::CreateChannel
            }
            Work::Start { target, content, .. } => {
                let Some(channel) = owned_channel.as_deref() else {
                    tracing::debug!("not starting a Live Activity for session {session}: no channel");
                    return Outcome::Failed("no channel".into());
                };
                Call::Start { token: &target.token, channel, attributes: &target.attributes, content }
            }
            Work::BroadcastTo { channel, event, content, .. } => Call::Broadcast { channel, event: *event, content },
            Work::DeleteChannel { channel, .. } => Call::DeleteChannel { channel },
            Work::Broadcast { .. } => return Outcome::Done(None),
        };
        let request = match apns::request(route, &call, self.direct.as_mut(), &self.relay, crate::now_unix()) {
            Ok(request) => request,
            Err(why) => return Outcome::Failed(why),
        };
        apns::outcome(route.via, &call, self.curl.send(&request))
    }

    /// 刷新或者收起：拆成会话每个频道上的请求，收起时再删频道，排在原来的位置。
    fn expand(
        &mut self,
        index: usize,
        session: SessionId,
        event: ActivityEvent,
        content: ActivityContent,
        deadline: Instant,
    ) {
        let now = Instant::now();
        let Some(open) = self.open.iter_mut().find(|open| open.session == session) else { return };
        if open.content != content {
            open.content = content.clone();
            self.save();
        }
        let Some(open) = self.open.iter().find(|open| open.session == session) else { return };
        let mut works: Vec<Work> = open
            .channels
            .iter()
            .map(|open| Work::BroadcastTo {
                route: open.route,
                channel: open.channel.clone(),
                event,
                content: content.clone(),
            })
            .collect();
        if event == ActivityEvent::End {
            works.extend(
                open.channels
                    .iter()
                    .map(|open| Work::DeleteChannel { route: open.route, channel: open.channel.clone() }),
            );
        }
        let items = works
            .into_iter()
            .map(|work| Queued { session, work, not_before: now, deadline, failures: 0 })
            .collect::<Vec<_>>();
        self.queue.splice(index..index, items);
    }

    /// 没办成，按退避排到后面再试；过了期限就放弃。
    fn retry(&mut self, index: usize, after: Option<Duration>, why: &str) {
        let item = &mut self.queue[index];
        let backoff = BACKOFF[item.failures.min(BACKOFF.len() - 1)];
        item.failures += 1;
        let not_before = Instant::now() + after.map_or(backoff, |after| after.max(backoff));
        if not_before > item.deadline {
            tracing::warn!("giving up a push for session {}: {why}", item.session);
            let item = self.queue.remove(index);
            self.finished(&item);
            return;
        }
        tracing::debug!("push for session {} will be retried: {why}", item.session);
        item.not_before = not_before;
    }

    /// 办成了以后：建好的频道记下来；换了环境才推成的，把登记的环境改过来。
    fn succeeded(&mut self, item: &Queued, channel: Option<String>) {
        match &item.work {
            Work::CreateChannel { route, content } => {
                let Some(channel) = channel else { return };
                let session = item.session;
                let open = match self.open.iter_mut().position(|open| open.session == session) {
                    Some(at) => &mut self.open[at],
                    None => {
                        self.open.push(OpenSession { session, content: content.clone(), channels: Vec::new() });
                        self.open.last_mut().expect("just pushed")
                    }
                };
                open.channels.push(OpenChannel { route: *route, channel });
                self.save();
            }
            Work::Start { route, target, other_env: true, .. } => {
                match devices::set_push_env(&self.dirs, target.device, &target.token, route.env) {
                    Ok(true) => {
                        tracing::info!("remote device {} is registered for {:?} push now", target.device, route.env)
                    }
                    Ok(false) => {}
                    Err(err) => tracing::warn!("cannot update a push registration: {err}"),
                }
            }
            _ => {}
        }
    }

    /// 一个请求办完了（成了或者放弃了）：删频道的，频道从记录里去掉。
    fn finished(&mut self, item: &Queued) {
        let Work::DeleteChannel { channel, .. } = &item.work else { return };
        let Some(at) = self.open.iter().position(|open| open.session == item.session) else { return };
        self.open[at].channels.retain(|open| open.channel != *channel);
        if self.open[at].channels.is_empty() {
            self.open.remove(at);
        }
        self.save();
    }

    /// push-to-start token 不认：第一次换另一个环境再起（那边没有频道时先建一个），换了还不认就删掉
    /// 登记。
    fn bad_token(&mut self, index: usize, item: Queued, why: &str) {
        let Work::Start { route, target, content, other_env } = item.work else { return };
        if other_env {
            tracing::info!("APNs does not know the push token of remote device {} ({why}), dropping it", target.device);
            if let Err(err) = devices::drop_push_token(&self.dirs, target.device, &target.token) {
                tracing::warn!("cannot remove a push registration: {err}");
            }
            return;
        }
        tracing::debug!("APNs refused a push token in {:?} ({why}), trying {:?}", route.env, route.other_env().env);
        let now = Instant::now();
        let other = route.other_env();
        let works = [
            Work::CreateChannel { route: other, content: content.clone() },
            Work::Start { route: other, target, content, other_env: true },
        ];
        let items: Vec<Queued> = works.into_iter().map(|work| queued(item.session, work, now)).collect();
        self.queue.splice(index..index, items);
    }

    fn channel(&self, session: SessionId, route: Route) -> Option<&str> {
        let open = self.open.iter().find(|open| open.session == session)?;
        open.channels.iter().find(|open| open.route == route).map(|open| open.channel.as_str())
    }

    fn read_file(&self) -> io::Result<Vec<OpenSession>> {
        let path = self.dirs.remote_access_push_channels_file().ok_or_else(no_home)?;
        let file: ChannelsFile = read_json(&path)?.unwrap_or_default();
        if file.version > FILE_VERSION {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("version {} is too new", file.version)));
        }
        Ok(file.sessions)
    }

    fn save(&self) {
        let saved = (|| {
            self.dirs.create_remote_access_dir()?;
            let path = self.dirs.remote_access_push_channels_file().ok_or_else(no_home)?;
            write_json(&path, &ChannelsFile { version: FILE_VERSION, sessions: self.open.clone() })
        })();
        if let Err(err) = saved {
            tracing::warn!("cannot save the push channels in use: {err}");
        }
    }
}
