//! Git 面板的操作：在后台跑 git，跑的时候面板顶上写着在做什么，跑完马上重读，出错弹框。
//! 丢掉改动、删除储藏这类拿不回来的操作先问一下。

use std::path::PathBuf;

use gpui::{Context, PromptLevel, Window, prelude::*};
use runode_git_status::{self as git, CommitOptions, FileDiff, HunkAction, Section};

use super::{
    FileOp, GitCheckout, GitCommit, GitCommitAmend, GitCommitAndPush, GitCommitAndSync, GitCreateBranch, GitDiscardAll,
    GitFetch, GitFileAction, GitPull, GitPush, GitRefresh, GitStageAll, GitStash, GitStashIncludeUntracked,
    GitStashPopLatest, GitSync, GitUndoLastCommit, GitUnstageAll,
    rows::{Busy, GitPanel, GitSection},
};
use crate::workspace::WindowView;

/// 提交成功以后接着做什么。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AfterCommit {
    Nothing,
    Push,
    Sync,
}

/// 弹框里的错误说明最多这么多个字符，git 偶尔会输出很长的一串。
const MAX_ERROR_CHARS: usize = 2000;

impl WindowView {
    /// 当前 workspace 的仓库；不在仓库里或者上一个操作还没跑完时为空。
    fn git_repo(&self) -> Option<git::Repo> {
        let project = &self.workspace().project;
        if project.git_panel.busy.is_some() {
            return None;
        }
        project.git.as_ref().map(git::Snapshot::repo)
    }

    /// 在后台对当前 workspace 的仓库跑 `op`，跑的时候面板上写着 `busy`。跑完马上重读，成功时
    /// 再调 `done`，出错时弹框。
    fn run_git<T: Send + 'static>(
        &mut self,
        busy: Busy,
        window: &mut Window,
        cx: &mut Context<Self>,
        op: impl FnOnce(&git::Repo) -> git::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(repo) = self.git_repo() else {
            return;
        };
        let id = self.workspace().id;
        self.workspace_mut().project.git_panel.busy = Some(busy);
        cx.notify();
        let job = cx.background_spawn(async move { op(&repo) });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                // 跑的时候 workspace 可能已经关掉了；切走了的等切回来时再读。
                let Some(workspace) = this.workspaces.iter_mut().find(|workspace| workspace.id == id) else {
                    return;
                };
                workspace.project.git_panel.busy = None;
                if this.workspace().id == id {
                    this.refresh_project(cx);
                }
                match result {
                    Ok(value) => done(this, value, window, cx),
                    Err(err) => show_git_error(&err, window, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 先问一下，答「确定」才调 `then`。
    fn confirm_git(
        &mut self,
        title: String,
        detail: String,
        confirm: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let answer =
            window.prompt(PromptLevel::Warning, &title, Some(&detail), &[&confirm, &*rust_i18n::t!("git.cancel")], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update_in(cx, |this, window, cx| then(this, window, cx)).ok();
            }
        })
        .detach();
    }

    /// `section` 那一段里路径是 `path` 的文件。
    fn git_file(&self, section: Section, path: &PathBuf) -> Option<FileDiff> {
        let git = self.workspace().project.git.as_ref()?;
        git.files(section).iter().find(|file| &file.path == path).cloned()
    }

    /// 文件的右键菜单和行尾按钮：打开、在访达中显示、打进终端、暂存、取消暂存、丢掉改动。
    pub(super) fn git_file_action(&mut self, action: &GitFileAction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.workspace().project.git.as_ref().map(|git| git.root.clone()) else {
            return;
        };
        let full = root.join(&action.path);
        let paths = vec![action.path.clone()];
        match action.op {
            FileOp::Open => self.open_preview(&full, true, cx),
            FileOp::Reveal => cx.reveal_path(&full),
            FileOp::InsertPath => self.insert_path(&full, None, window, cx),
            FileOp::Stage => self.run_git(Busy::Stage, window, cx, move |repo| repo.stage(&paths), |_, (), _, _| {}),
            FileOp::Unstage => {
                self.run_git(Busy::Stage, window, cx, move |repo| repo.unstage(&paths), |_, (), _, _| {});
            }
            FileOp::Discard => {
                let Some(file) = self.git_file(action.section, &action.path) else {
                    return;
                };
                self.discard_files(vec![file], window, cx);
            }
        }
    }

    /// 丢掉未暂存段里这些文件的改动，先问一下；未跟踪的文件会被删掉。
    fn discard_files(&mut self, files: Vec<FileDiff>, window: &mut Window, cx: &mut Context<Self>) {
        let untracked = files.iter().filter(|file| file.status == git::FileStatus::Untracked).count();
        let (title, detail) = match files.as_slice() {
            [] => return,
            [file] => {
                let name = file
                    .path
                    .file_name()
                    .map_or_else(|| file.path.display().to_string(), |name| name.to_string_lossy().into_owned());
                if untracked == 1 {
                    (
                        rust_i18n::t!("git.delete_untracked_title", name = name),
                        rust_i18n::t!("git.delete_untracked_detail"),
                    )
                } else {
                    (rust_i18n::t!("git.discard_title", name = name), rust_i18n::t!("git.irreversible"))
                }
            }
            files if untracked > 0 => (
                rust_i18n::t!("git.discard_all_title", count = files.len()),
                rust_i18n::t!("git.discard_all_untracked_detail", count = untracked),
            ),
            files => (rust_i18n::t!("git.discard_all_title", count = files.len()), rust_i18n::t!("git.irreversible")),
        };
        let confirm = rust_i18n::t!("git.discard_confirm").into_owned();
        self.confirm_git(title.into_owned(), detail.into_owned(), confirm, window, cx, move |this, window, cx| {
            this.run_git(Busy::Discard, window, cx, move |repo| repo.discard(&files), |_, (), _, _| {});
        });
    }

    /// 整段的操作：暂存全部冲突或未暂存的文件、取消暂存全部、丢掉全部未暂存的改动。
    pub(super) fn git_section_action(
        &mut self,
        section: GitSection,
        stage: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(git) = self.workspace().project.git.as_ref() else {
            return;
        };
        let source = section.source();
        let files: Vec<FileDiff> =
            GitPanel::files(git, section).into_iter().map(|fi| git.files(source)[fi].clone()).collect();
        match (section, stage) {
            (GitSection::Staged, _) => {
                self.run_git(Busy::Stage, window, cx, git::Repo::unstage_all, |_, (), _, _| {});
            }
            (GitSection::Merge | GitSection::Unstaged, true) => {
                let paths: Vec<PathBuf> = files.into_iter().map(|file| file.path).collect();
                self.run_git(Busy::Stage, window, cx, move |repo| repo.stage(&paths), |_, (), _, _| {});
            }
            (GitSection::Unstaged, false) => self.discard_files(files, window, cx),
            _ => {}
        }
    }

    /// 改动块上的按钮：暂存、取消暂存或丢掉这一块。
    pub(super) fn git_hunk_action(
        &mut self,
        section: Section,
        path: PathBuf,
        hunk: usize,
        action: HunkAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(file) = self.git_file(section, &path) else {
            return;
        };
        let busy = if action == HunkAction::Discard { Busy::Discard } else { Busy::Stage };
        self.run_git(busy, window, cx, move |repo| repo.apply_hunk(&file, hunk, action), |_, (), _, _| {});
    }

    /// 储藏那一段的按钮：应用、弹出，或者问过以后删掉。
    pub(super) fn git_stash_action(&mut self, index: usize, op: StashOp, window: &mut Window, cx: &mut Context<Self>) {
        match op {
            StashOp::Apply => {
                self.run_git(Busy::Stash, window, cx, move |repo| repo.stash_apply(index), |_, (), _, _| {});
            }
            StashOp::Pop => self.run_git(Busy::Stash, window, cx, move |repo| repo.stash_pop(index), |_, (), _, _| {}),
            StashOp::Drop => {
                let title = rust_i18n::t!("git.drop_stash_title", index = index).into_owned();
                let detail = rust_i18n::t!("git.irreversible").into_owned();
                let confirm = rust_i18n::t!("git.drop_stash_confirm").into_owned();
                self.confirm_git(title, detail, confirm, window, cx, move |this, window, cx| {
                    this.run_git(Busy::Stash, window, cx, move |repo| repo.stash_drop(index), |_, (), _, _| {});
                });
            }
        }
    }

    /// 提交说明框里的文字，去掉末尾的空白。
    fn commit_message(&self, cx: &Context<Self>) -> String {
        let panel = &self.workspace().project.git_panel;
        panel.commit_box.as_ref().map(|area| area.read(cx).text().trim_end().to_owned()).unwrap_or_default()
    }

    /// 提交。没写说明时不提交（改上次提交除外，沿用原来的说明）；没有暂存的改动时问一下要不要
    /// 暂存全部再提交。
    fn commit(&mut self, amend: bool, after: AfterCommit, window: &mut Window, cx: &mut Context<Self>) {
        let message = self.commit_message(cx);
        let Some(git) = self.workspace().project.git.as_ref() else {
            return;
        };
        if self.workspace().project.git_panel.busy.is_some() || (message.trim().is_empty() && !amend) {
            return;
        }
        if git.staged.is_empty() && !amend {
            if git.unstaged.is_empty() {
                return;
            }
            let answer = window.prompt(
                PromptLevel::Info,
                &rust_i18n::t!("git.stage_all_title"),
                Some(&rust_i18n::t!("git.stage_all_detail")),
                &[&*rust_i18n::t!("git.stage_all_confirm"), &*rust_i18n::t!("git.cancel")],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                if answer.await.ok() == Some(0) {
                    this.update_in(cx, |this, window, cx| {
                        let options = CommitOptions { amend, stage_all: true };
                        this.run_commit(message, options, after, window, cx);
                    })
                    .ok();
                }
            })
            .detach();
            return;
        }
        self.run_commit(message, CommitOptions { amend, stage_all: false }, after, window, cx);
    }

    /// 提交成功就清空说明框；接着推送或同步失败时提交已经做了，说明框照样清空，再弹框说推送的错。
    fn run_commit(
        &mut self,
        message: String,
        options: CommitOptions,
        after: AfterCommit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let op = move |repo: &git::Repo| {
            repo.commit(&message, options)?;
            Ok(match after {
                AfterCommit::Nothing => None,
                AfterCommit::Push => repo.push().err(),
                AfterCommit::Sync => repo.sync().err(),
            })
        };
        self.run_git(Busy::Commit, window, cx, op, |this, remote_error, window, cx| {
            if let Some(area) = &this.workspace().project.git_panel.commit_box {
                area.update(cx, |area, cx| area.set_text(String::new(), cx));
            }
            if let Some(err) = remote_error {
                show_git_error(&err, window, cx);
            }
        });
    }

    pub(super) fn git_commit(&mut self, _: &GitCommit, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(false, AfterCommit::Nothing, window, cx);
    }

    pub(super) fn git_commit_amend(&mut self, _: &GitCommitAmend, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(true, AfterCommit::Nothing, window, cx);
    }

    pub(super) fn git_commit_and_push(&mut self, _: &GitCommitAndPush, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(false, AfterCommit::Push, window, cx);
    }

    pub(super) fn git_commit_and_sync(&mut self, _: &GitCommitAndSync, window: &mut Window, cx: &mut Context<Self>) {
        self.commit(false, AfterCommit::Sync, window, cx);
    }

    /// 撤销上次提交，改动留在暂存区；说明框空着时把那次的说明放回去，改改再提交。
    pub(super) fn git_undo_last_commit(&mut self, _: &GitUndoLastCommit, window: &mut Window, cx: &mut Context<Self>) {
        self.run_git(Busy::Commit, window, cx, git::Repo::undo_last_commit, |this, message, _, cx| {
            if let Some(area) = &this.workspace().project.git_panel.commit_box
                && area.read(cx).text().trim().is_empty()
            {
                area.update(cx, |area, cx| area.set_text(message.trim_end().to_owned(), cx));
            }
        });
    }

    pub(super) fn git_refresh(&mut self, _: &GitRefresh, _: &mut Window, cx: &mut Context<Self>) {
        self.refresh_project(cx);
    }

    pub(super) fn git_fetch(&mut self, _: &GitFetch, window: &mut Window, cx: &mut Context<Self>) {
        self.run_git(Busy::Fetch, window, cx, git::Repo::fetch, |_, (), _, _| {});
    }

    pub(super) fn git_pull(&mut self, _: &GitPull, window: &mut Window, cx: &mut Context<Self>) {
        self.run_git(Busy::Pull, window, cx, git::Repo::pull, |_, (), _, _| {});
    }

    /// 推送；还没有上游分支时顺带发布到远端。
    pub(super) fn git_push(&mut self, _: &GitPush, window: &mut Window, cx: &mut Context<Self>) {
        self.run_git(Busy::Push, window, cx, git::Repo::push, |_, (), _, _| {});
    }

    pub(super) fn git_sync(&mut self, _: &GitSync, window: &mut Window, cx: &mut Context<Self>) {
        self.run_git(Busy::Sync, window, cx, git::Repo::sync, |_, (), _, _| {});
    }

    pub(super) fn git_stage_all(&mut self, _: &GitStageAll, window: &mut Window, cx: &mut Context<Self>) {
        self.run_git(Busy::Stage, window, cx, git::Repo::stage_all, |_, (), _, _| {});
    }

    pub(super) fn git_unstage_all(&mut self, _: &GitUnstageAll, window: &mut Window, cx: &mut Context<Self>) {
        self.run_git(Busy::Stage, window, cx, git::Repo::unstage_all, |_, (), _, _| {});
    }

    pub(super) fn git_discard_all(&mut self, _: &GitDiscardAll, window: &mut Window, cx: &mut Context<Self>) {
        self.git_section_action(GitSection::Unstaged, false, window, cx);
    }

    pub(super) fn git_stash(&mut self, _: &GitStash, window: &mut Window, cx: &mut Context<Self>) {
        self.run_git(Busy::Stash, window, cx, |repo| repo.stash(None, false), |_, (), _, _| {});
    }

    pub(super) fn git_stash_include_untracked(
        &mut self,
        _: &GitStashIncludeUntracked,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_git(Busy::Stash, window, cx, |repo| repo.stash(None, true), |_, (), _, _| {});
    }

    pub(super) fn git_stash_pop_latest(&mut self, _: &GitStashPopLatest, window: &mut Window, cx: &mut Context<Self>) {
        self.git_stash_action(0, StashOp::Pop, window, cx);
    }

    pub(super) fn git_checkout(&mut self, _: &GitCheckout, window: &mut Window, cx: &mut Context<Self>) {
        self.open_branch_picker(false, window, cx);
    }

    pub(super) fn git_create_branch(&mut self, _: &GitCreateBranch, window: &mut Window, cx: &mut Context<Self>) {
        self.open_branch_picker(true, window, cx);
    }

    /// 切到分支列表里选中的分支。
    pub(super) fn checkout_branch(&mut self, branch: git::Branch, window: &mut Window, cx: &mut Context<Self>) {
        self.run_git(Busy::Checkout, window, cx, move |repo| repo.checkout(&branch), |_, (), _, _| {});
    }

    /// 从当前提交新建分支并切过去。
    pub(super) fn create_branch(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.run_git(Busy::Checkout, window, cx, move |repo| repo.create_branch(&name), |_, (), _, _| {});
    }
}

/// 储藏那一段每行上的操作。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StashOp {
    Apply,
    Pop,
    Drop,
}

/// git 报的错：只有一个按钮，不用等回答。
fn show_git_error(err: &git::GitError, window: &mut Window, cx: &mut Context<WindowView>) {
    let mut detail: String = err.message.chars().take(MAX_ERROR_CHARS).collect();
    if detail.len() < err.message.len() {
        detail.push('…');
    }
    drop(window.prompt(
        PromptLevel::Warning,
        &rust_i18n::t!("git.failed"),
        Some(&detail),
        &[&*rust_i18n::t!("git.ok")],
        cx,
    ));
}
