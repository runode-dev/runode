//! 文件树、Git 面板和预览栏都要画的 git 状态：按状态上的颜色，以及认出 app 交来的是不是同一份状态。

use std::sync::Arc;

use gpui::{App, AppContext as _, Global};
use runode_git::{FileStatus, GitError, Refusal, Repos};
use runode_types::color::Rgb;

use crate::ui::theme::{ADDED, MODIFIED, REMOVED, RENAMED};

/// 文件名、标签名和状态字母按 git 状态上的颜色。
pub(crate) fn status_color(status: FileStatus) -> Rgb {
    match status {
        FileStatus::Modified => MODIFIED,
        FileStatus::Added | FileStatus::Untracked => ADDED,
        FileStatus::Deleted | FileStatus::Conflicted => REMOVED,
        FileStatus::Renamed => RENAMED,
    }
}

/// 两次交来的 git 状态是同一份：app 只在状态变了时才换一份新的，所以按是不是同一个 `Arc` 比。
pub(crate) fn same_repos(old: &Option<Arc<Repos>>, new: &Option<Arc<Repos>>) -> bool {
    match (old, new) {
        (Some(old), Some(new)) => Arc::ptr_eq(old, new),
        (old, new) => old.is_none() && new.is_none(),
    }
}

/// 按界面的语言写出 git 操作没办成的原因；git 自己的报错和别处写好的原样显示。
pub(crate) fn git_error_text(err: &GitError) -> String {
    let path = |path: &std::path::Path| path.display().to_string();
    let text = match err {
        GitError::Git(message) | GitError::Other(message) => return message.clone(),
        GitError::Exited(status) => rust_i18n::t!("git.error.exited", status = status),
        GitError::Spawn(error) => rust_i18n::t!("git.error.spawn", error = error),
        GitError::Timeout { secs, lock: None } => rust_i18n::t!("git.error.timeout", secs = secs),
        GitError::Timeout { secs, lock: Some(lock) } => {
            rust_i18n::t!("git.error.timeout_lock", secs = secs, lock = path(lock))
        }
        GitError::Io { path: at, error } => rust_i18n::t!("git.error.io", path = path(at), error = error),
        GitError::Refused(refusal) => match refusal {
            Refusal::NotBranch(name) => rust_i18n::t!("git.error.not_branch", name = name),
            Refusal::UnknownRemote(branch) => rust_i18n::t!("git.error.unknown_remote", branch = branch),
            Refusal::NotCommit(id) => rust_i18n::t!("git.error.not_commit", id = id),
            Refusal::OutsideRepo(at) => rust_i18n::t!("git.error.outside_repo", path = path(at)),
            Refusal::NotHunkable(at) => rust_i18n::t!("git.error.not_hunkable", path = path(at)),
            Refusal::NoSuchHunk => rust_i18n::t!("git.error.no_such_hunk"),
            Refusal::HunkTruncated(at) => rust_i18n::t!("git.error.hunk_truncated", path = path(at)),
            Refusal::HunkChanged(at) => rust_i18n::t!("git.error.hunk_changed", path = path(at)),
            Refusal::DiffUnreadable(at) => rust_i18n::t!("git.error.diff_unreadable", path = path(at)),
            Refusal::StatusUnreadable => rust_i18n::t!("git.error.status_unreadable"),
            Refusal::StatusChanged(at) => rust_i18n::t!("git.error.status_changed", path = path(at)),
            Refusal::Conflicted(at) => rust_i18n::t!("git.error.conflicted", path = path(at)),
            Refusal::UnresolvedConflicts => rust_i18n::t!("git.error.unresolved_conflicts"),
            Refusal::NewlineInPath(at) => rust_i18n::t!("git.error.newline_in_path", path = format!("{at:?}")),
            Refusal::NoExcludeFile => rust_i18n::t!("git.error.no_exclude_file"),
            Refusal::BranchNotUtf8 => rust_i18n::t!("git.error.branch_not_utf8"),
            Refusal::NoCommits => rust_i18n::t!("git.error.no_commits"),
            Refusal::Merging => rust_i18n::t!("git.error.merging"),
            Refusal::Sequencing => rust_i18n::t!("git.error.sequencing"),
            Refusal::SquashPending => rust_i18n::t!("git.error.squash_pending"),
            Refusal::ParentMissing => rust_i18n::t!("git.error.parent_missing"),
            Refusal::DetachedFirstCommit => rust_i18n::t!("git.error.detached_first_commit"),
            Refusal::HeadMoved => rust_i18n::t!("git.error.head_moved"),
            Refusal::HeadMovedRestored => rust_i18n::t!("git.error.head_moved_restored"),
            Refusal::NoRemote => rust_i18n::t!("git.error.no_remote"),
            Refusal::StashChanged(rev) => rust_i18n::t!("git.error.stash_changed", rev = rev),
            _ => return refusal.to_string(),
        },
        _ => return err.to_string(),
    };
    text.into_owned()
}

/// 本机的 git 跑不跑得起来，`check_git_installed` 查过以后才有。
struct GitInstalled(bool);

impl Global for GitInstalled {}

/// 在后台查本机的 git 装没装，查完调 `done`。
pub(crate) fn check_git_installed(cx: &mut App, done: impl FnOnce(&mut App) + 'static) {
    let job = cx.background_spawn(async { runode_git::installed() });
    cx.spawn(async move |cx| {
        let installed = job.await;
        cx.update(|cx| {
            cx.set_global(GitInstalled(installed));
            done(cx);
        });
    })
    .detach();
}

/// 查过、本机的 git 跑不起来。
pub(crate) fn git_missing(cx: &App) -> bool {
    cx.try_global::<GitInstalled>().is_some_and(|installed| !installed.0)
}

/// 不在仓库里时面板上的话：`local` 的仓库而本机的 git 跑不起来时说先装 git，不然看着像是目录不对。
pub(crate) fn not_repo_text(local: bool, cx: &App) -> String {
    if local && git_missing(cx) {
        return crate::i18n::tr(&crate::i18n::os_key("panel.git_missing"));
    }
    rust_i18n::t!("panel.not_repo").into_owned()
}
