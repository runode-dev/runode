//! 打开窗口：新窗口的默认选项，以及按存档或从零建一个 `WindowView` 窗口。

use gpui::{App, AppContext as _, Bounds, Window, WindowBounds, WindowOptions, px, size};

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
        // Wayland 和 X11 按它把窗口对上 .desktop 文件，任务栏的图标和窗口分组靠它。
        app_id: Some(runode_update::BUNDLE_ID.into()),
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
        disable_display_sync(window);
        cx.new(|cx| match saved {
            Some(saved) => WindowView::restore(saved, shell, window, cx),
            None => WindowView::new(shell, window, cx),
        })
    });
    if let Err(err) = opened {
        tracing::error!("failed to open window: {err:#}");
    }
}

/// 关掉 Metal 图层的 `displaySyncEnabled`：画完就交出去，由窗口服务器在下一次合成时用上。开着时 drawable
/// 要排到 vsync 才呈现，终端程序的新画面平均晚一帧多上屏；Ghostty 直接设 IOSurface 当图层内容，也不排这个队。
#[cfg(target_os = "macos")]
fn disable_display_sync(window: &Window) {
    use objc2::{msg_send, rc::Retained, runtime::AnyObject, sel};
    use raw_window_handle::RawWindowHandle;

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    // SAFETY: GPUI 的 AppKit 句柄指着它画画的视图，窗口开着时一直有效；这里在主线程上。GPUI 建窗口时就让
    // 这个视图用上了自己的 CAMetalLayer，别的图层没有这个设置，所以先问它认不认。
    unsafe {
        let view = handle.ns_view.cast::<AnyObject>().as_ref();
        let Some(layer): Option<Retained<AnyObject>> = msg_send![view, layer] else { return };
        let supported: bool = msg_send![&*layer, respondsToSelector: sel!(setDisplaySyncEnabled:)];
        if supported {
            let _: () = msg_send![&*layer, setDisplaySyncEnabled: false];
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn disable_display_sync(_: &Window) {}
