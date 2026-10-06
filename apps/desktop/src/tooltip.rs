//! 悬停提示：鼠标在按钮或标题上停一会儿弹出的一小块文字，有快捷键时跟在说明后面。

use gpui::{Action, AnyView, App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};
use runode_shared_types::color::Rgb;

use crate::terminal_view::hsla;

/// `action` 在键位表里的快捷键。后加的绑定优先，取最后一个，快捷键改了也跟着变。
pub fn shortcut_text(action: &dyn Action, cx: &App) -> Option<SharedString> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    keymap.bindings_for_action(action).next_back().map(|binding| {
        let strokes: Vec<_> = binding.keystrokes().iter().map(ToString::to_string).collect();
        SharedString::from(strokes.join(" "))
    })
}

/// 交给 `.tooltip()` 的构造函数：`text` 说明作用，`action` 绑了快捷键时在后面写上。
/// 快捷键等弹出时才查，查的是那一刻的键位表。
pub fn tooltip(
    text: impl Into<SharedString>,
    action: Option<&dyn Action>,
    fg: Rgb,
    bg: Rgb,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    let action = action.map(Action::boxed_clone);
    move |_, cx| {
        let shortcut = action.as_deref().and_then(|action| shortcut_text(action, cx));
        cx.new(|_| Tooltip { text: text.clone(), shortcut, fg, bg }).into()
    }
}

struct Tooltip {
    text: SharedString,
    shortcut: Option<SharedString>,
    fg: Rgb,
    bg: Rgb,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(4.))
            .flex()
            .gap(px(8.))
            .rounded(px(4.))
            .border_1()
            .border_color(hsla(self.fg).opacity(0.15))
            .bg(hsla(self.bg.mix(self.fg, 0.08)))
            .text_size(px(12.))
            .text_color(hsla(self.fg))
            .child(self.text.clone())
            .children(
                self.shortcut.clone().map(|shortcut| div().text_color(hsla(self.fg).opacity(0.5)).child(shortcut)),
            )
    }
}
