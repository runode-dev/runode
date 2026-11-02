//! 分段选项（单选的一排按钮）：一条圆角的浅色轨道，里面几段挨着的按钮，选中的那段底色亮一些。
//! 自带 `Role::RadioGroup`、每段的 `Role::RadioButton` 和选没选（`aria_toggled`），点击和辅助工具的按下都
//! 经 `a11y` 的 `Press`。竖排或每项带说明的单选组不用它。
//!
//! 调用方接着写的样式盖在轨道自己的上面，写法同 `Button`。

use gpui::{
    AnyElement, App, Context, Div, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement,
    RenderOnce, Role, SharedString, Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window, div,
    prelude::*, px,
};

use crate::{
    a11y::Press as _,
    font_size::{FontSize, scaled},
    theme::ActiveTheme as _,
};

#[derive(IntoElement)]
pub struct Segmented {
    div: Stateful<Div>,
    items: Vec<Stateful<Div>>,
    fill: bool,
}

impl Segmented {
    /// `label` 是这一组报给辅助工具的名字。
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let div = div().id(id).role(Role::RadioGroup).aria_label(label);
        Self { div, items: Vec::new(), fill: false }
    }

    /// 加一段：`label` 是显示的文字，也是报给辅助工具的名字，`selected` 是选没选，点了调 `pick`。
    pub fn option<V: 'static>(
        mut self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        selected: bool,
        cx: &mut Context<V>,
        pick: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    ) -> Self {
        let label = label.into();
        let theme = cx.theme();
        let (fg, on_bg, hover_bg) = (theme.foreground, theme.mix(0.12), theme.mix(0.07));
        let item = div()
            .id(id)
            .role(Role::RadioButton)
            .aria_label(label.clone())
            .aria_toggled(selected.into())
            .px(px(10.))
            .flex()
            .items_center()
            .rounded(px(5.))
            .cursor_pointer()
            .map(|item| {
                if selected {
                    item.bg(on_bg).text_color(fg)
                } else {
                    item.text_color(fg.opacity(0.7)).hover(move |item| item.bg(hover_bg))
                }
            })
            .on_press(cx, pick)
            .child(label);
        self.items.push(item);
        self
    }

    /// 各段平分轨道的宽度，文字居中。
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self
    }
}

impl RenderOnce for Segmented {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let Self { mut div, items, fill } = self;
        let user = std::mem::take(div.style());
        let mut div = div
            .flex()
            .h(scaled(26.))
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(7.))
            .bg(theme.mix(0.06))
            .border_1()
            .border_color(theme.foreground.opacity(0.13))
            .text_size(FontSize::Body)
            .children(items.into_iter().map(|item| -> AnyElement {
                if fill { item.flex_1().justify_center().into_any_element() } else { item.into_any_element() }
            }));
        div.style().refine(&user);
        div
    }
}

impl Styled for Segmented {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl InteractiveElement for Segmented {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for Segmented {}
