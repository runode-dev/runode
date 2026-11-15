//! 发推送的线程：`Rounds` 决定了要推的通知在这里按每台手机拆成一个个 HTTPS 请求排队发，失败的按
//! 情况退避重试、换环境或者放弃。一个请求在退避时别的照发。
//!
//! 出错时：
//! - 没拿到响应、5xx、429：按 `BACKOFF` 隔着再试（429 带了 `Retry-After` 时至少等那么久），最多试到
//!   排进队满 `DEADLINE`。
//! - device token 不认（410、400 `BadDeviceToken`）：直连时可能是登记的环境不对，换另一个环境再发一次，
//!   成了把登记的环境改过来，还不认就删掉这条登记；经中转时环境在句柄里，直接删掉。
//! - 直连的密钥不对（403 `InvalidProviderToken`）：记错误，直连的都不发了，直到配置换了密钥。
//! - 直连 FCM 时：token 作废了或不对，没有环境可换，直接删掉登记；access token 不认（401）重换了再试；
//!   Google 或 FCM 不认服务账号，记错误，直连 FCM 的先不发，过了 `FCM_RECHECK` 或配置换了服务账号时
//!   重读再试。
//! - 别的错误：记下来，这条不发了。

use std::{
    io,
    path::PathBuf,
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
    fcm::{self, FcmKey},
};
use crate::devices;

/// 一条通知最多试这么久，过了就不发了：等回答的事过时了。
const DEADLINE: Duration = Duration::from_secs(60);
/// 第几次失败后隔多久再试，之后都按最后一项。
const BACKOFF: [Duration; 3] = [Duration::from_secs(2), Duration::from_secs(10), Duration::from_secs(30)];
/// FCM 说服务账号不对之后，隔这么久重读文件、再试直连。
const FCM_RECHECK: Duration = Duration::from_secs(10 * 60);

/// 推给一台手机：走哪条路线、哪台设备的哪个 device token，带着给这台手机拼好的通知。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) route: Route,
    pub(crate) device: DeviceId,
    pub(crate) token: String,
    pub(crate) notification: Notification,
}

/// 发送线程要的配置：直连的凭据（`None` 是没配）和中转服务的基址。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Credentials {
    pub(crate) apns: Option<ApnsKey>,
    /// FCM 服务账号 JSON 文件。
    pub(crate) fcm: Option<PathBuf>,
    pub(crate) relay: Option<String>,
}

/// 交给发送线程的事。
enum Command {
    Settings(Credentials),
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
            // 换了 APNs 密钥 key id 就跟着变，等配置换了就够；FCM 的服务账号只认路径，在控制台补权限、
            // 在原路径换文件、校准时钟都不改配置，所以隔一阵自己重读再试。
            apns: Credential::new(
                APNS,
                |key: &ApnsKey| DirectKey::load(&key.key_file, &key.key_id, &key.team_id, &key.bundle),
                None,
            ),
            fcm: Credential::new(FCM, |file: &PathBuf| FcmKey::load(file), Some(FCM_RECHECK)),
            relay: runode_protocol::push::RELAY_URL.map(str::to_owned),
            queue: Vec::new(),
        };
        thread::Builder::new().name("remote-access-push".into()).spawn(move || worker.run(&rx))?;
        Ok(Self { tx })
    }

    pub(crate) fn settings(&self, credentials: Credentials) {
        let _ = self.tx.send(Command::Settings(credentials));
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

/// 日志里怎么称呼一种直连的凭据和它连的服务。
struct Kind {
    credential: &'static str,
    service: &'static str,
}

const APNS: Kind = Kind { credential: "the APNs key", service: "APNs" };
const FCM: Kind = Kind { credential: "the FCM service account", service: "FCM" };

/// 一种直连的凭据：配置里的 `C`，读好的 `K`。
struct Credential<C, K> {
    kind: Kind,
    read: fn(&C) -> Result<K, String>,
    /// 配置，变了才重读。
    config: Option<C>,
    /// 读好的凭据；没配或者读不了时为 `None`。
    key: Option<K>,
    /// 配了却没读成：有走这条路的通知要发时再读一次，文件补好了不用改配置也能恢复。
    load_failed: bool,
    /// 对面说凭据不对的时刻：之后走这条路的都不发，直到配置换了，或者过了 `recheck`。
    broken: Option<Instant>,
    /// 对面说凭据不对之后隔多久重读、再试；为 `None` 时一直等到配置换了。
    recheck: Option<Duration>,
}

impl<C: PartialEq, K> Credential<C, K> {
    fn new(kind: Kind, read: fn(&C) -> Result<K, String>, recheck: Option<Duration>) -> Self {
        Self { kind, read, config: None, key: None, load_failed: false, broken: None, recheck }
    }

    /// 配置换成 `config`；变了才重读，也忘掉对面说过凭据不对。
    fn set(&mut self, config: Option<C>) {
        if config != self.config {
            self.broken = None;
            self.config = config;
            self.load_failed = false;
            self.load();
        }
    }

    /// 上次没读成时再读一次。
    fn reload_if_failed(&mut self) {
        if self.load_failed {
            self.load();
        }
    }

    /// 照 `config` 读凭据。连着读不成时只有头一回记警告，免得每条通知都记一遍。
    fn load(&mut self) {
        let Some(config) = &self.config else {
            self.key = None;
            return;
        };
        let Kind { credential, service } = self.kind;
        match (self.read)(config) {
            Ok(key) => {
                if self.load_failed {
                    tracing::info!("read {credential}; pushing straight to {service} again");
                }
                self.key = Some(key);
                self.load_failed = false;
            }
            Err(err) => {
                if self.load_failed {
                    tracing::debug!("still cannot read {credential}: {err}");
                } else {
                    tracing::warn!("cannot push straight to {service}: {err}");
                }
                self.key = None;
                self.load_failed = true;
            }
        }
    }

    /// 能用的凭据：读好了，对面也没说过不对；说过不对、又过了 `recheck` 时重读一次再给。
    fn usable(&mut self) -> Option<&mut K> {
        if let Some(since) = self.broken {
            if !self.recheck.is_some_and(|recheck| since.elapsed() >= recheck) {
                return None;
            }
            self.broken = None;
            self.load();
        }
        self.key.as_mut()
    }

    /// 对面说凭据不对。
    fn refuse(&mut self) {
        self.broken = Some(Instant::now());
    }
}

struct Worker {
    dirs: Dirs,
    curl: Curl,
    apns: Credential<ApnsKey, DirectKey>,
    fcm: Credential<PathBuf, FcmKey>,
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
            Command::Settings(credentials) => {
                self.relay = credentials.relay;
                self.apns.set(credentials.apns);
                self.fcm.set(credentials.fcm);
            }
            Command::Notify(targets) => {
                // 轮询线程只在设置变了时才发 `Settings`，没读成的凭据只能在这里补读；一批读一次，
                // 只有要走那条路时才读，通知本来就稀，不另外限频。
                if targets.iter().any(|target| target.route.via == Via::Direct) {
                    self.apns.reload_if_failed();
                }
                if targets.iter().any(|target| target.route.via == Via::Fcm) {
                    self.fcm.reload_if_failed();
                }
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
        // 排在前面的请求要等响应，轮到时可能已经过了期限。
        if Instant::now() > item.deadline {
            tracing::warn!("giving up a push for session {session}: it waited too long to be sent");
            self.queue.remove(index);
            return;
        }
        let target = &item.target;
        let now = crate::now_unix();
        let outcome = if route.via == Via::Fcm {
            let Some(key) = self.fcm.usable() else {
                tracing::debug!("not pushing for session {session}: no usable FCM service account");
                self.queue.remove(index);
                return;
            };
            fcm::push(key, &self.curl, &target.token, &target.notification, now)
        } else {
            if route.via == Via::Direct && (self.apns.usable().is_none() || self.curl.check() != Ok(true)) {
                tracing::debug!("not pushing for session {session}: no usable APNs key, or curl has no HTTP/2");
                self.queue.remove(index);
                return;
            }
            match apns::request(
                route,
                &target.token,
                &target.notification,
                self.apns.key.as_mut(),
                self.relay.as_deref(),
                now,
            ) {
                Ok(request) => apns::outcome(route.via, self.curl.send(&request)),
                Err(why) => Outcome::Failed(why),
            }
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
                let why = if route.via == Via::Fcm {
                    if let Some(key) = &mut self.fcm.key {
                        key.forget_access_token();
                    }
                    "FCM refused the access token"
                } else {
                    if let Some(key) = &mut self.apns.key {
                        key.forget_jwt();
                    }
                    "the APNs token expired"
                };
                self.retry(index, Some(Duration::ZERO), why);
            }
            Outcome::BadToken(why) => {
                let item = self.queue.remove(index);
                self.bad_token(index, item, &why);
            }
            Outcome::BadKey(why) => {
                if route.via == Via::Fcm {
                    tracing::error!(
                        "FCM refused the configured service account ({why}); not pushing straight to FCM for a while"
                    );
                    self.fcm.refuse();
                } else {
                    tracing::error!(
                        "APNs refused the configured key ({why}); not pushing straight to APNs until it changes"
                    );
                    self.apns.refuse();
                }
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
        // `Retry-After` 是对面给的秒数，大得离谱时加上去会溢出，同样算过了期限。
        let wait = after.map_or(backoff, |after| after.max(backoff));
        let session = item.target.notification.session;
        let Some(not_before) = Instant::now().checked_add(wait).filter(|at| *at <= item.deadline) else {
            tracing::warn!("giving up a push for session {session}: {why}");
            self.queue.remove(index);
            return;
        };
        tracing::debug!("push for session {session} will be retried: {why}");
        item.not_before = not_before;
    }

    /// token 不认：直连 APNs 时第一次换另一个环境再发，换了还不认、经中转（环境在句柄里）或直连 FCM
    /// （不分环境）时就删掉登记。
    fn bad_token(&mut self, index: usize, item: Queued, why: &str) {
        let target = item.target;
        if item.other_env || target.route.via == Via::Relay || target.route.via == Via::Fcm {
            tracing::info!(
                "the push service does not know the push token of remote device {} ({why}), dropping it",
                target.device
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    const KIND: Kind = Kind { credential: "the test key", service: "the test service" };

    /// 对面说过凭据不对：没有 `recheck` 时一直不给，有时过了就重读、照常给。
    #[test]
    fn refused_credentials_come_back_only_after_the_recheck() {
        let mut forever = Credential::new(KIND, |config: &u8| Ok(*config), None);
        forever.set(Some(1));
        forever.refuse();
        assert!(forever.usable().is_none());

        let mut timed = Credential::new(KIND, |config: &u8| Ok(*config), Some(Duration::from_millis(50)));
        timed.set(Some(1));
        timed.refuse();
        assert!(timed.usable().is_none());
        thread::sleep(Duration::from_millis(60));
        assert_eq!(timed.usable().copied(), Some(1));
        assert!(timed.broken.is_none());

        // 配置换了，不管过没过 `recheck` 都重读。
        forever.set(Some(2));
        assert_eq!(forever.usable().copied(), Some(2));
    }
}
