//! 悬停提示：鼠标在按钮或标题上停一会儿弹出的一小块文字，有快捷键时跟在说明后面。

use gpui::{Action, AnyView, App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};
use runode_shared_types::color::Rgb;

use crate::ui::hsla;

/// 说明再长也不比这宽，多了折行。
const TOOLTIP_MAX_WIDTH: f32 = 420.;
/// 代码样的长说明（`code_tooltip`）最宽、最高这么多，再高的截掉：悬停提示里滚动不了。
const CODE_TOOLTIP_MAX_WIDTH: f32 = 520.;
const CODE_TOOLTIP_MAX_HEIGHT: f32 = 420.;

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
        cx.new(|_| Tooltip { text: text.clone(), shortcut, font: None, fg, bg }).into()
    }
}

/// 交给 `.tooltip()` 的构造函数：一段多行的、代码样的说明（比如提示词），用等宽字体 `font` 小字画，
/// 比普通的悬停提示宽，太高时截掉下面的。
pub fn code_tooltip(
    text: impl Into<SharedString>,
    font: SharedString,
    fg: Rgb,
    bg: Rgb,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tooltip { text: text.clone(), shortcut: None, font: Some(font.clone()), fg, bg }).into()
}

struct Tooltip {
    text: SharedString,
    shortcut: Option<SharedString>,
    /// 代码样的说明用的等宽字体，见 `code_tooltip`。
    font: Option<SharedString>,
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
            .text_color(hsla(self.fg))
            .map(|tooltip| match &self.font {
                Some(font) => tooltip
                    .max_w(px(CODE_TOOLTIP_MAX_WIDTH))
                    .max_h(px(CODE_TOOLTIP_MAX_HEIGHT))
                    .overflow_hidden()
                    .py(px(6.))
                    .font_family(font.clone())
                    .text_size(px(11.)),
                None => tooltip.max_w(px(TOOLTIP_MAX_WIDTH)).text_size(px(12.)),
            })
            // 字放在能缩的一格里，长的一行才会在最宽处折行，不然撑出框外。
            .child(div().flex_1().min_w_0().child(self.text.clone()))
            .children(
                self.shortcut.clone().map(|shortcut| div().text_color(hsla(self.fg).opacity(0.5)).child(shortcut)),
            )
    }
}
