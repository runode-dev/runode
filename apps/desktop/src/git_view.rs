//! 文件树、Git 面板和预览栏都要画的 git 状态：按状态上的颜色，以及认出 app 交来的是不是同一份状态。

use std::{collections::HashMap, path::Path, sync::Arc};

use gpui::{App, AppContext as _, Global, Hsla};
use runode_git::{FileStatus, GitError, Refusal, Repos, Snapshot};
use runode_types::color::Rgb;

use crate::ui::theme::{ADDED, MODIFIED, REMOVED, RENAMED, Theme};

/// 文件名、标签名和状态字母按 git 状态上的颜色。
pub(crate) fn status_color(status: FileStatus) -> Rgb {
    match status {
        FileStatus::Modified => MODIFIED,
        FileStatus::Added | FileStatus::Untracked => ADDED,
        FileStatus::Deleted | FileStatus::Conflicted => REMOVED,
        FileStatus::Renamed => RENAMED,
    }
}

/// 按主题的状态色给 git 状态上色：改过的是 `info`，新加和未跟踪的是 `success`，删掉和冲突的是
/// `destructive`。
pub(crate) fn status_tint(status: FileStatus, theme: &Theme) -> Hsla {
    match status {
        FileStatus::Modified | FileStatus::Renamed => theme.info,
        FileStatus::Added | FileStatus::Untracked => theme.success,
        FileStatus::Deleted | FileStatus::Conflicted => theme.destructive,
    }
}

/// 主仓库的 `repo_change_summary`。
pub(crate) fn change_summary(repos: &Repos) -> (usize, usize, usize) {
    repo_change_summary(&repos.main)
}

/// 仓库 `repo` 里有改动的文件数和相对 HEAD 一共增删的行数。部分暂存的文件两段里都有，只算一个，行数取
/// `Snapshot::head_stats` 里的，那里没有时才把两段的相加。只算已读好的状态，每次画都会调。
pub(crate) fn repo_change_summary(repo: &Snapshot) -> (usize, usize, usize) {
    let mut stats: HashMap<&Path, (usize, usize)> = HashMap::new();
    for file in repo.staged.iter().chain(&repo.unstaged) {
        let stat = stats.entry(&file.path).or_default();
        stat.0 += file.added;
        stat.1 += file.removed;
    }
    for (path, head) in &repo.head_stats {
        if let Some(stat) = stats.get_mut(path.as_path()) {
            *stat = *head;
        }
    }
    let (added, removed) = stats.values().fold((0, 0), |(added, removed), stat| (added + stat.0, removed + stat.1));
    (stats.len(), added, removed)
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

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
    };

    use runode_git::{FileDiff, RepoInfo, RepoKind, Snapshot};

    use super::*;

    fn file(path: &str, status: FileStatus, added: usize, removed: usize) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status,
            added,
            removed,
            hunks: Vec::new(),
            binary: false,
            truncated: false,
            gitlink: false,
        }
    }

    fn repo(staged: Vec<FileDiff>, unstaged: Vec<FileDiff>, head_stats: &[(&str, (usize, usize))]) -> Snapshot {
        Snapshot {
            root: PathBuf::from("/repo"),
            git_dir: PathBuf::from("/repo/.git"),
            prefix: PathBuf::new(),
            kind: RepoKind::Main,
            staged,
            unstaged,
            statuses: HashMap::new(),
            ignored: HashSet::new(),
            head_stats: head_stats.iter().map(|(path, stat)| (PathBuf::from(path), *stat)).collect(),
            info: RepoInfo::default(),
        }
    }

    #[test]
    fn change_summary_counts_partly_staged_files_once() {
        let staged = vec![file("a.rs", FileStatus::Modified, 2, 1), file("b.rs", FileStatus::Added, 5, 0)];
        let unstaged = vec![file("a.rs", FileStatus::Modified, 3, 0), file("new.txt", FileStatus::Untracked, 4, 0)];
        // 部分暂存的 a.rs 相对 HEAD 是 +4 −1，不是两段相加的 +5 −1。
        let repos = Repos::new(repo(staged, unstaged, &[("a.rs", (4, 1))]));
        assert_eq!(change_summary(&repos), (3, 4 + 5 + 4, 1));
    }

    #[test]
    fn change_summary_adds_both_sections_without_head_stats() {
        let staged = vec![file("a.rs", FileStatus::Modified, 2, 1)];
        let unstaged = vec![file("a.rs", FileStatus::Modified, 3, 2)];
        let repos = Repos::new(repo(staged, unstaged, &[]));
        assert_eq!(change_summary(&repos), (1, 5, 3));
    }

    #[test]
    fn change_summary_of_a_clean_repo_is_empty() {
        let repos = Repos::new(repo(Vec::new(), Vec::new(), &[("stale.rs", (1, 1))]));
        assert_eq!(change_summary(&repos), (0, 0, 0));
    }
}
