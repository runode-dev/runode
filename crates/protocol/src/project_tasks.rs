//! 一个目录里能跑的项目命令：用户自己加的命令、Makefile 的目标、package.json 的 scripts，回
//! `ClientMsg::ListProjectTasks`。手机在会话卡片上列出来，点一下就在那个会话里跑。
//!
//! 自己加的命令都在 runode 根目录的 `tasks.json` 里（`runode_paths::Dirs::tasks_file`）：
//! `{"global": {"名字": "命令行", …}, "projects": {"/项目/目录": {"名字": "命令行", …}, …}}`。宿主列出
//! 请求的目录所在的那个项目（`projects` 里是它自己或上级的、最深的那个）的命令和通用的命令，再从请求的
//! 目录往上找最近的 Makefile 和最近的 package.json（各找各的）；每一份是一个 `TaskSource`。每条命令的完整命令行由宿主拼好（`make -C ..`、按锁文件挑 `pnpm`、`yarn` 这类），
//! 前端原样打进 shell 就行，不用懂各种构建工具。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// `TaskSource::tasks` 一次最多给这么多条。
pub const MAX_PROJECT_TASKS: usize = 200;

/// 一个列出命令的文件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSource {
    pub kind: TaskSourceKind,
    /// 文件的绝对路径。
    pub file: PathBuf,
    /// `Custom` 的命令是给哪个项目目录的，也是它们跑的目录；别的种类为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    /// 按文件里的先后，最多 `MAX_PROJECT_TASKS` 条。
    pub tasks: Vec<ProjectTask>,
    /// 多于 `MAX_PROJECT_TASKS` 条，多出来的没给。
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
pub struct ProjectTask {
    /// 目标名或 script 名。
    pub name: String,
    /// 在请求的目录里打进 shell 就能跑的完整命令行，名字里有 shell 的特殊字符时已经加了引号。
    pub command: String,
    /// 说明：Makefile 目标那一行 `##` 后面的话，package.json 和自己加的命令里是写下的命令本身。
    #[serde(default)]
    pub description: Option<String>,
}
