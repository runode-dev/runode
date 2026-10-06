//! 在后台读一次项目：git 状态和文件树要显示的目录内容，以及按 git 状态给路径找标记。

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use runode_git::{self as git, FileStatus};

use super::MAX_COMPACT;

/// 目录里的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::window) struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// 读一个目录，目录在前，各自按名字排，不分大小写；读不了时为空。
fn read_dir(dir: &Path) -> Option<Vec<DirEntry>> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" || name == ".DS_Store" {
                return None;
            }
            // 指向目录的符号链接也当目录；只有符号链接才要再查一次。
            let is_dir = match entry.file_type() {
                Ok(kind) if kind.is_symlink() => fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()),
                Ok(kind) => kind.is_dir(),
                Err(_) => false,
            };
            Some(DirEntry { name, is_dir })
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Some(entries)
}

/// 目录里只有一个子目录时是它的名字，文件树把这样的目录和子目录并成一行。
pub(super) fn single_dir(entries: &[DirEntry]) -> Option<&str> {
    match entries {
        [entry] if entry.is_dir => Some(&entry.name),
        _ => None,
    }
}

/// 读 `dirs` 这些目录，再往下多看一层：收起的子目录只有一个子目录时要和它并成一行，得先
/// 知道它里面有什么；这样的链条一直读到头。被忽略的子目录不往下看，里面多半很大。
pub(super) fn list_dirs(dirs: Vec<PathBuf>, decorator: &Decorator) -> Vec<(PathBuf, Option<Vec<DirEntry>>)> {
    let mut listed: HashSet<PathBuf> = dirs.iter().cloned().collect();
    let mut listings: Vec<_> = dirs
        .into_iter()
        .map(|dir| {
            let listing = read_dir(&dir);
            (dir, listing)
        })
        .collect();
    let children: Vec<_> = listings
        .iter()
        .flat_map(|(dir, listing)| {
            listing.iter().flatten().filter(|entry| entry.is_dir).map(|entry| dir.join(&entry.name))
        })
        .filter(|child| decorator.of(child, true) != Decoration::Ignored)
        .collect();
    for child in children {
        let mut next = Some(child);
        for _ in 0..MAX_COMPACT {
            let Some(dir) = next.take().filter(|dir| listed.insert(dir.clone())) else {
                break;
            };
            let listing = read_dir(&dir);
            next = listing.as_deref().and_then(single_dir).map(|name| dir.join(name));
            listings.push((dir, listing));
        }
    }
    listings
}

/// 后台读到的一份结果：git 状态，以及文件树根目录和展开的目录的内容，读不了的目录为 `None`。
pub(super) struct Scan {
    /// 读的是哪个目录：当前终端的目录，取不到时是 workspace 的目录。
    pub(super) dir: PathBuf,
    /// 文件树的根目录：`dir` 所在仓库的根，不在仓库里时是 `dir` 本身。
    pub(super) root: PathBuf,
    /// 这个仓库以及它里面的子模块和嵌套仓库。
    pub(super) git: Option<git::Repos>,
    pub(super) listings: Vec<(PathBuf, Option<Vec<DirEntry>>)>,
    pub(super) untracked: git::UntrackedCache,
    /// 读这一份花的时间。
    pub(super) cost: Duration,
}

/// 读一遍项目；`options` 说读不读其他工作树（Git 面板没开时不读）。
pub(super) fn scan(
    dir: PathBuf,
    expanded: Vec<PathBuf>,
    mut untracked: git::UntrackedCache,
    options: git::ReadOptions,
) -> Scan {
    let started = Instant::now();
    let git = git::snapshot_repos(&dir, &mut untracked, options);
    let root = git.as_ref().map_or_else(|| dir.clone(), |git| git.main.root.clone());
    let dirs = std::iter::once(root.clone()).chain(expanded.into_iter().filter(|path| path.starts_with(&root)));
    let listings = list_dirs(dirs.collect(), &Decorator::new(git.as_ref()));
    Scan { dir, root, git, listings, untracked, cost: started.elapsed() }
}

/// 文件树里一项的 git 标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::window) enum Decoration {
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

/// 按 git 状态给文件树里的路径找标记。子模块和嵌套仓库里的文件按它们自己那个仓库的状态。
pub(super) struct Decorator<'a> {
    git: Option<&'a git::Repos>,
    /// 各个仓库里有改动的文件，相对主仓库根。主仓库的直接借它的路径，子仓库的要拼上前缀。
    statuses: HashMap<Cow<'a, Path>, FileStatus>,
    /// 含有改动文件的目录，相对主仓库根，值是归总后的状态。
    changed_dirs: HashMap<PathBuf, FileStatus>,
}

impl<'a> Decorator<'a> {
    pub(super) fn new(git: Option<&'a git::Repos>) -> Self {
        let mut statuses = HashMap::new();
        for repo in git.iter().flat_map(|git| git.iter()) {
            for (path, &status) in &repo.statuses {
                let path = if repo.prefix.as_os_str().is_empty() {
                    Cow::Borrowed(path.as_path())
                } else {
                    Cow::Owned(repo.prefix.join(path))
                };
                statuses.insert(path, status);
            }
        }
        let mut changed_dirs: HashMap<PathBuf, FileStatus> = HashMap::new();
        for (path, &status) in &statuses {
            for dir in path.ancestors().skip(1).filter(|dir| !dir.as_os_str().is_empty()) {
                let current = changed_dirs.get(dir).copied();
                changed_dirs.insert(dir.to_path_buf(), dir_status(current, status));
            }
        }
        Self { git, statuses, changed_dirs }
    }

    pub(super) fn of(&self, path: &Path, is_dir: bool) -> Decoration {
        let Some(git) = self.git else {
            return Decoration::None;
        };
        let Ok(rel) = path.strip_prefix(&git.main.root) else {
            return Decoration::None;
        };
        if git.is_ignored(rel) {
            return Decoration::Ignored;
        }
        let own = self.statuses.get(rel).copied();
        if is_dir {
            // 目录自己也可能有状态：子模块记着的提交号变了，嵌套仓库在外层是个未跟踪的目录。
            let inside = self.changed_dirs.get(rel).copied();
            if let Some(status) = own.map(|own| dir_status(inside, own)).or(inside) {
                return Decoration::ContainsChanges(status);
            }
        } else if let Some(status) = own {
            return Decoration::Status(status);
        }
        Decoration::None
    }
}
