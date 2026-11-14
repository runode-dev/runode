//! 全 app 统一的状态标记：琥珀色的实心光标是在等你回答，依次点亮的盲文格是在干活，`>_` 是普通 shell 或
//! 空闲，绿色对勾是做完了。标记只是图形，不报无障碍节点，状态由所在的那一行写进自己的描述。

use gpui::{AnyElement, App, ElementId, div, prelude::*, px, svg};

use crate::{
    icons::{CHECK_ICON, PROMPT_ICON},
    spinner::{blinking_cursor, braille, braille_still},
    theme::ActiveTheme as _,
};

/// 标记表示的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkKind {
    /// 等你回答；`blink` 时光标闪烁。
    Waiting {
        blink: bool,
    },
    /// 在干活；`spin` 时盲文格转动，否则停在一格上。
    Working {
        spin: bool,
    },
    Idle,
    Done,
}

/// `size` 见方的状态标记。会动的（在干活、闪烁的光标）在 `id` 处单独是一个小视图，动一格只通知它。
pub fn status_mark(id: impl Into<ElementId>, kind: MarkKind, size: f32, cx: &App) -> AnyElement {
    let theme = cx.theme();
    match kind {
        MarkKind::Waiting { blink: false } => div()
            .flex_none()
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .child(div().w(px(size * 13. / 24.)).h(px(size * 22. / 24.)).rounded(px(1.)).bg(theme.warning))
            .into_any_element(),
        MarkKind::Waiting { blink: true } => {
            div().flex_none().text_color(theme.warning).child(blinking_cursor(id, size)).into_any_element()
        }
        MarkKind::Working { spin: true } => {
            div().flex_none().text_color(theme.info).child(braille(id, size)).into_any_element()
        }
        MarkKind::Working { spin: false } => {
            div().flex_none().text_color(theme.info).child(braille_still(size)).into_any_element()
        }
        MarkKind::Idle => {
            svg().path(PROMPT_ICON).flex_none().size(px(size)).text_color(theme.muted_foreground).into_any_element()
        }
        MarkKind::Done => {
            svg().path(CHECK_ICON).flex_none().size(px(size)).text_color(theme.success).into_any_element()
        }
    }
}
