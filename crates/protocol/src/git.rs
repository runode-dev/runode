//! 前端请宿主在一个会话所在的仓库里读写 git：`ClientMsg::Git` 带着 `GitRequest`，宿主按那个会话
//! shell 当前的目录找到仓库，办完回 `HostMsg::GitStatus`、`GitDiff` 或 `GitBranches`，办不了回
//! `HostMsg::Error`。手机这类没有自己的 git 的前端用它；桌面直接读本地的仓库，不走这里。
//!
//! 这里的类型是线上的样子，和 `runode_git` 的对应类型字段一一对应，由宿主转换；路径一律相对仓库根。
//! 枚举新加的取值旧的一方读成 `Unknown`。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// `ClientMsg::Git` 要办的事。改仓库的操作办完后回改完的 `GitStatus`，省得前端再要一次。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GitRequest {
    /// 读分支和改动的文件，回 `GitStatus`。
    Status,
    /// 读一个文件在暂存段（`staged`）或未暂存段的逐行改动，回 `GitDiff`。
    Diff {
        path: PathBuf,
        staged: bool,
    },
    /// 暂存这些文件，含删掉的文件。
    Stage {
        paths: Vec<PathBuf>,
    },
    /// 把这些文件撤回到未暂存段；暂存了的改名要连同旧路径一起给。
    Unstage {
        paths: Vec<PathBuf>,
    },
    StageAll,
    UnstageAll,
    /// 提交暂存的改动；`stage_all` 时先暂存所有改动。
    Commit {
        message: String,
        #[serde(default)]
        stage_all: bool,
    },
    Fetch,
    Pull,
    /// 推送当前分支，还没设上游时推到同名的远端分支并设成上游。
    Push,
    /// 先拉取再推送。
    Sync,
    /// 列出本地和远端分支，回 `GitBranches`。
    Branches,
    /// 切到 `GitBranch::name` 这个分支；远端分支切到同名的本地分支，没有时新建一个跟踪它的。
    Checkout {
        branch: String,
        remote: bool,
    },
    /// 比自己新的一方才有的操作，宿主回 `Error`。
    #[serde(other)]
    Unknown,
}

/// 仓库此刻的样子，回 `GitRequest::Status` 和改仓库的操作。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    /// 仓库根目录，宿主那边的绝对路径。
    pub root: PathBuf,
    /// 当前分支；分离头指针时为空。
    #[serde(default)]
    pub branch: Option<String>,
    /// HEAD 的短哈希；还没有提交时为空。
    #[serde(default)]
    pub head: Option<String>,
    /// 上游分支，如 `origin/main`。
    #[serde(default)]
    pub upstream: Option<String>,
    #[serde(default)]
    pub ahead: u32,
    #[serde(default)]
    pub behind: u32,
    /// 配了至少一个远端。
    #[serde(default)]
    pub has_remote: bool,
    /// 做到一半的合并之类。
    #[serde(default)]
    pub operation: Option<GitOperation>,
    /// 已暂存、未暂存（含未跟踪）两段的文件，按路径排序；部分暂存的文件两段里都有。
    #[serde(default)]
    pub staged: Vec<GitFile>,
    #[serde(default)]
    pub unstaged: Vec<GitFile>,
}

/// 做到一半、等着继续或放弃的操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitOperation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    #[serde(other)]
    Unknown,
}

/// 有改动的一个文件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFile {
    /// 相对仓库根；删掉的文件是原来的路径。
    pub path: PathBuf,
    /// 改名前的路径。
    #[serde(default)]
    pub old_path: Option<PathBuf>,
    pub status: GitFileStatus,
    #[serde(default)]
    pub added: u32,
    #[serde(default)]
    pub removed: u32,
    #[serde(default)]
    pub binary: bool,
    /// 子模块那样记着一个提交号的条目。
    #[serde(default)]
    pub gitlink: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitFileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
    #[serde(other)]
    Unknown,
}

/// 一个文件在一段里的逐行改动，回 `GitRequest::Diff`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFileDiff {
    pub file: GitFile,
    pub hunks: Vec<GitHunk>,
    /// 改动太多，或者是没读内容的未跟踪文件，`hunks` 不全。
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHunk {
    /// `@@ -a,b +c,d @@` 以及后面的函数名之类。
    pub header: String,
    pub lines: Vec<GitLine>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLine {
    pub kind: GitLineKind,
    /// 在旧文件和新文件里的行号；新增的行没有旧行号，删掉的行没有新行号。
    #[serde(default)]
    pub old: Option<u32>,
    #[serde(default)]
    pub new: Option<u32>,
    /// 去掉开头的 `+`、`-` 或空格，制表符换成了空格。
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitLineKind {
    Context,
    Added,
    Removed,
    #[serde(other)]
    Unknown,
}

/// 分支列表里的一项。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranch {
    /// 本地分支是 `main` 这样的名字，远端分支带上远端名，如 `origin/main`。
    pub name: String,
    pub remote: bool,
    /// 当前所在的分支。
    #[serde(default)]
    pub current: bool,
    #[serde(default)]
    pub upstream: Option<String>,
    /// 最近一次提交的标题和相对时间（如 `2 days ago`）。
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub date: String,
}
