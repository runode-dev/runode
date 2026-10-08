//! 侧栏每个 workspace 行上的仓库信息：当前分支，以及 `origin` 在 GitHub 上时 owner 的头像。

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime},
};

use gpui::{AppContext, Context, SharedString};

use super::super::WindowView;

/// 隔多久重新问一次各 workspace 的分支。
const BRANCH_INTERVAL: Duration = Duration::from_secs(5);
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

impl WindowView {
    /// 隔 `BRANCH_INTERVAL` 在后台问一次每个 workspace 的分支，头像每次运行只找一次。
    pub(in crate::window) fn refresh_repo_badges(&mut self, cx: &mut Context<Self>) {
        for workspace in &mut self.workspaces {
            let badge = &mut workspace.repo;
            if badge.checking || badge.checked.is_some_and(|at| at.elapsed() < BRANCH_INTERVAL) {
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

/// 下到旁边的临时文件里再改名，下到一半时界面读不到半张图。
fn download_avatar(owner: &str, file: &Path) {
    let partial = file.with_extension("part");
    let downloaded = Command::new("curl")
        .args(["-sfL", "--create-dirs", "--max-time", AVATAR_TIMEOUT_SECS, "-o"])
        .arg(&partial)
        .arg(format!("https://github.com/{owner}.png?size=64"))
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
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
    use super::github_owner;

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
