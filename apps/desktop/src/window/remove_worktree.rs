//! 关掉开在 runode 建的工作树（主工作树下 `runode_git::WORKTREES_DIR` 里的一级）上的工作区时，问要不要连工作树
//! 一起删掉：删的是工作树的目录，分支已经合进主工作树当前分支的一起删掉，没合并的留着。工作树里有改动或没跟踪的
//! 文件时 git 不让删，再问一次要不要强制删、把它们一起丢掉。

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    time::Duration,
};

use futures::channel::oneshot;
use gpui::{Context, PromptButton, PromptLevel, Window, prelude::*};

use super::{WindowView, new_worktree::worktree_dir_name};
use crate::{host_client::hosts::HostId, project_host::ProjectHost};

/// 删工作树最多试这么多次，每次之前等 `REMOVE_RETRY_DELAY`。
const REMOVE_ATTEMPTS: usize = 6;
const REMOVE_RETRY_DELAY: Duration = Duration::from_millis(300);

/// 关工作区时能一起删的工作树。
pub(super) struct ClosingWorktree {
    host: HostId,
    main: PathBuf,
    worktree: PathBuf,
    /// 工作树上检出的、新建它时拉出的那个分支：新建对话框建的目录名按分支取（`worktree_dir_name`），看板任务建的
    /// 分支是 `runode_git::TASK_BRANCH_PREFIX` 加目录名。后来切到了别的分支时为空，那个分支不跟着删。
    branch: Option<String>,
}

/// 分支 `branch` 是新建目录名是 `dir_name` 的工作树时拉出的。
fn created_with(dir_name: &OsStr, branch: &str) -> bool {
    *dir_name == *worktree_dir_name(branch)
        || branch.strip_prefix(runode_git::TASK_BRANCH_PREFIX).is_some_and(|rest| *dir_name == *rest)
}

impl WindowView {
    /// 第 `ix` 个 workspace 开在 runode 建的工作树的根上、而且没有别的 workspace 开在这个工作树里时，关它时能一起
    /// 删的工作树。
    pub(super) fn closing_worktree(&self, ix: usize) -> Option<ClosingWorktree> {
        let workspace = &self.workspaces[ix];
        let main = workspace.repo.main_worktree.clone()?;
        if workspace.dir.parent() != Some(main.join(runode_git::WORKTREES_DIR).as_path()) {
            return None;
        }
        (!self.worktree_in_use(&workspace.host, &workspace.dir, Some(ix))).then(|| ClosingWorktree {
            host: workspace.host.clone(),
            main,
            worktree: workspace.dir.clone(),
            branch: workspace
                .repo
                .branch
                .as_ref()
                .map(|branch| branch.to_string())
                .filter(|branch| workspace.dir.file_name().is_some_and(|name| created_with(name, branch))),
        })
    }

    /// 主机 `host` 上除了第 `except` 个以外，有 workspace 开在 `worktree` 或它下面的目录里。
    fn worktree_in_use(&self, host: &HostId, worktree: &Path, except: Option<usize>) -> bool {
        self.workspaces
            .iter()
            .enumerate()
            .any(|(ix, w)| Some(ix) != except && w.host == *host && w.dir.starts_with(worktree))
    }

    /// 问关不关开在 `worktree` 上的工作区：标题是 `title`，`detail` 是关它本来要说的话，后面说明能连工作树一起删。
    /// 选了删除时做 `close` 再删工作树，选了只关闭时只做 `close`。
    pub(super) fn confirm_close_worktree(
        &mut self,
        title: &str,
        detail: Option<String>,
        worktree: ClosingWorktree,
        window: &mut Window,
        cx: &mut Context<Self>,
        close: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let hint =
            rust_i18n::t!("worktree.remove_hint", dir = worktree.host.display_dir(&worktree.worktree)).into_owned();
        let detail = match detail {
            Some(detail) => format!("{detail}\n\n{hint}"),
            None => hint,
        };
        let answer = crate::prompt::show(
            window,
            PromptLevel::Warning,
            title,
            Some(&detail),
            &[
                PromptButton::new(rust_i18n::t!("worktree.remove_and_close")),
                PromptButton::new(rust_i18n::t!("worktree.close_only")),
                PromptButton::cancel(rust_i18n::t!("close.cancel")),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let remove = match answer.await {
                Ok(0) => true,
                Ok(1) => false,
                _ => return,
            };
            this.update_in(cx, |this, window, cx| {
                close(this, window, cx);
                // 问的时候可能又有 workspace 开在了这个工作树里。
                if remove && !this.worktree_in_use(&worktree.host, &worktree.worktree, None) {
                    this.remove_worktree(worktree, false, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 在后台删掉工作树，接着删它已经合并的分支；`force` 时连改动一起删。删不掉时弹框说原因，没强制过的问要不要
    /// 强制删。
    fn remove_worktree(&mut self, worktree: ClosingWorktree, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let repo = ProjectHost::of(&worktree.host).repo(runode_git::Repo::new(worktree.main.clone()));
        let (dir, branch) = (worktree.worktree.clone(), worktree.branch.clone());
        // 删的任务单独跑完、结果经通道交回：关的是最后一个工作区时窗口跟着关，等结果的那头没了也照样删。
        let (done, result) = oneshot::channel();
        let executor = cx.background_executor().clone();
        cx.background_spawn(async move {
            // 刚关的工作区里的 shell 是宿主异步结束的，还没退出时占着目录（Windows 上删不掉），先等一下，删不掉再
            // 隔一会儿重试，都不行才算删不掉。
            let mut attempts = 0;
            let removed = loop {
                executor.timer(REMOVE_RETRY_DELAY).await;
                attempts += 1;
                let removed = repo.remove_worktree(&dir, force);
                if removed.is_ok() || attempts == REMOVE_ATTEMPTS {
                    break removed;
                }
            };
            // 有没合并的提交时分支删不掉，留着，不算失败。
            if removed.is_ok()
                && let Some(branch) = branch
                && let Err(err) = repo.delete_merged_branch(&branch)
            {
                tracing::debug!("kept branch {branch} of the removed worktree: {err}");
            }
            drop(done.send(removed));
        })
        .detach();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Err(err)) = result.await else { return };
            this.update_in(cx, |this, window, cx| {
                this.report_remove_failed(worktree, force, &crate::git_view::git_error_text(&err), window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn report_remove_failed(
        &mut self,
        worktree: ClosingWorktree,
        forced: bool,
        error: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = rust_i18n::t!("worktree.remove_failed");
        if forced {
            let answer = crate::prompt::show(
                window,
                PromptLevel::Warning,
                &title,
                Some(error),
                &[PromptButton::cancel(rust_i18n::t!("worktree.ok"))],
                cx,
            );
            // 只说一声，但收的那头要留到框关掉。
            cx.foreground_executor().spawn(answer).detach();
            return;
        }
        let detail = format!("{error}\n\n{}", rust_i18n::t!("worktree.remove_force_hint"));
        let answer = crate::prompt::show(
            window,
            PromptLevel::Warning,
            &title,
            Some(&detail),
            &[
                PromptButton::new(rust_i18n::t!("worktree.remove_force")),
                PromptButton::cancel(rust_i18n::t!("close.cancel")),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            this.update_in(cx, |this, window, cx| this.remove_worktree(worktree, true, window, cx)).ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 新建对话框和看板任务各自拉出的分支认得出来，后来切过去的别的分支不算。
    #[test]
    fn recognizes_the_branch_a_worktree_was_created_with() {
        assert!(created_with(OsStr::new("feat-login"), "feat/login"));
        assert!(created_with(OsStr::new("fix"), "runode/fix"));
        assert!(!created_with(OsStr::new("fix"), "main"));
        assert!(!created_with(OsStr::new("fix"), "other/fix"));
    }
}
