//! 终端里的语音输入：`Dictate` 开始录音；按住超过 `HOLD` 再松开就结束，点一下就一直录到再按一次。
//! 结束后在后台识别，文字当作粘贴打进终端，不会自己执行。录音时按 Esc 放弃。

use std::time::{Duration, Instant};

use gpui::{
    Context, KeyUpEvent, Modifiers, ModifiersChangedEvent, PromptLevel, Role, Stateful, Task, Window, div, prelude::*,
    px,
};
use runode_speech::Recorder;
use runode_types::color::Rgb;

use super::{Dictate, TerminalView};
use crate::{
    menus::OpenSettings,
    ui::{font_size::FontSize, hsla},
};

/// 按下到松开不到这么久算点了一下，之后一直录，再按一次才结束。
const HOLD: Duration = Duration::from_millis(350);

pub(super) enum Dictation {
    Recording {
        /// 麦克风还在开时是 `None`：蓝牙耳机要先切到通话模式，可能要一两秒，不在界面线程上等。
        recorder: Option<Recorder>,
        _opening: Task<()>,
        /// 麦克风开好之前就该结束了，开好就结束。
        finish_when_open: bool,
        started: Instant,
        /// 按下快捷键时按着的修饰键，松开其中任何一个都算松开了快捷键：macOS 按着 ⌘ 时不发别的键的松开。
        modifiers: Modifiers,
        /// 快捷键还按着；它按住不放时系统重复发的按下不算再按一次。
        held: bool,
    },
    Transcribing {
        _task: Task<()>,
    },
}

impl TerminalView {
    pub(super) fn dictate(&mut self, _: &Dictate, window: &mut Window, cx: &mut Context<Self>) {
        match &self.dictation {
            None => self.start_dictation(window, cx),
            Some(Dictation::Recording { held: false, .. }) => self.end_recording(window, cx),
            Some(_) => {}
        }
        cx.notify();
    }

    fn start_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.config.dictation_model;
        if !crate::dictation::Settings::new(&self.config).available() {
            let size = crate::dictation::format_size(runode_speech::download_size(model));
            let detail = rust_i18n::t!("dictation.no_model.detail", size = size);
            let answer = window.prompt(
                PromptLevel::Info,
                &rust_i18n::t!("dictation.no_model.title"),
                Some(&detail),
                &[&*rust_i18n::t!("dictation.no_model.open_settings"), &*rust_i18n::t!("dictation.cancel")],
                cx,
            );
            cx.spawn_in(window, async move |_, cx| {
                if answer.await.ok() == Some(0) {
                    cx.update(|window, cx| window.dispatch_action(Box::new(OpenSettings), cx)).ok();
                }
            })
            .detach();
            return;
        }
        let open = cx.background_spawn(async { Recorder::start() });
        let opening = cx.spawn_in(window, async move |this, cx| {
            let opened = open.await;
            this.update_in(cx, |view, window, cx| {
                match opened {
                    Ok(opened) => {
                        if let Some(Dictation::Recording { recorder, finish_when_open, .. }) = &mut view.dictation {
                            *recorder = Some(opened);
                            if *finish_when_open {
                                view.end_recording(window, cx);
                            }
                        }
                    }
                    Err(err) => {
                        view.dictation = None;
                        report(&err, window, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.dictation = Some(Dictation::Recording {
            recorder: None,
            _opening: opening,
            finish_when_open: false,
            started: Instant::now(),
            modifiers: window.modifiers(),
            held: true,
        });
    }

    fn end_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Dictation::Recording { recorder, finish_when_open, .. }) = &mut self.dictation {
            match recorder.take() {
                Some(recorder) => self.finish_dictation(recorder, window, cx),
                None => *finish_when_open = true,
            }
        }
    }

    fn finish_dictation(&mut self, recorder: Recorder, window: &mut Window, cx: &mut Context<Self>) {
        let job = crate::dictation::transcribe(crate::dictation::Settings::new(&self.config), recorder, cx);
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |view, window, cx| {
                view.dictation = None;
                match result {
                    Ok(text) if !text.is_empty() => view.paste_text(text, window, cx),
                    Ok(_) => {}
                    Err(err) => report(&err, window, cx),
                }
                cx.notify();
            })
            .ok();
        });
        self.dictation = Some(Dictation::Transcribing { _task: task });
    }

    pub(super) fn dictation_key_up(&mut self, _: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.dictation_released(window, cx);
    }

    pub(super) fn dictation_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(Dictation::Recording { modifiers, .. }) = &self.dictation
            && !modifiers.is_subset_of(&event.modifiers)
        {
            self.dictation_released(window, cx);
        }
    }

    fn dictation_released(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dictation::Recording { started, held, .. }) = &mut self.dictation else {
            return;
        };
        if !*held {
            return;
        }
        *held = false;
        if started.elapsed() >= HOLD {
            self.end_recording(window, cx);
        }
        cx.notify();
    }

    /// 录音时按了 Esc：丢掉录到的音。返回是否吃掉了这个键。
    pub(super) fn cancel_dictation(&mut self, cx: &mut Context<Self>) -> bool {
        if !matches!(self.dictation, Some(Dictation::Recording { .. })) {
            return false;
        }
        if let Some(Dictation::Recording { recorder: Some(recorder), .. }) = self.dictation.take() {
            cx.background_spawn(async move { drop(recorder.stop()) }).detach();
        }
        cx.notify();
        true
    }

    /// 终端底部正中的提示：正在听、正在识别。
    pub(super) fn render_dictation(&self, foreground: Rgb, background: Rgb) -> Option<Stateful<gpui::Div>> {
        let (message, recording) = match self.dictation.as_ref()? {
            Dictation::Recording { recorder, .. } => (rust_i18n::t!("dictation.listening"), recorder.is_some()),
            Dictation::Transcribing { .. } => (rust_i18n::t!("dictation.transcribing"), false),
        };
        let fg = hsla(foreground);
        let badge = div()
            .px(px(10.))
            .py(px(3.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(4.))
            .bg(hsla(background.mix(foreground, 0.1)))
            .border_1()
            .border_color(fg.opacity(0.15))
            .text_size(FontSize::Small)
            .text_color(fg.opacity(0.9))
            .when(recording, |badge| badge.child(div().size(px(8.)).rounded_full().bg(gpui::red())))
            .child(message.clone().into_owned());
        Some(
            div()
                .id("dictation")
                .role(Role::Label)
                .aria_label(message.into_owned())
                .absolute()
                .bottom(px(6.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(badge),
        )
    }
}

fn report(err: &anyhow::Error, window: &mut Window, cx: &mut Context<TerminalView>) {
    let detail = if err.is::<runode_speech::Silent>() {
        rust_i18n::t!("dictation.silent").into_owned()
    } else {
        format!("{err:#}")
    };
    // 只有一个「好」，不等回答。
    drop(window.prompt(
        PromptLevel::Warning,
        &rust_i18n::t!("dictation.failed"),
        Some(&detail),
        &[&*rust_i18n::t!("dictation.ok")],
        cx,
    ));
}
