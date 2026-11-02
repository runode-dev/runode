//! 监听右侧面板在看的仓库或目录，判断哪些文件变动值得重读。

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use futures::channel::mpsc::UnboundedSender;
use runode_git as git;

use crate::{host_client::hosts::HostId, project_host::ProjectHost};

pub(in crate::window) use runode_files::Changes as ProjectChanges;

/// 右侧面板在看的仓库或目录的监听。
pub(in crate::window) struct ProjectWatch {
    pub(super) host: HostId,
    pub(super) root: PathBuf,
    /// 不在 `root` 下面的 git 目录（worktree 的，worktree 里子模块的），也要听。
    pub(super) git_dirs: Vec<PathBuf>,
    /// 事件里的路径解析过符号链接，按这两个的真实路径比。
    real_root: PathBuf,
    real_git_dirs: Vec<PathBuf>,
    source: Source,
}

/// 听着的那一个，丢掉就停。
enum Source {
    Local { _watch: runode_files::Watch },
    Remote(RemoteWatch),
}

impl ProjectWatch {
    /// 开始监听 `host` 上的项目，事件里的路径交给 `events`；监听不了时为空，由调用方退回定时重读。会阻塞。
    pub(super) fn new(
        host: HostId,
        root: PathBuf,
        git_dirs: Vec<PathBuf>,
        events: UnboundedSender<ProjectChanges>,
    ) -> Option<Self> {
        let dirs: Vec<PathBuf> = std::iter::once(&root).chain(&git_dirs).cloned().collect();
        if host != HostId::Local {
            let source = Source::Remote(RemoteWatch::start(ProjectHost::of(&host), dirs, events)?);
            // 远端交回的路径就按这里给的写法。
            let (real_root, real_git_dirs) = (root.clone(), git_dirs.clone());
            return Some(Self { host, root, git_dirs, real_root, real_git_dirs, source });
        }
        let watch = runode_files::watch(dirs, move |changes| {
            events.unbounded_send(changes).ok();
        })?;
        let real_root = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        let real_git_dirs = git_dirs.iter().map(|dir| fs::canonicalize(dir).unwrap_or_else(|_| dir.clone())).collect();
        Some(Self { host, root, git_dirs, real_root, real_git_dirs, source: Source::Local { _watch: watch } })
    }

    /// 还在听：远端的连接断了时不再听，由调用方重建，建好之前退回定时重读。
    pub(super) fn alive(&self) -> bool {
        match &self.source {
            Source::Local { .. } => true,
            Source::Remote(remote) => remote.alive.load(Ordering::Relaxed),
        }
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

/// 远端项目的监听：一个线程一直问远端攒下的改动（`ProjectHost::wait_for_changes`），丢掉就停。
struct RemoteWatch {
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
}

impl RemoteWatch {
    /// 远端开始听了才交回；远端听不了、连不上时为空。
    fn start(host: ProjectHost, dirs: Vec<PathBuf>, events: UnboundedSender<ProjectChanges>) -> Option<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let token = NEXT.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        if let Err(err) = host.wait_for_changes(token, dirs.clone(), &stop) {
            tracing::warn!("cannot watch {:?} on {:?}: {err}", dirs.first(), host.name());
            return None;
        }
        let alive = Arc::new(AtomicBool::new(true));
        let watch = Self { stop: stop.clone(), alive: alive.clone() };
        let poll = move || {
            while !stop.load(Ordering::Relaxed) {
                let changes = match host.wait_for_changes(token, dirs.clone(), &stop) {
                    Ok(Some(changes)) if changes.paths.is_empty() && !changes.everything => continue,
                    Ok(Some(changes)) => changes,
                    // 远端换了一个进程、重新开始听：之间的改动听不到，整个重读。
                    Ok(None) => ProjectChanges { paths: dirs.clone(), everything: true },
                    Err(err) => {
                        if !stop.load(Ordering::Relaxed) {
                            tracing::info!("stopped watching {:?} on {:?}: {err}", dirs.first(), host.name());
                        }
                        break;
                    }
                };
                if events.unbounded_send(changes).is_err() {
                    break;
                }
            }
            alive.store(false, Ordering::Relaxed);
        };
        std::thread::Builder::new().name("project-watch-remote".into()).spawn(poll).ok()?;
        Some(watch)
    }
}

impl Drop for RemoteWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 不在仓库里时只有文件树正显示着的目录里的改动才算，`.git` 照旧不看文件树。
    #[test]
    fn non_repo_changes_count_only_in_listed_dirs() {
        let root = std::env::temp_dir();
        let watch =
            ProjectWatch::new(HostId::Local, root.clone(), vec![], futures::channel::mpsc::unbounded().0).unwrap();
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
