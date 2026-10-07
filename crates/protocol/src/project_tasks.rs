//! 一个目录里能跑的项目命令：Makefile 的目标、package.json 的 scripts，回 `ClientMsg::ListProjectTasks`。
//! 手机在会话卡片上列出来，点一下就在那个会话里跑。
//!
//! 宿主从请求的目录往上找最近的 Makefile 和最近的 package.json（两样各找各的），每找到一个是一个
//! `TaskSource`。每条命令的完整命令行由宿主拼好（`make -C ..`、按锁文件挑 `pnpm`、`yarn` 这类），
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
    /// 说明：Makefile 目标那一行 `##` 后面的话，package.json 里是 script 本身。
    #[serde(default)]
    pub description: Option<String>,
}
