//! 管终端会话的宿主进程和各个前端（桌面、命令行、TUI、手机）之间的消息。
//!
//! 一条连接上是一串帧（`frame`）：PTY 的输出、键盘输入和快照原样放在帧里，控制消息
//! （`message`）编成 JSON。同一条连接上的帧按顺序到达，改 VT 状态的操作（改尺寸、换主题）
//! 由宿主在输出流里插一条控制消息标出位置，两边的 VT 在同一个位置做同样的事，才不会分叉。
//!
//! 这里只放数据和编解码，只依赖 `runode_shared_types` 和 serde；终端仿真和 PTY 都不碰。

pub mod frame;
pub mod message;

pub use frame::{Frame, FrameError, FrameKind, MAX_PAYLOAD, read_frame, write_frame};
pub use message::{
    AttachMode, BuildId, Caps, ClientKind, ClientMsg, FinishedCommand, GoodbyeReason, HostMsg, Placement, SessionId,
    SessionInfo,
};

/// 宿主给每个会话的 shell 设的环境变量：这个会话的 `SessionId`。在终端里跑的命令行据此知道
/// 自己在哪个会话里。
pub const ENV_SESSION: &str = "RUNODE_SESSION";

/// 宿主给 shell 设的环境变量：宿主监听的 socket 的路径。命令行先连它，没有时才按 runode 的
/// 目录约定去找。
pub const ENV_SOCKET: &str = "RUNODE_SOCKET";

/// 协议的版本。消息的含义或帧格式变了、旧的一方读不懂时加一；只是加了可以缺省的字段不用加。
pub const PROTOCOL_VERSION: u32 = 2;
