//! 「管理权限」浮窗：打开时摆在系统设置的窗口旁边（等不到它的窗口时摆在主窗口那块屏的右侧），浮在别的程序的
//! 窗口上面，列出同一份权限，用户在系统设置里开好了当场看得到；不抢焦点，app 退到后台也不藏起来。一个 app 里
//! 只开一个，列出的几项全开了时说一声再自己关掉，系统设置退出时也关掉。

use std::{sync::Arc, time::Duration};

use futures::StreamExt as _;
use gpui::{
    AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, Global, Image, ImageFormat, Pixels, Render, Role,
    Size, Subscription, Task, Window, WindowBounds, WindowControlArea, WindowHandle, WindowKind, WindowOptions, div,
    img, point, prelude::*, px, size, svg,
};
use objc2::rc::Retained;
use objc2_app_kit::NSWindow;
use objc2_foundation::NSPoint;

use super::{
    super::{headline, text_label},
    Permissions, WatchGuard, draggable_app_icon, rows,
    system_settings::{self, QuitWatch, SettingsWindow},
};
use crate::{
    config::AppConfig,
    ui::{
        self,
        a11y::{Press as _, StaticText as _},
        button::{Button, ButtonSize, ButtonVariant},
        font_size::{self, FontSize},
        icons::{CHECK_ICON, CLOSE_ICON},
    },
};

/// 浮窗按基准字号估的宽高，开窗时乘界面字号的倍数；高度放不下屏幕时缩到放得下，内容滚动。
const SIZE: (f32, f32) = (420., 500.);
/// 等不到系统设置的窗口时，浮窗和屏幕可见区域右边、上下边留的空。
const SCREEN_MARGIN: f32 = 24.;
/// 浮窗和系统设置的窗口之间隔的空。
const SETTINGS_GAP: f32 = 12.;
/// 全开了以后「都开好了」留多久再关。
const ALL_GRANTED_LINGER: Duration = Duration::from_secs(1);
/// 顶上拖着挪窗口的那一条的高度，也是关闭按钮所在的一行。
const BAR_HEIGHT: f32 = 32.;

#[derive(Default)]
struct PermissionPanel {
    /// 开着的浮窗；窗口关掉后句柄失效，`update` 它会失败。
    window: Option<WindowHandle<PanelView>>,
    /// 在等系统设置的窗口，等到了再开或挪浮窗；再打开时换掉，关浮窗时丢掉。
    placing: Option<Task<()>>,
}

impl Global for PermissionPanel {}

/// 列出的几项还没全开时，把浮窗摆到系统设置的窗口旁边：开着时先提到最前，等到系统设置的窗口后挪过去；没开着
/// （连同已经关掉的）时等到了再开。只在打开时摆一次，不跟着系统设置的窗口走。等这一轮派发结束再办：按钮在别的窗口里时那个窗口正被
/// 借着。
pub(super) fn open(cx: &mut App) {
    cx.defer(|cx| {
        let (granted, total) = Permissions::global(cx).read(cx).count();
        if granted == total {
            return;
        }
        let shown = cx.default_global::<PermissionPanel>().window;
        // 提到最前、挪窗口时 AppKit 会当场回调 GPUI，回调要借 App，所以这些都在 `cx.update` 外面做。
        let placing = cx.spawn(async move |cx| {
            if let Some(ns_window) = shown.and_then(|handle| cx.update(|cx| ns_window_of(handle, cx))) {
                ns_window.orderFrontRegardless();
            }
            let settings = system_settings::wait(cx).await;
            let placed = cx.update(|cx| match shown {
                Some(handle) => move_target(handle, settings, cx).map(|(ns_window, to)| (ns_window, Some(to))),
                None => {
                    let handle = open_window(settings, cx);
                    cx.default_global::<PermissionPanel>().window = handle;
                    handle.and_then(|handle| ns_window_of(handle, cx)).map(|ns_window| (ns_window, None))
                }
            });
            match placed {
                Some((ns_window, Some(to))) => ns_window.setFrameTopLeftPoint(to),
                // 系统设置这时可能已经到了前台，app 不在前台时普通的 `orderFront` 不一定提得上来。
                Some((ns_window, None)) => ns_window.orderFrontRegardless(),
                None => {}
            }
        });
        cx.default_global::<PermissionPanel>().placing = Some(placing);
    });
}

/// `window` 是管理权限的浮窗。
pub(crate) fn is_panel(window: AnyWindowHandle, cx: &App) -> bool {
    cx.try_global::<PermissionPanel>()
        .and_then(|panel| panel.window)
        .is_some_and(|handle| handle.window_id() == window.window_id())
}

/// 关掉浮窗，没开着时什么也不做；还在等着开的不再开。
pub(crate) fn close(cx: &mut App) {
    if !cx.has_global::<PermissionPanel>() {
        return;
    }
    let panel = cx.global_mut::<PermissionPanel>();
    panel.placing = None;
    if let Some(handle) = panel.window.take() {
        handle.update(cx, |_, window, _| window.remove_window()).ok();
    }
}

fn ns_window_of(handle: WindowHandle<PanelView>, cx: &mut App) -> Option<Retained<NSWindow>> {
    handle.update(cx, |_, window, _| crate::window::ns_window(window)).ok().flatten()
}

/// 开着的浮窗 `handle` 该挪到系统设置的窗口 `settings` 旁边的哪一点（AppKit 坐标的左上角）；没等到它的窗口时
/// 不挪。等的时候浮窗被用户关掉或全开了自己关掉时不再开。
fn move_target(
    handle: WindowHandle<PanelView>,
    settings: Option<SettingsWindow>,
    cx: &mut App,
) -> Option<(Retained<NSWindow>, NSPoint)> {
    let Ok((panel, ns_window)) =
        handle.update(cx, |_, window, _| (window.bounds().size, crate::window::ns_window(window)))
    else {
        cx.default_global::<PermissionPanel>().window = None;
        return None;
    };
    let settings = settings?;
    let display = cx.find_display(settings.display)?;
    let to = place(settings.bounds, panel, display.visible_bounds());
    Some((ns_window?, system_settings::appkit_point(settings.display, to.origin)))
}

/// 浮窗摆在哪，三者是同一块屏上相对左上角的坐标：`settings` 是系统设置的窗口，`panel` 是浮窗的大小，`visible`
/// 是屏幕的可用区域。先放在系统设置右边隔 `SETTINGS_GAP`、顶边对齐；右边放不下放左边；两边都放不下时右边缘对齐
/// 系统设置的右边缘、压在它里侧，系统设置伸出可用区域时靠可用区域右边；最后整体夹在可用区域里。
fn place(settings: Bounds<Pixels>, panel: Size<Pixels>, visible: Bounds<Pixels>) -> Bounds<Pixels> {
    let gap = px(SETTINGS_GAP);
    let panel = size(panel.width.min(visible.size.width), panel.height.min(visible.size.height));
    let right = settings.right() + gap;
    let left = settings.left() - gap - panel.width;
    let x = if right + panel.width <= visible.right() {
        right
    } else if left >= visible.left() {
        left
    } else {
        settings.right().min(visible.right()) - panel.width
    };
    let x = x.min(visible.right() - panel.width).max(visible.left());
    let y = settings.top().min(visible.bottom() - panel.height).max(visible.top());
    Bounds::new(point(x, y), panel)
}

/// 开浮窗：等到了系统设置的窗口 `settings` 时摆在它旁边，否则摆在主窗口那块屏的右侧、上下居中。
fn open_window(settings: Option<SettingsWindow>, cx: &mut App) -> Option<WindowHandle<PanelView>> {
    let scale = font_size::scale(cx);
    let wanted = size(px(SIZE.0 * scale), px(SIZE.1 * scale));
    let beside = settings.and_then(|settings| Some((settings, cx.find_display(settings.display)?)));
    let (display, window_bounds) = match beside {
        Some((settings, display)) => {
            let visible = display.visible_bounds();
            (Some(display), Some(place(settings.bounds, wanted, visible)))
        }
        None => {
            let display = crate::window::front_window(cx)
                .and_then(|handle| handle.update(cx, |_, window, cx| window.display(cx)).ok().flatten())
                .or_else(|| cx.primary_display());
            let bounds = display.as_ref().map(|display| {
                let visible = display.visible_bounds();
                let width = wanted.width.min(visible.size.width - px(SCREEN_MARGIN * 2.));
                let height = wanted.height.min(visible.size.height - px(SCREEN_MARGIN * 2.));
                let origin = point(
                    visible.right() - px(SCREEN_MARGIN) - width,
                    visible.top() + (visible.size.height - height) / 2.,
                );
                Bounds::new(origin, size(width, height))
            });
            (display, bounds)
        }
    };
    let options = WindowOptions {
        window_bounds: window_bounds.map(WindowBounds::Windowed),
        display_id: display.map(|display| display.id()),
        kind: WindowKind::PopUp,
        focus: false,
        show: true,
        is_resizable: false,
        is_minimizable: false,
        titlebar: None,
        app_id: Some(runode_update::BUNDLE_ID.into()),
        tabbing_identifier: None,
        ..crate::window::base_options()
    };
    let opened = cx.open_window(options, |window, cx| {
        window.set_window_title(&rust_i18n::t!("onboarding.permissions_manage_title"));
        float(window);
        cx.new(|cx| PanelView::new(window, cx))
    });
    opened.inspect_err(|err| tracing::warn!("failed to open the permissions panel: {err:#}")).ok()
}

/// 补上 GPUI 弹出窗口没有的几样：app 退到后台（用户去点系统设置）时不藏起来；浮在普通窗口上面，但不盖住
/// 菜单和系统的弹框；不画红绿灯；不进「窗口」菜单，切换窗口时跳过。
fn float(window: &Window) {
    use objc2_app_kit::{NSWindowButton, NSWindowCollectionBehavior};

    /// `NSFloatingWindowLevel`。
    const FLOATING: isize = 3;
    let Some(ns_window) = crate::window::ns_window(window) else { return };
    ns_window.setHidesOnDeactivate(false);
    ns_window.setLevel(FLOATING);
    for button in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
        if let Some(button) = ns_window.standardWindowButton(button) {
            button.setHidden(true);
        }
    }
    ns_window.setExcludedFromWindowsMenu(true);
    ns_window.setCollectionBehavior(ns_window.collectionBehavior() | NSWindowCollectionBehavior::IgnoresCycle);
}

struct PanelView {
    permissions: Entity<Permissions>,
    icon: Arc<Image>,
    /// 全开了、等着关窗口；有它时画「都开好了」。
    closing: Option<Task<()>>,
    _watching: WatchGuard,
    /// 系统设置退出时关掉浮窗；浮窗关了跟着不再听。
    _settings_quit: (QuitWatch, Task<()>),
    _subscriptions: [Subscription; 2],
}

impl PanelView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let permissions = Permissions::global(cx);
        let watching = Permissions::watch(&permissions, cx);
        let (quit_watch, mut quits) = system_settings::watch_quit();
        let quit = cx.spawn(async move |_, cx| {
            if quits.next().await.is_some() {
                cx.update(close);
            }
        });
        let subscriptions = [
            cx.observe_in(&permissions, window, |this, _, window, cx| this.follow(window, cx)),
            cx.observe_global::<AppConfig>(|_, cx| cx.notify()),
        ];
        cx.on_release(|this: &mut Self, cx| ui::release_bitmap(this.icon.clone(), cx)).detach();
        let mut this = Self {
            permissions,
            icon: Arc::new(Image::from_bytes(ImageFormat::Png, crate::about::ICON_PNG.to_vec())),
            closing: None,
            _watching: watching,
            _settings_quit: (quit_watch, quit),
            _subscriptions: subscriptions,
        };
        this.follow(window, cx);
        this
    }

    /// 授权变了：重画，列出的几项全开了时过 `ALL_GRANTED_LINGER` 关掉窗口。
    fn follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (granted, total) = self.permissions.read(cx).count();
        if granted == total && self.closing.is_none() {
            self.closing = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(ALL_GRANTED_LINGER).await;
                this.update_in(cx, |_, window, _| window.remove_window()).ok();
            }));
        }
        cx.notify();
    }
}

impl Render for PanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 界面的字号按 rem 写（`ui::font_size`），和主窗口一样设成配置的界面字号。
        window.set_rem_size(font_size::rem_size(cx));
        // 窗口里没有终端，主题按配置的颜色。
        let (fg, bg) = {
            let config = &cx.global::<AppConfig>().0;
            (config.foreground, config.background)
        };
        let theme = &crate::window::install_theme(fg, bg, cx);
        let title = rust_i18n::t!("onboarding.permissions_manage_title").into_owned();
        let close = Button::new("permissions-panel-close", rust_i18n::t!("onboarding.permissions_panel_close"))
            .variant(ButtonVariant::Ghost)
            .with_size(ButtonSize::IconSm)
            .icon(CLOSE_ICON)
            .cursor_pointer()
            .on_press(cx, |_, window, _| window.remove_window());
        // 按钮右边那一段拖着挪窗口；窗口不能缩放，双击不做事。
        let handle = div()
            .id("permissions-panel-drag")
            .flex_1()
            .h_full()
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.start_window_move());
        let bar =
            div().flex_none().w_full().h(px(BAR_HEIGHT)).pl(px(8.)).flex().items_center().child(close).child(handle);
        let icon = div()
            .id("permissions-panel-icon")
            .role(Role::Image)
            .aria_label(rust_i18n::t!("onboarding.permissions_panel_icon").into_owned())
            .child(img(ui::draw_bitmap(&self.icon)).size(px(64.)));
        let icon = draggable_app_icon(icon, &self.icon, 64.);
        let note = if self.closing.is_some() {
            let text = rust_i18n::t!("onboarding.permissions_all_granted").into_owned();
            div()
                .id("permissions-panel-all-granted")
                .aria_static_text(text.clone())
                .flex()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .text_size(FontSize::Body)
                .text_color(theme.success)
                .child(svg().path(CHECK_ICON).size(px(14.)).text_color(theme.success))
                .child(text)
        } else {
            text_label(
                "permissions-panel-hint",
                rust_i18n::t!("onboarding.permissions_panel_hint").into_owned(),
                theme.foreground.opacity(0.75),
            )
            .text_size(FontSize::Body)
            .text_center()
        };
        let note = div().w_full().px(px(14.)).py(px(10.)).rounded(px(10.)).bg(theme.ring.opacity(0.08)).child(note);
        let card = div()
            .w_full()
            .rounded(px(12.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .child(rows(&self.permissions, theme, cx));
        let body = div()
            .id("permissions-panel-body")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .px(px(20.))
            .pb(px(20.))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(14.))
            .child(icon)
            .child(headline(title.clone(), 20.))
            .child(note)
            .child(card);
        ui::font::apply(div(), cx)
            .id("permissions-panel")
            .role(Role::Group)
            .aria_label(title)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(bar)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    const PANEL: Size<Pixels> = Size { width: px(420.), height: px(500.) };
    /// 1440×900 的屏，顶上让出菜单栏。
    const VISIBLE: Bounds<Pixels> =
        Bounds { origin: gpui::Point { x: px(0.), y: px(25.) }, size: Size { width: px(1440.), height: px(875.) } };

    #[test]
    fn beside_the_right_edge() {
        let settings = bounds(200., 100., 700., 600.);
        assert_eq!(place(settings, PANEL, VISIBLE), bounds(912., 100., 420., 500.));
    }

    #[test]
    fn beside_the_left_edge_when_the_right_is_full() {
        let settings = bounds(700., 100., 700., 600.);
        assert_eq!(place(settings, PANEL, VISIBLE), bounds(268., 100., 420., 500.));
    }

    #[test]
    fn inside_the_right_edge_when_neither_side_fits() {
        let settings = bounds(300., 100., 900., 600.);
        assert_eq!(place(settings, PANEL, VISIBLE), bounds(780., 100., 420., 500.));
        // 系统设置伸出屏幕右边时靠屏幕右边。
        let settings = bounds(300., 100., 1300., 600.);
        assert_eq!(place(settings, PANEL, VISIBLE), bounds(1020., 100., 420., 500.));
    }

    #[test]
    fn clamped_into_the_visible_area() {
        // 系统设置顶在菜单栏下面、底下放不下浮窗：往上挪到放得下。
        let settings = bounds(200., 600., 700., 280.);
        assert_eq!(place(settings, PANEL, VISIBLE), bounds(912., 400., 420., 500.));
        // 屏幕比浮窗还矮：缩到可用区域的高度，顶在可用区域上边。
        let short = Bounds { origin: VISIBLE.origin, size: size(px(1440.), px(400.)) };
        assert_eq!(place(bounds(200., 0., 700., 300.), PANEL, short), bounds(912., 25., 420., 400.));
    }

    #[test]
    fn on_a_secondary_display_in_its_own_coordinates() {
        // 副屏的可用区域没有菜单栏，原点是那块屏的左上角；系统设置的位置也相对它。
        let visible = bounds(0., 0., 1920., 1080.);
        let settings = bounds(1300., 200., 700., 600.);
        assert_eq!(place(settings, PANEL, visible), bounds(868., 200., 420., 500.));
    }
}
