//! 远程访问那一页的配对手机：一行标题和按钮，等手机时下面是二维码和链接，配好后提醒
//! `terminal-host`。生成口令、等手机的过程见 `remote_access::pairing`；关设置窗口时口令随之丢掉。

use gpui::{ClipboardItem, Context, Div, SharedString, div, prelude::*, px};

use super::{
    SettingsView,
    controls::{Colors, button, on_click, row, switch},
};
use crate::remote_access::pairing::{BACKGROUND_KEY, Pairing, qr_code};

/// 二维码一个模块的边长，像素。
const MODULE_SIZE: f32 = 4.;

impl SettingsView {
    fn start_pairing(&mut self, cx: &mut Context<Self>) {
        self.pairing.start(|this: &mut Self| Some(&mut this.pairing), Vec::new(), cx);
    }

    pub(super) fn render_pairing(&mut self, colors: Colors, cx: &mut Context<Self>) -> Div {
        let title = rust_i18n::t!("settings.pairing.title").into_owned();
        let start = |label: &str, cx: &mut Context<Self>| {
            let enabled = self.config.remote_access;
            button("pair", rust_i18n::t!(label).into_owned(), colors)
                .when(!enabled, |button| button.opacity(0.5))
                .when(enabled, |button| button.on_click(on_click(cx, |this, _, cx| this.start_pairing(cx))))
        };
        let idle_hint = || {
            let key = if self.config.remote_access { "settings.pairing.hint" } else { "settings.pairing.off" };
            SharedString::from(rust_i18n::t!(key).into_owned())
        };
        let cancel = |cx: &mut Context<Self>| {
            button("pair-cancel", rust_i18n::t!("settings.pairing.cancel").into_owned(), colors).on_click(on_click(
                cx,
                |this, _, cx| {
                    this.pairing = Pairing::Idle;
                    cx.notify();
                },
            ))
        };
        match &self.pairing {
            Pairing::Idle => row(title, Some(idle_hint()), start("settings.pairing.start", cx), None, None, colors),
            Pairing::Failed(err) => {
                let err = err.clone();
                row(title, Some(idle_hint()), start("settings.pairing.start", cx), None, Some(err), colors)
            }
            Pairing::Starting { .. } => {
                let hint = rust_i18n::t!("settings.pairing.starting").into_owned();
                row(title, Some(hint.into()), cancel(cx), None, None, colors)
            }
            Pairing::Paired { name } => {
                let hint = rust_i18n::t!("settings.pairing.paired", name = name).into_owned();
                let on = self.config.values(BACKGROUND_KEY).first().is_some_and(|value| value == "true");
                let background = switch("pairing-background", on, colors).on_click(on_click(cx, move |this, _, cx| {
                    this.write_or_report(BACKGROUND_KEY, vec![(!on).to_string()], cx)
                }));
                let error = self.errors.get(BACKGROUND_KEY).cloned();
                div()
                    .flex()
                    .flex_col()
                    .child(row(title, Some(hint.into()), start("settings.pairing.again", cx), None, None, colors))
                    .child(row(
                        rust_i18n::t!("settings.key.terminal_host").into_owned(),
                        Some(rust_i18n::t!("settings.pairing.background").into_owned().into()),
                        background,
                        None,
                        error,
                        colors,
                    ))
            }
            Pairing::Waiting(waiting) => {
                let hint = rust_i18n::t!("settings.pairing.waiting", time = waiting.remaining()).into_owned();
                let uri = waiting.uri();
                let copy = button("pair-copy", rust_i18n::t!("settings.pairing.copy").into_owned(), colors).on_click(
                    on_click(cx, move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(uri.to_string()))),
                );
                let (qr, uri) = (waiting.qr(), waiting.uri());
                div().flex().flex_col().child(row(title, Some(hint.into()), cancel(cx), None, None, colors)).child(
                    div().py(px(14.)).flex().items_center().gap(px(16.)).child(qr_code(qr, MODULE_SIZE)).child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .items_start()
                            .gap(px(8.))
                            .child(
                                div()
                                    .w_full()
                                    .text_size(px(11.5))
                                    .text_color(colors.fg.opacity(0.55))
                                    .truncate()
                                    .child(uri),
                            )
                            .child(copy),
                    ),
                )
            }
        }
    }
}
