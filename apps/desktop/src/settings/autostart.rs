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
        let mut rows = gpui::div().flex().flex_col();
        for kind in KINDS.into_iter().filter(|kind| kind.supported()) {
            let state = self.autostart.get(kind.name()).copied();
            let on = state.unwrap_or(false);
            let title = rust_i18n::t!(format!("settings.autostart.{}.title", kind.name())).into_owned();
            // 还没查到装没装时也不能点：不知道点下去是装还是卸。
            let busy = state.is_none() || self.autostart_jobs.contains_key(kind.name());
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

    /// 在后台查各自启装没装，查到了再画：要看服务文件、问 SMAppService，不能每画一次都在界面线程上查。
    /// 开设置页时查一次，之后只在这里装卸时跟着更新；开着设置页时用命令行装卸的，重开设置页才看得到。
    pub(super) fn refresh_autostart(&mut self, cx: &mut Context<Self>) {
        let job = cx.background_spawn(async move {
            let dirs = runode_paths::Dirs::from_env();
            KINDS
                .into_iter()
                .filter(|kind| kind.supported())
                .map(|kind| (kind.name(), runode_autostart::is_installed(kind, &dirs)))
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let states = job.await;
            this.update(cx, |this, cx| {
                this.autostart.extend(states);
                cx.notify();
            })
            .ok();
        })
        .detach();
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
            let result = if runode_autostart::is_installed(kind, &dirs) {
                runode_autostart::uninstall(kind, &dirs).map(drop)
            } else {
                std::env::current_exe()
                    .and_then(std::fs::canonicalize)
                    .and_then(|exe| runode_autostart::install(kind, &dirs, &exe))
                    .map(drop)
            };
            (result, runode_autostart::is_installed(kind, &dirs))
        });
        let task = cx.spawn(async move |this, cx| {
            let (result, installed) = job.await;
            this.update(cx, |this, cx| {
                this.autostart_jobs.remove(kind.name());
                this.autostart.insert(kind.name(), installed);
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

const KINDS: [Kind; 2] = [Kind::App, Kind::Host];

fn error_id(kind: Kind) -> String {
    format!("autostart-{}", kind.name())
}
