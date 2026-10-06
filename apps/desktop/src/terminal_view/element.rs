//! 终端网格的 GPUI 元素：布局时按单元格尺寸调整终端大小、启动 shell，绘制时挂上输入和鼠标事件再画一帧。

use gpui::{
    App, Bounds, DispatchPhase, ElementId, ElementInputHandler, GlobalElementId, LayoutId, MouseMoveEvent,
    MouseUpEvent, Pixels, Style, Window, prelude::*, relative,
};
use runode_shared_types::grid::GridSize;
use runode_terminal::session::Session;

use super::{TerminalView, paint::paint_frame};
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
            // 窗口刚建好时设的焦点不触发 `on_focus`，之后要等有输出才会开始闪；提前启动的
            // shell 输出早已喂完，所以画的时候发现有焦点却没在闪就补上。
            if focused && view._cursor_blink.is_none() {
                view.reset_cursor_blink(window, cx);
            }
            let metrics = view.metrics(window);
            view.refresh_input();
            // 绘制时要同时用到帧和 `&mut view`（字形缓存），所以先把帧取出来，画完再放回。没有界面
            // 这份 VT（只看状态、还在等屏幕、断开时没有屏幕）时只有背景。
            let Some(frame) = view.screen.shown_mut().map(Session::take_frame) else {
                return;
            };
            paint_frame(view, &frame, bounds.origin, metrics, focused, window);
            // 第一次画出字（多半是 shell 的提示符）的那一帧。
            static FIRST_CONTENT: startup::Once = startup::Once::new();
            FIRST_CONTENT.mark_when("first_content", || frame.cells.iter().any(|cell| !cell.text.trim().is_empty()));
            // 补全菜单盖在终端内容上面。
            view.paint_completion(&frame, metrics, window);
            if let Some(session) = view.screen.shown_mut() {
                session.restore_frame(frame);
            }
        });
    }
}
