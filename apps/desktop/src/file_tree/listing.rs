//! 读文件树要显示的目录，以及按 git 状态给路径找标记。不碰界面；app 在后台读 git 状态
//! 的同一次任务里调 `list_dirs`，文件树展开目录、改了文件时也在后台调。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use runode_git::{self as git, FileStatus};

pub use runode_files::Listings;
pub(crate) use runode_files::{DirEntry, MAX_COMPACT, single_dir};

/// 读 `dirs` 这些目录，被忽略的子目录（按 git 状态 `git` 算）不往下看，见 `runode_files::list_dirs`。
pub fn list_dirs(dirs: Vec<PathBuf>, git: Option<&git::Repos>) -> Listings {
    let changes = GitChanges::new(git);
    let decorator = Decorator::new(git, &changes);
    runode_files::list_dirs(dirs, |child| decorator.of(child, true) == Decoration::Ignored)
}

/// 文件树里一项的 git 标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decoration {
    None,
    Status(FileStatus),
    /// 目录里有改动的文件，带着按 `dir_status` 归总出的状态。
    ContainsChanges(FileStatus),
    Ignored,
}

/// 目录里已归总的状态 `current` 再并进一个文件的状态 `status`，第一个文件时 `current` 为空：
/// 有冲突的算冲突，有改动已跟踪文件的（修改、删除、改名）都算修改，只有新文件的才保留新增
/// 或未跟踪。
fn dir_status(current: Option<FileStatus>, status: FileStatus) -> FileStatus {
    let rank = |status| match status {
        FileStatus::Conflicted => 3,
        FileStatus::Modified | FileStatus::Deleted | FileStatus::Renamed => 2,
        FileStatus::Added => 1,
        FileStatus::Untracked => 0,
    };
    let status = match status {
        FileStatus::Deleted | FileStatus::Renamed => FileStatus::Modified,
        status => status,
    };
    match current {
        Some(current) if rank(current) >= rank(status) => current,
        _ => status,
    }
}

/// 按一份 git 状态算好的改动表：哪些文件有改动、哪些目录里含着改动。改动多时算一遍不便宜，文件树按
/// 快照留着一份，git 状态换了才重算。
#[derive(Default)]
pub(crate) struct GitChanges {
    /// 主仓库和子仓库里有改动的文件，相对主仓库根，见 `Repos::statuses`。
    statuses: HashMap<PathBuf, FileStatus>,
    /// 含有改动文件的目录，相对主仓库根，值是归总后的状态。
    changed_dirs: HashMap<PathBuf, FileStatus>,
}

impl GitChanges {
    pub(crate) fn new(git: Option<&git::Repos>) -> Self {
        let statuses: HashMap<_, _> = git.iter().flat_map(|git| git.statuses()).collect();
        let mut changed_dirs: HashMap<PathBuf, FileStatus> = HashMap::new();
        for (path, &status) in &statuses {
            for dir in path.ancestors().skip(1).filter(|dir| !dir.as_os_str().is_empty()) {
                let current = changed_dirs.get(dir).copied();
                changed_dirs.insert(dir.to_path_buf(), dir_status(current, status));
            }
        }
        Self { statuses, changed_dirs }
    }
}

/// 按 git 状态给文件树里的路径找标记。子模块和嵌套仓库里的文件按它们自己那个仓库的状态。`changes` 要是
/// 按同一份 `git` 算的。
pub(crate) struct Decorator<'a> {
    git: Option<&'a git::Repos>,
    changes: &'a GitChanges,
}

impl<'a> Decorator<'a> {
    pub(crate) fn new(git: Option<&'a git::Repos>, changes: &'a GitChanges) -> Self {
        Self { git, changes }
    }

    pub(crate) fn of(&self, path: &Path, is_dir: bool) -> Decoration {
        let Some(git) = self.git else {
            return Decoration::None;
        };
        let Ok(rel) = path.strip_prefix(&git.main.root) else {
            return Decoration::None;
        };
        if git.is_ignored(rel) {
            return Decoration::Ignored;
        }
        let own = self.changes.statuses.get(rel).copied();
        if is_dir {
            // 目录自己也可能有状态：子模块记着的提交号变了，嵌套仓库在外层是个未跟踪的目录。
            let inside = self.changes.changed_dirs.get(rel).copied();
            if let Some(status) = own.map(|own| dir_status(inside, own)).or(inside) {
                return Decoration::ContainsChanges(status);
            }
        } else if let Some(status) = own {
            return Decoration::Status(status);
        }
        Decoration::None
    }
}
