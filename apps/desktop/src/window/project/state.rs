//! 一个 workspace 的项目状态：读的是哪个目录、git 状态、后台读的调度，以及换上后台读到的结果。文件树
//! 自己的状态（展开的目录、选中的行）在 `file_tree` 里，Git 面板的（排好的行、提交说明框）在
//! `git_panel` 里。

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::file_tree::TreeScan;
use runode_git as git;
use runode_protocol::TaskSource;

use super::scan::Scan;
use crate::window::model::Workspace;

/// 一个 workspace 的改动和读项目的调度。
#[derive(Default)]
pub(in crate::window) struct Project {
    /// 文件树的根目录，跟着当前终端的目录变；还没读过时为空，读到过一次之后就一直有。
    pub root: Option<PathBuf>,
    /// 上次读的是哪个目录，终端换了目录时据此让文件树定位过去。
    pub(super) dir: Option<PathBuf>,
    /// 最近一次读到的 git 状态：主仓库以及它里面的子模块和嵌套仓库；不在 git 仓库里时为空。和预览栏、文件树
    /// 共用一份，变了才换一份新的。
    pub git: Option<Arc<git::Repos>>,
    /// 当前目录在不在 git 仓库里，没问过时为空；面板都收着时只靠它决定显不显示 Git 按钮。
    pub(super) in_repo: Option<bool>,
    /// 上次问 `in_repo` 的目录和时刻，以及后台是不是正在问。
    pub(super) probed: Option<(PathBuf, Instant)>,
    pub(super) probing: bool,
    /// 后台正在读，读完之前不再发起。
    pub(super) refreshing: bool,
    /// 监听到了改动还没重读，或者读的时候又有了改动。
    pub(super) stale: bool,
    /// 上次开始读的时刻，以及读了多久，据此决定下次隔多久。
    pub(super) refreshed_at: Option<Instant>,
    pub(super) scan_cost: Duration,
    /// 标题栏命令菜单里的命令（Makefile 的目标、package.json 的 scripts）和给哪个目录列的；还没列过时
    /// 为空。后台正在列时不再发起；Makefile、package.json 这类文件变了时标成要重列。
    pub tasks: Option<(PathBuf, Vec<TaskSource>)>,
    pub tasks_listing: bool,
    pub tasks_stale: bool,
    /// 命令菜单里收起来的分组，按列出命令的文件记。
    pub tasks_folded: HashSet<PathBuf>,
    /// 命令菜单里上次跑的命令行，再打开菜单时选中它，回车就再跑一次；记在窗口存档里，重启后还在。
    pub tasks_last: Option<String>,
    /// 交给后台读的时候拿走，读完放回来。
    pub(super) untracked: git::UntrackedCache,
}

impl Workspace {
    /// 换上后台读到的结果：窗口这边有变化时第一项为真；第二项是交给文件树（`FileTree::apply_scan`）的那份，
    /// 终端换了目录时带上新目录，文件树定位过去；`started` 是发起这次读时取的 `read_ticket`。
    pub(super) fn apply_scan(&mut self, scan: Scan, started: u64) -> (bool, TreeScan) {
        let project = &mut self.project;
        // 第一次读时 `root` 还是空的，下面换上根目录时一定算作有变化。
        let mut changed = false;
        project.untracked = scan.untracked;
        project.in_repo = Some(scan.git.is_some());
        project.probed = Some((scan.dir.clone(), Instant::now()));
        if project.root.as_ref() != Some(&scan.root) {
            project.root = Some(scan.root.clone());
            changed = true;
        }
        if project.git.as_deref() != scan.git.as_ref() {
            project.git = scan.git.map(Arc::new);
            changed = true;
        }
        let reveal = (project.dir.as_ref() != Some(&scan.dir)).then(|| scan.dir.clone());
        if reveal.is_some() {
            project.dir = Some(scan.dir);
            changed = true;
        }
        let tree = TreeScan { root: scan.root, git: project.git.clone(), listings: scan.listings, reveal, started };
        (changed, tree)
    }
}
