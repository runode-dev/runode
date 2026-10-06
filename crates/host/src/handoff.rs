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
//! - 提交时旧宿主交出 PTY（`Pty::release`，不结束 shell）和没写出去的输入，从此不再碰这些会话；
//!   新宿主写进没写出去的输入、打开闸门、开始接受连接，回 `HandoffDone`。旧宿主随后退出
//!   （`Stopped::Handoff`），不删 socket 文件。

mod give;
mod take;

use std::fmt;

use runode_protocol::{HandoffRefusal, SessionId};

/// `Host::take_over` 的选项。
#[derive(Clone, Debug, Default)]
pub struct TakeOverOptions {
    /// 测试用：把自己编的快照的格式版本当成这个。和旧宿主的不同时，会话的屏幕退成从 VT 重放
    /// 重建。`None` 时用这个构建真正的格式版本。
    pub snapshot_format: Option<u16>,
}

/// 交接成功：接手了几个会话（含还没启动 shell 的），其中哪些的屏幕是从 VT 重放重建的（快照格式
/// 不同或者解不开，回滚历史没能带过来）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TakeOverReport {
    pub sessions: usize,
    pub replayed: Vec<SessionId>,
}

/// 交接没成。旧宿主照旧跑着、会话都在它手里，这边的 `Host` 也没留下任何状态。
#[derive(Debug)]
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
