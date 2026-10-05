//! 在后台读一次项目：git 状态和文件树要显示的目录内容，以及按 git 状态给路径找标记。

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use runode_git_status::{self as git, FileStatus};

use super::MAX_COMPACT;

/// 目录里的一项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::workspace) struct DirEntry {
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
    let mut listings: Vec<_> = dirs.into_iter().map(|dir| {
        let listing = read_dir(&dir);
        (dir, listing)
    }).collect();
    let children: Vec<_> = listings
        .iter()
        .flat_map(|(dir, listing)| listing.iter().flatten().filter(|entry| entry.is_dir).map(|entry| dir.join(&entry.name)))
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
    pub(super) git: Option<git::Snapshot>,
    pub(super) listings: Vec<(PathBuf, Option<Vec<DirEntry>>)>,
    pub(super) untracked: git::UntrackedCache,
    /// 读这一份花的时间。
    pub(super) cost: Duration,
}

pub(super) fn scan(dir: PathBuf, expanded: Vec<PathBuf>, mut untracked: git::UntrackedCache) -> Scan {
    let started = Instant::now();
    let git = git::snapshot(&dir, &mut untracked);
    let root = git.as_ref().map_or_else(|| dir.clone(), |git| git.root.clone());
    let dirs = std::iter::once(root.clone()).chain(expanded.into_iter().filter(|path| path.starts_with(&root)));
    let listings = list_dirs(dirs.collect(), &Decorator::new(git.as_ref()));
    Scan { dir, root, git, listings, untracked, cost: started.elapsed() }
}

/// 文件树里一项的 git 标记。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::workspace) enum Decoration {
    None,
    Status(FileStatus),
    /// 目录里有改动的文件。
    ContainsChanges,
    Ignored,
}

/// 按 git 状态给文件树里的路径找标记。
pub(super) struct Decorator<'a> {
    git: Option<&'a git::Snapshot>,
    /// 含有改动文件的目录，相对仓库根。
    changed_dirs: HashSet<&'a Path>,
    ignored: HashSet<&'a Path>,
}

impl<'a> Decorator<'a> {
    pub(super) fn new(git: Option<&'a git::Snapshot>) -> Self {
        let mut changed_dirs = HashSet::new();
        let mut ignored = HashSet::new();
        if let Some(git) = git {
            for path in git.statuses.keys() {
                changed_dirs.extend(path.ancestors().skip(1).filter(|dir| !dir.as_os_str().is_empty()));
            }
            ignored.extend(git.ignored.iter().map(PathBuf::as_path));
        }
        Self { git, changed_dirs, ignored }
    }

    /// `rel` 相对仓库根，它或者它所在的目录被忽略。
    fn ignores(&self, rel: &Path) -> bool {
        rel.ancestors().any(|dir| self.ignored.contains(dir))
    }

    pub(super) fn of(&self, path: &Path, is_dir: bool) -> Decoration {
        let Some(git) = self.git else {
            return Decoration::None;
        };
        let Ok(rel) = path.strip_prefix(&git.root) else {
            return Decoration::None;
        };
        if self.ignores(rel) {
            return Decoration::Ignored;
        }
        if is_dir {
            if self.changed_dirs.contains(rel) {
                return Decoration::ContainsChanges;
            }
        } else if let Some(status) = git.statuses.get(rel) {
            return Decoration::Status(*status);
        }
        Decoration::None
    }
}
