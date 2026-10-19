//! 监听右侧面板在看的仓库或目录，判断哪些文件变动值得重读。

use std::{
    fs,
    path::{Path, PathBuf},
};

use futures::channel::mpsc::UnboundedSender;
use notify::Watcher as _;
use runode_git as git;

/// 监听到的一批改动，见 `changed_paths`。
#[derive(Debug, Default, PartialEq)]
pub(in crate::window) struct ProjectChanges {
    /// 变了的路径。
    pub(in crate::window) paths: Vec<PathBuf>,
    /// 监听出了错、要求整个重扫：说不准哪里变了，按路径筛的（预览的文件、分支和 tag、命令文件）都要当作
    /// 变了，不能只看 `paths`。
    pub(in crate::window) everything: bool,
}

impl ProjectChanges {
    /// 并上后到的一批。
    pub(in crate::window) fn merge(&mut self, more: Self) {
        self.paths.extend(more.paths);
        self.everything |= more.everything;
    }
}

/// 右侧面板在看的仓库或目录的监听。
pub(in crate::window) struct ProjectWatch {
    pub(super) root: PathBuf,
    /// 不在 `root` 下面的 git 目录（worktree 的，worktree 里子模块的），也要听。
    pub(super) git_dirs: Vec<PathBuf>,
    /// 事件里的路径解析过符号链接，按这两个的真实路径比。
    real_root: PathBuf,
    real_git_dirs: Vec<PathBuf>,
    _watcher: notify::RecommendedWatcher,
}

impl ProjectWatch {
    /// 开始监听，事件里的路径交给 `events`（见 `changed_paths`）；监听不了时为空，由调用方退回定时重读。
    pub(super) fn new(root: PathBuf, git_dirs: Vec<PathBuf>, events: UnboundedSender<ProjectChanges>) -> Option<Self> {
        let everything: Vec<PathBuf> = std::iter::once(&root).chain(&git_dirs).cloned().collect();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Some(changes) = changed_paths(event, &everything) {
                events.unbounded_send(changes).ok();
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

    /// `path` 变了要不要重读。git 目录里只看暂存区、引用这些（写对象和锁文件之后总跟着改它们）；
    /// 被忽略的文件只在它所在的目录正显示在文件树里（`listed`，见 `FileTree::is_listed`）时才算，免得
    /// 产物目录一直在写、跟着一直重读；忽略与否按文件所在的那个仓库（`git`）算，不在 git 仓库里时所有
    /// 文件都按被忽略的算。
    pub(super) fn affects(&self, path: &Path, git: Option<&git::Repos>, listed: &dyn Fn(&Path) -> bool) -> bool {
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
        let ignored = git.is_none_or(|git| git.is_ignored(rel));
        !ignored || rel.parent().is_some_and(|parent| listed(&self.root.join(parent)))
    }
}

/// 监听到的事件里变了的路径；只是读了文件的为空。出错（比如事件太多丢了一些）或者要求整个重扫时，
/// 说不准哪里变了，标上 `ProjectChanges::everything`，路径是 `everything`（根目录和要听的 git 目录），重读一遍。
fn changed_paths(event: notify::Result<notify::Event>, everything: &[PathBuf]) -> Option<ProjectChanges> {
    let all = || ProjectChanges { paths: everything.to_vec(), everything: true };
    match event {
        Ok(event) if event.need_rescan() => Some(all()),
        Ok(event) if event.kind.is_access() => None,
        Ok(event) => Some(ProjectChanges { paths: event.paths, everything: false }),
        Err(err) => {
            tracing::debug!("监听出错，整个重读：{err}");
            Some(all())
        }
    }
}

#[cfg(test)]
mod tests {
    use notify::{
        Event, EventKind,
        event::{AccessKind, Flag, ModifyKind},
    };

    use super::*;

    /// 丢了事件、要求重扫时不会因为没有路径就什么都不读。
    #[test]
    fn errors_and_rescans_count_as_everything_changed() {
        let everything = [PathBuf::from("/repo"), PathBuf::from("/main/.git/worktrees/repo")];
        let modified = Event::new(EventKind::Modify(ModifyKind::Any)).add_path("/repo/a.rs".into());
        let file = ProjectChanges { paths: vec![PathBuf::from("/repo/a.rs")], everything: false };
        assert_eq!(changed_paths(Ok(modified), &everything), Some(file));
        let read = Event::new(EventKind::Access(AccessKind::Any)).add_path("/repo/a.rs".into());
        assert_eq!(changed_paths(Ok(read), &everything), None);
        let all = ProjectChanges { paths: everything.to_vec(), everything: true };
        let rescan = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert_eq!(changed_paths(Ok(rescan), &everything).as_ref(), Some(&all));
        let failed = Err(notify::Error::generic("dropped events"));
        assert_eq!(changed_paths(failed, &everything).as_ref(), Some(&all));
    }

    /// 不在仓库里时只有文件树正显示着的目录里的改动才算，`.git` 照旧不看文件树。
    #[test]
    fn non_repo_changes_count_only_in_listed_dirs() {
        let root = std::env::temp_dir();
        let watch = ProjectWatch::new(root.clone(), vec![], futures::channel::mpsc::unbounded().0).unwrap();
        let only_root = |dir: &Path| dir == root;
        assert!(watch.affects(&root.join("a.txt"), None, &only_root));
        assert!(!watch.affects(&root.join("sub/b.txt"), None, &only_root));
        assert!(watch.affects(&root.join(".git/index"), None, &only_root));
        let with_sub = |dir: &Path| dir == root || dir == root.join("sub");
        assert!(watch.affects(&root.join("sub/b.txt"), None, &with_sub));
    }

    /// 防抖时并起来的几批里有一批要求整个重读，并完也要整个重读。
    #[test]
    fn merged_changes_keep_the_rescan() {
        let mut changes = ProjectChanges { paths: vec![PathBuf::from("/repo/a.rs")], everything: false };
        changes.merge(ProjectChanges { paths: vec![PathBuf::from("/repo")], everything: true });
        changes.merge(ProjectChanges { paths: vec![PathBuf::from("/repo/b.rs")], everything: false });
        assert!(changes.everything);
        assert_eq!(changes.paths, ["/repo/a.rs", "/repo", "/repo/b.rs"].map(PathBuf::from));
    }
}
