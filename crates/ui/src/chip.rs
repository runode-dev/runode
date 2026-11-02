//! 可以选上、取消的小标签（多选里的一项）：选上的描强调色、打个勾。自带 `Role::CheckBox`、报给辅助工具的
//! 名字和选没选；点击照旧用 `a11y` 的 `Press`。
//!
//! 调用方接着写的样式盖在组件自己的上面，写法同 `Button`。

use gpui::{
    App, Div, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement, RenderOnce, Role, SharedString,
    Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::*, px, svg,
};

use crate::{
    font_size::{FontSize, scaled},
    icons::CHECK_ICON,
    theme::ActiveTheme as _,
};

#[derive(IntoElement)]
pub struct Chip {
    div: Stateful<Div>,
    label: SharedString,
    on: bool,
    icon: Option<&'static str>,
}

impl Chip {
    /// `label` 是显示的文字，也是报给辅助工具的名字，`on` 是选上没有。
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, on: bool) -> Self {
        let label = label.into();
        let div = div().id(id).role(Role::CheckBox).aria_label(label.clone()).aria_toggled(on.into());
        Self { div, label, on, icon: None }
    }

    /// 画在文字左边的图标。
    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }
}

impl RenderOnce for Chip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let Self { mut div, label, on, icon } = self;
        let user = std::mem::take(div.style());
        let hover_bg = theme.mix(0.07);
        let mut div = div
            .h(scaled(24.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(6.))
            .border_1()
            .border_color(if on { theme.ring } else { theme.control_border })
            .text_size(FontSize::Body)
            .cursor_pointer()
            .when(on, |chip| chip.bg(theme.ring.opacity(0.15)))
            .hover(move |chip| chip.bg(hover_bg))
            .children(on.then(|| svg().path(CHECK_ICON).size(px(11.)).text_color(theme.ring)))
            .children(icon.map(|icon| svg().path(icon).size(px(12.)).text_color(theme.foreground.opacity(0.7))))
            .child(label);
        div.style().refine(&user);
        div
    }
}

impl Styled for Chip {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl InteractiveElement for Chip {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for Chip {}
