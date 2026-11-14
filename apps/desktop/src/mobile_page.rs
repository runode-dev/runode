//! 侧栏导航里「手机」那一页，别处叫它配对设备页。没配过任何设备时先介绍手机端能做什么，再装 App（有安装
//! 链接时），最后到配对这一步；配过设备后一进来就是配对这一步。配对这一步是一整页：上面一张配对卡（二维码
//! 和步骤，下面是这个码的状态：在等、过期、没开成、配好了；`remote_access::pairing`），下面两栏，左边配对
//! 过的设备（`devices`，连着的标出来，能撤销），右边远程访问的开关。按 Esc 就请窗口收起它
//! （`MobileEvent::Close`），还在等的口令随之丢掉。
//!
//! 远程访问那一栏能改手机上显示的电脑名（配置项 `remote-access-name`，改了监听方换上新名字后重新生成
//! 二维码），也能挑手机先走哪个网络：挑中的地址排在配对 URI 最前面，手机先试它，连不上再试别的。
//!
//! 这一页是单独的视图，窗口把它画在侧栏右边、缓存着；配置变了它自己重画，设备表变了重画所有窗口。等扫码时
//! 转着的盲文格画在缓存外面的一层（`spinner_overlay`），转一格不让这一页重画。

pub(crate) mod devices;

use std::{
    cell::Cell,
    net::{IpAddr, Ipv4Addr},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    Action, AnyElement, App, ClipboardItem, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    Hsla, KeyDownEvent, Render, Role, SharedString, Stateful, Subscription, Task, Window, canvas, div, prelude::*, px,
    svg,
};
use runode_protocol::remote::{DeviceId, DeviceKind};
use runode_remote::{host_name, local_interfaces};

use crate::{
    config::AppConfig,
    remote_access::pairing::{Pairing, Qr, qr_code},
    ui::icons::{CHECK_ICON, CLOSE_ICON, MONITOR_ICON, MORE_ICON, PHONE_ICON, REFRESH_ICON},
    ui::{
        a11y::{Disable as _, Press, PressDown as _, StaticText},
        button::{Button, ButtonSize, ButtonVariant},
        font_size::{FontSize, scaled},
        input::{Input, InputEvent, InputState},
        kbd::Kbd,
        menu::{Menu, MenuHost, MenuItem},
        select::SelectTrigger,
        spinner::{self, SpinnerLayer},
        status_mark::{MarkKind, status_mark},
        switch::Switch,
        theme::{ActiveTheme as _, Theme},
    },
};
use devices::Paired;

/// 手机上显示的电脑名的配置项。
const NAME_KEY: &str = "remote-access-name";
/// 远程访问在哪听的配置项，网络的下拉框选的就是它；所有地址（0.0.0.0）是「不在同一个网络也能连」。
const ADDRESS_KEY: &str = "remote-access-address";
/// agent 等回答时推到手机的配置项。
const PUSH_KEY: &str = "remote-access-push";

/// iPhone 上装 App 的链接；还没有公开的安装链接时为 `None`，引导里跳过装 App 这一步。
const IOS_APP_URL: Option<&str> = None;
/// 引导页二维码一个模块的边长，像素。
const MODULE_SIZE: f32 = 5.;
/// 二维码那一栏的宽度：版本 10 上下的二维码加留白放得下。
const QR_COLUMN_WIDTH: f32 = 300.;
/// 配对卡上二维码白底方块的边长，像素；码按它缩放。
const QR_SIDE: f32 = 150.;
/// 复制了配对链接后，按钮上的「已复制」留这么久。
const COPIED_FOR: Duration = Duration::from_secs(2);

/// 这一页要窗口办的事。
pub(crate) enum MobileEvent {
    /// 收起这一页。
    Close,
}

/// 设备「更多」菜单里的撤销：先问用户，再撤销 `id`（`MobilePage::revoke_device`）。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(crate) struct RevokePairedDevice {
    id: DeviceId,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Intro,
    Install,
    Pair,
}

/// 「推一条试试」进行到哪了。
#[derive(Default)]
enum PushProbe {
    #[default]
    Idle,
    Sending {
        _task: Task<()>,
    },
    /// 推到了几台。
    Sent(usize),
    /// 没推出去，原因已经翻译好。
    Failed(String),
}

pub(crate) struct MobilePage {
    step: Step,
    pairing: Pairing,
    focus: FocusHandle,
    /// 装 App 的二维码，有安装链接时才有。
    install_qr: Option<Rc<Qr>>,
    /// 电脑名的输入框，以及它的回车、Esc、失焦和配置的改动；空着时用系统的电脑名 `system_name`。
    name: Entity<InputState>,
    /// 输入框上次和配置对齐时配置里的名字。输入框里还是它时用户没改过，不写回，配置在别处改了时跟着换。
    synced_name: String,
    system_name: SharedString,
    _name_events: [Subscription; 3],
    /// 这台机器的网卡和地址，进配对这一步、点刷新时重读。
    interfaces: Vec<(String, IpAddr)>,
    /// 二维码上次照着生成的 `ADDRESS_KEY`；配置在别处改了时据此重新生成。
    synced_address: Option<IpAddr>,
    /// 这一页打开「不在同一个网络也能连」之前 `ADDRESS_KEY` 的值，关掉时写回。
    anywhere_before: Option<Option<IpAddr>>,
    /// 网络的下拉列表展开着。
    network_open: bool,
    /// 刚复制了配对链接：按钮上写「已复制」，任务到点后换回来。
    copied: Option<Task<()>>,
    /// 这一页里会转的盲文格画在这一层，转一格不让这一页重画。
    spinners: SpinnerLayer,
    /// 开着的设备「更多」菜单。
    menu: Option<Menu>,
    /// 菜单是哪台设备的「更多」按钮开的。
    menu_device: Option<DeviceId>,
    /// 开着菜单的「更多」按钮上一帧的下沿离窗口顶多远，菜单最高到窗口底边。
    menu_bottom: Rc<Cell<f32>>,
    test_push: PushProbe,
    /// 远程访问那一栏的开关上次没写进配置的原因。
    config_error: Option<String>,
}

impl EventEmitter<MobileEvent> for MobilePage {}

impl Focusable for MobilePage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl MenuHost for MobilePage {
    fn menu_slot(&mut self) -> &mut Option<Menu> {
        &mut self.menu
    }

    fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }
}

impl Render for MobilePage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focus_menu(window, cx);
        self.render_page(window, cx)
    }
}

impl MobilePage {
    /// 刚打开的一页：配过设备时在配对这一步，否则在第一步。设备表现读一次，不等下一轮；读到配过设备而还在
    /// 第一步时翻到配对这一步。这两种都不生成口令（`show_pair_idle`），用户只是来看设备时不开出能用的口令，
    /// 也不把别处（`runode remote pair`）在用的口令换掉。
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let system_name = host_name();
        let saved = cx.global::<AppConfig>().0.remote_access_name.clone().unwrap_or_default();
        let name = cx.new(|cx| {
            InputState::new(saved.clone(), cx)
                .with_placeholder(system_name.clone())
                .with_label(rust_i18n::t!("mobile.name_label").into_owned())
        });
        let events = cx.subscribe_in(&name, window, |this, field, event: &InputEvent, window, cx| match event {
            InputEvent::Next => window.focus(&this.focus, cx),
            // 不改了：换回配置里的名字。
            InputEvent::Dismiss => {
                let saved = cx.global::<AppConfig>().0.remote_access_name.clone().unwrap_or_default();
                field.update(cx, |field, cx| field.set_query(saved.clone(), cx));
                this.synced_name = saved;
                window.focus(&this.focus, cx);
            }
            InputEvent::Changed(_) | InputEvent::Previous => {}
        });
        // 别的窗口或者手改配置换了名字：输入框没改过时跟着换，免得失焦时把旧名字写回去。换了在哪听时
        // 重新生成二维码。远程访问那一栏的开关画的是配置，配置一变就重画。
        let config = cx.observe_global::<AppConfig>(|this, cx| {
            let address = cx.global::<AppConfig>().0.remote_access_address;
            if address != this.synced_address {
                this.synced_address = address;
                this.restart_pairing(cx);
            }
            // 不是所有地址了（下拉框、别的窗口或手改配置换掉了），记着的打开前的值作废。
            if !address.is_some_and(|addr| addr.is_unspecified()) {
                this.anywhere_before = None;
            }
            let saved = cx.global::<AppConfig>().0.remote_access_name.clone().unwrap_or_default();
            if saved != this.synced_name {
                if this.name.read(cx).query().trim() == this.synced_name {
                    this.name.update(cx, |field, cx| field.set_query(saved.clone(), cx));
                }
                this.synced_name = saved;
            }
            cx.notify();
        });
        // 回车、点别处都是离开输入框，在这里写回。
        let blurred = cx.on_focus_out(&name.focus_handle(cx), window, |this, _, _, cx| this.save_machine_name(cx));
        let paired = !devices::devices(cx).is_empty();
        let refreshed = devices::refresh(cx);
        cx.spawn(async move |this, cx| {
            refreshed.await;
            this.update(cx, |this, cx| {
                if this.step == Step::Intro && !devices::devices(cx).is_empty() {
                    this.show_pair_idle(cx);
                }
            })
            .ok();
        })
        .detach();
        Self {
            step: if paired { Step::Pair } else { Step::Intro },
            pairing: Pairing::Idle,
            focus: cx.focus_handle(),
            install_qr: IOS_APP_URL.and_then(Qr::new).map(Rc::new),
            name,
            synced_name: saved,
            system_name: system_name.into(),
            _name_events: [events, blurred, config],
            interfaces: if paired { local_interfaces() } else { Vec::new() },
            synced_address: cx.global::<AppConfig>().0.remote_access_address,
            anywhere_before: None,
            network_open: false,
            copied: None,
            spinners: SpinnerLayer::new(cx),
            menu: None,
            menu_device: None,
            menu_bottom: Rc::new(Cell::new(0.)),
            test_push: PushProbe::Idle,
            config_error: None,
        }
    }

    /// 画这一页里转着的盲文格的那一层，窗口把它紧跟在缓存的这一页后面画。
    pub(crate) fn spinner_overlay(&self) -> AnyElement {
        self.spinners.overlay()
    }

    /// 用户改过输入框里的电脑名时写回配置；监听方换上新名字后重新生成二维码。要收起这一页时先调：
    /// 没按回车就切到别处时，这一页连同监听失焦的订阅一起没了，等不到失焦再写。
    pub(crate) fn save_machine_name(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).query().trim().to_owned();
        if name == self.synced_name {
            return;
        }
        let values = if name.is_empty() { Vec::new() } else { vec![name.clone()] };
        let written = runode_config::config_path()
            .ok_or_else(|| rust_i18n::t!("settings.no_home").into_owned())
            .and_then(|path| {
                crate::config::write_values(&path, NAME_KEY, &values, cx)
                    .map_err(|err| rust_i18n::t!("settings.write_failed", err = err.to_string()).into_owned())
            });
        match written {
            Ok(_) => {
                self.synced_name = name;
                self.restart_pairing(cx);
            }
            Err(err) => self.pairing = Pairing::Failed(err),
        }
        cx.notify();
    }

    /// 二维码已经在、在等或者远程访问开着却没生成成的时候按现在的名字和网络重新生成；还没开始（远程访问
    /// 没开）时不动。
    fn restart_pairing(&mut self, cx: &mut Context<Self>) {
        let pending = matches!(self.pairing, Pairing::Starting { .. } | Pairing::Waiting(_));
        let failed = matches!(self.pairing, Pairing::Failed(_)) && cx.global::<AppConfig>().0.remote_access;
        if self.step == Step::Pair && (pending || failed) {
            self.start_pairing(cx);
        }
    }

    fn refresh_interfaces(&mut self) {
        self.interfaces = local_interfaces();
    }

    /// 下拉框里挑了在哪听：用户自己挑的，「不在同一个网络也能连」记着的打开前的值作废。
    fn pick_network(&mut self, network: Option<IpAddr>, cx: &mut Context<Self>) {
        self.network_open = false;
        self.anywhere_before = None;
        self.write_address(network, cx);
        cx.notify();
    }

    /// 「不在同一个网络也能连」拨到 `on`，写什么、记什么由 `anywhere_switch` 定；没写进去时记着的不变。
    fn set_anywhere(&mut self, on: bool, cx: &mut Context<Self>) {
        let current = cx.global::<AppConfig>().0.remote_access_address;
        let (address, before) = anywhere_switch(current, self.anywhere_before, on);
        if address == current || self.write_address(address, cx) {
            self.anywhere_before = before;
        }
        cx.notify();
    }

    /// 写回在哪听（`None` 是所有局域网地址），配置重载后由观察配置的回调重新生成二维码；写进去了回真。
    fn write_address(&mut self, network: Option<IpAddr>, cx: &mut Context<Self>) -> bool {
        let value = network.map(|addr| addr.to_string()).unwrap_or_default();
        match crate::config::set(ADDRESS_KEY, &value, cx) {
            Ok(()) => true,
            Err(err) => {
                self.pairing =
                    Pairing::Failed(rust_i18n::t!("settings.write_failed", err = err.to_string()).into_owned());
                false
            }
        }
    }

    /// 停在配对这一步、不生成口令：口令等用户按「生成二维码」才生成。
    fn show_pair_idle(&mut self, cx: &mut Context<Self>) {
        self.step = Step::Pair;
        self.pairing = Pairing::Idle;
        self.network_open = false;
        self.refresh_interfaces();
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
        self.pairing.start(|this: &mut Self| Some(&mut this.pairing), cx);
    }

    /// 在配置里打开远程访问，等监听方开好就生成口令。
    fn turn_on_remote_access(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = crate::config::set(runode_config::REMOTE_ACCESS, "true", cx) {
            self.pairing = Pairing::Failed(rust_i18n::t!("settings.write_failed", err = err.to_string()).into_owned());
            cx.notify();
            return;
        }
        self.config_error = None;
        self.start_pairing(cx);
    }

    /// 远程访问的开关：打开时照 `turn_on_remote_access`；关掉时丢掉在等的口令，配对卡换回没开的样子。
    fn set_remote_access(&mut self, on: bool, cx: &mut Context<Self>) {
        if on {
            self.turn_on_remote_access(cx);
            return;
        }
        if self.write_switch(runode_config::REMOTE_ACCESS, false, cx) {
            self.pairing = Pairing::Idle;
        }
        cx.notify();
    }

    /// 把开关 `key` 写成 `on`，写不进去时记下原因；写进去了回真。
    fn write_switch(&mut self, key: &str, on: bool, cx: &mut Context<Self>) -> bool {
        let written = crate::config::set(key, if on { "true" } else { "false" }, cx);
        self.config_error = written
            .as_ref()
            .err()
            .map(|err| rust_i18n::t!("settings.write_failed", err = err.to_string()).into_owned());
        cx.notify();
        written.is_ok()
    }

    /// 马上给登记过推送的手机推一条通知（`runode_remote::send_test_push`），用当前配置里的推送设置，推送
    /// 开没开都推。
    fn send_test_push(&mut self, cx: &mut Context<Self>) {
        if matches!(self.test_push, PushProbe::Sending { .. }) {
            return;
        }
        let dirs = runode_paths::Dirs::from_env();
        let settings = crate::remote_access::push_settings(&cx.global::<AppConfig>().0);
        let content = runode_remote::TestPush {
            title: rust_i18n::t!("settings.test_push.push_title").into_owned(),
            lines: vec![rust_i18n::t!("settings.test_push.push_line").into_owned()],
        };
        let job = cx.background_spawn(async move { runode_remote::send_test_push(&dirs, &settings, &content) });
        let task = cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.test_push = match result {
                    Ok(0) => PushProbe::Failed(rust_i18n::t!("settings.test_push.no_phone").into_owned()),
                    Ok(count) => PushProbe::Sent(count),
                    Err(err) => PushProbe::Failed(rust_i18n::t!("settings.test_push.failed", err = err).into_owned()),
                };
                cx.notify();
            })
            .ok();
        });
        self.test_push = PushProbe::Sending { _task: task };
        cx.notify();
    }

    /// 在设备 `id` 的「更多」按钮下面弹出菜单；已经开着时关掉。
    fn toggle_device_menu(&mut self, id: DeviceId, window: &mut Window, cx: &mut Context<Self>) {
        if self.dropdown_open() {
            self.close_menu(window, cx);
            return;
        }
        let revoke = MenuItem::new(rust_i18n::t!("machine.revoke"), Box::new(RevokePairedDevice { id }), true, cx);
        self.menu_device = Some(id);
        self.open_dropdown(vec![Some(revoke)], self.focus.clone(), None, cx);
    }

    fn render_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = &cx.theme().clone();
        self.spinners.clear();
        let pair = self.step == Step::Pair;
        let content = match self.step {
            Step::Intro => self.render_intro(theme, cx),
            Step::Install => self.render_install(theme, cx),
            Step::Pair => self.render_pair(theme, window, cx),
        };
        let body = div().id("mobile-scroll").size_full().overflow_y_scroll().flex().flex_col().items_center();
        let body = if pair {
            body.px(px(24.)).pt(px(44.)).pb(px(32.)).child(div().w_full().max_w(scaled(980.)).child(content))
        } else {
            body.px(px(48.)).py(px(40.)).child(div().my_auto().w_full().max_w(px(820.)).child(content))
        };
        let label = rust_i18n::t!("mobile.close").into_owned();
        let close = Button::new("mobile-close", label.clone())
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
            .on_press(cx, |_, _, cx| cx.emit(MobileEvent::Close));
        let page = div()
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
            .on_action(cx.listener(|this, action: &RevokePairedDevice, window, cx| {
                this.revoke_device(action.id, window, cx);
            }))
            .size_full()
            .relative()
            .text_color(theme.foreground)
            .child(body)
            .child(div().absolute().top(px(8.)).right(px(14.)).child(close));
        self.spinners.scope(page)
    }

    /// 没配过设备时的第一页：手机端能做什么，下面开始的按钮。
    fn render_intro(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let first = if IOS_APP_URL.is_some() { Step::Install } else { Step::Pair };
        let eyebrow_text = rust_i18n::t!("mobile.eyebrow").into_owned();
        let eyebrow = div()
            .id("mobile-eyebrow")
            .aria_static_text(eyebrow_text.clone())
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(FontSize::Body)
            .text_color(theme.foreground.opacity(0.5))
            .child(svg().path(PHONE_ICON).size(px(14.)).text_color(theme.foreground.opacity(0.5)))
            // 只在画出来时转大写：报给辅助工具的照文案写，全大写的词读屏会逐个字母念。
            .child(eyebrow_text.to_uppercase());
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

    /// 配对这一步的整页：标题和说明，配对卡，下面配对过的设备和远程访问两栏。还没配过设备时（从第一步
    /// 走过来）底下有返回的按钮。
    fn render_pair(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let paired = devices::devices(cx).to_vec();
        let title = rust_i18n::t!("mobile.page_title").into_owned();
        let body = rust_i18n::t!("mobile.page_body").into_owned();
        let mut header = div().flex().flex_col().gap(px(4.));
        if IOS_APP_URL.is_some() && paired.is_empty() {
            header = header.child(div().pb(px(8.)).child(step_line(2, theme)));
        }
        let header = header
            .child(
                div()
                    .id("mobile-headline")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(title.clone())
                    .text_size(FontSize::Display)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(text_label("mobile-lead", body, theme.subtle_foreground).text_size(FontSize::Medium));
        let card = self.render_pair_card(theme, cx);
        let devices = self.render_devices(&paired, theme, window, cx);
        let remote = self.render_remote(theme, cx);
        let back = paired.is_empty().then(|| {
            let back_to = if IOS_APP_URL.is_some() { Step::Install } else { Step::Intro };
            let label = format!("←  {}", rust_i18n::t!("mobile.back"));
            div().child(
                Button::new("mobile-back", spoken(&label))
                    .variant(ButtonVariant::Link)
                    .with_size(ButtonSize::Lg)
                    .cursor_pointer()
                    .child(label)
                    .on_press(cx, move |this, _, cx| this.go(back_to, cx)),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .text_size(FontSize::Medium)
            .child(header)
            .child(card)
            .child(div().flex().flex_wrap().items_start().gap(px(20.)).child(devices).child(remote))
            .children(back)
    }

    /// 配对卡：左边二维码（还没有码时是同样大的占位），右边配对新手机的步骤和这个码的状态；配好了时右边换成
    /// 配好了的说明和接下来能做的事。
    fn render_pair_card(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let visual = match &self.pairing {
            Pairing::Waiting(waiting) => {
                let qr = waiting.qr();
                // 一个模块按半点取整，Retina 屏上落在整像素上，码的边是清楚的；白底留白是模块的 1.5 倍。
                let module = (QR_SIDE * 2. / (qr.width() as f32 + 3.)).floor() / 2.;
                // 二维码里就是配对链接，写进说明里，读屏和自动化工具不用扫码也拿得到。
                qr_image("mobile-pair-qr", rust_i18n::t!("mobile.pair_qr").into_owned(), waiting.uri().to_string())
                    .size(px(QR_SIDE))
                    .rounded(px(12.))
                    .bg(gpui::white())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(qr_code(qr, module))
                    .into_any_element()
            }
            Pairing::Paired { .. } => qr_placeholder(theme)
                .child(svg().path(CHECK_ICON).size(px(28.)).text_color(theme.success))
                .into_any_element(),
            _ => qr_placeholder(theme)
                .child(svg().path(PHONE_ICON).size(px(28.)).text_color(theme.muted_foreground.opacity(0.6)))
                .into_any_element(),
        };
        let right = match &self.pairing {
            Pairing::Paired { name } => self.render_paired(name, theme, cx),
            _ => {
                let steps = ["mobile.step1", "mobile.step2", "mobile.step3"].into_iter().enumerate().map(|(i, key)| {
                    let n = i + 1;
                    let text = rust_i18n::t!(key).into_owned();
                    div()
                        .id(SharedString::from(format!("mobile-step-{n}")))
                        .role(Role::ListItem)
                        .aria_label(text.clone())
                        .flex()
                        .items_start()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex_none()
                                .size(scaled(22.))
                                .rounded_full()
                                .bg(theme.mix(0.10))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(FontSize::Body)
                                .child(n.to_string()),
                        )
                        .child(div().min_w_0().pt(px(2.)).child(text))
                });
                let copy_hint = matches!(self.pairing, Pairing::Waiting(_)).then(|| {
                    text_label(
                        "mobile-copy-hint",
                        rust_i18n::t!("mobile.copy_hint").into_owned(),
                        theme.muted_foreground,
                    )
                    .text_size(FontSize::Body)
                });
                card_column()
                    .gap(px(16.))
                    .child(
                        div()
                            .id("mobile-pair-title")
                            .role(Role::Heading)
                            .aria_level(2)
                            .aria_label(rust_i18n::t!("mobile.pair_new").into_owned())
                            .text_size(FontSize::Title)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(rust_i18n::t!("mobile.pair_new").into_owned()),
                    )
                    .child(
                        div()
                            .id("mobile-steps")
                            .role(Role::List)
                            .aria_label(rust_i18n::t!("mobile.pair_new").into_owned())
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .children(steps),
                    )
                    .child(self.render_pair_status(theme, cx))
                    .children(copy_hint)
            }
        };
        div()
            .id("mobile-pair-card")
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(32.))
            .gap_y(px(24.))
            .pt(px(22.))
            .pb(px(22.))
            .pl(px(22.))
            .pr(px(28.))
            .rounded(px(14.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .child(visual)
            .child(right)
    }

    /// 配对卡上分隔线下面那一行：这个码现在怎样，右边能做的事。
    fn render_pair_status(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let remote_on = cx.global::<AppConfig>().0.remote_access;
        let row = div()
            .pt(px(14.))
            .border_t_1()
            .border_color(theme.border)
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(12.))
            .gap_y(px(8.))
            .text_color(theme.subtle_foreground);
        let actions = div().ml_auto().flex().items_center().gap(px(8.));
        let turn_on = |cx: &mut Context<Self>| {
            Button::new("mobile-turn-on", rust_i18n::t!("mobile.turn_on").into_owned())
                .variant(ButtonVariant::Default)
                .on_press(cx, |this, _, cx| this.turn_on_remote_access(cx))
        };
        match &self.pairing {
            Pairing::Waiting(waiting) => {
                let uri = waiting.uri();
                let regenerate = Button::new("mobile-regenerate", rust_i18n::t!("mobile.regenerate").into_owned())
                    .variant(ButtonVariant::Outline)
                    .on_press(cx, |this, _, cx| this.start_pairing(cx));
                // 复制配对链接：扫不了码的手机照着输，别的电脑没法扫码、粘贴它配对。复制后按钮上的字换成
                // 「已复制」，留 `COPIED_FOR`。
                let label = if self.copied.is_some() { "mobile.copied" } else { "mobile.manual" };
                let manual = Button::new("mobile-copy-code", rust_i18n::t!(label).into_owned())
                    .variant(ButtonVariant::Ghost)
                    .text_color(theme.subtle_foreground)
                    .on_press(cx, move |this, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(uri.to_string()));
                        this.copied = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(COPIED_FOR).await;
                            this.update(cx, |this, cx| {
                                this.copied = None;
                                cx.notify();
                            })
                            .ok();
                        }));
                        cx.notify();
                    });
                row.child(status_mark("mobile-pair-mark", MarkKind::Working { spin: true }, 13., cx))
                    .child(text_label(
                        "mobile-waiting",
                        rust_i18n::t!("mobile.waiting_scan").into_owned(),
                        theme.subtle_foreground,
                    ))
                    .child(expires_line(waiting.expires_at(), waiting.expires_unix(), theme))
                    .child(actions.child(regenerate).child(manual))
            }
            Pairing::Starting { .. } => {
                row.child(status_mark("mobile-pair-mark", MarkKind::Working { spin: true }, 13., cx)).child(text_label(
                    "mobile-starting",
                    rust_i18n::t!("settings.pairing.starting").into_owned(),
                    theme.subtle_foreground,
                ))
            }
            Pairing::Idle if remote_on => row.child(
                actions.child(
                    Button::new("mobile-generate", rust_i18n::t!("mobile.generate").into_owned())
                        .variant(ButtonVariant::Default)
                        .on_press(cx, |this, _, cx| this.start_pairing(cx)),
                ),
            ),
            Pairing::Idle => row
                .child(
                    text_label("mobile-off", rust_i18n::t!("mobile.off").into_owned(), theme.subtle_foreground)
                        .flex_1()
                        .min_w_0(),
                )
                .child(actions.child(turn_on(cx))),
            Pairing::Failed(err) => {
                let next = if remote_on {
                    Button::new("mobile-generate", rust_i18n::t!("mobile.retry").into_owned())
                        .variant(ButtonVariant::Default)
                        .on_press(cx, |this, _, cx| this.start_pairing(cx))
                } else {
                    turn_on(cx)
                };
                row.child(text_label("mobile-error", err.clone(), theme.destructive).flex_1().min_w_0())
                    .child(actions.child(next))
            }
            Pairing::Paired { .. } => row,
        }
    }

    /// 配好了：和谁配上了，`terminal-host` 关着时提醒一句，下面是完成和再配一台。
    fn render_paired(&self, name: &str, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let background_on = cx.global::<AppConfig>().0.terminal_host;
        let title = rust_i18n::t!("mobile.paired_title").into_owned();
        let actions = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                Button::new("mobile-done", rust_i18n::t!("mobile.done").into_owned())
                    .variant(ButtonVariant::Default)
                    .on_press(cx, |_, _, cx| cx.emit(MobileEvent::Close)),
            )
            .child(
                Button::new("mobile-again", rust_i18n::t!("mobile.again").into_owned())
                    .variant(ButtonVariant::Outline)
                    .on_press(cx, |this, _, cx| this.start_pairing(cx)),
            );
        // `terminal-host` 关着时退出 app 宿主跟着退出、远程访问跟着停，提醒一句。
        let background = (!background_on).then(|| {
            notice("mobile-background", rust_i18n::t!("settings.pairing.background").into_owned(), theme).child(
                div().pt(px(10.)).child(
                    Button::new("mobile-keep", rust_i18n::t!("mobile.keep").into_owned())
                        .variant(ButtonVariant::Outline)
                        .on_press(
                            cx,
                            // 配置重载后这一页跟着重画，这一块就收起来了。
                            |_, _, cx| {
                                if let Err(err) = crate::config::set(runode_config::TERMINAL_HOST, "true", cx) {
                                    tracing::warn!("could not turn on {}: {err:#}", runode_config::TERMINAL_HOST);
                                }
                            },
                        ),
                ),
            )
        });
        card_column()
            .gap(px(12.))
            .child(
                div()
                    .id("mobile-pair-title")
                    .role(Role::Heading)
                    .aria_level(2)
                    .aria_label(title.clone())
                    .text_size(FontSize::Title)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(text_label(
                "mobile-paired-body",
                rust_i18n::t!("mobile.paired_body", name = name).into_owned(),
                theme.subtle_foreground,
            ))
            .children(background)
            .child(div().pt(px(4.)).child(actions))
    }

    /// 左栏：配对过的设备，一台一行；没有时一句话。
    fn render_devices(&self, paired: &[Paired], theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Div {
        let title = rust_i18n::t!("mobile.devices").into_owned();
        let list = if paired.is_empty() {
            card(theme).child(
                text_label(
                    "mobile-no-devices",
                    rust_i18n::t!("mobile.no_devices").into_owned(),
                    theme.muted_foreground,
                )
                .px(px(16.))
                .py(px(14.)),
            )
        } else {
            let rows: Vec<_> = paired
                .iter()
                .enumerate()
                .map(|(i, device)| self.device_row(device, i > 0, theme, window, cx))
                .collect();
            card(theme).child(div().id("mobile-devices").role(Role::List).aria_label(title.clone()).children(rows))
        };
        let note = (!paired.is_empty()).then(|| {
            text_label("mobile-unpair-note", rust_i18n::t!("mobile.unpair_note").into_owned(), theme.muted_foreground)
                .px(px(2.))
                .text_size(FontSize::Body)
        });
        div()
            .flex_grow(1.)
            .flex_shrink(1.)
            .flex_basis(scaled(400.))
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(section_title(title, theme))
            .child(list)
            .children(note)
    }

    /// 配对过的一台设备：手机或电脑的图标，名字，连着没有或哪天配的，右边「更多」菜单（撤销，先问一句）。
    fn device_row(
        &self,
        device: &Paired,
        divided: bool,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = device.id;
        let (year, month, day) = local_date(device.paired_at);
        let date = rust_i18n::t!("mobile.date", y = year, m = month, d = day);
        let paired_on = rust_i18n::t!("mobile.paired_on", date = date).into_owned();
        let (icon, kind) = match device.kind {
            DeviceKind::Mobile => (PHONE_ICON, rust_i18n::t!("mobile.kind_mobile").into_owned()),
            DeviceKind::Desktop => (MONITOR_ICON, rust_i18n::t!("mobile.kind_desktop").into_owned()),
        };
        let live = rust_i18n::t!("mobile.live").into_owned();
        // 读给辅助工具的：是哪种、连着没有、哪天配的，各是一句。
        let summary = if device.live { format!("{kind}. {live}. {paired_on}") } else { format!("{kind}. {paired_on}") };
        let status =
            div().flex().items_center().gap(px(6.)).text_size(FontSize::Body).text_color(theme.subtle_foreground);
        let status = if device.live {
            status.child(div().flex_none().size(px(6.)).rounded_full().bg(theme.success)).child(live)
        } else {
            status.child(paired_on)
        };
        let open = self.menu_device == Some(id) && self.dropdown_open();
        let menu = if open { self.render_dropdown_menu(self.menu_bottom.get(), window, cx) } else { None };
        let bottom = self.menu_bottom.clone();
        let more = Button::new(
            SharedString::from(format!("mobile-device-more-{id}")),
            rust_i18n::t!("mobile.device_more", name = device.name.as_ref()).into_owned(),
        )
        .icon(MORE_ICON)
        .icon_size(15.)
        .with_size(ButtonSize::Icon)
        .size(px(28.))
        .rounded(px(7.))
        .aria_expanded(open)
        .relative()
        // 菜单开着时在捕获阶段就关掉、不再往下传：不然菜单自己的「点到外面就关」先关了，按下又把它打开。
        .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
            if this.dropdown_open() {
                cx.stop_propagation();
                this.close_menu(window, cx);
            }
        }))
        .on_press_down(cx, move |this, window, cx| this.toggle_device_menu(id, window, cx))
        .when(open, |more| {
            more.child(
                canvas(move |bounds, _, _| bottom.set(f32::from(bounds.bottom())), |_, (), _, _| {})
                    .absolute()
                    .size_full(),
            )
        })
        .children(menu);
        div()
            .id(SharedString::from(format!("mobile-device-{id}")))
            .role(Role::ListItem)
            .aria_label(device.name.clone())
            .aria_description(summary)
            .ml(px(16.))
            .pr(px(10.))
            .py(px(12.))
            .when(divided, |row| row.border_t_1().border_color(theme.border))
            .flex()
            .items_center()
            .gap(px(12.))
            .child(svg().flex_none().path(icon).size(px(18.)).text_color(theme.subtle_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(
                        div()
                            .text_size(FontSize::Large)
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(device.name.clone()),
                    )
                    .child(status),
            )
            .child(more)
    }

    /// 右栏：远程访问的开关，开着时还有电脑名和网络，最后是推送和「推一条试试」。
    fn render_remote(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let config = &cx.global::<AppConfig>().0;
        let (remote_on, push_on) = (config.remote_access, config.remote_access_push);
        let anywhere = config.remote_access_address.is_some_and(|addr| addr.is_unspecified());
        let title = rust_i18n::t!("mobile.remote_title").into_owned();
        let mut rows: Vec<AnyElement> = vec![
            switch_row(
                "mobile-remote-access-row",
                Switch::new("mobile-remote-access", rust_i18n::t!("mobile.remote_on").into_owned(), remote_on)
                    .on_press(cx, move |this, _, cx| this.set_remote_access(!remote_on, cx)),
                rust_i18n::t!("mobile.remote_on").into_owned(),
                rust_i18n::t!("mobile.remote_on_hint").into_owned(),
                None,
                None,
                theme,
            )
            .into_any_element(),
            switch_row(
                "mobile-anywhere-row",
                Switch::new("mobile-anywhere", rust_i18n::t!("mobile.anywhere").into_owned(), anywhere)
                    .on_press(cx, move |this, _, cx| this.set_anywhere(!anywhere, cx)),
                rust_i18n::t!("mobile.anywhere").into_owned(),
                rust_i18n::t!("mobile.anywhere_hint").into_owned(),
                None,
                None,
                theme,
            )
            .into_any_element(),
        ];
        if remote_on {
            rows.push(self.render_machine_name(theme, cx).into_any_element());
            rows.push(self.render_network(theme, cx).into_any_element());
        }
        let sending = matches!(self.test_push, PushProbe::Sending { .. });
        let label = if sending { "settings.test_push.sending" } else { "mobile.test_push" };
        let test = Button::new("mobile-test-push", rust_i18n::t!(label).into_owned())
            .variant(ButtonVariant::Outline)
            .aria_disabled(sending)
            .on_press(cx, |this, _, cx| this.send_test_push(cx));
        let result = match &self.test_push {
            PushProbe::Sent(count) => {
                Some((rust_i18n::t!("settings.test_push.sent", count = count).into_owned(), theme.subtle_foreground))
            }
            PushProbe::Failed(err) => Some((err.clone(), theme.destructive)),
            PushProbe::Idle | PushProbe::Sending { .. } => None,
        };
        let result = result.map(|(text, color)| text_label("mobile-test-push-result", text, color));
        rows.push(
            switch_row(
                "mobile-push-row",
                Switch::new("mobile-push", rust_i18n::t!("mobile.push").into_owned(), push_on).on_press(
                    cx,
                    move |this, _, cx| {
                        this.write_switch(PUSH_KEY, !push_on, cx);
                    },
                ),
                rust_i18n::t!("mobile.push").into_owned(),
                rust_i18n::t!("mobile.push_hint").into_owned(),
                result,
                Some(test.into_any_element()),
                theme,
            )
            .into_any_element(),
        );
        let rows = rows.into_iter().enumerate().map(|(i, row)| {
            div().mx(px(16.)).py(px(12.)).when(i > 0, |div| div.border_t_1().border_color(theme.border)).child(row)
        });
        let error = self
            .config_error
            .clone()
            .map(|err| text_label("mobile-config-error", err, theme.destructive).px(px(2.)).text_size(FontSize::Body));
        div()
            .flex_grow(1.25)
            .flex_shrink(1.)
            .flex_basis(scaled(440.))
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(section_title(title.clone(), theme))
            .child(card(theme).child(div().id("mobile-remote").role(Role::Group).aria_label(title).children(rows)))
            .children(error)
    }

    /// 电脑名：输入框，空着时浮着系统的电脑名，下面说手机上会看到什么。
    fn render_machine_name(&self, theme: &Theme, cx: &App) -> Stateful<Div> {
        let saved = cx.global::<AppConfig>().0.remote_access_name.clone();
        let shown = saved.map_or_else(|| self.system_name.clone(), SharedString::from);
        labeled(
            "mobile-name",
            rust_i18n::t!("mobile.name_label").into_owned(),
            // 和下面网络的下拉框一个样子。
            Input::new(&self.name)
                .h(scaled(32.))
                .px(px(10.))
                .rounded(px(7.))
                .bg(theme.field)
                .text_size(FontSize::Medium),
            rust_i18n::t!("mobile.name_hint", name = shown).into_owned(),
            theme,
        )
    }

    /// 网络（在哪听）：展开、收起的下拉框，旁边是重读网卡的按钮；展开时选项列在下面：所有局域网地址、
    /// 0.0.0.0、各个网卡的地址，配置里的地址这台电脑现在没有时也列上。
    fn render_network(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let current = cx.global::<AppConfig>().0.remote_access_address;
        let label = |network: Option<IpAddr>| -> SharedString {
            let Some(addr) = network else {
                return rust_i18n::t!("mobile.network_lan").into_owned().into();
            };
            if addr.is_unspecified() {
                return rust_i18n::t!("mobile.network_any", addr = addr).into_owned().into();
            }
            match self.interfaces.iter().find(|(_, seen)| *seen == addr) {
                Some((name, addr)) => network_label(name, *addr).into(),
                None => rust_i18n::t!("mobile.network_missing", addr = addr).into_owned().into(),
            }
        };
        let hint = match current {
            None => rust_i18n::t!("mobile.network_lan_hint"),
            Some(addr) if addr.is_unspecified() => rust_i18n::t!("mobile.network_any_hint"),
            Some(_) => rust_i18n::t!("mobile.network_addr_hint"),
        };
        let select =
            SelectTrigger::new("mobile-network", rust_i18n::t!("mobile.network_label").into_owned(), label(current))
                .aria_expanded(self.network_open)
                .h(scaled(32.))
                .px(px(10.))
                .rounded(px(7.))
                .bg(theme.field)
                .border_1()
                .border_color(theme.input)
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
            .size(scaled(32.))
            .rounded(px(7.))
            .cursor_pointer()
            .on_press(cx, |this, _, cx| {
                this.refresh_interfaces();
                this.restart_pairing(cx);
                cx.notify();
            });
        let options = self.network_open.then(|| {
            let mut choices = vec![None, Some(IpAddr::from(Ipv4Addr::UNSPECIFIED))];
            choices.extend(self.interfaces.iter().map(|(_, addr)| Some(*addr)));
            if !choices.contains(&current) {
                choices.push(current);
            }
            div()
                .id("mobile-network-options")
                .role(Role::ListBox)
                .aria_label(rust_i18n::t!("mobile.network_label").into_owned())
                .flex()
                .flex_col()
                .p(px(4.))
                .rounded(px(8.))
                .bg(theme.field)
                .border_1()
                .border_color(theme.border)
                .children(choices.into_iter().map(|network| {
                    let chosen = network == current;
                    let text = label(network);
                    // id 按地址取，不按行号：重新列网卡后顺序、个数变了，VoiceOver 的焦点不跳。
                    let id = match network {
                        Some(addr) => SharedString::from(format!("mobile-network-option-{addr}")),
                        None => SharedString::from("mobile-network-option-lan"),
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
            hint.into_owned(),
            theme,
        )
    }
}

/// 口令还剩多久失效的那一行。倒计时经 `SpinnerLayer` 画在这一页外面，每秒走一格不让这一页重画；辅助
/// 工具读的是不随时间变的失效时刻（Unix 秒 `unix` 的本地钟点），缓存着的节点不会报出过时的秒数。
fn expires_line(until: Instant, unix: u64, theme: &Theme) -> Stateful<Div> {
    let time = runode_todo::LocalTime::from_unix(unix)
        .map_or_else(String::new, |at| format!("{:02}:{:02}", at.hour, at.minute));
    div()
        .id("mobile-expires")
        .aria_static_text(rust_i18n::t!("mobile.expires_at", time = time).into_owned())
        .flex()
        .items_center()
        .gap(px(4.))
        .text_color(theme.muted_foreground)
        .child(rust_i18n::t!("mobile.expires_before").into_owned())
        .child(spinner::countdown("mobile-expires-countdown", until))
        .child(rust_i18n::t!("mobile.expires_after").into_owned())
}

/// Unix 秒在本地时区里是哪年哪月哪天。
fn local_date(secs: u64) -> (i32, i32, i32) {
    runode_todo::LocalTime::from_unix(secs).map_or((1970, 1, 1), |at| (at.year, at.month.into(), at.day.into()))
}

/// 「不在同一个网络也能连」拨到 `on`：`current` 是 `ADDRESS_KEY` 现在的值，`before` 是这一页打开它时记下
/// 的打开前的值。回要写进 `ADDRESS_KEY` 的值和之后记着的值：打开写所有地址（0.0.0.0），记下原来的值；关掉
/// 写回记着的值，没记着时写空值（所有局域网地址），用户原来挑的网卡不会丢、监听的范围也不比原来宽。已经
/// 是要拨到的样子时都不变。
fn anywhere_switch(
    current: Option<IpAddr>,
    before: Option<Option<IpAddr>>,
    on: bool,
) -> (Option<IpAddr>, Option<Option<IpAddr>>) {
    let anywhere = current.is_some_and(|addr| addr.is_unspecified());
    match (on, anywhere) {
        (true, false) => (Some(IpAddr::from(Ipv4Addr::UNSPECIFIED)), Some(current)),
        (false, true) => (before.flatten(), None),
        (true, true) | (false, false) => (current, before),
    }
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

/// 引导页的大标题；每页只有一个，id 写死。
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

/// 一句报给辅助工具的文字：GPUI 不报子元素里的文字，要另外写。
fn text_label(id: &'static str, text: String, color: Hsla) -> Stateful<Div> {
    div().id(id).aria_static_text(text.clone()).text_color(color).child(text)
}

/// 二维码的外框，作为一张图报给辅助工具，`content` 是码里的链接。
fn qr_image(id: &'static str, label: String, content: String) -> Stateful<Div> {
    div().id(id).flex_none().role(Role::Image).aria_label(label).aria_description(content)
}

/// 配对卡上还没有二维码时那块占位，和二维码一样大。
fn qr_placeholder(theme: &Theme) -> Div {
    div()
        .flex_none()
        .size(px(QR_SIDE))
        .rounded(px(12.))
        .bg(theme.mix(0.04))
        .border_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .justify_center()
}

/// 配对卡右边那一栏，窄到放不下时折到二维码下面。
fn card_column() -> Div {
    div().flex_grow(1.).flex_shrink(1.).flex_basis(scaled(360.)).min_w_0().flex().flex_col()
}

/// 下面两栏各自的那张卡片。
fn card(theme: &Theme) -> Div {
    div().rounded(px(12.)).bg(theme.surface).border_1().border_color(theme.border).overflow_hidden()
}

/// 下面两栏卡片上的小标题；卡片自己以它为名报给辅助工具，它不另报。
fn section_title(title: String, theme: &Theme) -> Div {
    div()
        .px(px(2.))
        .text_size(FontSize::Body)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.subtle_foreground)
        .child(title)
}

/// 远程访问卡片里带开关的一行：名字和说明，`note` 接在说明下面，`extra` 放在开关左边。开关自己以名字报给
/// 辅助工具，说明另报；`id` 让各行的说明分得开。
fn switch_row(
    id: &'static str,
    switch: Switch,
    title: String,
    hint: String,
    note: Option<Stateful<Div>>,
    extra: Option<AnyElement>,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(14.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(FontSize::Large).font_weight(FontWeight::MEDIUM).child(title))
                .child(text_label("hint", hint, theme.subtle_foreground))
                .children(note.map(|note| note.text_size(FontSize::Body))),
        )
        .children(extra)
        .child(switch)
}

/// 「第 n 步（共 2 步）」，前面一个圈着数字的小圆。只有装 App 和配对两步都在时才画。
fn step_line(n: u32, theme: &Theme) -> Stateful<Div> {
    let text = rust_i18n::t!("mobile.step", n = n, total = 2).into_owned();
    div()
        .id("mobile-step")
        .aria_static_text(text.clone())
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

/// 远程访问卡片里带输入框或下拉框的一行：名字，内容，下面一行说明。名字不报给辅助工具，内容的控件自己以它
/// 为名；`id` 让两项的说明分得开。
fn labeled(id: &'static str, label: String, content: impl IntoElement, hint: String, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(div().text_size(FontSize::Large).font_weight(FontWeight::MEDIUM).child(label))
        .child(content)
        .child(text_label("hint", hint, theme.subtle_foreground).text_size(FontSize::Body))
}

/// 一块浅底的提示文字。
fn notice(id: &'static str, text: String, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .aria_static_text(text.clone())
        .p(px(14.))
        .rounded(px(10.))
        .bg(theme.mix(0.04))
        .border_1()
        .border_color(theme.border)
        .text_size(FontSize::Medium)
        .line_height(scaled(20.))
        .text_color(theme.subtle_foreground)
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
    fn the_anywhere_switch_restores_what_was_there_before() {
        let any = Some(IpAddr::from(Ipv4Addr::UNSPECIFIED));
        let nic: Option<IpAddr> = Some("10.0.0.10".parse().unwrap());

        // 起点是空值（所有局域网地址）：打开写 0.0.0.0，关掉写回空值。
        let (written, before) = anywhere_switch(None, None, true);
        assert_eq!((written, before), (any, Some(None)));
        assert_eq!(anywhere_switch(written, before, false), (None, None));

        // 起点是某张网卡的地址：关掉时写回这个地址，不放宽成所有局域网地址。
        let (written, before) = anywhere_switch(nic, None, true);
        assert_eq!((written, before), (any, Some(nic)));
        assert_eq!(anywhere_switch(written, before, false), (nic, None));

        // 进来时就是 0.0.0.0、没记着打开前的值：关掉写空值，再打开记下空值。
        let (written, before) = anywhere_switch(any, None, false);
        assert_eq!((written, before), (None, None));
        assert_eq!(anywhere_switch(written, before, true), (any, Some(None)));

        // 已经是要拨到的样子：不变。
        assert_eq!(anywhere_switch(any, Some(nic), true), (any, Some(nic)));
        assert_eq!(anywhere_switch(nic, None, false), (nic, None));
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
