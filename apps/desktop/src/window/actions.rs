//! 窗口绑定的动作：新建、关闭和切换 workspace、标签与分屏，以及调整、放大分屏。

use std::path::PathBuf;

use gpui::{Bounds, Context, Entity, EntityId, Pixels, Window};
use runode_types::pane::{self, Axis, Direction, SplitId};

use super::{
    ClosePane, CloseTab, CloseWorkspace, EqualizePanes, FocusNextPane, FocusPane, FocusPreviousPane, NewSplitDown,
    NewSplitRight, NewTab, NextTab, NextWorkspace, PreviousTab, PreviousWorkspace, ResizePane, SelectLastTab,
    SelectLastWorkspace, SelectTab, SelectWorkspace, TogglePaneZoom, ToggleSidebar, WindowView,
};
use crate::terminal_view::TerminalView;

/// 键盘调整分屏大小时每次挪动的像素。
const RESIZE_STEP: f32 = 10.;

impl WindowView {
    /// 切到目录是 `dir` 的 workspace（目录按规范化后的比，经过符号链接也认得出），给了 `name` 就改成
    /// 这个名字；还没有时在当前 workspace 下面新建一个，`name` 为空时按目录取名。规范化在别的线程上做
    /// （`inspect_dir`），做完才办。
    pub(super) fn open_workspace(
        &mut self,
        dir: PathBuf,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let (_, target) = super::model::inspect_dir(dir.clone()).await;
            this.update_in(cx, |this, window, cx| this.open_workspace_resolved(dir, &target, name, window, cx)).ok();
        })
        .detach();
    }

    fn open_workspace_resolved(
        &mut self,
        dir: PathBuf,
        target: &std::path::Path,
        name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.workspaces.iter().position(|workspace| workspace.real_dir() == target) {
            if let Some(name) = name {
                self.set_workspace_name(ix, name.into(), window, cx);
            }
            self.activate_workspace(ix, window, cx);
            return;
        }
        if let Some(view) = self.spawn_terminal(Some(&dir), window, cx) {
            self.insert_workspace(self.active + 1, dir, name, view, window, cx);
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

    pub(super) fn select_last_workspace(
        &mut self,
        _: &SelectLastWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        if let Some(id) = self.tab().map(|tab| tab.id) {
            self.close_tab_by_id(id, window, cx);
        }
    }

    pub(super) fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.titlebar_tab_count(cx);
        if count > 0 {
            let next = self.front_tab(cx).map_or(0, |front| (front + 1) % count);
            self.select_titlebar_tab(next, window, cx);
        }
    }

    pub(super) fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.titlebar_tab_count(cx);
        if count > 0 {
            let previous = self.front_tab(cx).map_or(count - 1, |front| (front + count - 1) % count);
            self.select_titlebar_tab(previous, window, cx);
        }
    }

    pub(super) fn select_tab(&mut self, action: &SelectTab, window: &mut Window, cx: &mut Context<Self>) {
        self.select_titlebar_tab(action.0, window, cx);
    }

    pub(super) fn select_last_tab(&mut self, _: &SelectLastTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(last) = self.titlebar_tab_count(cx).checked_sub(1) {
            self.select_titlebar_tab(last, window, cx);
        }
    }

    /// 切到标题栏里的第 `ix` 个标签：终端标签在前，文件标签接在后面；没有这么多时不动。
    fn select_titlebar_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let terminals = self.workspace().tabs.len();
        if ix < terminals {
            self.activate(ix, window, cx);
        } else if ix < self.titlebar_tab_count(cx) {
            self.show_file_tab(ix - terminals, window, cx);
        }
    }

    pub(super) fn new_split_right(&mut self, _: &NewSplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Horizontal, window, cx);
    }

    pub(super) fn new_split_down(&mut self, _: &NewSplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Vertical, window, cx);
    }

    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.spawn_beside_focused(window, cx) {
            self.split_with(view, axis, window, cx);
        }
    }

    /// 把当前终端一分为二，`view` 放在右边或下边并获得焦点；workspace 里没有标签时新开一个标签。
    pub(super) fn split_with(
        &mut self,
        view: Entity<TerminalView>,
        axis: Axis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab().is_none() {
            self.insert_tab(0, view, window, cx);
            return;
        }
        let split_id = self.next_id();
        let (id, entry) = self.pane_entry(view, window, cx);
        let Some(tab) = self.tab_mut() else {
            return;
        };
        tab.root.split(tab.focused, id, axis, split_id);
        tab.panes.insert(id, entry);
        tab.focused = id;
        tab.zoomed = false;
        self.activate(self.workspace().active, window, cx);
    }

    pub(super) fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focused) = self.tab().map(|tab| tab.focused) {
            self.confirm_close_pane(focused, window, cx);
        }
    }

    pub(super) fn focus_next_pane(&mut self, _: &FocusNextPane, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_adjacent(1, window, cx);
    }

    pub(super) fn focus_previous_pane(&mut self, _: &FocusPreviousPane, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_adjacent(-1, window, cx);
    }

    /// 按从左到右、从上到下的顺序切到后一个（`step` 为 1）或前一个终端，首尾相接。
    fn focus_adjacent(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab() else {
            return;
        };
        let leaves = tab.root.leaves();
        let Some(at) = leaves.iter().position(|id| *id == tab.focused) else {
            return;
        };
        let next = leaves[(at as isize + step).rem_euclid(leaves.len() as isize) as usize];
        self.focus_pane_in_active_tab(next, window, cx);
    }

    pub(super) fn focus_pane(&mut self, action: &FocusPane, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.neighbor_pane(action.0) {
            self.focus_pane_in_active_tab(target, window, cx);
        }
    }

    /// 当前标签里当前分屏在 `direction` 这个方向上相邻的分屏。
    pub(super) fn neighbor_pane(&self, direction: Direction) -> Option<EntityId> {
        let tab = self.tab()?;
        let layout = self.layout.borrow();
        let rect = |bounds: &Bounds<Pixels>| pane::Rect {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
        };
        let from = layout.panes.get(&tab.focused).map(rect)?;
        if tab.zoomed {
            // 放大时只画了这一个，别的分屏记着的还是放大前的位置，全落在它里面：按分屏树的比例在它现在占的
            // 这一块里重新排一遍再找。
            let rects = tab.root.rects(from);
            let from = rects.iter().find(|(id, _)| *id == tab.focused).map(|(_, rect)| *rect)?;
            pane::neighbor(from, direction, rects.into_iter().filter(|(id, _)| *id != tab.focused))
        } else {
            let candidates = tab
                .root
                .leaves()
                .into_iter()
                .filter(|id| *id != tab.focused)
                .filter_map(|id| layout.panes.get(&id).map(|bounds| (id, rect(bounds))));
            pane::neighbor(from, direction, candidates)
        }
    }

    /// 切到当前标签里的另一个终端；放大着的话先恢复，否则看不到它。
    pub(super) fn focus_pane_in_active_tab(&mut self, pane: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        // 调用方记着的分屏可能已经关掉了，切过去 `Tab::focused_view` 就找不到它。
        let Some(tab) = self.tab_mut().filter(|tab| tab.panes.contains_key(&pane)) else {
            return;
        };
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
        let Some(tab) = workspace.tabs.get_mut(workspace.active) else {
            return;
        };
        // 放大时其他分屏看不见，调了也看不出效果。
        let resized = !tab.zoomed && tab.root.resize(tab.focused, action.0, RESIZE_STEP, &size_of);
        drop(layout);
        if resized {
            self.save(cx);
            cx.notify();
        }
    }

    pub(super) fn equalize_panes(&mut self, _: &EqualizePanes, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_mut() {
            tab.root.equalize();
            self.save(cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_pane_zoom(&mut self, _: &TogglePaneZoom, _: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut() else {
            return;
        };
        if !tab.root.is_leaf() {
            tab.zoomed = !tab.zoomed;
            self.save(cx);
            cx.notify();
        }
    }
}
