//! 叠在可滚动区域上的细滚动条：鼠标在区域里或者正拖着时显示，内容没超出时不画。拖滑块滚动，
//! 点在滑块外面的轨道上，滑块中间跳到那里再接着拖。
//!
//! 放在滚动区域的父元素里、排在滚动区域后面，父元素和滚动区域一样大。位置和长度在画的时候
//! 现读 `ScrollHandle`（`gpui::list` 的是 `ListState`，见 `list_scrollbar`），滚动区域这一帧刚算好的
//! 偏移量不会晚一帧。`gpui::list` 没画过的项按估的高度算，滑块的长短会随着滚动慢慢变准；拖着滑块时
//! 列表的总高度先定住，滑块不会跑开。
//!
//! 竖的滚动条可以带改动标记（`markers`）：按在全文里的位置画在轨道上，内容能滚时一直画着，
//! 不等鼠标进来，滚的时候看得出哪里改了。
//!
//! 内容能滚时报给辅助工具：方向和滚到了哪里（0 到 1），位置只占轨道那一条，不盖住内容。

use std::{cell::Cell, ops::Range, rc::Rc};

use gpui::{
    A11ySubtreeBuilder, App, Axis, Bounds, CursorStyle, DispatchPhase, Edges, Element, ElementId, GlobalElementId,
    Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, ListState, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Orientation, Pixels, Position, Role, ScrollHandle, Size, Style, Window, accesskit,
    fill, point, px, relative, size,
};

/// 轨道的宽度，也是能按住的宽度；滑块画在轨道中间，这么粗。
const TRACK_WIDTH: f32 = 10.;
const THUMB_WIDTH: f32 = 6.;
/// 内容再长，滑块也不短于这么长。
const MIN_THUMB_LENGTH: f32 = 24.;
/// 改动标记再短也画这么长，只改了一行也看得见。
const MIN_MARKER_LENGTH: f32 = 2.;

/// `handle` 所在的滚动区域在 `axis` 方向上的滚动条，颜色是 `color` 调淡。
pub fn scrollbar(id: impl Into<ElementId>, handle: ScrollHandle, axis: Axis, color: Hsla) -> Scrollbar {
    Scrollbar { id: id.into(), handle: Handle::Scroll(handle), axis, color, markers: Vec::new(), scale: 1. }
}

/// `gpui::list` 的竖滚动条，样子和用法同 `scrollbar`。
pub fn list_scrollbar(id: impl Into<ElementId>, list: ListState, color: Hsla) -> Scrollbar {
    Scrollbar { id: id.into(), handle: Handle::List(list), axis: Axis::Vertical, color, markers: Vec::new(), scale: 1. }
}

/// 滚动条管的是哪种滚动区域。
#[derive(Clone)]
enum Handle {
    Scroll(ScrollHandle),
    List(ListState),
}

impl Handle {
    /// 沿 `axis` 的可见长度、最多能滚多远、已经滚了多远。
    fn extent(&self, axis: Axis) -> (Pixels, Pixels, Pixels) {
        let along = |point: gpui::Point<Pixels>| match axis {
            Axis::Vertical => point.y,
            Axis::Horizontal => point.x,
        };
        let (bounds, max, offset) = match self {
            Handle::Scroll(handle) => (handle.bounds(), handle.max_offset(), handle.offset()),
            Handle::List(list) => {
                (list.viewport_bounds(), list.max_offset_for_scrollbar(), list.scroll_px_offset_for_scrollbar())
            }
        };
        let viewport = match axis {
            Axis::Vertical => bounds.size.height,
            Axis::Horizontal => bounds.size.width,
        };
        (viewport, along(max), -along(offset))
    }

    /// 沿 `axis` 滚到离开头 `scrolled` 处。
    fn set(&self, axis: Axis, scrolled: Pixels) {
        match self {
            Handle::Scroll(handle) => {
                let mut offset = handle.offset();
                match axis {
                    Axis::Vertical => offset.y = -scrolled,
                    Axis::Horizontal => offset.x = -scrolled,
                }
                handle.set_offset(offset);
            }
            Handle::List(list) => list.set_offset_from_scrollbar(point(px(0.), -scrolled)),
        }
    }

    /// 开始、结束拖滑块；列表在拖着时定住总高度。
    fn drag(&self, dragging: bool) {
        if let Handle::List(list) = self {
            if dragging {
                list.scrollbar_drag_started();
            } else {
                list.scrollbar_drag_ended();
            }
        }
    }
}

/// 轨道上的一段改动标记：占全文的哪一段（0 到 1）和颜色。
pub type Marker = (Range<f32>, Hsla);

/// 共 `rows` 行、`changes` 从上往下给出改了的几行和颜色时的改动标记，挨着的同色几段并成一段。
pub fn row_markers(rows: usize, changes: impl IntoIterator<Item = (Range<usize>, Hsla)>) -> Vec<Marker> {
    let mut runs: Vec<(Range<usize>, Hsla)> = Vec::new();
    for (range, color) in changes {
        match runs.last_mut() {
            Some((run, last)) if run.end == range.start && *last == color => run.end = range.end,
            _ => runs.push((range, color)),
        }
    }
    let rows = rows.max(1) as f32;
    runs.into_iter().map(|(run, color)| (run.start as f32 / rows..run.end as f32 / rows, color)).collect()
}

pub struct Scrollbar {
    id: ElementId,
    handle: Handle,
    axis: Axis,
    color: Hsla,
    markers: Vec<Marker>,
    /// 窗口的缩放，`prepaint` 时记下；报给辅助工具的位置按物理像素算。
    scale: f32,
}

/// 跨帧留着的状态。
#[derive(Clone, Default)]
struct State {
    /// 正拖着滑块：按下时鼠标在滑块起点之后多远。
    grab: Rc<Cell<Option<Pixels>>>,
    /// 上一帧画没画，鼠标进出区域时据此决定要不要重画。
    shown: Rc<Cell<bool>>,
}

/// 沿滚动方向的几何：轨道起点和长度，滑块相对轨道起点的位置和长度，以及最多能滚多远。
#[derive(Clone, Copy)]
struct Geometry {
    track_start: Pixels,
    track_len: Pixels,
    thumb_start: Pixels,
    thumb_len: Pixels,
    max: Pixels,
}

/// 可见长度 `viewport`、最多能滚 `max`、已经滚了 `scrolled` 时，长 `track` 的轨道上滑块的起点和
/// 长度；不能滚时为空。
fn thumb(viewport: f32, max: f32, scrolled: f32, track: f32) -> Option<(f32, f32)> {
    if max < 1. || track <= 0. {
        return None;
    }
    let len = (track * viewport / (viewport + max)).max(MIN_THUMB_LENGTH).min(track);
    Some(((scrolled / max).clamp(0., 1.) * (track - len), len))
}

impl Scrollbar {
    /// 轨道上画的改动标记，只有竖的滚动条画。
    pub fn markers(mut self, markers: Vec<Marker>) -> Self {
        self.markers = markers;
        self
    }

    fn geometry(&self, bounds: Bounds<Pixels>) -> Option<Geometry> {
        let (viewport, max, scrolled) = self.handle.extent(self.axis);
        let (track_start, track_len) = match self.axis {
            Axis::Vertical => (bounds.top(), bounds.size.height),
            Axis::Horizontal => (bounds.left(), bounds.size.width),
        };
        let (start, len) = thumb(f32::from(viewport), f32::from(max), f32::from(scrolled), f32::from(track_len))?;
        Some(Geometry { track_start, track_len, thumb_start: px(start), thumb_len: px(len), max })
    }

    /// 轨道：竖的贴着右边，横的贴着底边。
    fn track(&self, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        match self.axis {
            Axis::Vertical => Bounds::new(
                point(bounds.right() - px(TRACK_WIDTH), bounds.top()),
                size(px(TRACK_WIDTH), bounds.size.height),
            ),
            Axis::Horizontal => Bounds::new(
                point(bounds.left(), bounds.bottom() - px(TRACK_WIDTH)),
                size(bounds.size.width, px(TRACK_WIDTH)),
            ),
        }
    }

    /// 把滑块起点挪到离轨道起点 `thumb_start` 处，滚动区域跟着滚。
    fn scroll_to(handle: &Handle, axis: Axis, geometry: Geometry, thumb_start: Pixels) {
        let room = geometry.track_len - geometry.thumb_len;
        let ratio = if room > px(0.) { (thumb_start / room).clamp(0., 1.) } else { 0. };
        handle.set(axis, geometry.max * ratio);
    }
}

impl IntoElement for Scrollbar {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Scrollbar {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn a11y_role(&self) -> Option<Role> {
        (self.handle.extent(self.axis).1 >= px(1.)).then_some(Role::ScrollBar)
    }

    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        let (_, max, scrolled) = self.handle.extent(self.axis);
        node.set_orientation(match self.axis {
            Axis::Vertical => Orientation::Vertical,
            Axis::Horizontal => Orientation::Horizontal,
        });
        node.set_min_numeric_value(0.);
        node.set_max_numeric_value(1.);
        node.set_numeric_value(f64::from((scrolled / max).clamp(0., 1.)));
    }

    /// 元素和整个滚动区域一样大，报给辅助工具的位置换成轨道那一条，不然点内容时命中的是滚动条。
    fn a11y_synthetic_children(&mut self, hitbox: &mut Option<Hitbox>, builder: &mut A11ySubtreeBuilder) {
        if let Some(track) = hitbox.as_ref().map(|hitbox| hitbox.bounds) {
            let scale = f64::from(self.scale);
            let (origin, end) = (track.origin, track.bottom_right());
            builder.parent_node().set_bounds(accesskit::Rect {
                x0: f64::from(origin.x) * scale,
                y0: f64::from(origin.y) * scale,
                x1: f64::from(end.x) * scale,
                y1: f64::from(end.y) * scale,
            });
        }
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            position: Position::Absolute,
            inset: Edges { top: px(0.).into(), left: px(0.).into(), ..Default::default() },
            size: Size { width: relative(1.).into(), height: relative(1.).into() },
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Option<Hitbox> {
        self.scale = window.scale_factor();
        // 轨道上的点击和悬停归滚动条，滚轮照常交给下面的滚动区域。
        self.geometry(bounds).map(|_| window.insert_hitbox(self.track(bounds), HitboxBehavior::BlockMouseExceptScroll))
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        _: &mut App,
    ) {
        let Some(id) = id else {
            return;
        };
        let state = window.with_element_state(id, |state: Option<State>, _| {
            let state = state.unwrap_or_default();
            (state.clone(), state)
        });
        let geometry = self.geometry(bounds);
        let inside = bounds.contains(&window.mouse_position());
        let dragging = state.grab.get().is_some();
        let shown = geometry.is_some() && (inside || dragging);
        state.shown.set(shown);

        // 鼠标进出区域时重画，滚动条跟着出现或消失；拖着时滑块跟着鼠标走。
        window.on_mouse_event({
            let (state, handle, axis) = (state.clone(), self.handle.clone(), self.axis);
            move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                if let (Some(grab), Some(geometry)) = (state.grab.get(), geometry) {
                    if event.pressed_button != Some(MouseButton::Left) {
                        state.grab.set(None);
                        handle.drag(false);
                    } else {
                        let along = match axis {
                            Axis::Vertical => event.position.y,
                            Axis::Horizontal => event.position.x,
                        };
                        Self::scroll_to(&handle, axis, geometry, along - geometry.track_start - grab);
                        cx.stop_propagation();
                    }
                    window.refresh();
                } else if bounds.contains(&event.position) != state.shown.get() && geometry.is_some() {
                    window.refresh();
                }
            }
        });
        window.on_mouse_event({
            let (state, handle) = (state.clone(), self.handle.clone());
            move |event: &MouseUpEvent, phase, window, _| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left && state.grab.take().is_some() {
                    handle.drag(false);
                    window.refresh();
                }
            }
        });

        let (Some(geometry), Some(hitbox)) = (geometry, hitbox.as_ref()) else {
            return;
        };
        window.set_cursor_style(CursorStyle::Arrow, hitbox);
        window.on_mouse_event({
            let (state, hitbox, handle, axis) = (state, hitbox.clone(), self.handle.clone(), self.axis);
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                    return;
                }
                let along = match axis {
                    Axis::Vertical => event.position.y,
                    Axis::Horizontal => event.position.x,
                } - geometry.track_start;
                let grab = if along >= geometry.thumb_start && along <= geometry.thumb_start + geometry.thumb_len {
                    along - geometry.thumb_start
                } else {
                    // 点在轨道上：滑块中间跳到这里，接着按住就能拖。
                    let grab = geometry.thumb_len / 2.;
                    Self::scroll_to(&handle, axis, geometry, along - grab);
                    grab
                };
                state.grab.set(Some(grab));
                handle.drag(true);
                cx.stop_propagation();
                window.refresh();
            }
        });

        let track = self.track(bounds);
        if self.axis == Axis::Vertical {
            for (range, color) in &self.markers {
                let len = (geometry.track_len * (range.end - range.start)).max(px(MIN_MARKER_LENGTH));
                let start = geometry.track_start + geometry.track_len * range.start;
                window.paint_quad(fill(Bounds::new(point(track.left(), start), size(track.size.width, len)), *color));
            }
        }
        if !shown {
            return;
        }
        let inset = px((TRACK_WIDTH - THUMB_WIDTH) / 2.);
        let thumb = match self.axis {
            Axis::Vertical => Bounds::new(
                point(track.left() + inset, geometry.track_start + geometry.thumb_start),
                size(px(THUMB_WIDTH), geometry.thumb_len),
            ),
            Axis::Horizontal => Bounds::new(
                point(geometry.track_start + geometry.thumb_start, track.top() + inset),
                size(geometry.thumb_len, px(THUMB_WIDTH)),
            ),
        };
        let active = dragging || hitbox.is_hovered(window);
        let color = self.color.opacity(if active { 0.5 } else { 0.28 });
        window.paint_quad(fill(thumb, color).corner_radii(px(THUMB_WIDTH / 2.)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_follows_the_scroll_position() {
        // 内容是可见部分的 4 倍：滑块占轨道的四分之一，滚到一半时在中间。
        assert_eq!(thumb(100., 300., 0., 100.), Some((0., 25.)));
        assert_eq!(thumb(100., 300., 150., 100.), Some((37.5, 25.)));
        assert_eq!(thumb(100., 300., 300., 100.), Some((75., 25.)));
        // 内容很长时滑块不短于最小长度；滚过头时停在两端。
        assert_eq!(thumb(100., 100_000., 200_000., 100.), Some((100. - MIN_THUMB_LENGTH, MIN_THUMB_LENGTH)));
        assert_eq!(thumb(100., 0., 0., 100.), None);
    }

    #[test]
    fn adjacent_rows_of_one_color_merge_into_one_marker() {
        let (green, red) = (gpui::green(), gpui::red());
        let markers = row_markers(10, [(1..2, green), (2..3, green), (3..4, red), (5..6, red)]);
        assert_eq!(markers, vec![(0.1..0.3, green), (0.3..0.4, red), (0.5..0.6, red)]);
    }
}
