//! 把右侧面板的 Git 页接到窗口上。页本身在 `git_panel`：一个窗口一个 `GitPanel`，各 workspace 的面板
//! 都在它里面。这里每次对它做什么之前告诉它当前是哪个 workspace，画的时候交给它栏宽和状态栏的高度，画面板
//! 外框和顶上的标签；后台读到的 git 状态由 `refresh_project` 经 `GitPanel::set_git` 交给它。它要窗口办的事
//! （开 diff 和文件的预览、把路径打进终端、开新标签、重读、打开分支列表和写提交说明的对话框、拖图表的分隔线、
//! 设置变了要存档）经 `GitPanelEvent` 交回来。
//!
//! 切换、新建分支的浮层（`branch_picker`）和 AI 写提交说明（`ai_message`）留在窗口：它们是盖在窗口上的浮层，
//! 开着时窗口的其余部分不报给辅助工具，分支列表还和 agent 列表共用写法、切 workspace 时由窗口关掉。选好以后
//! 调页的 `GitPanel::checkout`、`GitPanel::create_branch`、`GitPanel::write_message` 去跑 git，和面板上的
//! 操作一起按仓库排队。

mod ai_message;
mod branch_picker;

use crate::git_panel::{GitPanel, GitPanelEvent, Input};
use gpui::{Context, Div, Entity, Stateful, Window, div, prelude::*};

pub(super) use ai_message::CommitMessageDialog;
pub(super) use branch_picker::BranchPicker;

use super::{Divider, WindowView, cards, preview::DiffTarget, project::panel_shell, status_bar};
use crate::ui::{font_size::FontSize, theme::ActiveTheme as _};

impl WindowView {
    /// 对当前 workspace 的 Git 面板做 `f`：先告诉它是哪个 workspace。会让页重画，不能在画的时候调。
    pub(super) fn with_git_panel<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut GitPanel, &mut Context<GitPanel>) -> R,
    ) -> R {
        let id = self.workspace().id;
        self.git_panel.update(cx, |panel, cx| {
            panel.set_workspace(id);
            f(panel, cx)
        })
    }

    /// 右侧面板的 Git 页：外框和顶上的标签在这里画，里面是页。
    pub(super) fn render_git_panel(&mut self, width: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let input = Input { width, status_bar_height: status_bar::height(cx) };
        let id = self.workspace().id;
        self.git_panel.update(cx, |panel, _| {
            panel.set_workspace(id);
            panel.set_input(input);
        });
        panel_shell("git-panel", width, cards(cx), cx)
            .bg(cx.theme().background)
            .text_size(FontSize::Body)
            .child(self.render_panel_tabs(cx))
            .child(div().flex_1().min_h_0().child(self.git_panel.clone()))
    }

    /// 拖图表上沿的分隔线到窗口里的纵坐标 `y`，`viewport` 是面板底边的纵坐标。
    pub(super) fn resize_git_graph(&mut self, y: f32, viewport: f32, cx: &mut Context<Self>) {
        self.git_panel.update(cx, |panel, cx| {
            panel.resize_graph(y, viewport);
            cx.notify();
        });
    }

    /// Git 面板要窗口办的事。
    pub(super) fn on_git_panel_event(
        &mut self,
        _: &Entity<GitPanel>,
        event: &GitPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            GitPanelEvent::OpenDiff { root, rel, old_rel, side, clicks } => {
                let target =
                    DiffTarget { root: root.clone(), rel: rel.clone(), old_rel: old_rel.clone(), side: side.clone() };
                self.click_diff(target, *clicks, cx);
            }
            GitPanelEvent::OpenFile(path) => self.open_preview(path, true, false, cx),
            GitPanelEvent::InsertPath(path) => self.insert_paths(vec![path.clone()], None, window, cx),
            GitPanelEvent::FocusTerminal => self.focus_terminal(window, cx),
            GitPanelEvent::OpenTerminal(dir) => {
                if let Some(view) = self.spawn_terminal(Some(dir), window, cx) {
                    self.insert_tab(self.workspace().active + 1, view, window, cx);
                }
            }
            // 切走了的 workspace 等切回来时再读。
            GitPanelEvent::Refresh(id) => {
                if self.workspace().id == *id {
                    self.refresh_project(cx);
                }
            }
            GitPanelEvent::HunkApplied => self.with_preview(cx, |panel, cx| panel.reload_diff(cx)),
            GitPanelEvent::OpenBranchPicker { root, create_only, start } => {
                self.open_branch_picker(root, *create_only, start.clone(), window, cx);
            }
            GitPanelEvent::WriteCommitMessage { root, ask: true } => {
                self.open_commit_message_dialog(root.clone(), window, cx);
            }
            GitPanelEvent::WriteCommitMessage { root, ask: false } => self.generate_commit_message(root, window, cx),
            GitPanelEvent::StartGraphResize => {
                self.dragging_divider = Some(Divider::GitGraph);
                cx.notify();
            }
            GitPanelEvent::OptionsChanged => self.save(cx),
        }
    }
}
