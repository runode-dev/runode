//! 右侧的 Git 面板，仿 VSCode 的源代码管理：顶上是当前分支和同步按钮，下面是提交说明框和提交
//! 按钮，再下面按冲突、已暂存、未暂存分段列出改动的文件和储藏。文件和改动块都能暂存、取消暂存
//! 或丢掉，标题栏的「更多」菜单里是拉取、推送、分支和储藏这些操作。
//!
//! 排成行的状态在 `rows`，列表的各行在 `list`，在后台跑 git 在 `run`，切换和新建分支的浮层在
//! `branch_picker`。git 命令本身由 `runode_git_status::Repo` 去跑。

mod branch_picker;
mod list;
mod rows;
mod run;

use std::{ops::Range, path::PathBuf};

use gpui::{
    Action, AnyElement, Context, Div, Focusable, MouseButton, MouseDownEvent, SharedString, Stateful, Window, actions,
    div, prelude::*, px, svg, uniform_list,
};
use runode_git_status::{Operation, RepoInfo, Section};
use runode_shared_types::color::Rgb;

use super::{
    WindowView, divider_color,
    files::menu_item,
    project::{RENAMED, panel_message, panel_shell},
};
use crate::{
    assets::{BRANCH_ICON, CHECK_ICON, CHEVRON_DOWN_ICON, MORE_ICON, REFRESH_ICON, SYNC_ICON},
    terminal_view::hsla,
    text_area::{TextArea, TextAreaEvent},
    tooltip::tooltip,
};

pub(super) use branch_picker::BranchPicker;
pub(super) use rows::GitPanel;

actions!(
    runode,
    [
        /// 提交暂存的改动；没有暂存的改动时问一下要不要暂存全部再提交。
        GitCommit,
        /// 改上次提交：说明框空着时沿用原来的说明。
        GitCommitAmend,
        GitCommitAndPush,
        GitCommitAndSync,
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
        GitCreateBranch
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

/// 文件的右键菜单里的一项：对 `section` 那一段里路径是 `path`（相对仓库根）的文件做 `op`。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct GitFileAction {
    pub op: FileOp,
    pub section: Section,
    pub path: PathBuf,
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
    }

    /// 当前 workspace 的提交说明框，第一次用时建出来；按 cmd-enter 提交。提示里写着当前分支，
    /// 分支变了才换。
    fn sync_commit_box(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let branch = self.workspace().project.git.as_ref().map(|git| git.info.branch.clone()).unwrap_or_default();
        let panel = &mut self.workspaces[self.active].project.git_panel;
        if panel.commit_box.is_none() {
            let area = cx.new(|cx| {
                let mut area = TextArea::new(cx);
                area.set_line_limits(COMMIT_BOX_LINES.0, COMMIT_BOX_LINES.1, cx);
                area
            });
            let events = cx.subscribe_in(&area, window, |this, _, event: &TextAreaEvent, window, cx| match event {
                TextAreaEvent::Submit => this.git_commit(&GitCommit, window, cx),
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_git_panel(
        &mut self,
        width: f32,
        rightmost: bool,
        fg: Rgb,
        bg: Rgb,
        font: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let in_repo = self.workspace().project.git.is_some();
        if in_repo {
            self.sync_commit_box(window, cx);
        }
        let project = &self.workspace().project;
        let panel = &project.git_panel;
        let dim = hsla(fg).opacity(0.5);
        let header = self
            .panel_header(rightmost, fg)
            .child(div().flex_none().text_color(hsla(fg)).child(rust_i18n::t!("git.title").into_owned()))
            .child(div().flex_1().min_w_0().truncate().text_color(dim).children(panel.busy.map(|busy| busy.label())))
            .when(in_repo, |header| {
                header
                    .child(self.header_button(
                        "git-refresh",
                        REFRESH_ICON,
                        Some(rust_i18n::t!("git.refresh").into_owned()),
                        fg,
                        bg,
                        cx,
                        |this, _, window, cx| {
                            this.git_refresh(&GitRefresh, window, cx);
                        },
                    ))
                    .child(self.header_button("git-more", MORE_ICON, None, fg, bg, cx, |this, event, _, cx| {
                        this.open_git_menu(event.position, cx);
                    }))
            });
        let body: AnyElement = match &project.git {
            _ if project.root.is_none() => div().flex_1().into_any_element(),
            None => panel_message(rust_i18n::t!("panel.not_repo").into_owned(), fg).into_any_element(),
            Some(git) => {
                let list: AnyElement = if panel.rows.is_empty() {
                    panel_message(rust_i18n::t!("panel.no_changes").into_owned(), fg).into_any_element()
                } else {
                    uniform_list(
                        "git-rows",
                        panel.rows.len(),
                        cx.processor(move |this, range: Range<usize>, _, cx| {
                            this.render_git_rows(range, fg, bg, &font, cx)
                        }),
                    )
                    .track_scroll(&panel.scroll)
                    .flex_1()
                    .into_any_element()
                };
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.render_branch_bar(&git.info, fg, bg, cx))
                    .children(git.info.operation.map(|operation| operation_banner(operation, fg)))
                    .child(self.render_commit_area(&git.info, !git.is_clean(), fg, bg, window, cx))
                    .child(list)
                    .into_any_element()
            }
        };
        panel_shell("git-panel", width, fg)
            .track_focus(&self.git_focus)
            .bg(hsla(bg))
            .text_size(px(12.))
            .child(header)
            .child(body)
    }

    /// 标题栏上的图标按钮；按下时不往外传，免得标题栏把它当成拖动窗口。弹出菜单的按钮不带提示，
    /// 不然提示会盖住菜单的第一项。
    #[allow(clippy::too_many_arguments)]
    fn header_button(
        &self,
        id: &'static str,
        icon: &'static str,
        text: Option<String>,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
        handler: impl Fn(&mut Self, &MouseDownEvent, &mut Window, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        super::titlebar::icon_toggle(id, icon, 14., false, fg, bg)
            .flex_none()
            .size(px(HEADER_BUTTON_SIZE))
            .when_some(text, |button, text| button.tooltip(tooltip(text, None, fg, bg)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    handler(this, event, window, cx);
                }),
            )
    }

    /// 当前分支（点了切换分支）和右边的同步按钮：有上游时写着落后、领先几个提交，没有上游时是
    /// 发布分支。
    fn render_branch_bar(&self, info: &RepoInfo, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let busy = self.workspace().project.git_panel.busy.is_some();
        let fg_hsla = hsla(fg);
        let hover_bg = hsla(bg.mix(fg, 0.08));
        let name = match (&info.branch, &info.head) {
            (Some(branch), _) => branch.clone(),
            (None, Some(head)) => rust_i18n::t!("git.detached", head = head).into_owned(),
            (None, None) => rust_i18n::t!("git.no_commits").into_owned(),
        };
        let branch = div()
            .id("git-branch")
            .flex_initial()
            .min_w_0()
            .h(px(22.))
            .px(px(6.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .gap(px(6.))
            .hover(|branch| branch.bg(hover_bg))
            .tooltip(tooltip(rust_i18n::t!("git.checkout"), Some(&GitCheckout), fg, bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.git_checkout(&GitCheckout, window, cx);
                }),
            )
            .child(svg().flex_none().path(BRANCH_ICON).size(px(14.)).text_color(fg_hsla.opacity(0.75)))
            .child(div().min_w_0().truncate().child(name));
        let sync = if info.upstream.is_some() {
            let counts = format!("{}↓ {}↑", info.behind, info.ahead);
            let text = rust_i18n::t!("git.sync_tooltip", upstream = info.upstream.clone().unwrap_or_default());
            Some((counts, text, PrimaryAction::Sync))
        } else if info.has_remote && info.branch.is_some() && info.head.is_some() {
            Some((
                rust_i18n::t!("git.publish").into_owned(),
                rust_i18n::t!("git.publish_tooltip"),
                PrimaryAction::Publish,
            ))
        } else {
            None
        };
        let sync = sync.map(|(label, text, action)| {
            div()
                .id("git-sync")
                .flex_none()
                .h(px(22.))
                .px(px(6.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .gap(px(4.))
                .text_color(fg_hsla.opacity(if busy { 0.35 } else { 0.75 }))
                .tooltip(tooltip(text, None, fg, bg))
                .child(svg().flex_none().path(SYNC_ICON).size(px(13.)).text_color(fg_hsla.opacity(if busy {
                    0.35
                } else {
                    0.75
                })))
                .child(label)
                .when(!busy, |sync| {
                    sync.hover(|sync| sync.bg(hover_bg)).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            if action == PrimaryAction::Sync {
                                this.git_sync(&GitSync, window, cx);
                            } else {
                                this.git_push(&GitPush, window, cx);
                            }
                        }),
                    )
                })
        });
        div()
            .flex_none()
            .h(px(BRANCH_BAR_HEIGHT))
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(4.))
            .text_color(fg_hsla)
            .child(branch)
            .child(div().flex_1())
            .children(sync)
    }

    /// 提交说明框，下面是提交按钮和旁边弹出更多提交方式的箭头。没有改动时提交按钮换成同步或者
    /// 发布分支。
    fn render_commit_area(
        &self,
        info: &RepoInfo,
        dirty: bool,
        fg: Rgb,
        bg: Rgb,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let panel = &self.workspace().project.git_panel;
        let busy = panel.busy.is_some();
        let fg_hsla = hsla(fg);
        let Some(area) = panel.commit_box.clone() else {
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
        let enabled = !busy && (primary != PrimaryAction::Commit || (dirty && has_message));
        let accent = hsla(RENAMED);
        let on_accent = gpui::white();
        let box_border = if focused { accent } else { fg_hsla.opacity(0.15) };
        let commit_box = div()
            .id("git-commit-box")
            .flex_none()
            .w_full()
            .px(px(6.))
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
            .child(area);
        let main = div()
            .id("git-commit")
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
            .child(svg().flex_none().path(icon).size(px(14.)).text_color(on_accent))
            .child(div().min_w_0().truncate().child(label))
            .when(enabled, |main| {
                main.hover(|main| main.bg(accent.opacity(0.85))).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        match primary {
                            PrimaryAction::Commit => this.git_commit(&GitCommit, window, cx),
                            PrimaryAction::Sync => this.git_sync(&GitSync, window, cx),
                            PrimaryAction::Publish => this.git_push(&GitPush, window, cx),
                        }
                    }),
                )
            });
        let more = div()
            .id("git-commit-more")
            .flex_none()
            .w(px(COMMIT_BUTTON_HEIGHT))
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
                more.hover(|more| more.bg(accent.opacity(0.85))).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.open_commit_menu(event.position, cx);
                    }),
                )
            });
        div()
            .flex_none()
            .px(px(10.))
            .pb(px(8.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .border_b_1()
            .border_color(divider_color(fg_hsla))
            .child(commit_box)
            .child(
                div()
                    .flex_none()
                    .h(px(COMMIT_BUTTON_HEIGHT))
                    .flex()
                    .when(!enabled, |buttons| buttons.opacity(0.5))
                    .child(main)
                    .child(more),
            )
    }

    /// 提交按钮旁边箭头弹出的菜单：各种提交方式。
    fn open_commit_menu(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let item = |key: &str, action: Box<dyn Action>| Some(menu_item(key, action, true, cx));
        let items = vec![
            item("git.commit", Box::new(GitCommit)),
            item("git.commit_amend", Box::new(GitCommitAmend)),
            item("git.commit_and_push", Box::new(GitCommitAndPush)),
            item("git.commit_and_sync", Box::new(GitCommitAndSync)),
            None,
            item("git.undo_last_commit", Box::new(GitUndoLastCommit)),
        ];
        let target = self.git_focus.clone();
        self.open_menu(position, items, target, cx);
    }

    /// 标题栏「更多」弹出的菜单：拉取推送、分支、整体暂存和储藏。
    fn open_git_menu(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let enabled = self.workspace().project.git_panel.busy.is_none();
        let item = |key: &str, action: Box<dyn Action>| Some(menu_item(key, action, enabled, cx));
        let items = vec![
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
        ];
        let target = self.git_focus.clone();
        self.open_menu(position, items, target, cx);
    }
}

/// 合并、变基这些进行到一半时的提示条。
fn operation_banner(operation: Operation, fg: Rgb) -> Div {
    let text = match operation {
        Operation::Merge => rust_i18n::t!("git.operation.merge"),
        Operation::Rebase => rust_i18n::t!("git.operation.rebase"),
        Operation::CherryPick => rust_i18n::t!("git.operation.cherry_pick"),
        Operation::Revert => rust_i18n::t!("git.operation.revert"),
    };
    div()
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
