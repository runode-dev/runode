//! 对项目里一个仓库跑 git：`ProjectRepo` 的方法和 `runode_git::Repo` 的同名同参，本机的直接调；远端的编成
//! `GitOp` 交给那边的 `runode --ssh-project` 跑，结果按 JSON 带回来。

use std::path::{Path, PathBuf};

use runode_git::{
    self as git, Branch, CommitOptions, DiffSide, DiffView, FileDiff, GitError, History, HunkAction, ReadOptions,
    Stash, TooLarge,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{
    ProjectHost, client,
    wire::{Call, Reply},
};

/// 远端的 git 操作最多等这么久：拉取、推送要走网络。
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// 项目所在机器上的一个仓库。
#[derive(Clone)]
pub(crate) struct ProjectRepo {
    host: ProjectHost,
    repo: git::Repo,
    /// 仓库的根目录，路径按项目所在的机器算。
    pub(crate) root: PathBuf,
}

/// 远端要跑的一个 git 操作，字段是 `runode_git::Repo` 同名方法的参数。
#[derive(Debug, Serialize, Deserialize)]
pub(super) enum GitOp {
    Branches,
    Checkout { branch: Branch },
    CreateBranch { name: String, start: Option<String> },
    History { limit: usize },
    CheckoutDetached { id: String },
    Stage { paths: Vec<PathBuf> },
    Unstage { paths: Vec<PathBuf> },
    Discard { files: Vec<FileDiff> },
    StageAll,
    UnstageAll,
    Commit { message: String, options: CommitOptions },
    PendingDiff { staged: bool },
    RecentMessages { limit: usize },
    UndoLastCommit,
    Fetch,
    FetchUnattended,
    Pull,
    Push,
    Sync,
    Stash { message: Option<String>, include_untracked: bool },
    StashApply { stash: Stash },
    StashPop { stash: Stash },
    StashDrop { stash: Stash },
    ApplyHunk { file: FileDiff, hunk: usize, action: HunkAction },
    RemoveWorktree { worktree: PathBuf, force: bool },
    FileView { path: PathBuf, old_path: Option<PathBuf>, side: DiffSide },
    NewBytes { path: PathBuf, side: DiffSide, options: ReadOptions },
    OldBytes { path: PathBuf, old_path: Option<PathBuf>, side: DiffSide, options: ReadOptions },
}

impl GitOp {
    /// 在远端对 `repo` 跑，结果编成 JSON。
    pub(super) fn run(self, repo: &git::Repo) -> serde_json::Result<serde_json::Value> {
        use serde_json::to_value as json;
        match self {
            Self::Branches => json(repo.branches()),
            Self::Checkout { branch } => json(repo.checkout(&branch)),
            Self::CreateBranch { name, start } => json(repo.create_branch(&name, start.as_deref())),
            Self::History { limit } => json(repo.history(limit)),
            Self::CheckoutDetached { id } => json(repo.checkout_detached(&id)),
            Self::Stage { paths } => json(repo.stage(&paths)),
            Self::Unstage { paths } => json(repo.unstage(&paths)),
            Self::Discard { files } => json(repo.discard(&files)),
            Self::StageAll => json(repo.stage_all()),
            Self::UnstageAll => json(repo.unstage_all()),
            Self::Commit { message, options } => json(repo.commit(&message, options)),
            Self::PendingDiff { staged } => json(repo.pending_diff(staged)),
            Self::RecentMessages { limit } => json(repo.recent_messages(limit)),
            Self::UndoLastCommit => json(repo.undo_last_commit()),
            Self::Fetch => json(repo.fetch()),
            Self::FetchUnattended => json(repo.fetch_unattended()),
            Self::Pull => json(repo.pull()),
            Self::Push => json(repo.push()),
            Self::Sync => json(repo.sync()),
            Self::Stash { message, include_untracked } => json(repo.stash(message.as_deref(), include_untracked)),
            Self::StashApply { stash } => json(repo.stash_apply(&stash)),
            Self::StashPop { stash } => json(repo.stash_pop(&stash)),
            Self::StashDrop { stash } => json(repo.stash_drop(&stash)),
            Self::ApplyHunk { file, hunk, action } => json(repo.apply_hunk(&file, hunk, action)),
            Self::RemoveWorktree { worktree, force } => json(repo.remove_worktree(&worktree, force)),
            Self::FileView { path, old_path, side } => json(repo.file_view(&path, old_path.as_deref(), &side)),
            Self::NewBytes { path, side, options } => json(repo.new_bytes(&path, &side, options)),
            Self::OldBytes { path, old_path, side, options } => {
                json(repo.old_bytes(&path, old_path.as_deref(), &side, options))
            }
        }
    }
}

/// 本机直接调 `$local`；远端发 `$op`，没办成时交回 `$failed(原因)`。
macro_rules! dispatch {
    ($self:ident, $local:expr, $op:expr, $failed:expr) => {
        match &$self.host {
            ProjectHost::Local => $local,
            ProjectHost::Remote(client) => remote(client, &$self.root, $op).unwrap_or_else($failed),
        }
    };
}

/// git 操作没办成：原因当作 git 的报错。
fn git_failed<T>(message: String) -> git::Result<T> {
    Err(GitError { message })
}

fn remote<T: DeserializeOwned>(client: &client::Client, root: &Path, op: GitOp) -> Result<T, String> {
    match client.call(Call::Git { root: root.to_path_buf(), op }, GIT_TIMEOUT)? {
        (Reply::Git(value), _) => serde_json::from_value(value).map_err(|err| err.to_string()),
        (other, _) => Err(super::unexpected(&other)),
    }
}

impl ProjectRepo {
    pub(crate) fn new(host: ProjectHost, repo: git::Repo) -> Self {
        Self { root: repo.root.clone(), host, repo }
    }

    pub(crate) fn is_remote(&self) -> bool {
        self.host.is_remote()
    }

    /// 读不到时没有分支。
    pub(crate) fn branches(&self) -> Vec<Branch> {
        dispatch!(self, self.repo.branches(), GitOp::Branches, |err| {
            tracing::warn!("cannot list remote branches: {err}");
            Vec::new()
        })
    }

    pub(crate) fn checkout(&self, branch: &Branch) -> git::Result {
        dispatch!(self, self.repo.checkout(branch), GitOp::Checkout { branch: branch.clone() }, git_failed)
    }

    pub(crate) fn create_branch(&self, name: &str, start: Option<&str>) -> git::Result {
        let op = GitOp::CreateBranch { name: name.to_owned(), start: start.map(str::to_owned) };
        dispatch!(self, self.repo.create_branch(name, start), op, git_failed)
    }

    pub(crate) fn history(&self, limit: usize) -> git::Result<History> {
        dispatch!(self, self.repo.history(limit), GitOp::History { limit }, git_failed)
    }

    pub(crate) fn checkout_detached(&self, id: &str) -> git::Result {
        let op = GitOp::CheckoutDetached { id: id.to_owned() };
        dispatch!(self, self.repo.checkout_detached(id), op, git_failed)
    }

    pub(crate) fn stage(&self, paths: &[PathBuf]) -> git::Result {
        dispatch!(self, self.repo.stage(paths), GitOp::Stage { paths: paths.to_vec() }, git_failed)
    }

    pub(crate) fn unstage(&self, paths: &[PathBuf]) -> git::Result {
        dispatch!(self, self.repo.unstage(paths), GitOp::Unstage { paths: paths.to_vec() }, git_failed)
    }

    pub(crate) fn discard(&self, files: &[FileDiff]) -> git::Result {
        dispatch!(self, self.repo.discard(files), GitOp::Discard { files: files.to_vec() }, git_failed)
    }

    pub(crate) fn stage_all(&self) -> git::Result {
        dispatch!(self, self.repo.stage_all(), GitOp::StageAll, git_failed)
    }

    pub(crate) fn unstage_all(&self) -> git::Result {
        dispatch!(self, self.repo.unstage_all(), GitOp::UnstageAll, git_failed)
    }

    pub(crate) fn commit(&self, message: &str, options: CommitOptions) -> git::Result {
        let op = GitOp::Commit { message: message.to_owned(), options };
        dispatch!(self, self.repo.commit(message, options), op, git_failed)
    }

    pub(crate) fn pending_diff(&self, staged: bool) -> git::Result<String> {
        dispatch!(self, self.repo.pending_diff(staged), GitOp::PendingDiff { staged }, git_failed)
    }

    pub(crate) fn recent_messages(&self, limit: usize) -> git::Result<Vec<String>> {
        dispatch!(self, self.repo.recent_messages(limit), GitOp::RecentMessages { limit }, git_failed)
    }

    pub(crate) fn undo_last_commit(&self) -> git::Result<String> {
        dispatch!(self, self.repo.undo_last_commit(), GitOp::UndoLastCommit, git_failed)
    }

    pub(crate) fn fetch(&self) -> git::Result {
        dispatch!(self, self.repo.fetch(), GitOp::Fetch, git_failed)
    }

    pub(crate) fn fetch_unattended(&self) -> git::Result {
        dispatch!(self, self.repo.fetch_unattended(), GitOp::FetchUnattended, git_failed)
    }

    pub(crate) fn pull(&self) -> git::Result {
        dispatch!(self, self.repo.pull(), GitOp::Pull, git_failed)
    }

    pub(crate) fn push(&self) -> git::Result {
        dispatch!(self, self.repo.push(), GitOp::Push, git_failed)
    }

    pub(crate) fn sync(&self) -> git::Result {
        dispatch!(self, self.repo.sync(), GitOp::Sync, git_failed)
    }

    pub(crate) fn stash(&self, message: Option<&str>, include_untracked: bool) -> git::Result {
        let op = GitOp::Stash { message: message.map(str::to_owned), include_untracked };
        dispatch!(self, self.repo.stash(message, include_untracked), op, git_failed)
    }

    pub(crate) fn stash_apply(&self, stash: &Stash) -> git::Result {
        dispatch!(self, self.repo.stash_apply(stash), GitOp::StashApply { stash: stash.clone() }, git_failed)
    }

    pub(crate) fn stash_pop(&self, stash: &Stash) -> git::Result {
        dispatch!(self, self.repo.stash_pop(stash), GitOp::StashPop { stash: stash.clone() }, git_failed)
    }

    pub(crate) fn stash_drop(&self, stash: &Stash) -> git::Result {
        dispatch!(self, self.repo.stash_drop(stash), GitOp::StashDrop { stash: stash.clone() }, git_failed)
    }

    pub(crate) fn apply_hunk(&self, file: &FileDiff, hunk: usize, action: HunkAction) -> git::Result {
        let op = GitOp::ApplyHunk { file: file.clone(), hunk, action };
        dispatch!(self, self.repo.apply_hunk(file, hunk, action), op, git_failed)
    }

    pub(crate) fn remove_worktree(&self, worktree: &Path, force: bool) -> git::Result {
        let op = GitOp::RemoveWorktree { worktree: worktree.to_path_buf(), force };
        dispatch!(self, self.repo.remove_worktree(worktree, force), op, git_failed)
    }

    pub(crate) fn file_view(
        &self,
        path: &Path,
        old_path: Option<&Path>,
        side: &DiffSide,
    ) -> git::Result<Option<DiffView>> {
        let op =
            GitOp::FileView { path: path.to_path_buf(), old_path: old_path.map(Path::to_path_buf), side: side.clone() };
        dispatch!(self, self.repo.file_view(path, old_path, side), op, git_failed)
    }

    /// 远端读不到时当作太大，不显示。
    pub(crate) fn new_bytes(
        &self,
        path: &Path,
        side: &DiffSide,
        options: ReadOptions,
    ) -> Result<Option<Vec<u8>>, TooLarge> {
        let op = GitOp::NewBytes { path: path.to_path_buf(), side: side.clone(), options };
        dispatch!(self, self.repo.new_bytes(path, side, options), op, |_| Err(TooLarge))
    }

    /// 远端读不到时当作太大，不显示。
    pub(crate) fn old_bytes(
        &self,
        path: &Path,
        old_path: Option<&Path>,
        side: &DiffSide,
        options: ReadOptions,
    ) -> Result<Option<Vec<u8>>, TooLarge> {
        let op = GitOp::OldBytes {
            path: path.to_path_buf(),
            old_path: old_path.map(Path::to_path_buf),
            side: side.clone(),
            options,
        };
        dispatch!(self, self.repo.old_bytes(path, old_path, side, options), op, |_| Err(TooLarge))
    }
}
