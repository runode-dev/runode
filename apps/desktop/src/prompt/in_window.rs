//! 画在窗口里的确认框，经 `App::set_prompt_builder` 交给 GPUI，`Window::prompt` 弹的就是它。用在没有原生
//! 确认框的平台上：GPUI 自带的那个白底、不跟主题，详细说明不换行、超出就截掉，也不认回车和 Esc。
//!
//! 样子照 `ui::dialog`。按钮从右往左按 `answers` 的顺序排，第一个是主按钮，回车按它；Esc 按
//! `PromptButton::Cancel` 的那个，没有时不起作用。

use gpui::{
    App, AppContext as _, Axis, Context, EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent, PromptButton,
    PromptHandle, PromptLevel, PromptResponse, Render, RenderablePromptHandle, Role, ScrollHandle, SharedString,
    Window, div, prelude::*, px,
};

use crate::ui::{
    a11y::{Press as _, StaticText as _},
    button::{Button, ButtonVariant},
    dialog::{Dialog, backdrop, is_dismiss_key},
    font_size::FontSize,
    scrollbar::scrollbar,
    theme::ActiveTheme as _,
};

/// 交给 `App::set_prompt_builder` 的构造函数：在窗口里画对话框，`message` 和 `detail` 是正文，`answers` 是按钮，
/// 不按 `level` 区分样子。
pub(super) fn build(
    _level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    answers: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let view = cx.new(|cx| InWindowPrompt {
        message: message.to_owned().into(),
        detail: detail.map(|detail| detail.to_owned().into()),
        answers: answers.to_vec(),
        focus: cx.focus_handle(),
        scroll: ScrollHandle::new(),
    });
    handle.with_view(view, window, cx)
}

struct InWindowPrompt {
    message: SharedString,
    detail: Option<SharedString>,
    answers: Vec<PromptButton>,
    focus: FocusHandle,
    /// 详细说明的滚动位置。
    scroll: ScrollHandle,
}

impl InWindowPrompt {
    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let answer = if is_dismiss_key(keystroke) {
            self.answers.iter().position(PromptButton::is_cancel)
        } else if keystroke.key == "enter" && !keystroke.modifiers.modified() {
            (!self.answers.is_empty()).then_some(0)
        } else {
            None
        };
        if let Some(ix) = answer {
            cx.stop_propagation();
            cx.emit(PromptResponse(ix));
        }
    }
}

impl EventEmitter<PromptResponse> for InWindowPrompt {}

impl Focusable for InWindowPrompt {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for InWindowPrompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fg = cx.theme().foreground;
        let buttons: Vec<_> = self
            .answers
            .iter()
            .enumerate()
            .rev()
            .map(|(ix, answer)| {
                let variant = if ix == 0 { ButtonVariant::Default } else { ButtonVariant::Secondary };
                Button::new(("prompt-answer", ix), answer.label().clone())
                    .variant(variant)
                    .h_auto()
                    .py(px(5.))
                    .px(px(12.))
                    .cursor_pointer()
                    .on_press(cx, move |_, _, cx| cx.emit(PromptResponse(ix)))
            })
            .collect();
        // 详细说明可能分好几段，一行一行画，每行放不下时折行。长的（命令的输出、出错的原因）窗口里放不下时
        // 只有这一块缩短、在里面滚，标题和按钮总在框里。
        let detail = self.detail.clone().map(|detail| {
            div()
                .relative()
                .flex()
                .flex_col()
                .min_h_0()
                .child(
                    div()
                        .id("prompt-detail")
                        .aria_static_text(detail.clone())
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .text_color(fg.opacity(0.8))
                        .children(detail.lines().map(|line| div().child(line.to_owned())).collect::<Vec<_>>()),
                )
                .child(scrollbar("prompt-detail-scrollbar", self.scroll.clone(), Axis::Vertical))
        });
        let panel = Dialog::new("prompt-dialog", self.message.clone())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .w(px(420.))
            .max_w_full()
            .max_h_full()
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .id("prompt-message")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(self.message.clone())
                    .flex_none()
                    .text_size(FontSize::Title)
                    .font_weight(FontWeight::BOLD)
                    .child(self.message.clone()),
            )
            .children(detail)
            .child(div().flex_none().flex().flex_wrap().justify_end().gap(px(8.)).children(buttons));
        // 点到外面不算答：和别的平台的确认框一样，非得按一个按钮。
        backdrop("prompt-backdrop").child(panel)
    }
}
