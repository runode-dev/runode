//! 单独的终端窗口：上面一条标题栏（终端的标题和目录、置顶、放回主窗口），下面是终端。终端仍属于主窗口里原来的
//! 标签（`Tab::popped`），标题、agent 的状态和会话照旧由主窗口管。关掉这个窗口和按「放回主窗口」一样，放回那个
//! 标签里浮着，会话不断；主窗口不再把它算作单独窗口里的终端时（放回去了、终端关了、挪走了、主窗口关了），这个窗口
//! 跟着关掉。

use gpui::{
    App, AppContext as _, Bounds, Context, DisplayId, Entity, EntityId, Focusable as _, MouseButton, MouseDownEvent,
    Pixels, Render, Subscription, Window, WindowBounds, WindowControlArea, WindowHandle, WindowOptions, div,
    prelude::*, px, size, svg,
};

use super::super::{TITLEBAR_HEIGHT, TRAFFIC_LIGHTS_WIDTH, WindowView, titlebar};
use crate::terminal_view::{TerminalEvent, TerminalView};
use crate::{
    config::AppConfig,
    ui::{
        a11y::Press as _,
        button::{Button, ButtonSize},
        font_size::FontSize,
        icons::{PIN_ICON, POP_IN_ICON, TERMINAL_ICON},
        theme::{ActiveTheme as _, Theme},
        tooltip::tooltip,
    },
};

/// 单独的终端窗口最小这么大。
const MIN_SIZE: (f32, f32) = (280., 160.);

/// 单独的终端窗口的根视图。
struct PaneWindow {
    pane: EntityId,
    view: Entity<TerminalView>,
    owner: WindowHandle<WindowView>,
    /// 用户按了置顶，窗口浮在别的程序的窗口上面。
    pinned: bool,
    _subscriptions: Vec<Subscription>,
}

/// 开一个单独的终端窗口，放在显示器 `display` 上的 `bounds` 处（位置相对这块显示器），把主窗口 `owner` 里的终端
/// `view`（分屏 `pane`）放进去，焦点给它。
pub(super) fn open(
    owner: WindowHandle<WindowView>,
    owner_view: Entity<WindowView>,
    pane: EntityId,
    view: Entity<TerminalView>,
    bounds: Bounds<Pixels>,
    display: Option<DisplayId>,
    cx: &mut App,
) {
    let (title, _) = titlebar::pane_label(view.read(cx));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id: display,
        window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
        titlebar: Some(gpui::TitlebarOptions { title: Some(title), ..titlebar::titlebar_options() }),
        window_decorations: titlebar::window_decorations(),
        // 拖动和双击由标题栏自己办，和主窗口一样。
        app_owns_titlebar_drag: true,
        app_id: Some(runode_update::BUNDLE_ID.into()),
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| {
            view.update(cx, |view, cx| view.attach_window(window, cx));
            window.focus(&view.focus_handle(cx), cx);
            // 关窗口就是放回主窗口：主窗口放回去以后不再把它算作单独窗口里的，下面的订阅收到就不会再关一次。
            window.on_window_should_close(cx, move |_, cx| {
                put_back(owner, pane, cx);
                true
            });
            let subscriptions = vec![
                cx.observe_in(&owner_view, window, move |this: &mut PaneWindow, owner, window, cx| {
                    if !owner.read(cx).holds_popped(pane) {
                        let view = this.view.clone();
                        cx.defer(move |cx| redock(view, pane, cx));
                        window.remove_window();
                    }
                }),
                cx.observe_release_in(&owner_view, window, |_, _, window, _| window.remove_window()),
                // 标题跟着终端的标题和目录走；终端每有输出都会通知，只在这两种事件时重画。
                cx.subscribe(&view, |_, _, event: &TerminalEvent, cx| {
                    if matches!(event, TerminalEvent::TitleChanged | TerminalEvent::MetaChanged) {
                        cx.notify();
                    }
                }),
                cx.observe_global_in::<Theme>(window, |this: &mut PaneWindow, _, cx| {
                    this.view.update(cx, |_, cx| cx.notify());
                    cx.notify();
                }),
                cx.observe_global::<AppConfig>(|this: &mut PaneWindow, cx| {
                    this.view.update(cx, |_, cx| cx.notify());
                    cx.notify();
                }),
            ];
            PaneWindow { pane, view, owner, pinned: false, _subscriptions: subscriptions }
        })
    });
    if let Err(err) = opened {
        tracing::warn!("failed to open the terminal window: {err:#}");
        cx.defer(move |cx| put_back(owner, pane, cx));
    }
}

/// 终端 `pane` 开在单独的窗口里时把那个窗口提到前面、焦点给终端，返回找到了没有。
pub(super) fn activate(pane: EntityId, cx: &mut App) -> bool {
    cx.windows().into_iter().filter_map(|handle| handle.downcast::<PaneWindow>()).any(|handle| {
        handle
            .update(cx, |this, window, cx| {
                let found = this.pane == pane;
                if found {
                    window.activate_window();
                    window.focus(&this.view.focus_handle(cx), cx);
                }
                found
            })
            .unwrap_or(false)
    })
}

/// 终端 `pane` 不在单独窗口里了、还在某个主窗口里时，它的焦点和窗口订阅换回那个主窗口。
fn redock(view: Entity<TerminalView>, pane: EntityId, cx: &mut App) {
    for handle in cx.windows().into_iter().filter_map(|handle| handle.downcast::<WindowView>()) {
        let held = handle
            .update(cx, |this, window, cx| {
                let held = this.locate(pane).is_some();
                if held {
                    view.update(cx, |view, cx| view.attach_window(window, cx));
                }
                held
            })
            .unwrap_or(false);
        if held {
            break;
        }
    }
}

/// 把终端 `pane` 放回主窗口 `owner` 里它的标签。等这一轮派发结束再放：点按钮时 GPUI 正借着这个窗口派发事件。
fn put_back(owner: WindowHandle<WindowView>, pane: EntityId, cx: &mut App) {
    cx.defer(move |cx| {
        if let Err(err) = owner.update(cx, |view, window, cx| view.pop_in_pane(pane, window, cx)) {
            tracing::warn!("failed to put the terminal back into its window: {err:#}");
        }
    });
}

impl Render for PaneWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 界面的字号按 rem 写（`ui::font_size`），和主窗口一样设成配置的界面字号。
        window.set_rem_size(crate::ui::font_size::rem_size(cx));
        let (name, dir) = titlebar::pane_label(self.view.read(cx));
        window.set_window_title(&name);
        // 标题栏两头给窗口按钮让地方：全屏时红绿灯和 Windows 的按钮都不画。
        let fullscreen = window.is_fullscreen();
        let left = if cfg!(target_os = "macos") && !fullscreen { TRAFFIC_LIGHTS_WIDTH } else { 10. };
        let right = if fullscreen { 0. } else { titlebar::window_controls_width(window) } + 8.;
        let fg = cx.theme().foreground;
        let pin = can_keep_on_top().then(|| {
            let label = rust_i18n::t!(if self.pinned { "pane_window.stop_on_top" } else { "pane_window.keep_on_top" });
            Button::new("pane-window-pin", label.clone())
                .icon(PIN_ICON)
                .with_size(ButtonSize::IconSm)
                .tooltip(tooltip(label, None))
                .toggled(Some(self.pinned))
                .on_press(cx, |this, window, cx| {
                    this.pinned = !this.pinned;
                    keep_on_top(window, this.pinned);
                    cx.notify();
                })
        });
        let pop_in = rust_i18n::t!("pane_window.pop_in");
        let pop_in = Button::new("pane-window-pop-in", pop_in.clone())
            .icon(POP_IN_ICON)
            .with_size(ButtonSize::IconSm)
            .tooltip(tooltip(pop_in, None))
            .on_press(cx, |this, _, cx| put_back(this.owner, this.pane, cx));
        // 标题这一段拖着挪窗口、双击缩放；按钮在它外面，按按钮不会拖。
        let heading = div()
            .id("pane-window-heading")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(FontSize::Body)
            .child(svg().flex_none().path(TERMINAL_ICON).size(px(14.)).text_color(fg.opacity(0.6)))
            .child(div().flex_shrink_0().max_w(gpui::relative(0.6)).truncate().text_color(fg.opacity(0.9)).child(name))
            .children(dir.map(|dir| div().min_w_0().truncate().text_color(fg.opacity(0.45)).child(dir)))
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(MouseButton::Left, |event: &MouseDownEvent, window, cx| {
                crate::ui::drag_window(event, window, cx);
            });
        let bar = div()
            .flex_none()
            .h(px(TITLEBAR_HEIGHT))
            .pl(px(left))
            .pr(px(right))
            .flex()
            .items_center()
            .gap(px(4.))
            .child(heading)
            .children(pin)
            .child(pop_in);
        // 终端四周留一点，和分屏卡片里一样。
        let inset = px(4.);
        crate::ui::font::apply(div(), cx)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .child(bar)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(div().absolute().top_0().left(inset).right(inset).bottom(inset).child(self.view.clone())),
            )
            .children(titlebar::window_controls(window, cx))
            .children(titlebar::resize_edges(window))
    }
}

/// 这个系统上能让窗口浮在别的程序的窗口上面。
fn can_keep_on_top() -> bool {
    cfg!(any(target_os = "macos", windows))
}

/// 让窗口浮在别的程序的窗口上面（`on_top`），或者回到普通的层次。
#[cfg(target_os = "macos")]
fn keep_on_top(window: &Window, on_top: bool) {
    use objc2::{msg_send, runtime::AnyObject};
    use raw_window_handle::RawWindowHandle;

    /// `NSFloatingWindowLevel`、`NSNormalWindowLevel`。
    const FLOATING: isize = 3;
    const NORMAL: isize = 0;
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    // SAFETY: GPUI 的 AppKit 句柄指着它画画的视图，窗口开着时一直有效；这里在主线程上。
    unsafe {
        let view = handle.ns_view.cast::<AnyObject>().as_ref();
        let ns_window: *mut AnyObject = msg_send![view, window];
        if let Some(ns_window) = ns_window.as_ref() {
            let _: () = msg_send![ns_window, setLevel: if on_top { FLOATING } else { NORMAL }];
        }
    }
}

#[cfg(windows)]
fn keep_on_top(window: &Window, on_top: bool) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else { return };
    let after = if on_top { HWND_TOPMOST } else { HWND_NOTOPMOST };
    // SAFETY: GPUI 的 Win32 句柄在窗口开着时一直有效；只改层次，不挪不改大小。
    unsafe { SetWindowPos(handle.hwnd.get() as _, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) };
}

#[cfg(not(any(target_os = "macos", windows)))]
fn keep_on_top(_: &Window, _: bool) {}
