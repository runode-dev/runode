//! 监听右侧面板在看的仓库或目录，判断哪些文件变动值得重读。

use std::{
    fs,
    path::{Path, PathBuf},
};

use futures::channel::mpsc::UnboundedSender;
use notify::Watcher as _;

use super::state::Project;

/// 右侧面板在看的仓库或目录的监听。
pub(in crate::workspace) struct ProjectWatch {
    pub(super) root: PathBuf,
    /// 不在 `root` 下面的 git 目录（worktree 的，worktree 里子模块的），也要听。
    pub(super) git_dirs: Vec<PathBuf>,
    /// 事件里的路径解析过符号链接，按这两个的真实路径比。
    real_root: PathBuf,
    real_git_dirs: Vec<PathBuf>,
    _watcher: notify::RecommendedWatcher,
}

impl ProjectWatch {
    /// 开始监听，事件里的路径交给 `events`；监听不了时为空，由调用方退回定时重读。
    pub(super) fn new(root: PathBuf, git_dirs: Vec<PathBuf>, events: UnboundedSender<Vec<PathBuf>>) -> Option<Self> {
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event
                && !event.kind.is_access()
            {
                events.unbounded_send(event.paths).ok();
            }
        })
        .inspect_err(|err| tracing::warn!("监听 {} 失败：{err}", root.display()))
        .ok()?;
        for dir in std::iter::once(&root).chain(&git_dirs) {
            if let Err(err) = watcher.watch(dir, notify::RecursiveMode::Recursive) {
                tracing::warn!("监听 {} 失败：{err}", dir.display());
                return None;
            }
        }
        let real_root = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let real_git_dirs = git_dirs.iter().map(|dir| fs::canonicalize(dir).unwrap_or_else(|_| dir.clone())).collect();
        Some(Self { root, git_dirs, real_root, real_git_dirs, _watcher: watcher })
    }

    /// `path` 变了要不要重读。git 目录里只看暂存区、引用这些，写对象和锁文件之后总跟着
    /// 改它们；`root` 下面的 git 目录（主仓库的 `.git`、子模块在 `.git/modules` 下的、嵌套
    /// 仓库自己的 `.git`）按路径里的 `.git` 认。被忽略的文件只在它所在的目录正显示在文件树里
    /// 时才算，免得编译时产物目录里一直在写，跟着一直重读；忽略与否按文件所在的那个仓库算。
    pub(super) fn affects(&self, path: &Path, project: &Project) -> bool {
        // 文件系统监视的守护进程每次 `git status` 都会在自己的目录里写一下，也不看。
        let git_internal = |rel: &Path| {
            !rel.components().any(|part| part.as_os_str() == "objects" || part.as_os_str() == "fsmonitor--daemon")
                && rel.extension().is_none_or(|ext| ext != "lock")
        };
        if let Some(rel) = self.git_dirs.iter().chain(&self.real_git_dirs).find_map(|dir| path.strip_prefix(dir).ok()) {
            return git_internal(rel);
        }
        let Some(rel) = [&self.root, &self.real_root].into_iter().find_map(|root| path.strip_prefix(root).ok()) else {
            return false;
        };
        if let Some(ix) = rel.components().position(|part| part.as_os_str() == ".git") {
            return git_internal(&rel.components().skip(ix + 1).collect::<PathBuf>());
        }
        let ignored = project.git.as_ref().is_some_and(|git| git.is_ignored(rel));
        !ignored || rel.parent().is_some_and(|parent| project.listings.contains_key(&self.root.join(parent)))
    }
}
