//! 宿主升级时的交接：旧宿主把监听的 socket、锁和各个会话的 PTY 连同状态交给新版本的宿主，
//! shell 和里面跑着的程序不中断，socket 的路径不变。
//!
//! 新宿主（`Host::take_over`）以 `ClientKind::Successor` 连上旧宿主现有的 socket，发
//! `ClientMsg::Handoff`；旧宿主（`give`）检查能不能交、让会话停下来交出状态，回
//! `HostMsg::HandoffBegin` 后在同一条连接上发描述符消息（`runode_protocol::HandoffPart`）。
//!
//! 两阶段提交，提交点是旧宿主收到 `ClientMsg::HandoffReady`、发出 `HandoffPart::Commit`：
//! - 在那之前旧宿主的 PTY 原样留着（交出去的是复制的一份 master），新宿主接手的 PTY 停在闸门上
//!   （`Pty::adopt_paused`），一个字节都不读不写；任何一方出错、超时，新宿主放弃
//!   （`HandoffAbort` 或者断开），旧宿主回滚，接着读、重放冻结期间存下的请求，什么都不丢。
//! - 提交时旧宿主的会话交出 PTY（`Pty::release`，不结束 shell）和没写出去的输入，不再读写；
//!   新宿主写进没写出去的输入、打开闸门、开始接受连接，回 `HandoffDone`。旧宿主手里还留着交出去
//!   的那份 master 的复制，收到 `HandoffDone` 才关掉，随后退出（`Stopped::Handoff`），不删 socket
//!   文件。新宿主没回 `HandoffDone`（提交后死了，或者没收到 `Commit` 放弃了）时，只剩旧宿主握着
//!   PTY：它杀掉新宿主、等它不在了，用留着的 master 把会话接回来，接着服务（`give` 的 `reclaim`）。

mod give;
mod take;

use std::{
    fmt,
    os::{fd::AsRawFd as _, unix::net::UnixStream},
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) use give::{Asked, give};
use runode_protocol::{HandoffRefusal, SessionId};

use crate::Host;

/// 交出会话时默认给新宿主多久收下会话、回 `HandoffReady`（从开始发会话算起，发送本身也在内），
/// 见 `Host::set_handoff_deadline`。期限之和见 `GIVE_READY_WINDOW`。
pub(crate) const DEFAULT_DEADLINE: Duration = Duration::from_secs(20);

/// 旧宿主（默认期限下）从收到新宿主的 `ClientMsg::Handoff` 到不再接受 `HandoffReady`、回滚，最多
/// 这么久：停下各个会话 + 写完 `HandoffBegin` + `DEFAULT_DEADLINE`（发出会话、等回话）。新宿主
/// 这之后才回 `HandoffReady` 只会被回滚，拉起新宿主的一方等结果时要比它宽。
pub const GIVE_READY_WINDOW: Duration =
    give::PREPARE_TIMEOUT.checked_add(give::DETACH_TIMEOUT).unwrap().checked_add(DEFAULT_DEADLINE).unwrap();

/// 新宿主从准备回 `HandoffReady`（`TakeOverOptions::on_ready` 被调用）到 `Host::take_over` 返回，
/// 最多这么久：等旧宿主的 `Commit`、它没提交就断开时再等它退出完，以及失败时交回各个 PTY。
pub const TAKE_OVER_AFTER_READY: Duration =
    take::COMMIT_TIMEOUT.checked_add(take::EXIT_GRACE).unwrap().checked_add(take::RELEASE_TIMEOUT).unwrap();

/// `Host::take_over` 的选项。
#[derive(Clone, Default)]
pub struct TakeOverOptions {
    /// 测试用：把自己编的快照的格式版本当成这个。和旧宿主的不同时，会话的屏幕退成从 VT 重放
    /// 重建。`None` 时用这个构建真正的格式版本。
    pub snapshot_format: Option<u16>,
    /// 过了这个时刻就不再回 `HandoffReady`，放弃接手（旧宿主回滚）。拉起新宿主的一方等结果有
    /// 时限，过了时限它当作失败；这里保证它放弃以后这边不会再接手成功。`None` 时不限。
    pub ready_by: Option<Instant>,
    /// 回 `HandoffReady` 之前调一次：从这里起交接最多再花 `TAKE_OVER_AFTER_READY` 就有结果，
    /// 等结果的一方可以据此放宽时限。
    pub on_ready: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl fmt::Debug for TakeOverOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TakeOverOptions")
            .field("snapshot_format", &self.snapshot_format)
            .field("ready_by", &self.ready_by)
            .field("on_ready", &self.on_ready.is_some())
            .finish()
    }
}

/// 交接成功：接手了几个会话（含还没启动 shell 的），其中哪些的屏幕是从 VT 重放重建的（快照格式
/// 不同或者解不开，回滚历史没能带过来）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TakeOverReport {
    pub sessions: usize,
    pub replayed: Vec<SessionId>,
}

/// 交接没成。旧宿主照旧跑着、会话都在它手里，这边的 `Host` 也没留下任何状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TakeOverError {
    /// 旧宿主不交，见 `HandoffRefusal`。
    Refused(HandoffRefusal),
    /// 旧宿主不会交接：协议 3 及更早的版本（回 `Incompatible`），或者 `Welcome::handoff` 为 0。
    PreHandoff,
    /// 连不上、对面说的不对、接手某个会话失败、旧宿主没提交就走了等等，说明在里面。
    Failed(String),
}

impl fmt::Display for TakeOverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(reason) => write!(f, "the old host refused to hand over: {reason:?}"),
            Self::PreHandoff => write!(f, "the old host cannot hand its sessions over"),
            Self::Failed(reason) => write!(f, "the handoff failed: {reason}"),
        }
    }
}

impl std::error::Error for TakeOverError {}

impl Host {
    /// 宿主跑在 app 里，app 要退出、会话要留下：`yielding` 时把会话交给这个 app 拉起的单独一个
    /// 进程的宿主（`Host::take_over`），和升级时一样，只是不拦同一个构建的、不拦连着的界面、不拦
    /// 自己不是单独跑的，交接时也不给界面发 `Goodbye`（交接没成时 app 接着用这些会话）。交接没成时
    /// 调用方改回 false。没在监听（`Host::listen`）时交不了。
    pub fn yield_on_quit(&self, yielding: bool) {
        self.shared.peers().yielding = yielding;
    }
}

/// 连在 `stream` 另一头的进程号；对面已经断开时读不到。macOS 上（`LOCAL_PEERPID`）是最近用过
/// 对面那个 socket 的进程：连的一方读时，对方要已经 accept 过这条连接才准，之前读到的是最早建
/// 监听 socket 的进程，监听的 socket 交接过以后那就不是现在的宿主了。Linux 上（`SO_PEERCRED`）
/// 连的一方读到的总是调 `listen` 的进程，同样不跟着交接走。所以新宿主认旧宿主时以 `Welcome`
/// 里自报的为准，见 `take::old_host_pid`；接受连接的一方读到的是连上来的进程，一直准。
#[cfg(target_os = "macos")]
pub fn peer_pid(stream: &UnixStream) -> Option<libc::pid_t> {
    let mut pid: libc::pid_t = 0;
    let mut len = libc::socklen_t::try_from(size_of::<libc::pid_t>()).unwrap_or_default();
    // SAFETY: 描述符来自 `stream`；值指向本地变量，长度是它的大小。
    let result = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID, (&raw mut pid).cast(), &raw mut len)
    };
    (result == 0 && pid > 0).then_some(pid)
}

#[cfg(not(target_os = "macos"))]
pub fn peer_pid(stream: &UnixStream) -> Option<libc::pid_t> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = libc::socklen_t::try_from(size_of::<libc::ucred>()).unwrap_or_default();
    // SAFETY: 描述符来自 `stream`；值指向本地变量，长度是它的大小。
    let result = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &raw mut len)
    };
    (result == 0 && cred.pid > 0).then_some(cred.pid)
}

/// 杀掉连在 `stream` 另一头的进程（卡住的新宿主）：它手里那些停着的 PTY 随之关掉，不影响这边。
fn kill_peer(stream: &UnixStream) {
    // SAFETY: 没有参数，总是成功。
    let own = unsafe { libc::getpid() };
    match peer_pid(stream) {
        Some(pid) if pid > 1 && pid != own => {
            tracing::warn!("killing the stuck new host {pid}");
            // SAFETY: 只发信号。
            if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
                tracing::warn!("failed to kill the new host {pid}: {}", std::io::Error::last_os_error());
            }
        }
        _ => tracing::warn!("cannot tell which process the stuck new host is"),
    }
}

/// 进程 `pid` 还在跑：已经退出、等着被收尸的不算。
#[cfg(target_os = "macos")]
fn running(pid: libc::pid_t) -> bool {
    // SAFETY: `proc_bsdinfo` 是纯数据的 C 结构，全零是合法的初值。
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).unwrap_or(libc::c_int::MAX);
    // SAFETY: 输出参数指向本地变量，长度是它的大小。
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    written == size && info.pbi_status != libc::SZOMB
}

#[cfg(not(target_os = "macos"))]
fn running(pid: libc::pid_t) -> bool {
    // SAFETY: 信号 0 只检查进程在不在。
    unsafe { libc::kill(pid, 0) == 0 }
}

/// 日志里的毫秒数。
fn ms(duration: Duration) -> u128 {
    duration.as_millis()
}
