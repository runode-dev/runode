//! 控制消息：前端发给宿主的 `ClientMsg` 和宿主发给前端的 `HostMsg`，编成 JSON 放在
//! `FrameKind::Control` 帧里，带着 `type` 字段区分种类。
//!
//! 新旧两边之间什么改动读得懂：
//! - 对面多发了自己不认识的字段：忽略，照常读。
//! - 新加的字段带 `#[serde(default)]`：旧的一方发来的消息里没有它，按默认值读。
//! - 新加的字段是必填的（没有 `#[serde(default)]`）：旧的一方发来的这条消息解析失败。
//! - 新加的消息种类、新加的 `ClientKind`：旧的一方读成 `Unknown`，能回一句「不认识」而不是
//!   断开；其他枚举（`AttachMode`、`GoodbyeReason` 等）新加的取值读不了，整条消息解析失败。
//!
//! 所以加字段时带上 `#[serde(default)]`；改了已有消息的含义、新加必填字段或者新加枚举取值
//! （上面能读成 `Unknown` 的除外）时，加 `PROTOCOL_VERSION`。

use std::{fmt, path::PathBuf, str::FromStr};

use runode_shared_types::{grid::GridSize, session::SessionMeta, settings::TermSettings, shell::IntegrationMode};
use serde::{Deserialize, Serialize};

/// 一个终端会话的标识：128 位随机数，写成 32 个小写十六进制数字。宿主重启、交接后照旧，
/// 前端靠它找回原来的会话。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u128);

impl SessionId {
    /// 一个新的随机标识，从系统的随机数源取。
    #[cfg(unix)]
    pub fn random() -> std::io::Result<Self> {
        use std::io::Read as _;

        let mut bytes = [0u8; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(Self(u128::from_le_bytes(bytes)))
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// `SessionId` 的写法不对：不是正好 32 个十六进制数字。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidSessionId;

impl fmt::Display for InvalidSessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a session id is 32 hexadecimal digits")
    }
}

impl std::error::Error for InvalidSessionId {}

impl FromStr for SessionId {
    type Err = InvalidSessionId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(InvalidSessionId);
        }
        u128::from_str_radix(s, 16).map(Self).map_err(|_| InvalidSessionId)
    }
}

impl Serialize for SessionId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// 是哪一次构建，由版本号和构建时的标识拼成，宿主和前端各自报。快照格式还没有兼容保证，
/// 两边的构建一样才用快照，否则退回 VT 重放。
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BuildId(pub String);

/// 前端的种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Desktop,
    Cli,
    Tui,
    Mobile,
    /// 比自己新的一方才有的种类。旧宿主照样读得了 `Hello`，能按协议版本回 `Incompatible`。
    #[serde(other)]
    Unknown,
}

/// 前端能做什么。缺的项按不能读。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Caps {
    /// 能解宿主这个构建编的快照（`AttachMode::Snapshot`），即自己也跑着同一个 libghostty。
    pub snapshot: bool,
    /// 自己跑着一份 VT，能接 VT 重放（`AttachMode::VtReplay`）。
    pub vt_replay: bool,
}

/// 连上会话时要什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachMode {
    /// 先给一份快照（`FrameKind::Snapshot` 帧，以 `HostMsg::SnapshotEnd` 结束），再接着给 PTY
    /// 输出。界面上的 VT 解出快照后接着喂输出，和宿主那份一模一样。
    Snapshot,
    /// 构建不一样时的兜底：快照帧里放的是重画当前屏幕的 VT 序列，喂给一份新的 VT 后大致
    /// 复原，回滚历史、DECSC 存的光标等会丢。
    VtReplay,
    /// 不要屏幕内容，只要 `HostMsg::Meta` 这些状态，命令行列会话时用。
    MetaOnly,
}

/// 前端发给宿主的消息。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// 连上后的第一条消息。宿主回 `HostMsg::Welcome`，协议版本对不上时回 `Incompatible`。
    Hello {
        protocol: u32,
        build: BuildId,
        client: ClientKind,
        #[serde(default)]
        caps: Caps,
    },
    /// 要所有会话的列表，宿主回 `SessionList`。
    ListSessions,
    /// 新开一个会话。`req` 是前端自己编的号，宿主回 `Spawned` 时带回来。开好的会话不会自动
    /// 连上，前端接着发 `Attach`。
    Spawn { req: u32, size: GridSize, cwd: Option<PathBuf>, integration: IntegrationMode },
    /// 连上一个会话，宿主回 `Attached`。`size` 是前端视图的尺寸，宿主按它改会话的尺寸；
    /// 只看状态（`MetaOnly`）的不带。
    Attach { id: SessionId, size: Option<GridSize>, mode: AttachMode },
    /// 不再看这个会话；会话照旧跑着。
    Detach { id: SessionId },
    /// 前端视图的尺寸变了。宿主改好 PTY 和自己的 VT 后，在输出流里发 `Resized`，前端到那里
    /// 才改自己的 VT。
    Resize { id: SessionId, size: GridSize },
    /// 这个会话在前端是不是当前看着的，宿主据此决定发不发通知。
    Focus { id: SessionId, focused: bool },
    /// ⌘K 清屏。宿主在 VT 回到 ground 时往输出流里插清屏的序列，两份 VT 一起清。
    ClearScreen { id: SessionId },
    /// 结束会话：关掉它的 PTY，进程收到 SIGHUP。
    Kill { id: SessionId },
    /// 换主题：默认颜色、调色板和光标样式。宿主应用到自己的 VT 后，在每个会话的输出流里发
    /// `ThemeApplied`，前端到那里才应用，见 `HostMsg::ThemeApplied`。
    SetTheme { settings: TermSettings },
    /// 改宿主的选项。
    SetOptions { record_history: bool },
    /// 读会话屏幕上的文字，宿主回 `ScreenText`。`lines` 为 `None` 时是当前一屏，否则是从最后
    /// 一个有字的行往上这么多行，含回滚历史。
    ReadScreen { id: SessionId, lines: Option<u32> },
    /// 新版本的宿主要接手：旧宿主把 PTY 和监听的 socket 交过去后退出。
    Handoff,
    /// 让宿主退出。`kill_sessions` 为假时会话跟着宿主一起留到交接或者下次启动，见
    /// `GoodbyeReason`。
    Shutdown { kill_sessions: bool },
    /// 比自己新的一方才有的消息。宿主回 `HostMsg::Error`，连接照旧。
    #[serde(other)]
    Unknown,
}

/// 宿主发给前端的消息。带 `id` 的消息和这个会话的 `FrameKind::Output` 帧在同一条连接上按
/// 顺序到达，`Resized`、`ThemeApplied` 这类标记的位置就是 VT 要做这件事的位置。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMsg {
    /// 回 `Hello`。`snapshot_format` 是宿主编的快照的格式版本（libghostty 快照开头的版本号）。
    Welcome {
        protocol: u32,
        build: BuildId,
        host_pid: u32,
        snapshot_format: u16,
    },
    /// 协议版本对不上，宿主接着关掉连接。
    Incompatible {
        protocol: u32,
        build: BuildId,
        reason: String,
    },
    SessionList {
        sessions: Vec<SessionInfo>,
    },
    /// 回 `Spawn`。
    Spawned {
        req: u32,
        id: SessionId,
    },
    /// 回 `Attach`。`channel` 是这个会话的帧在这条连接上用的通道，见 `Frame::channel`；
    /// `mode` 是宿主实际给的，前端要快照、构建又不一样时退成 `VtReplay`。之后先是快照帧和
    /// `SnapshotEnd`（`MetaOnly` 时没有），再是输出。
    Attached {
        id: SessionId,
        channel: u32,
        size: GridSize,
        mode: AttachMode,
        meta: SessionMeta,
    },
    /// 快照或 VT 重放发完了，之后的 `Output` 帧接着它喂。
    SnapshotEnd {
        id: SessionId,
    },
    /// 宿主在这里改了 VT 的尺寸；前端的 VT 也在这里改，之后的输出是按新尺寸来的。
    Resized {
        id: SessionId,
        size: GridSize,
    },
    /// 宿主在这里应用了 `SetTheme` 的主题；前端的 VT 也在这里应用。应用主题会改 VT 的状态
    /// （比如重设光标闪烁），两边要在输出流的同一个位置做。
    ThemeApplied {
        id: SessionId,
    },
    /// 会话对外公布的状态变了：标题、agent、目录、shell 集成报告的东西。
    Meta {
        id: SessionId,
        meta: SessionMeta,
    },
    /// shell 集成报告一条命令运行完了，要记进历史时由前端记。
    CommandFinished {
        id: SessionId,
        command: FinishedCommand,
    },
    /// 宿主没法再保证前端的 VT 和自己的一样（比如前端读得太慢、输出被丢掉了），前端要重新
    /// `Attach`。
    Resync {
        id: SessionId,
        reason: String,
    },
    /// 会话里的 shell 退出了。`status` 是退出码，被信号结束等拿不到时为 `None`。
    Exited {
        id: SessionId,
        status: Option<i32>,
    },
    /// 回 `ReadScreen`：一行一个 `\n`，行尾空白去掉。
    ScreenText {
        id: SessionId,
        text: String,
    },
    /// 请求没法办，`req`、`id` 是对得上的那条请求的。
    Error {
        req: Option<u32>,
        id: Option<SessionId>,
        message: String,
    },
    /// 宿主要断开这条连接了。
    Goodbye {
        reason: GoodbyeReason,
    },
    /// 比自己新的宿主才有的消息，前端忽略它。
    #[serde(other)]
    Unknown,
}

/// `SessionList` 里的一个会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub size: GridSize,
    pub meta: SessionMeta,
    /// 现在连着它的前端有几个；为 0 的是没有窗口在看的后台会话。
    #[serde(default)]
    pub clients: u32,
    /// shell 已经退出，会话还留着给前端看最后的屏幕。
    #[serde(default)]
    pub exited: bool,
}

/// shell 集成报告运行完的一条命令。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishedCommand {
    pub cmd: String,
    /// 命令在哪个目录里运行。
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// 退出码；shell 没报告时为空。
    #[serde(default)]
    pub exit: Option<i32>,
    /// 开始运行的时刻，Unix 秒。
    pub ts: u64,
}

/// 宿主为什么断开。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoodbyeReason {
    /// 前端让它退出（`Shutdown`）。
    Shutdown,
    /// 交接给了新版本的宿主，前端重新连上就是新宿主。
    Handoff,
    /// 没有会话也没有前端，空闲太久自己退出。
    Idle,
    /// 宿主出了错。
    Error { message: String },
}
