//! 任务分支上的 PR：看板开着时隔 `POLL` 经用户自己装的 gh 读一次各任务分支上的 PR，卡片上显示；PR 合并了的
//! 任务标成完成，宿主随后照常收掉它的终端、工作树和分支。还没有 PR 的可以把分支推上去开一个
//! （`Kanban::create_pull_request`）。gh 没装、没登录、仓库不在 GitHub 上时读不到，
//! 卡片上就不显示，不报错。

use std::{collections::HashMap, ffi::OsString, path::PathBuf, time::Duration};

use gpui::{Context, Task};
use runode_git::Repo;
use runode_protocol::todo::Todo;

use super::{Kanban, ToastState, store};
use crate::github_actions::gh::{Gh, PullRequest};

/// 隔多久重读一次。
const POLL: Duration = Duration::from_secs(60);

/// 各任务（按标识）分支上的 PR。
pub(super) type Pulls = HashMap<String, PullRequest>;

/// 有分支的任务：在任务的目录里问 gh 这个分支上的 PR，仓库由 gh 按远端认。
fn target(todo: &Todo) -> Option<(String, PathBuf, String)> {
    Some((todo.id.clone(), todo.spec.dir.clone(), todo.branch.clone()?))
}

/// 一直隔 `POLL` 读一次，看板丢掉时随返回的任务一起停。读不到的保留上次的。
pub(super) fn poll(cx: &mut Context<Kanban>) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            let Ok((targets, path)) = this.update(cx, |this, cx| {
                let targets: Vec<_> = store::data(cx).todos.iter().filter_map(target).collect();
                (targets, this.input.shell_path.clone())
            }) else {
                return;
            };
            let found = cx.background_executor().spawn(async move { fetch(targets, path) }).await;
            let updated = this.update(cx, |this, cx| {
                for (id, found) in found {
                    match found {
                        Ok(Some(pr)) => {
                            if pr.state == "MERGED" {
                                this.finish_merged(&id, cx);
                            }
                            this.pulls.insert(id, pr);
                        }
                        Ok(None) => {
                            this.pulls.remove(&id);
                        }
                        Err(err) => tracing::debug!("cannot read the pull request of todo {id}: {err}"),
                    }
                }
                cx.notify();
            });
            if updated.is_err() {
                return;
            }
            cx.background_executor().timer(POLL).await;
        }
    })
}

type Found = Vec<(String, Result<Option<PullRequest>, String>)>;

fn fetch(targets: Vec<(String, PathBuf, String)>, path: Option<OsString>) -> Found {
    targets
        .into_iter()
        .map(|(id, dir, branch)| {
            let gh = Gh { dir, path: path.clone() };
            (id, gh.pull_request(&branch))
        })
        .collect()
}

/// 在任务的工作树里推送分支（还没设上游时推到同名的远端分支），再开 PR，返回开出的 PR。
fn create(worktree: PathBuf, branch: String, path: Option<OsString>) -> Result<Option<PullRequest>, String> {
    let repo = Repo::open(&worktree).ok_or_else(|| format!("{} is not in a git repository", worktree.display()))?;
    repo.push().map_err(|err| err.to_string())?;
    let gh = Gh { dir: worktree, path };
    gh.create_pull_request(&branch)?;
    gh.pull_request(&branch)
}

impl Kanban {
    /// PR 合并了：还没完成的任务标成完成。每个任务在这个看板里只标一次，用户取消完成后不再标回去。
    fn finish_merged(&mut self, id: &str, cx: &mut Context<Self>) {
        let open = store::data(cx).todos.iter().any(|todo| todo.id == id && todo.done_at.is_none());
        if open && self.merged.insert(id.to_owned()) {
            self.finish_todo(id.to_owned(), true, cx);
        }
    }

    /// 把任务的分支推上去开 PR，顶上的提示条说办没办成。只给还有工作树和分支的任务。
    pub(super) fn create_pull_request(&mut self, todo: &Todo, cx: &mut Context<Self>) {
        let (Some(worktree), Some(branch)) = (todo.worktree_dir.clone(), todo.branch.clone()) else {
            return;
        };
        let (id, path) = (todo.id.clone(), self.input.shell_path.clone());
        let serial = self.show_toast(ToastState::Creating, cx);
        let created = cx.background_executor().spawn(async move { create(worktree, branch, path) });
        cx.spawn(async move |this, cx| {
            let created = created.await;
            this.update(cx, |this, cx| match created {
                Ok(pr) => {
                    if let Some(pr) = pr {
                        this.pulls.insert(id, pr);
                    }
                    this.set_toast(serial, None, cx);
                }
                Err(reason) => {
                    let text = rust_i18n::t!("kanban.toast.pr_failed", reason = reason).into_owned();
                    this.show_toast(ToastState::Failed(text), cx);
                }
            })
            .ok();
        })
        .detach();
    }
}
