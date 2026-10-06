//! 升级时旧宿主交给新宿主的东西：监听的 socket、各个会话的 PTY 和它们的状态。
//!
//! 旧宿主回了 `HostMsg::HandoffBegin` 以后，在同一条连接上发一串描述符消息（每条带着几个文件
//! 描述符），依次是一条 `HandoffPart::Host`、`sessions` 条 `HandoffPart::Session`，新宿主回
//! `ClientMsg::HandoffReady` 后再发一条 `HandoffPart::Commit`。一条消息的数据由 `encode_part`
//! 编、`decode_part` 解：
//!
//! ```text
//! u32 LE 头的字节数 | 头（JSON：{"part": HandoffPart, "blocks": [各块的字节数]}） | 块 1 | 块 2 | ...
//! ```
//!
//! 快照、重放这类大块的字节原样跟在头后面，不进 JSON。每种 `HandoffPart` 带哪些块、哪些描述符
//! 写在它的文档里。
//!
//! 交接的两端是不同版本的宿主，这里的格式和 `message` 模块文档里列的消息一样冻结：已有字段的
//! 名字、类型和含义不改，只能加带 `#[serde(default)]` 的字段，或者在已有的块后面加块；改了含义
//! 才加 `HANDOFF_FORMAT`。嵌在里面的别处的类型（`TermSettings`、`GridSize`、`SessionMeta`、
//! `BuildId`、`SessionId`）同样受这条约束：比如给 `TermSettings` 加不带默认值的字段，新宿主就读
//! 不了旧宿主发的 `Session`。旧宿主只写自己的 `HANDOFF_FORMAT`，新宿主读得了
//! `OLDEST_READABLE_HANDOFF_FORMAT..=HANDOFF_FORMAT`。

use std::{fmt, path::PathBuf};

use runode_shared_types::{grid::GridSize, session::SessionMeta, settings::TermSettings};
use serde::{Deserialize, Deserializer, Serialize};

use crate::message::{BuildId, SessionId};

/// 这个版本的宿主交出会话时写的交接格式，见 `HostMsg::Welcome::handoff`。
pub const HANDOFF_FORMAT: u32 = 1;

/// 这个版本的宿主接手时读得了的最旧的交接格式，和 `HANDOFF_FORMAT` 一起放进 `ClientMsg::Handoff`。
pub const OLDEST_READABLE_HANDOFF_FORMAT: u32 = 1;

/// 交接时一条描述符消息里的东西。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HandoffPart {
    /// 宿主自己的状态，第一条。描述符是 `[监听的 socket, 锁文件]`；没有块。
    Host {
        /// 这串消息用的交接格式，和 `HandoffBegin::format` 一样。
        format: u32,
        /// 旧宿主的构建。
        build: BuildId,
        /// 旧宿主编的快照的格式版本，见 `HostMsg::Welcome::snapshot_format`；和新宿主的一样才解
        /// `Session` 带的快照，否则用重放。
        snapshot_format: u16,
        /// 接下来有几条 `Session`。
        sessions: u32,
        /// 前端用 `SetTheme` 设过的主题；没设过时为空，新宿主照旧等前端来设。
        #[serde(default)]
        theme: Option<TermSettings>,
        /// 记不记命令历史，见 `ClientMsg::SetOptions`。
        record_history: bool,
        /// 监听的 socket 的路径。描述符传过去后路径不变，shell 里的 `RUNODE_SOCKET` 照样有效。
        socket: PathBuf,
    },
    /// 一个会话。已经启动的会话带一个描述符 `[PTY master]`，还没启动的不带，新宿主重新打开伪终端；
    /// 两个块依次是 `snapshot`（宿主那份 VT 的原始快照，没抹掉报告口令；编不出来时为空）和
    /// `replay`（重画当前屏幕的 VT 序列，快照用不了时用它）。
    Session {
        id: SessionId,
        /// shell 已经启动了。
        started: bool,
        /// shell 的进程号；还没启动时为空。
        #[serde(default)]
        pid: Option<u32>,
        size: GridSize,
        /// 交给 shell 集成脚本的报告口令，新宿主据此接着采用 shell 的报告。
        #[serde(default)]
        report_token: Option<ReportToken>,
        /// 宿主那份 VT 现在套着的主题。
        settings: TermSettings,
        /// `Spawn` 时指定的程序，见 `ClientMsg::Spawn::shell`；还没启动的会话 `Start` 时用。
        #[serde(default)]
        shell: Option<String>,
        /// 会话开在哪个目录。
        #[serde(default)]
        start_dir: Option<PathBuf>,
        /// `Spawn` 时另外设的环境变量，见 `ClientMsg::Spawn::env`；还没启动的会话 `Start` 时用。
        #[serde(default)]
        env: Vec<(String, String)>,
        /// 会话对外公布的状态。装箱只是为了别让这种消息比另外几种大太多，JSON 里一样。
        #[serde(default)]
        meta: Box<SessionMeta>,
        /// shell 集成报告过提示符。
        #[serde(default)]
        prompt_reported: bool,
        /// 正在运行、还没报告结束的命令，结束时记进历史。
        #[serde(default)]
        running: Option<RunningCommand>,
        /// shell 集成在这次提示符前报告的目录，还没被下一次等输入取走。
        #[serde(default)]
        pending_shell_cwd: Option<PathBuf>,
        /// 收到了、还没被下一个 OSC 133;C 取走的 `command` 报告：外层为空是没收到，里面为空是报告
        /// 没带命令原文。JSON 里没有这个字段是外层为空，`null` 是里面为空。
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
        pending_command: Option<Option<String>>,
        /// 给前端的输出抹报告口令的状态，停在一条报告中间时新宿主接着抹。
        #[serde(default)]
        redactor: RedactorState,
    },
    /// 提交：旧宿主收到 `ClientMsg::HandoffReady` 后发，之后会话归新宿主。没有描述符；
    /// `pending_input` 里每个会话依次一块，是旧宿主收下了、还没写进 PTY 的输入，新宿主先写它们。
    Commit {
        #[serde(default)]
        pending_input: Vec<SessionId>,
    },
}

impl HandoffPart {
    /// 这种消息至少带几块。多出来的块是以后的版本加的，读的一方不认识就不管。
    fn blocks_needed(&self) -> usize {
        match self {
            Self::Host { .. } => 0,
            Self::Session { .. } => 2,
            Self::Commit { pending_input } => pending_input.len(),
        }
    }
}

/// 正在运行的命令，结束时记进历史，见 `FinishedCommand`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningCommand {
    pub cmd: String,
    /// 命令在哪个目录里运行。
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// 开始运行的时刻，Unix 秒。
    pub ts: u64,
}

/// 抹报告口令的状态：输出停在一条 shell 集成报告的开头序列或者内容中间时，接着的输出要接着
/// 认、接着抹。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RedactorState {
    /// 报告的开头序列已经对上了几个字节。
    pub matched: u8,
    /// 正处在一条报告的内容里。
    pub inside: bool,
}

/// shell 集成的报告口令。`Debug` 不打出口令，免得交接的消息进了日志就漏出去。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReportToken(pub String);

impl fmt::Debug for ReportToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// 消息的头：`HandoffPart` 和跟在后面的各块的字节数。
#[derive(Serialize, Deserialize)]
struct Header<P> {
    part: P,
    blocks: Vec<u64>,
}

/// 编、解一条交接消息失败的原因。
#[derive(Debug)]
pub enum HandoffPartError {
    /// 头编不成或者解不开 JSON，比如路径不是 UTF-8、对面的格式读不懂。
    Json(serde_json::Error),
    /// 数据比头里说的短：连头的字节数都不够，或者块的字节数加起来超出了数据。
    Truncated,
    /// 所有的块之后还剩这么多字节。
    Trailing(usize),
    /// 这种消息要 `needed` 块，只有 `got` 块。
    MissingBlocks { needed: usize, got: usize },
    /// 头超过了 `u32` 能写的长度。
    TooLong,
}

impl fmt::Display for HandoffPartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(err) => write!(f, "bad handoff header: {err}"),
            Self::Truncated => write!(f, "handoff message is shorter than its header says"),
            Self::Trailing(len) => write!(f, "{len} stray bytes after the handoff message's blocks"),
            Self::MissingBlocks { needed, got } => write!(f, "handoff message has {got} blocks, needs {needed}"),
            Self::TooLong => write!(f, "handoff header is too long"),
        }
    }
}

impl std::error::Error for HandoffPartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(err) => Some(err),
            _ => None,
        }
    }
}

/// 把一条交接消息编成描述符消息的数据：头连同 `blocks` 原样拼在后面。`part` 里的路径不是 UTF-8
/// 时编不成 JSON，返回错误。
pub fn encode_part(part: &HandoffPart, blocks: &[&[u8]]) -> Result<Vec<u8>, HandoffPartError> {
    let header = Header { part, blocks: blocks.iter().map(|block| block.len() as u64).collect() };
    let json = serde_json::to_vec(&header).map_err(HandoffPartError::Json)?;
    let len = u32::try_from(json.len()).map_err(|_| HandoffPartError::TooLong)?;
    let total = 4 + json.len() + blocks.iter().map(|block| block.len()).sum::<usize>();
    let mut data = Vec::with_capacity(total);
    data.extend_from_slice(&len.to_le_bytes());
    data.extend_from_slice(&json);
    for block in blocks {
        data.extend_from_slice(block);
    }
    Ok(data)
}

/// 解 `encode_part` 编的数据，块借用 `data` 里的字节。长度一律对着数据核对：头或者块超出数据、
/// 块之后还有多余的字节、块比这种消息要的少，都报错。
pub fn decode_part(data: &[u8]) -> Result<(HandoffPart, Vec<&[u8]>), HandoffPartError> {
    let (len, rest) = data.split_first_chunk::<4>().ok_or(HandoffPartError::Truncated)?;
    let len = usize::try_from(u32::from_le_bytes(*len)).map_err(|_| HandoffPartError::Truncated)?;
    let (json, mut rest) = rest.split_at_checked(len).ok_or(HandoffPartError::Truncated)?;
    let header: Header<HandoffPart> = serde_json::from_slice(json).map_err(HandoffPartError::Json)?;
    let mut blocks = Vec::with_capacity(header.blocks.len().min(rest.len() + 1));
    for &block_len in &header.blocks {
        let block_len = usize::try_from(block_len).map_err(|_| HandoffPartError::Truncated)?;
        let (block, tail) = rest.split_at_checked(block_len).ok_or(HandoffPartError::Truncated)?;
        blocks.push(block);
        rest = tail;
    }
    if !rest.is_empty() {
        return Err(HandoffPartError::Trailing(rest.len()));
    }
    let needed = header.part.blocks_needed();
    if blocks.len() < needed {
        return Err(HandoffPartError::MissingBlocks { needed, got: blocks.len() });
    }
    Ok((header.part, blocks))
}

/// 字段出现了就是外层的 `Some`，`null` 读成 `Some(None)`；字段不在时由 `#[serde(default)]` 给
/// `None`。
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}
