//! 发推送的线程：`Rounds` 决定了要推的通知在这里按每台手机拆成一个个 HTTPS 请求排队发，失败的按
//! 情况退避重试、换环境或者放弃。一个请求在退避时别的照发。
//!
//! 出错时：
//! - 没拿到响应、5xx、429：隔 2、10、30 秒（之后都是 30 秒，429 带了 `Retry-After` 时至少等那么久）
//!   再试，最多试到排进队一分钟。
//! - device token 不认（410、400 `BadDeviceToken`）：可能是登记的环境不对，换另一个环境再发一次；成了
//!   把登记的环境改过来，还不认就删掉这条登记。
//! - 直连的密钥不对（403 `InvalidProviderToken`）：记错误，直连的都不发了，直到配置换了密钥。
//! - 别的错误：记下来，这条不发了。

use std::{
    io,
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use runode_paths::Dirs;
use runode_protocol::{SessionId, push::Notification, remote::DeviceId};

use super::{
    ApnsKey,
    apns::{self, DirectKey, Outcome, Route, Via},
    curl::Curl,
};
use crate::devices;

/// 一条通知最多试这么久，过了就不发了：等回答的事过时了。
const DEADLINE: Duration = Duration::from_secs(60);
/// 第几次失败后隔多久再试，之后都按最后一项。
const BACKOFF: [Duration; 3] = [Duration::from_secs(2), Duration::from_secs(10), Duration::from_secs(30)];

/// 推给一台手机：走哪条路线、哪台设备的哪个 device token，带着给这台手机拼好的通知。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) route: Route,
    pub(crate) device: DeviceId,
    pub(crate) token: String,
    pub(crate) notification: Notification,
}

/// 交给发送线程的事。
enum Command {
    /// 直连的密钥（`None` 是没配）和中转服务的基址。
    Settings(Option<ApnsKey>, Option<String>),
    Notify(Vec<Target>),
    /// 会话不再等了：排着还没发出去的这个会话的通知都不发了。
    Cancel(SessionId),
    /// 现在登记着的手机和 token：排着的通知里不是推给它们的都不发了。
    Keep(Vec<(DeviceId, String)>),
}

/// 发送线程的把手。丢掉时线程办完手上那个请求就结束，不等排着的。
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
            relay: runode_protocol::push::RELAY_URL.map(str::to_owned),
            queue: Vec::new(),
        };
        thread::Builder::new().name("remote-access-push".into()).spawn(move || worker.run(&rx))?;
        Ok(Self { tx })
    }

    pub(crate) fn settings(&self, direct: Option<ApnsKey>, relay: Option<String>) {
        let _ = self.tx.send(Command::Settings(direct, relay));
    }

    pub(crate) fn notify(&self, targets: Vec<Target>) {
        let _ = self.tx.send(Command::Notify(targets));
    }

    pub(crate) fn cancel(&self, session: SessionId) {
        let _ = self.tx.send(Command::Cancel(session));
    }

    pub(crate) fn keep(&self, registered: Vec<(DeviceId, String)>) {
        let _ = self.tx.send(Command::Keep(registered));
    }
}

/// 排着的一个请求。
struct Queued {
    target: Target,
    /// 是换了另一个环境再发的。
    other_env: bool,
    /// 这之前不发：在退避。
    not_before: Instant,
    /// 过了这个时刻还没办成就放弃。
    deadline: Instant,
    /// 失败了几次。
    failures: usize,
}

fn queued(target: Target, other_env: bool, now: Instant) -> Queued {
    Queued { target, other_env, not_before: now, deadline: now + DEADLINE, failures: 0 }
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
    relay: Option<String>,
    queue: Vec<Queued>,
}

impl Worker {
    fn run(&mut self, rx: &mpsc::Receiver<Command>) {
        loop {
            let now = Instant::now();
            let ready = self.queue.iter().position(|item| item.not_before <= now);
            let wake = self.queue.iter().map(|item| item.not_before).min();
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

    fn command(&mut self, command: Command) {
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
            Command::Notify(targets) => {
                // 同一台手机、同一个会话还在退避的旧通知不发了：它和新的用同一个 collapse-id，晚到会把
                // 新的问题替成旧的。
                self.queue.retain(|item| {
                    !targets.iter().any(|target| {
                        target.device == item.target.device
                            && target.notification.session == item.target.notification.session
                    })
                });
                let now = Instant::now();
                self.queue.extend(targets.into_iter().map(|target| queued(target, false, now)));
            }
            Command::Cancel(session) => self.queue.retain(|item| item.target.notification.session != session),
            Command::Keep(registered) => self.queue.retain(|item| {
                registered.iter().any(|(device, token)| *device == item.target.device && *token == item.target.token)
            }),
        }
    }

    /// 办排在 `index` 的请求。
    fn work(&mut self, index: usize) {
        let item = &self.queue[index];
        let (route, session) = (item.target.route, item.target.notification.session);
        // 排在前面的请求每个最多等十几秒，轮到时可能已经过了期限。
        if Instant::now() > item.deadline {
            tracing::warn!("giving up a push for session {session}: it waited too long to be sent");
            self.queue.remove(index);
            return;
        }
        let target = &item.target;
        if route.via == Via::Direct && (self.direct.is_none() || self.direct_broken) {
            tracing::debug!("not pushing for session {session}: no usable APNs key");
            self.queue.remove(index);
            return;
        }
        let outcome = match apns::request(
            route,
            &target.token,
            &target.notification,
            self.direct.as_mut(),
            self.relay.as_deref(),
            crate::now_unix(),
        ) {
            Ok(request) => apns::outcome(route.via, self.curl.send(&request)),
            Err(why) => Outcome::Failed(why),
        };
        match outcome {
            Outcome::Done => {
                let item = self.queue.remove(index);
                tracing::info!("push for session {session}: notified via {:?} ({:?})", route.via, route.env);
                if item.other_env {
                    let target = &item.target;
                    match devices::set_push_env(&self.dirs, target.device, &target.token, route.env) {
                        Ok(true) => {
                            tracing::info!("remote device {} is registered for {:?} push now", target.device, route.env)
                        }
                        Ok(false) => {}
                        Err(err) => tracing::warn!("cannot update a push registration: {err}"),
                    }
                }
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
                self.queue.remove(index);
            }
            Outcome::Failed(why) => {
                tracing::warn!("push for session {session} failed: {why}");
                self.queue.remove(index);
            }
        }
    }

    /// 没办成，按退避排到后面再试；过了期限就放弃。
    fn retry(&mut self, index: usize, after: Option<Duration>, why: &str) {
        let item = &mut self.queue[index];
        let backoff = BACKOFF[item.failures.min(BACKOFF.len() - 1)];
        item.failures += 1;
        let not_before = Instant::now() + after.map_or(backoff, |after| after.max(backoff));
        let session = item.target.notification.session;
        if not_before > item.deadline {
            tracing::warn!("giving up a push for session {session}: {why}");
            self.queue.remove(index);
            return;
        }
        tracing::debug!("push for session {session} will be retried: {why}");
        item.not_before = not_before;
    }

    /// device token 不认：第一次换另一个环境再发，换了还不认就删掉登记。
    fn bad_token(&mut self, index: usize, item: Queued, why: &str) {
        let target = item.target;
        if item.other_env {
            tracing::info!("APNs does not know the push token of remote device {} ({why}), dropping it", target.device);
            if let Err(err) = devices::drop_push_token(&self.dirs, target.device, &target.token) {
                tracing::warn!("cannot remove a push registration: {err}");
            }
            return;
        }
        let other = target.route.other_env();
        tracing::debug!("APNs refused a push token in {:?} ({why}), trying {:?}", target.route.env, other.env);
        let retry = queued(Target { route: other, ..target }, true, Instant::now());
        self.queue.insert(index, Queued { deadline: item.deadline, ..retry });
    }
}
