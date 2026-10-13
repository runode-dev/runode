//! 终端里的程序用 CSI 8 t 要求改窗口尺寸（宿主转来的 `HostMsg::ResizeWindow`）：配置项
//! `vt-window-resize-allowed` 开着、终端正显示着（没被铺满窗口的页或放大的预览栏盖住）并独占标签、
//! 窗口不是全屏时，按终端要多出或少掉的格数改窗口，侧栏、标题栏和右侧面板照旧。照 Ghostty 的规矩：
//! 小于 40 列 10 行的按 40 列 10 行办，窗口不大过屏幕，左上角不动。

use gpui::{App, Context, Entity, Window};

use super::{WindowView, project::PanelWidths};
use crate::{config::AppConfig, terminal_view::TerminalView};

/// 程序最多能把终端缩到这么小，免得缩成看不见来藏起自己的输出。
const MIN_COLUMNS: u16 = 40;
const MIN_ROWS: u16 = 10;

impl WindowView {
    pub(super) fn resize_for_program(
        &mut self,
        view: &Entity<TerminalView>,
        (wi, ti): (usize, usize),
        (rows, columns): (u16, u16),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let alone = self.workspaces[wi].tabs[ti].panes.len() == 1;
        if !cx.global::<AppConfig>().0.vt_window_resize_allowed
            || !self.is_shown(wi, ti)
            || self.page_covers_terminals()
            || self.preview_maximized(cx)
            || !alone
            || window.is_fullscreen()
        {
            tracing::debug!("ignored a program's request to resize the window");
            return;
        }
        let size = view.read(cx).size();
        let scale = window.scale_factor();
        let mut dw = grow(columns, MIN_COLUMNS, size.cols, size.cell_width_px, scale);
        if dw > 0. {
            // 窄窗口里右侧的预览栏和面板被压窄过的，窗口一放宽先放回它们原来的宽度，要多给这一截。
            let total = |widths: PanelWidths| f64::from(widths.preview + widths.panel);
            let now = self.right_panel_widths(f32::from(window.viewport_size().width), cx);
            dw += total(self.right_panel_widths(f32::INFINITY, cx)) - total(now);
        }
        let dh = grow(rows, MIN_ROWS, size.rows, size.cell_height_px, scale);
        if dw != 0. || dh != 0. {
            resize_by(window, cx, dw, dh);
        }
    }
}

/// 终端从 `have` 格变成程序要的 `want` 格（不少于 `min`），这一维要多出的点数，少了为负；`want` 为 0 是这一维
/// 不变。`cell_px` 是一格的像素数，`scale` 是每点几个像素。
fn grow(want: u16, min: u16, have: u16, cell_px: u16, scale: f32) -> f64 {
    if want == 0 { 0. } else { (f64::from(want.max(min)) - f64::from(have)) * f64::from(cell_px) / f64::from(scale) }
}

/// 把窗口加宽 `dw`、加高 `dh` 点，不大过所在屏幕的可用区域，左上角不动。放到主线程的下一轮再改：
/// `setFrame` 当场回调 GPUI 的改尺寸，GPUI 这时正借着窗口，会 panic。
#[cfg(target_os = "macos")]
fn resize_by(window: &Window, cx: &App, dw: f64, dh: f64) {
    use objc2_app_kit::NSView;
    use raw_window_handle::RawWindowHandle;

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    // SAFETY: GPUI 的 AppKit 句柄指着窗口的内容视图，窗口开着时一直有效；这里在主线程上。
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let Some(ns_window) = view.window() else { return };
    cx.foreground_executor()
        .spawn(async move {
            let Some(screen) = ns_window.screen() else { return };
            let visible = screen.visibleFrame();
            let old = ns_window.frame();
            let mut frame = old;
            frame.size.width = (old.size.width + dw).min(visible.size.width);
            frame.size.height = (old.size.height + dh).min(visible.size.height);
            // AppKit 的原点在左下角：高度变了，原点跟着挪，顶边才不动。
            frame.origin.y = old.origin.y + old.size.height - frame.size.height;
            let frame = ns_window.constrainFrameRect_toScreen(frame, Some(&screen));
            ns_window.setFrame_display(frame, true);
        })
        .detach();
}

/// shortcut: 别的平台只改内容尺寸，不限制在屏幕里、也不保持左上角；有了那边的桌面版再补。
#[cfg(not(target_os = "macos"))]
fn resize_by(window: &mut Window, _: &App, dw: f64, dh: f64) {
    let size = window.viewport_size();
    window.resize(gpui::size(size.width + gpui::px(dw as f32), size.height + gpui::px(dh as f32)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grow_counts_points_from_cells() {
        // 0 是这一维不变。
        assert_eq!(grow(0, MIN_COLUMNS, 80, 16, 2.), 0.);
        // 80 列到 120 列，每格 16 像素，2 倍屏上是 8 点。
        assert_eq!(grow(120, MIN_COLUMNS, 80, 16, 2.), 320.);
        // 缩小为负。
        assert_eq!(grow(60, MIN_COLUMNS, 80, 16, 2.), -160.);
        // 要得比下限还小按下限办。
        assert_eq!(grow(1, MIN_ROWS, 24, 32, 2.), -224.);
    }
}
