//! 通用页的登录时自启：两个开关，一个是 app 自己（只有 macOS），一个是后台的无界面宿主
//! （`runode --host`，手机远程访问靠它）。开关的状态就是服务文件在不在（`runode_autostart`），
//! 和命令行的 `runode service` 是同一套，所以不进配置文件。

use gpui::{Context, Div, prelude::*};
use runode_autostart::Kind;

use super::{
    SettingsView,
    controls::{Colors, Press, row, switch},
};

impl SettingsView {
    pub(super) fn render_autostart(&mut self, colors: Colors, cx: &mut Context<Self>) -> Div {
        let dirs = runode_paths::Dirs::from_env();
        let mut rows = gpui::div().flex().flex_col();
        for kind in [Kind::App, Kind::Host].into_iter().filter(|kind| kind.supported()) {
            let on = runode_autostart::is_installed(kind, &dirs);
            let title = rust_i18n::t!(format!("settings.autostart.{}.title", kind.name())).into_owned();
            let switch = switch(format!("autostart-{}", kind.name()), title.clone(), on, colors)
                .on_press(cx, move |this, _, cx| this.toggle_autostart(kind, cx));
            rows = rows.child(row(
                title,
                Some(rust_i18n::t!(format!("settings.autostart.{}.hint", kind.name())).into_owned().into()),
                switch,
                None,
                self.errors.get(&error_id(kind)).cloned(),
                colors,
            ));
        }
        rows
    }

    /// 装上或去掉 `kind` 的自启，失败的原因记在那一行下面。
    fn toggle_autostart(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let dirs = runode_paths::Dirs::from_env();
        let result = if runode_autostart::is_installed(kind, &dirs) {
            runode_autostart::uninstall(kind, &dirs).map(drop)
        } else {
            std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .and_then(|exe| runode_autostart::install(kind, &dirs, &exe))
                .map(drop)
        };
        match result {
            Ok(()) => drop(self.errors.remove(&error_id(kind))),
            Err(err) => {
                let message = rust_i18n::t!("settings.autostart.failed", err = err.to_string()).into_owned();
                self.errors.insert(error_id(kind), message);
            }
        }
        cx.notify();
    }
}

fn error_id(kind: Kind) -> String {
    format!("autostart-{}", kind.name())
}
