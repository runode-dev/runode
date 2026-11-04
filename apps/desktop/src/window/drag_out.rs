//! 拖出主窗口的那块（浮动的终端、远程桌面）搬进单独窗口以后，那个窗口跟着鼠标走到松开：主窗口一直接着这次拖动
//! 的鼠标事件，每挪一下按主窗口里的位置把单独窗口摆过去。

use gpui::{AnyWindowHandle, App, Pixels, Point, Window};

/// 这个窗口所在的系统上 app 能自己摆窗口（`move_window`）：Linux 上只有 X11 能，Wayland 不让程序摆窗口的位置。
pub(super) fn follows_drag(window: &Window) -> bool {
    native::can_move(window)
}

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

    pub(super) fn can_move(_: &Window) -> bool {
        true
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

    pub(super) fn can_move(_: &Window) -> bool {
        true
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

/// 照屏幕坐标摆窗口：X11 上经自己的一条 x11rb 连接，窗口号是全服务器通用的，GPUI 的窗口也摆得动。
#[cfg(target_os = "linux")]
mod native {
    use std::cell::OnceCell;

    use gpui::{Pixels, Point, Window};
    use raw_window_handle::RawWindowHandle;
    use x11rb::{
        connection::Connection as _,
        protocol::xproto::{
            self, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask, Gravity,
        },
        rust_connection::RustConnection,
    };

    /// 屏幕上的一点，按像素，相对根窗口。
    pub(super) struct ScreenPoint {
        x: i32,
        y: i32,
    }

    pub(super) struct NativeWindow(xproto::Window);

    /// 连着 GPUI 所在的 X 服务器（`DISPLAY`）。
    struct X11 {
        conn: RustConnection,
        root: xproto::Window,
        /// 窗口管理器认 `_NET_MOVERESIZE_WINDOW` 时是这个原子：经它按窗口本身（不算窗口管理器加的边框）摆位置。
        moveresize: Option<xproto::Atom>,
    }

    thread_local! {
        /// 只在主线程上用；连不上时记着连不上，不再试。
        static X11: OnceCell<Option<X11>> = const { OnceCell::new() };
    }

    impl X11 {
        fn connect() -> Option<Self> {
            let (conn, screen) = x11rb::connect(None).ok()?;
            let root = conn.setup().roots.get(screen)?.root;
            let moveresize = ewmh_moveresize(&conn, root);
            Some(Self { conn, root, moveresize })
        }
    }

    fn with_x11<R>(f: impl FnOnce(&X11) -> Option<R>) -> Option<R> {
        X11.with(|cell| cell.get_or_init(X11::connect).as_ref().and_then(f))
    }

    fn atom(conn: &RustConnection, name: &[u8]) -> Option<xproto::Atom> {
        Some(conn.intern_atom(false, name).ok()?.reply().ok()?.atom)
    }

    /// 窗口 `window` 上类型为窗口的属性 `property` 里的第一个值。
    fn window_property(conn: &RustConnection, window: xproto::Window, property: xproto::Atom) -> Option<u32> {
        let reply = conn.get_property(false, window, property, AtomEnum::WINDOW, 0, 1).ok()?.reply().ok()?;
        reply.value32()?.next()
    }

    /// 在跑着的、符合 EWMH 的窗口管理器认 `_NET_MOVERESIZE_WINDOW` 时返回这个原子。根窗口上的
    /// `_NET_SUPPORTING_WM_CHECK` 指着的窗口自己也带着同一个值，才算窗口管理器还在跑。
    fn ewmh_moveresize(conn: &RustConnection, root: xproto::Window) -> Option<xproto::Atom> {
        let check = atom(conn, b"_NET_SUPPORTING_WM_CHECK")?;
        let wm = window_property(conn, root, check)?;
        if window_property(conn, wm, check)? != wm {
            return None;
        }
        let supported = atom(conn, b"_NET_SUPPORTED")?;
        let moveresize = atom(conn, b"_NET_MOVERESIZE_WINDOW")?;
        let reply = conn.get_property(false, root, supported, AtomEnum::ATOM, 0, u32::MAX / 4).ok()?.reply().ok()?;
        reply.value32()?.any(|atom| atom == moveresize).then_some(moveresize)
    }

    fn xid(window: &Window) -> Option<xproto::Window> {
        let handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
        match handle.as_raw() {
            RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
            RawWindowHandle::Xlib(handle) => u32::try_from(handle.window).ok(),
            _ => None,
        }
    }

    pub(super) fn can_move(window: &Window) -> bool {
        xid(window).is_some() && with_x11(|_| Some(())).is_some()
    }

    /// `window` 内容里的 `at` 在屏幕上的位置。
    pub(super) fn screen_point(window: &Window, at: Point<Pixels>) -> Option<ScreenPoint> {
        let window_id = xid(window)?;
        let scale = window.scale_factor();
        let (x, y) = ((f32::from(at.x) * scale).round() as i16, (f32::from(at.y) * scale).round() as i16);
        with_x11(|x11| {
            let reply = x11.conn.translate_coordinates(window_id, x11.root, x, y).ok()?.reply().ok()?;
            Some(ScreenPoint { x: reply.dst_x.into(), y: reply.dst_y.into() })
        })
    }

    impl NativeWindow {
        pub(super) fn of(window: &Window) -> Option<Self> {
            xid(window).map(Self)
        }

        /// 窗口内容的左上角挪到 `to`。窗口管理器认 `_NET_MOVERESIZE_WINDOW` 时按 `StaticGravity` 请它摆，加了边框
        /// 也摆得准；没有窗口管理器时直接改窗口的位置。
        pub(super) fn move_to(&self, to: ScreenPoint) {
            with_x11(|x11| {
                match x11.moveresize {
                    Some(moveresize) => {
                        // 低 8 位是 gravity，第 8、9 位是带了 x、y，第 12 位起是来源：1 是普通程序。
                        let flags = u32::from(Gravity::STATIC) | 1 << 8 | 1 << 9 | 1 << 12;
                        let data = [flags, to.x as u32, to.y as u32, 0, 0];
                        let event = ClientMessageEvent::new(32, self.0, moveresize, data);
                        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
                        x11.conn.send_event(false, x11.root, mask, event).ok()?;
                    }
                    None => {
                        x11.conn.configure_window(self.0, &ConfigureWindowAux::new().x(to.x).y(to.y)).ok()?;
                    }
                }
                x11.conn.flush().ok()
            });
        }
    }
}
