//! 在后台读一次项目：git 状态，以及文件树要显示的目录内容（读目录在 `runode_file_tree::list_dirs`）。

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use runode_file_tree::{Listings, list_dirs};
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
    Scan { dir, root, git, listings, untracked, cost: started.elapsed() }
}
