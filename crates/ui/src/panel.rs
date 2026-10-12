//! 窗口外框和右侧面板共用的写法：分隔线的颜色、卡片样式的卡片、面板的外框、顶上那一条、标题行和居中的
//! 说明，改动行数的标签，以及标题栏和面板上带图标的开关按钮。

use gpui::{BoxShadow, Div, Hsla, MouseButton, Role, Stateful, div, point, prelude::*, px, svg};
use runode_shared_types::color::Rgb;

use crate::{
    drag_window, hsla,
    icons::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON},
    theme::{ADDED, REMOVED},
};

/// 透明标题栏的高度：终端内容从它下面开始，这一条用来拖动窗口，多个标签时也画在这里；
/// 显示侧栏时红绿灯落在侧栏顶上。
pub const TITLEBAR_HEIGHT: f32 = 36.;
/// 卡片样式下卡片的圆角，分屏和面板顶上标题条的高度。
pub const CARD_RADIUS: f32 = 10.;
pub const PANE_HEADER_HEIGHT: f32 = 30.;
/// 分隔线两边各留这么宽能按住拖动。
pub const DIVIDER_GRAB_WIDTH: f32 = 6.;

/// 面板之间、标题下面和标签之间这些分隔线的颜色：前景色调淡，各处一样深。
pub fn divider_color(fg: Hsla) -> Hsla {
    fg.opacity(0.09)
}

/// 卡片的外观：终端背景色的圆角块，一圈淡淡的边和一点阴影，从外框上浮起来。
pub fn card(fg: Hsla, bg: Hsla) -> Div {
    div().rounded(px(CARD_RADIUS)).bg(bg).border_1().border_color(fg.opacity(0.08)).shadow(vec![BoxShadow {
        color: Hsla::black().opacity(0.06),
        offset: point(px(0.), px(1.)),
        blur_radius: px(3.),
        spread_radius: px(0.),
        inset: false,
    }])
}

/// 加了多少行、删了多少行的标签，「+N」和「−N」。
pub fn added_label(count: usize) -> Div {
    div().flex_none().text_color(hsla(ADDED)).child(format!("+{count}"))
}

pub fn removed_label(count: usize) -> Div {
    div().flex_none().text_color(hsla(REMOVED)).child(format!("−{count}"))
}

/// 右侧面板标签下面那一行：这一页的标题和按钮。
pub fn panel_title() -> Div {
    div().flex_none().h(px(PANE_HEADER_HEIGHT)).px(px(10.)).flex().items_center().gap(px(8.))
}

/// 面板里居中的一句说明，比如不在仓库里、没有改动、预览不了；也报给辅助工具。
pub fn panel_message(text: String, fg: Rgb) -> Stateful<Div> {
    div()
        .id("panel-message")
        .role(Role::Label)
        .aria_label(text.clone())
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px(px(16.))
        .text_color(hsla(fg).opacity(0.5))
        .child(text)
}

/// 预览栏和右侧面板顶上的一条。经典样式下和标题栏等高，能拖动窗口、双击缩放；卡片样式（`cards`）下是
/// 卡片的标题条，和分屏的一样高。
pub fn panel_header(fg: Rgb, cards: bool) -> Div {
    let header = div()
        .flex_none()
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(8.))
        .border_b_1()
        .border_color(divider_color(hsla(fg)));
    if cards {
        return header.h(px(PANE_HEADER_HEIGHT));
    }
    header.h(px(TITLEBAR_HEIGHT)).on_mouse_down(MouseButton::Left, drag_window)
}

/// 右侧面板的外框：定宽、占满高度、竖着排。经典样式下左边一条分隔线，卡片样式（`cards`）下是一张
/// 卡片。底色和字号由调用方接着写。
pub fn panel_shell(id: &'static str, width: f32, fg: Rgb, bg: Rgb, cards: bool) -> Stateful<Div> {
    let shell = if cards {
        card(hsla(fg), hsla(bg)).overflow_hidden()
    } else {
        div().border_l_1().border_color(divider_color(hsla(fg)))
    };
    shell.id(id).flex_none().w(px(width)).h_full().flex().flex_col()
}

/// 标题栏和面板上带图标的开关按钮，图标边长 `icon_size`；悬停时底色和图标变亮，`shown` 时
/// 底色一直亮着，图标也亮一些。位置、尺寸、提示和点击由调用方接着写，报给辅助工具的名字
/// （`aria_label`，通常是提示的文字）和开关状态也是。
pub fn icon_toggle(
    id: &'static str,
    icon: &'static str,
    icon_size: f32,
    shown: bool,
    fg: Rgb,
    bg: Rgb,
) -> Stateful<Div> {
    let hover_bg = hsla(bg.mix(fg, 0.10));
    let active_bg = hsla(bg.mix(fg, 0.07));
    let fg = hsla(fg);
    div()
        .id(id)
        .role(Role::Button)
        .group(id)
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .when(shown, |button| button.bg(active_bg))
        .hover(|button| button.bg(hover_bg))
        .child(
            svg()
                .path(icon)
                .size(px(icon_size))
                .text_color(fg.opacity(if shown { 0.9 } else { 0.55 }))
                .group_hover(id, |icon| icon.text_color(fg)),
        )
}

/// 树形列表里展开（`expanded`）或收起的箭头。
pub fn chevron(expanded: bool, fg: Rgb) -> gpui::Svg {
    svg()
        .flex_none()
        .path(if expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
        .size(px(12.))
        .text_color(hsla(fg).opacity(0.45))
}
