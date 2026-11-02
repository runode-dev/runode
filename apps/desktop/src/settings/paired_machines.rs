//! 远程主机页里「配对的电脑」一节：配对过的每台电脑一行（`paired::machines`），写着连接状态，可以改显示名、
//! 连接、断开、看远程桌面、解除配对。还有工作区、终端用着的电脑不能解除。

use crate::ui::theme::Theme;
use gpui::{
    AppContext as _, Context, Div, Entity, Focusable as _, Role, SharedString, Subscription, Window, div, prelude::*,
    px, svg,
};
use runode_remote::client::PairedMachine;

use super::{SettingsView, controls::Press as _};
use crate::{
    host_client::{
        hosts::{self, HostId, Status},
        paired,
    },
    remote_hosts,
    ui::{
        button::{Button, ButtonVariant},
        font_size::FontSize,
        icons::MONITOR_ICON,
        input::{Input, InputEvent, InputState},
    },
};

/// 正在改显示名的那台电脑：它的标识和输入框。
pub(super) struct Renaming {
    id: String,
    field: Entity<InputState>,
    _events: Subscription,
}

impl SettingsView {
    pub(super) fn render_paired_machines(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let machines = paired::machines();
        let rows = div().flex().flex_col();
        if machines.is_empty() {
            let text = rust_i18n::t!("paired.settings_empty").into_owned();
            return rows.child(
                div()
                    .id("paired-machines-empty")
                    .role(Role::Label)
                    .aria_label(text.clone())
                    .py(px(12.))
                    .text_size(FontSize::Body)
                    .text_color(theme.foreground.opacity(0.55))
                    .child(text),
            );
        }
        let in_use = (self.hosts_in_use)(cx);
        rows.children(machines.iter().map(|machine| {
            let used = in_use.contains(&HostId::Paired(machine.id.clone()));
            self.render_paired_machine(machine, used, theme, cx)
        }))
    }

    fn render_paired_machine(
        &self,
        machine: &PairedMachine,
        in_use: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let id = HostId::Paired(machine.id.clone());
        let title = remote_hosts::display_name(&id, cx);
        // 起了显示名的电脑，名字下面另写那台电脑报的主机名。
        let host_name = remote_hosts::has_display_name(&id, cx).then(|| machine.host_name.clone());
        let status = hosts::status_of(&id);
        let text = status_line(&status);
        let renaming = self.renaming_machine.as_ref().filter(|renaming| renaming.id == machine.id);
        // 各行的元素 id 挂在这一行带标识的 id 下面，拼不出别的行的 id。
        let button = |key: &str, op: &'static str| {
            let label = rust_i18n::t!(key).into_owned();
            let named = rust_i18n::t!("settings.ssh_hosts.named", action = label.as_str(), host = title.as_str());
            Button::new(op, named.into_owned()).variant(ButtonVariant::Secondary).child(label)
        };
        let rename = renaming.is_none().then(|| {
            let machine = machine.id.clone();
            button("paired.rename", "rename")
                .on_press(cx, move |this, window, cx| this.start_machine_rename(machine.clone(), window, cx))
        });
        let reconnect = matches!(status, Status::Idle | Status::Stopped { .. } | Status::Waiting { .. }).then(|| {
            let key =
                if matches!(status, Status::Waiting { .. }) { "ssh.reconnect" } else { "settings.ssh_hosts.connect" };
            let id = id.clone();
            button(key, "connect").on_press(cx, move |_, _, _| hosts::retry_now(&id))
        });
        let disconnect =
            matches!(status, Status::Connecting(_) | Status::Connected(_) | Status::Waiting { .. }).then(|| {
                let id = id.clone();
                button("settings.ssh_hosts.disconnect", "disconnect")
                    .on_press(cx, move |_, _, cx| remote_hosts::disconnect(&id, cx))
            });
        let remote_desktop = (crate::screen_viewer::SUPPORTED
            && matches!(status, Status::Connected(_))
            && remote_hosts::screen_capturable(&id, cx))
        .then(|| {
            let id = id.clone();
            button("ssh.remote_desktop", "remote-desktop")
                .on_press(cx, move |_, _, cx| crate::window::show_remote_desktop(id.clone(), cx))
        });
        let forget = button("paired.forget", "forget");
        let forget = if in_use {
            let why = rust_i18n::t!("paired.in_use").into_owned();
            forget.disabled(true).tooltip(crate::ui::tooltip::tooltip(why, None))
        } else {
            forget.on_press(cx, move |_, _, cx| remote_hosts::forget(&id, cx))
        };
        let name = match renaming {
            Some(renaming) => Input::new(&renaming.field).w(px(240.)).into_any_element(),
            None => div()
                .id("name")
                .role(Role::Heading)
                .aria_level(3)
                .aria_label(title.clone())
                .text_size(FontSize::Medium)
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(title.clone())
                .into_any_element(),
        };
        div()
            .id(SharedString::from(format!("paired-machine:{}", machine.id)))
            .py(px(12.))
            .flex()
            .items_center()
            .gap(px(16.))
            .border_b_1()
            .border_color(theme.control_border.opacity(0.6))
            .child(svg().flex_none().path(MONITOR_ICON).size(px(16.)).text_color(theme.foreground.opacity(0.6)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(name)
                    .children(host_name.map(|host_name| {
                        div()
                            .id("host-name")
                            .role(Role::Label)
                            .aria_label(host_name.clone())
                            .text_size(FontSize::Body)
                            .text_color(theme.foreground.opacity(0.55))
                            .child(host_name)
                    }))
                    .child(
                        div()
                            .id("status")
                            .role(Role::Status)
                            .aria_label(text.clone())
                            .text_size(FontSize::Body)
                            .text_color(if matches!(status, Status::Stopped { .. }) {
                                theme.destructive
                            } else {
                                theme.foreground.opacity(0.55)
                            })
                            .child(text),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .children(remote_desktop)
                    .children(rename)
                    .children(reconnect)
                    .children(disconnect)
                    .child(forget),
            )
    }

    /// 把电脑 `id` 的名字换成输入框，现在的显示名全选；回车改名，Esc 不改。
    fn start_machine_rename(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let host = HostId::Paired(id.clone());
        let text = remote_hosts::display_name(&host, cx);
        let select = text.len();
        let field = cx.new(|cx| {
            InputState::editing(text, select, cx).with_label(rust_i18n::t!("workspace.edit_host_name").into_owned())
        });
        let events = cx.subscribe_in(&field, window, move |this, field, event: &InputEvent, window, cx| match event {
            InputEvent::Next => {
                let name = field.read(cx).query().to_owned();
                remote_hosts::set_display_name(&host, &name, cx);
                this.finish_machine_rename(window, cx);
            }
            InputEvent::Dismiss => this.finish_machine_rename(window, cx),
            InputEvent::Changed(_) | InputEvent::Previous => {}
        });
        window.focus(&field.focus_handle(cx), cx);
        self.renaming_machine = Some(Renaming { id, field, _events: events });
        cx.notify();
    }

    fn finish_machine_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming_machine.take().is_some() {
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }
}

/// 一台电脑的连接状态；连上的那台装的是另一个版本时说明文件和 Git 用不了。
fn status_line(status: &Status) -> String {
    match status {
        Status::Connected(connected) if connected.mismatch => {
            rust_i18n::t!("paired.build_mismatch", build = connected.build.as_str()).into_owned()
        }
        status => remote_hosts::status_text(status),
    }
}
