//! 侧栏顶上的「手机端」那一页：先介绍手机端能做什么，再装 App（有安装链接时），最后扫码配对这台 Mac
//! （`remote_access::pairing`）。已经配过手机时第一页换成配对过的设备（`devices`），连着的标出来，能撤销，也能
//! 再配一台。远程访问没开时在这一页就能打开。按 Esc 就请窗口收起它（`MobileEvent::Close`），还在等的口令随之丢掉。
//!
//! 配对这一步能改手机上显示的电脑名（配置项 `remote-access-name`，改了监听方换上新名字后重新生成
//! 二维码），也能挑手机先走哪个网络：挑中的地址排在配对 URI 最前面，手机先试它，连不上再试别的。
//!
//! 这一页是单独的视图，窗口把它画在侧栏右边、缓存着；配置变了由窗口叫它重画，设备表变了重画所有窗口。

pub(crate) mod devices;

use std::{net::IpAddr, rc::Rc};

use gpui::{
    App, ClipboardItem, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent,
    Render, Role, SharedString, Stateful, Subscription, Window, div, prelude::*, px, svg,
};
use runode_remote::{host_name, local_interfaces};

use crate::{
    assets::{PHONE_ICON, REFRESH_ICON, TRASH_ICON},
    config::AppConfig,
    remote_access::pairing::{Pairing, Qr, qr_code},
    ui::{
        a11y::Press,
        button::{Button, ButtonSize, ButtonVariant},
        font_size::{FontSize, scaled},
        input::{Input, InputEvent, InputState},
        select::SelectTrigger,
        theme::{ActiveTheme as _, Theme},
    },
};
use devices::Paired;

/// 手机上显示的电脑名的配置项，见模块说明。
const NAME_KEY: &str = "remote-access-name";

/// iPhone 上装 App 的链接；还没有公开的安装链接时为 `None`，引导里跳过装 App 这一步。
const IOS_APP_URL: Option<&str> = None;
/// 引导页二维码一个模块的边长，像素。
const MODULE_SIZE: f32 = 5.;
/// 二维码那一栏的宽度：版本 10 上下的二维码加留白放得下。
const QR_COLUMN_WIDTH: f32 = 300.;

/// 这一页要窗口办的事。
pub(crate) enum MobileEvent {
    /// 收起这一页。
    Close,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Intro,
    Install,
    Pair,
}

pub(crate) struct MobilePage {
    step: Step,
    pairing: Pairing,
    focus: FocusHandle,
    /// 装 App 的二维码，有安装链接时才有。
    install_qr: Option<Rc<Qr>>,
    /// 电脑名的输入框，以及它的回车、Esc 和失焦；空着时用系统的电脑名 `system_name`。
    name: Entity<InputState>,
    system_name: SharedString,
    _name_events: [Subscription; 2],
    /// 这台机器的网卡和地址，进配对这一步、点刷新时重读。
    interfaces: Vec<(String, IpAddr)>,
    /// 挑的网络，它的地址排在配对 URI 最前面；`None` 时按系统的顺序。
    network: Option<IpAddr>,
    /// 网络的下拉列表展开着。
    network_open: bool,
}

impl EventEmitter<MobileEvent> for MobilePage {}

impl Focusable for MobilePage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for MobilePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_page(cx)
    }
}

impl MobilePage {
    /// 刚打开的一页，在第一步；设备表现读一次，不等下一轮。
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let system_name = host_name();
        let saved = cx.global::<AppConfig>().0.remote_access_name.clone().unwrap_or_default();
        let name = cx.new(|cx| {
            InputState::new(saved, cx)
                .with_placeholder(system_name.clone())
                .with_label(rust_i18n::t!("mobile.name_label").into_owned())
        });
        let events = cx.subscribe_in(&name, window, |this, field, event: &InputEvent, window, cx| match event {
            InputEvent::Next => window.focus(&this.focus, cx),
            // 不改了：换回配置里的名字。
            InputEvent::Dismiss => {
                let saved = cx.global::<AppConfig>().0.remote_access_name.clone().unwrap_or_default();
                field.update(cx, |field, cx| field.set_query(saved, cx));
                window.focus(&this.focus, cx);
            }
            InputEvent::Changed(_) | InputEvent::Previous => {}
        });
        // 回车、点别处都是离开输入框，在这里写回。
        let blurred = cx.on_focus_out(&name.focus_handle(cx), window, |this, _, _, cx| this.save_machine_name(cx));
        devices::refresh(cx).detach();
        Self {
            step: Step::Intro,
            pairing: Pairing::Idle,
            focus: cx.focus_handle(),
            install_qr: IOS_APP_URL.and_then(Qr::new).map(Rc::new),
            name,
            system_name: system_name.into(),
            _name_events: [events, blurred],
            interfaces: Vec::new(),
            network: None,
            network_open: false,
        }
    }

    /// 输入框里的电脑名和配置里的不一样时写回配置；监听方换上新名字后重新生成二维码。要收起这一页时先调：
    /// 没按回车就切到别处时，这一页连同监听失焦的订阅一起没了，等不到失焦再写。
    pub(crate) fn save_machine_name(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).query().trim().to_owned();
        let saved = cx.global::<AppConfig>().0.remote_access_name.clone().unwrap_or_default();
        if name == saved {
            return;
        }
        let values = if name.is_empty() { Vec::new() } else { vec![name] };
        let written = runode_config::config_path()
            .ok_or_else(|| rust_i18n::t!("settings.no_home").into_owned())
            .and_then(|path| {
                crate::config::write_values(&path, NAME_KEY, &values, cx)
                    .map_err(|err| rust_i18n::t!("settings.write_failed", err = err.to_string()).into_owned())
            });
        match written {
            Ok(_) => self.restart_pairing(cx),
            Err(err) => self.pairing = Pairing::Failed(err),
        }
        cx.notify();
    }

    /// 二维码已经在或者在等的时候按现在的名字和网络重新生成；还没开始（远程访问没开）时不动。
    fn restart_pairing(&mut self, cx: &mut Context<Self>) {
        if self.step == Step::Pair && matches!(self.pairing, Pairing::Starting { .. } | Pairing::Waiting(_)) {
            self.start_pairing(cx);
        }
    }

    /// 重读网卡；挑的那个地址没了就回到按系统的顺序。
    fn refresh_interfaces(&mut self) {
        self.interfaces = local_interfaces();
        if self.network.is_some_and(|addr| !self.interfaces.iter().any(|(_, seen)| *seen == addr)) {
            self.network = None;
        }
    }

    fn pick_network(&mut self, network: Option<IpAddr>, cx: &mut Context<Self>) {
        self.network = network;
        self.network_open = false;
        self.restart_pairing(cx);
        cx.notify();
    }

    /// 翻到 `step`。到配对这一步时远程访问开着就马上生成口令，离开时丢掉口令。
    pub(crate) fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        self.pairing = Pairing::Idle;
        self.network_open = false;
        self.refresh_interfaces();
        if step == Step::Pair && cx.global::<AppConfig>().0.remote_access {
            self.start_pairing(cx);
        }
        cx.notify();
    }

    fn start_pairing(&mut self, cx: &mut Context<Self>) {
        let extra = self.network.into_iter().collect();
        self.pairing.start(|this: &mut Self| Some(&mut this.pairing), extra, cx);
    }

    /// 在配置里打开远程访问，等监听方开好就生成口令。
    fn turn_on_remote_access(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = crate::config::set(runode_config::REMOTE_ACCESS, "true", cx) {
            self.pairing = Pairing::Failed(rust_i18n::t!("settings.write_failed", err = err.to_string()).into_owned());
            cx.notify();
            return;
        }
        self.start_pairing(cx);
    }

    fn render_page(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = &cx.theme().clone();
        let content = match self.step {
            Step::Intro => self.render_intro(theme, cx),
            Step::Install => self.render_install(theme, cx),
            Step::Pair => self.render_pair(theme, cx),
        };
        div()
            .id("mobile-page")
            .role(Role::Group)
            .aria_label(rust_i18n::t!("mobile.entry").into_owned())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    cx.emit(MobileEvent::Close);
                }
            }))
            .size_full()
            .overflow_y_scroll()
            .text_color(theme.foreground)
            .flex()
            .flex_col()
            .items_center()
            .px(px(48.))
            .py(px(40.))
            .child(div().my_auto().w_full().max_w(px(820.)).child(content))
    }

    fn render_intro(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let first = if IOS_APP_URL.is_some() { Step::Install } else { Step::Pair };
        let eyebrow_text = rust_i18n::t!("mobile.eyebrow").into_owned();
        let eyebrow = div()
            .id("mobile-eyebrow")
            .role(Role::Label)
            .aria_label(eyebrow_text.clone())
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(FontSize::Body)
            .text_color(theme.foreground.opacity(0.5))
            .child(svg().path(PHONE_ICON).size(px(14.)).text_color(theme.foreground.opacity(0.5)))
            .child(eyebrow_text);
        let paired = devices::devices(cx).to_vec();
        if !paired.is_empty() {
            let rows: Vec<_> = paired.iter().map(|device| device_row(device, theme, cx)).collect();
            let another = Button::new("mobile-pair-another", rust_i18n::t!("mobile.pair_another").into_owned())
                .variant(ButtonVariant::Outline)
                .with_size(ButtonSize::Lg)
                .rounded_full()
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .gap(px(8.))
                .child(rust_i18n::t!("mobile.pair_another").into_owned())
                .child(svg().path(PHONE_ICON).size(px(14.)).text_color(theme.foreground.opacity(0.8)))
                .on_press(cx, move |this, _, cx| this.go(first, cx));
            return div()
                .max_w(px(640.))
                .flex()
                .flex_col()
                .gap(px(18.))
                .child(eyebrow)
                .child(headline(rust_i18n::t!("mobile.paired_intro_title").into_owned(), 40.))
                .child(lead(rust_i18n::t!("mobile.paired_intro_body").into_owned(), theme))
                .child(
                    div()
                        .id("mobile-devices")
                        .role(Role::List)
                        .aria_label(rust_i18n::t!("mobile.devices").into_owned())
                        .pt(px(12.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .children(rows),
                )
                .child(div().pt(px(24.)).flex().child(another));
        }
        div()
            .max_w(px(560.))
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(eyebrow)
            .child(headline(rust_i18n::t!("mobile.intro_title").into_owned(), 40.))
            .child(lead(rust_i18n::t!("mobile.intro_body").into_owned(), theme))
            .child(
                div().pt(px(12.)).child(
                    {
                        let label = format!("{}  →", rust_i18n::t!("mobile.start"));
                        Button::new("mobile-start", spoken(&label))
                            .variant(ButtonVariant::Default)
                            .with_size(ButtonSize::Lg)
                            .rounded_full()
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .child(label)
                    }
                    .on_press(cx, move |this, _, cx| this.go(first, cx)),
                ),
            )
    }

    fn render_install(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let url = IOS_APP_URL.unwrap_or_default();
        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(step_line(1, theme))
            .child(headline(rust_i18n::t!("mobile.install_title").into_owned(), 32.))
            .child(lead(rust_i18n::t!("mobile.install_body").into_owned(), theme))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(
                        Button::new("mobile-open-app", rust_i18n::t!("mobile.open_link").into_owned())
                            .variant(ButtonVariant::Outline)
                            .with_size(ButtonSize::Lg)
                            .rounded_full()
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .on_press(cx, move |_, _, cx| cx.open_url(url)),
                    )
                    .child(
                        Button::new("mobile-copy-app", rust_i18n::t!("mobile.copy_link").into_owned())
                            .variant(ButtonVariant::Link)
                            .with_size(ButtonSize::Lg)
                            .cursor_pointer()
                            .on_press(cx, move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(url.to_owned()))
                            }),
                    ),
            );
        let right = div().flex_none().w(px(QR_COLUMN_WIDTH)).flex().justify_center().children(
            self.install_qr.clone().map(|qr| {
                qr_image("mobile-install-qr", rust_i18n::t!("mobile.install_qr").into_owned(), url.to_owned())
                    .child(qr_code(qr, MODULE_SIZE))
            }),
        );
        let footer = footer(
            {
                let label = format!("←  {}", rust_i18n::t!("mobile.back"));
                Button::new("mobile-back", spoken(&label))
                    .variant(ButtonVariant::Link)
                    .with_size(ButtonSize::Lg)
                    .cursor_pointer()
                    .child(label)
            }
            .on_press(cx, |this, _, cx| this.go(Step::Intro, cx)),
            Some(
                {
                    let label = format!("{}  →", rust_i18n::t!("mobile.next"));
                    Button::new("mobile-next", spoken(&label))
                        .variant(ButtonVariant::Default)
                        .with_size(ButtonSize::Lg)
                        .rounded_full()
                        .font_weight(FontWeight::MEDIUM)
                        .cursor_pointer()
                        .child(label)
                }
                .on_press(cx, |this, _, cx| this.go(Step::Pair, cx)),
            ),
        );
        columns(left, right).child(footer)
    }

    fn render_pair(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let config = &cx.global::<AppConfig>().0;
        let (remote_on, background_on) = (config.remote_access, config.terminal_host);
        let back_to = if IOS_APP_URL.is_some() { Step::Install } else { Step::Intro };
        let back = {
            let label = format!("←  {}", rust_i18n::t!("mobile.back"));
            Button::new("mobile-back", spoken(&label))
                .variant(ButtonVariant::Link)
                .with_size(ButtonSize::Lg)
                .cursor_pointer()
                .child(label)
        }
        .on_press(cx, move |this, _, cx| this.go(back_to, cx));
        let mut left = div().flex_1().min_w_0().flex().flex_col().gap(px(18.));
        if IOS_APP_URL.is_some() {
            left = left.child(step_line(2, theme));
        }

        if let Pairing::Paired { name } = &self.pairing {
            let actions = div()
                .flex()
                .items_center()
                .gap(px(16.))
                .child(
                    Button::new("mobile-done", rust_i18n::t!("mobile.done").into_owned())
                        .variant(ButtonVariant::Default)
                        .with_size(ButtonSize::Lg)
                        .rounded_full()
                        .font_weight(FontWeight::MEDIUM)
                        .cursor_pointer()
                        .on_press(cx, |_, _, cx| cx.emit(MobileEvent::Close)),
                )
                .child(
                    Button::new("mobile-again", rust_i18n::t!("mobile.again").into_owned())
                        .variant(ButtonVariant::Link)
                        .with_size(ButtonSize::Lg)
                        .cursor_pointer()
                        .on_press(cx, |this, _, cx| this.start_pairing(cx)),
                );
            // `terminal-host` 关着时退出 app 宿主跟着退出、远程访问跟着停，提醒一句。
            let background = (!background_on).then(|| {
                notice("mobile-background", rust_i18n::t!("settings.pairing.background").into_owned(), theme).child(
                    div().pt(px(10.)).child(
                        Button::new("mobile-keep", rust_i18n::t!("mobile.keep").into_owned())
                            .variant(ButtonVariant::Outline)
                            .with_size(ButtonSize::Lg)
                            .rounded_full()
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .on_press(
                                cx,
                                // 配置重载后窗口跟着重画，这一块就收起来了。
                                |_, _, cx| {
                                    if let Err(err) = crate::config::set(runode_config::TERMINAL_HOST, "true", cx) {
                                        tracing::warn!("could not turn on {}: {err:#}", runode_config::TERMINAL_HOST);
                                    }
                                },
                            ),
                    ),
                )
            });
            let left = left
                .child(headline(rust_i18n::t!("mobile.paired_title").into_owned(), 32.))
                .child(lead(rust_i18n::t!("mobile.paired_body", name = name).into_owned(), theme))
                .children(background)
                .child(actions);
            return div().max_w(px(560.)).child(left);
        }

        left = left
            .child(headline(rust_i18n::t!("mobile.pair_title").into_owned(), 32.))
            .child(lead(rust_i18n::t!("mobile.pair_body").into_owned(), theme));
        if let Pairing::Failed(err) = &self.pairing {
            left = left.child(notice("mobile-error", err.clone(), theme));
        }
        if remote_on {
            left = left.child(self.render_machine_name(theme, cx)).child(self.render_network(theme, cx));
        }
        let right = div().flex_none().w(px(QR_COLUMN_WIDTH)).flex().flex_col().items_center().gap(px(12.));
        let right = match &self.pairing {
            Pairing::Waiting(waiting) => {
                let uri = waiting.uri();
                left = left.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(text_label(
                            "mobile-cant-scan",
                            rust_i18n::t!("mobile.cant_scan").into_owned(),
                            theme.foreground.opacity(0.5),
                        ))
                        .child({
                            let uri = uri.clone();
                            Button::new("mobile-copy-code", rust_i18n::t!("mobile.copy_code").into_owned())
                                .variant(ButtonVariant::Link)
                                .with_size(ButtonSize::Lg)
                                .cursor_pointer()
                                .on_press(cx, move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(uri.to_string()))
                                })
                        }),
                );
                // 二维码里就是配对链接，写进说明里，读屏和自动化工具不用扫码也拿得到。
                let qr = qr_image("mobile-pair-qr", rust_i18n::t!("mobile.pair_qr").into_owned(), uri.to_string())
                    .child(qr_code(waiting.qr(), MODULE_SIZE));
                right.child(qr).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .text_size(FontSize::Body)
                        .child(text_label(
                            "mobile-expires",
                            rust_i18n::t!("mobile.expires", time = waiting.remaining()).into_owned(),
                            theme.foreground.opacity(0.5),
                        ))
                        .child(
                            Button::new("mobile-regenerate", rust_i18n::t!("mobile.regenerate").into_owned())
                                .variant(ButtonVariant::Link)
                                .with_size(ButtonSize::Lg)
                                .cursor_pointer()
                                .on_press(cx, |this, _, cx| this.start_pairing(cx)),
                        ),
                )
            }
            Pairing::Starting { .. } => {
                right.child(placeholder(rust_i18n::t!("settings.pairing.starting").into_owned(), theme))
            }
            Pairing::Idle | Pairing::Failed(_) => {
                left = if remote_on {
                    let label =
                        if matches!(self.pairing, Pairing::Failed(_)) { "mobile.retry" } else { "mobile.generate" };
                    left.child(
                        div().child(
                            Button::new("mobile-generate", rust_i18n::t!(label).into_owned())
                                .variant(ButtonVariant::Default)
                                .with_size(ButtonSize::Lg)
                                .rounded_full()
                                .font_weight(FontWeight::MEDIUM)
                                .cursor_pointer()
                                .on_press(cx, |this, _, cx| this.start_pairing(cx)),
                        ),
                    )
                } else {
                    left.child(notice("mobile-off", rust_i18n::t!("mobile.off").into_owned(), theme)).child(
                        div().child(
                            Button::new("mobile-turn-on", rust_i18n::t!("mobile.turn_on").into_owned())
                                .variant(ButtonVariant::Default)
                                .with_size(ButtonSize::Lg)
                                .rounded_full()
                                .font_weight(FontWeight::MEDIUM)
                                .cursor_pointer()
                                .on_press(cx, |this, _, cx| this.turn_on_remote_access(cx)),
                        ),
                    )
                };
                right.child(placeholder(String::new(), theme))
            }
            Pairing::Paired { .. } => unreachable!("handled above"),
        };
        columns(left, right).child(footer(back, None))
    }

    /// 电脑名：输入框，空着时浮着系统的电脑名，下面说手机上会看到什么。
    fn render_machine_name(&self, theme: &Theme, cx: &App) -> Stateful<Div> {
        let saved = cx.global::<AppConfig>().0.remote_access_name.clone();
        let shown = saved.map_or_else(|| self.system_name.clone(), SharedString::from);
        labeled(
            "mobile-name",
            rust_i18n::t!("mobile.name_label").into_owned(),
            // 和下面网络的下拉框一个样子，见 `input_box`。
            Input::new(&self.name)
                .h(scaled(36.))
                .px(px(12.))
                .rounded(px(8.))
                .bg(theme.mix(0.04))
                .text_size(FontSize::Medium),
            rust_i18n::t!("mobile.name_hint", name = shown).into_owned(),
            theme,
        )
    }

    /// 网络：展开、收起的下拉框，旁边是重读网卡的按钮；展开时选项列在下面。
    fn render_network(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let label = |network: Option<IpAddr>| -> SharedString {
            match network.and_then(|addr| self.interfaces.iter().find(|(_, seen)| *seen == addr)) {
                Some((name, addr)) => network_label(name, *addr).into(),
                None => rust_i18n::t!("mobile.network_auto").into_owned().into(),
            }
        };
        let select = SelectTrigger::new(
            "mobile-network",
            rust_i18n::t!("mobile.network_label").into_owned(),
            label(self.network),
        )
        .aria_expanded(self.network_open)
        .h(scaled(36.))
        .px(px(12.))
        .rounded(px(8.))
        .bg(theme.mix(0.04))
        .border_1()
        .border_color(theme.foreground.opacity(0.15))
        .text_size(FontSize::Medium)
        .flex_1()
        .hover(|select| select.bg(theme.mix(0.08)))
        .on_press(cx, |this, _, cx| {
            this.network_open = !this.network_open;
            cx.notify();
        });
        let refresh = Button::new("mobile-network-refresh", rust_i18n::t!("mobile.refresh_networks"))
            .variant(ButtonVariant::Outline)
            .icon(REFRESH_ICON)
            .with_size(ButtonSize::Icon)
            .size(px(36.))
            .rounded(px(8.))
            .cursor_pointer()
            .on_press(cx, |this, _, cx| {
                this.refresh_interfaces();
                this.restart_pairing(cx);
                cx.notify();
            });
        let options = self.network_open.then(|| {
            let choices = std::iter::once(None).chain(self.interfaces.iter().map(|(_, addr)| Some(*addr)));
            div()
                .id("mobile-network-options")
                .role(Role::ListBox)
                .aria_label(rust_i18n::t!("mobile.network_label").into_owned())
                .flex()
                .flex_col()
                .p(px(4.))
                .rounded(px(8.))
                .bg(theme.mix(0.04))
                .border_1()
                .border_color(theme.foreground.opacity(0.12))
                .children(choices.map(|network| {
                    let chosen = network == self.network;
                    let text = label(network);
                    // id 按地址取，不按行号：重新列网卡后顺序、个数变了，VoiceOver 的焦点不跳。
                    let id = match network {
                        Some(addr) => SharedString::from(format!("mobile-network-option-{addr}")),
                        None => SharedString::from("mobile-network-option-auto"),
                    };
                    div()
                        .id(id)
                        .role(Role::ListBoxOption)
                        .aria_label(text.clone())
                        .aria_selected(chosen)
                        .px(px(10.))
                        .py(px(7.))
                        .rounded(px(6.))
                        .text_size(FontSize::Medium)
                        .cursor_pointer()
                        .when(chosen, |option| option.bg(theme.mix(0.08)))
                        .hover(|option| option.bg(theme.mix(0.08)))
                        .child(text)
                        .on_press(cx, move |this, _, cx| this.pick_network(network, cx))
                }))
        });
        labeled(
            "mobile-network-field",
            rust_i18n::t!("mobile.network_label").into_owned(),
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().flex().items_center().gap(px(8.)).child(select).child(refresh))
                .children(options),
            rust_i18n::t!("mobile.network_hint").into_owned(),
            theme,
        )
    }
}

/// 配对过的一台设备：图标，名字，连着没有和哪天配的，右边撤销（先问一句，见 `revoke_device`）。
fn device_row(device: &Paired, theme: &Theme, cx: &mut Context<MobilePage>) -> Stateful<Div> {
    let id = device.id;
    let (year, month, day) = local_date(device.paired_at);
    let date = rust_i18n::t!("mobile.date", y = year, m = month, d = day);
    let paired_on = rust_i18n::t!("mobile.paired_on", date = date).into_owned();
    let summary =
        if device.live { format!("{} · {paired_on}", rust_i18n::t!("machine.live")) } else { paired_on.clone() };
    let status =
        div().flex().items_center().gap(px(6.)).text_size(FontSize::Body).text_color(theme.foreground.opacity(0.5));
    let status = if device.live {
        status
            .child(div().size(px(7.)).rounded_full().bg(gpui::green()))
            .child(rust_i18n::t!("machine.live").into_owned())
            .child("·")
            .child(paired_on)
    } else {
        status.child(paired_on)
    };
    div()
        .id(SharedString::from(format!("mobile-device-{id}")))
        .role(Role::ListItem)
        .aria_label(device.name.clone())
        .aria_description(summary)
        .flex()
        .items_center()
        .gap(px(14.))
        .p(px(14.))
        .rounded(px(12.))
        .bg(theme.mix(0.04))
        .border_1()
        .border_color(theme.foreground.opacity(0.1))
        .child(
            div()
                .flex_none()
                .size(px(36.))
                .rounded(px(8.))
                .bg(theme.mix(0.08))
                .flex()
                .items_center()
                .justify_center()
                .child(svg().path(PHONE_ICON).size(px(16.)).text_color(theme.foreground.opacity(0.8))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(
                    div()
                        .text_size(FontSize::Large)
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate()
                        .child(device.name.clone()),
                )
                .child(status),
        )
        .child(
            Button::new(SharedString::from(format!("mobile-device-revoke-{id}")), rust_i18n::t!("machine.revoke"))
                .icon(TRASH_ICON)
                .icon_size(15.)
                .with_size(ButtonSize::Icon)
                .size(px(30.))
                .rounded(px(6.))
                .cursor_pointer()
                .on_press(cx, move |this, window, cx| this.revoke_device(id, window, cx)),
        )
}

/// Unix 秒在本地时区里是哪年哪月哪天。
fn local_date(secs: u64) -> (i32, i32, i32) {
    runode_todo::LocalTime::from_unix(secs).map_or((1970, 1, 1), |at| (at.year, at.month.into(), at.day.into()))
}

/// 下拉框里一个网络的名字：地址加网卡名；Tailscale 分到的地址写 Tailscale，比 `utun3` 好认。
fn network_label(interface: &str, addr: IpAddr) -> String {
    let tailscale = match addr {
        // 100.64.0.0/10，Tailscale 分给各台机器的 IPv4。
        IpAddr::V4(v4) => v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64,
        // fd7a:115c:a1e0::/48，Tailscale 的 IPv6。
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    };
    format!("{addr} ({})", if tailscale { "Tailscale" } else { interface })
}

/// 左边说明、右边二维码，下面一条页脚。
fn columns(left: Div, right: Div) -> Div {
    div().flex().flex_col().gap(px(36.)).child(div().flex().items_start().gap(px(48.)).child(left).child(right))
}

fn footer(back: Button, next: Option<Button>) -> Div {
    div().flex().items_center().justify_between().child(back).children(next)
}

/// 一页的大标题；每页只有一个，id 写死。
fn headline(text: String, size: f32) -> Stateful<Div> {
    div()
        .id("mobile-headline")
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
    text_label("mobile-lead", text, theme.foreground.opacity(0.6)).text_size(FontSize::Title).line_height(scaled(24.))
}

/// 一句报给辅助工具的文字：GPUI 不报文字本身，名字要另外写。
fn text_label(id: &'static str, text: String, color: Hsla) -> Stateful<Div> {
    div().id(id).role(Role::Label).aria_label(text.clone()).text_color(color).child(text)
}

/// 二维码的外框，作为一张图报给辅助工具，`content` 是码里的链接。
fn qr_image(id: &'static str, label: String, content: String) -> Stateful<Div> {
    div().id(id).flex_none().role(Role::Image).aria_label(label).aria_description(content)
}

/// 「第 n 步（共 2 步）」，前面一个圈着数字的小圆。只有装 App 和配对两步都在时才画。
fn step_line(n: u32, theme: &Theme) -> Stateful<Div> {
    let text = rust_i18n::t!("mobile.step", n = n, total = 2).into_owned();
    div()
        .id("mobile-step")
        .role(Role::Label)
        .aria_label(text.clone())
        .flex()
        .items_center()
        .gap(px(10.))
        .text_size(FontSize::Medium)
        .text_color(theme.foreground.opacity(0.6))
        .child(
            div()
                .size(px(26.))
                .rounded_full()
                .border_1()
                .border_color(theme.foreground.opacity(0.15))
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.foreground)
                .child(n.to_string()),
        )
        .child(text)
}

/// 一项设置：小标题，内容，下面一行说明。小标题不报给辅助工具，内容的控件自己以它为名；`id` 让
/// 两项的说明分得开。
fn labeled(id: &'static str, label: String, content: impl IntoElement, hint: String, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(div().text_size(FontSize::Body).font_weight(FontWeight::SEMIBOLD).child(label))
        .child(content)
        .child(
            text_label("hint", hint, theme.foreground.opacity(0.5)).text_size(FontSize::Body).line_height(scaled(18.)),
        )
}

/// 一块浅底的提示文字。
fn notice(id: &'static str, text: String, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Label)
        .aria_label(text.clone())
        .p(px(14.))
        .rounded(px(10.))
        .bg(theme.mix(0.04))
        .border_1()
        .border_color(theme.foreground.opacity(0.1))
        .text_size(FontSize::Medium)
        .line_height(scaled(20.))
        .text_color(theme.foreground.opacity(0.75))
        .child(text)
}

/// 还没有二维码时右边那块占位，和二维码差不多大。
fn placeholder(text: String, theme: &Theme) -> Stateful<Div> {
    div()
        .id("mobile-qr-placeholder")
        .when(!text.is_empty(), |placeholder| placeholder.role(Role::Label).aria_label(text.clone()))
        .size(px(240.))
        .rounded(px(12.))
        .bg(theme.mix(0.04))
        .border_1()
        .border_color(theme.foreground.opacity(0.08))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .p(px(20.))
        .text_size(FontSize::Body)
        .text_color(theme.foreground.opacity(0.5))
        .child(svg().path(PHONE_ICON).size(px(28.)).text_color(theme.foreground.opacity(0.25)))
        .child(text)
}

/// 按钮报给辅助工具的名字：去掉画在文字两边的箭头。
fn spoken(label: &str) -> SharedString {
    label.trim_matches(|c: char| c == '←' || c == '→' || c.is_whitespace()).to_owned().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只有 Unix 的 `tm` 带着时差（`tm_gmtoff`）。
    #[cfg(unix)]
    #[test]
    fn the_pairing_day_is_read_in_local_time() {
        // 2026-10-08 12:00 UTC：本地比 UTC 早不到 12 小时（UTC-12 到 UTC+11:59）还是这一天，
        // 早 12 小时及以上（UTC+12 到 UTC+14）已经是第二天。时区按跑测试的机器，不改进程的 TZ。
        let time: libc::time_t = 1_791_460_800;
        // SAFETY: `tm` 是本地变量，全零是合法的初值；localtime_r 只读 `time`、只写 `tm`。
        let offset = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&time, &mut tm);
            tm.tm_gmtoff
        };
        let day = if offset < 12 * 3600 { 8 } else { 9 };
        assert_eq!(local_date(1_791_460_800), (2026, 10, day));
    }

    #[test]
    fn tailscale_addresses_are_named_tailscale() {
        assert_eq!(network_label("en0", "10.0.0.10".parse().unwrap()), "10.0.0.10 (en0)");
        assert_eq!(network_label("utun3", "100.101.102.103".parse().unwrap()), "100.101.102.103 (Tailscale)");
        assert_eq!(network_label("en0", "100.128.0.1".parse().unwrap()), "100.128.0.1 (en0)");
        assert_eq!(network_label("utun3", "fd7a:115c:a1e0::1".parse().unwrap()), "fd7a:115c:a1e0::1 (Tailscale)");
        assert_eq!(network_label("en0", "2001:db8::1".parse().unwrap()), "2001:db8::1 (en0)");
    }
}
