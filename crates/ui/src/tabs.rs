//! 标签条共用的部件：标签上关闭按钮的边长、拖动标签时跟着鼠标的卡片和目标上的落点提示。颜色从主题读，只在 render、paint 和拖动时算样式的回调里调。

use std::cmp::Ordering;

use gpui::{App, Axis, BoxShadow, Div, Hsla, Pixels, SharedString, StyleRefinement, div, point, prelude::*, px};

use crate::{font_size::FontSize, theme::ActiveTheme as _};

/// 标签上关闭按钮的边长，和 `ButtonSize::IconXs` 一样。
pub const TAB_CLOSE_SIZE: f32 = 16.;

/// 拖动排序时目标上的落点提示：画在目标靠近原位置的另一侧，往后（右、下）拖插到它后面，往前
/// 拖插到它前面。`axis` 是各项排开的方向，标签横着排，侧栏的 workspace 竖着排。
pub fn drop_marker(style: StyleRefinement, from: usize, to: usize, axis: Axis, cx: &App) -> StyleRefinement {
    let marker = cx.theme().foreground.opacity(0.6);
    let style = match (from.cmp(&to), axis) {
        (Ordering::Equal, _) => return style,
        (Ordering::Less, Axis::Horizontal) => style.border_r_2(),
        (Ordering::Greater, Axis::Horizontal) => style.border_l_2(),
        (Ordering::Less, Axis::Vertical) => style.border_b_2(),
        (Ordering::Greater, Axis::Vertical) => style.border_t_2(),
    };
    style.border_color(marker)
}

/// 拖动标签或 workspace 时跟着鼠标的那张卡片：带边框和阴影的圆角块，`label` 放不下时截断。底色 `bg`
/// 由拖动预览在自己画的时候从主题算，和被拖的那一项选中时一样。
pub fn drag_chip(width: Pixels, height: Pixels, label: SharedString, bg: Hsla, cx: &App) -> Div {
    let fg = cx.theme().foreground;
    div()
        .w(width)
        .h(height)
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .bg(bg)
        .border_1()
        .border_color(fg.opacity(0.15))
        .shadow(vec![BoxShadow {
            color: Hsla::black().opacity(0.3),
            offset: point(px(0.), px(2.)),
            blur_radius: px(8.),
            spread_radius: px(0.),
            inset: false,
        }])
        .text_size(FontSize::Body)
        .text_color(fg)
        .child(div().min_w_0().truncate().child(label))
}
