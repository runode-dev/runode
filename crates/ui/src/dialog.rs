//! 对话框（照 shadcn 的 `Dialog`）的面板：圆角、浮层底色、描边和大阴影，自带 `Role::Dialog` 和报给辅助工具的
//! 名字，按下鼠标不往外传（点在对话框里不算点到外面）。颜色在 render 里从主题读；调用方接着写的样式盖在
//! 组件自己的上面，写法同 `Button`。
//!
//! 铺满窗口的遮罩是 `backdrop`，点到外面关不关由调用方接着写；对话框开着时把下面的界面藏起来不给辅助工具读
//! （`a11y` 的 `Hide`）由调用方管；焦点也由调用方管，面板要拿焦点时自己 `track_focus`。按什么键关掉见
//! `is_dismiss_key`。

use gpui::{
    AnyElement, App, Div, ElementId, Hsla, InteractiveElement, Interactivity, IntoElement, Keystroke, MouseButton,
    ParentElement, RenderOnce, SharedString, Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window,
    div, prelude::*, px,
};

use crate::{font_size::FontSize, panel::TITLEBAR_HEIGHT, theme::ActiveTheme as _};

/// 对话框开着时铺满窗口的底子：压暗下面的界面，挡住下面的点击和悬停，对话框放进来时在标题栏下面一点、
/// 横向居中。调用方的样式盖在这些上面，点到外面就关时接着写 `on_mouse_down`。
pub fn backdrop(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .size_full()
        .pt(px(TITLEBAR_HEIGHT + 48.))
        .px(px(16.))
        .pb(px(16.))
        .flex()
        .justify_center()
        .items_start()
        .bg(Hsla::black().opacity(0.15))
        .occlude()
}

/// 关掉对话框的键：不带修饰键的 Esc。
pub fn is_dismiss_key(keystroke: &Keystroke) -> bool {
    keystroke.key == "escape" && !keystroke.modifiers.modified()
}

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
