//! 浮动的终端：标签里的一个终端浮到别的分屏上面，拖标题挪、拖四边四角改大小，按「停靠」回到浮起来前的位置；
//! 拖到窗口外松开或按「搬进单独窗口」，就搬进单独的终端窗口（`popout`）。
//!
//! 浮着的终端仍在分屏树（`Tab::root`）里原来的位置上，画分屏时从树的副本里拿掉（`Tab::docked_root`），所以停靠
//! 回去时位置、比例都和浮起来前一样。标签里总留着一个不浮动的终端。浮着的位置和大小按终端区归一化
//! （`Tab::floating`），窗口改大小时跟着缩放。搬进单独窗口的终端仍算这个标签里浮着的（`Tab::popped`），存档、
//! 布局照浮着的记；主窗口的当前终端（`Tab::focused`）不会是它，主窗口的焦点交不到别的窗口里。

mod popout;

use std::collections::HashMap;

use gpui::{
    AnyElement, App, Bounds, Context, CursorStyle, Div, EntityId, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, ResizeEdge, SharedString, Size, Window, actions, div, point, prelude::*, px, relative, size,
};
use runode_types::pane::{Node, Rect};

use super::{
    WindowView,
    float_frame::{self, clamp_frame, resize_frame},
    model::Tab,
};

actions!(
    runode,
    [
        /// 当前分屏浮到别的分屏上面；浮着的停靠回原来的位置。
        TogglePaneFloat,
        /// 当前分屏搬进单独的终端窗口。
        MovePaneToWindow
    ]
);

/// 浮动的终端最小这么大，放得下标题条和几行字。
const MIN_FLOAT: Size<Pixels> = Size { width: px(280.), height: px(160.) };
/// 第一块浮起来的终端在终端区里的位置和大小。
const FIRST_FRAME: Rect = Rect { x: 0.2, y: 0.15, width: 0.6, height: 0.65 };
/// 再浮起一块时比最上面那块往右下错开终端区的这么多。
const CASCADE: f32 = 0.03;

/// 正在拖浮动的终端 `pane`：从窗口里的 `from` 按下，那时它在终端区里的 `frame`，终端区大小是 `area`；`resize` 有值时
/// 拖的是那条边或那个角。
pub(super) struct FloatDrag {
    pane: EntityId,
    resize: Option<ResizeEdge>,
    from: Point<Pixels>,
    frame: Bounds<Pixels>,
    area: Size<Pixels>,
}

impl Tab {
    /// 终端 `id` 浮着。
    pub(super) fn floats(&self, id: EntityId) -> bool {
        self.floating.iter().any(|(pane, _)| *pane == id)
    }

    /// 画分屏用的树：拿掉浮着的终端，留下的按原来的比例占满终端区。
    pub(super) fn docked_root(&self) -> Node<EntityId> {
        let mut root = self.root.clone();
        for (id, _) in &self.floating {
            root.remove(*id);
        }
        root
    }

    /// 终端 `id` 能浮起来：还没浮着，标签里另有不浮动的终端。
    pub(super) fn can_float(&self, id: EntityId) -> bool {
        !self.floats(id) && self.root.leaves().into_iter().any(|other| other != id && !self.floats(other))
    }

    /// 去掉终端 `id` 的浮动，它回到分屏树里原来的位置，在单独窗口里的也放回来；返回它原来浮着没有。
    pub(super) fn unfloat(&mut self, id: EntityId) -> bool {
        self.popped.remove(&id);
        let before = self.floating.len();
        self.floating.retain(|(pane, _)| *pane != id);
        self.floating.len() != before
    }

    /// 不浮动的终端都关了、挪走了时，最下面那块浮着的停靠回去，先挑不在单独窗口里的：终端区不能只剩浮着的。
    /// 当前终端不能是单独窗口里的，换成不浮动的。
    pub(super) fn keep_one_docked(&mut self) {
        if !self.floating.is_empty() && self.root.leaves().into_iter().all(|id| self.floats(id)) {
            let ix = self.floating.iter().position(|(id, _)| !self.popped.contains(id)).unwrap_or(0);
            let (id, _) = self.floating.remove(ix);
            self.popped.remove(&id);
        }
        if self.popped.contains(&self.focused)
            && let Some(docked) = self.docked_root().leaves().first()
        {
            self.focused = *docked;
        }
    }

    /// 新浮起一块放在哪：没有浮着的时用 `FIRST_FRAME`，有了就比最上面那块往右下错开，不出终端区。
    fn next_frame(&self) -> Rect {
        let Some((_, top)) = self.floating.last() else {
            return FIRST_FRAME;
        };
        let x = (top.x + CASCADE).min(1. - top.width).max(0.);
        let y = (top.y + CASCADE).min(1. - top.height).max(0.);
        Rect { x, y, ..*top }
    }
}

impl WindowView {
    pub(super) fn toggle_pane_float(&mut self, _: &TogglePaneFloat, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.tab().map(|tab| tab.focused) else {
            return;
        };
        if self.tab().is_some_and(|tab| tab.floats(id)) {
            self.dock_pane(id, window, cx);
        } else {
            self.float_pane(id, window, cx);
        }
    }

    /// 当前标签里的终端 `id` 浮到别的分屏上面，焦点给它；它是标签里唯一不浮动的终端时不浮。
    pub(super) fn float_pane(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_mut().filter(|tab| tab.can_float(id)) else {
            return;
        };
        let frame = tab.next_frame();
        tab.floating.push((id, frame));
        tab.collapsed.remove(&id);
        self.focus_pane_in_active_tab(id, window, cx);
    }

    /// 当前标签里浮着的终端 `id` 停靠回浮起来前的位置，焦点给它。
    pub(super) fn dock_pane(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if self.float_drag.as_ref().is_some_and(|drag| drag.pane == id) {
            self.float_drag = None;
        }
        if self.tab_mut().is_some_and(|tab| tab.unfloat(id)) {
            self.focus_pane_in_active_tab(id, window, cx);
        }
    }

    pub(super) fn move_pane_to_window(&mut self, _: &MovePaneToWindow, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tab().map(|tab| tab.focused) {
            self.pop_out_pane(id, None, window, cx);
        }
    }

    /// 当前标签里的终端 `id` 搬进单独的终端窗口，没浮着的先浮起来（标签里唯一不浮动的终端搬不走）。给了 `origin`
    /// （相对这个窗口所在显示器的位置）时窗口放在那里，否则放在它浮着的地方。
    pub(super) fn pop_out_pane(
        &mut self,
        id: EntityId,
        origin: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let area = self.layout.borrow().area;
        let Some(tab) = self.tab_mut() else {
            return;
        };
        if tab.popped.contains(&id) || !(tab.floats(id) || tab.can_float(id)) {
            return;
        }
        if !tab.floats(id) {
            let frame = tab.next_frame();
            tab.floating.push((id, frame));
            tab.collapsed.remove(&id);
        }
        tab.popped.insert(id);
        tab.keep_one_docked();
        let Some(&(_, frame)) = tab.floating.iter().find(|(pane, _)| *pane == id) else {
            return;
        };
        let view = tab.panes[&id].0.clone();
        if self.float_drag.as_ref().is_some_and(|drag| drag.pane == id) {
            self.float_drag = None;
        }
        // 窗口照它浮着时的大小，加上标题栏，放在这个窗口所在的显示器上。
        let area = area.unwrap_or_else(|| Bounds::new(Point::default(), window.viewport_size()));
        let (width, height) = (f32::from(area.size.width), f32::from(area.size.height));
        let at = window.bounds().origin + area.origin + point(px(frame.x * width), px(frame.y * height));
        let size = size(px(frame.width * width), px(frame.height * height + super::TITLEBAR_HEIGHT));
        let bounds = Bounds { origin: origin.unwrap_or(at), size };
        let display = window.display(cx);
        let bounds = match &display {
            Some(display) => float_frame::clamp_to_display(bounds, display.bounds(), MIN_FLOAT),
            None => bounds,
        };
        if let Some(owner) = window.window_handle().downcast::<WindowView>() {
            let (entity, display) = (cx.entity(), display.map(|display| display.id()));
            cx.defer(move |cx| popout::open(owner, entity, id, view, bounds, display, cx));
        }
        self.activate(self.workspace().active, window, cx);
    }

    /// 单独窗口里的终端 `id` 放回它的标签里浮着，切过去，焦点给它。
    pub(super) fn pop_in_pane(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((wi, ti)) = self.locate(id) else {
            return;
        };
        let tab = &mut self.workspaces[wi].tabs[ti];
        if !tab.popped.remove(&id) {
            return;
        }
        let view = tab.panes[&id].0.clone();
        view.update(cx, |view, cx| view.attach_window(window, cx));
        window.activate_window();
        self.show_pane(id, window, cx);
    }

    /// 终端 `id` 在单独窗口里。
    pub(super) fn holds_popped(&self, id: EntityId) -> bool {
        self.workspaces.iter().any(|workspace| workspace.tabs.iter().any(|tab| tab.popped.contains(&id)))
    }

    /// 终端 `id` 在单独窗口里，那个窗口在前台、看得见。
    pub(super) fn watching_popped(&self, id: EntityId, cx: &App) -> bool {
        self.holds_popped(id) && popout::watched(id, cx)
    }

    /// 终端 `id` 在单独窗口里时把那个窗口提到前面、焦点给它，返回是不是。
    pub(super) fn activate_popped(&self, id: EntityId, cx: &mut App) -> bool {
        self.holds_popped(id) && popout::activate(id, cx)
    }

    /// 浮着的终端 `id` 提到别的浮着的上面；返回挪了没有。
    fn raise_float(&mut self, id: EntityId) -> bool {
        let Some(tab) = self.tab_mut() else {
            return false;
        };
        match tab.floating.iter().position(|(pane, _)| *pane == id) {
            Some(ix) if ix + 1 != tab.floating.len() => {
                let entry = tab.floating.remove(ix);
                tab.floating.push(entry);
                true
            }
            _ => false,
        }
    }

    /// 从窗口里的 `from` 开始拖浮着的终端 `id`，`resize` 有值时拖的是那条边或那个角；它提到最上面。
    pub(super) fn start_float_drag(
        &mut self,
        id: EntityId,
        from: Point<Pixels>,
        resize: Option<ResizeEdge>,
        cx: &mut Context<Self>,
    ) {
        let Some(area) = self.layout.borrow().area.map(|area| area.size) else {
            return;
        };
        let Some(&(_, rect)) = self.tab().and_then(|tab| tab.floating.iter().find(|(pane, _)| *pane == id)) else {
            return;
        };
        let (width, height) = (f32::from(area.width), f32::from(area.height));
        let frame = Bounds {
            origin: point(px(rect.x * width), px(rect.y * height)),
            size: size(px(rect.width * width), px(rect.height * height)),
        };
        self.float_drag = Some(FloatDrag { pane: id, resize, from, frame, area });
        self.raise_float(id);
        cx.notify();
    }

    fn end_float_drag(&mut self, cx: &mut Context<Self>) {
        if self.float_drag.take().is_some() {
            self.save(cx);
            cx.notify();
        }
    }

    /// 当前标签里浮着的终端，从下到上；摆在终端区里，位置按终端区的比例。按下哪块哪块提到最上面，四边四角拖着改大小。
    pub(super) fn render_floating_panes(
        &self,
        tab: &Tab,
        badges: &HashMap<EntityId, SharedString>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        tab.floating
            .iter()
            .filter(|(id, _)| !tab.popped.contains(id))
            .map(|&(id, frame)| {
                let grips = float_frame::resize_grips(
                    id,
                    |this, id, from, edge, cx| this.start_float_drag(id, from, Some(edge), cx),
                    cx,
                );
                div()
                    .id(("pane-float", id))
                    .absolute()
                    .left(relative(frame.x))
                    .top(relative(frame.y))
                    .w(relative(frame.width))
                    .h(relative(frame.height))
                    .occlude()
                    .rounded_lg()
                    .shadow_lg()
                    // 按下时先提到最上面，按下的事照常交给终端。
                    .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                        if this.raise_float(id) {
                            cx.notify();
                        }
                    }))
                    .child(self.render_leaf(tab, id, badges, true, window, cx))
                    .children(grips)
                    .into_any_element()
            })
            .collect()
    }

    /// 拖浮着的终端时盖住整个窗口、接住鼠标移动和松开的一层；没在拖时为空。
    pub(super) fn render_float_drag(&self, cx: &mut Context<Self>) -> Option<Div> {
        let drag = self.float_drag.as_ref()?;
        Some(
            div()
                .absolute()
                .size_full()
                .occlude()
                .cursor(drag.resize.map_or(CursorStyle::ClosedHand, float_frame::resize_cursor))
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    cx.stop_propagation();
                    // 在窗口外松开时可能收不到松开事件，回来时按键已经没按着了。
                    if event.pressed_button != Some(MouseButton::Left) {
                        this.end_float_drag(cx);
                        return;
                    }
                    let Some(drag) = &this.float_drag else { return };
                    let delta = event.position - drag.from;
                    let frame = match drag.resize {
                        Some(edge) => resize_frame(drag.frame, edge, delta, drag.area, MIN_FLOAT),
                        None => Bounds { origin: drag.frame.origin + delta, size: drag.frame.size },
                    };
                    let (pane, rect) = (drag.pane, normalized(clamp_frame(frame, drag.area, MIN_FLOAT), drag.area));
                    if let Some(tab) = this.tab_mut()
                        && let Some(entry) = tab.floating.iter_mut().find(|(id, _)| *id == pane)
                        && entry.1 != rect
                    {
                        entry.1 = rect;
                        cx.notify();
                    }
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_float_drag(cx)))
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseUpEvent, window, cx| {
                        let Some(drag) = this.float_drag.take() else { return };
                        // 松在窗口里（落在盖在上面的窗口按钮、拉伸边上时也算「外面」）只是拖完了。
                        let inside = Bounds::new(Point::default(), window.viewport_size()).contains(&event.position);
                        if drag.resize.is_some() || inside {
                            this.save(cx);
                            cx.notify();
                            return;
                        }
                        // 窗口在它那块显示器上的位置，加上松开的地方，减去按下时鼠标在这一块里的位置。
                        let area = this.layout.borrow().area.map_or(Point::default(), |area| area.origin);
                        let grab = drag.from - area - drag.frame.origin;
                        let origin = window.bounds().origin + event.position - grab;
                        this.pop_out_pane(drag.pane, Some(origin), window, cx);
                    }),
                ),
        )
    }
}

/// 终端区里的 `frame` 按终端区的大小 `area` 归一化。
fn normalized(frame: Bounds<Pixels>, area: Size<Pixels>) -> Rect {
    let (width, height) = (f32::from(area.width).max(1.), f32::from(area.height).max(1.));
    Rect {
        x: f32::from(frame.origin.x) / width,
        y: f32::from(frame.origin.y) / height,
        width: f32::from(frame.size.width) / width,
        height: f32::from(frame.size.height) / height,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use runode_types::pane::Axis;

    use super::*;

    fn id(n: u64) -> EntityId {
        EntityId::from((1u64 << 32) | n)
    }

    /// 左边 1，右边上 2、下 3 的标签，焦点在 1。
    fn tab() -> Tab {
        let mut root = Node::Leaf(id(1));
        root.split(id(1), id(2), Axis::Horizontal, 10);
        root.split(id(2), id(3), Axis::Vertical, 11);
        Tab {
            id: 1,
            root,
            panes: HashMap::new(),
            focused: id(1),
            zoomed: false,
            collapsed: HashSet::new(),
            bell: false,
            done: HashSet::new(),
            floating: Vec::new(),
            popped: HashSet::new(),
        }
    }

    #[test]
    fn floating_panes_keep_their_place_in_the_tree() {
        let mut tab = tab();
        tab.floating.push((id(2), FIRST_FRAME));
        assert_eq!(tab.docked_root().leaves(), [id(1), id(3)]);
        assert_eq!(tab.root.leaves(), [id(1), id(2), id(3)]);
        assert!(tab.unfloat(id(2)));
        assert!(!tab.unfloat(id(2)));
        assert_eq!(tab.docked_root(), tab.root);
    }

    #[test]
    fn one_pane_always_stays_docked() {
        let mut tab = tab();
        assert!(tab.can_float(id(1)));
        tab.floating.push((id(1), FIRST_FRAME));
        tab.floating.push((id(2), FIRST_FRAME));
        assert!(!tab.can_float(id(2)), "already floating");
        assert!(!tab.can_float(id(3)), "the last docked pane");
        // 最后一个不浮动的关掉后，最下面那块浮着的停靠回去。
        tab.root.remove(id(3));
        tab.keep_one_docked();
        assert_eq!(tab.floating.iter().map(|(pane, _)| *pane).collect::<Vec<_>>(), [id(2)]);
    }

    #[test]
    fn new_floats_cascade_without_leaving_the_area() {
        let mut tab = tab();
        assert_eq!(tab.next_frame(), FIRST_FRAME);
        tab.floating.push((id(1), FIRST_FRAME));
        let next = tab.next_frame();
        assert!((next.x - (FIRST_FRAME.x + CASCADE)).abs() < 1e-6 && next.width == FIRST_FRAME.width);
        for _ in 0..40 {
            let next = tab.next_frame();
            tab.floating.push((id(2), next));
        }
        let (_, top) = tab.floating.last().unwrap();
        assert!(top.x + top.width <= 1. + 1e-6 && top.y + top.height <= 1. + 1e-6);
    }

    #[test]
    fn frames_are_kept_as_fractions_of_the_area() {
        let area = size(px(1000.), px(500.));
        let frame = Bounds { origin: point(px(250.), px(100.)), size: size(px(500.), px(250.)) };
        assert_eq!(normalized(frame, area), Rect { x: 0.25, y: 0.2, width: 0.5, height: 0.5 });
    }
}
