//! 一个终端、一个工作区在哪台机器上（`HostId`），和远端主机的连接状态（`Status`）。远端有两种：经 SSH 连的
//! 主机（`ssh`）和配对过的、开着远程访问的别的电脑（`paired`）。两种各管各的连接，状态都用 `Status` 报、经
//! `events` 告诉界面，这台电脑睡醒或网络变了时都马上再试（`watch_network`）；这里按 `HostId` 分流，调用方不管
//! 远端是哪种。

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        LazyLock, Mutex, Once, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

pub mod forward;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use runode_protocol::{ClientMsg, SessionId};

use super::{Link, Timeouts, build, paired, ssh};

/// 一个终端、一个工作区在哪台机器上。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HostId {
    /// 这台机器（`host_client::link`）。
    Local,
    /// 经 SSH 连的主机，按用户填的连接目标（`Destination` 的写法）认。
    Ssh(String),
    /// 配对过的电脑，按 `PairedMachine::id` 认。
    Paired(String),
}

/// 配对的电脑在 `HostId::key` 里写成这个前缀加 `PairedMachine::id`。`Destination::parse` 认不出端口不是数字的
/// 写法，这样写的不会是哪台 SSH 主机的连接目标。
const PAIRED_KEY_PREFIX: &str = "runode://paired/";

impl HostId {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    /// 存档、显示名表和系统表里认这台主机的写法：SSH 主机是连接目标原样，配对的电脑是 `PAIRED_KEY_PREFIX`
    /// 加它的标识；本机为空。
    pub fn key(&self) -> Option<String> {
        match self {
            Self::Local => None,
            Self::Ssh(name) => Some(name.clone()),
            Self::Paired(id) => Some(format!("{PAIRED_KEY_PREFIX}{id}")),
        }
    }

    /// 按 `key` 的写法认回来：不是配对的电脑的写法都是 SSH 主机的连接目标。
    pub fn from_key(key: &str) -> Self {
        match key.strip_prefix(PAIRED_KEY_PREFIX) {
            Some(id) => Self::Paired(id.to_owned()),
            None => Self::Ssh(key.to_owned()),
        }
    }

    /// 存档里记的主机（`key` 的写法），为空的是本机。
    pub fn from_saved(key: Option<&str>) -> Self {
        key.map_or(Self::Local, Self::from_key)
    }

    /// 这台机器上的家目录；远端的是连上时那边报的，没连着时为空。
    pub fn home(&self) -> Option<PathBuf> {
        match self {
            Self::Local => runode_paths::Dirs::from_env().home,
            remote => match status_of(remote) {
                Status::Connected(connected) => connected.home,
                _ => None,
            },
        }
    }

    /// 给人看的这台机器上的路径，家目录写成 `~`；远端的路径用 `/` 分隔（`Connected::home`）。
    pub fn display_dir(&self, dir: &Path) -> String {
        match self {
            Self::Local => crate::ui::display_dir(dir),
            _ => self
                .home()
                .and_then(|home| runode_paths::home_relative(dir, &home, '/'))
                .unwrap_or_else(|| dir.display().to_string()),
        }
    }
}

/// 到 `host` 的那条连接；远端的头一回用时开始连。认不出主机时是一条从不连上的连接，请求都失败：不能退回本机，
/// 不然远端工作区的布局、开的终端落到本机宿主上。
pub fn link_of(host: &HostId) -> Link {
    static NOWHERE: LazyLock<Link> = LazyLock::new(|| Link::with_timeouts(build(), Timeouts::REMOTE));
    match host {
        HostId::Local => super::link().clone(),
        HostId::Ssh(name) => ssh::remote(name).map_or_else(|| NOWHERE.clone(), |remote| remote.link.clone()),
        HostId::Paired(id) => paired::remote(id).map_or_else(|| NOWHERE.clone(), |remote| remote.link.clone()),
    }
}

/// 远端主机 `host` 现在的连接状态；还没有这台主机时是没在连，也不新建它。本机不经这里，算没在连。
pub fn status_of(host: &HostId) -> Status {
    match host {
        HostId::Local => Status::Idle,
        HostId::Ssh(name) => ssh::find(name).map_or(Status::Idle, |host| host.status()),
        HostId::Paired(id) => paired::find(id).map_or(Status::Idle, |host| host.status()),
    }
}

/// 连着时最近一次心跳到远端宿主来回花的时间；没连着、还没测过时为空。
pub fn latency(host: &HostId) -> Option<Duration> {
    match host {
        HostId::Local => None,
        HostId::Ssh(name) => ssh::find(name)?.latency(),
        HostId::Paired(id) => paired::find(id)?.latency(),
    }
}

/// 远端主机 `host` 不等退避马上再连，停着的（要用户来办的）也再试一次；还没有这台主机时新建、开始连。
pub fn retry_now(host: &HostId) {
    match host {
        HostId::Local => {}
        HostId::Ssh(name) => {
            if let Some(remote) = ssh::remote(name) {
                remote.retry_now();
            }
        }
        HostId::Paired(id) => {
            if let Some(remote) = paired::remote(id) {
                remote.retry_now();
            }
        }
    }
}

/// 要用远端主机 `host`：还没有时新建、开始连，停着、等着重连、没在连的马上再试；连着、正在连的不动。交回认不认得
/// 这台主机：本机、认不出写法的 SSH 主机、没配对过的电脑都不认得。
pub fn ensure_connecting(host: &HostId) -> bool {
    let idle = |status: Status| matches!(status, Status::Stopped { .. } | Status::Waiting { .. } | Status::Idle);
    match host {
        HostId::Local => false,
        HostId::Ssh(name) => {
            let Some(remote) = ssh::remote(name) else { return false };
            if idle(remote.status()) {
                remote.retry_now();
            }
            true
        }
        HostId::Paired(id) => {
            let Some(remote) = paired::remote(id) else { return false };
            if idle(remote.status()) {
                remote.retry_now();
            }
            true
        }
    }
}

/// 断开远端主机 `host`，不再自己重连，直到 `retry_now`；那边的会话照常跑着。还没有这台主机时什么都不做。
/// 会阻塞。
pub fn disconnect(host: &HostId) {
    match host {
        HostId::Local => {}
        HostId::Ssh(name) => {
            if let Some(remote) = ssh::find(name) {
                remote.disconnect();
            }
        }
        HostId::Paired(id) => {
            if let Some(remote) = paired::find(id) {
                remote.disconnect();
            }
        }
    }
}

/// 忘掉远端主机 `host`：断开，不再列在 `remotes` 里；之后再用它时从头连。配对的电脑的配对记录留着，要解除配对
/// 用 `paired::unpair`。会阻塞。
pub fn forget(host: &HostId) {
    match host {
        HostId::Local => {}
        HostId::Ssh(name) => ssh::forget(name),
        HostId::Paired(id) => paired::forget(id),
    }
}

/// 这次运行里连过（或正连着）的远端主机：先是 SSH 主机，再是配对的电脑，各自按名字、标识排好。
pub fn remotes() -> Vec<HostId> {
    let ssh = ssh::remotes().into_iter().map(|host| HostId::Ssh(host.name.clone()));
    ssh.chain(paired::remotes().into_iter().map(|host| HostId::Paired(host.id.clone()))).collect()
}

/// 远端主机 `host` 是 Windows（路径和命令按那边的规矩写）；还没认出来时、本机都不算。
pub fn is_windows(host: &HostId) -> bool {
    match host {
        HostId::Local => false,
        HostId::Ssh(name) => {
            ssh::find(name).is_some_and(|host| host.family() == runode_ssh::bootstrap::Family::Windows)
        }
        HostId::Paired(id) => paired::find(id).is_some_and(|host| host.is_windows()),
    }
}

/// `hold` 时之后新建的远端主机先不连，停在 `Status::Idle`，等用户 `retry_now`：启动时恢复窗口期间按配置的
/// `ssh-restore-on-launch` 设。
pub fn hold_new_hosts(hold: bool) {
    HOLD_NEW.store(hold, Ordering::Relaxed);
}

/// 新建的远端主机要不要先不连（`hold_new_hosts`）。
pub(super) fn holding_new_hosts() -> bool {
    HOLD_NEW.load(Ordering::Relaxed)
}

/// 把配置里宿主关心的部分（`messages`）告诉各台远端主机的宿主，之后新连的主机也照它。
pub fn configure(messages: Vec<ClientMsg>) {
    for host in remotes() {
        let link = link_of(&host);
        for message in &messages {
            link.send(message.clone());
        }
    }
    *HOST_OPTIONS.lock().unwrap_or_else(PoisonError::into_inner) = messages;
}

/// 配置里宿主关心的部分，新建远端主机的 `Link` 时照发：`Link` 记着最近一次的，连上时补发。
pub(super) fn send_host_options(link: &Link) {
    for message in HOST_OPTIONS.lock().unwrap_or_else(PoisonError::into_inner).iter() {
        link.send(message.clone());
    }
}

/// 远端主机上发生的、界面要知道的事。只能取一次。
pub fn events() -> Option<UnboundedReceiver<RemoteEvent>> {
    EVENTS.1.lock().unwrap_or_else(PoisonError::into_inner).take()
}

pub(super) fn emit(event: RemoteEvent) {
    let _ = EVENTS.0.unbounded_send(event);
}

/// 界面要知道的事。
#[derive(Debug)]
pub enum RemoteEvent {
    /// 这台主机的连接状态变了。
    Status(HostId),
    /// 连上了这台主机（第一次或断开后又连上），`alive` 是那时它的宿主里还活着的会话。
    Connected { host: HostId, alive: HashSet<SessionId> },
    /// ssh 要问用户。
    Ask(ssh::Ask),
    /// 这台主机被忘掉了（`forget`）。
    Forgotten(HostId),
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
    /// 不再自己重连，等 `retry_now`：要用户来办才能再连（认证没过、主机密钥变了、平台不支持、装不上，配对被
    /// 撤销），或者重连的退避走完了还连不上。
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
    /// 远端宿主不是这个构建（升级时那边还连着装旧版本的桌面，或者配对的电脑装的是别的版本）。
    pub mismatch: bool,
    /// 远端宿主跟着 ssh 的登录会话走，最后一个连接断开时可能被系统结束（`Opening::session_bound`）。
    pub session_bound: bool,
    /// 远端的家目录，不知道时为空。Windows 远端的路径也用 `/` 分隔（`C:/Users/me`）。
    pub home: Option<PathBuf>,
    /// 远端的系统，`runode_ssh::bridge::Opening::os` 的写法；不知道时为空。
    pub os: String,
}

/// 连接做到了哪一步。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// 连 ssh、认证；配对的电脑是找到它、过门禁。
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

/// 第几次重连前等多久；走完了还连不上就停下。
pub(super) const RETRY_DELAYS: [Duration; 6] = [
    Duration::ZERO,
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];
/// 多久看一次睡没睡过、本机地址变没变（`watch_network`）。
const NETWORK_POLL: Duration = Duration::from_secs(5);
/// 两次看之间多走了这么久以上，算这台电脑睡过一觉（`watch_network`）。
const SLEPT_AFTER: Duration = Duration::from_secs(30);
/// 连上撑过这么久算稳了，下次断开从头退避。
pub(super) const STABLE_AFTER: Duration = Duration::from_secs(60);
/// 连着时每隔这么久问一次宿主还在不在。
pub(super) const HEARTBEAT: Duration = Duration::from_secs(15);
/// 心跳最多等这么久回话，等不到按断开处理。
pub(super) const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
/// 重连上以后列会话最多等这么久。
pub(super) const LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// 配置里宿主关心的部分（`SetTheme`、`SetOptions`），连上新主机时照发。
static HOST_OPTIONS: Mutex<Vec<ClientMsg>> = Mutex::new(Vec::new());
/// 为真时新建的主机先不连，等 `retry_now`。
static HOLD_NEW: AtomicBool = AtomicBool::new(false);
#[allow(clippy::type_complexity)]
static EVENTS: LazyLock<(UnboundedSender<RemoteEvent>, Mutex<Option<UnboundedReceiver<RemoteEvent>>>)> =
    LazyLock::new(|| {
        let (tx, rx) = unbounded();
        (tx, Mutex::new(Some(rx)))
    });

/// 开始看睡没睡过、网络变没变（`watch_network`），整个进程只起一次：头一台远端主机建出来时调。
pub(super) fn watch_network_once() {
    static WATCH_NETWORK: Once = Once::new();
    WATCH_NETWORK.call_once(|| {
        if let Err(err) = thread::Builder::new().name("remote-network".into()).spawn(watch_network) {
            tracing::error!("cannot start watching the network: {err}");
        }
    });
}

/// 每隔 `NETWORK_POLL` 看一眼：这台电脑睡过一觉，或本机的地址变了（换了网络、连上 VPN），就叫正在重连和重连
/// 放弃了的主机马上再试（`RemoteHost::nudge`）。
///
/// 睡没睡过按墙上时钟比 `Instant` 多走了 `SLEPT_AFTER` 以上认：macOS 和 Linux 的 `Instant` 不算睡着的时间，
/// App Nap 推迟定时器时两边一起慢，不算睡过。Windows 的 `Instant` 算睡着的时间，那里按比 `NETWORK_POLL` 多等了
/// `SLEPT_AFTER` 以上认。
fn watch_network() {
    let addresses = || {
        let mut addresses = runode_remote::local_addresses();
        addresses.sort_unstable();
        addresses
    };
    let mut seen = addresses();
    let mut last = (SystemTime::now(), Instant::now());
    loop {
        thread::sleep(NETWORK_POLL);
        let now = (SystemTime::now(), Instant::now());
        let waited = now.1 - last.1;
        let slept = now.0.duration_since(last.0).is_ok_and(|wall| wall > waited + SLEPT_AFTER)
            || cfg!(windows) && waited > NETWORK_POLL + SLEPT_AFTER;
        last = now;
        let current = addresses();
        let changed = current != seen;
        seen = current;
        if !slept && !changed {
            continue;
        }
        tracing::info!("woke up or the network changed (slept {slept}, addresses changed {changed}): reconnecting");
        ssh::nudge_all();
        paired::nudge_all();
    }
}

#[cfg(test)]
mod tests {
    use super::HostId;

    /// 配对的电脑和 SSH 主机在存档里的写法认得回来；旧存档里的 SSH 主机照旧是 SSH 主机。
    #[test]
    fn host_keys_round_trip() {
        let paired = HostId::Paired("0123456789abcdef0123456789abcdef".into());
        let key = paired.key().unwrap();
        assert_eq!(HostId::from_key(&key), paired);
        for name in ["vps", "me@host", "me@host:2222", "ssh://me@host:22", "[::1]:22", "runode"] {
            let ssh = HostId::Ssh(name.into());
            assert_eq!(ssh.key().as_deref(), Some(name));
            assert_eq!(HostId::from_key(name), ssh);
        }
        assert_eq!(HostId::Local.key(), None);
        assert_eq!(HostId::from_saved(None), HostId::Local);
        assert_eq!(HostId::from_saved(Some("vps")), HostId::Ssh("vps".into()));
    }

    /// 配对的电脑的写法不是哪台 SSH 主机的连接目标，不会和用户填的主机撞上。
    #[test]
    fn paired_keys_are_not_ssh_destinations() {
        let key = HostId::Paired("0123456789abcdef0123456789abcdef".into()).key().unwrap();
        assert!(runode_ssh::Destination::parse(&key).is_none(), "{key} parses as an ssh destination");
        assert!(runode_ssh::Destination::parse(&format!("{key}/")).is_none());
    }
}
