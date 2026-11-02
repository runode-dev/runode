//! ssh 要问用户时（`RemoteEvent::Ask`）在窗口里弹的对话框：密码、口令和验证码用遮着的输入框，认不认主机
//! 指纹这类是否的问题给两个按钮，请触碰安全密钥这类只是告知的提示等 ssh 办完自己收起。一次只显示一个，
//! 后来的排着队。

use crate::ui::dialog::{backdrop, is_dismiss_key};
use std::collections::VecDeque;

use gpui::{
    Context, Div, Entity, FocusHandle, Focusable, FontWeight, KeyDownEvent, Role, Stateful, Subscription, Window, div,
    prelude::*, px,
};
use runode_ssh::askpass::Kind;

use super::WindowView;
use crate::{
    host_client::remote::Ask,
    ui::{
        a11y::Press,
        button::{Button, ButtonVariant},
        dialog::Dialog,
        font_size::FontSize,
        input::{Input, InputEvent, InputState},
        theme::ActiveTheme as _,
    },
};

/// 排着队的一个询问。
pub(super) struct AskDialog {
    id: u64,
    ask: Ask,
    focus: FocusHandle,
    /// 要输入的（`Kind::Secret`）才有。
    field: Option<(Entity<InputState>, Subscription)>,
}

/// 窗口里排着的询问，最前面的那个显示着。
pub(super) type Asks = VecDeque<AskDialog>;

impl WindowView {
    /// 把 ssh 的询问排进这个窗口；排在最前面时把焦点给它。
    pub(crate) fn show_ask(&mut self, mut ask: Ask, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_id();
        let field = (ask.kind == Kind::Secret).then(|| {
            let field = cx.new(|cx| InputState::new(String::new(), cx).with_masked().with_label(ask.prompt.clone()));
            let events =
                cx.subscribe_in(&field, window, move |this, field, event: &InputEvent, window, cx| match event {
                    InputEvent::Next => {
                        let text = field.read(cx).query().to_owned();
                        this.answer_ask(id, Some(text), window, cx);
                    }
                    InputEvent::Dismiss => this.answer_ask(id, None, window, cx),
                    InputEvent::Changed(_) | InputEvent::Previous => {}
                });
            (field, events)
        });
        // 只是告知的提示，ssh 办完（用户触碰了安全密钥）就收起。
        if let Some(dismissed) = ask.dismissed.take() {
            cx.spawn_in(window, async move |this, cx| {
                let _ = dismissed.await;
                this.update_in(cx, |this, window, cx| this.answer_ask(id, None, window, cx)).ok();
            })
            .detach();
        }
        self.asks.push_back(AskDialog { id, ask, focus: cx.focus_handle(), field });
        if self.asks.len() == 1 {
            self.focus_ask(window, cx);
        }
        cx.notify();
    }

    fn focus_ask(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.asks.front() else {
            return;
        };
        match &dialog.field {
            Some((field, _)) => window.focus(&field.focus_handle(cx), cx),
            None => window.focus(&dialog.focus, cx),
        }
    }

    /// 回答询问 `id`（`None` 是取消），收起它，焦点给下一个，没有了就还给窗口。
    fn answer_ask(&mut self, id: u64, reply: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.asks.iter().position(|dialog| dialog.id == id) else {
            return;
        };
        let front = ix == 0;
        if let Some(dialog) = self.asks.remove(ix) {
            dialog.ask.answer(reply);
        }
        if front {
            if self.asks.is_empty() {
                window.focus(&self.focus_handle(cx), cx);
            } else {
                self.focus_ask(window, cx);
            }
        }
        cx.notify();
    }

    fn ask_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if is_dismiss_key(&event.keystroke) {
            cx.stop_propagation();
            if let Some(id) = self.asks.front().map(|dialog| dialog.id) {
                self.answer_ask(id, None, window, cx);
            }
        }
    }

    pub(super) fn render_ask(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let dialog = self.asks.front()?;
        let id = dialog.id;
        let fg = cx.theme().foreground;
        let title = rust_i18n::t!("ssh.ask_title", host = dialog.ask.host.as_str()).into_owned();
        // ssh 的提示常是好几行（主机指纹），照原样一行一行画。
        let prompt = div()
            .id("ssh-ask-prompt")
            .role(Role::Label)
            .aria_label(dialog.ask.prompt.clone())
            .flex()
            .flex_col()
            .gap(px(2.))
            .text_color(fg.opacity(0.8))
            .children(dialog.ask.prompt.lines().map(|line| div().child(line.to_owned())));
        let button = |key: &'static str, label: String, variant: ButtonVariant| {
            Button::new(key, label).variant(variant).h_auto().py(px(5.)).px(px(12.)).cursor_pointer()
        };
        let buttons = match dialog.ask.kind {
            Kind::Secret => {
                let field = dialog.field.as_ref().map(|(field, _)| field.clone());
                vec![
                    button("ssh-ask-cancel", rust_i18n::t!("ssh.ask_cancel").into_owned(), ButtonVariant::Secondary)
                        .on_press(cx, move |this, window, cx| this.answer_ask(id, None, window, cx)),
                    button("ssh-ask-continue", rust_i18n::t!("ssh.ask_continue").into_owned(), ButtonVariant::Default)
                        .on_press(cx, move |this, window, cx| {
                            let text = field.as_ref().map(|field| field.read(cx).query().to_owned());
                            this.answer_ask(id, text, window, cx);
                        }),
                ]
            }
            // 认不认主机指纹不设默认按钮：回车不替用户答「是」。
            Kind::Confirm => vec![
                button("ssh-ask-no", rust_i18n::t!("ssh.ask_no").into_owned(), ButtonVariant::Secondary)
                    .on_press(cx, move |this, window, cx| this.answer_ask(id, None, window, cx)),
                button("ssh-ask-yes", rust_i18n::t!("ssh.ask_yes").into_owned(), ButtonVariant::Secondary)
                    .on_press(cx, move |this, window, cx| this.answer_ask(id, Some("yes".into()), window, cx)),
            ],
            Kind::Notice => vec![
                button("ssh-ask-cancel", rust_i18n::t!("ssh.ask_cancel").into_owned(), ButtonVariant::Secondary)
                    .on_press(cx, move |this, window, cx| this.answer_ask(id, None, window, cx)),
            ],
        };
        let panel = Dialog::new("ssh-ask-dialog", title.clone())
            .track_focus(&dialog.focus)
            .on_key_down(cx.listener(Self::ask_key))
            .w(px(460.))
            .max_w_full()
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .id("ssh-ask-title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(title.clone())
                    .text_size(FontSize::Title)
                    .font_weight(FontWeight::BOLD)
                    .child(title),
            )
            .child(prompt)
            .children(dialog.field.as_ref().map(|(field, _)| Input::new(field).bg(cx.theme().background)))
            .child(div().flex().justify_end().gap(px(8.)).children(buttons));
        // 铺满窗口的底子挡住下面的点击；点到外面不算取消，免得误点掉一次输密码。
        Some(backdrop("ssh-ask-backdrop").child(panel))
    }
}
