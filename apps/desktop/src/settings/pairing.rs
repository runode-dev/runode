//! 远程访问那一页的配对手机：一行标题和按钮，按下去切到窗口里的手机端引导页的配对那一步
//! （`window::show_pairing`），二维码、电脑名、网络和配好后的提醒都在那里。

use gpui::{Context, Div, prelude::*};

use super::{
    SettingsView,
    controls::{Colors, button, on_click, row},
};

impl SettingsView {
    pub(super) fn render_pairing(&mut self, colors: Colors, cx: &mut Context<Self>) -> Div {
        // 等这一轮更新结束再打开引导页，不在设置页处理点击的当中去改窗口。
        let pair = button("pair", rust_i18n::t!("settings.pairing.start").into_owned(), colors)
            .on_click(on_click(cx, |_, _, cx| cx.defer(crate::window::show_pairing)));
        let title = rust_i18n::t!("settings.pairing.title").into_owned();
        row(title, Some(rust_i18n::t!("settings.pairing.hint").into_owned().into()), pair, None, None, colors)
    }
}
