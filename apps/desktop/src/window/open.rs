//! 打开窗口：新窗口的默认选项，以及按存档或从零建一个 `WindowView` 窗口。

use gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};

use super::{WindowView, persist::format::SavedWindow, should_close, titlebar_options};
use crate::prespawn::Prespawned;

/// 新窗口的默认选项：屏幕居中的默认大小。
pub(crate) fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(960.), px(620.)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(320.), px(200.))),
        titlebar: Some(titlebar_options()),
        // 标题栏的拖动和双击由 `WindowView` 自己处理；否则 AppKit 会抢先处理标题栏区域的双击，
        // 在标签或新建按钮上双击也会缩放窗口。
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
}

/// 打开一个新窗口，里面一个终端；`shell` 是启动时提前拉起的 shell，没有时现启动一个。
pub(crate) fn open_window(cx: &mut App, shell: Option<Prespawned>) {
    let options = window_options(cx);
    open_window_with(cx, options, None, shell);
}

pub(crate) fn open_window_with(
    cx: &mut App,
    options: WindowOptions,
    saved: Option<SavedWindow>,
    shell: Option<Prespawned>,
) {
    let opened = cx.open_window(options, |window, cx| {
        window.on_window_should_close(cx, should_close);
        cx.new(|cx| match saved {
            Some(saved) => WindowView::restore(saved, shell, window, cx),
            None => WindowView::new(shell, window, cx),
        })
    });
    if let Err(err) = opened {
        tracing::error!("failed to open window: {err:#}");
    }
}
