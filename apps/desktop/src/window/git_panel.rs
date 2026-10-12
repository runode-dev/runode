//! 右侧的 Git 面板：顶上是当前分支和同步按钮，下面是提交说明框和提交
//! 按钮，再下面按冲突、已暂存、未暂存分段列出改动的文件和储藏。文件和改动块都能暂存、取消暂存
//! 或丢掉，标题栏的「更多」菜单里是拉取、推送、分支和储藏这些操作。
//!
//! 仓库里有子模块或嵌套仓库时，每个仓库一块：块头是可以收起的仓库名和
//! 分支，展开后是这个仓库自己的分支栏、提交说明框和各段改动，仓库的操作挪到块头的「更多」菜单，
//! 标题栏的「更多」只留面板的显示设置（树形式、显示子模块和嵌套仓库、显示其他工作树）。块里的
//! 按钮作用到这块的仓库；菜单和快捷键派发的动作作用到 `WindowView::git_target` 选的仓库。
//!
//! 面板底部单独一栏是提交历史的图表，可以收起，上沿拖动改变高度，见 `graph`。同一个仓库的其他
//! 工作树排在子仓库后面，各占一块，块头的菜单见 `worktree`。
//!
//! 排成行的状态在 `rows`，列表的各行在 `list`，在后台跑 git 在 `run`，切换和新建分支的浮层在
//! `branch_picker`。git 命令本身由 `runode_git::Repo` 去跑。

mod ai_message;
mod branch_picker;
mod graph;
mod list;
mod rows;
mod run;
mod tree;
mod worktree;

/// 文件树的搜索结果以树形式查看时也按这个分层。
pub(super) use tree::{TreeItem, file_tree};

use crate::ui::theme::ActiveTheme as _;
use std::{
    ops::Range,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui::SharedString;
use gpui::{
    Action, Animation, AnimationExt, AnyElement, Context, Div, ElementId, Focusable, Hsla, MouseButton, MouseDownEvent,
    Role, Stateful, Transformation, Window, actions, div, list, percentage, prelude::*, px, svg, uniform_list,
};
use runode_git::{self as git, Operation, RepoKind, Section};
use runode_shared_types::color::Rgb;

use super::{
    TITLEBAR_HEIGHT, WindowView,
    files::{MenuItem, check_item, menu_item},
    model::base_name,
    project::{added_label, panel_message, panel_shell, panel_title, removed_label},
    status_bar,
};
use crate::ui::a11y::{Disable, PressDown};
use crate::ui::menu::MenuHost as _;
use crate::{
    assets::{
        BRANCH_ICON, CHECK_ICON, CHEVRON_DOWN_ICON, GIT_ICON, MORE_ICON, REFRESH_ICON, SPARKLE_ICON, SYNC_ICON,
        VIEW_LIST_ICON, VIEW_TREE_ICON,
    },
    ui::{
        font_size::{FontSize, scaled},
        hsla,
        scrollbar::list_scrollbar,
        text_area::{TextArea, TextAreaEvent},
        tooltip::tooltip,
    },
};
use rows::{GitRow, GitSection};

pub(super) use ai_message::CommitMessageDialog;
pub(super) use branch_picker::BranchPicker;
pub(super) use rows::{Busy, GitPanel};

actions!(
    runode,
    [
        /// 提交暂存的改动；没有暂存的改动时问一下要不要暂存全部再提交。
        GitCommit,
        /// 改上次提交：说明框空着时沿用原来的说明。
        GitCommitAmend,
        GitCommitAndPush,
        GitCommitAndSync,
        /// 打开 AI 写提交说明的对话框，选 agent 和提示词。
        GitGenerateCommitMessage,
        /// 撤销上次提交，改动留在暂存区。
        GitUndoLastCommit,
        GitRefresh,
        GitFetch,
        GitPull,
        GitPush,
        /// 先拉取再推送。
        GitSync,
        GitStageAll,
        GitUnstageAll,
        GitDiscardAll,
        GitStash,
        GitStashIncludeUntracked,
        GitStashPopLatest,
        /// 打开分支列表，切到别的分支。
        GitCheckout,
        GitCreateBranch,
        /// 改动的文件在树形式和列表形式之间切换。
        GitToggleTreeView,
        /// 显示或藏起子模块和嵌套仓库。
        GitToggleSubmodules,
        /// 显示或藏起同一个仓库的其他工作树。
        GitToggleWorktrees
    ]
);

/// 对一个改动的文件做的事。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileOp {
    Open,
    Reveal,
    InsertPath,
    Stage,
    Unstage,
    Discard,
}

/// 文件的右键菜单里的一项：对根目录是 `repo` 的仓库里、`section` 那一段里路径是 `path`
/// （相对那个仓库的根）的文件做 `op`。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct GitFileAction {
    pub repo: PathBuf,
    pub op: FileOp,
    pub section: Section,
    pub path: PathBuf,
}

/// 以树形式查看时对一个目录下所有改动做的事。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirOp {
    Stage,
    Unstage,
    Discard,
}

/// 目录的右键菜单和行尾按钮：对根目录是 `repo` 的仓库里、`section` 那一段里 `dir`（相对仓库根）
/// 下面的所有文件做 `op`。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(in crate::window) struct GitDirAction {
    pub repo: PathBuf,
    pub section: GitSection,
    pub dir: PathBuf,
    pub op: DirOp,
}

/// 提交说明框最少和最多显示几行，再多就在框里滚动。
const COMMIT_BOX_LINES: (usize, usize) = (1, 8);
/// 分支一行和提交按钮的高度。
const BRANCH_BAR_HEIGHT: f32 = 30.;
const COMMIT_BUTTON_HEIGHT: f32 = 26.;
/// 标题栏上图标按钮的边长。
const HEADER_BUTTON_SIZE: f32 = 22.;

/// 提交按钮按下去做什么：提交，没有可提交的改动时换成同步或者发布分支。
#[derive(Clone, Copy, PartialEq, Eq)]
enum PrimaryAction {
    Commit,
    Sync,
    Publish,
}

impl WindowView {
    /// 窗口上 Git 面板的动作：菜单项派发过来，也能绑快捷键。
    pub(super) fn bind_git_actions(window: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        window
            .on_action(cx.listener(Self::git_file_action))
            .on_action(cx.listener(Self::git_dir_action))
            .on_action(cx.listener(Self::git_worktree_action))
            .on_action(cx.listener(Self::git_commit_action))
            .on_action(cx.listener(Self::select_commit_agent))
            .on_action(cx.listener(|this, _: &GitGenerateCommitMessage, window, cx| {
                if let Some(root) = this.git_target(window, cx) {
                    this.open_commit_message_dialog(root, window, cx);
                }
            }))
            .on_action(cx.listener(Self::git_commit))
            .on_action(cx.listener(Self::git_commit_amend))
            .on_action(cx.listener(Self::git_commit_and_push))
            .on_action(cx.listener(Self::git_commit_and_sync))
            .on_action(cx.listener(Self::git_undo_last_commit))
            .on_action(cx.listener(Self::git_refresh))
            .on_action(cx.listener(Self::git_fetch))
            .on_action(cx.listener(Self::git_pull))
            .on_action(cx.listener(Self::git_push))
            .on_action(cx.listener(Self::git_sync))
            .on_action(cx.listener(Self::git_stage_all))
            .on_action(cx.listener(Self::git_unstage_all))
            .on_action(cx.listener(Self::git_discard_all))
            .on_action(cx.listener(Self::git_stash))
            .on_action(cx.listener(Self::git_stash_include_untracked))
            .on_action(cx.listener(Self::git_stash_pop_latest))
            .on_action(cx.listener(Self::git_checkout))
            .on_action(cx.listener(Self::git_create_branch))
            .on_action(cx.listener(Self::git_toggle_tree_view))
            .on_action(cx.listener(|this, _: &GitToggleSubmodules, _, cx| {
                this.git_show_subs = !this.git_show_subs;
                this.save(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &GitToggleWorktrees, _, cx| {
                this.git_show_worktrees = !this.git_show_worktrees;
                this.save(cx);
                // 藏起来时不再读其他工作树，重新显示时要读一遍。
                this.refresh_project(cx);
                cx.notify();
            }))
    }

    /// 根目录是 `root` 的仓库的提交说明框，第一次用时建出来；按 cmd-enter 提交到这个仓库。提示里
    /// 写着当前分支，分支变了才换。
    fn sync_commit_box(&mut self, root: &Path, branch: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.workspaces[self.active].project.git_panel.repo_mut(root);
        if panel.commit_box.is_none() {
            let area = cx.new(|cx| {
                let mut area = TextArea::new(cx);
                area.set_line_limits(COMMIT_BOX_LINES.0, COMMIT_BOX_LINES.1, cx);
                area.set_label(rust_i18n::t!("git.message_label"), cx);
                area
            });
            let target = root.to_path_buf();
            let events =
                cx.subscribe_in(&area, window, move |this, _, event: &TextAreaEvent, window, cx| match event {
                    TextAreaEvent::Submit => this.submit_commit_box(target.clone(), window, cx),
                    TextAreaEvent::Changed => cx.notify(),
                });
            panel.commit_box = Some(area);
            panel.commit_events = Some(events);
        }
        if panel.placeholder_branch.as_ref() != Some(&branch) {
            let text = match &branch {
                Some(branch) => rust_i18n::t!("git.message_placeholder_branch", branch = branch),
                None => rust_i18n::t!("git.message_placeholder"),
            };
            if let Some(area) = &panel.commit_box {
                area.update(cx, |area, cx| area.set_placeholder(text.into_owned().into(), cx));
            }
            panel.placeholder_branch = Some(branch);
        }
    }

    /// 菜单和快捷键派发的动作作用到哪个仓库：提交说明框有焦点的那个，否则最近点过的那块，
    /// 都没有时是主仓库。不在仓库里时为空。
    pub(super) fn git_target(&self, window: &Window, cx: &gpui::App) -> Option<PathBuf> {
        let project = &self.workspace().project;
        let git = project.git.as_ref()?;
        let panel = &project.git_panel;
        let focused = git.iter().find(|repo| {
            panel
                .repos
                .get(&repo.root)
                .and_then(|repo| repo.commit_box.as_ref())
                .is_some_and(|area| area.focus_handle(cx).is_focused(window))
        });
        let active =
            panel.active.as_ref().and_then(|root| git.iter().find(|repo| repo.root == *root && panel.shows(repo)));
        Some(focused.or(active).unwrap_or(&git.main).root.clone())
    }

    /// 记下点的是哪一块的仓库，菜单和快捷键的动作随后作用到它，图表也换成它的。
    fn set_git_active(&mut self, root: &Path) {
        let project = &mut self.workspace_mut().project;
        if project.git_panel.active.as_deref() != Some(root) {
            project.git_panel.active = Some(root.to_path_buf());
            project.git_panel.rebuild(project.git.as_deref());
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_git_panel(
        &mut self,
        width: f32,
        fg: Rgb,
        bg: Rgb,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        // 先跟上窗口的设置，下面按它算显示哪些仓库。
        let view = (self.git_tree, self.git_show_subs, self.git_show_worktrees);
        let project = &mut self.workspaces[self.active].project;
        let panel = &mut project.git_panel;
        if (panel.tree, panel.show_subs, panel.show_worktrees) != view {
            (panel.tree, panel.show_subs, panel.show_worktrees) = view;
            panel.rebuild(project.git.as_deref());
        }
        // 显示着的各个仓库的提交说明框；多个仓库时收起的块不显示说明框。
        let shown: Vec<_> = match &self.workspace().project.git {
            Some(git) => {
                let panel = &self.workspace().project.git_panel;
                let multi = panel.multi(git);
                git.iter()
                    .filter(|repo| panel.shows(repo) && (!multi || panel.repo_expanded(repo)))
                    .map(|repo| (repo.root.clone(), repo.info.branch.clone()))
                    .collect()
            }
            None => Vec::new(),
        };
        // 别处的仓库的说明框空着、也没有操作在跑时不再留着；写了一半的留着，回来时还在。丢掉的
        // 说明框还有焦点的话，按键就没处去了，交给面板。
        let project = &mut self.workspaces[self.active].project;
        let graph_root = project
            .git
            .as_ref()
            .and_then(|git| git.get(project.git_panel.graph_repo(git)))
            .map(|repo| repo.root.clone());
        let panel = &mut project.git_panel;
        panel.width = width;
        panel.a11y = window.is_a11y_active();
        let mut lost_focus = false;
        // 图表显示的仓库的块收着时也留着，读到的历史才在。
        panel.repos.retain(|root, repo| {
            let keep = shown.iter().any(|(shown, _)| shown == root)
                || graph_root.as_ref() == Some(root)
                || repo.busy.is_some()
                || repo.commit_box.as_ref().is_some_and(|area| !area.read(cx).text().is_empty());
            if !keep && repo.commit_box.as_ref().is_some_and(|area| area.focus_handle(cx).is_focused(window)) {
                lost_focus = true;
            }
            keep
        });
        if lost_focus {
            window.focus(&self.git_focus, cx);
        }
        for (root, branch) in shown {
            self.sync_commit_box(&root, branch, window, cx);
        }
        self.sync_graphs(cx);
        let project = &self.workspace().project;
        let panel = &project.git_panel;
        let dim = hsla(fg).opacity(0.5);
        let multi = project.git.as_ref().is_some_and(|git| panel.multi(git));
        let main_root = project.git.as_ref().map(|git| git.main.root.clone());
        // 多个仓库时各块的忙碌状态写在块头上。
        let busy = main_root.as_deref().filter(|_| !multi).and_then(|root| panel.busy(root));
        let title = rust_i18n::t!("git.title").into_owned();
        let header = panel_title()
            .child(
                div()
                    .id("git-title")
                    .role(Role::Heading)
                    .aria_label(title.clone())
                    .flex_none()
                    .text_color(hsla(fg))
                    .child(title.clone()),
            )
            .child(
                div()
                    .id("git-busy")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(dim)
                    .when_some(busy, |status, busy| status.role(Role::Status).aria_label(busy.label()))
                    .children(busy.map(Busy::label)),
            )
            .when_some(main_root, |header, root| {
                let (icon, text) = if self.git_tree {
                    (VIEW_LIST_ICON, rust_i18n::t!("git.view_as_list"))
                } else {
                    (VIEW_TREE_ICON, rust_i18n::t!("git.view_as_tree"))
                };
                header
                    .child(self.header_button(
                        "git-view-mode",
                        icon,
                        Some(text.into_owned()),
                        cx,
                        |this, window, cx| this.git_toggle_tree_view(&GitToggleTreeView, window, cx),
                    ))
                    .child(self.header_button(
                        "git-refresh",
                        REFRESH_ICON,
                        Some(rust_i18n::t!("git.refresh").into_owned()),
                        cx,
                        |this, window, cx| {
                            this.git_refresh(&GitRefresh, window, cx);
                        },
                    ))
                    .child(
                        self.header_button("git-more", MORE_ICON, None, cx, move |this, window, cx| {
                            this.open_git_menu(&root, true, window.mouse_position(), cx);
                        })
                        // 多个仓库时这里只有显示设置，仓库的操作在块头的「更多操作」里。
                        .aria_label(rust_i18n::t!(if multi { "git.view_options" } else { "git.more" }).into_owned()),
                    )
            });
        let body: AnyElement = match &project.git {
            _ if project.root.is_none() => div().flex_1().into_any_element(),
            None => panel_message(rust_i18n::t!("panel.not_repo").into_owned(), cx).into_any_element(),
            Some(_) if multi => div()
                .id("git-repos")
                .role(Role::Tree)
                .aria_label(title)
                .flex_1()
                .min_h_0()
                .relative()
                .child(
                    list(
                        panel.list.clone(),
                        cx.processor(move |this, ix: usize, window, cx| this.render_git_item(ix, fg, bg, window, cx)),
                    )
                    .size_full(),
                )
                .child(list_scrollbar("git-items-scroll", panel.list.clone()))
                .into_any_element(),
            Some(git) => {
                let git = &git.main;
                // 只剩「没有改动」那一行时不当列表画，和别的空状态一样居中说一句。
                let list: AnyElement = if matches!(panel.rows[..], [] | [GitRow::Clean(_)]) {
                    panel_message(rust_i18n::t!("panel.no_changes").into_owned(), cx).into_any_element()
                } else {
                    div()
                        .id("git-changes")
                        .role(Role::Tree)
                        .aria_label(title)
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(
                            uniform_list(
                                "git-rows",
                                panel.rows.len(),
                                cx.processor(move |this, range: Range<usize>, _, cx| {
                                    this.render_git_rows(range, fg, bg, cx)
                                }),
                            )
                            .track_scroll(&panel.scroll)
                            .flex_1(),
                        )
                        .into_any_element()
                };
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.render_branch_bar(0, git, fg, bg, cx))
                    .children(git.info.operation.map(|operation| operation_banner(&git.root, operation, fg)))
                    .child(self.render_commit_area(0, git, fg, bg, window, cx))
                    .child(list)
                    .into_any_element()
            }
        };
        let room = f32::from(window.viewport_size().height) - TITLEBAR_HEIGHT - status_bar::height(cx);
        let graph = project
            .git
            .as_ref()
            .filter(|_| project.root.is_some())
            .map(|git| self.render_graph_pane(git, room, fg, bg, cx));
        panel_shell("git-panel", width, super::cards(cx), cx)
            .track_focus(&self.git_focus)
            .bg(hsla(bg))
            .text_size(FontSize::Body)
            .child(self.render_panel_tabs(cx))
            .child(header)
            .child(body)
            .children(graph)
    }

    /// 多个仓库时列表里的一项：块头是仓库标题连着分支栏和提交说明框，其余和只有一个仓库时的行
    /// 一样。点在哪一块里，菜单和快捷键的动作就作用到那块的仓库。
    fn render_git_item(
        &mut self,
        ix: usize,
        fg: Rgb,
        bg: Rgb,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project = &self.workspace().project;
        let (Some(git), Some(&row)) = (project.git.as_ref(), project.git_panel.rows.get(ix)) else {
            return div().into_any_element();
        };
        let Some(repo) = git.get(row.repo()) else {
            return div().into_any_element();
        };
        let root = repo.root.clone();
        let element = match row {
            GitRow::Repo(ri) => self.render_repo_block(ri, repo, fg, bg, window, cx).into_any_element(),
            row => self.render_git_row(ix, row, git, fg, bg, cx),
        };
        div()
            .w_full()
            .capture_any_mouse_down(cx.listener(move |this, _, _, _| this.set_git_active(&root)))
            .child(element)
            .into_any_element()
    }

    /// 多个仓库时一块的开头：可以收起的标题，写着仓库名（子仓库是相对主仓库的路径）、分支或者
    /// 正在跑的操作、改动的文件数和「更多」按钮；展开时下面是分支栏、提示条和提交说明框。
    fn render_repo_block(
        &self,
        ri: usize,
        repo: &git::Snapshot,
        fg: Rgb,
        bg: Rgb,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let panel = &self.workspace().project.git_panel;
        let expanded = panel.repo_expanded(repo);
        let active = panel.active.as_deref() == Some(repo.root.as_path());
        let fg_hsla = hsla(fg);
        let dim = fg_hsla.opacity(0.5);
        let name = repo_name(repo);
        let kind = match repo.kind {
            RepoKind::Main => rust_i18n::t!("git.repo_kind.main"),
            RepoKind::Submodule => rust_i18n::t!("git.repo_kind.submodule"),
            RepoKind::Nested => rust_i18n::t!("git.repo_kind.nested"),
            RepoKind::Worktree => rust_i18n::t!("git.repo_kind.worktree"),
        };
        let worktree = repo.kind == RepoKind::Worktree;
        let detail = match panel.busy(&repo.root) {
            Some(busy) => busy.label(),
            None => branch_name(&repo.info),
        };
        let changed = repo.changed();
        let root = repo.root.clone();
        let more = div()
            .id(repo_id("git-repo-more", &repo.root))
            .role(Role::Button)
            .aria_label(rust_i18n::t!("git.more").into_owned())
            .flex_none()
            .size(px(20.))
            .rounded(px(3.))
            .flex()
            .items_center()
            .justify_center()
            .hover(|button| button.bg(hsla(bg.mix(fg, 0.14))))
            .child(svg().path(MORE_ICON).size(px(14.)).text_color(fg_hsla.opacity(0.75)))
            .on_press_down(cx, {
                let root = root.clone();
                move |this, window, cx| this.open_git_menu(&root, false, window.mouse_position(), cx)
            });
        // 块头整行报成可以展开收起的一项：名字是仓库名，说明里是种类、分支或正在跑的操作，和改了
        // 几个文件。
        let mut description = format!("{kind} · {detail}");
        if changed > 0 {
            description.push_str(&format!(" · {changed}"));
        }
        let title = div()
            .id(repo_id("git-repo", &repo.root))
            .role(Role::TreeItem)
            .aria_level(1)
            .aria_expanded(expanded)
            .aria_label(name.clone())
            .aria_description(description)
            .flex_none()
            .h(scaled(list::ROW_HEIGHT + 4.))
            .w_full()
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .when(ri > 0, |title| title.border_t_1().border_color(cx.theme().border))
            .when(active, |title| title.bg(hsla(bg.mix(fg, 0.04))))
            .hover(|title| title.bg(hsla(bg.mix(fg, 0.06))))
            .text_color(fg_hsla)
            .tooltip(tooltip(format!("{kind} · {}", repo.root.display()), None))
            .child(list::chevron(expanded, cx))
            .child(svg().flex_none().path(GIT_ICON).size(px(13.)).text_color(fg_hsla.opacity(0.75)))
            .child(div().flex_initial().min_w_0().truncate().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
            .child(div().flex_1().min_w_0().truncate().text_size(FontSize::Small).text_color(dim).child(detail))
            .child(more)
            .when(changed > 0, |title| title.child(list::count_badge(changed, fg, bg)))
            .on_press_down(cx, move |this, window, cx| {
                // 收起时焦点还在这块的说明框里的话，按键就没处去了，交给面板。
                if this.commit_box(&root).is_some_and(|area| area.focus_handle(cx).is_focused(window)) {
                    window.focus(&this.git_focus, cx);
                }
                // 辅助工具按下时不经过 `render_git_item` 的鼠标捕获，这里补记是哪一块。
                this.set_git_active(&root);
                let project = &mut this.workspace_mut().project;
                project.git_panel.toggle_repo(&root, expanded, project.git.as_deref());
                cx.notify();
            })
            .when(worktree, |title| {
                let root = repo.root.clone();
                title.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.set_git_active(&root);
                        this.open_worktree_menu(&root, event.position, cx);
                    }),
                )
            });
        div().w_full().flex().flex_col().child(title).when(expanded, |block| {
            block
                .child(self.render_branch_bar(ri, repo, fg, bg, cx))
                .children(repo.info.operation.map(|operation| operation_banner(&repo.root, operation, fg)))
                .child(self.render_commit_area(ri, repo, fg, bg, window, cx))
        })
    }

    /// 标题栏上的图标按钮；按下时不往外传，免得标题栏把它当成拖动窗口。弹出菜单的按钮不带提示，
    /// 不然提示会盖住菜单的第一项，报给辅助工具的名字由调用方另给。
    fn header_button(
        &self,
        id: &'static str,
        icon: &'static str,
        text: Option<String>,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        super::titlebar::icon_toggle(id, icon, 14., false, cx)
            .role(Role::Button)
            .flex_none()
            .size(px(HEADER_BUTTON_SIZE))
            .when_some(text, |button, text| button.aria_label(text.clone()).tooltip(tooltip(text, None)))
            .on_press_down(cx, handler)
    }

    /// 第 `ri` 个仓库的当前分支（点了切换分支）、没提交的改动加减了多少行，和右边的同步按钮：有上游时写着落后、领先几个
    /// 提交，没有上游时是发布分支。
    fn render_branch_bar(&self, ri: usize, repo: &git::Snapshot, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let info = &repo.info;
        let busy = self.workspace().project.git_panel.busy(&repo.root);
        let spinning = busy.is_some_and(Busy::is_remote);
        let busy = busy.is_some();
        let fg_hsla = hsla(fg);
        let hover_bg = hsla(bg.mix(fg, 0.08));
        let name = branch_name(info);
        let root = repo.root.clone();
        let branch = div()
            .id(repo_id("git-branch", &repo.root))
            .role(Role::ComboBox)
            .aria_label(rust_i18n::t!("git.checkout").into_owned())
            .aria_value(name.clone())
            .flex_initial()
            .min_w_0()
            .h(scaled(22.))
            .px(px(6.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .gap(px(6.))
            .hover(|branch| branch.bg(hover_bg))
            .tooltip(tooltip(rust_i18n::t!("git.checkout"), Some(&GitCheckout)))
            .on_press_down(cx, {
                let root = root.clone();
                move |this, window, cx| this.open_branch_picker(&root, false, None, window, cx)
            })
            .child(svg().flex_none().path(BRANCH_ICON).size(px(14.)).text_color(fg_hsla.opacity(0.75)))
            .child(div().min_w_0().truncate().child(name));
        // 没提交的改动一共加减了多少行，为 0 的那边不画。
        let (added, removed) = (repo.added(), repo.removed());
        let changes = (added > 0 || removed > 0).then(|| {
            let label = [(added > 0).then(|| format!("+{added}")), (removed > 0).then(|| format!("−{removed}"))]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            div()
                .id(repo_id("git-line-changes", &repo.root))
                .role(Role::Label)
                .aria_label(label)
                .flex_none()
                .flex()
                .gap(px(6.))
                .text_size(FontSize::Body)
                .when(added > 0, |changes| changes.child(added_label(added)))
                .when(removed > 0, |changes| changes.child(removed_label(removed)))
        });
        // 按钮上只有箭头和数字，报给辅助工具的名字用写全了的「同步更改」。
        let sync = if info.upstream.is_some() {
            let counts = format!("{}↓ {}↑", info.behind, info.ahead);
            let text = rust_i18n::t!("git.sync_tooltip", upstream = info.upstream.clone().unwrap_or_default());
            let name = rust_i18n::t!("git.sync_changes", behind = info.behind, ahead = info.ahead).into_owned();
            Some((counts, name, text, PrimaryAction::Sync))
        } else if info.has_remote && info.branch.is_some() && info.head.is_some() {
            let label = rust_i18n::t!("git.publish").into_owned();
            Some((label.clone(), label, rust_i18n::t!("git.publish_tooltip"), PrimaryAction::Publish))
        } else {
            None
        };
        let sync = sync.map(|(label, name, text, action)| {
            let root = root.clone();
            div()
                .id(repo_id("git-sync", &repo.root))
                .role(Role::Button)
                .aria_label(name)
                .aria_description(text.clone().into_owned())
                .flex_none()
                .h(scaled(22.))
                .px(px(6.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .gap(px(4.))
                .text_color(fg_hsla.opacity(if busy { 0.35 } else { 0.75 }))
                .tooltip(tooltip(text, None))
                .child(sync_icon(("git-sync-icon", ri), spinning, 13., fg_hsla.opacity(if busy { 0.35 } else { 0.75 })))
                .child(label)
                .when(!busy, |sync| {
                    sync.hover(|sync| sync.bg(hover_bg))
                        .on_press_down(cx, move |this, window, cx| this.run_primary(&root, action, window, cx))
                })
        });
        div()
            .flex_none()
            .h(scaled(BRANCH_BAR_HEIGHT))
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(4.))
            .text_color(fg_hsla)
            .child(branch)
            .children(changes)
            .child(div().flex_1())
            .children(sync)
    }

    /// 第 `ri` 个仓库的提交说明框，下面是提交按钮和旁边弹出更多提交方式的箭头。没有改动时提交
    /// 按钮换成同步或者发布分支。
    fn render_commit_area(
        &self,
        ri: usize,
        repo: &git::Snapshot,
        fg: Rgb,
        bg: Rgb,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let panel = &self.workspace().project.git_panel;
        let (info, dirty, root) = (&repo.info, !repo.is_clean(), repo.root.clone());
        let running = panel.busy(&root);
        let busy = running.is_some();
        let fg_hsla = hsla(fg);
        let Some(area) = panel.repos.get(&root).and_then(|repo| repo.commit_box.clone()) else {
            return div();
        };
        let focused = area.focus_handle(cx).is_focused(window);
        let has_message = !area.read(cx).text().trim().is_empty();
        let primary = if dirty {
            PrimaryAction::Commit
        } else if info.upstream.is_some() && (info.ahead > 0 || info.behind > 0) {
            PrimaryAction::Sync
        } else if info.upstream.is_none() && info.has_remote && info.branch.is_some() && info.head.is_some() {
            PrimaryAction::Publish
        } else {
            PrimaryAction::Commit
        };
        let (icon, label) = match primary {
            PrimaryAction::Commit => (CHECK_ICON, rust_i18n::t!("git.commit").into_owned()),
            PrimaryAction::Sync => {
                (SYNC_ICON, rust_i18n::t!("git.sync_changes", behind = info.behind, ahead = info.ahead).into_owned())
            }
            PrimaryAction::Publish => (SYNC_ICON, rust_i18n::t!("git.publish_branch").into_owned()),
        };
        // 推送、拉取要跑好几秒：跑的时候按钮写着在做什么，连远端的图标还转着圈，免得以为没点上。
        let label = running.map_or(label, Busy::label);
        // 主按钮的底色由前景混进背景算出，跟着主题走，和新建工作区对话框的主按钮一样。
        let (accent, accent_hover, on_accent) = (hsla(bg.mix(fg, 0.16)), hsla(bg.mix(fg, 0.22)), fg_hsla);
        let icon = match running {
            Some(running) if running.is_remote() => sync_icon(("git-commit-icon", ri), true, 14., on_accent),
            _ => svg().flex_none().path(icon).size(px(14.)).text_color(on_accent).into_any_element(),
        };
        let enabled = !busy && (primary != PrimaryAction::Commit || (dirty && has_message));
        let box_border = if focused { fg_hsla.opacity(0.4) } else { fg_hsla.opacity(0.15) };
        // 说明框右上角让 AI 写说明的按钮：有改动、仓库没在忙时才能点，写的时候说明框底下的提交按钮
        // 写着在生成。
        let can_write = dirty && !busy;
        let sparkle = div()
            .id(repo_id("git-commit-sparkle", &repo.root))
            .role(Role::Button)
            .aria_label(rust_i18n::t!("git.ai_message.button").into_owned())
            .absolute()
            .top(px(2.))
            .right(px(2.))
            .size(px(24.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            // 说明框整块是文字光标，按钮上换回箭头，能点时是手指。
            .cursor_default()
            .tooltip(tooltip(rust_i18n::t!("git.ai_message.button"), None))
            .child(svg().path(SPARKLE_ICON).size(px(18.)).text_color(fg_hsla.opacity(if can_write {
                0.75
            } else {
                0.3
            })))
            .when(can_write, |sparkle| {
                let root = root.clone();
                sparkle
                    .cursor_pointer()
                    .hover(|sparkle| sparkle.bg(fg_hsla.opacity(0.08)))
                    .on_press_down(cx, move |this, window, cx| this.generate_commit_message(&root, window, cx))
            });
        let commit_box = div()
            .id(repo_id("git-commit-box", &repo.root))
            .relative()
            .flex_none()
            .w_full()
            .pl(px(6.))
            .pr(px(30.))
            .py(px(4.))
            .rounded(px(4.))
            .border_1()
            .border_color(box_border)
            .bg(hsla(bg.mix(fg, 0.04)))
            .text_color(fg_hsla)
            .cursor_text()
            .on_mouse_down(MouseButton::Left, {
                let area = area.clone();
                move |_, window, cx| window.focus(&area.focus_handle(cx), cx)
            })
            .child(area)
            .child(sparkle);
        let main = div()
            .id(repo_id("git-commit", &repo.root))
            .role(Role::Button)
            .aria_label(label.clone())
            .aria_disabled(!enabled)
            .flex_1()
            .min_w_0()
            .h_full()
            .rounded_l(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .bg(accent)
            .text_color(on_accent)
            .child(icon)
            .child(div().min_w_0().truncate().child(label))
            .when(enabled, |main| {
                let root = root.clone();
                main.hover(move |main| main.bg(accent_hover))
                    .on_press_down(cx, move |this, window, cx| this.run_primary(&root, primary, window, cx))
            });
        let more = div()
            .id(repo_id("git-commit-more", &repo.root))
            .role(Role::Button)
            .aria_label(rust_i18n::t!("git.commit_more").into_owned())
            .flex_none()
            .w(scaled(COMMIT_BUTTON_HEIGHT))
            .h_full()
            .rounded_r(px(4.))
            .border_l_1()
            .border_color(on_accent.opacity(0.3))
            .flex()
            .items_center()
            .justify_center()
            .bg(accent)
            .child(svg().path(CHEVRON_DOWN_ICON).size(px(12.)).text_color(on_accent))
            .when(!busy, |more| {
                more.hover(move |more| more.bg(accent_hover)).on_press_down(cx, move |this, window, cx| {
                    this.open_commit_menu(&root, window.mouse_position(), cx)
                })
            });
        div()
            .flex_none()
            .px(px(10.))
            .pb(px(8.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(commit_box)
            .child(
                div()
                    .flex_none()
                    .h(scaled(COMMIT_BUTTON_HEIGHT))
                    .flex()
                    .when(!enabled, |buttons| buttons.opacity(0.5))
                    .child(main)
                    .child(more),
            )
    }

    /// 提交按钮旁边箭头弹出的菜单：各种提交方式，作用到根目录是 `root` 的仓库。
    fn open_commit_menu(&mut self, root: &Path, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        self.set_git_active(root);
        let item = |key: &str, action: Box<dyn Action>| Some(menu_item(rust_i18n::t!(key), action, true, cx));
        let items = vec![
            item("git.commit", Box::new(GitCommit)),
            item("git.commit_amend", Box::new(GitCommitAmend)),
            item("git.commit_and_push", Box::new(GitCommitAndPush)),
            item("git.commit_and_sync", Box::new(GitCommitAndSync)),
            None,
            item("git.ai_message.menu", Box::new(GitGenerateCommitMessage)),
            None,
            item("git.undo_last_commit", Box::new(GitUndoLastCommit)),
        ];
        let target = self.git_focus.clone();
        self.open_menu(position, items, target, cx);
    }

    /// 「更多」弹出的菜单。块头上的（`header` 为假）是拉取推送、分支、整体暂存和储藏，作用到根目录
    /// 是 `root` 的仓库；标题栏上的是面板的显示设置，只有一个仓库时前面还有那个仓库的操作。
    fn open_git_menu(
        &mut self,
        root: &Path,
        header: bool,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let project = &self.workspace().project;
        let multi = project.git.as_ref().is_some_and(|git| project.git_panel.multi(git));
        let mut items = if header && multi { Vec::new() } else { self.git_repo_menu_items(root, cx) };
        if header {
            if !items.is_empty() {
                items.push(None);
            }
            items.extend([
                Some(menu_item(
                    rust_i18n::t!(if self.git_tree { "git.view_as_list" } else { "git.view_as_tree" }),
                    Box::new(GitToggleTreeView),
                    true,
                    cx,
                )),
                Some(check_item(
                    rust_i18n::t!("git.show_submodules").into_owned(),
                    None,
                    self.git_show_subs,
                    Box::new(GitToggleSubmodules),
                )),
                Some(check_item(
                    rust_i18n::t!("git.show_worktrees").into_owned(),
                    None,
                    self.git_show_worktrees,
                    Box::new(GitToggleWorktrees),
                )),
            ]);
        }
        let target = self.git_focus.clone();
        self.open_menu(position, items, target, cx);
    }

    /// 作用到根目录是 `root` 的仓库的那些菜单项，并记下点的是这个仓库。
    fn git_repo_menu_items(&mut self, root: &Path, cx: &mut Context<Self>) -> Vec<Option<MenuItem>> {
        self.set_git_active(root);
        let enabled = self.workspace().project.git_panel.busy(root).is_none();
        let item = |key: &str, action: Box<dyn Action>| Some(menu_item(rust_i18n::t!(key), action, enabled, cx));
        // 其他工作树的「更多」菜单前面加上打开、显示和删除它。
        let mut items = if self.is_worktree(root) {
            let mut items = self.worktree_menu_items(root, cx);
            items.push(None);
            items
        } else {
            Vec::new()
        };
        items.extend([
            item("git.pull", Box::new(GitPull)),
            item("git.push", Box::new(GitPush)),
            item("git.sync", Box::new(GitSync)),
            item("git.fetch", Box::new(GitFetch)),
            None,
            item("git.checkout", Box::new(GitCheckout)),
            item("git.create_branch", Box::new(GitCreateBranch)),
            None,
            item("git.stage_all", Box::new(GitStageAll)),
            item("git.unstage_all", Box::new(GitUnstageAll)),
            item("git.discard_all", Box::new(GitDiscardAll)),
            None,
            item("git.stash", Box::new(GitStash)),
            item("git.stash_include_untracked", Box::new(GitStashIncludeUntracked)),
            item("git.stash_pop_latest", Box::new(GitStashPopLatest)),
            None,
            item("git.undo_last_commit", Box::new(GitUndoLastCommit)),
        ]);
        items
    }

    /// 改动的文件在树形式和列表形式之间切换，所有 workspace 一起，存进窗口存档。
    fn git_toggle_tree_view(&mut self, _: &GitToggleTreeView, _: &mut Window, cx: &mut Context<Self>) {
        self.git_tree = !self.git_tree;
        self.save(cx);
        cx.notify();
    }
}

/// 同步图标；`spinning` 时一直转圈，表示还在连远端。
pub(super) fn sync_icon(id: impl Into<ElementId>, spinning: bool, size: f32, color: Hsla) -> AnyElement {
    let icon = svg().flex_none().path(SYNC_ICON).size(px(size)).text_color(color);
    if !spinning {
        return icon.into_any_element();
    }
    // 不限帧时每一帧都要重画，连带它所在的整个窗口按屏幕刷新率重画；帧率和 `AgentKind::spinner` 默认的一格
    // 80 毫秒一样。
    let animation = Animation::new(Duration::from_secs(1)).repeat().with_max_fps(12.5);
    icon.with_animation(id, animation, |icon, delta| {
        icon.with_transformation(Transformation::rotate(percentage(delta)))
    })
    .into_any_element()
}

/// 一个仓库的块里各个元素的 id，按根目录取：辅助工具按元素 id 给节点编号，按块的序号取的话，多出或少了
/// 子仓库时后面几块的节点就对不上了。
pub(super) fn repo_id(prefix: &str, root: &Path) -> SharedString {
    format!("{prefix}-{}", root.display()).into()
}

/// 块头和图表标题上写的仓库名：主仓库和工作树是目录名（工作树多半不在主仓库目录里，完整路径在
/// 块头的 tooltip 里），子仓库是相对主仓库的路径。
fn repo_name(repo: &git::Snapshot) -> String {
    match repo.kind {
        RepoKind::Main | RepoKind::Worktree => base_name(&repo.root),
        RepoKind::Submodule | RepoKind::Nested => repo.prefix.display().to_string(),
    }
}

/// 分支栏和块头上写的分支：分离头指针时写着提交，还没有提交时写着没有提交。
fn branch_name(info: &git::RepoInfo) -> String {
    match (&info.branch, &info.head) {
        (Some(branch), _) => branch.clone(),
        (None, Some(head)) => rust_i18n::t!("git.detached", head = head).into_owned(),
        (None, None) => rust_i18n::t!("git.no_commits").into_owned(),
    }
}

/// 根目录是 `root` 的仓库合并、变基这些进行到一半时的提示条。
fn operation_banner(root: &Path, operation: Operation, fg: Rgb) -> Stateful<Div> {
    let text = match operation {
        Operation::Merge => rust_i18n::t!("git.operation.merge"),
        Operation::Rebase => rust_i18n::t!("git.operation.rebase"),
        Operation::CherryPick => rust_i18n::t!("git.operation.cherry_pick"),
        Operation::Revert => rust_i18n::t!("git.operation.revert"),
    };
    div()
        .id(repo_id("git-operation", root))
        .role(Role::Status)
        .aria_label(text.clone().into_owned())
        .flex_none()
        .mx(px(10.))
        .mb(px(6.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(4.))
        .bg(hsla(super::project::MODIFIED).opacity(0.15))
        .text_color(hsla(fg))
        .child(text.into_owned())
}
