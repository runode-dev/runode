//! 对话框（照 shadcn 的 `Dialog`）的面板：圆角、浮层底色、描边和大阴影，自带 `Role::Dialog` 和报给辅助工具的
//! 名字，按下鼠标不往外传（点在对话框里不算点到外面）。颜色在 render 里从主题读；调用方接着写的样式盖在
//! 组件自己的上面，写法同 `Button`。
//!
//! 只管面板本身：铺满窗口、点到外面就关的遮罩，和对话框开着时把下面的界面藏起来不给辅助工具读
//! （`a11y` 的 `Hide`），由调用方管；焦点也由调用方管，面板要拿焦点时自己 `track_focus`。

use gpui::{
    AnyElement, App, Div, ElementId, InteractiveElement, Interactivity, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::*,
    px,
};

use crate::{font_size::FontSize, theme::ActiveTheme as _};

#[derive(IntoElement)]
pub struct Dialog {
    div: Stateful<Div>,
}

impl Dialog {
    /// `title` 是报给辅助工具的名字，通常就是对话框的标题。
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>) -> Self {
        let div = div()
            .id(id)
            .role(gpui::Role::Dialog)
            .aria_label(title)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        Self { div }
    }
}

impl RenderOnce for Dialog {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let Self { mut div } = self;
        let user = std::mem::take(div.style());
        let mut div = div
            .rounded(px(12.))
            .bg(theme.popover)
            .border_1()
            .border_color(theme.input)
            .shadow_lg()
            .text_size(FontSize::Body)
            .text_color(theme.popover_foreground);
        div.style().refine(&user);
        div
    }
}

impl Styled for Dialog {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl ParentElement for Dialog {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.div.extend(elements);
    }
}

impl InteractiveElement for Dialog {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for Dialog {}
