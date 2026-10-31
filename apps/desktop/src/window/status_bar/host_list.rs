//! 状态栏上的「远程主机」：这次运行里连过几台 SSH 主机（`host_client::remote::remotes`），一台都没有时不显示。
//! 浮层里一台一行，写着连接状态，没连着的能连上、连着的能断开；最下面一行去设置页的 SSH 主机列表，在那里
//! 忘掉主机、装登录服务、清理旧构建。

use gpui::{AnyElement, Context, Div, FontWeight, Hsla, Role, SharedString, Stateful, div, prelude::*, px, svg};

use super::{ACTIVE_DOT, PENDING_DOT, PressDown as _, StatusPopover, popover_header};
use crate::{
    assets::{CHEVRON_RIGHT_ICON, SERVER_ICON},
    host_client::remote::{self as hosts, RemoteHost, Status},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        font_size::{FontSize, scaled},
        hsla,
        theme::ActiveTheme as _,
        tooltip::tooltip,
    },
    window::WindowView,
};

/// 连接状态的小圆点：连着是绿的，正在连、等着重连是黄的，别的是灰的。
pub(in crate::window) fn dot_color(status: &Status, fg: Hsla) -> Hsla {
    match status {
        Status::Connected(_) => hsla(ACTIVE_DOT),
        Status::Connecting(_) | Status::Waiting { .. } => hsla(PENDING_DOT),
        Status::Idle | Status::Stopped { .. } => fg.opacity(0.3),
    }
}

impl WindowView {
    /// 状态栏上的那一块；一台连过的主机都没有时为空。
    pub(super) fn render_hosts_item(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let count = hosts::remotes().len();
        if count == 0 {
            return None;
        }
        let label = if count == 1 {
            rust_i18n::t!("status.hosts_count_one").into_owned()
        } else {
            rust_i18n::t!("status.hosts_count", count = count).into_owned()
        };
        let icon = svg().flex_none().path(SERVER_ICON).size(px(13.)).text_color(cx.theme().foreground.opacity(0.6));
        Some(
            self.status_item("status-hosts", StatusPopover::Hosts, cx)
                .aria_description(label.clone())
                .child(icon)
                .child(label),
        )
    }

    pub(super) fn render_hosts_popover(&self, cx: &mut Context<Self>) -> AnyElement {
        let fg = cx.theme().foreground;
        let remotes = hosts::remotes();
        let connected = remotes.iter().filter(|host| matches!(host.status(), Status::Connected(_))).count();
        let header = popover_header(
            Some(SERVER_ICON),
            rust_i18n::t!("status.hosts_title").into_owned(),
            rust_i18n::t!("status.hosts_summary", connected = connected, total = remotes.len()).into_owned(),
            fg,
            cx,
        );
        let rows: Vec<AnyElement> =
            remotes.iter().map(|host| self.render_host_row(host, cx).into_any_element()).collect();
        let manage = rust_i18n::t!("status.hosts_manage").into_owned();
        let manage_row = div()
            .id("manage-hosts")
            .role(Role::Link)
            .aria_label(manage.clone())
            .h(scaled(30.))
            .mx(px(6.))
            .px(px(8.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .hover(|row| row.bg(cx.theme().mix(0.10)))
            .child(div().flex_1().child(manage))
            .child(svg().flex_none().path(CHEVRON_RIGHT_ICON).size(px(12.)).text_color(fg.opacity(0.5)))
            .on_press_down(cx, |this, window, cx| {
                this.status_popover = None;
                this.open_ssh_settings(window, cx);
            });
        div()
            .w(px(340.))
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .id("host-rows")
                    .role(Role::List)
                    .aria_label(rust_i18n::t!("status.hosts_title"))
                    .max_h(px(360.))
                    .py(px(4.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .child(div().h(px(1.)).bg(cx.theme().border))
            .child(div().py(px(4.)).child(manage_row))
            .into_any_element()
    }

    /// 一台主机的一行：状态点、名字和状态，右边连接或断开。
    fn render_host_row(&self, host: &RemoteHost, cx: &mut Context<Self>) -> Stateful<Div> {
        let fg = cx.theme().foreground;
        let name = host.name.clone();
        let status = host.status();
        let state = state_text(host, &status);
        // 连着、正在连、等着重连的能断开（断开后不再自己重连）；别的能马上连。
        let live = matches!(status, Status::Connected(_) | Status::Connecting(_) | Status::Waiting { .. });
        let (key, op) =
            if live { ("status.host_disconnect", "disconnect") } else { ("status.host_connect", "connect") };
        let action = rust_i18n::t!(key).into_owned();
        let named = rust_i18n::t!("settings.ssh_hosts.named", action = action.as_str(), host = name.as_str());
        let button = {
            let name = name.clone();
            Button::new(op, named.into_owned())
                .variant(ButtonVariant::Ghost)
                .with_size(ButtonSize::Sm)
                .child(action)
                .on_press_down(cx, move |_, _, cx| {
                    if live {
                        crate::remote_hosts::disconnect(&name, cx);
                    } else if let Some(host) = hosts::find(&name) {
                        host.retry_now();
                    }
                })
        };
        // 行的 id 按主机名取，主机多了、顺序变了，辅助工具的焦点也跟着同一台主机。
        div()
            .id(SharedString::from(format!("host:{name}")))
            .role(Role::ListItem)
            .aria_label(name.clone())
            .aria_description(state.clone())
            .mx(px(6.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().flex_none().size(px(7.)).rounded_full().bg(dot_color(&status, fg)))
            .child(
                div()
                    .id("info")
                    .role(Role::Label)
                    .aria_label(format!("{name} · {state}"))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).child(name))
                    .child(
                        div()
                            .id("state")
                            .truncate()
                            .text_size(FontSize::Small)
                            .text_color(fg.opacity(0.55))
                            .tooltip(tooltip(state.clone(), None))
                            .child(state),
                    ),
            )
            .child(button)
    }
}

/// 心跳测的延迟 `ms` 毫秒写给人看，不到 1 毫秒的写成 `<1`。
pub(super) fn latency_text(ms: u128) -> String {
    let ms = if ms == 0 { "<1".to_owned() } else { ms.to_string() };
    rust_i18n::t!("status.host_latency", ms = ms).into_owned()
}

/// 一台主机的状态写成一行：连着时写明远端的系统、带上心跳测的延迟，没连上时是原因。
fn state_text(host: &RemoteHost, status: &Status) -> String {
    let Status::Connected(connected) = status else {
        return crate::remote_hosts::status_text(status);
    };
    let latency = host.latency().map(|latency| latency_text(latency.as_millis()));
    let system = match connected.os.as_str() {
        "macos" => Some("macOS"),
        "linux" => Some("Linux"),
        "windows" => Some("Windows"),
        _ => None,
    };
    [system.map(str::to_owned), Some(rust_i18n::t!("status.host_connected").into_owned()), latency]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}
