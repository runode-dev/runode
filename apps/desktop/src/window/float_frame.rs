//! 浮在窗口内容上面的一块（远程桌面、浮动的终端）的几何：拖边或角改大小、限在所在的区域里，以及四边四角接住
//! 按下的那几条把手。位置都相对所在区域的左上角。

use gpui::{
    Bounds, Context, CursorStyle, Div, EntityId, MouseButton, MouseDownEvent, Pixels, Point, ResizeEdge, Size,
    Stateful, div, point, prelude::*, px, size,
};

use super::WindowView;

/// 四角拖着改大小的那块的边长。
const RESIZE_GRIP: f32 = 14.;
/// 四边拖着改大小的那条的宽度。
const RESIZE_EDGE: f32 = 6.;

/// 拖边或角 `edge` 挪了 `delta` 后那块在哪：对面的边不动，拖的边不越出大小为 `area` 的区域，也不缩到比 `min` 小。
pub(super) fn resize_frame(
    frame: Bounds<Pixels>,
    edge: ResizeEdge,
    delta: Point<Pixels>,
    area: Size<Pixels>,
    min: Size<Pixels>,
) -> Bounds<Pixels> {
    use ResizeEdge::*;
    let (mut left, mut top) = (f32::from(frame.origin.x), f32::from(frame.origin.y));
    let (mut right, mut bottom) = (left + f32::from(frame.size.width), top + f32::from(frame.size.height));
    let (min_w, min_h) = (f32::from(min.width), f32::from(min.height));
    let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
    if matches!(edge, Left | TopLeft | BottomLeft) {
        left = (left + dx).min(right - min_w).max(0.);
    }
    if matches!(edge, Right | TopRight | BottomRight) {
        right = (right + dx).max(left + min_w).min(f32::from(area.width));
    }
    if matches!(edge, Top | TopLeft | TopRight) {
        top = (top + dy).min(bottom - min_h).max(0.);
    }
    if matches!(edge, Bottom | BottomLeft | BottomRight) {
        bottom = (bottom + dy).max(top + min_h).min(f32::from(area.height));
    }
    Bounds { origin: point(px(left), px(top)), size: size(px(right - left), px(bottom - top)) }
}

/// 把那块限在大小为 `area` 的区域里：不比区域大，整块都在区域里，区域放得下时不比 `min` 小。
pub(super) fn clamp_frame(frame: Bounds<Pixels>, area: Size<Pixels>, min: Size<Pixels>) -> Bounds<Pixels> {
    let (aw, ah) = (f32::from(area.width), f32::from(area.height));
    let w = f32::from(frame.size.width).min(aw).max(f32::from(min.width).min(aw));
    let h = f32::from(frame.size.height).min(ah).max(f32::from(min.height).min(ah));
    let x = f32::from(frame.origin.x).clamp(0., (aw - w).max(0.));
    let y = f32::from(frame.origin.y).clamp(0., (ah - h).max(0.));
    Bounds { origin: point(px(x), px(y)), size: size(px(w), px(h)) }
}

pub(super) fn resize_cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

/// 拖着改大小的开始：那块 `id` 的边或角从窗口里的这一点按下。
pub(super) type StartResize = fn(&mut WindowView, EntityId, Point<Pixels>, ResizeEdge, &mut Context<WindowView>);

/// 那块 `id` 四边四角拖着改大小的把手，放在它里面贴边摆；按下时调 `start`。四角排在四边后面，叠在上面先接住按下。
pub(super) fn resize_grips(id: EntityId, start: StartResize, cx: &mut Context<WindowView>) -> [Stateful<Div>; 8] {
    let grip = |name: &'static str, edge: ResizeEdge| {
        div().id((name, id.as_u64() as usize)).absolute().cursor(resize_cursor(edge)).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                start(this, id, event.position, edge, cx);
            }),
        )
    };
    let (edge, corner) = (px(RESIZE_EDGE), px(RESIZE_GRIP));
    [
        grip("float-resize-top", ResizeEdge::Top).top_0().left_0().w_full().h(edge),
        grip("float-resize-bottom", ResizeEdge::Bottom).bottom_0().left_0().w_full().h(edge),
        grip("float-resize-left", ResizeEdge::Left).top_0().left_0().h_full().w(edge),
        grip("float-resize-right", ResizeEdge::Right).top_0().right_0().h_full().w(edge),
        grip("float-resize-top-left", ResizeEdge::TopLeft).top_0().left_0().size(corner),
        grip("float-resize-top-right", ResizeEdge::TopRight).top_0().right_0().size(corner),
        grip("float-resize-bottom-left", ResizeEdge::BottomLeft).bottom_0().left_0().size(corner),
        grip("float-resize-bottom-right", ResizeEdge::BottomRight).bottom_0().right_0().size(corner),
    ]
}

/// 要单独开的窗口 `bounds` 限在显示器的范围 `display`（`PlatformDisplay::bounds`）里。两者在同一套坐标里：macOS
/// 上窗口的位置相对它所在的显示器，显示器的原点是 0；别的系统上都是整个桌面的坐标，显示器带着它在桌面上的位置。
pub(super) fn clamp_to_display(bounds: Bounds<Pixels>, display: Bounds<Pixels>, min: Size<Pixels>) -> Bounds<Pixels> {
    let local = Bounds { origin: bounds.origin - display.origin, size: bounds.size };
    let clamped = clamp_frame(local, display.size, min);
    Bounds { origin: clamped.origin + display.origin, size: clamped.size }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_on_a_secondary_display_stays_there() {
        let display = Bounds { origin: point(px(1920.), px(-200.)), size: size(px(1600.), px(900.)) };
        let inside = Bounds { origin: point(px(2400.), px(0.)), size: size(px(500.), px(300.)) };
        assert_eq!(clamp_to_display(inside, display, MIN), inside);
        let past = Bounds { origin: point(px(3400.), px(-500.)), size: size(px(500.), px(300.)) };
        assert_eq!(clamp_to_display(past, display, MIN).origin, point(px(3020.), px(-200.)));
    }

    const MIN: Size<Pixels> = Size { width: px(360.), height: px(240.) };

    #[test]
    fn the_frame_stays_inside_the_area() {
        let area = size(px(1200.), px(800.));
        let outside = Bounds { origin: point(px(1100.), px(-50.)), size: size(px(500.), px(300.)) };
        let frame = clamp_frame(outside, area, MIN);
        assert_eq!(frame.origin, point(px(700.), px(0.)));
        assert_eq!(frame.size, size(px(500.), px(300.)));
        let huge = Bounds { origin: point(px(0.), px(0.)), size: size(px(5000.), px(5000.)) };
        assert_eq!(clamp_frame(huge, area, MIN).size, area);
        let tiny = Bounds { origin: point(px(10.), px(10.)), size: size(px(10.), px(10.)) };
        assert_eq!(clamp_frame(tiny, area, MIN).size, MIN);
    }

    #[test]
    fn resizing_keeps_the_opposite_edges() {
        let area = size(px(1200.), px(800.));
        let frame = Bounds { origin: point(px(100.), px(100.)), size: size(px(500.), px(300.)) };
        let grown = resize_frame(frame, ResizeEdge::TopLeft, point(px(-50.), px(-30.)), area, MIN);
        assert_eq!(grown, Bounds { origin: point(px(50.), px(70.)), size: size(px(550.), px(330.)) });
        let past_area = resize_frame(frame, ResizeEdge::Left, point(px(-500.), px(0.)), area, MIN);
        assert_eq!(past_area, Bounds { origin: point(px(0.), px(100.)), size: size(px(600.), px(300.)) });
        let shrunk = resize_frame(frame, ResizeEdge::Top, point(px(0.), px(500.)), area, MIN);
        assert_eq!(shrunk, Bounds { origin: point(px(100.), px(160.)), size: size(px(500.), MIN.height) });
        let wider = resize_frame(frame, ResizeEdge::Right, point(px(900.), px(0.)), area, MIN);
        assert_eq!(wider.origin, frame.origin);
        assert_eq!(wider.size, size(px(1100.), px(300.)));
    }
}
