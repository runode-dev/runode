//! 开关（照 shadcn 的 `Switch`）：一条圆角的轨道，里面一颗白色的圆钮。打开时钮在右边、轨道是
//! `Theme::ring`，关着时轨道是淡淡的前景色。自带 `Role::Switch`、报给辅助工具的名字和开没开；点击照旧用
//! `a11y` 的 `Press`，不可用时调用方自己灰掉、报 `aria_disabled`、不挂点击。
//!
//! 给了 `label` 时那段文字画在轨道右边，和轨道是同一个节点，点文字也算点开关。调用方接着写的样式盖在
//! 组件自己的上面，写法同 `Button`。

use gpui::{
    App, Div, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement, RenderOnce, SharedString,
    Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::*, px,
};

use crate::theme::ActiveTheme as _;

#[derive(IntoElement)]
pub struct Switch {
    div: Stateful<Div>,
    checked: bool,
    text: Option<SharedString>,
}

impl Switch {
    /// `label` 是报给辅助工具的名字，`checked` 是开着没有。
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, checked: bool) -> Self {
        let div = div().id(id).role(gpui::Role::Switch).aria_label(label).aria_toggled(checked.into());
        Self { div, checked, text: None }
    }

    /// 画在轨道右边的文字。
    pub fn label(mut self, text: impl Into<SharedString>) -> Self {
        self.text = Some(text.into());
        self
    }
}

/// 把 `el` 画成轨道：34×20，四周留 2px，钮 16px。
fn track<E: Styled + ParentElement + FluentBuilder>(el: E, checked: bool, color: gpui::Hsla) -> E {
    el.w(px(34.))
        .h(px(20.))
        .flex_none()
        .rounded_full()
        .p(px(2.))
        .flex()
        .when(checked, |track| track.justify_end())
        .bg(color)
        .child(div().size(px(16.)).rounded_full().bg(gpui::white()).shadow_sm())
}

impl RenderOnce for Switch {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let color = if self.checked { theme.ring } else { theme.foreground.opacity(0.18) };
        let Self { mut div, checked, text } = self;
        let user = std::mem::take(div.style());
        let mut div = match text {
            None => track(div, checked, color),
            Some(text) => div.flex().items_center().gap(px(8.)).child(track(gpui::div(), checked, color)).child(text),
        };
        div.style().refine(&user);
        div
    }
}

impl Styled for Switch {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl InteractiveElement for Switch {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for Switch {}
