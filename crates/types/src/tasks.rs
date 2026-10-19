//! 一个目录里能跑的命令：用户自己加的命令、Makefile 的目标、package.json 的 scripts，回
//! `ClientMsg::ListTasks`。手机在会话卡片上列出来，点一下就在那个会话里跑。
//!
//! 自己加的命令存在 `runode_paths::Dirs::tasks_file` 的 `tasks.json` 里，分 `global` 和按项目目录的
//! `projects` 两份。每一份是一个 `TaskSource`；每条命令的完整命令行由宿主拼好，前端在
//! `TaskSource::project`（为空时是请求的目录）里原样打进 shell 就行，不用懂各种构建工具。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// `TaskSource::tasks` 一次最多给这么多条。
pub const MAX_TASKS: usize = 200;

/// 一个列出命令的文件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSource {
    pub kind: TaskSourceKind,
    /// 文件的绝对路径。
    #[serde(serialize_with = "crate::lossy_path::serialize")]
    pub file: PathBuf,
    /// `Custom` 的命令是给哪个项目目录的，也是它们跑的目录（不是请求的目录时，前端在这里开终端跑）；
    /// 别的种类为空，在请求的目录里跑。
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "crate::lossy_path::serialize_opt")]
    pub project: Option<PathBuf>,
    /// 按文件里的先后，最多 `MAX_TASKS` 条。
    pub tasks: Vec<TaskEntry>,
    /// 多于 `MAX_TASKS` 条，多出来的没给。
    #[serde(default)]
    pub truncated: bool,
}

/// 命令从哪种文件里来。新的种类旧的前端读成 `Unknown`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSourceKind {
    /// 用户自己加的、只用于 `TaskSource::project` 这个项目的命令，在项目目录里跑。
    Custom,
    /// 用户自己加的、所有目录都列的命令，在请求的目录里跑。
    Global,
    /// `GNUmakefile`、`makefile` 或 `Makefile` 里的目标。
    Makefile,
    /// package.json 里的 `scripts`。
    PackageJson,
    #[serde(other)]
    Unknown,
}

/// 一条能跑的命令。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEntry {
    /// 目标名或 script 名。
    pub name: String,
    /// 在 `TaskSource::project`（为空时是请求的目录）里打进 shell 就能跑的完整命令行，名字里有 shell 的
    /// 特殊字符时已经加了引号。
    pub command: String,
    /// 说明：Makefile 目标那一行 `##` 后面的话，package.json 和自己加的命令里是写下的命令本身。
    #[serde(default)]
    pub description: Option<String>,
}
