//! 吸附在右侧面板上的远程桌面：右侧那一栏从上到下是吸附在上方的几台、面板、吸附在下方的几台，各台和面板之间
//! 一条分隔线拖着改那一台的高度。一栏里的先后记在 `ScreenViewers::column`。拖着浮动的远程桌面经过这一栏时按
//! 鼠标落在哪台的哪一半、或面板的哪一半定下插到哪（`SnapTarget`），松开就插进去。

use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    AnyElement, App, Bounds, Context, CursorStyle, Entity, EntityId, Focusable as _, MouseButton, MouseDownEvent,
    Pixels, Point, StyleRefinement, Window, WindowControlArea, canvas, div, prelude::*, px, relative,
};

use super::{OpenScreenViewer, Placement, cached_page, set_input};
use crate::screen_viewer::{SUPPORTED, ScreenViewer};
use crate::ui::{card::Card, theme::ActiveTheme as _};
use crate::window::{
    CARD_GAP, DIVIDER_GRAB_WIDTH, Divider, TITLEBAR_HEIGHT, WindowView, WorkspaceId, cached_slot, cards, column_slot,
    persist::format::SavedSnappedScreen, slot::Slot,
};

/// 吸附着的一台默认占这一栏的比例；拖分隔线时每台不小于 `SNAP_RATIO_MIN`。
const SNAP_RATIO: f32 = 0.45;
const SNAP_RATIO_MIN: f32 = 0.15;
/// 吸附着的几台加起来最多占这一栏的这么多，剩下的留给面板。
const SNAP_RATIO_MAX: f32 = 0.85;
/// 又吸附上一台、加起来超过这么多时几台按比例缩到这么多。
const SNAP_RATIO_FILL: f32 = 0.7;
/// 拖着经过右侧那一栏时标出插到哪的那条线的粗细。
const TARGET_LINE: f32 = 4.;
/// 吸附着的远程桌面藏起来这么久就断开连接，远端不再采集、编码；再显示时重连。
const SUSPEND_DELAY: Duration = Duration::from_secs(10);

/// 吸附在右侧面板上的那台的样子。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Snap {
    side: SnapSide,
    /// 吸附时（或取消钉住时）所在的 workspace，没钉住时只在它里面显示。
    workspace: WorkspaceId,
    /// 钉住了：在它那台主机上的各个 workspace 都显示。
    pub(super) pinned: bool,
    /// 占这一栏的比例。
    ratio: f32,
}

/// 吸附在右侧面板的哪一头。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SnapSide {
    Top,
    Bottom,
}

/// 松开时插到右侧那一栏的哪。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SnapTarget {
    side: SnapSide,
    anchor: Anchor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Anchor {
    /// 插在这台的上面或下面。
    Before(EntityId),
    After(EntityId),
    /// 插在这一头挨着面板的地方。
    Panel,
}

/// 这一帧画进右侧那一栏的一台：它是谁、在哪一头，和它 prepaint 时记下的位置（`OpenScreenViewer::snap_bounds`）。
#[derive(Clone, Debug)]
pub(super) struct ShownSnap {
    id: EntityId,
    side: SnapSide,
    cell: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl ShownSnap {
    /// 最近一次画在窗口里的位置；还没画过时为空。
    fn bounds(&self) -> Option<Bounds<Pixels>> {
        self.cell.get()
    }
}

impl WindowView {
    /// 一栏里吸附着的远程桌面，从上到下。
    fn snaps(&self) -> Vec<(&OpenScreenViewer, Snap)> {
        let viewers = &self.screen_viewers;
        viewers
            .column
            .iter()
            .filter_map(|id| {
                let open = &viewers.open[viewers.position(*id)?];
                match open.placement {
                    Placement::Snapped(snap) => Some((open, snap)),
                    _ => None,
                }
            })
            .collect()
    }

    /// 吸附着的远程桌面在当前 workspace 里该不该显示：钉住了看主机，没钉住看是不是吸附时的 workspace。
    pub(super) fn snap_belongs_here(&self, open: &OpenScreenViewer, snap: Snap, cx: &App) -> bool {
        if snap.pinned {
            self.workspace().host.label() == Some(open.view.read(cx).host())
        } else {
            self.workspace().id == snap.workspace
        }
    }

    /// 远程桌面 `id` 吸附在右侧面板上，而上一帧没画它，它藏着。
    pub(super) fn screen_hidden(&self, id: EntityId) -> bool {
        let viewers = &self.screen_viewers;
        viewers.position(id).is_some_and(|ix| matches!(viewers.open[ix].placement, Placement::Snapped(_)))
            && !viewers.shown_snaps.iter().any(|shown| shown.id == id)
    }

    /// 藏着的那台吸附着的远程桌面 `id` 归到当前 workspace（钉住的不用）。
    pub(super) fn claim_snap(&mut self, id: EntityId) {
        let workspace = self.workspace().id;
        let Some(ix) = self.screen_viewers.position(id) else { return };
        if let Placement::Snapped(snap) = &mut self.screen_viewers.open[ix].placement
            && !snap.pinned
        {
            snap.workspace = workspace;
        }
    }

    /// 切到了别的 workspace：有钉住的远程桌面属于它那台主机、它的右侧面板却收着时，打开面板露出远程桌面。
    pub(in crate::window) fn follow_pinned_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pinned_here =
            self.snaps().into_iter().any(|(open, snap)| snap.pinned && self.snap_belongs_here(open, snap, cx));
        if pinned_here && self.workspace().panel.is_none() {
            self.set_panel(Some(self.workspace().last_panel), window, cx);
        }
    }

    /// 钉住或取消钉住吸附着的远程桌面 `id`；取消时它归到当前 workspace，不会当场藏起来。
    pub(super) fn toggle_screen_pin(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let workspace = self.workspace().id;
        let Some(ix) = self.screen_viewers.position(id) else { return };
        let open = &mut self.screen_viewers.open[ix];
        let Placement::Snapped(snap) = &mut open.placement else { return };
        snap.pinned = !snap.pinned;
        snap.workspace = workspace;
        set_input(&open.view, open.input(), cx);
        self.save(cx);
        cx.notify();
    }

    /// 远程桌面 `id` 插到右侧那一栏的 `target`，焦点给它。`was` 是拖起来之前它吸附着的样子，照旧钉没钉住、
    /// 照旧那么高；新吸附上的用默认高度，几台加起来太高时一起缩。
    pub(super) fn snap_screen(
        &mut self,
        id: EntityId,
        target: SnapTarget,
        was: Option<Snap>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace().id;
        let pinned = was.is_some_and(|snap| snap.pinned);
        let ratio = was.map_or(SNAP_RATIO, |snap| snap.ratio);
        let viewers = &mut self.screen_viewers;
        viewers.column.retain(|other| *other != id && viewers.open.iter().any(|open| open.view.entity_id() == *other));
        let side_of = |other: &EntityId| {
            viewers.open.iter().find(|open| open.view.entity_id() == *other).and_then(|open| match open.placement {
                Placement::Snapped(snap) => Some(snap.side),
                _ => None,
            })
        };
        let at = match target.anchor {
            Anchor::Before(other) => viewers.column.iter().position(|ix| *ix == other),
            Anchor::After(other) => viewers.column.iter().position(|ix| *ix == other).map(|ix| ix + 1),
            Anchor::Panel => None,
        };
        let at = at.unwrap_or_else(|| match target.side {
            SnapSide::Top => {
                viewers.column.iter().rposition(|other| side_of(other) == Some(SnapSide::Top)).map_or(0, |ix| ix + 1)
            }
            SnapSide::Bottom => viewers
                .column
                .iter()
                .position(|other| side_of(other) == Some(SnapSide::Bottom))
                .unwrap_or(viewers.column.len()),
        });
        viewers.column.insert(at, id);
        let Some(ix) = viewers.position(id) else { return };
        let open = &mut viewers.open[ix];
        open.placement = Placement::Snapped(Snap { side: target.side, workspace, pinned, ratio });
        set_input(&open.view, open.input(), cx);
        window.focus(&open.view.focus_handle(cx), cx);
        self.fit_snaps(cx);
        self.save(cx);
        cx.notify();
    }

    /// 当前 workspace 里显示的几台加起来超过 `SNAP_RATIO_MAX` 时按比例缩到 `SNAP_RATIO_FILL`。
    fn fit_snaps(&mut self, cx: &App) {
        let here: Vec<EntityId> = self
            .snaps()
            .into_iter()
            .filter(|(open, snap)| self.snap_belongs_here(open, *snap, cx))
            .map(|(open, _)| open.view.entity_id())
            .collect();
        let total: f32 = self.ratios(&here).sum();
        if total <= SNAP_RATIO_MAX {
            return;
        }
        let scale = SNAP_RATIO_FILL / total;
        for open in &mut self.screen_viewers.open {
            if let Placement::Snapped(snap) = &mut open.placement
                && here.contains(&open.view.entity_id())
            {
                snap.ratio = (snap.ratio * scale).max(SNAP_RATIO_MIN);
            }
        }
    }

    fn ratios<'a>(&'a self, ids: &'a [EntityId]) -> impl Iterator<Item = f32> + 'a {
        self.screen_viewers.open.iter().filter_map(move |open| match open.placement {
            Placement::Snapped(snap) if ids.contains(&open.view.entity_id()) => Some(snap.ratio),
            _ => None,
        })
    }

    /// 吸附着的几台：藏起来的开始计时（`SUSPEND_DELAY`），到时还藏着就断开；显示着的停掉计时，断开着的重连。
    /// 要在画右侧那一栏（`render_right_column`）之后调。
    pub(super) fn sync_hidden_screens(&mut self, cx: &mut Context<Self>) {
        let hidden: Vec<EntityId> = self
            .snaps()
            .into_iter()
            .map(|(open, _)| open.view.entity_id())
            .filter(|id| self.screen_hidden(*id))
            .collect();
        for open in &mut self.screen_viewers.open {
            let id = open.view.entity_id();
            if hidden.contains(&id) {
                if open.suspend_timer.is_none() && !open.view.read(cx).suspended() {
                    open.suspend_timer = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SUSPEND_DELAY).await;
                        this.update(cx, |this, cx| {
                            let Some(ix) = this.screen_viewers.position(id) else { return };
                            this.screen_viewers.open[ix].suspend_timer = None;
                            if this.screen_hidden(id) {
                                this.screen_viewers.open[ix].view.update(cx, |view, cx| view.suspend(cx));
                            }
                        })
                        .ok();
                    }));
                }
                continue;
            }
            open.suspend_timer = None;
            if open.view.read(cx).suspended() {
                // 画的时候通知别的视图不会再要一帧，等这一帧画完再叫它重连。
                let view = open.view.clone();
                cx.defer(move |cx| view.update(cx, |view, cx| view.resume(cx)));
            }
        }
    }

    /// 窗口每画一帧先调：右侧面板那一栏画没画、画了哪几台，要等这一帧画到它才知道。
    pub(in crate::window) fn begin_screen_frame(&mut self) {
        self.screen_viewers.column_shown = false;
        self.screen_viewers.shown_snaps.clear();
    }

    /// 右侧那一栏：吸附在上方的几台、面板、吸附在下方的几台，每台挨着面板的那边一条分隔线拖着改它的高度。`panel`
    /// 是面板那一块缓存的视图，`width` 是这一栏的宽度。
    pub(in crate::window) fn render_right_column(
        &mut self,
        panel: &Entity<Slot>,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.screen_viewers.column_shown = true;
        let bounds = self.screen_viewers.column_bounds.clone();
        let record = canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {}).absolute().size_full();
        let column = div().relative().flex_none().w(px(width)).h_full().flex().flex_col().child(record);
        let snaps: Vec<(Entity<ScreenViewer>, Snap, _)> = self
            .snaps()
            .into_iter()
            .filter(|(open, snap)| self.snap_belongs_here(open, *snap, cx))
            .map(|(open, snap)| (open.view.clone(), snap, open.snap_bounds.clone()))
            .collect();
        self.screen_viewers.shown_snaps = snaps
            .iter()
            .map(|(view, snap, bounds)| ShownSnap { id: view.entity_id(), side: snap.side, cell: bounds.clone() })
            .collect();
        if snaps.is_empty() {
            return column.child(column_slot(panel, width, window)).into_any_element();
        }
        let cards = cards(cx);
        let gap = snap_gap(cards);
        let total: f32 = snaps.iter().map(|(_, snap, _)| snap.ratio).sum();
        // 加起来太高时（钉住的几台到了别的 workspace 又碰上那里吸附着的）画的时候先缩，给面板留地方。
        let scale = (SNAP_RATIO_MAX / total).min(1.);
        let border = cx.theme().border;
        let screen = |view: &Entity<ScreenViewer>, snap: Snap, bounds: &Rc<Cell<_>>| {
            let bounds = bounds.clone();
            let record = canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {}).absolute().size_full();
            let page = cached_page(view.clone(), window);
            let height = relative(snap.ratio * scale);
            if cards {
                Card::new()
                    .rounded_b(px(0.))
                    .relative()
                    .flex_none()
                    .h(height)
                    .overflow_hidden()
                    .child(record)
                    .child(page)
                    .into_any_element()
            } else {
                div()
                    .relative()
                    .flex_none()
                    .h(height)
                    .overflow_hidden()
                    .border_l_1()
                    .border_color(border)
                    .map(|screen| if snap.side == SnapSide::Top { screen.border_b_1() } else { screen.border_t_1() })
                    .child(record)
                    .child(page)
                    .into_any_element()
            }
        };
        let (top, bottom): (Vec<_>, Vec<_>) = snaps.iter().partition(|(_, snap, _)| snap.side == SnapSide::Top);
        // 经典样式下右侧面板顶上那条兼当标题栏、盖着右上角的按钮；上方有吸附着的远程桌面时让出这一条。
        let strip = (!cards && !top.is_empty()).then(|| {
            crate::ui::panel::panel_header(false, cx).border_l_1().w_full().window_control_area(WindowControlArea::Drag)
        });
        let strip_height = if strip.is_some() { TITLEBAR_HEIGHT } else { 0. };
        let offset = (gap - DIVIDER_GRAB_WIDTH) / 2.;
        let mut handles = Vec::new();
        let mut above = 0.;
        for (i, (view, snap, _)) in top.iter().enumerate() {
            above += snap.ratio * scale;
            let handle =
                snap_handle(view.entity_id(), cx).top(relative(above)).mt(px(strip_height + i as f32 * gap + offset));
            handles.push(handle);
        }
        let mut below = 0.;
        for (k, (view, snap, _)) in bottom.iter().rev().enumerate() {
            below += snap.ratio * scale;
            handles.push(snap_handle(view.entity_id(), cx).bottom(relative(below)).mb(px(k as f32 * gap + offset)));
        }
        let panel = cached_slot(panel, StyleRefinement::default().flex_1().min_h_0().w(px(width)), window);
        column
            .when(cards, |column| column.gap(px(CARD_GAP)))
            .children(strip)
            .children(top.iter().map(|(view, snap, bounds)| screen(view, *snap, bounds)))
            .child(panel)
            .children(bottom.iter().map(|(view, snap, bounds)| screen(view, *snap, bounds)))
            .children(handles)
            .into_any_element()
    }

    /// 拖吸附着的远程桌面 `id` 挨着面板的分隔线，分隔线跟到窗口里的纵坐标 `y`。
    pub(in crate::window) fn resize_snapped_screen(&mut self, id: EntityId, y: f32, cx: &App) {
        let viewers = &self.screen_viewers;
        let (Some(column), Some(shown)) =
            (viewers.snap_column(), viewers.shown_snaps.iter().find(|shown| shown.id == id).cloned())
        else {
            return;
        };
        let Some(bounds) = shown.bounds() else { return };
        let half_gap = snap_gap(cards(cx)) / 2.;
        let height = f32::from(column.size.height);
        let ratio = match shown.side {
            SnapSide::Top => (y - half_gap - f32::from(bounds.origin.y)) / height,
            SnapSide::Bottom => (f32::from(bounds.bottom()) - y - half_gap) / height,
        };
        let here: Vec<EntityId> = viewers.shown_snaps.iter().map(|shown| shown.id).collect();
        // 加起来超过 `SNAP_RATIO_MAX` 时画的是缩过的高度：先把这个缩放记进各台的比例，画出来的样子不变，拖的这台
        // 才能按画出来的高度改。
        let total: f32 = self.ratios(&here).sum();
        if total > SNAP_RATIO_MAX {
            let scale = SNAP_RATIO_MAX / total;
            for open in &mut self.screen_viewers.open {
                if let Placement::Snapped(snap) = &mut open.placement
                    && here.contains(&open.view.entity_id())
                {
                    snap.ratio *= scale;
                }
            }
        }
        let others: Vec<EntityId> = here.into_iter().filter(|other| *other != id).collect();
        let room = SNAP_RATIO_MAX - self.ratios(&others).sum::<f32>();
        // 别的几台缩到让不出 `SNAP_RATIO_MIN` 时以总和不超过上限为准，不然画的时候又要缩。
        let ratio = ratio.max(SNAP_RATIO_MIN).min(room);
        if let Some(ix) = self.screen_viewers.position(id)
            && let Placement::Snapped(snap) = &mut self.screen_viewers.open[ix].placement
        {
            snap.ratio = ratio;
        }
    }

    /// 双击分隔线：远程桌面 `id` 回到默认高度。
    fn reset_snap_ratio(&mut self, id: EntityId, cx: &mut Context<Self>) {
        if let Some(ix) = self.screen_viewers.position(id)
            && let Placement::Snapped(snap) = &mut self.screen_viewers.open[ix].placement
        {
            snap.ratio = SNAP_RATIO;
        }
        self.fit_snaps(cx);
        self.save(cx);
    }

    /// 右侧面板下方有吸附着的远程桌面时面板的底边在窗口里的纵坐标；没有时为空。
    pub(in crate::window) fn snapped_panel_bottom(&self, cx: &App) -> Option<f32> {
        let first = self.screen_viewers.shown_snaps.iter().find(|shown| shown.side == SnapSide::Bottom)?;
        Some(f32::from(first.bounds()?.origin.y) - snap_gap(cards(cx)))
    }

    /// 拖着的鼠标在 `position` 时松开插到右侧那一栏的哪；不在这一栏上时为空。
    pub(super) fn snap_target(&self, position: Point<Pixels>) -> Option<SnapTarget> {
        let viewers = &self.screen_viewers;
        let column = viewers.snap_column()?;
        if !column.contains(&position) {
            return None;
        }
        for shown in &viewers.shown_snaps {
            let Some(bounds) = shown.bounds() else { continue };
            if position.y >= bounds.origin.y && position.y < bounds.bottom() {
                let anchor =
                    if position.y < bounds.center().y { Anchor::Before(shown.id) } else { Anchor::After(shown.id) };
                return Some(SnapTarget { side: shown.side, anchor });
            }
        }
        let (top, bottom) = self.panel_span(column);
        let side = if position.y < (top + bottom) / 2. { SnapSide::Top } else { SnapSide::Bottom };
        Some(SnapTarget { side, anchor: Anchor::Panel })
    }

    /// 右侧那一栏里面板占的那一段的上下边：上方最后一台的底边到下方第一台的顶边。
    fn panel_span(&self, column: Bounds<Pixels>) -> (Pixels, Pixels) {
        let shown = &self.screen_viewers.shown_snaps;
        let top = shown
            .iter()
            .filter(|shown| shown.side == SnapSide::Top)
            .filter_map(ShownSnap::bounds)
            .next_back()
            .map_or(column.origin.y, |bounds| bounds.bottom());
        let bottom = shown
            .iter()
            .find(|shown| shown.side == SnapSide::Bottom)
            .and_then(ShownSnap::bounds)
            .map_or(column.bottom(), |bounds| bounds.origin.y);
        (top, bottom)
    }

    /// 标出插到 `target` 的那条线，画在拖动时盖住窗口的那一层上。
    pub(super) fn render_snap_target(&self, target: SnapTarget, cx: &App) -> Option<gpui::Div> {
        let column = self.screen_viewers.snap_column()?;
        let half_gap = px(snap_gap(cards(cx)) / 2.);
        let bounds_of = |id: EntityId| self.screen_viewers.shown_snaps.iter().find(|shown| shown.id == id)?.bounds();
        let y = match target.anchor {
            Anchor::Before(id) => bounds_of(id)?.origin.y - half_gap,
            Anchor::After(id) => bounds_of(id)?.bottom() + half_gap,
            Anchor::Panel => {
                let (top, bottom) = self.panel_span(column);
                match target.side {
                    SnapSide::Top => top + half_gap,
                    SnapSide::Bottom => bottom - half_gap,
                }
            }
        };
        let y = (y - px(TARGET_LINE / 2.)).clamp(column.origin.y, column.bottom() - px(TARGET_LINE));
        let ring = cx.theme().ring;
        Some(
            div()
                .absolute()
                .left(column.origin.x)
                .top(y)
                .w(column.size.width)
                .h(px(TARGET_LINE))
                .rounded_full()
                .bg(ring),
        )
    }

    /// 存进窗口存档的吸附着的远程桌面，从上到下。
    pub(in crate::window) fn save_snapped_screens(&self, cx: &App) -> Vec<SavedSnappedScreen> {
        self.snaps()
            .into_iter()
            .map(|(open, snap)| SavedSnappedScreen {
                host: open.view.read(cx).host().to_owned(),
                bottom: snap.side == SnapSide::Bottom,
                pinned: snap.pinned,
                workspace: self.workspaces.iter().position(|workspace| workspace.id == snap.workspace),
                ratio: Some(snap.ratio),
            })
            .collect()
    }

    /// 照窗口存档把远程桌面吸附回右侧那一栏，重新连那几台主机；焦点不给它们。`ids` 是存档里各个 workspace 建出来的
    /// 编号。
    pub(in crate::window) fn restore_snapped_screens(
        &mut self,
        saved: Vec<SavedSnappedScreen>,
        ids: &[WorkspaceId],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !SUPPORTED || saved.is_empty() {
            return;
        }
        for saved in saved {
            if self.screen_viewers.views().any(|view| view.read(cx).host() == saved.host) {
                continue;
            }
            let workspace = saved.workspace.and_then(|ix| ids.get(ix).copied()).unwrap_or(self.workspace().id);
            let side = if saved.bottom { SnapSide::Bottom } else { SnapSide::Top };
            let ratio = saved.ratio.unwrap_or(SNAP_RATIO).clamp(SNAP_RATIO_MIN, SNAP_RATIO_MAX);
            // 先不连，显示出来才连：属于别的 workspace 的不一启动就连上。
            let view = cx.new(|cx| ScreenViewer::new_suspended(saved.host, cx));
            let mut open = self.new_screen(view, window, cx);
            open.placement = Placement::Snapped(Snap { side, workspace, pinned: saved.pinned, ratio });
            set_input(&open.view, open.input(), cx);
            self.screen_viewers.column.push(open.view.entity_id());
            self.screen_viewers.open.push(open);
        }
        self.follow_pinned_screen(window, cx);
        cx.notify();
    }
}

/// 吸附着的远程桌面 `id` 挨着面板的分隔线，位置由调用方接着写。
fn snap_handle(id: EntityId, cx: &mut Context<WindowView>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(("screen-snap-divider", id.as_u64() as usize))
        .absolute()
        .left_0()
        .w_full()
        .h(px(DIVIDER_GRAB_WIDTH))
        .cursor(CursorStyle::ResizeUpDown)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                if event.click_count >= 2 {
                    this.reset_snap_ratio(id, cx);
                } else {
                    this.dragging_divider = Some(Divider::SnappedScreen(id));
                }
                cx.notify();
            }),
        )
}

/// 吸附着的远程桌面和面板、彼此之间空多宽：卡片样式下是卡片间距，经典样式下只有一条线。
fn snap_gap(cards: bool) -> f32 {
    if cards { CARD_GAP } else { 0. }
}
