//! 语音输入页的模型文件一行：选中的模型下没下、下载进度，以及下载、取消和删除。

use crate::ui::theme::Theme;
use gpui::{AppContext as _, Context, Div};

use super::{
    SettingsView,
    controls::{Press as _, row},
};
use crate::{
    dictation::{self, Download, Downloads, format_size},
    ui::button::{Button, ButtonVariant},
};

impl SettingsView {
    pub(super) fn render_dictation(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let model = self.config.dictation_model;
        let size = runode_speech::download_size(model);
        let state = match cx.try_global::<Downloads>().and_then(|downloads| downloads.0.get(&model)) {
            Some(Download::Running { done, total, .. }) => Some(Ok((*done, *total))),
            Some(Download::Failed(err)) => Some(Err(err.clone())),
            None => None,
        };
        let (status, button, error) = match state {
            Some(Ok((done, total))) => {
                let percent = (done * 100).checked_div(total).unwrap_or(0).to_string();
                let status = rust_i18n::t!(
                    "settings.dictation.downloading",
                    percent = percent,
                    done = format_size(done),
                    total = format_size(total)
                );
                let cancel = Button::new("dictation-cancel", rust_i18n::t!("settings.dictation.cancel"))
                    .variant(ButtonVariant::Outline)
                    .on_press(cx, move |_, _, cx| dictation::cancel_download(model, cx));
                (status, cancel, None)
            }
            _ if dictation::is_installed(model) => {
                let status = rust_i18n::t!("settings.dictation.downloaded", size = format_size(size));
                let remove = Button::new("dictation-remove", rust_i18n::t!("settings.dictation.remove"))
                    .variant(ButtonVariant::Outline)
                    .on_press(cx, move |_, _, cx| {
                        // 正在识别时要等识别器放手，不在界面线程上等。
                        let removed = cx.background_spawn(async move { dictation::remove(model) });
                        cx.spawn(async move |this, cx| {
                            let result = removed.await;
                            this.update(cx, |this, cx| {
                                match result {
                                    Ok(()) => drop(this.errors.remove(ERROR_ID)),
                                    Err(err) => drop(this.errors.insert(ERROR_ID.into(), err.to_string())),
                                }
                                cx.notify();
                            })
                            .ok();
                        })
                        .detach();
                    });
                (status, remove, None)
            }
            failed => {
                let status = rust_i18n::t!("settings.dictation.not_downloaded", size = format_size(size));
                let download = Button::new("dictation-download", rust_i18n::t!("settings.dictation.download"))
                    .variant(ButtonVariant::Outline)
                    .on_press(cx, move |_, _, cx| dictation::start_download(model, cx));
                let error = match failed {
                    Some(Err(err)) => Some(rust_i18n::t!("settings.dictation.failed", err = err).into_owned()),
                    _ => None,
                };
                (status, download, error)
            }
        };
        let error = error.or_else(|| self.errors.get(ERROR_ID).cloned());
        row(
            rust_i18n::t!("settings.dictation.title").into_owned(),
            Some(status.into_owned().into()),
            button,
            None,
            error,
            theme,
        )
    }
}

const ERROR_ID: &str = "dictation-files";
