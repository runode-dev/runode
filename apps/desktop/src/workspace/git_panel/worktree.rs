//! 同一个仓库的其他工作树在 Git 面板里各占一块，块头的右键菜单和「更多」菜单里多几项：在新的
//! 终端标签里打开它、在访达中显示、删掉它。当前所在的工作树就是主仓库那块，不给删。

use std::path::{Path, PathBuf};

use gpui::{Action, Context, Pixels, Point, PromptLevel, Window};
use runode_git::{self as git, RepoKind};

use super::rows::Busy;
use crate::workspace::{
    WindowView,
    files::{MenuItem, menu_item},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeOp {
    OpenInNewTab,
    Reveal,
    Remove,
}

/// 对根目录是 `worktree` 的那个工作树做 `op`。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct GitWorktreeAction {
    pub worktree: PathBuf,
    pub op: WorktreeOp,
}

impl WindowView {
    /// 根目录是 `root` 的那个其他工作树。
    fn worktree(&self, root: &Path) -> Option<&git::Snapshot> {
        let git = self.workspace().project.git.as_ref()?;
        git.worktrees.iter().find(|wt| wt.root == root && wt.kind == RepoKind::Worktree)
    }

    /// 根目录是 `root` 的仓库是不是另一个工作树。
    pub(super) fn is_worktree(&self, root: &Path) -> bool {
        self.worktree(root).is_some()
    }

    /// 工作树那几项菜单。主工作树删不了（终端在某个链接工作树里时它也作为其他工作树出现），不给
    /// 删除；主仓库有操作在跑时删除按不动。
    pub(super) fn worktree_menu_items(&self, root: &Path, cx: &Context<Self>) -> Vec<Option<MenuItem>> {
        let main = self.workspace().project.git.as_ref().map(|git| git.main.root.clone());
        let enabled = main.is_some_and(|main| self.workspace().project.git_panel.busy(&main).is_none());
        let removable = self.worktree(root).is_some_and(git::Snapshot::is_linked_worktree);
        let item = |key: &str, op, enabled| {
            Some(menu_item(key, Box::new(GitWorktreeAction { worktree: root.to_path_buf(), op }), enabled, cx))
        };
        let mut items = vec![
            item("git.worktree.open_in_new_tab", WorktreeOp::OpenInNewTab, true),
            item("files.reveal", WorktreeOp::Reveal, true),
        ];
        if removable {
            items.push(item("git.worktree.remove", WorktreeOp::Remove, enabled));
        }
        items
    }

    /// 工作树块头的右键菜单。
    pub(super) fn open_worktree_menu(&mut self, root: &Path, position: Point<Pixels>, cx: &mut Context<Self>) {
        let items = self.worktree_menu_items(root, cx);
        let target = self.git_focus.clone();
        self.open_menu(position, items, target, cx);
    }

    pub(super) fn git_worktree_action(
        &mut self,
        action: &GitWorktreeAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = action.worktree.clone();
        match action.op {
            WorktreeOp::OpenInNewTab => {
                if let Some(view) = self.spawn_terminal(Some(&path), window, cx) {
                    self.insert_tab(self.workspace().active + 1, view, window, cx);
                }
            }
            WorktreeOp::Reveal => cx.reveal_path(&path),
            WorktreeOp::Remove => self.remove_worktree(path, window, cx),
        }
    }

    /// 删掉工作树，先问一下。有改动（含未跟踪的文件）时 git 不让删，说明这些改动会一起丢掉，
    /// 确认了才强制删。在主仓库上跑，主仓库那块写着忙。
    fn remove_worktree(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(git) = self.workspace().project.git.as_ref() else {
            return;
        };
        let Some(worktree) = git.worktrees.iter().find(|wt| wt.root == path) else {
            return;
        };
        if !worktree.is_linked_worktree() {
            return;
        }
        let main = git.main.root.clone();
        let dirty = !worktree.is_clean();
        let detached = worktree.info.branch.is_none();
        let name =
            path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
        let title = rust_i18n::t!("git.worktree.remove_title", name = name).into_owned();
        // 游离 HEAD 的工作树没有分支留下来，只在它这里的提交删了以后就找不到了。
        let detail = match (dirty, detached) {
            (false, false) => rust_i18n::t!("git.worktree.remove_detail").into_owned(),
            (false, true) => rust_i18n::t!("git.worktree.remove_detached_detail").into_owned(),
            (true, false) => rust_i18n::t!("git.worktree.remove_dirty_detail").into_owned(),
            (true, true) => format!(
                "{} {}",
                rust_i18n::t!("git.worktree.remove_dirty_detail"),
                rust_i18n::t!("git.worktree.remove_detached_detail")
            ),
        };
        let confirm = if dirty {
            rust_i18n::t!("git.worktree.remove_force")
        } else {
            rust_i18n::t!("git.worktree.remove_confirm")
        };
        let answer =
            window.prompt(PromptLevel::Warning, &title, Some(&detail), &[&confirm, &*rust_i18n::t!("git.cancel")], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update_in(cx, |this, window, cx| {
                    let op = move |repo: &git::Repo| repo.remove_worktree(&path, dirty);
                    this.run_git(&main, Busy::RemoveWorktree, window, cx, op, |_, (), _, _| {});
                })
                .ok();
            }
        })
        .detach();
    }
}
