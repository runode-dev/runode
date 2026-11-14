//! 首次启动的引导页：欢迎、外观、集成与权限、完成四步，一步一屏，macOS 上从集成与权限另能点进管理权限的
//! 一屏，可以随时关掉。窗口把它画在侧栏右边、
//! 缓存着；配置变了它自己重画。按 Esc、关闭按钮、「暂时跳过」和最后一步的按钮都请窗口收起它
//! （`OnboardingEvent::Close`）。要不要在启动时打开记在 `state` 里。

mod look;
#[cfg(target_os = "macos")]
mod permissions;
mod setup;
mod state;

use std::sync::Arc;

use gpui::{
    Action, AnyElement, App, Context, Div, ElementId, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, Image,
    ImageFormat, KeyDownEvent, Render, Role, ScrollHandle, Stateful, Subscription, Window, div, img, point, prelude::*,
    px, svg,
};

#[cfg(target_os = "macos")]
use permissions::panel::close as close_permissions_panel;
#[cfg(target_os = "macos")]
pub(crate) use permissions::panel::is_panel as is_permissions_panel;
pub(crate) use state::{first_launch, mark_done};

/// 关掉管理权限的浮窗；只有 macOS 上有这个浮窗。
#[cfg(not(target_os = "macos"))]
fn close_permissions_panel(_: &mut App) {}

/// `window` 是管理权限的浮窗；只有 macOS 上有这个浮窗。
#[cfg(not(target_os = "macos"))]
pub(crate) fn is_permissions_panel(_: gpui::AnyWindowHandle, _: &App) -> bool {
    false
}

use crate::{
    config::AppConfig,
    ui::{
        self,
        a11y::{Press as _, StaticText as _},
        button::{Button, ButtonSize, ButtonVariant},
        font_size::{FontSize, scaled},
        icons::{ARROW_LEFT_ICON, CHECK_ICON, CLOSE_ICON, PALETTE_ICON, PLUG_ICON},
        kbd::Kbd,
        keys,
        theme::{ActiveTheme as _, Theme},
    },
    window::{GotoAgent, NewSplitRight, NextAgent},
};

/// 这一页要窗口办的事。
pub(crate) enum OnboardingEvent {
    /// 收起这一页。
    Close,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Welcome,
    Look,
    Setup,
    /// 从集成与权限那一步点进来，管理各项系统权限。
    #[cfg(target_os = "macos")]
    Permissions,
    Done,
}

pub(crate) struct Onboarding {
    step: Step,
    focus: FocusHandle,
    scroll: ScrollHandle,
    logo: Arc<Image>,
    /// 外观这一步用的主题，建页时在后台读，读完之前为空。
    themes: Option<look::Themes>,
    /// 外观这一步上次写主题失败的原因。
    theme_error: Option<String>,
    /// 集成与权限这一步的状态。
    setup: setup::Setup,
    /// 集成与权限、管理权限两屏用的系统权限。
    #[cfg(target_os = "macos")]
    permissions: gpui::Entity<permissions::Permissions>,
    /// 集成与权限、管理权限两屏开着时拿着，定时查授权。
    #[cfg(target_os = "macos")]
    watching: Option<permissions::WatchGuard>,
    #[cfg(target_os = "macos")]
    _permissions: Subscription,
    _config: Subscription,
}

impl EventEmitter<OnboardingEvent> for Onboarding {}

impl Focusable for Onboarding {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Onboarding {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.theme().clone();
        let (content, nav) = match self.step {
            Step::Welcome => (self.render_welcome(theme, cx), None),
            Step::Look => {
                let title = rust_i18n::t!("onboarding.look_title").into_owned();
                let lead = rust_i18n::t!("onboarding.look_body").into_owned();
                let body = self.render_look(theme, cx);
                (render_stage(PALETTE_ICON, title, lead, body, theme), Some(stage_nav(Step::Welcome, Step::Setup, cx)))
            }
            Step::Setup => {
                let title = rust_i18n::t!("onboarding.setup_title").into_owned();
                let lead = rust_i18n::t!("onboarding.setup_body").into_owned();
                let body = self.render_setup(theme, cx);
                (render_stage(PLUG_ICON, title, lead, body, theme), Some(stage_nav(Step::Look, Step::Done, cx)))
            }
            #[cfg(target_os = "macos")]
            Step::Permissions => (self.render_permissions(theme, cx), Some(stage_nav(Step::Setup, Step::Done, cx))),
            Step::Done => (render_done(theme, cx), None),
        };
        let scroll = div()
            .id("onboarding-scroll")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .flex()
            .flex_col()
            .items_center()
            .px(px(48.))
            .pb(px(if nav.is_some() { 12. } else { 40. }))
            .when(nav.is_none(), |scroll| scroll.pt(px(16.)))
            .child(div().my_auto().w_full().max_w(scaled(720.)).child(content));
        let title = div()
            .min_w_0()
            .truncate()
            .text_size(FontSize::Medium)
            .font_weight(FontWeight::SEMIBOLD)
            .child(rust_i18n::t!("onboarding.title").into_owned());
        let label = rust_i18n::t!("onboarding.close").into_owned();
        let close = Button::new("onboarding-close", label.clone())
            .variant(ButtonVariant::Ghost)
            .icon(CLOSE_ICON)
            .icon_size(13.)
            .h(scaled(28.))
            .pl(px(9.))
            .pr(px(6.))
            .gap(px(7.))
            .rounded(px(7.))
            .text_color(theme.subtle_foreground)
            .occlude()
            .child(label)
            .child(Kbd::new("esc"))
            .on_press(cx, |_, _, cx| cx.emit(OnboardingEvent::Close));
        let header = div()
            .flex_none()
            .w_full()
            .h(scaled(34.))
            .mt(px(8.))
            .pl(px(20.))
            .pr(px(14.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .child(title)
            .child(close);
        div()
            .id("onboarding-page")
            .role(Role::Group)
            .aria_label(rust_i18n::t!("onboarding.title").into_owned())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    cx.emit(OnboardingEvent::Close);
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.foreground)
            .child(header)
            .child(scroll)
            .children(nav.map(|nav| div().flex_none().w_full().pt(px(4.)).pb(px(20.)).child(nav)))
    }
}

impl Onboarding {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let config = cx.observe_global::<AppConfig>(|this, cx| {
            this.follow_theme(cx);
            cx.notify();
        });
        // 管理权限的浮窗跟着这一页关掉，不管这一页是怎么离开的。
        cx.on_release(|this: &mut Self, cx| {
            ui::release_bitmap(this.logo.clone(), cx);
            close_permissions_panel(cx);
        })
        .detach();
        Self::load_themes(cx);
        #[cfg(target_os = "macos")]
        let permissions = permissions::Permissions::global(cx);
        #[cfg(target_os = "macos")]
        let permissions_changed = cx.observe(&permissions, |_, _, cx| cx.notify());
        Self {
            step: Step::Welcome,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            logo: Arc::new(Image::from_bytes(ImageFormat::Png, crate::about::ICON_PNG.to_vec())),
            themes: None,
            theme_error: None,
            setup: setup::Setup::default(),
            #[cfg(target_os = "macos")]
            permissions,
            #[cfg(target_os = "macos")]
            watching: None,
            #[cfg(target_os = "macos")]
            _permissions: permissions_changed,
            _config: config,
        }
    }

    /// 翻到 `step`，从顶上看起。
    fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        if step == Step::Setup {
            self.read_skill(cx);
        }
        #[cfg(target_os = "macos")]
        if !matches!(step, Step::Setup | Step::Permissions) {
            self.watching = None;
        } else if self.watching.is_none() {
            self.watching = Some(permissions::Permissions::watch(&self.permissions, cx));
        }
        self.step = step;
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    fn render_welcome(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let logo = div()
            .id("onboarding-logo")
            .role(Role::Image)
            .aria_label(rust_i18n::t!("onboarding.logo").into_owned())
            .child(img(ui::draw_bitmap(&self.logo)).size(px(112.)));
        let skip = rust_i18n::t!("onboarding.skip").into_owned();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(18.))
            .text_center()
            .child(logo)
            .child(headline(rust_i18n::t!("onboarding.welcome_title").into_owned(), 36.))
            .child(lead(rust_i18n::t!("onboarding.welcome_body").into_owned(), theme).max_w(scaled(560.)))
            .child(
                div().pt(px(12.)).child(
                    primary("onboarding-continue", rust_i18n::t!("onboarding.continue").into_owned(), true)
                        .on_press(cx, |this, _, cx| this.go(Step::Look, cx)),
                ),
            )
            .child(
                Button::new("onboarding-skip", skip.clone())
                    .variant(ButtonVariant::Link)
                    .cursor_pointer()
                    .text_color(theme.subtle_foreground)
                    .child(skip)
                    .on_press(cx, |_, _, cx| cx.emit(OnboardingEvent::Close)),
            )
    }
}

/// 中间的一步：顶上图标、标题和说明，下面这一步的内容。
fn render_stage(icon: &'static str, title: String, lead_text: String, body: AnyElement, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(20.))
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.))
                .text_center()
                .child(badge(icon, 44., theme))
                .child(headline(title, 24.))
                .child(lead(lead_text, theme)),
        )
        .child(body)
}

/// 中间几步钉在页底的返回和继续，内容再长也不用滚动去找。
fn stage_nav(back: Step, next: Step, cx: &mut Context<Onboarding>) -> Div {
    let back = Button::new("onboarding-back", rust_i18n::t!("onboarding.back").into_owned())
        .variant(ButtonVariant::Secondary)
        .with_size(ButtonSize::Icon)
        .icon(ARROW_LEFT_ICON)
        .icon_size(16.)
        .size(scaled(36.))
        .rounded_full()
        .cursor_pointer()
        .on_press(cx, move |this, _, cx| this.go(back, cx));
    let next = primary("onboarding-continue", rust_i18n::t!("onboarding.continue").into_owned(), true)
        .on_press(cx, move |this, _, cx| this.go(next, cx));
    div().flex().items_center().justify_center().gap(px(16.)).child(back).child(next)
}

/// 最后一步：几个值得记住的快捷键，取自键位表。
fn render_done(theme: &Theme, cx: &mut Context<Onboarding>) -> Div {
    let rows: [(&dyn Action, &'static str, String); 3] = [
        (&GotoAgent, "onboarding-key-goto-agent", rust_i18n::t!("onboarding.key_goto_agent").into_owned()),
        (&NextAgent, "onboarding-key-next-agent", rust_i18n::t!("onboarding.key_next_agent").into_owned()),
        (&NewSplitRight, "onboarding-key-split-right", rust_i18n::t!("onboarding.key_split_right").into_owned()),
    ];
    let card = div()
        .w_full()
        .rounded(px(12.))
        .bg(theme.surface)
        .border_1()
        .border_color(theme.border)
        .overflow_hidden()
        .children(rows.into_iter().enumerate().map(|(ix, (action, id, label))| {
            shortcut_row(id, label, keys::binding_caps(action, cx), theme)
                .when(ix > 0, |row| row.border_t_1().border_color(theme.border))
        }));
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(18.))
        .text_center()
        .child(badge(CHECK_ICON, 56., theme))
        .child(headline(rust_i18n::t!("onboarding.done_title").into_owned(), 32.))
        .child(lead(rust_i18n::t!("onboarding.done_body").into_owned(), theme))
        .child(div().w_full().max_w(scaled(520.)).pt(px(8.)).child(card))
        .child(
            div().pt(px(12.)).child(
                primary("onboarding-start", rust_i18n::t!("onboarding.start").into_owned(), false)
                    .on_press(cx, |_, _, cx| cx.emit(OnboardingEvent::Close)),
            ),
        )
}

/// 快捷键卡片里的一行：左边作用，右边一个键一块键帽，没绑定时写没绑定。整行连同快捷键作为一句报给辅助工具，
/// 键帽不另报。
fn shortcut_row(id: &'static str, label: String, caps: Option<Vec<Vec<String>>>, theme: &Theme) -> Stateful<Div> {
    let not_bound = || rust_i18n::t!("onboarding.not_bound").into_owned();
    let spoken = match &caps {
        Some(strokes) => strokes.iter().map(|caps| keys::join(caps)).collect::<Vec<_>>().join(" "),
        None => not_bound(),
    };
    let keys = match caps {
        Some(strokes) => div().flex().items_center().gap(px(4.)).children(strokes.into_iter().flatten().map(|cap| {
            Kbd::new(cap).h(scaled(24.)).min_w(scaled(24.)).px(px(7.)).rounded(px(5.)).text_size(FontSize::Medium)
        })),
        None => div().text_color(theme.subtle_foreground).child(not_bound()),
    };
    div()
        .id(id)
        .aria_static_text(format!("{label}: {spoken}"))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(16.))
        .px(px(18.))
        .py(px(12.))
        .text_size(FontSize::Large)
        .child(div().min_w_0().child(label))
        .child(keys)
}

/// 主按钮：反色、圆角到底。`text` 是报给辅助工具的名字，`arrow` 时画出来的字后面带个箭头。
fn primary(id: &'static str, text: String, arrow: bool) -> Button {
    let shown = if arrow { format!("{text}  →") } else { text.clone() };
    Button::new(id, text)
        .variant(ButtonVariant::Default)
        .with_size(ButtonSize::Lg)
        .rounded_full()
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .child(shown)
}

/// 一步顶上那个圆底的图标。
fn badge(icon: &'static str, size: f32, theme: &Theme) -> Div {
    div()
        .flex_none()
        .size(px(size))
        .rounded_full()
        .bg(theme.mix(0.08))
        .flex()
        .items_center()
        .justify_center()
        .child(svg().path(icon).size(px(size * 0.46)).text_color(theme.foreground))
}

/// 每步的大标题；每页只有一个，id 写死。
fn headline(text: String, size: f32) -> Stateful<Div> {
    div()
        .id("onboarding-headline")
        .role(Role::Heading)
        .aria_level(1)
        .aria_label(text.clone())
        .text_size(scaled(size))
        .line_height(scaled(size * 1.2))
        .font_weight(FontWeight::BOLD)
        .child(text)
}

/// 大标题下面的一段说明；每页只有一段，id 写死。
fn lead(text: String, theme: &Theme) -> Stateful<Div> {
    text_label("onboarding-lead", text, theme.foreground.opacity(0.6))
        .text_size(FontSize::Title)
        .line_height(scaled(24.))
}

/// 一句报给辅助工具的文字：GPUI 不报子元素里的文字，要另外写。
fn text_label(id: impl Into<ElementId>, text: String, color: Hsla) -> Stateful<Div> {
    div().id(id).aria_static_text(text.clone()).text_color(color).child(text)
}
