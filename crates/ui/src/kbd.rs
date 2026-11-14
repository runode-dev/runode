//! 按键提示（照 shadcn 的 `Kbd`）：一小块圆角的底子，里面用等宽字写一个键或一串组合键，比如搜索框右边的
//! `⌘K`。只是装饰，不报给辅助工具：键的说明由调用方写进它旁边那个控件的名字或提示里。
//!
//! 调用方接着写的样式盖在组件自己的上面。

use gpui::{
    App, Div, IntoElement, ParentElement, RenderOnce, SharedString, StyleRefinement, Styled, Window, div, prelude::*,
    px,
};

use crate::{
    font,
    font_size::{FontSize, scaled},
    theme::ActiveTheme as _,
};

#[derive(IntoElement)]
pub struct Kbd {
    div: Div,
    text: SharedString,
}

impl Kbd {
    /// `text` 是画出来的键，由调用方按平台写好（比如经 `keys::text`）。
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { div: div(), text: text.into() }
    }
}

impl RenderOnce for Kbd {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let Self { mut div, text } = self;
        let user = std::mem::take(div.style());
        let mut div = div
            .flex_none()
            .h(scaled(18.))
            .px(px(5.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.mix(0.08))
            .text_size(FontSize::Small)
            .text_color(theme.muted_foreground);
        if let Some(family) = font::mono(cx) {
            div = div.font_family(family);
        }
        div.style().refine(&user);
        div.child(text)
    }
}

impl Styled for Kbd {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}
