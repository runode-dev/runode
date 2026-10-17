//! 通用页的登录时自启：两个开关，一个是 app 自己（只有 macOS），一个是后台的无界面宿主
//! （`runode --host`，手机远程访问靠它）。开关的状态就是服务文件在不在、登录项登没登记（`runode_autostart`），
//! 和命令行的 `runode service` 是同一套，所以不进配置文件。

use crate::ui::theme::Theme;
use gpui::{AppContext as _, Context, Div, prelude::*};
use runode_autostart::Kind;

use super::{
    SettingsView,
    controls::{Press, row},
};
use crate::ui::{a11y::Disable as _, switch::Switch};

impl SettingsView {
    pub(super) fn render_autostart(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let dirs = runode_paths::Dirs::from_env();
        let mut rows = gpui::div().flex().flex_col();
        for kind in [Kind::App, Kind::Host].into_iter().filter(|kind| kind.supported()) {
            let on = runode_autostart::is_installed(kind, &dirs);
            let title = rust_i18n::t!(format!("settings.autostart.{}.title", kind.name())).into_owned();
            let busy = self.autostart_jobs.contains_key(kind.name());
            let switch = Switch::new(format!("autostart-{}", kind.name()), title.clone(), on);
            let switch = if busy {
                switch.opacity(0.5).aria_disabled(true)
            } else {
                switch.on_press(cx, move |this, _, cx| this.toggle_autostart(kind, cx))
            };
            rows = rows.child(row(
                title,
                Some(rust_i18n::t!(format!("settings.autostart.{}.hint", kind.name())).into_owned().into()),
                switch,
                None,
                self.errors.get(&error_id(kind)).cloned(),
                theme,
            ));
        }
        rows
    }

    /// 在后台装上或去掉 `kind` 的自启，失败的原因记在那一行下面。要起 launchctl、systemctl 或走
    /// SMAppService，用户的 systemd 连不上时还要重试好几秒，不能卡着界面；跑着时开关不可用，连点不会排出
    /// 一装一卸两个任务。
    fn toggle_autostart(&mut self, kind: Kind, cx: &mut Context<Self>) {
        if self.autostart_jobs.contains_key(kind.name()) {
            return;
        }
        let job = cx.background_spawn(async move {
            let dirs = runode_paths::Dirs::from_env();
            if runode_autostart::is_installed(kind, &dirs) {
                runode_autostart::uninstall(kind, &dirs).map(drop)
            } else {
                std::env::current_exe()
                    .and_then(std::fs::canonicalize)
                    .and_then(|exe| runode_autostart::install(kind, &dirs, &exe))
                    .map(drop)
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.autostart_jobs.remove(kind.name());
                match result {
                    Ok(()) => drop(this.errors.remove(&error_id(kind))),
                    Err(err) => {
                        let message = rust_i18n::t!("settings.autostart.failed", err = err.to_string()).into_owned();
                        this.errors.insert(error_id(kind), message);
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.autostart_jobs.insert(kind.name(), task);
        cx.notify();
    }
}

fn error_id(kind: Kind) -> String {
    format!("autostart-{}", kind.name())
}
