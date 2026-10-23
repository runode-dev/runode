//! 侧栏每个 workspace 行上的仓库信息：当前分支，以及 `origin` 在 GitHub 上时 owner 的头像。

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime},
};

use gpui::{AppContext, Context, SharedString};

use super::super::WindowView;

/// 隔多久重新问一次当前 workspace 的分支。
const BRANCH_INTERVAL: Duration = Duration::from_secs(5);
/// 别的 workspace 隔多久问一次：每个都按 `BRANCH_INTERVAL` 起 git 太多。切过去以后按 `BRANCH_INTERVAL` 算，
/// 下一轮就问。
const BACKGROUND_BRANCH_INTERVAL: Duration = Duration::from_secs(60);
/// 缓存的头像过了这么久再下载一次，owner 换了头像也跟得上。
const AVATAR_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// 下载头像最多等多久，没网时不一直占着后台线程。
const AVATAR_TIMEOUT_SECS: &str = "10";

#[derive(Default)]
pub(in crate::window) struct RepoBadge {
    /// 当前分支，HEAD 分离时是短哈希；不在 git 仓库里时为空。
    pub branch: Option<SharedString>,
    /// 下载好的 owner 头像。
    pub avatar: Option<PathBuf>,
    checked: Option<Instant>,
    checking: bool,
    /// 这次运行里找过头像了，找没找到都不再找，之后只刷新分支。
    avatar_looked: bool,
}

impl RepoBadge {
    /// 问到过 workspace 目录 `workspace_dir` 在仓库里时，下面的 `dir` 也在（`.git` 里面除外），右侧面板问
    /// 在不在仓库里时不用再起一个 git。
    pub(in crate::window) fn covers_repo_dir(&self, workspace_dir: &Path, dir: &Path) -> bool {
        self.branch.is_some()
            && dir.strip_prefix(workspace_dir).is_ok_and(|rel| !rel.components().any(|part| part.as_os_str() == ".git"))
    }
}

impl WindowView {
    /// 在后台问各 workspace 的分支，当前的隔 `BRANCH_INTERVAL`、别的隔 `BACKGROUND_BRANCH_INTERVAL`；
    /// 头像每次运行只找一次。
    pub(in crate::window) fn refresh_repo_badges(&mut self, cx: &mut Context<Self>) {
        let active = self.active;
        for (ix, workspace) in self.workspaces.iter_mut().enumerate() {
            let badge = &mut workspace.repo;
            let interval = if ix == active { BRANCH_INTERVAL } else { BACKGROUND_BRANCH_INTERVAL };
            if badge.checking || badge.checked.is_some_and(|at| at.elapsed() < interval) {
                continue;
            }
            badge.checking = true;
            let (id, dir, find_avatar) = (workspace.id, workspace.dir.clone(), !badge.avatar_looked);
            let job = cx.background_spawn(async move {
                let branch = runode_git::current_branch(&dir);
                let avatar = (find_avatar && branch.is_some()).then(|| avatar_for(&dir)).flatten();
                (branch, avatar)
            });
            cx.spawn(async move |this, cx| {
                let (branch, avatar) = job.await;
                this.update(cx, |this, cx| {
                    // 问的时候 workspace 可能已经关掉了。
                    let Some(workspace) = this.workspaces.iter_mut().find(|workspace| workspace.id == id) else {
                        return;
                    };
                    let badge = &mut workspace.repo;
                    badge.checking = false;
                    badge.checked = Some(Instant::now());
                    let branch = branch.map(SharedString::from);
                    let changed = badge.branch != branch || (find_avatar && badge.avatar != avatar);
                    badge.branch = branch;
                    if find_avatar {
                        badge.avatar = avatar;
                        badge.avatar_looked = true;
                    }
                    if changed {
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }
}

/// `dir` 所在仓库的 `origin` 在 GitHub 上时，owner 头像的缓存文件；没缓存或缓存旧了先下载。下载失败时
/// 用旧的缓存。
fn avatar_for(dir: &Path) -> Option<PathBuf> {
    let url = runode_git::origin_url(dir)?;
    let owner = github_owner(&url)?;
    let file = runode_paths::Dirs::from_env().github_avatar_file(owner)?;
    let age =
        file.metadata().and_then(|meta| meta.modified()).ok().and_then(|at| SystemTime::now().duration_since(at).ok());
    if age.is_none_or(|age| age > AVATAR_MAX_AGE) {
        download_avatar(owner, &file);
    }
    file.exists().then_some(file)
}

/// 下到旁边的临时文件里再改名，下到一半时界面读不到半张图。同一个 owner 的几个 workspace 会同时下，
/// 临时文件各用各的，不会一个写到一半被另一个截断或者改名拿走。
fn download_avatar(owner: &str, file: &Path) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let partial = file.with_extension(format!("{}-{}.part", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let mut command = Command::new("curl");
    command
        .args(["-sfL", "--create-dirs", "--max-time", AVATAR_TIMEOUT_SECS, "-o"])
        .arg(&partial)
        .arg(format!("https://github.com/{owner}.png?size=64"))
        .stdin(Stdio::null());
    crate::subprocess::hide_window(&mut command);
    let downloaded = command.status().is_ok_and(|status| status.success());
    if !downloaded || std::fs::rename(&partial, file).is_err() {
        let _ = std::fs::remove_file(&partial);
    }
}

/// GitHub 远端地址里的 owner：`git@github.com:owner/repo.git`、`https://github.com/owner/repo`、
/// `ssh://git@github.com/owner/repo.git`。owner 要拼进路径和网址，只认 GitHub 用户名能用的字符。
fn github_owner(url: &str) -> Option<&str> {
    let (before, rest) = url.split_once("github.com")?;
    // 主机名得正好是 github.com，不能是 notgithub.com 这类。
    if !(before.is_empty() || before.ends_with('@') || before.ends_with('/')) {
        return None;
    }
    let owner = rest.strip_prefix(':').or_else(|| rest.strip_prefix('/'))?.split('/').next()?;
    let valid = !owner.is_empty() && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    valid.then_some(owner)
}

#[cfg(test)]
mod tests {
    use super::{Path, RepoBadge, github_owner};

    #[test]
    fn a_repo_badge_covers_dirs_below_the_workspace() {
        let workspace = Path::new("/code/app");
        let mut badge = RepoBadge::default();
        // 还没问到分支，或者不在仓库里：看不出来，要自己问。
        assert!(!badge.covers_repo_dir(workspace, workspace));
        badge.branch = Some("main".into());
        assert!(badge.covers_repo_dir(workspace, workspace));
        assert!(badge.covers_repo_dir(workspace, Path::new("/code/app/src")));
        assert!(!badge.covers_repo_dir(workspace, Path::new("/code/app/.git/refs")));
        assert!(!badge.covers_repo_dir(workspace, Path::new("/code")));
        assert!(!badge.covers_repo_dir(workspace, Path::new("/code/other")));
    }

    #[test]
    fn github_owner_reads_common_remote_urls() {
        for url in [
            "git@github.com:runode-dev/runode.git",
            "https://github.com/runode-dev/runode",
            "https://user@github.com/runode-dev/runode.git",
            "ssh://git@github.com/runode-dev/runode.git",
        ] {
            assert_eq!(github_owner(url), Some("runode-dev"), "{url}");
        }
        for url in ["git@gitlab.com:a/b.git", "https://notgithub.com/a/b", "https://github.com/../b", "github.com"] {
            assert_eq!(github_owner(url), None, "{url}");
        }
    }
}
