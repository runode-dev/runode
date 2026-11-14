//! 系统权限（只在 macOS 上有）：完全磁盘访问、屏幕录制、辅助功能和通知开没开、去哪里开。一个 app 里只有
//! 一份 `Permissions`，`rows` 把各项一项一行画出来。引导页里「管理权限」这一屏列出各项；临时签名的 app
//! 请求授权后系统不一定把它列进系统设置的列表，所以这一屏和浮窗上的 app 图标都能拖进列表，这一屏另给「在访达
//! 中显示」，让用户从访达里拖。

pub(super) mod panel;
mod system_settings;

use std::{
    rc::{Rc, Weak},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui::{
    App, AsyncApp, Context, Div, Entity, ExternalDragPayload, FileDragPaths, FontWeight, Global, Image, Render, Role,
    SharedString, Stateful, Window, div, img, prelude::*, px, svg,
};

use super::{Onboarding, headline, text_label};
use crate::{
    alerts::NotificationAccess,
    screen_process::Access,
    ui::{
        self,
        a11y::{Press as _, StaticText as _},
        button::{Button, ButtonSize, ButtonVariant},
        font_size::{FontSize, scaled},
        icons::{ARROW_UP_RIGHT_ICON, BELL_ICON, CHECK_ICON, FOLDER_ICON, KEYBOARD_ICON, MONITOR_ICON},
        theme::Theme,
    },
};

/// 有人看着时隔多久查一次授权。
const POLL: Duration = Duration::from_secs(2);

/// 这个进程里按过屏幕录制、辅助功能的「授权…」：系统只在头一次请求时弹框，请求完马上返回，之后再按打开
/// 系统设置。
static SCREEN_REQUESTED: AtomicBool = AtomicBool::new(false);
static INPUT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// 一项授权，按列出来的顺序。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    FullDisk,
    Screen,
    Input,
    Notifications,
}

impl Kind {
    const ALL: [Self; 4] = [Self::FullDisk, Self::Screen, Self::Input, Self::Notifications];

    /// 拼元素 id 用。
    fn key(self) -> &'static str {
        match self {
            Self::FullDisk => "disk",
            Self::Screen => "screen",
            Self::Input => "input",
            Self::Notifications => "notifications",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::FullDisk => FOLDER_ICON,
            Self::Screen => MONITOR_ICON,
            Self::Input => KEYBOARD_ICON,
            Self::Notifications => BELL_ICON,
        }
    }

    fn name(self) -> String {
        match self {
            Self::FullDisk => rust_i18n::t!("onboarding.permission_disk"),
            Self::Screen => rust_i18n::t!("onboarding.permission_screen"),
            Self::Input => rust_i18n::t!("onboarding.permission_input"),
            Self::Notifications => rust_i18n::t!("onboarding.permission_notifications"),
        }
        .into_owned()
    }

    fn hint(self) -> String {
        match self {
            Self::FullDisk => rust_i18n::t!("onboarding.permission_disk_hint"),
            Self::Screen => rust_i18n::t!("onboarding.permission_screen_hint"),
            Self::Input => rust_i18n::t!("onboarding.permission_input_hint"),
            Self::Notifications => rust_i18n::t!("onboarding.permission_notifications_hint"),
        }
        .into_owned()
    }

    /// 系统设置里这一项的那一栏。
    fn settings_url(self) -> String {
        let privacy = |pane: &str| format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
        match self {
            Self::FullDisk => privacy("Privacy_AllFiles"),
            Self::Screen => privacy("Privacy_ScreenCapture"),
            Self::Input => privacy("Privacy_Accessibility"),
            Self::Notifications => format!(
                "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={}",
                runode_update::BUNDLE_ID
            ),
        }
    }
}

/// 打开系统设置里 `kind` 那一栏；打开系统设置一律经这里。在 .app 里打开隐私的几栏时另开浮窗（`panel`），
/// 列表里没有 Runode 时用户从浮窗上把图标拖进去；通知那一栏不用拖。
fn open_settings(kind: Kind, cx: &mut App) {
    cx.open_url(&kind.settings_url());
    if kind != Kind::Notifications && crate::about::app_bundle().is_some() {
        panel::open(cx);
    }
}

/// 查到的授权，一个 app 里只有一份（`Permissions::global`），变了才通知。
pub(super) struct Permissions {
    /// 第一次查到之前为空。
    access: Option<Access>,
    /// 不在 .app 里、或者还没问到时为空。
    notifications: Option<NotificationAccess>,
    /// 在 .app 里：通知中心和「在访达中显示」只在 .app 里用得上。
    in_app: bool,
    /// 发出去的 `WatchGuard` 共用的那一份；全部丢掉后升级不了，定时查的任务据此停下。
    watching: Weak<()>,
}

struct PermissionsState(Entity<Permissions>);

impl Global for PermissionsState {}

/// 拿着时定时查授权；同一份 `Permissions` 的守卫全部丢掉后不再查。
pub(super) struct WatchGuard {
    _watching: Rc<()>,
}

impl Permissions {
    /// 这个 app 里的那一份，第一次用到时才建。
    pub(super) fn global(cx: &mut App) -> Entity<Self> {
        if let Some(state) = cx.try_global::<PermissionsState>() {
            return state.0.clone();
        }
        let permissions = cx.new(|_| Self {
            access: None,
            notifications: None,
            in_app: crate::alerts::notifications_available(),
            watching: Weak::new(),
        });
        cx.set_global(PermissionsState(permissions.clone()));
        permissions
    }

    /// 开始定时查授权，没在查时马上查一次。
    pub(super) fn watch(this: &Entity<Self>, cx: &mut App) -> WatchGuard {
        if let Some(watching) = this.read(cx).watching.upgrade() {
            return WatchGuard { _watching: watching };
        }
        let watching = Rc::new(());
        let alive = Rc::downgrade(&watching);
        this.update(cx, |this, cx| {
            this.watching = alive.clone();
            let in_app = this.in_app;
            cx.spawn(async move |this, cx| {
                while alive.strong_count() > 0 {
                    let found = query(in_app, cx).await;
                    if this.update(cx, |this, cx| this.take(found, cx)).is_err() {
                        break;
                    }
                    cx.background_executor().timer(POLL).await;
                }
            })
            .detach();
        });
        WatchGuard { _watching: watching }
    }

    /// 在 .app 里。
    pub(super) fn in_app(&self) -> bool {
        self.in_app
    }

    /// 列出的几项：通知中心在 .app 外会让进程退出，不在 .app 里时不列通知。
    fn kinds(&self) -> impl Iterator<Item = Kind> + use<> {
        let in_app = self.in_app;
        Kind::ALL.into_iter().filter(move |kind| in_app || *kind != Kind::Notifications)
    }

    fn granted(&self, kind: Kind) -> bool {
        let access = self.access.unwrap_or_default();
        match kind {
            Kind::FullDisk => access.full_disk,
            Kind::Screen => access.screen.screen_capture,
            Kind::Input => access.screen.post_event,
            Kind::Notifications => self.notifications == Some(NotificationAccess::Allowed),
        }
    }

    /// 开了几项、一共几项。
    pub(super) fn count(&self) -> (usize, usize) {
        let kinds: Vec<Kind> = self.kinds().collect();
        (kinds.iter().filter(|kind| self.granted(**kind)).count(), kinds.len())
    }

    /// 马上另查一次，不等下一轮。
    fn check(&mut self, cx: &mut Context<Self>) {
        let in_app = self.in_app;
        cx.spawn(async move |this, cx| {
            let found = query(in_app, cx).await;
            this.update(cx, |this, cx| this.take(found, cx)).ok();
        })
        .detach();
    }

    /// 记下查到的授权，变了才通知。
    fn take(&mut self, (access, notifications): (Access, Option<NotificationAccess>), cx: &mut Context<Self>) {
        if self.access != Some(access) || self.notifications != notifications {
            self.access = Some(access);
            self.notifications = notifications;
            cx.notify();
        }
    }

    /// 按了一行的「授权…」。完全磁盘访问没有请求的接口，直接打开系统设置；屏幕录制、辅助功能在这个进程里
    /// 头一次按时请求、系统弹框，之后打开系统设置；通知没问过时请求，拒绝过时打开系统设置。
    fn grant(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let requested = match kind {
            Kind::FullDisk => None,
            Kind::Screen => Some(&SCREEN_REQUESTED),
            Kind::Input => Some(&INPUT_REQUESTED),
            Kind::Notifications => {
                if self.notifications == Some(NotificationAccess::Denied) {
                    open_settings(kind, cx);
                    return;
                }
                let requested = crate::alerts::request_notifications();
                cx.spawn(async move |this, cx| {
                    requested.await;
                    this.update(cx, |this, cx| this.check(cx)).ok();
                })
                .detach();
                return;
            }
        };
        if !requested.is_some_and(|requested| !requested.swap(true, Ordering::Relaxed)) {
            open_settings(kind, cx);
            return;
        }
        let job = cx.background_spawn(async move {
            if kind == Kind::Screen {
                runode_screen::request_screen_capture();
            } else {
                runode_screen::request_post_event();
            }
        });
        cx.spawn(async move |this, cx| {
            job.await;
            this.update(cx, |this, cx| this.check(cx)).ok();
        })
        .detach();
    }
}

/// 另起进程查屏幕录制、辅助功能和完全磁盘访问，在 .app 里另问通知中心。
async fn query(in_app: bool, cx: &mut AsyncApp) -> (Access, Option<NotificationAccess>) {
    let access = cx.background_executor().spawn(async { crate::screen_process::current_access() });
    let notifications = if in_app { crate::alerts::notification_access().await } else { None };
    (access.await, notifications)
}

/// 列出的各项，一项一行，行间一条分隔线。元素 id 只按权限的种类取，一个窗口里同一帧只能画一份。
pub(super) fn rows<V: 'static>(state: &Entity<Permissions>, theme: &Theme, cx: &mut Context<V>) -> Div {
    let permissions = state.read(cx);
    let kinds: Vec<(Kind, bool)> = permissions.kinds().map(|kind| (kind, permissions.granted(kind))).collect();
    div().flex().flex_col().children(kinds.into_iter().enumerate().map(|(ix, (kind, granted))| {
        row(state, kind, granted, theme, cx).when(ix > 0, |row| row.border_t_1().border_color(theme.border))
    }))
}

/// 一项一行：左边图标，中间名字、打开系统设置的按钮和一句说明，右边开了时的对勾或「授权…」。
fn row<V: 'static>(state: &Entity<Permissions>, kind: Kind, granted: bool, theme: &Theme, cx: &mut Context<V>) -> Div {
    let key = kind.key();
    let id = |part: &str| SharedString::from(format!("onboarding-permission-{key}-{part}"));
    let name = kind.name();
    let open = Button::new(id("open"), rust_i18n::t!("onboarding.permissions_open_settings", name = name.as_str()))
        .variant(ButtonVariant::Ghost)
        .with_size(ButtonSize::IconXs)
        .icon(ARROW_UP_RIGHT_ICON)
        .cursor_pointer()
        .on_press(cx, move |_, _, cx| open_settings(kind, cx));
    let status = if granted {
        let granted = rust_i18n::t!("onboarding.permission_granted").into_owned();
        div()
            .id(id("status"))
            .aria_static_text(rust_i18n::t!("onboarding.permission_granted_named", name = name.as_str()).into_owned())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(4.))
            .text_size(FontSize::Body)
            .text_color(theme.success)
            .child(svg().path(CHECK_ICON).size(px(14.)).text_color(theme.success))
            .child(granted)
            .into_any_element()
    } else {
        let state = state.clone();
        Button::new(id("grant"), rust_i18n::t!("onboarding.permission_grant", name = name.as_str()))
            .variant(ButtonVariant::Secondary)
            .with_size(ButtonSize::Sm)
            .cursor_pointer()
            .child(rust_i18n::t!("onboarding.grant").into_owned())
            .on_press(cx, move |_, _, cx| state.update(cx, |state, cx| state.grant(kind, cx)))
            .into_any_element()
    };
    let icon = div()
        .flex_none()
        .size(px(30.))
        .rounded(px(7.))
        .bg(theme.mix(0.08))
        .flex()
        .items_center()
        .justify_center()
        .child(svg().path(kind.icon()).size(px(16.)).text_color(theme.foreground));
    let text = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(
                    text_label(id("title"), name, theme.foreground)
                        .text_size(FontSize::Medium)
                        .font_weight(FontWeight::MEDIUM),
                )
                .child(open),
        )
        .child(text_label(id("hint"), kind.hint(), theme.foreground.opacity(0.55)).text_size(FontSize::Small));
    div().flex().items_center().gap(px(12.)).px(px(16.)).py(px(10.)).child(icon).child(text).child(status)
}

/// 拖着的 app 图标，也是拖动时窗口里跟着鼠标画的那张图。
#[derive(Clone)]
struct AppBundleDrag {
    image: Arc<Image>,
    /// 边长，点。
    size: f32,
}

impl Render for AppBundleDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        img(ui::draw_bitmap(&self.image)).size(px(self.size))
    }
}

/// `icon` 画着边长 `size` 点的 app 图标 `image`；在 .app 里时让它能拖到别的 app，拖出窗口时交给系统的是
/// Runode.app 本身，用户把它拖进系统设置的权限列表。
fn draggable_app_icon(icon: Stateful<Div>, image: &Arc<Image>, size: f32) -> Stateful<Div> {
    let Some(bundle) = crate::about::app_bundle() else { return icon };
    icon.cursor_grab()
        .on_drag(AppBundleDrag { image: image.clone(), size }, |drag, _, _, cx| cx.new(|_| drag.clone()))
        .external_drag_payload(move |_: &AppBundleDrag, _, _| {
            Some(ExternalDragPayload::Files(FileDragPaths::new([(bundle.clone(), true)])))
        })
}

impl Onboarding {
    /// 管理权限这一屏的内容：app 图标、标题，提示怎么把 Runode 加进系统设置的列表，下面一项一行。
    pub(super) fn render_permissions(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let logo = div()
            .id("onboarding-logo")
            .role(Role::Image)
            .aria_label(rust_i18n::t!("onboarding.logo").into_owned())
            .child(img(ui::draw_bitmap(&self.logo)).size(px(72.)));
        let logo = draggable_app_icon(logo, &self.logo, 72.);
        let hint = self.permissions.read(cx).in_app().then(|| {
            let reveal = rust_i18n::t!("onboarding.permissions_reveal").into_owned();
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(12.))
                .px(px(14.))
                .py(px(10.))
                .rounded(px(10.))
                .bg(theme.mix(0.05))
                .border_1()
                .border_color(theme.border)
                .child(
                    text_label(
                        "onboarding-permissions-hint",
                        rust_i18n::t!("onboarding.permissions_drag_hint").into_owned(),
                        theme.foreground.opacity(0.75),
                    )
                    .flex_1()
                    .min_w_0()
                    .text_size(FontSize::Body),
                )
                .child(
                    Button::new("onboarding-permissions-reveal", reveal)
                        .variant(ButtonVariant::Secondary)
                        .with_size(ButtonSize::Sm)
                        .cursor_pointer()
                        .on_press(cx, |_, _, cx| {
                            if let Some(bundle) = crate::about::app_bundle() {
                                cx.reveal_path(&bundle);
                            }
                        }),
                )
        });
        let card = div()
            .w_full()
            .rounded(px(12.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .overflow_hidden()
            .child(rows(&self.permissions, theme, cx));
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(14.))
            .child(logo)
            .child(headline(rust_i18n::t!("onboarding.permissions_manage_title").into_owned(), 24.))
            .child(div().w_full().max_w(scaled(600.)).flex().flex_col().gap(px(12.)).children(hint).child(card))
    }
}
