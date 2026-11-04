//! 搬进单独窗口的远程桌面：窗口里只有这一页，标题栏和主窗口一样透明，页面的工具条就是标题栏，拖它移动窗口；
//! macOS 上红绿灯在它左头，Windows 上右头自己画窗口按钮（`titlebar::window_controls`）。关掉窗口就收起、断开；
//! 页面上的「放回主窗口」把它搬回最前面的主窗口（`put_back`），连接不断。

use gpui::{
    App, AppContext as _, Bounds, Context, DisplayId, Entity, EntityId, Focusable as _, Pixels, Point, Render,
    Subscription, Window, WindowBounds, WindowHandle, WindowOptions, div, prelude::*, px, size,
};

use super::super::{TITLEBAR_HEIGHT, TRAFFIC_LIGHTS_WIDTH, drag_out, install_theme, titlebar};
use super::put_back;
use crate::host_client::hosts::HostId;
use crate::screen_viewer::{Input, ScreenViewer, ScreenViewerEvent, Titlebar};
use crate::{config::AppConfig, ui::theme::ActiveTheme as _};

/// 单独窗口最小这么大。
const MIN_SIZE: (f32, f32) = (360., 240.);

/// 单独窗口的根视图。
struct ScreenWindow {
    viewer: Entity<ScreenViewer>,
    /// 窗口外观上次按深色还是浅色设的。
    dark: Option<bool>,
    _events: Subscription,
    /// 配置（连同主题的颜色）、界面字号和语言变了时重画：远程桌面页是缓存的视图，画面静止时没人叫它重画。
    _config: Subscription,
}

/// 开一个单独的窗口，放在显示器 `display` 上的 `bounds` 处（和 `Window::bounds` 同一套坐标），把远程桌面页 `viewer` 放进去，
/// 焦点给它；`activate` 为假时窗口不到前台，主窗口仍接着鼠标拖动。
pub(super) fn open(
    viewer: Entity<ScreenViewer>,
    bounds: Bounds<Pixels>,
    display: Option<DisplayId>,
    activate: bool,
    cx: &mut App,
) {
    let host = crate::remote_hosts::display_name(viewer.read(cx).host(), cx);
    let title = rust_i18n::t!("screen_viewer.title", host = host).into_owned();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id: display,
        focus: activate,
        window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
        titlebar: Some(gpui::TitlebarOptions { title: Some(title.into()), ..titlebar::titlebar_options() }),
        // 拖动和双击由工具条自己办，和主窗口一样。
        app_owns_titlebar_drag: true,
        app_id: Some(runode_update::BUNDLE_ID.into()),
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| {
            let events =
                cx.subscribe_in(&viewer, window, |this: &mut ScreenWindow, _, event, window, cx| match event {
                    ScreenViewerEvent::Close => window.remove_window(),
                    ScreenViewerEvent::PopIn => {
                        let viewer = this.viewer.clone();
                        cx.defer(move |cx| put_back(viewer, cx));
                        window.remove_window();
                    }
                    _ => {}
                });
            window.focus(&viewer.focus_handle(cx), cx);
            let config = cx.observe_global::<AppConfig>(|this: &mut ScreenWindow, cx| {
                this.viewer.update(cx, |_, cx| cx.notify());
                cx.notify();
            });
            ScreenWindow { viewer, dark: None, _events: events, _config: config }
        })
    });
    if let Err(err) = opened {
        tracing::warn!("failed to open the remote desktop window: {err:#}");
    }
}

/// 主机 `host` 的远程桌面开在单独的窗口里时把那个窗口提到前面、焦点给它，返回找到了没有。
pub(super) fn activate(host: &HostId, cx: &mut App) -> bool {
    cx.windows().into_iter().filter_map(|handle| handle.downcast::<ScreenWindow>()).any(|handle| {
        handle
            .update(cx, |this, window, cx| {
                let found = this.viewer.read(cx).host() == host;
                if found {
                    window.activate_window();
                    window.focus(&this.viewer.focus_handle(cx), cx);
                }
                found
            })
            .unwrap_or(false)
    })
}

/// 放着远程桌面页 `viewer` 的单独窗口。
fn window_of(viewer: EntityId, cx: &App) -> Option<WindowHandle<ScreenWindow>> {
    cx.windows()
        .into_iter()
        .filter_map(|handle| handle.downcast::<ScreenWindow>())
        .find(|handle| handle.read(cx).is_ok_and(|this| this.viewer.entity_id() == viewer))
}

/// 远程桌面页 `viewer` 的单独窗口提到前面、焦点给它。
pub(super) fn activate_view(viewer: EntityId, cx: &mut App) {
    if let Some(handle) = window_of(viewer, cx) {
        let _ = handle.update(cx, |this, window, cx| {
            window.activate_window();
            window.focus(&this.viewer.focus_handle(cx), cx);
        });
    }
}

/// 远程桌面页 `viewer` 的单独窗口挪到左上角在主窗口 `owner` 内容里 `at` 处的地方；窗口还没开出来时不挪。
pub(super) fn follow(owner: &Window, viewer: EntityId, at: Point<Pixels>, cx: &mut App) {
    if let Some(handle) = window_of(viewer, cx) {
        drag_out::move_window(owner, handle.into(), at, cx);
    }
}

impl Render for ScreenWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 界面的字号按 rem 写（`ui::font_size`），和主窗口一样设成配置的界面字号。
        window.set_rem_size(crate::ui::font_size::rem_size(cx));
        // 窗口里没有终端，主题按配置的颜色，和没有终端的主窗口一样。
        let (fg, bg) = {
            let config = &cx.global::<AppConfig>().0;
            (config.foreground, config.background)
        };
        let dark = install_theme(fg, bg, cx).background.l < 0.5;
        if self.dark != Some(dark) {
            self.dark = Some(dark);
            match_appearance(window, dark);
        }
        // 工具条两头给窗口按钮让地方：全屏时红绿灯和 Windows 的按钮都不画，工具条也收起来。
        let fullscreen = window.is_fullscreen();
        let lights = cfg!(target_os = "macos") && !fullscreen;
        let bar = Titlebar {
            height: TITLEBAR_HEIGHT,
            left: if lights { TRAFFIC_LIGHTS_WIDTH } else { 0. },
            right: if fullscreen { 0. } else { titlebar::window_controls_width(window) },
            fullscreen,
            reveal_top: if cfg!(target_os = "macos") { TITLEBAR_HEIGHT } else { 0. },
        };
        let viewer = self.viewer.clone();
        let changed = viewer
            .update(cx, |viewer, _| viewer.set_input(Input { floating: false, titlebar: Some(bar), pinned: None }));
        // 这一页是单独的视图，换了样子要它自己重画；画的时候发的通知要等这一帧画完再发。
        if changed {
            cx.defer(move |cx| viewer.update(cx, |_, cx| cx.notify()));
        }
        crate::ui::font::apply(div(), cx)
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .child(self.viewer.clone())
            .children(titlebar::window_controls(window, cx))
            .children(titlebar::resize_edges(window))
    }
}

/// 窗口的外观跟着界面的深浅走：全屏时系统滑下来的标题栏照它画底色，不然深色界面上露出一条白的。
#[cfg(target_os = "macos")]
fn match_appearance(window: &Window, dark: bool) {
    use objc2::{class, msg_send, runtime::AnyObject};
    use objc2_foundation::NSString;
    use raw_window_handle::RawWindowHandle;

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    let name = NSString::from_str(if dark { "NSAppearanceNameDarkAqua" } else { "NSAppearanceNameAqua" });
    // SAFETY: GPUI 的 AppKit 句柄指着它画画的视图，窗口开着时一直有效；这里在主线程上。
    unsafe {
        let view = handle.ns_view.cast::<AnyObject>().as_ref();
        let ns_window: *mut AnyObject = msg_send![view, window];
        let appearance: *mut AnyObject = msg_send![class!(NSAppearance), appearanceNamed: &*name];
        if let (Some(ns_window), false) = (ns_window.as_ref(), appearance.is_null()) {
            let _: () = msg_send![ns_window, setAppearance: appearance];
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn match_appearance(_: &Window, _: bool) {}
