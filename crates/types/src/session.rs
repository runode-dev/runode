//! 一个终端会话对外公布的状态：标签上的名字、前台 agent、所在目录、shell 集成报告的东西，
//! 以及最近是不是有别的终端里的程序在操作它。管着会话的一方随时把它发给连着的各个界面，界面
//! 不用自己去读 VT 或问操作系统。会话的标识 `SessionId` 也在这里。

use std::{ffi::OsString, fmt, path::PathBuf, str::FromStr, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{agent::Agent, shell::ShellNames};

/// 一个终端会话对外公布的状态。缺的字段按默认值读。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SessionMeta {
    /// 程序设置的标题，agent 的状态前缀已经拆到 `agent` 里；没设置时为 `None`。
    pub title: Option<String>,
    /// 程序没设置标题时用的名字：前台程序名，或者 shell 所在目录的名字。
    pub fallback_title: Option<String>,
    /// 前台 agent 和它的状态；不是 agent 在前台时为 `None`。
    pub agent: Option<Agent>,
    /// shell 当前所在的目录。
    #[serde(serialize_with = "crate::lossy_path::serialize_opt")]
    pub cwd: Option<PathBuf>,
    /// shell 最近一次等着输入时所在的目录；还没等过输入时为 `None`。插件管理器在提示符出来后可能临时切进别的目录，所以和 `cwd` 分开。
    #[serde(serialize_with = "crate::lossy_path::serialize_opt")]
    pub prompt_cwd: Option<PathBuf>,
    /// 前台是不是 shell 自己，即没有命令在运行。
    pub foreground_is_shell: bool,
    /// shell 集成报告的 shell 自己的 PATH；界面找用户装的命令行工具（gh）时用。PATH 不一定是合法的 UTF-8，
    /// 所以按操作系统的字符串存。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_path: Option<OsString>,
    /// shell 集成报告的函数名。列表可能很长，状态每次变了都要拷一份，所以共享着放：名字没变时
    /// 各份状态指向同一份，不必每次深拷贝。
    #[serde(skip_serializing_if = "no_names")]
    pub shell_names: Arc<ShellNames>,
    /// 前台程序的名字（进程名，不带路径）；还没启动、读不到时为 `None`。
    pub foreground: Option<String>,
    /// 最近一次别的终端里的程序（经命令行）操作这个会话的记录；用户自己在界面里打字后清掉。
    pub driver: Option<Driver>,
    /// shell 的进程号；还没启动时为 `None`。界面按它把进程和监听的端口归到终端上。
    pub pid: Option<u32>,
}

/// 没有函数名时不写出来，读的一方按默认值读回同样的空表。
fn no_names(names: &Arc<ShellNames>) -> bool {
    **names == ShellNames::default()
}

/// 别的终端里的程序操作了这个会话：谁、做了什么、什么时候。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Driver {
    /// 操作方所在会话的标识，写成 32 个小写十六进制数字（和会话标识的写法一样）；不在 runode
    /// 的终端里跑的程序为 `None`。
    #[serde(default)]
    pub by: Option<String>,
    pub action: DriveAction,
    /// 最近一次操作的时刻，Unix 毫秒。连着不停地操作时最多每秒更新一次，免得状态跟着每个按键变。
    pub at_ms: u64,
}

/// 别的程序对会话做了什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriveAction {
    /// 逐字打字（输入帧）。
    Input,
    /// 发控制键（`ctrl-c`、`up` 这类）。
    Keys,
    /// 粘贴一段文字。
    Paste,
    /// 清屏。
    ClearScreen,
    /// 结束会话。
    Kill,
    /// 比自己新的一方才有的操作。
    #[serde(other)]
    Unknown,
}

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
