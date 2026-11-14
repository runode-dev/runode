//! 复选框（照 shadcn 的 `Checkbox`）：一个小圆角方块，勾上时是 `Theme::primary` 的实底打勾，半勾（`Toggled::Mixed`）
//! 时实底画一道横线，没勾时只有一圈边。自带 `Role::CheckBox`、报给辅助工具的名字和勾没勾；按下照旧用 `a11y` 的
//! `Press` 或 `PressDown`，不可用时调用方自己灰掉、报 `aria_disabled`、不挂按下的处理。
//!
//! 给了 `label` 时那段文字画在方块右边，和方块是同一个节点，点文字也算点方块。调用方接着写的样式盖在组件自己
//! 的上面。

use gpui::{
    App, Div, ElementId, InteractiveElement, Interactivity, IntoElement, ParentElement, RenderOnce, SharedString,
    Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Toggled, Window, div, prelude::*, px, svg,
};

use crate::{
    icons::{CHECK_ICON, MINUS_ICON},
    theme::ActiveTheme as _,
};

/// 方块的边长。
const BOX_SIZE: f32 = 14.;

#[derive(IntoElement)]
pub struct Checkbox {
    div: Stateful<Div>,
    state: Toggled,
    text: Option<SharedString>,
}

impl Checkbox {
    /// `label` 是报给辅助工具的名字，`state` 是勾上、没勾还是半勾。
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, state: Toggled) -> Self {
        let div = div().id(id).role(gpui::Role::CheckBox).aria_label(label).aria_toggled(state);
        Self { div, state, text: None }
    }

    /// 半勾时另外报给辅助工具的说明：macOS 的辅助工具读到的勾选值只分勾了和没勾。
    pub fn mixed(mut self, description: impl Into<SharedString>) -> Self {
        if self.state == Toggled::Mixed {
            self.div = self.div.aria_description(description);
        }
        self
    }

    /// 画在方块右边的文字。
    pub fn label(mut self, text: impl Into<SharedString>) -> Self {
        self.text = Some(text.into());
        self
    }
}

/// 复选框画出来的方块，`state` 是勾上、没勾还是半勾。
pub fn checkbox_box(state: Toggled, cx: &App) -> Div {
    let theme = cx.theme();
    let (primary, on_primary, border) = (theme.primary, theme.primary_foreground, theme.control_border);
    let mark = match state {
        Toggled::True => Some(CHECK_ICON),
        Toggled::Mixed => Some(MINUS_ICON),
        Toggled::False => None,
    };
    let square = div().flex_none().size(px(BOX_SIZE)).rounded(px(4.)).flex().items_center().justify_center();
    match mark {
        Some(icon) => square.bg(primary).child(svg().path(icon).size(px(10.)).text_color(on_primary)),
        None => square.border_1().border_color(border),
    }
}

impl RenderOnce for Checkbox {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { mut div, state, text } = self;
        let user = std::mem::take(div.style());
        let mut div = div.flex_none().flex().items_center().gap(px(8.)).child(checkbox_box(state, cx)).children(text);
        div.style().refine(&user);
        div
    }
}

impl Styled for Checkbox {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl InteractiveElement for Checkbox {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for Checkbox {}
