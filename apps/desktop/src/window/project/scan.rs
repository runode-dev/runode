//! 在后台读一次项目：git 状态，以及文件树要显示的目录内容（读目录在 `file_tree::list_dirs`）。

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::file_tree::{Listings, list_dirs};
use runode_git as git;

/// 后台读到的一份结果：git 状态，以及文件树根目录和展开的目录的内容，读不了的目录为 `None`。
pub(super) struct Scan {
    /// 读的是哪个目录：当前终端的目录，取不到时是 workspace 的目录。
    pub(super) dir: PathBuf,
    /// 文件树的根目录：`dir` 所在仓库的根，不在仓库里时是 `dir` 本身。
    pub(super) root: PathBuf,
    /// 这个仓库以及它里面的子模块和嵌套仓库。
    pub(super) git: Option<git::Repos>,
    pub(super) listings: Listings,
    /// 监听 `root` 时还要另外听的 git 目录，见 `watch_git_dirs`。
    pub(super) watch_git_dirs: Vec<PathBuf>,
    pub(super) untracked: git::UntrackedCache,
    /// 读这一份花的时间。
    pub(super) cost: Duration,
}

/// 读一遍项目；`expanded` 是文件树里展开着的目录，`worktrees` 说读不读其他工作树（Git 面板没开时不读）。
pub(super) fn scan(dir: PathBuf, expanded: Vec<PathBuf>, mut untracked: git::UntrackedCache, worktrees: bool) -> Scan {
    let started = Instant::now();
    let git = git::snapshot_repos(&dir, &mut untracked, worktrees);
    let root = git.as_ref().map_or_else(|| dir.clone(), |git| git.main.root.clone());
    let dirs = std::iter::once(root.clone()).chain(expanded.into_iter().filter(|path| path.starts_with(&root)));
    let listings = list_dirs(dirs.collect(), git.as_ref());
    let watch_git_dirs = watch_git_dirs(&root, git.as_ref());
    Scan { dir, root, git, listings, watch_git_dirs, untracked, cost: started.elapsed() }
}

/// 监听 `root` 时还要另外听的 git 目录。主仓库是 worktree 时它的 git 目录、连同里面子模块的 git 目录都不在
/// `root` 下面；一个套在另一个里面的只听外面那个。worktree 的分支、tag 在 `commondir` 指向的共用 git 目录里，
/// 也要听。要解符号链接、读文件，在后台算：目录在挂死的网络卷上时会卡住。
fn watch_git_dirs(root: &Path, git: Option<&git::Repos>) -> Vec<PathBuf> {
    let real_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut git_dirs: Vec<(PathBuf, PathBuf)> = git
        .iter()
        .flat_map(|git| git.iter())
        .flat_map(|repo| {
            let common =
                fs::read_to_string(repo.git_dir.join("commondir")).ok().map(|common| repo.git_dir.join(common.trim()));
            std::iter::once(repo.git_dir.clone()).chain(common)
        })
        .map(|dir| {
            let real = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
            (dir, real)
        })
        .filter(|(_, real)| !real.starts_with(&real_root))
        .collect();
    git_dirs.sort_by(|a, b| a.1.cmp(&b.1));
    git_dirs.dedup_by(|inner, outer| inner.1.starts_with(&outer.1));
    git_dirs.into_iter().map(|(dir, _)| dir).collect()
}
