//! 树形列表行尾的一排图标按钮（每个是一个 `Button`）：平时藏着，鼠标移到行上或辅助工具在读
//! 时才画出来；切换着的按钮（固定着的工作流）不悬停也露着。

use std::borrow::Cow;

use gpui::{Context, Div, Window, div, prelude::*, px};

use crate::{
    a11y::PressDown,
    button::{Button, ButtonSize},
    tooltip::tooltip,
};

type Handler<V> = Box<dyn Fn(&mut V, &mut Window, &mut Context<V>)>;

/// 行尾的一个图标按钮：图标、提示文字和按下时在视图 `V` 上做的事。
pub struct RowButton<V> {
    icon: &'static str,
    text: Cow<'static, str>,
    /// 按下去是切换的，切换着时图标亮一些（`Button::toggled`）；普通按钮为空。
    pub toggled: Option<bool>,
    handler: Handler<V>,
}

pub fn button<V>(
    icon: &'static str,
    text: Cow<'static, str>,
    handler: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> RowButton<V> {
    RowButton { icon, text, toggled: None, handler: Box::new(handler) }
}

/// 一行行尾的按钮。`shown` 为假（鼠标不在这行、辅助工具也没开）时只画切换着的，一个都没有就不画、
/// 不占宽，名字能排满整行；`enabled` 为假时按不动。不用 `hidden` 加 `group_hover` 露出来：gpui 在
/// prepaint 时还不知道这一帧行被悬停，会跳过藏着的按钮，paint 时却要画它们，就 panic 了。
pub fn row_buttons<V: 'static>(
    buttons: Vec<RowButton<V>>,
    shown: bool,
    enabled: bool,
    cx: &mut Context<V>,
) -> Option<Div> {
    // 先编号再筛：按钮的 id 按它在整排里的位置取，露出别的按钮时切换着的那个 id 不变。
    let buttons: Vec<(usize, RowButton<V>)> =
        buttons.into_iter().enumerate().filter(|(_, button)| shown || button.toggled == Some(true)).collect();
    if buttons.is_empty() {
        return None;
    }
    let row = div().flex_none().flex().items_center().gap(px(2.)).children(buttons.into_iter().map(|(bi, button)| {
        let handler = button.handler;
        Button::new(("row-button", bi), button.text.clone())
            .icon(button.icon)
            .with_size(ButtonSize::IconSm)
            .toggled(button.toggled)
            .disabled(!enabled)
            .tooltip(tooltip(button.text, None))
            .when(enabled, |button| button.on_press_down(cx, handler))
    }));
    Some(row)
}
