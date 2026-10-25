//! 把终端挪到别处（别的分屏旁边，或者某个 workspace 里的新标签），以及两个终端对调位置。挪的是同一个
//! 终端，会话、屏幕和事件订阅都跟着走，不重开 shell；只在一个窗口里挪。
//!
//! 卡片样式下拖分屏的标题条（`DraggedPane`）：放到别的分屏靠边处就在那一侧分开它，放到正中对调
//! （`drop_zone`）；放到标签上挪进那个标签，放到标签条的空白处或侧栏的 workspace 上成为新标签。
//! 键盘上有和相邻分屏对调（`SwapPane`）、挪成新标签（`MovePaneToNewTab`）两个动作。

use gpui::{
    Action, Context, Entity, EntityId, Pixels, Point, Render, SharedString, Subscription, Window, actions, prelude::*,
    px,
};
use runode_protocol::Placement;
use runode_types::pane::Direction;

use super::WindowView;
use super::model::{Tab, TabId, WorkspaceId};
use crate::terminal_view::TerminalView;
use crate::ui::{panel::PANE_HEADER_HEIGHT, tabs::drag_chip, theme::ActiveTheme as _};

actions!(
    runode,
    [
        /// 把当前分屏挪成紧跟当前标签后面的新标签。
        MovePaneToNewTab
    ]
);

/// 当前分屏和这个方向上相邻的分屏对调位置。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct SwapPane(pub Direction);

/// 挪到哪。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Destination {
    /// 把这个终端一分为二，挪来的在 `Direction` 那一侧。
    Beside(EntityId, Direction),
    /// 这个 workspace 里的新标签，排在标签 `after` 后面，为空时排在最后。
    NewTab { workspace: WorkspaceId, after: Option<TabId> },
}

/// 拖动中的分屏，跟着鼠标画出它的标题。
#[derive(Clone)]
pub(super) struct DraggedPane {
    pub(super) pane: EntityId,
    title: SharedString,
}

impl DraggedPane {
    pub(super) fn new(pane: EntityId, title: SharedString) -> Self {
        Self { pane, title }
    }
}

impl Render for DraggedPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        drag_chip(px(220.), px(PANE_HEADER_HEIGHT), self.title.clone(), cx.theme().card, cx)
    }
}

/// 分屏放到另一个分屏上的哪一块。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DropZone {
    /// 在这一侧分开目标。
    Edge(Direction),
    /// 和目标对调。
    Center,
}

/// 落点在目标分屏里的相对位置（各轴 0 到 1）落在哪一块：离四边都不近的中间一块对调，其余按最近的边。
pub(super) fn drop_zone(x: f32, y: f32) -> DropZone {
    // 中间一块离各边至少占边长的这么多。
    const MARGIN: f32 = 0.25;
    let middle = MARGIN..=1. - MARGIN;
    if middle.contains(&x) && middle.contains(&y) {
        return DropZone::Center;
    }
    let edges = [(x, Direction::Left), (1. - x, Direction::Right), (y, Direction::Up), (1. - y, Direction::Down)];
    let (_, side) = edges.into_iter().min_by(|a, b| a.0.total_cmp(&b.0)).unwrap_or((0., Direction::Right));
    DropZone::Edge(side)
}

/// `position` 落在 `bounds` 里的哪一块，不在里面时为 `None`。
pub(super) fn zone_at(bounds: gpui::Bounds<Pixels>, position: Point<Pixels>) -> Option<DropZone> {
    if !bounds.contains(&position) || bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
        return None;
    }
    let x = (position.x - bounds.origin.x) / bounds.size.width;
    let y = (position.y - bounds.origin.y) / bounds.size.height;
    Some(drop_zone(x, y))
}

/// 从标签里拿出来的终端：它和对它事件的订阅，它是不是干完了还没看，是不是折叠着。
struct Detached {
    entry: (Entity<TerminalView>, Subscription),
    done: bool,
    collapsed: bool,
}

impl Tab {
    /// 拿出终端 `pane` 和跟着它走的标记，不动布局。
    fn release(&mut self, pane: EntityId) -> Detached {
        let entry = self.panes.remove(&pane).expect("the pane was located in this tab");
        Detached { entry, done: self.done.remove(&pane), collapsed: self.collapsed.remove(&pane) }
    }

    /// 收进 `release` 拿出来的终端 `pane`，不动布局。
    fn adopt(&mut self, pane: EntityId, detached: Detached) {
        self.panes.insert(pane, detached.entry);
        if detached.done {
            self.done.insert(pane);
        }
        if detached.collapsed {
            self.collapsed.insert(pane);
        }
    }
}

impl WindowView {
    /// 把终端 `pane` 挪到 `to`，返回挪成了没有：`pane` 或目标不在这个窗口里、挪到自己旁边时不挪。
    /// `focus` 为真时切过去；否则显示着的还是原来那个标签。
    pub(super) fn move_pane(
        &mut self,
        pane: EntityId,
        to: Destination,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((wi, ti)) = self.locate(pane) else {
            return false;
        };
        let source = &self.workspaces[wi].tabs[ti];
        match to {
            Destination::Beside(target, _) => {
                if target == pane || self.locate(target).is_none() {
                    return false;
                }
            }
            Destination::NewTab { workspace, after } => {
                let Some(dest) = self.workspaces.iter().find(|w| w.id == workspace) else {
                    return false;
                };
                if after.is_some_and(|after| dest.tabs.iter().all(|tab| tab.id != after)) {
                    return false;
                }
                // 自己独占的标签挪到自己后面：位置不变。
                if source.panes.len() == 1 && after == Some(source.id) {
                    return true;
                }
            }
        }
        let focused = self.focused_view().map(Entity::entity_id);
        let detached = self.detach_pane(pane, wi, ti);
        match to {
            Destination::Beside(target, side) => {
                let id = self.next_id();
                let (wi, ti) = self.locate(target).expect("the target was located before detaching");
                // 有焦点的终端挪到同一个显示着的标签里别处时焦点仍在它上面。
                let keep = focused == Some(pane) && self.is_shown(wi, ti);
                let tab = &mut self.workspaces[wi].tabs[ti];
                tab.root.insert(target, pane, side, id);
                tab.adopt(pane, detached);
                tab.zoomed = false;
                if focus || keep {
                    tab.focused = pane;
                }
            }
            Destination::NewTab { workspace, after } => {
                let mut tab = self.tab_holding(pane, detached.entry);
                if detached.done {
                    tab.done.insert(pane);
                }
                let workspace = self
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == workspace)
                    .expect("the workspace was found before detaching");
                let ix = after
                    .and_then(|after| workspace.tabs.iter().position(|tab| tab.id == after))
                    .map_or(workspace.tabs.len(), |ix| ix + 1);
                workspace.add_tab(ix, tab);
            }
        }
        self.settle(pane, focus, focused, window, cx);
        true
    }

    /// 终端 `a` 和 `b` 对调位置，可以在不同的标签里。返回对调成了没有。`focus` 为真时切到 `a` 的新位置。
    pub(super) fn swap_panes(
        &mut self,
        a: EntityId,
        b: EntityId,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (Some(at_a), Some(at_b)) = (self.locate(a), self.locate(b)) else {
            return false;
        };
        if a == b {
            return false;
        }
        let focused = self.focused_view().map(Entity::entity_id);
        if at_a == at_b {
            let tab = &mut self.workspaces[at_a.0].tabs[at_a.1];
            tab.root.swap(a, b);
            tab.zoomed = false;
        } else {
            let detached_a = self.workspaces[at_a.0].tabs[at_a.1].release(a);
            let detached_b = self.workspaces[at_b.0].tabs[at_b.1].release(b);
            // `b` 搬进 `a` 的位置，`a` 搬进 `b` 的。
            for ((wi, ti), (gone, came), detached) in [(at_a, (a, b), detached_b), (at_b, (b, a), detached_a)] {
                let tab = &mut self.workspaces[wi].tabs[ti];
                tab.root.swap(a, b);
                tab.adopt(came, detached);
                if tab.focused == gone {
                    tab.focused = came;
                }
                tab.zoomed = false;
            }
        }
        self.settle(a, focus, focused, window, cx);
        true
    }

    /// 把 `pane` 从第 `wi` 个 workspace 的第 `ti` 个标签里拿出来，不结束会话：兄弟分屏顶上来；标签里只剩
    /// 它时连标签一起拿掉。
    fn detach_pane(&mut self, pane: EntityId, wi: usize, ti: usize) -> Detached {
        let workspace = &mut self.workspaces[wi];
        let tab = &mut workspace.tabs[ti];
        let detached = tab.release(pane);
        match tab.root.remove(pane) {
            Some(next) if tab.focused == pane => {
                tab.focused = next;
                tab.zoomed = false;
            }
            Some(_) => {}
            None => _ = workspace.remove_tab(ti),
        }
        detached
    }

    /// 挪动、对调之后收尾：`focus` 时切到 `pane`；否则显示着的标签照新样子显示，原来有焦点的终端
    /// （`focused`）不在窗口里显示了时把焦点交给当前标签新的当前分屏。
    fn settle(
        &mut self,
        pane: EntityId,
        focus: bool,
        focused: Option<EntityId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if focus {
            self.show_pane(pane, window, cx);
        } else if self.focused_view().map(Entity::entity_id) != focused {
            self.activate(self.workspace().active, window, cx);
        } else {
            self.start_shown(cx);
            self.sync_visibility(window, cx);
            self.save(cx);
            cx.notify();
        }
    }

    /// 照 `Open` 的摆法 `placement` 放到终端 `near` 旁边时挪到哪；`near` 不在这个窗口里时为 `None`。
    pub(super) fn destination(&self, near: EntityId, placement: Placement) -> Option<Destination> {
        Some(match placement {
            Placement::Tab => {
                let (wi, ti) = self.locate(near)?;
                let workspace = &self.workspaces[wi];
                Destination::NewTab { workspace: workspace.id, after: Some(workspace.tabs[ti].id) }
            }
            Placement::Right => Destination::Beside(near, Direction::Right),
            Placement::Down => Destination::Beside(near, Direction::Down),
        })
    }

    /// 把拖来的分屏放到分屏 `target` 上：`zone` 靠边时在那一侧分开它，在正中时和它对调。
    pub(super) fn drop_pane_on_pane(
        &mut self,
        pane: EntityId,
        target: EntityId,
        zone: DropZone,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match zone {
            DropZone::Edge(side) => self.move_pane(pane, Destination::Beside(target, side), true, window, cx),
            DropZone::Center => self.swap_panes(pane, target, true, window, cx),
        };
    }

    /// 把拖来的分屏放到标签 `tab` 上：挪到那个标签当前分屏的右边。拖的就是这个标签里的分屏时不动。
    pub(super) fn drop_pane_on_tab(&mut self, pane: EntityId, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((wi, ti)) = self.locate_tab(tab) else {
            return;
        };
        let target = &self.workspaces[wi].tabs[ti];
        if target.panes.contains_key(&pane) {
            return;
        }
        let beside = target.focused;
        self.move_pane(pane, Destination::Beside(beside, Direction::Right), true, window, cx);
    }

    /// 把拖来的分屏放到 workspace `workspace` 上（侧栏的一行或当前 workspace 的标签条）：成为它排在最后
    /// 的新标签。
    pub(super) fn drop_pane_as_tab(
        &mut self,
        pane: EntityId,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_pane(pane, Destination::NewTab { workspace, after: None }, true, window, cx);
    }

    pub(super) fn swap_pane(&mut self, action: &SwapPane, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(from), Some(other)) = (self.tab().map(|tab| tab.focused), self.neighbor_pane(action.0)) else {
            return;
        };
        self.swap_panes(from, other, true, window, cx);
    }

    pub(super) fn move_pane_to_new_tab(&mut self, _: &MovePaneToNewTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab() else {
            return;
        };
        let (pane, after) = (tab.focused, tab.id);
        let workspace = self.workspace().id;
        self.move_pane(pane, Destination::NewTab { workspace, after: Some(after) }, true, window, cx);
    }

    /// 拖着分屏经过分屏 `target` 时记下落在哪一块，画出落点提示；`position` 是鼠标的位置。
    pub(super) fn track_pane_drop(
        &mut self,
        dragged: EntityId,
        target: EntityId,
        bounds: gpui::Bounds<Pixels>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let zone = zone_at(bounds, position).filter(|_| dragged != target).map(|zone| (target, zone));
        let current = self.pane_drop.filter(|(pane, _)| *pane == target);
        if zone != current && (zone.is_some() || current.is_some()) {
            self.pane_drop = zone;
            cx.notify();
        }
    }

    /// 拖着分屏时分屏 `pane` 上的落点提示：落在哪一块就盖住哪一块；没在拖、没落在它上面时为 `None`。
    pub(super) fn pane_drop_overlay(&self, pane: EntityId, cx: &Context<Self>) -> Option<gpui::Div> {
        let (_, zone) = self.pane_drop.filter(|(target, _)| *target == pane)?;
        let ring = cx.theme().ring;
        let overlay = gpui::div().absolute().bg(ring.opacity(0.18)).border_2().border_color(ring.opacity(0.7));
        let half = gpui::relative(0.5);
        Some(match zone {
            DropZone::Center => overlay.inset_0(),
            DropZone::Edge(Direction::Left) => overlay.left_0().top_0().bottom_0().w(half),
            DropZone::Edge(Direction::Right) => overlay.right_0().top_0().bottom_0().w(half),
            DropZone::Edge(Direction::Up) => overlay.left_0().right_0().top_0().h(half),
            DropZone::Edge(Direction::Down) => overlay.left_0().right_0().bottom_0().h(half),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_middle_swaps_and_the_rest_splits_toward_the_nearest_edge() {
        assert_eq!(drop_zone(0.5, 0.5), DropZone::Center);
        assert_eq!(drop_zone(0.1, 0.5), DropZone::Edge(Direction::Left));
        assert_eq!(drop_zone(0.9, 0.4), DropZone::Edge(Direction::Right));
        assert_eq!(drop_zone(0.5, 0.1), DropZone::Edge(Direction::Up));
        assert_eq!(drop_zone(0.3, 0.95), DropZone::Edge(Direction::Down));
        // 角上按离得更近的那条边。
        assert_eq!(drop_zone(0.05, 0.15), DropZone::Edge(Direction::Left));
    }
}
