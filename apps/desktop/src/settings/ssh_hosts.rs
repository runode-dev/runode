//! SSH 页的主机列表：连过的每台主机一行，写着连接状态，可以连接、断开、忘掉。主机是这次运行里连过的
//! （`host_client::remote::remotes`），不进配置文件；还有工作区、终端用着的主机不能忘掉。宿主会跟着 ssh 的
//! 登录被结束的主机，行里另有一句提示和「装成登录服务」；连着的主机能清理远端用不着的 runode 构建。

use crate::ui::theme::Theme;
use gpui::{Context, Div, Role, SharedString, div, prelude::*, px};

use super::{SettingsView, controls::Press as _};
use crate::{
    host_client::remote::{self, RemoteHost, Status},
    ssh_hosts::{self, PruneJob, ServiceJob},
    ui::{
        button::{Button, ButtonVariant},
        font_size::FontSize,
    },
};

impl SettingsView {
    pub(super) fn render_ssh_hosts(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let hosts = remote::remotes();
        let rows = div().flex().flex_col();
        if hosts.is_empty() {
            let text = rust_i18n::t!("settings.ssh_hosts.empty").into_owned();
            return rows.child(
                div()
                    .id("ssh-hosts-empty")
                    .role(Role::Label)
                    .aria_label(text.clone())
                    .py(px(12.))
                    .text_size(FontSize::Body)
                    .text_color(theme.foreground.opacity(0.55))
                    .child(text),
            );
        }
        let in_use = (self.hosts_in_use)(cx);
        rows.children(hosts.iter().map(|host| self.render_ssh_host(host, in_use.contains(&host.name), theme, cx)))
    }

    fn render_ssh_host(
        &self,
        host: &RemoteHost,
        in_use: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let name = host.name.clone();
        let title = ssh_hosts::display_name(&name, cx).to_owned();
        // 起了显示名的主机，名字下面另写连接目标。
        let destination = ssh_hosts::has_display_name(&name, cx).then(|| name.clone());
        let status = host.status();
        let text = status_line(&status);
        // 各行的元素 id 挂在这一行带主机名的 id 下面，主机名拼不出别的行的 id。
        let button = |key: &str, op: &'static str| {
            let label = rust_i18n::t!(key).into_owned();
            let named = rust_i18n::t!("settings.ssh_hosts.named", action = label.as_str(), host = title.as_str());
            Button::new(op, named.into_owned()).variant(ButtonVariant::Secondary).child(label)
        };
        let reconnect = matches!(status, Status::Idle | Status::Stopped { .. } | Status::Waiting { .. }).then(|| {
            let key =
                if matches!(status, Status::Waiting { .. }) { "ssh.reconnect" } else { "settings.ssh_hosts.connect" };
            let name = name.clone();
            button(key, "connect").on_press(cx, move |_, _, _| {
                if let Some(host) = remote::find(&name) {
                    host.retry_now();
                }
            })
        });
        let disconnect =
            matches!(status, Status::Connecting(_) | Status::Connected(_) | Status::Waiting { .. }).then(|| {
                let name = name.clone();
                button("settings.ssh_hosts.disconnect", "disconnect")
                    .on_press(cx, move |_, _, cx| ssh_hosts::disconnect(&name, cx))
            });
        let remote_desktop = (crate::screen_viewer::SUPPORTED
            && matches!(status, Status::Connected(_))
            && ssh_hosts::screen_capturable(&name, cx))
        .then(|| {
            let name = name.clone();
            button("ssh.remote_desktop", "remote-desktop")
                .on_press(cx, move |_, _, cx| crate::window::show_remote_desktop(name.clone(), cx))
        });
        let bound = matches!(&status, Status::Connected(connected) if connected.session_bound);
        let job = ssh_hosts::service_job(&name, cx);
        let prune_job = ssh_hosts::prune_job(&name, cx);
        // 提示和它要不要用醒目的颜色。
        let notice = match (&job, &prune_job) {
            (Some(job), _) => {
                Some((ssh_hosts::service_job_text(job), !matches!(job, ServiceJob::Done { moved: true })))
            }
            (None, Some(prune_job)) => {
                Some((ssh_hosts::prune_job_text(prune_job), matches!(prune_job, PruneJob::Failed(_))))
            }
            (None, None) => bound.then(|| (rust_i18n::t!("settings.ssh_hosts.bound").into_owned(), true)),
        };
        let prune = matches!(status, Status::Connected(_)).then(|| {
            let button = button("settings.ssh_hosts.prune", "prune");
            if matches!(prune_job, Some(PruneJob::Running)) {
                return button.disabled(true);
            }
            let name = name.clone();
            button.on_press(cx, move |_, _, cx| ssh_hosts::prune_builds(&name, cx))
        });
        let install = (bound && !matches!(job, Some(ServiceJob::Done { .. }))).then(|| {
            let button = button("settings.ssh_hosts.install_service", "install-service");
            if matches!(job, Some(ServiceJob::Running)) {
                return button.disabled(true);
            }
            let name = name.clone();
            button.on_press(cx, move |_, _, cx| ssh_hosts::install_service(&name, false, cx))
        });
        let forget = button("settings.ssh_hosts.forget", "forget");
        let forget = if in_use {
            let why = rust_i18n::t!("settings.ssh_hosts.in_use").into_owned();
            forget.disabled(true).tooltip(crate::ui::tooltip::tooltip(why, None))
        } else {
            let name = name.clone();
            forget.on_press(cx, move |_, _, cx| ssh_hosts::forget(&name, cx))
        };
        div()
            .id(SharedString::from(format!("ssh-host:{name}")))
            .py(px(12.))
            .flex()
            .items_center()
            .gap(px(16.))
            .border_b_1()
            .border_color(theme.control_border.opacity(0.6))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .id("name")
                            .role(Role::Heading)
                            .aria_level(3)
                            .aria_label(title.clone())
                            .text_size(FontSize::Medium)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(title.clone()),
                    )
                    .children(destination.map(|destination| {
                        div()
                            .id("destination")
                            .role(Role::Label)
                            .aria_label(destination.clone())
                            .text_size(FontSize::Body)
                            .text_color(theme.foreground.opacity(0.55))
                            .child(destination)
                    }))
                    .child(
                        div()
                            .id("status")
                            .role(Role::Label)
                            .aria_label(text.clone())
                            .text_size(FontSize::Body)
                            .text_color(theme.foreground.opacity(0.55))
                            .child(text),
                    )
                    .children(notice.map(|(notice, warn)| {
                        div()
                            .id("notice")
                            .role(Role::Label)
                            .aria_label(notice.clone())
                            .text_size(FontSize::Body)
                            .text_color(if warn { theme.destructive } else { theme.foreground.opacity(0.55) })
                            .child(notice)
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .children(install)
                    .children(prune)
                    .children(remote_desktop)
                    .children(reconnect)
                    .children(disconnect)
                    .child(forget),
            )
    }
}

/// 一台主机的连接状态；连上的远端是另一个构建时带上它的构建。
fn status_line(status: &Status) -> String {
    match status {
        Status::Connected(connected) if connected.mismatch => {
            rust_i18n::t!("settings.ssh_hosts.mismatch", build = connected.build.as_str()).into_owned()
        }
        status => ssh_hosts::status_text(status),
    }
}
