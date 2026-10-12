//! 窗口外框和右侧面板共用的写法：卡片样式的卡片、面板的外框、顶上那一条、标题行和居中的说明，以及改动
//! 行数的标签。颜色从主题读（分隔线是 `Theme::border`），所以只在 render 里调。

use gpui::{App, BoxShadow, Div, Hsla, MouseButton, Role, Stateful, div, point, prelude::*, px, svg};

use crate::{
    drag_window, hsla,
    icons::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON},
    theme::{ADDED, ActiveTheme as _, REMOVED},
};

/// 透明标题栏的高度：终端内容从它下面开始，这一条用来拖动窗口，多个标签时也画在这里；
/// 显示侧栏时红绿灯落在侧栏顶上。
pub const TITLEBAR_HEIGHT: f32 = 36.;
/// 卡片样式下卡片的圆角，分屏和面板顶上标题条的高度。
pub const CARD_RADIUS: f32 = 10.;
pub const PANE_HEADER_HEIGHT: f32 = 30.;
/// 分隔线两边各留这么宽能按住拖动。
pub const DIVIDER_GRAB_WIDTH: f32 = 6.;

/// 卡片的外观：终端背景色的圆角块，一圈淡淡的边和一点阴影，从外框上浮起来。
pub fn card(cx: &App) -> Div {
    let theme = cx.theme();
    div().rounded(px(CARD_RADIUS)).bg(theme.card).border_1().border_color(theme.foreground.opacity(0.08)).shadow(vec![
        BoxShadow {
            color: Hsla::black().opacity(0.06),
            offset: point(px(0.), px(1.)),
            blur_radius: px(3.),
            spread_radius: px(0.),
            inset: false,
        },
    ])
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
pub fn panel_message(text: String, cx: &App) -> Stateful<Div> {
    div()
        .id("panel-message")
        .role(Role::Label)
        .aria_label(text.clone())
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px(px(16.))
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// 预览栏和右侧面板顶上的一条。经典样式下和标题栏等高，能拖动窗口、双击缩放；卡片样式（`cards`）下是
/// 卡片的标题条，和分屏的一样高。
pub fn panel_header(cards: bool, cx: &App) -> Div {
    let header =
        div().flex_none().px(px(10.)).flex().items_center().gap(px(8.)).border_b_1().border_color(cx.theme().border);
    if cards {
        return header.h(px(PANE_HEADER_HEIGHT));
    }
    header.h(px(TITLEBAR_HEIGHT)).on_mouse_down(MouseButton::Left, drag_window)
}

/// 右侧面板的外框：定宽、占满高度、竖着排。经典样式下左边一条分隔线，卡片样式（`cards`）下是一张
/// 卡片。底色和字号由调用方接着写。
pub fn panel_shell(id: &'static str, width: f32, cards: bool, cx: &App) -> Stateful<Div> {
    let shell = if cards { card(cx).overflow_hidden() } else { div().border_l_1().border_color(cx.theme().border) };
    shell.id(id).flex_none().w(px(width)).h_full().flex().flex_col()
}

/// 树形列表里展开（`expanded`）或收起的箭头。
pub fn chevron(expanded: bool, cx: &App) -> gpui::Svg {
    svg()
        .flex_none()
        .path(if expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
        .size(px(12.))
        .text_color(cx.theme().foreground.opacity(0.45))
}
