//! 侧栏顶上的「手机端」：窗口主区域换成一页引导，先介绍手机端能做什么，再装 App（有安装链接时），
//! 最后扫码配对这台 Mac（`remote_access::pairing`）。已经配过手机时第一页换成配对过的设备（`machine`
//! 读的那份），连着的标出来，能撤销，也能再配一台。远程访问没开时在这一页就能打开。切到任何一个
//! 标签或 workspace（`activate`）、按 Esc 就收起，还在等的口令随之丢掉。
//!
//! 配对这一步能改手机上显示的电脑名（配置项 `remote-access-name`，改了监听方换上新名字后重新生成
//! 二维码），也能挑手机先走哪个网络：挑中的地址排在配对 URI 最前面，手机先试它，连不上再试别的。

use std::{net::IpAddr, rc::Rc};

use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Div, Entity, FocusHandle, Focusable, FontWeight, Hsla,
    KeyDownEvent, MouseButton, SharedString, Stateful, Subscription, Window, div, prelude::*, px, svg,
};
use runode_remote_access::{host_name, local_interfaces};
use runode_shared_types::color::Rgb;

use super::{
    CARD_GAP, TITLEBAR_HEIGHT, WindowView, card, cards, drag_window, frame_color,
    machine::{Paired, RevokeDevice, devices},
};
use crate::{
    assets::{CHEVRON_DOWN_ICON, PHONE_ICON, REFRESH_ICON, TRASH_ICON},
    config::AppConfig,
    remote_access::pairing::{BACKGROUND_KEY, Pairing, Qr, qr_code},
    ui::{
        hsla,
        text_field::{TextField, TextFieldEvent},
    },
};

/// 手机上显示的电脑名的配置项，见模块说明。
const NAME_KEY: &str = "remote-access-name";

/// iPhone 上装 App 的链接；还没有公开的安装链接时为 `None`，引导里跳过装 App 这一步。
const IOS_APP_URL: Option<&str> = None;
/// 引导页二维码一个模块的边长，像素。
const MODULE_SIZE: f32 = 5.;
/// 二维码那一栏的宽度：版本 10 上下的二维码加留白放得下。
const QR_COLUMN_WIDTH: f32 = 300.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Intro,
    Install,
    Pair,
}

pub(super) struct MobilePage {
    step: Step,
    pairing: Pairing,
    pub(super) focus: FocusHandle,
    /// 装 App 的二维码，有安装链接时才有。
    install_qr: Option<Rc<Qr>>,
    /// 电脑名的输入框，以及它的回车、Esc 和失焦；空着时用系统的电脑名 `system_name`。
    name: Entity<TextField>,
    system_name: SharedString,
    _name_events: [Subscription; 2],
    /// 这台机器的网卡和地址，进配对这一步、点刷新时重读。
    interfaces: Vec<(String, IpAddr)>,
    /// 挑的网络，它的地址排在配对 URI 最前面；`None` 时按系统的顺序。
    network: Option<IpAddr>,
    /// 网络的下拉列表展开着。
    network_open: bool,
}

impl WindowView {
    /// 打开手机端引导页；已经开着时把焦点给它。
    pub(super) fn show_mobile(&mut self, _: &super::ShowMobile, window: &mut Window, cx: &mut Context<Self>) {
        if self.mobile.is_none() {
            let system_name = host_name();
            let saved = cx.global::<AppConfig>().0.remote_access_name.clone().unwrap_or_default();
            let name = cx.new(|cx| TextField::new(saved, cx).with_placeholder(system_name.clone()));
            let events =
                cx.subscribe_in(&name, window, |this, field, event: &TextFieldEvent, window, cx| match event {
                    TextFieldEvent::Next => this.focus_mobile(window, cx),
                    // 不改了：换回配置里的名字。
                    TextFieldEvent::Dismiss => {
                        let saved = cx.global::<AppConfig>().0.remote_access_name.clone().unwrap_or_default();
                        field.update(cx, |field, cx| field.set_query(saved, cx));
                        this.focus_mobile(window, cx);
                    }
                    TextFieldEvent::Changed(_) | TextFieldEvent::Previous => {}
                });
            // 回车、点别处都是离开输入框，在这里写回。
            let blurred = cx.on_focus_out(&name.focus_handle(cx), window, |this, _, _, cx| this.save_machine_name(cx));
            self.mobile = Some(MobilePage {
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
            });
        }
        self.focus_mobile(window, cx);
    }

    fn focus_mobile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(page) = &self.mobile {
            window.focus(&page.focus, cx);
        }
        cx.notify();
    }

    /// 输入框里的电脑名和配置里的不一样时写回配置；监听方换上新名字后重新生成二维码。
    fn save_machine_name(&mut self, cx: &mut Context<Self>) {
        let Some(page) = &self.mobile else { return };
        let name = page.name.read(cx).query().trim().to_owned();
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
            Ok(_) => self.restart_mobile_pairing(cx),
            Err(err) => {
                if let Some(page) = &mut self.mobile {
                    page.pairing = Pairing::Failed(err);
                }
            }
        }
        cx.notify();
    }

    /// 二维码已经在或者在等的时候按现在的名字和网络重新生成；还没开始（远程访问没开）时不动。
    fn restart_mobile_pairing(&mut self, cx: &mut Context<Self>) {
        let Some(page) = &self.mobile else { return };
        if page.step == Step::Pair && matches!(page.pairing, Pairing::Starting { .. } | Pairing::Waiting(_)) {
            self.start_mobile_pairing(cx);
        }
    }

    /// 重读网卡；挑的那个地址没了就回到按系统的顺序。
    fn refresh_interfaces(&mut self) {
        if let Some(page) = &mut self.mobile {
            page.interfaces = local_interfaces();
            if page.network.is_some_and(|addr| !page.interfaces.iter().any(|(_, seen)| *seen == addr)) {
                page.network = None;
            }
        }
    }

    fn pick_network(&mut self, network: Option<IpAddr>, cx: &mut Context<Self>) {
        if let Some(page) = &mut self.mobile {
            page.network = network;
            page.network_open = false;
        }
        self.restart_mobile_pairing(cx);
        cx.notify();
    }

    fn close_mobile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mobile.take().is_some() {
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// 翻到 `step`。到配对这一步时远程访问开着就马上生成口令，离开时丢掉口令。
    fn go_mobile(&mut self, step: Step, cx: &mut Context<Self>) {
        let Some(page) = &mut self.mobile else { return };
        page.step = step;
        page.pairing = Pairing::Idle;
        page.network_open = false;
        self.refresh_interfaces();
        if step == Step::Pair && cx.global::<AppConfig>().0.remote_access {
            self.start_mobile_pairing(cx);
        }
        cx.notify();
    }

    fn start_mobile_pairing(&mut self, cx: &mut Context<Self>) {
        if let Some(page) = &mut self.mobile {
            let extra = page.network.into_iter().collect();
            page.pairing.start(|this: &mut Self| this.mobile.as_mut().map(|page| &mut page.pairing), extra, cx);
        }
    }

    /// 在配置里打开远程访问，等监听方开好就生成口令。
    fn turn_on_remote_access(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = crate::config::set("remote-access", "true", cx) {
            if let Some(page) = &mut self.mobile {
                page.pairing =
                    Pairing::Failed(rust_i18n::t!("settings.write_failed", err = err.to_string()).into_owned());
            }
            cx.notify();
            return;
        }
        self.start_mobile_pairing(cx);
    }

    /// 侧栏顶上的入口，引导页开着时高亮。
    pub(super) fn render_mobile_entry(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let active = self.mobile.is_some();
        let active_bg = hsla(bg.mix(fg, 0.10));
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let fg = hsla(fg);
        div()
            .id("mobile-entry")
            .flex_none()
            .h(px(30.))
            .mx(px(6.))
            .mb(px(4.))
            .px(px(8.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .map(|row| {
                if active {
                    row.bg(active_bg).text_color(fg)
                } else {
                    row.text_color(fg.opacity(0.7)).hover(|row| row.bg(hover_bg).text_color(fg))
                }
            })
            .child(svg().flex_none().path(PHONE_ICON).size(px(14.)).text_color(fg.opacity(if active {
                0.9
            } else {
                0.6
            })))
            .child(div().min_w_0().truncate().child(rust_i18n::t!("mobile.entry").into_owned()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.show_mobile(&super::ShowMobile, window, cx);
                }),
            )
    }

    /// 引导页开着时窗口的内容：侧栏（收着时没有），右边整块是引导页，顶上留一条拖动窗口、放红绿灯。
    pub(super) fn render_mobile_body(
        &mut self,
        fg: Rgb,
        bg: Rgb,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let fullscreen = window.is_fullscreen();
        let cards = cards(cx);
        let sidebar = self.sidebar_visible().then(|| self.render_sidebar(fg, bg, window, cx));
        let handle = sidebar.is_some().then(|| self.render_sidebar_handle(cx));
        let toggle =
            (!fullscreen).then(|| self.render_sidebar_toggle(fg, if cards { frame_color(fg, bg) } else { bg }, cx));
        let page = self.render_mobile_page(fg, bg, cx);
        let main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(div().flex_none().h(px(TITLEBAR_HEIGHT)).on_mouse_down(MouseButton::Left, drag_window))
            .child(if cards {
                card(hsla(fg), hsla(bg))
                    .flex_1()
                    .min_h_0()
                    .mr(px(CARD_GAP))
                    .mb(px(CARD_GAP))
                    .overflow_hidden()
                    .child(page)
            } else {
                div().flex_1().min_h_0().child(page)
            });
        let mut body: Vec<AnyElement> = Vec::new();
        body.extend(sidebar.map(IntoElement::into_any_element));
        if cards {
            body.push(div().flex_none().w(px(CARD_GAP)).into_any_element());
        }
        body.push(main.into_any_element());
        body.extend(handle.map(IntoElement::into_any_element));
        body.extend(toggle.map(IntoElement::into_any_element));
        body
    }

    fn render_mobile_page(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let page = self.mobile.as_ref().expect("the mobile page is open");
        let colors = Colors::new(fg, bg);
        let content = match page.step {
            Step::Intro => self.render_intro(colors, cx),
            Step::Install => self.render_install(page, colors, cx),
            Step::Pair => self.render_pair(page, colors, cx),
        };
        div()
            .id("mobile-page")
            .track_focus(&page.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    this.close_mobile(window, cx);
                }
            }))
            .size_full()
            .overflow_y_scroll()
            .text_color(colors.fg)
            .flex()
            .flex_col()
            .items_center()
            .px(px(48.))
            .py(px(40.))
            .child(div().my_auto().w_full().max_w(px(820.)).child(content))
    }

    fn render_intro(&self, colors: Colors, cx: &mut Context<Self>) -> Div {
        let first = if IOS_APP_URL.is_some() { Step::Install } else { Step::Pair };
        let eyebrow = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(12.))
            .text_color(colors.fg.opacity(0.5))
            .child(svg().path(PHONE_ICON).size(px(14.)).text_color(colors.fg.opacity(0.5)))
            .child(rust_i18n::t!("mobile.eyebrow").into_owned());
        let paired = devices(cx).to_vec();
        if !paired.is_empty() {
            let rows: Vec<_> =
                paired.iter().enumerate().map(|(ix, device)| device_row(ix, device, colors, cx)).collect();
            let another = pill("mobile-pair-another", rust_i18n::t!("mobile.pair_another").into_owned(), false, colors)
                .gap(px(8.))
                .child(svg().path(PHONE_ICON).size(px(14.)).text_color(colors.fg.opacity(0.8)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go_mobile(first, cx)));
            return div()
                .max_w(px(640.))
                .flex()
                .flex_col()
                .gap(px(18.))
                .child(eyebrow)
                .child(headline(rust_i18n::t!("mobile.paired_intro_title").into_owned(), 40.))
                .child(lead(rust_i18n::t!("mobile.paired_intro_body").into_owned(), colors))
                .child(div().pt(px(12.)).flex().flex_col().gap(px(8.)).children(rows))
                .child(div().pt(px(24.)).flex().child(another));
        }
        div()
            .max_w(px(560.))
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(eyebrow)
            .child(headline(rust_i18n::t!("mobile.intro_title").into_owned(), 40.))
            .child(lead(rust_i18n::t!("mobile.intro_body").into_owned(), colors))
            .child(
                div().pt(px(12.)).child(
                    pill("mobile-start", format!("{}  →", rust_i18n::t!("mobile.start")), true, colors)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go_mobile(first, cx))),
                ),
            )
    }

    fn render_install(&self, page: &MobilePage, colors: Colors, cx: &mut Context<Self>) -> Div {
        let url = IOS_APP_URL.unwrap_or_default();
        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(step_line(1, colors))
            .child(headline(rust_i18n::t!("mobile.install_title").into_owned(), 32.))
            .child(lead(rust_i18n::t!("mobile.install_body").into_owned(), colors))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .child(
                        pill("mobile-open-app", rust_i18n::t!("mobile.open_link").into_owned(), false, colors)
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.open_url(url))),
                    )
                    .child(link("mobile-copy-app", rust_i18n::t!("mobile.copy_link").into_owned(), colors).on_click(
                        cx.listener(move |_, _: &ClickEvent, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(url.to_owned()))
                        }),
                    )),
            );
        let right = div()
            .flex_none()
            .w(px(QR_COLUMN_WIDTH))
            .flex()
            .justify_center()
            .children(page.install_qr.clone().map(|qr| qr_code(qr, MODULE_SIZE)));
        let footer = footer(
            link("mobile-back", format!("←  {}", rust_i18n::t!("mobile.back")), colors)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.go_mobile(Step::Intro, cx))),
            Some(
                pill("mobile-next", format!("{}  →", rust_i18n::t!("mobile.next")), true, colors)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.go_mobile(Step::Pair, cx))),
            ),
        );
        columns(left, right).child(footer)
    }

    fn render_pair(&self, page: &MobilePage, colors: Colors, cx: &mut Context<Self>) -> Div {
        let config = &cx.global::<AppConfig>().0;
        let (remote_on, background_on) = (config.remote_access, config.terminal_host);
        let back_to = if IOS_APP_URL.is_some() { Step::Install } else { Step::Intro };
        let back = link("mobile-back", format!("←  {}", rust_i18n::t!("mobile.back")), colors)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go_mobile(back_to, cx)));
        let mut left = div().flex_1().min_w_0().flex().flex_col().gap(px(18.));
        if IOS_APP_URL.is_some() {
            left = left.child(step_line(2, colors));
        }

        if let Pairing::Paired { name } = &page.pairing {
            let actions = div()
                .flex()
                .items_center()
                .gap(px(16.))
                .child(
                    pill("mobile-done", rust_i18n::t!("mobile.done").into_owned(), true, colors)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_mobile(window, cx))),
                )
                .child(
                    link("mobile-again", rust_i18n::t!("mobile.again").into_owned(), colors)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_mobile_pairing(cx))),
                );
            // 宿主跑在 app 里时退出 app 远程访问跟着停，和设置里一样提醒一句。
            let background = (!background_on).then(|| {
                notice(rust_i18n::t!("settings.pairing.background").into_owned(), colors).child(
                    div().pt(px(10.)).child(
                        pill("mobile-keep", rust_i18n::t!("mobile.keep").into_owned(), false, colors).on_click(
                            // 配置重载后窗口跟着重画，这一块就收起来了。
                            cx.listener(|_, _: &ClickEvent, _, cx| {
                                if let Err(err) = crate::config::set(BACKGROUND_KEY, "true", cx) {
                                    tracing::warn!("could not turn on {BACKGROUND_KEY}: {err:#}");
                                }
                            }),
                        ),
                    ),
                )
            });
            let left = left
                .child(headline(rust_i18n::t!("mobile.paired_title").into_owned(), 32.))
                .child(lead(rust_i18n::t!("mobile.paired_body", name = name).into_owned(), colors))
                .children(background)
                .child(actions);
            return div().max_w(px(560.)).child(left);
        }

        left = left
            .child(headline(rust_i18n::t!("mobile.pair_title").into_owned(), 32.))
            .child(lead(rust_i18n::t!("mobile.pair_body").into_owned(), colors));
        if let Pairing::Failed(err) = &page.pairing {
            left = left.child(notice(err.clone(), colors));
        }
        if remote_on {
            left = left.child(self.render_machine_name(page, colors, cx)).child(self.render_network(page, colors, cx));
        }
        let right = div().flex_none().w(px(QR_COLUMN_WIDTH)).flex().flex_col().items_center().gap(px(12.));
        let right = match &page.pairing {
            Pairing::Waiting(waiting) => {
                let uri = waiting.uri();
                left = left.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(
                            div()
                                .text_color(colors.fg.opacity(0.5))
                                .child(rust_i18n::t!("mobile.cant_scan").into_owned()),
                        )
                        .child(
                            link("mobile-copy-code", rust_i18n::t!("mobile.copy_code").into_owned(), colors).on_click(
                                cx.listener(move |_, _: &ClickEvent, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(uri.to_string()))
                                }),
                            ),
                        ),
                );
                right.child(qr_code(waiting.qr(), MODULE_SIZE)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .text_size(px(12.))
                        .child(
                            div()
                                .text_color(colors.fg.opacity(0.5))
                                .child(rust_i18n::t!("mobile.expires", time = waiting.remaining()).into_owned()),
                        )
                        .child(
                            link("mobile-regenerate", rust_i18n::t!("mobile.regenerate").into_owned(), colors)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_mobile_pairing(cx))),
                        ),
                )
            }
            Pairing::Starting { .. } => {
                right.child(placeholder(rust_i18n::t!("settings.pairing.starting").into_owned(), colors))
            }
            Pairing::Idle | Pairing::Failed(_) => {
                left = if remote_on {
                    let label =
                        if matches!(page.pairing, Pairing::Failed(_)) { "mobile.retry" } else { "mobile.generate" };
                    left.child(
                        div().child(
                            pill("mobile-generate", rust_i18n::t!(label).into_owned(), true, colors)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_mobile_pairing(cx))),
                        ),
                    )
                } else {
                    left.child(notice(rust_i18n::t!("mobile.off").into_owned(), colors)).child(
                        div().child(
                            pill("mobile-turn-on", rust_i18n::t!("mobile.turn_on").into_owned(), true, colors)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.turn_on_remote_access(cx))),
                        ),
                    )
                };
                right.child(placeholder(String::new(), colors))
            }
            Pairing::Paired { .. } => unreachable!("handled above"),
        };
        columns(left, right).child(footer(back, None))
    }

    /// 电脑名：输入框，空着时浮着系统的电脑名，下面说手机上会看到什么。
    fn render_machine_name(&self, page: &MobilePage, colors: Colors, cx: &App) -> Div {
        let saved = cx.global::<AppConfig>().0.remote_access_name.clone();
        let shown = saved.map_or_else(|| page.system_name.clone(), SharedString::from);
        labeled(
            rust_i18n::t!("mobile.name_label").into_owned(),
            input_box(colors).child(page.name.clone()),
            rust_i18n::t!("mobile.name_hint", name = shown).into_owned(),
            colors,
        )
    }

    /// 网络：展开、收起的下拉框，旁边是重读网卡的按钮；展开时选项列在下面。
    fn render_network(&self, page: &MobilePage, colors: Colors, cx: &mut Context<Self>) -> Div {
        let label = |network: Option<IpAddr>| -> SharedString {
            match network.and_then(|addr| page.interfaces.iter().find(|(_, seen)| *seen == addr)) {
                Some((name, addr)) => network_label(name, *addr).into(),
                None => rust_i18n::t!("mobile.network_auto").into_owned().into(),
            }
        };
        let select = input_box(colors)
            .id("mobile-network")
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .justify_between()
            .cursor_pointer()
            .hover(|select| select.bg(colors.hover))
            .child(div().min_w_0().truncate().child(label(page.network)))
            .child(svg().flex_none().path(CHEVRON_DOWN_ICON).size(px(14.)).text_color(colors.fg.opacity(0.5)))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                if let Some(page) = &mut this.mobile {
                    page.network_open = !page.network_open;
                }
                cx.notify();
            }));
        let refresh = div()
            .id("mobile-network-refresh")
            .flex_none()
            .size(px(36.))
            .rounded(px(8.))
            .border_1()
            .border_color(colors.fg.opacity(0.15))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|button| button.bg(colors.hover))
            .child(svg().path(REFRESH_ICON).size(px(14.)).text_color(colors.fg.opacity(0.7)))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.refresh_interfaces();
                this.restart_mobile_pairing(cx);
                cx.notify();
            }));
        let options = page.network_open.then(|| {
            let choices = std::iter::once(None).chain(page.interfaces.iter().map(|(_, addr)| Some(*addr)));
            div()
                .flex()
                .flex_col()
                .p(px(4.))
                .rounded(px(8.))
                .bg(colors.panel)
                .border_1()
                .border_color(colors.fg.opacity(0.12))
                .children(choices.enumerate().map(|(ix, network)| {
                    let chosen = network == page.network;
                    div()
                        .id(("mobile-network-option", ix))
                        .px(px(10.))
                        .py(px(7.))
                        .rounded(px(6.))
                        .text_size(px(13.))
                        .cursor_pointer()
                        .when(chosen, |option| option.bg(colors.hover))
                        .hover(|option| option.bg(colors.hover))
                        .child(label(network))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.pick_network(network, cx)))
                }))
        });
        labeled(
            rust_i18n::t!("mobile.network_label").into_owned(),
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(div().flex().items_center().gap(px(8.)).child(select).child(refresh))
                .children(options),
            rust_i18n::t!("mobile.network_hint").into_owned(),
            colors,
        )
    }
}

/// 配对过的一台设备：图标，名字，连着没有和哪天配的，右边撤销（先问一句，见 `revoke_device`）。
fn device_row(ix: usize, device: &Paired, colors: Colors, cx: &mut Context<WindowView>) -> Stateful<Div> {
    let id = device.id;
    let (year, month, day) = local_date(device.paired_at);
    let date = rust_i18n::t!("mobile.date", y = year, m = month, d = day);
    let paired_on = rust_i18n::t!("mobile.paired_on", date = date).into_owned();
    let status = div().flex().items_center().gap(px(6.)).text_size(px(12.)).text_color(colors.fg.opacity(0.5));
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
        .id(("mobile-device", ix))
        .flex()
        .items_center()
        .gap(px(14.))
        .p(px(14.))
        .rounded(px(12.))
        .bg(colors.panel)
        .border_1()
        .border_color(colors.fg.opacity(0.1))
        .child(
            div()
                .flex_none()
                .size(px(36.))
                .rounded(px(8.))
                .bg(colors.hover)
                .flex()
                .items_center()
                .justify_center()
                .child(svg().path(PHONE_ICON).size(px(16.)).text_color(colors.fg.opacity(0.8))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).truncate().child(device.name.clone()))
                .child(status),
        )
        .child(
            div()
                .id(("mobile-device-revoke", ix))
                .flex_none()
                .size(px(30.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|button| button.bg(colors.hover))
                .child(svg().path(TRASH_ICON).size(px(15.)).text_color(colors.fg.opacity(0.6)))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.revoke_device(&RevokeDevice(id), window, cx);
                })),
        )
}

/// Unix 秒在本地时区里是哪年哪月哪天。
fn local_date(secs: u64) -> (i32, i32, i32) {
    let time = libc::time_t::try_from(secs).unwrap_or(libc::time_t::MAX);
    // SAFETY: `tm` 是本地变量，全零是合法的初值；localtime_r 只读 `time`、只写 `tm`。
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&time, &mut tm);
        tm
    };
    (tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
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

/// 引导页的几种颜色，由终端的前景、背景色调出来。
#[derive(Clone, Copy)]
struct Colors {
    fg: Hsla,
    bg: Hsla,
    /// 浅一档的面，提示块和二维码占位用。
    panel: Hsla,
    hover: Hsla,
}

impl Colors {
    fn new(fg: Rgb, bg: Rgb) -> Self {
        Self { fg: hsla(fg), bg: hsla(bg), panel: hsla(bg.mix(fg, 0.04)), hover: hsla(bg.mix(fg, 0.08)) }
    }
}

/// 左边说明、右边二维码，下面一条页脚。
fn columns(left: Div, right: Div) -> Div {
    div().flex().flex_col().gap(px(36.)).child(div().flex().items_start().gap(px(48.)).child(left).child(right))
}

fn footer(back: Stateful<Div>, next: Option<Stateful<Div>>) -> Div {
    div().flex().items_center().justify_between().child(back).children(next)
}

fn headline(text: String, size: f32) -> Div {
    div().text_size(px(size)).line_height(px(size * 1.2)).font_weight(FontWeight::BOLD).child(text)
}

fn lead(text: String, colors: Colors) -> Div {
    div().text_size(px(15.)).line_height(px(24.)).text_color(colors.fg.opacity(0.6)).child(text)
}

/// 「第 n 步（共 2 步）」，前面一个圈着数字的小圆。只有装 App 和配对两步都在时才画。
fn step_line(n: u32, colors: Colors) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .text_size(px(13.))
        .text_color(colors.fg.opacity(0.6))
        .child(
            div()
                .size(px(26.))
                .rounded_full()
                .border_1()
                .border_color(colors.fg.opacity(0.15))
                .flex()
                .items_center()
                .justify_center()
                .text_color(colors.fg)
                .child(n.to_string()),
        )
        .child(rust_i18n::t!("mobile.step", n = n, total = 2).into_owned())
}

/// 一项设置：小标题，内容，下面一行说明。
fn labeled(label: String, content: impl IntoElement, hint: String, colors: Colors) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).child(label))
        .child(content)
        .child(div().text_size(px(12.)).line_height(px(18.)).text_color(colors.fg.opacity(0.5)).child(hint))
}

/// 输入框和下拉框的外框。
fn input_box(colors: Colors) -> Div {
    div()
        .h(px(36.))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(8.))
        .bg(colors.panel)
        .border_1()
        .border_color(colors.fg.opacity(0.15))
        .text_size(px(13.))
}

/// 一块浅底的提示文字。
fn notice(text: String, colors: Colors) -> Div {
    div()
        .p(px(14.))
        .rounded(px(10.))
        .bg(colors.panel)
        .border_1()
        .border_color(colors.fg.opacity(0.1))
        .text_size(px(13.))
        .line_height(px(20.))
        .text_color(colors.fg.opacity(0.75))
        .child(text)
}

/// 还没有二维码时右边那块占位，和二维码差不多大。
fn placeholder(text: String, colors: Colors) -> Div {
    div()
        .size(px(240.))
        .rounded(px(12.))
        .bg(colors.panel)
        .border_1()
        .border_color(colors.fg.opacity(0.08))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .p(px(20.))
        .text_size(px(12.))
        .text_color(colors.fg.opacity(0.5))
        .child(svg().path(PHONE_ICON).size(px(28.)).text_color(colors.fg.opacity(0.25)))
        .child(text)
}

/// 胶囊按钮；`primary` 的实心、字用背景色，其余的描边。
fn pill(id: &'static str, label: impl Into<SharedString>, primary: bool, colors: Colors) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(36.))
        .px(px(20.))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .map(|button| {
            if primary {
                button.bg(colors.fg.opacity(0.9)).text_color(colors.bg).hover(|button| button.bg(colors.fg))
            } else {
                button.border_1().border_color(colors.fg.opacity(0.18)).hover(|button| button.bg(colors.hover))
            }
        })
        .child(label.into())
}

/// 带下划线的文字按钮。
fn link(id: &'static str, label: impl Into<SharedString>, colors: Colors) -> Stateful<Div> {
    div()
        .id(id)
        .text_size(px(13.))
        .text_color(colors.fg.opacity(0.75))
        .underline()
        .cursor_pointer()
        .hover(|link| link.text_color(colors.fg))
        .child(label.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pairing_day_is_read_in_local_time() {
        // 2026-10-08 12:00 UTC，在 UTC-12 到 UTC+11 之间都还是这一天。
        assert_eq!(local_date(1_791_460_800), (2026, 10, 8));
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
