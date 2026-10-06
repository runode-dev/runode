//! 叠在可滚动区域上的细滚动条：鼠标在区域里或者正拖着时显示，内容没超出时不画。拖滑块滚动，
//! 点在滑块外面的轨道上，滑块中间跳到那里再接着拖。
//!
//! 放在滚动区域的父元素里、排在滚动区域后面，父元素和滚动区域一样大。位置和长度在画的时候
//! 现读 `ScrollHandle`，滚动区域这一帧刚算好的偏移量不会晚一帧。

use std::{cell::Cell, rc::Rc};

use gpui::{
    App, Axis, Bounds, CursorStyle, DispatchPhase, Edges, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior,
    Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Position, ScrollHandle, Size, Style, Window, fill, point, px, relative, size,
};

/// 轨道的宽度，也是能按住的宽度；滑块画在轨道中间，这么粗。
const TRACK_WIDTH: f32 = 10.;
const THUMB_WIDTH: f32 = 6.;
/// 内容再长，滑块也不短于这么长。
const MIN_THUMB_LENGTH: f32 = 24.;

/// `handle` 所在的滚动区域在 `axis` 方向上的滚动条，颜色是 `color` 调淡。
pub fn scrollbar(id: impl Into<ElementId>, handle: ScrollHandle, axis: Axis, color: Hsla) -> Scrollbar {
    Scrollbar { id: id.into(), handle, axis, color }
}

pub struct Scrollbar {
    id: ElementId,
    handle: ScrollHandle,
    axis: Axis,
    color: Hsla,
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
    fn along(&self, point: gpui::Point<Pixels>) -> Pixels {
        match self.axis {
            Axis::Vertical => point.y,
            Axis::Horizontal => point.x,
        }
    }

    fn geometry(&self, bounds: Bounds<Pixels>) -> Option<Geometry> {
        let viewport = match self.axis {
            Axis::Vertical => self.handle.bounds().size.height,
            Axis::Horizontal => self.handle.bounds().size.width,
        };
        let max = self.along(self.handle.max_offset());
        let scrolled = -self.along(self.handle.offset());
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
    fn scroll_to(handle: &ScrollHandle, axis: Axis, geometry: Geometry, thumb_start: Pixels) {
        let room = geometry.track_len - geometry.thumb_len;
        let ratio = if room > px(0.) { (thumb_start / room).clamp(0., 1.) } else { 0. };
        let mut offset = handle.offset();
        match axis {
            Axis::Vertical => offset.y = -geometry.max * ratio,
            Axis::Horizontal => offset.x = -geometry.max * ratio,
        }
        handle.set_offset(offset);
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
            let state = state.clone();
            move |event: &MouseUpEvent, phase, window, _| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left && state.grab.take().is_some() {
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
                cx.stop_propagation();
                window.refresh();
            }
        });

        if !shown {
            return;
        }
        let track = self.track(bounds);
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
}
