//! 窗口绑定的动作：新建、关闭和切换 workspace、标签与分屏，以及调整、放大分屏。

use std::path::PathBuf;

use gpui::{Bounds, Context, EntityId, PathPromptOptions, Pixels, Window};
use runode_shared_types::pane::{self, Axis, Direction, SplitId};

use super::{
    CloseTab, ClosePane, CloseWorkspace, EqualizePanes, FocusNextPane, FocusPane, FocusPreviousPane, NewSplitDown,
    NewSplitRight, NewTab, NewWorkspace, NextTab, NextWorkspace, PreviousTab, PreviousWorkspace, ResizePane,
    SelectLastTab, SelectLastWorkspace, SelectTab, SelectWorkspace, TogglePaneZoom, ToggleSidebar, WindowView,
};

/// 键盘调整分屏大小时每次挪动的像素。
const RESIZE_STEP: f32 = 10.;

impl WindowView {
    pub(super) fn new_workspace(&mut self, _: &NewWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(rust_i18n::t!("workspace.choose").into_owned().into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| this.open_workspace(dir, window, cx)).ok();
        })
        .detach();
    }

    /// 切到目录是 `dir` 的 workspace，还没有时在当前 workspace 下面新建一个。
    fn open_workspace(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.workspaces.iter().position(|workspace| workspace.dir == dir) {
            self.activate_workspace(ix, window, cx);
            return;
        }
        if let Some(view) = self.spawn_terminal(Some(&dir), window, cx) {
            self.insert_workspace(self.active + 1, dir, view, window, cx);
        }
    }

    pub(super) fn close_workspace(&mut self, _: &CloseWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_close_workspace(self.active, window, cx);
    }

    pub(super) fn next_workspace(&mut self, _: &NextWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_workspace((self.active + 1) % self.workspaces.len(), window, cx);
    }

    pub(super) fn previous_workspace(&mut self, _: &PreviousWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.workspaces.len();
        self.activate_workspace((self.active + len - 1) % len, window, cx);
    }

    pub(super) fn select_workspace(&mut self, action: &SelectWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        if action.0 < self.workspaces.len() {
            self.activate_workspace(action.0, window, cx);
        }
    }

    pub(super) fn select_last_workspace(&mut self, _: &SelectLastWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_workspace(self.workspaces.len() - 1, window, cx);
    }

    pub(super) fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_shown = Some(!self.sidebar_visible());
        self.save(cx);
        cx.notify();
    }

    pub(super) fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.spawn_beside_focused(window, cx) {
            self.insert_tab(self.workspace().active + 1, view, window, cx);
        }
    }

    pub(super) fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_at(self.active, self.workspace().active, window, cx);
    }

    pub(super) fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace();
        self.activate((workspace.active + 1) % workspace.tabs.len(), window, cx);
    }

    pub(super) fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace();
        let len = workspace.tabs.len();
        self.activate((workspace.active + len - 1) % len, window, cx);
    }

    pub(super) fn select_tab(&mut self, action: &SelectTab, window: &mut Window, cx: &mut Context<Self>) {
        if action.0 < self.workspace().tabs.len() {
            self.activate(action.0, window, cx);
        }
    }

    pub(super) fn select_last_tab(&mut self, _: &SelectLastTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(self.workspace().tabs.len() - 1, window, cx);
    }

    pub(super) fn new_split_right(&mut self, _: &NewSplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Horizontal, window, cx);
    }

    pub(super) fn new_split_down(&mut self, _: &NewSplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Vertical, window, cx);
    }

    /// 把当前终端一分为二，新终端放在右边或下边并获得焦点。
    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.spawn_beside_focused(window, cx) else {
            return;
        };
        let split_id = self.next_id();
        let (id, entry) = self.pane_entry(view, window, cx);
        let tab = self.tab_mut();
        tab.root.split(tab.focused, id, axis, split_id);
        tab.panes.insert(id, entry);
        tab.focused = id;
        tab.zoomed = false;
        self.activate(self.workspace().active, window, cx);
    }

    pub(super) fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.tab().focused;
        self.close_pane_by_id(focused, window, cx);
    }

    pub(super) fn focus_next_pane(&mut self, _: &FocusNextPane, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_adjacent(1, window, cx);
    }

    pub(super) fn focus_previous_pane(&mut self, _: &FocusPreviousPane, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_adjacent(-1, window, cx);
    }

    /// 按从左到右、从上到下的顺序切到后一个（`step` 为 1）或前一个终端，首尾相接。
    fn focus_adjacent(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab();
        let leaves = tab.root.leaves();
        let Some(at) = leaves.iter().position(|id| *id == tab.focused) else {
            return;
        };
        let next = leaves[(at as isize + step).rem_euclid(leaves.len() as isize) as usize];
        self.focus_pane_in_active_tab(next, window, cx);
    }

    pub(super) fn focus_pane(&mut self, action: &FocusPane, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab();
        let layout = self.layout.borrow();
        let rect = |bounds: &Bounds<Pixels>| pane::Rect {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
        };
        let Some(from) = layout.panes.get(&tab.focused).map(rect) else {
            return;
        };
        let candidates = tab
            .root
            .leaves()
            .into_iter()
            .filter(|id| *id != tab.focused)
            .filter_map(|id| layout.panes.get(&id).map(|bounds| (id, rect(bounds))));
        let target = pane::neighbor(from, action.0, candidates);
        drop(layout);
        if let Some(target) = target {
            self.focus_pane_in_active_tab(target, window, cx);
        }
    }

    /// 切到当前标签里的另一个终端；放大着的话先恢复，否则看不到它。
    fn focus_pane_in_active_tab(&mut self, pane: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        tab.focused = pane;
        tab.zoomed = false;
        self.activate(self.workspace().active, window, cx);
    }

    pub(super) fn resize_pane(&mut self, action: &ResizePane, _: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout.borrow();
        let horizontal = matches!(action.0, Direction::Left | Direction::Right);
        let size_of = |id: SplitId| {
            layout
                .splits
                .get(&id)
                .map(|bounds| f32::from(if horizontal { bounds.size.width } else { bounds.size.height }))
        };
        let workspace = &mut self.workspaces[self.active];
        let tab = &mut workspace.tabs[workspace.active];
        // 放大时其他分屏看不见，调了也看不出效果。
        let resized = !tab.zoomed && tab.root.resize(tab.focused, action.0, RESIZE_STEP, &size_of);
        drop(layout);
        if resized {
            self.save(cx);
            cx.notify();
        }
    }

    pub(super) fn equalize_panes(&mut self, _: &EqualizePanes, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_mut().root.equalize();
        self.save(cx);
        cx.notify();
    }

    pub(super) fn toggle_pane_zoom(&mut self, _: &TogglePaneZoom, _: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        if !tab.root.is_leaf() {
            tab.zoomed = !tab.zoomed;
            self.save(cx);
            cx.notify();
        }
    }
}
