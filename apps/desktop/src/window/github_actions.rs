//! 把右侧面板的 GitHub Actions 页接到窗口上。页本身在 `runode_github_actions`：这里交给它仓库、分支、
//! shell 报告的 PATH 和固定到状态栏上的工作流，画面板外框和顶上的标签，替它在旁边的终端里跑 gh、
//! 在预览栏里开工作流文件，存档固定的工作流和三段的高度。

use crate::ui::theme::ActiveTheme as _;
use std::path::PathBuf;

use gpui::{AnyElement, App, Context, Div, Entity, Focusable, Stateful, Window, div, prelude::*};
use runode_github_actions::{ActionsEvent, ActionsPanel, Input, PROGRAM, render_pinned};

use super::{
    Divider, WindowView, cards,
    project::{SidePanel, panel_message, panel_shell},
    status_bar,
};
use crate::{terminal_view::TerminalEvent, ui::font_size::FontSize};

impl WindowView {
    pub(super) fn github_actions_shown(&self) -> bool {
        self.workspace().panel == Some(SidePanel::GitHubActions)
    }

    /// gh 在仓库根目录里跑；面板收着、还没读过 git 状态时在终端的目录里跑。
    fn actions_dir(&self, cx: &Context<Self>) -> PathBuf {
        match &self.workspace().project.git {
            Some(git) => git.main.root.clone(),
            None => self.project_dir(cx),
        }
    }

    /// 把当前的仓库、分支、PATH 和固定的工作流交给页。
    fn sync_github_actions(&mut self, cx: &mut Context<Self>) {
        let input = Input {
            dir: self.actions_dir(cx),
            branch: self.workspace().project.git.as_ref().and_then(|git| git.main.info.branch.clone()),
            path: self.shell_path(cx),
            pinned: self.workspace().pinned_workflows.clone(),
            focus: Some(self.focus_handle(cx)),
        };
        self.github_actions.update(cx, |page, _| page.set_input(input));
    }

    /// PATH 里有 gh、当前目录在仓库里时才显示标签；正显示着时仍留着。
    pub(super) fn github_actions_tab_visible(&self, cx: &App) -> bool {
        if self.github_actions_shown() {
            return true;
        }
        if !self.git_button_visible() {
            return false;
        }
        self.tool_on_path(&self.actions_found, PROGRAM, cx)
    }

    /// 窗口在前台时每秒看一次：页上显示着的、固定在状态栏上的数据，到时候了就重读。
    pub(super) fn poll_github_actions(&mut self, cx: &mut Context<Self>) {
        let shown = self.github_actions_shown();
        let status_bar = status_bar::shown(cx);
        if !shown && !(status_bar && !self.workspace().pinned_workflows.is_empty()) {
            return;
        }
        self.sync_github_actions(cx);
        self.github_actions.update(cx, |page, cx| page.poll(shown, status_bar, cx));
    }

    /// 切走或收起页时丢掉还没填完的名字。
    pub(super) fn cancel_github_actions_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.github_actions.update(cx, |page, cx| page.cancel_edit(window, cx));
    }

    /// 拖第 `si` 段上沿的分隔线到窗口里的纵坐标 `y`。
    pub(super) fn resize_actions_section(&mut self, si: usize, y: f32, cx: &mut Context<Self>) {
        self.github_actions.update(cx, |page, cx| {
            page.resize_section(si, y, cx);
            cx.notify();
        });
    }

    pub(super) fn render_github_actions_panel(&mut self, width: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let shell = panel_shell("github-actions-panel", width, cards(cx), cx)
            .bg(cx.theme().mix(0.03))
            .text_size(FontSize::Body)
            .child(self.render_panel_tabs(cx));
        if !self.git_button_visible() {
            return shell.child(panel_message(rust_i18n::t!("panel.not_repo").into_owned(), cx));
        }
        self.sync_github_actions(cx);
        shell.child(div().flex_1().min_h_0().child(self.github_actions.clone()))
    }

    /// 状态栏左边固定的工作流。颜色由 `render_pinned` 从主题读，`_fg`、`_bg` 用不着了，留着只为不在这一步
    /// 改状态栏的调用。
    pub(super) fn render_pinned_workflows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let items = self.github_actions.read(cx).pinned_items();
        render_pinned(items, status_bar::STATUS_BAR_HEIGHT - 6., cx)
    }

    /// 页要窗口办的事。
    pub(super) fn on_github_actions_event(
        &mut self,
        page: &Entity<ActionsPanel>,
        event: &ActionsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ActionsEvent::RunInTerminal { command, refetch } => {
                let dir = self.actions_dir(cx);
                let Some(view) = self.run_in_terminal(&dir, command.clone(), window, cx) else {
                    return;
                };
                let Some(refetch) = refetch.clone() else {
                    return;
                };
                // 命令打进去之前前台就是 shell，看到过前台不是 shell 以后再回到 shell 才算跑完。
                let id = view.entity_id();
                let page = page.clone();
                let mut ran = false;
                let watch = cx.subscribe(&view, move |this, view, event: &TerminalEvent, cx| {
                    let done = match event {
                        TerminalEvent::Exited => true,
                        TerminalEvent::MetaChanged => {
                            let shell = view.read(cx).meta().foreground_is_shell;
                            ran |= !shell;
                            ran && shell
                        }
                        _ => false,
                    };
                    if done {
                        this.actions_watches.remove(&id);
                        page.update(cx, |page, cx| page.refetch(refetch.clone(), cx));
                    }
                });
                self.actions_watches.insert(id, watch);
            }
            ActionsEvent::PinsChanged(pins) => {
                self.workspace_mut().pinned_workflows = pins.clone();
                self.save(cx);
                cx.notify();
            }
            ActionsEvent::OpenFile(path) => self.open_preview(path, true, cx),
            ActionsEvent::LayoutChanged => self.save(cx),
            ActionsEvent::StartResize(si) => {
                self.dragging_divider = Some(Divider::GitHubActions(*si));
                cx.notify();
            }
        }
    }
}
