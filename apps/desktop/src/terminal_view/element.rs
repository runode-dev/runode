//! 终端网格的 GPUI 元素：布局时按单元格尺寸调整终端大小、启动 shell，绘制时挂上输入和鼠标事件再画一帧。

use gpui::{
    App, Bounds, ContentMask, DispatchPhase, ElementId, ElementInputHandler, GlobalElementId, LayoutId, MouseMoveEvent,
    MouseUpEvent, Pixels, Size, Style, Window, fill, linear_color_stop, linear_gradient, point, prelude::*, px,
    relative, size,
};
use runode_shared_types::{color::Rgb, grid::GridSize};
use runode_terminal::session::Session;

use super::{
    TerminalView,
    crop::{Edges, pad_color},
    hsla,
    paint::paint_frame,
};
use crate::startup;

pub(super) struct TerminalElement {
    pub(super) view: gpui::Entity<TerminalView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            let first_layout = view.metrics.is_none();
            let metrics = view.metrics(window);
            let cols = (f32::from(bounds.size.width) / f32::from(metrics.cell.width)).floor();
            let rows = (f32::from(bounds.size.height) / f32::from(metrics.cell.height)).floor();
            let scale = window.scale_factor();
            let size = GridSize {
                cols: cols.clamp(1., u16::MAX as f32) as u16,
                rows: rows.clamp(1., u16::MAX as f32) as u16,
                cell_width_px: (f32::from(metrics.cell.width) * scale).round() as u16,
                cell_height_px: (f32::from(metrics.cell.height) * scale).round() as u16,
            };
            // 进程里第一个量出尺寸的终端就是启动时那个，记下来，下次启动好提前拉起 shell。
            if first_layout {
                crate::prespawn::remember(&view.config, size);
            }
            if view.adopted_size.take().is_some_and(|adopted| adopted != size) {
                view.respawn(size, window, cx);
            } else {
                view.screen.resize(size);
            }
            // 尺寸已经按实际设好，现在启动 shell；放到这一帧画完再做，启动失败时要关掉终端。还在等
            // 宿主给屏幕时实际尺寸还没请宿主改，等看上了（之后还会布局）再启动。
            if view.start_pending && view.screen.live().is_some() {
                view.start_pending = false;
                cx.defer_in(window, |view, _, cx| view.start_now(cx));
            }
            view.grid_origin = bounds.origin;
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        // 窗口第一次画终端，这时多半还只有背景。
        static FIRST_PAINT: startup::Once = startup::Once::new();
        FIRST_PAINT.mark_when("first_paint", || true);
        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.view.clone()), cx);
        // 移动和松开挂在窗口上：拖到网格外甚至窗口外时也要收到。
        window.on_mouse_event({
            let view = self.view.clone();
            move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    let inside = bounds.contains(&event.position);
                    view.update(cx, |view, cx| view.mouse_move(event, inside, cx));
                }
            }
        });
        window.on_mouse_event({
            let view = self.view.clone();
            move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    view.update(cx, |view, cx| view.mouse_up(event, cx));
                }
            }
        });
        let focused = focus_handle.is_focused(window);
        self.view.update(cx, |view, cx| {
            let metrics = view.metrics(window);
            view.refresh_input();
            // 绘制时要同时用到帧和 `&mut view`（字形缓存），所以先把帧取出来，画完再放回。没有界面
            // 这份 VT（只看状态、还在等屏幕、断开时没有屏幕）时只有背景。
            let Some(frame) = view.screen.shown_mut().map(Session::take_frame) else {
                view.sync_cursor_blink(false, cx);
                return;
            };
            // 闪烁计时器按这一帧起停。窗口刚建好时设的焦点不触发 `on_focus`，提前启动的 shell 的
            // 输出也早已喂完，所以要在画的时候起。
            view.sync_cursor_blink(focused && frame.cursor.is_some_and(|c| c.blinking), cx);
            let crop = view.crop_for(&frame);
            match crop {
                None => {
                    paint_frame(view, &frame, bounds.origin, metrics, focused, window);
                    // 补全菜单盖在终端内容上面。
                    view.paint_completion(&frame, metrics, window);
                }
                // 尺寸归别的前端管、VT 和视图对不上：只画 VT 落在视图里的那一块，裁掉的那几边渐隐，VT
                // 小了空出来的地方留白。
                Some(crop) => {
                    let view_size = view.screen.last_size();
                    let vt = GridSize { cols: frame.cols, rows: frame.rows, ..view_size };
                    paint_padding(bounds, vt, metrics.cell, pad_color(frame.background, frame.foreground), window);
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        let origin = crop.vt_origin(bounds.origin, metrics.cell);
                        paint_frame(view, &frame, origin, metrics, focused, window);
                        paint_fades(bounds, crop.edges(vt, view_size), metrics.cell, frame.background, window);
                        view.paint_completion(&frame, metrics, window);
                    });
                }
            }
            // 第一次画出字（多半是 shell 的提示符）的那一帧。
            static FIRST_CONTENT: startup::Once = startup::Once::new();
            FIRST_CONTENT.mark_when("first_content", || frame.cells.iter().any(|cell| !cell.text.trim().is_empty()));
            if let Some(session) = view.screen.shown_mut() {
                session.restore_frame(frame);
            }
        });
    }
}

/// VT 比视图小时空出来的右边和下边用较暗的底色 `color` 铺上。VT 从视图左上角画起。
fn paint_padding(bounds: Bounds<Pixels>, vt: GridSize, cell: Size<Pixels>, color: Rgb, window: &mut Window) {
    let used = size(cell.width * f32::from(vt.cols), cell.height * f32::from(vt.rows));
    let color = hsla(color);
    if used.width < bounds.size.width {
        let strip = size(bounds.size.width - used.width, bounds.size.height);
        window.paint_quad(fill(Bounds::new(bounds.origin + point(used.width, px(0.)), strip), color));
    }
    if used.height < bounds.size.height {
        let strip = size(used.width.min(bounds.size.width), bounds.size.height - used.height);
        window.paint_quad(fill(Bounds::new(bounds.origin + point(px(0.), used.height), strip), color));
    }
}

/// 裁掉了内容的那几边画一格宽的渐隐：从透明过渡到终端背景色 `background`。
fn paint_fades(bounds: Bounds<Pixels>, edges: Edges, cell: Size<Pixels>, background: Rgb, window: &mut Window) {
    let solid = hsla(background);
    let clear = solid.opacity(0.);
    // 渐变的角度：0 是从下往上，顺时针增加；颜色从透明变成背景色，背景色那头贴着视图的边。
    let fades = [
        (edges.left, Bounds::new(bounds.origin, size(cell.width, bounds.size.height)), 270.),
        (
            edges.right,
            Bounds::new(bounds.top_right() - point(cell.width, px(0.)), size(cell.width, bounds.size.height)),
            90.,
        ),
        (edges.top, Bounds::new(bounds.origin, size(bounds.size.width, cell.height)), 0.),
        (
            edges.bottom,
            Bounds::new(bounds.bottom_left() - point(px(0.), cell.height), size(bounds.size.width, cell.height)),
            180.,
        ),
    ];
    for (_, area, angle) in fades.into_iter().filter(|(cropped, ..)| *cropped) {
        window
            .paint_quad(fill(area, linear_gradient(angle, linear_color_stop(clear, 0.), linear_color_stop(solid, 1.))));
    }
}
