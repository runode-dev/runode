//! git 操作没办成的原因。`Display` 是英文的，宿主转给手机、写进日志时用；桌面按种类翻译成界面的语言。

use std::{fmt, path::PathBuf};

/// git 操作没办成。
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum GitError {
    /// git 自己的报错：标准错误（没有就用标准输出），去掉了 `hint:` 开头的提示。
    Git(String),
    /// git 失败了却什么也没说：它的退出状态。
    Exited(String),
    /// 没能运行 git。
    Spawn(String),
    /// git 超过 `secs` 秒没跑完，停下了；`lock` 是它可能留下的锁文件（只在 Windows 上找）。
    Timeout { secs: u64, lock: Option<PathBuf> },
    /// 读写仓库里的文件失败（`info/exclude`、要删掉的未跟踪文件）。
    Io { path: PathBuf, error: String },
    /// runode 自己看过以后没往下做。
    Refused(Refusal),
    /// 别处已经写成界面文字的原因（远端没答话、读不了预设），原样显示。
    Other(String),
}

/// runode 自己没往下做的情况。
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub enum Refusal {
    NotBranch(String),
    /// 找不到这个远端跟踪分支所属的远端。
    UnknownRemote(String),
    NotCommit(String),
    /// 路径带 `..`、是绝对路径或者在仓库外面。
    OutsideRepo(PathBuf),
    /// 这个文件（二进制、改名、子模块这类）不能按块操作。
    NotHunkable(PathBuf),
    NoSuchHunk,
    /// 这一块太长，没有读全。
    HunkTruncated(PathBuf),
    /// 读到以后这个文件的 diff 又变了。
    HunkChanged(PathBuf),
    DiffUnreadable(PathBuf),
    /// 丢弃改动前重读仓库的状态没读到，什么也没丢。
    StatusUnreadable,
    /// 丢弃改动前重读，这个文件的状态变了，什么也没丢。
    StatusChanged(PathBuf),
    /// 这个文件有没解决的冲突，没丢。
    Conflicted(PathBuf),
    /// 还有没解决的冲突，没有暂存全部再提交。
    UnresolvedConflicts,
    /// 目录名里有换行，写不进 `info/exclude`。
    NewlineInPath(PathBuf),
    NoExcludeFile,
    /// 撤销上一个提交：HEAD 指着的分支名不是 UTF-8。
    BranchNotUtf8,
    /// 撤销上一个提交：还没有提交。
    NoCommits,
    /// 撤销上一个提交：合并做到一半。
    Merging,
    /// 撤销上一个提交：cherry-pick 或 revert 做到一半。
    Sequencing,
    /// 撤销上一个提交：squash 合并了还没提交。
    SquashPending,
    /// 撤销上一个提交：上一个提交不在本地（浅克隆）。
    ParentMissing,
    /// 撤销上一个提交：HEAD 没在分支上，第一个提交撤销不了。
    DetachedFirstCommit,
    /// 撤销上一个提交：看过以后 HEAD 换了分支。
    HeadMoved,
    /// 撤销上一个提交：撤销时 HEAD 换了分支，已经恢复。
    HeadMovedRestored,
    NoRemote,
    /// 这条储藏已经变了，什么也没做。
    StashChanged(String),
}

impl From<Refusal> for GitError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Git(message) | Self::Other(message) => f.write_str(message),
            Self::Exited(status) => write!(f, "git failed ({status})"),
            Self::Spawn(error) => write!(f, "cannot run git: {error}"),
            Self::Timeout { secs, lock: None } => write!(f, "git did not finish within {secs} seconds and was stopped"),
            Self::Timeout { secs, lock: Some(lock) } => write!(
                f,
                "git did not finish within {secs} seconds and was stopped; it may have left {}, delete it once no other git is running",
                lock.display()
            ),
            Self::Io { path, error } => write!(f, "{}: {error}", path.display()),
            Self::Refused(refusal) => refusal.fmt(f),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBranch(name) => write!(f, "not a branch name: {name}"),
            Self::UnknownRemote(branch) => write!(f, "cannot find the remote of {branch}"),
            Self::NotCommit(id) => write!(f, "not a commit: {id}"),
            Self::OutsideRepo(path) => write!(f, "not a path in the repository: {}", path.display()),
            Self::NotHunkable(path) => write!(f, "{} cannot be changed hunk by hunk", path.display()),
            Self::NoSuchHunk => f.write_str("there is no such hunk"),
            Self::HunkTruncated(path) => {
                write!(f, "this hunk of {} is too long to be read in full", path.display())
            }
            Self::HunkChanged(path) => write!(f, "{} has changed; refresh and try again", path.display()),
            Self::DiffUnreadable(path) => write!(f, "cannot read the diff of {}", path.display()),
            Self::StatusUnreadable => f.write_str("cannot read the repository's status; nothing was discarded"),
            Self::StatusChanged(path) => {
                write!(f, "{} has changed; nothing was discarded, look at the latest changes first", path.display())
            }
            Self::Conflicted(path) => {
                write!(f, "{} has unresolved conflicts; resolve them first or use git", path.display())
            }
            Self::UnresolvedConflicts => f.write_str("there are unresolved conflicts; nothing was staged or committed"),
            Self::NewlineInPath(path) => write!(f, "{path:?} contains a line break and cannot go into info/exclude"),
            Self::NoExcludeFile => f.write_str("cannot find info/exclude"),
            Self::BranchNotUtf8 => f.write_str("the branch name is not UTF-8; nothing was undone"),
            Self::NoCommits => f.write_str("there are no commits to undo"),
            Self::Merging => f.write_str("a merge is in progress; nothing was undone"),
            Self::Sequencing => f.write_str("a cherry-pick or revert is in progress; nothing was undone"),
            Self::SquashPending => f.write_str("a squash merge hasn't been committed; nothing was undone"),
            Self::ParentMissing => {
                f.write_str("the previous commit is not available locally (shallow clone); nothing was undone")
            }
            Self::DetachedFirstCommit => f.write_str("HEAD is not on a branch; the first commit cannot be undone"),
            Self::HeadMoved => f.write_str("HEAD moved to another branch; nothing was undone"),
            Self::HeadMovedRestored => {
                f.write_str("HEAD moved to another branch while undoing; it was restored and nothing was undone")
            }
            Self::NoRemote => f.write_str("no remote is configured"),
            Self::StashChanged(rev) => {
                write!(f, "{rev} has changed; nothing was done, look at the latest stashes first")
            }
        }
    }
}

impl std::error::Error for GitError {}
