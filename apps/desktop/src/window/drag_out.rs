//! 拖出主窗口的那块（浮动的终端、远程桌面）搬进单独窗口以后，那个窗口跟着鼠标走到松开：主窗口一直接着这次拖动
//! 的鼠标事件，每挪一下按主窗口里的位置把单独窗口摆过去。

use gpui::{AnyWindowHandle, App, Pixels, Point, Window};

/// 这个系统上 app 能自己摆窗口（`move_window`）；Wayland 不让程序摆窗口的位置。
/// shortcut: Linux 上 X11 也不跟；要跟时经 x11rb 摆。
pub(super) const FOLLOWS_DRAG: bool = cfg!(any(target_os = "macos", windows));

/// 窗口 `window` 挪到左上角在主窗口 `owner` 内容里 `at` 处的地方（可以在主窗口外面）。
pub(super) fn move_window(owner: &Window, window: AnyWindowHandle, at: Point<Pixels>, cx: &mut App) {
    let Some(target) = native::screen_point(owner, at) else { return };
    let Ok(Some(window)) = window.update(cx, |_, window, _| native::NativeWindow::of(window)) else { return };
    // 挪窗口时系统当场回调 GPUI，GPUI 这时正借着窗口派发事件，放到主线程的下一轮再挪。
    cx.foreground_executor().spawn(async move { window.move_to(target) }).detach();
}

/// 照屏幕坐标摆窗口。
#[cfg(target_os = "macos")]
mod native {
    use gpui::{Pixels, Point, Window};
    use objc2::rc::Retained;
    use objc2_app_kit::{NSView, NSWindow};
    use objc2_foundation::NSPoint;
    use raw_window_handle::RawWindowHandle;

    /// 屏幕上的一点，AppKit 的坐标：原点在主屏左下角。
    pub(super) struct ScreenPoint(NSPoint);

    pub(super) struct NativeWindow(Retained<NSWindow>);

    fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
        let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
        // SAFETY: GPUI 的 AppKit 句柄指着窗口的内容视图，窗口开着时一直有效；这里在主线程上。
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        view.window()
    }

    /// `window` 内容里的 `at` 在屏幕上的位置。GPUI 的内容铺满整个窗口，连标题栏那一条。
    pub(super) fn screen_point(window: &Window, at: Point<Pixels>) -> Option<ScreenPoint> {
        let frame = ns_window(window)?.frame();
        let (x, y) = (f64::from(f32::from(at.x)), f64::from(f32::from(at.y)));
        Some(ScreenPoint(NSPoint::new(frame.origin.x + x, frame.origin.y + frame.size.height - y)))
    }

    impl NativeWindow {
        pub(super) fn of(window: &Window) -> Option<Self> {
            ns_window(window).map(Self)
        }

        /// 窗口的左上角挪到 `to`。
        pub(super) fn move_to(&self, to: ScreenPoint) {
            self.0.setFrameTopLeftPoint(to.0);
        }
    }
}

/// 照屏幕坐标摆窗口。
#[cfg(windows)]
mod native {
    use gpui::{Pixels, Point, Window};
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::{
        Foundation::{HWND, POINT, RECT},
        Graphics::Gdi::ClientToScreen,
        UI::WindowsAndMessaging::{GetWindowRect, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos},
    };

    /// 屏幕上的一点，按像素。
    pub(super) struct ScreenPoint(POINT);

    pub(super) struct NativeWindow(HWND);

    fn hwnd(window: &Window) -> Option<HWND> {
        let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else { return None };
        Some(handle.hwnd.get() as HWND)
    }

    /// `window` 内容里的 `at` 在屏幕上的位置。
    pub(super) fn screen_point(window: &Window, at: Point<Pixels>) -> Option<ScreenPoint> {
        let scale = window.scale_factor();
        let mut point =
            POINT { x: (f32::from(at.x) * scale).round() as i32, y: (f32::from(at.y) * scale).round() as i32 };
        // SAFETY: GPUI 的 Win32 句柄在窗口开着时一直有效。
        (unsafe { ClientToScreen(hwnd(window)?, &mut point) } != 0).then_some(ScreenPoint(point))
    }

    impl NativeWindow {
        pub(super) fn of(window: &Window) -> Option<Self> {
            hwnd(window).map(Self)
        }

        /// 窗口内容的左上角挪到 `to`：窗口四周还有一圈看不见的拉伸边，先量出内容离窗口外框多远。
        pub(super) fn move_to(&self, to: ScreenPoint) {
            let mut frame = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            let mut content = POINT { x: 0, y: 0 };
            // SAFETY: 句柄取自开着的窗口；窗口关了时这几个调用失败，什么也不做。
            unsafe {
                if GetWindowRect(self.0, &mut frame) == 0 || ClientToScreen(self.0, &mut content) == 0 {
                    return;
                }
                let (x, y) = (to.0.x - (content.x - frame.left), to.0.y - (content.y - frame.top));
                SetWindowPos(self.0, std::ptr::null_mut(), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    }
}

/// 这个系统上不挪窗口（`FOLLOWS_DRAG`）。
#[cfg(not(any(target_os = "macos", windows)))]
mod native {
    use gpui::{Pixels, Point, Window};

    pub(super) enum ScreenPoint {}

    pub(super) enum NativeWindow {}

    pub(super) fn screen_point(_: &Window, _: Point<Pixels>) -> Option<ScreenPoint> {
        None
    }

    impl NativeWindow {
        pub(super) fn of(_: &Window) -> Option<Self> {
            None
        }

        pub(super) fn move_to(&self, to: ScreenPoint) {
            match to {}
        }
    }
}
