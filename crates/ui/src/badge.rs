//! 徽标（照 shadcn 的 `Badge`）：一小块圆角的底子，里面一段小字，比如段标题上的个数。`BadgeVariant` 选样子，
//! 颜色在 render 里从主题读；调用方接着写的样式盖在组件自己的上面，写法同 `Button`。文字太长时截断。
//!
//! 和 `Card` 一样不一定要报给辅助工具，要报时先用 `Badge::id` 给一个 id。

use gpui::{
    App, Div, ElementId, FontWeight, InteractiveElement, Interactivity, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::*, px,
};

use crate::{
    font_size::{FontSize, scaled},
    theme::ActiveTheme as _,
};

/// 徽标的样子。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BadgeVariant {
    /// 实心的：前景色的底、背景色的粗体字，一眼看得到。
    #[default]
    Default,
    /// 淡淡一层底色，字也淡。
    Secondary,
    /// 描一圈边，没有底色。
    Outline,
}

#[derive(IntoElement)]
pub struct Badge {
    div: Div,
    text: SharedString,
    variant: BadgeVariant,
}

impl Badge {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { div: div(), text: text.into(), variant: BadgeVariant::default() }
    }

    pub fn variant(mut self, variant: BadgeVariant) -> Self {
        self.variant = variant;
        self
    }

    /// 给徽标一个元素 id，之后才能报给辅助工具。见 `Card::id`。
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.div.interactivity().element_id = Some(id.into());
        self
    }
}

impl RenderOnce for Badge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let Self { mut div, text, variant } = self;
        let user = std::mem::take(div.style());
        let div = div
            .flex_none()
            .min_w(px(18.))
            .h(scaled(16.))
            .px(px(5.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .text_size(FontSize::Tiny);
        let mut div = match variant {
            BadgeVariant::Default => {
                div.bg(theme.primary).text_color(theme.primary_foreground).font_weight(FontWeight::SEMIBOLD)
            }
            BadgeVariant::Secondary => div.bg(theme.mix(0.12)).text_color(theme.muted_foreground),
            BadgeVariant::Outline => div.border_1().border_color(theme.input).text_color(theme.foreground),
        };
        div.style().refine(&user);
        div.child(gpui::div().min_w_0().truncate().child(text))
    }
}

impl Styled for Badge {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl InteractiveElement for Badge {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for Badge {}
