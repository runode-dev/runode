//! 控制消息：前端发给宿主的 `ClientMsg` 和宿主发给前端的 `HostMsg`，编成 JSON 放在
//! `FrameKind::Control` 帧里，带着 `type` 字段区分种类。
//!
//! 新旧两边之间什么改动读得懂：
//! - 对面多发了自己不认识的字段：忽略，照常读。
//! - 新加的字段带 `#[serde(default)]`：旧的一方发来的消息里没有它，按默认值读。
//! - 新加的字段是必填的（没有 `#[serde(default)]`）：旧的一方发来的这条消息解析失败。
//! - 新加的消息种类、新加的 `ClientKind`、`GoodbyeReason`、`HandoffRefusal`：旧的一方读成
//!   `Unknown`，能回一句「不认识」或者照常断开，而不是整条读不了；其他枚举（`AttachMode` 等）
//!   新加的取值读不了，整条消息解析失败。
//!
//! 所以加字段时带上 `#[serde(default)]`；改了已有消息的含义、新加必填字段或者新加枚举取值
//! （上面能读成 `Unknown` 的除外）时，加 `PROTOCOL_VERSION`。
//!
//! 升级时新版本的宿主要从旧宿主手里接过会话（见 `ClientKind::Successor`、`ClientMsg::Handoff`），
//! 而旧宿主可能是任何一个已经发布过的版本，所以下面这些永远冻结，加 `PROTOCOL_VERSION` 也不能改：
//! - 帧头的格式（见 `frame`），以及交接时传描述符的消息头（`runode_terminal` 的 `fd_passing`：
//!   小端 u32 字节数加小端 u32 描述符个数）。
//! - `ClientMsg::Hello`、`HostMsg::Welcome`、`HostMsg::Incompatible`，以及交接用的
//!   `ClientMsg::Handoff`、`HandoffReady`、`HandoffAbort`、`HandoffDone`、
//!   `HostMsg::HandoffRefused`、`HandoffBegin`、`Goodbye` 和它们用到的 `HandoffRefusal`、
//!   `GoodbyeReason`：已有字段的名字、类型和含义都不改，以后只能加带 `#[serde(default)]` 的字段；
//!   `HandoffRefusal`、`GoodbyeReason` 可以加新的取值，旧的一方读成 `Unknown`。交接时旧宿主要
//!   给不同版本的前端发 `Goodbye { Handoff }`，前端也要和不同版本的宿主谈握手。
//! - `Hello` 里 `client` 是 `ClientKind::Successor` 的连接，宿主不比对协议版本，照样回
//!   `Welcome`；之后只认交接用的消息。新宿主据此能和协议版本不同的旧宿主谈交接。
//! - 交接时一条描述符消息里数据的编法和 `HandoffPart` 的格式，见 `handoff`；格式有自己的版本号
//!   `HANDOFF_FORMAT`。
//!
//! `handoff_compat` 测试里存着格式 1 的样例，永远要读得了。

use std::{fmt, path::PathBuf, str::FromStr};

use runode_shared_types::{
    clipboard::ClipboardAccess, grid::GridSize, session::SessionMeta, settings::TermSettings, shell::IntegrationMode,
};
use serde::{Deserialize, Serialize};

use crate::layout::WindowLayout;

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
    /// 要接手会话的新版本宿主（`runode --host --take-over`）。宿主对它不比对协议版本，照样回
    /// `Welcome`，见本模块文档里冻结的规则；这条连接接着发 `ClientMsg::Handoff`。
    Successor,
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
    /// 连上后的第一条消息。宿主回 `HostMsg::Welcome`，协议版本对不上时回 `Incompatible`（`client`
    /// 是 `ClientKind::Successor` 时不比对，见模块文档）。
    /// `session` 是发消息的程序自己所在的会话（在 runode 的终端里跑的命令行带上它），宿主据此
    /// 记下是谁在操作别的会话，见 `SessionMeta::driver`。
    Hello {
        protocol: u32,
        build: BuildId,
        client: ClientKind,
        #[serde(default)]
        caps: Caps,
        #[serde(default)]
        session: Option<SessionId>,
        /// 前端所在设备的名字（桌面填机器名），给别的前端看「尺寸由谁控制」，见
        /// `HostMsg::SizeOwner`。没有时宿主不替它编名字。
        #[serde(default)]
        device: Option<String>,
    },
    /// 要所有会话的列表，宿主回 `SessionList`。
    ListSessions,
    /// 新开一个会话。`req` 是前端自己编的号，宿主回 `Spawned` 时带回来。开好的会话不会自动
    /// 连上，前端接着发 `Attach`。
    Spawn {
        req: u32,
        size: GridSize,
        cwd: Option<PathBuf>,
        integration: IntegrationMode,
        /// 现在就启动 shell；为假时只开好伪终端，等 `Start`。
        #[serde(default = "yes")]
        start: bool,
        /// 要启动的程序，为空时用用户的 `$SHELL`。
        #[serde(default)]
        shell: Option<String>,
        /// 宿主还没收到过 `SetTheme` 时这个会话的 VT 一开始套的主题，比如配置还没加载完就
        /// 提前开的会话用前端自己读到的配置；收到过时一律用宿主当前的主题。
        #[serde(default)]
        settings: Option<TermSettings>,
        /// 启动 shell 时另外设的环境变量，同名的盖过宿主自己设的。
        #[serde(default)]
        env: Vec<(String, String)>,
    },
    /// 启动 `Spawn` 时 `start` 为假的会话的 shell；已经启动过时什么都不做。启动不了时宿主在
    /// 这个会话的输出流里发 `HostMsg::Exited`。
    Start { id: SessionId, integration: IntegrationMode },
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
    /// 换主题：默认颜色、调色板、光标样式和回滚上限，之后新开的会话也用它。宿主应用到自己的
    /// VT 后，在每个会话的输出流里发带着这份设置的 `ThemeApplied`，连着的前端到那里才应用，
    /// 不管是不是自己发的，见 `HostMsg::ThemeApplied`。
    SetTheme { settings: TermSettings },
    /// 改宿主的选项：记不记命令历史，以及终端里的程序读写剪贴板（OSC 52）的规矩。旧的界面不带
    /// `clipboard`，按默认值（照写、读前先问）。
    SetOptions {
        record_history: bool,
        #[serde(default)]
        clipboard: ClipboardAccess,
    },
    /// 读会话屏幕上的文字，宿主回 `ScreenText`。`lines` 为 `None` 时是当前一屏，否则是从最后
    /// 一个有字的行往上这么多行，含回滚历史。`command` 为 `Some(n)` 时不看 `lines`，读倒数第
    /// n 条命令（1 是最近一条）的输出，要 shell 集成标出的提示符。
    ReadScreen {
        id: SessionId,
        lines: Option<u32>,
        #[serde(default)]
        command: Option<u32>,
    },
    /// 给会话里的程序发控制键，按宿主那份 VT 当前的模式（应用光标键、Kitty 键盘协议等）编码后
    /// 写进去，回 `Done`。`keys` 每项是一个键的写法：`ctrl-c`、`up`、`f5`、`down*3` 这类。
    SendKeys { req: u32, id: SessionId, keys: Vec<String> },
    /// 往会话里粘贴一段文字，程序开着括号粘贴模式（mode 2004）时套上括号，回 `Done`。
    Paste { req: u32, id: SessionId, text: String },
    /// 要 app 里各个终端摆在哪，宿主转给界面，回 `HostMsg::Layout`。
    Layout { req: u32 },
    /// 界面办完了宿主转来的 `HostMsg::UiRequest`：`ui` 是那条请求的编号，`reply` 原样转给发请求
    /// 的一方。只有桌面的界面发。
    UiReply { ui: u64, reply: Box<HostMsg> },
    /// 在 app 里开一个新终端，宿主转给 app 的界面，回 `Opened`。`near` 是放在哪个会话的分屏
    /// 旁边，为空时放在最前面那个窗口当前的分屏旁边；`cwd` 为空时沿用旁边那个终端的目录；
    /// `focus` 为假时不切过去，不打断用户手上的事。
    Open {
        req: u32,
        placement: Placement,
        #[serde(default)]
        near: Option<SessionId>,
        #[serde(default)]
        cwd: Option<PathBuf>,
        #[serde(default)]
        focus: bool,
    },
    /// 在 app 里切到显示这个会话的分屏，激活它的窗口，回 `Done`。
    Reveal { req: u32, id: SessionId },
    /// 会话 `id` 里的程序用 OSC 52 写剪贴板，`text` 是解码好的文字（不超过
    /// `clipboard::MAX_CLIPBOARD_BYTES`）。宿主自己发起的请求，只出现在 `HostMsg::UiRequest` 里：
    /// 界面写好后回 `Done`，`req` 为 0（没有发请求的一方，也就没有它的编号）。前端直接发给宿主的
    /// 回 `Error`。旧的界面读成 `Unknown`，回 `Error`，宿主记一笔日志。
    WriteClipboard { id: SessionId, text: ClipboardContent },
    /// 会话 `id` 里的程序用 OSC 52 读剪贴板。和 `WriteClipboard` 一样只出现在 `HostMsg::UiRequest`
    /// 里。`ask` 为真时界面先弹框问用户（配置项 `clipboard-read` 是 `ask`），`program` 是那时会话
    /// 前台的程序名，问的时候给用户看。界面回 `HostMsg::ClipboardText`；用户不让读、剪贴板里没有
    /// 文字或者文字太长时 `text` 为空，宿主回给程序一个空的剪贴板。
    ReadClipboard {
        id: SessionId,
        ask: bool,
        #[serde(default)]
        program: Option<String>,
    },
    /// 新版本的宿主（`ClientKind::Successor`）要接手：旧宿主把 PTY 和监听的 socket 交过去后退出。
    /// `min_format`..=`max_format` 是新宿主读得了的交接格式（`handoff::HANDOFF_FORMAT`），旧宿主
    /// 只写自己的那一个，不在范围里时回 `HandoffRefused { UnsupportedFormat }`，否则回
    /// `HandoffBegin`，接着在同一条连接上发描述符消息（见 `handoff::HandoffPart`）。
    /// 早先的写法 `{"type":"handoff"}` 没有范围，读成 0..=0，哪个格式都不在里面。
    Handoff {
        #[serde(default)]
        min_format: u32,
        #[serde(default)]
        max_format: u32,
    },
    /// 新宿主收下了所有会话、准备好接手，等旧宿主发 `HandoffPart::Commit`。这是交接的提交点：
    /// 之前旧宿主随时能回滚，之后会话归新宿主。
    HandoffReady,
    /// 新宿主在 `HandoffReady` 之前出了错，放弃接手，旧宿主回滚、照常跑下去。
    HandoffAbort {
        #[serde(default)]
        reason: String,
    },
    /// 新宿主收到 `Commit` 并接管了监听的 socket，旧宿主可以退出了。
    HandoffDone,
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
    /// 回 `Hello`。`snapshot_format` 是宿主编的快照的格式版本（libghostty 快照开头的版本号），
    /// 前端解不了这个格式时改要 `VtReplay`。`standalone` 是宿主单独一个进程在跑（`runode --host`）；
    /// 为假时宿主跑在某个 app 的进程里，那个 app 才是它的界面，别的 app 不该接手它的会话、也不该
    /// 让它退出。`handoff` 是宿主交出会话时写的交接格式（`handoff::HANDOFF_FORMAT`），0 是这个
    /// 宿主不会交接。
    Welcome {
        protocol: u32,
        build: BuildId,
        host_pid: u32,
        snapshot_format: u16,
        #[serde(default)]
        standalone: bool,
        #[serde(default)]
        handoff: u32,
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
    /// `SnapshotEnd`（`MetaOnly` 时没有），再是输出。`settings` 是宿主那份 VT 现在套着的主题，
    /// `VtReplay` 时前端按它和 `size` 新建自己的 VT 再喂重放（快照里本来就带着）。
    Attached {
        id: SessionId,
        channel: u32,
        size: GridSize,
        mode: AttachMode,
        meta: SessionMeta,
        #[serde(default)]
        settings: Option<TermSettings>,
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
    /// 宿主在这里应用了 `settings` 这份主题；前端的 VT 也在这里应用同一份，不用自己当前的
    /// 配置。应用主题会改 VT 的状态（比如重设光标闪烁、按回滚上限丢掉历史），两边要在输出流的
    /// 同一个位置、用同样的设置做：前端自己的配置可能已经又变了，`SetTheme` 也可能是别的前端发的。
    ThemeApplied {
        id: SessionId,
        settings: TermSettings,
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
    /// 程序响了铃（BEL）。紧跟在含这个 BEL 的那块 `Output` 之后；只看状态（`MetaOnly`）的前端
    /// 也收得到，后台标签据此标出响过铃。
    Bell {
        id: SessionId,
    },
    /// 回 `Open`：新终端的会话。
    Opened {
        req: u32,
        id: SessionId,
    },
    /// 回 `Reveal` 这类没有别的结果要给的请求：办好了。
    Done {
        req: u32,
    },
    /// 回 `ReadScreen`：一行一个 `\n`，行尾空白去掉。`truncated` 为真时要的内容开头已经被挤出
    /// 回滚历史，给的只是还留着的部分。
    ScreenText {
        id: SessionId,
        text: String,
        #[serde(default)]
        truncated: bool,
    },
    /// 回 `ClientMsg::Layout`。
    Layout {
        req: u32,
        windows: Vec<WindowLayout>,
    },
    /// 宿主转给界面去办的请求（`Open`、`Reveal`、`Layout`），只发给登记为界面的连接（`Hello`
    /// 里 `client` 是 `Desktop` 的，有几个时是最近连上的那个）。`request` 原样带着发请求一方的
    /// `req`；界面办完了用 `ClientMsg::UiReply` 带着同一个 `ui` 回话。
    ///
    /// 宿主自己也用它请界面读写剪贴板（`ClientMsg::WriteClipboard`、`ClientMsg::ReadClipboard`），
    /// 这两种优先发给最近和那个会话交互过的界面，没有时才是最近连上的那个。
    UiRequest {
        ui: u64,
        request: Box<ClientMsg>,
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
    /// 回 `ClientMsg::Handoff`：这次不交接，宿主接着关掉连接，会话照旧在它手里。
    HandoffRefused {
        reason: HandoffRefusal,
    },
    /// 回 `ClientMsg::Handoff`：开始交接。`format` 是接下来的描述符消息用的交接格式，`sessions`
    /// 是要交出的会话个数，即 `HandoffPart::Host` 之后跟着几条 `HandoffPart::Session`。这之后
    /// 宿主在这条连接上只发描述符消息，不再发帧。
    HandoffBegin {
        format: u32,
        sessions: u32,
    },
    /// 谁的视图尺寸决定这个会话的尺寸变了，只发给带着尺寸连着这个会话的前端。`mine` 为真是收到的
    /// 这条连接自己；`owner` 是那个前端在 `Hello` 里报的设备名，没有 owner 或者它没报时为空。
    /// 这是状态不是 VT 的标记，尺寸本身照旧只在输出流里的 `Resized` 处改。
    SizeOwner {
        id: SessionId,
        mine: bool,
        #[serde(default)]
        owner: Option<String>,
    },
    /// 回 `ClientMsg::ReadClipboard`：剪贴板里的文字；没读（用户不让读、剪贴板里没有文字、文字
    /// 太长）时为空。只在 `ClientMsg::UiReply` 里出现。
    ClipboardText {
        id: SessionId,
        #[serde(default)]
        text: Option<ClipboardContent>,
    },
    /// 比自己新的宿主才有的消息，前端忽略它。
    #[serde(other)]
    Unknown,
}

/// 剪贴板里的文字，见 `ClientMsg::WriteClipboard`、`HostMsg::ClipboardText`。线上就是一个字符串；
/// `Debug` 只写字节数，带着它的消息（以及包着它们的 `UiRequest`、`UiReply`）怎么记进日志都不会带出
/// 剪贴板的内容。
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClipboardContent(pub String);

impl fmt::Debug for ClipboardContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} bytes>", self.0.len())
    }
}

impl From<String> for ClipboardContent {
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl From<&str> for ClipboardContent {
    fn from(text: &str) -> Self {
        Self(text.to_owned())
    }
}

/// `Open` 的新终端放在哪里。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// 紧跟在旁边那个终端的标签后面的新标签。
    Tab,
    /// 把旁边那个终端一分为二，新终端在右边。
    Right,
    /// 把旁边那个终端一分为二，新终端在下边。
    Down,
}

/// `SessionList` 里的一个会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub size: GridSize,
    pub meta: SessionMeta,
    /// 现在连着它的前端有几个。
    #[serde(default)]
    pub clients: u32,
    /// 有桌面的界面连着它（只看状态的也算）；为假的是没有窗口在显示的后台会话。
    #[serde(default)]
    pub claimed: bool,
    /// shell 已经退出，会话还留着给前端看最后的屏幕。
    #[serde(default)]
    pub exited: bool,
    /// 现在决定这个会话尺寸的前端的设备名，见 `HostMsg::SizeOwner`；没有 owner 或者它没报设备名
    /// 时为空。
    #[serde(default)]
    pub size_owner: Option<String>,
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
    /// 比自己新的宿主才有的原因，前端当作连接断了处理。
    #[serde(other)]
    Unknown,
}

/// 宿主为什么不交接，见 `HostMsg::HandoffRefused`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoffRefusal {
    /// 有桌面的界面连着它：多半是旧版本的 app 还开着，要先退出它。
    DesktopConnected,
    /// 已经在交给别的新宿主了。
    Busy,
    /// 宿主跑在某个 app 的进程里（`Welcome::standalone` 为假），会话归那个 app 管。
    NotStandalone,
    /// 宿主写的交接格式 `writes` 不在 `Handoff` 给的范围里。
    UnsupportedFormat { writes: u32 },
    /// 比自己新的宿主才有的原因。
    #[serde(other)]
    Unknown,
}

/// `ClientMsg::Spawn::start` 缺省时为真：旧的前端开会话时一律当场启动。
fn yes() -> bool {
    true
}
