//! 下拉选择的触发框（照 shadcn 的 `SelectTrigger`）：点开一个列表从里面挑，框里显示挑着的值，放不下时截断，
//! 右边一个下拉箭头。自带 `Role::ComboBox`、报给辅助工具的名字和值；展没展开由调用方用 `aria_expanded` 报。
//!
//! 高度、底色、边框和悬停的底色由调用方接着写，好和各自旁边的输入框对齐；写法同 `Button`。挂进来的子元素
//! （弹出的菜单、量位置的 canvas）画在箭头后面。

use gpui::{
    AnyElement, App, Div, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement, RenderOnce, Role,
    SharedString, Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::*, px, svg,
};

use crate::{icons::CHEVRON_DOWN_ICON, theme::ActiveTheme as _};

#[derive(IntoElement)]
pub struct SelectTrigger {
    div: Stateful<Div>,
    value: SharedString,
    leading: Option<AnyElement>,
    extra: Vec<AnyElement>,
}

impl SelectTrigger {
    /// `name` 是报给辅助工具的名字，`value` 是挑着的值，显示在框里。
    pub fn new(id: impl Into<ElementId>, name: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        let value = value.into();
        let div = div().id(id).role(Role::ComboBox).aria_label(name).aria_value(value.clone());
        Self { div, value, leading: None, extra: Vec::new() }
    }

    /// 画在值左边的东西，比如图标、色块。
    pub fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }
}

impl RenderOnce for SelectTrigger {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let chevron = cx.theme().foreground.opacity(0.6);
        let Self { mut div, value, leading, extra } = self;
        let user = std::mem::take(div.style());
        let mut div = div
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .children(leading)
            .child(gpui::div().flex_1().min_w_0().truncate().child(value))
            .child(svg().flex_none().path(CHEVRON_DOWN_ICON).size(px(12.)).text_color(chevron))
            .children(extra);
        div.style().refine(&user);
        div
    }
}

impl ParentElement for SelectTrigger {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.extra.extend(elements);
    }
}

impl Styled for SelectTrigger {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl InteractiveElement for SelectTrigger {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for SelectTrigger {}
