//! 浮层（照 shadcn 的 `Popover`）的外框：圆角、浮层底色、描边和阴影，挡住下面的鼠标（`occlude`）。选 agent、
//! 选分支、设置页的选择器、排列分屏、状态栏的浮层和菜单都用它。颜色在 render 里从主题读；调用方接着写的样式
//! 盖在组件自己的上面，写法同 `Button`。
//!
//! 只管外框：摆在哪（`anchored`、`deferred`）、role 和名字、焦点、点到外面就关，由调用方管。

use gpui::{
    AnyElement, App, Div, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement, RenderOnce,
    Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::*, px,
};

use crate::{font_size::FontSize, theme::ActiveTheme as _};

#[derive(IntoElement)]
pub struct Popover {
    div: Stateful<Div>,
}

impl Popover {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self { div: div().id(id).occlude() }
    }
}

impl RenderOnce for Popover {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let Self { mut div } = self;
        let user = std::mem::take(div.style());
        let mut div = div
            .rounded(px(8.))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.input)
            .shadow_md()
            .text_size(FontSize::Body)
            .text_color(theme.popover_foreground);
        div.style().refine(&user);
        div
    }
}

impl Styled for Popover {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl ParentElement for Popover {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.div.extend(elements);
    }
}

impl InteractiveElement for Popover {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for Popover {}
