//! 树形列表行尾的一排图标按钮，Git 面板和 GitHub Actions 页共用：平时藏着，鼠标移到行上或辅助工具在读
//! 时才画出来；切换着的按钮（固定着的工作流）不悬停也露着。

use std::borrow::Cow;

use gpui::{Context, Div, Role, Window, div, prelude::*, px, svg};

use crate::{
    a11y::{Disable, PressDown},
    theme::ActiveTheme as _,
    tooltip::tooltip,
};

type Handler<V> = Box<dyn Fn(&mut V, &mut Window, &mut Context<V>)>;

/// 行尾的一个图标按钮：图标、提示文字和按下时在视图 `V` 上做的事。
pub struct RowButton<V> {
    icon: &'static str,
    text: Cow<'static, str>,
    /// 按下去是切换的，切换着时图标亮一些；普通按钮为空。
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
    let buttons: Vec<RowButton<V>> =
        buttons.into_iter().filter(|button| shown || button.toggled == Some(true)).collect();
    if buttons.is_empty() {
        return None;
    }
    let hover_bg = cx.theme().mix(0.14);
    let fg = cx.theme().foreground;
    let row = div().flex_none().flex().items_center().gap(px(2.)).children(buttons.into_iter().enumerate().map(
        |(bi, button)| {
            let handler = button.handler;
            let opacity = match (enabled, button.toggled) {
                (false, _) => 0.3,
                (true, Some(true)) => 0.95,
                (true, _) => 0.75,
            };
            div()
                .id(("row-button", bi))
                .role(Role::Button)
                .aria_label(button.text.clone())
                .when_some(button.toggled, |button, toggled| button.aria_toggled(toggled.into()))
                .aria_disabled(!enabled)
                .flex_none()
                .size(px(20.))
                .rounded(px(3.))
                .flex()
                .items_center()
                .justify_center()
                .tooltip(tooltip(button.text, None))
                .child(svg().path(button.icon).size(px(14.)).text_color(fg.opacity(opacity)))
                .when(enabled, |button| button.hover(|button| button.bg(hover_bg)).on_press_down(cx, handler))
        },
    ));
    Some(row)
}
