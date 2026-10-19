//! 卡片（照 shadcn 的 `Card`）：背景色的圆角块，一圈淡淡的边和一点阴影，从外框上浮起来。颜色在 render 里从主题读；调用方接着写的样式盖在组件自己的
//! 上面，写法同 `Button`。
//!
//! 卡片不一定要报给辅助工具，所以 `Card::new` 不要 id；要报时先用 `Card::id` 给一个，再照常写 role 和名字。

use gpui::{
    AnyElement, App, BoxShadow, Div, ElementId, Hsla, InteractiveElement, Interactivity, IntoElement, ParentElement,
    RenderOnce, StatefulInteractiveElement, StyleRefinement, Styled, Window, div, point, prelude::*, px,
};

use crate::{
    panel::CARD_RADIUS,
    theme::{ActiveTheme as _, Theme},
};

#[derive(IntoElement)]
pub struct Card {
    div: Div,
}

impl Card {
    pub fn new() -> Self {
        Self { div: div() }
    }

    /// 给卡片一个元素 id，之后才能报给辅助工具、记悬停这类状态。和 `InteractiveElement::id` 设的是同一样
    /// 东西，只是不包成 `Stateful`，接着写的还是 `Card`。
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.div.interactivity().element_id = Some(id.into());
        self
    }
}

/// 把 `el` 画成卡片的样子。`Card` 用它；右侧面板的外框（`panel_shell`）在经典样式下不是卡片，两种样式要是
/// 同一个类型，所以直接用它。
pub(crate) fn card_style<E: Styled>(el: E, theme: &Theme) -> E {
    el.rounded(px(CARD_RADIUS)).bg(theme.card).border_1().border_color(theme.foreground.opacity(0.08)).shadow(vec![
        BoxShadow {
            color: Hsla::black().opacity(0.06),
            offset: point(px(0.), px(1.)),
            blur_radius: px(3.),
            spread_radius: px(0.),
            inset: false,
        },
    ])
}

impl Default for Card {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for Card {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { mut div } = self;
        let user = std::mem::take(div.style());
        let mut div = card_style(div, cx.theme());
        div.style().refine(&user);
        div
    }
}

impl Styled for Card {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl ParentElement for Card {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.div.extend(elements);
    }
}

impl InteractiveElement for Card {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for Card {}
