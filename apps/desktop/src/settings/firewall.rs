//! 远程访问页上 Windows 防火墙挡着局域网连接时的提示（`remote_access::firewall`）：手机在同一个网络里也只能
//! 经中继连过来。给一个按钮，要管理员确认后放行。别的系统上不查，这一行不出现。

use crate::ui::theme::Theme;
#[cfg(windows)]
use gpui::AppContext as _;
use gpui::{Context, Div, Task};

use super::{
    SettingsView,
    controls::{Press, row},
};
use crate::ui::a11y::Disable as _;
use crate::ui::button::{Button, ButtonVariant};

const ERROR_ID: &str = "firewall";

#[derive(Default)]
pub(super) struct State {
    /// 上次查到防火墙挡着；还没查完、查不出来时为假。
    blocked: bool,
    /// 在后台查或放行；跑着时按钮不可用。
    job: Option<Task<()>>,
    /// `job` 是放行：它跑完自己会再查，这期间不另起查询把它换掉。
    #[cfg(windows)]
    allowing: bool,
}

impl State {
    pub(super) fn blocked(&self) -> bool {
        self.blocked
    }
}

impl SettingsView {
    pub(super) fn render_firewall(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let busy = self.firewall.job.is_some();
        let allow = Button::new("firewall-allow", rust_i18n::t!("settings.firewall.allow").into_owned())
            .variant(ButtonVariant::Outline)
            .aria_disabled(busy)
            .on_press(cx, |this, _, cx| this.allow_firewall(cx));
        row(
            rust_i18n::t!("settings.firewall.title").into_owned(),
            Some(rust_i18n::t!("settings.firewall.hint").into_owned().into()),
            allow,
            None,
            self.errors.get(ERROR_ID).cloned(),
            theme,
        )
    }

    /// 在后台查防火墙挡没挡着这个程序在远程访问端口上的连接：要起 PowerShell 读规则表，不能卡着界面。开设置页时查一次，放行后再查。
    pub(super) fn refresh_firewall(&mut self, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            if self.firewall.allowing {
                return;
            }
            let Ok(exe) = std::env::current_exe() else { return };
            let port = self.config.remote_access_port;
            let job = cx.background_spawn(async move { crate::remote_access::firewall::inbound(&exe, port) });
            self.firewall.job = Some(cx.spawn(async move |this, cx| {
                let inbound = job.await;
                this.update(cx, |this, cx| {
                    this.firewall.job = None;
                    this.firewall.blocked = inbound == Some(crate::remote_access::firewall::Inbound::Blocked);
                    cx.notify();
                })
                .ok();
            }));
        }
        #[cfg(not(windows))]
        let _ = cx;
    }

    /// 弹管理员确认框放行，跑完再查一遍；拒绝了就照旧提示，不记错。
    fn allow_firewall(&mut self, cx: &mut Context<Self>) {
        #[cfg(windows)]
        {
            if self.firewall.job.is_some() {
                return;
            }
            let Ok(exe) = std::env::current_exe() else { return };
            self.errors.remove(ERROR_ID);
            let job = cx.background_spawn(async move { crate::remote_access::firewall::allow(&exe) });
            self.firewall.allowing = true;
            self.firewall.job = Some(cx.spawn(async move |this, cx| {
                let result = job.await;
                this.update(cx, |this, cx| {
                    this.firewall.job = None;
                    this.firewall.allowing = false;
                    match result {
                        Ok(()) => this.refresh_firewall(cx),
                        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {}
                        Err(err) => {
                            let message = rust_i18n::t!("settings.firewall.failed", err = err.to_string()).into_owned();
                            this.errors.insert(ERROR_ID.into(), message);
                        }
                    }
                    cx.notify();
                })
                .ok();
            }));
            cx.notify();
        }
        #[cfg(not(windows))]
        let _ = cx;
    }
}
