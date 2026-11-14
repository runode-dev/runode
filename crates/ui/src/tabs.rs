//! 标签条共用的部件：标签上关闭按钮的边长、拖动标签时跟着鼠标的卡片和目标上的落点提示，以及切换一块
//! 区域里几页的那排标签（`TabsList`，照 shadcn 的 Tabs）。颜色从主题读，只在 render、paint 和拖动时算样式的
//! 回调里调。

use std::cmp::Ordering;

use gpui::{
    AnyElement, App, Axis, BoxShadow, Div, ElementId, FontWeight, Hsla, InteractiveElement, Interactivity, IntoElement,
    ParentElement, Pixels, RenderOnce, Role, SharedString, Stateful, StatefulInteractiveElement, StyleRefinement,
    Styled, Window, div, point, prelude::*, px,
};

use crate::{
    font,
    font_size::{FontSize, scaled},
    theme::ActiveTheme as _,
};

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
    font::apply(div(), cx)
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

/// 切换一块区域里几页的那排标签：一条圆角的轨道，各段平分宽度，选中的那段垫一层底、字重一些。轨道报成
/// `Role::TabList`，各段是 `TabsTrigger`。调用方接着写的样式盖在轨道自己的上面。
#[derive(IntoElement)]
pub struct TabsList {
    div: Stateful<Div>,
    triggers: Vec<TabsTrigger>,
}

impl TabsList {
    /// `label` 是这一排报给辅助工具的名字。
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self { div: div().id(id).role(Role::TabList).aria_label(label), triggers: Vec::new() }
    }

    pub fn trigger(mut self, trigger: TabsTrigger) -> Self {
        self.triggers.push(trigger);
        self
    }
}

impl RenderOnce for TabsList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { mut div, triggers } = self;
        let user = std::mem::take(div.style());
        let mut div = div
            .flex()
            .items_center()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(9.))
            .bg(cx.theme().frame)
            .text_size(FontSize::Medium)
            .children(triggers.into_iter().map(|trigger| -> AnyElement { trigger.into_any_element() }));
        div.style().refine(&user);
        div
    }
}

impl Styled for TabsList {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl InteractiveElement for TabsList {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for TabsList {}

/// `TabsList` 里的一段：报成 `Role::Tab`，名字是显示的文字，选没选写进 `aria_selected`。点了办什么、悬停
/// 提示由调用方接着写（经 `a11y` 的 `Press` 或 `PressDown`，辅助工具才按得动）。
#[derive(IntoElement)]
pub struct TabsTrigger {
    div: Stateful<Div>,
    label: SharedString,
    count: Option<usize>,
    selected: bool,
}

impl TabsTrigger {
    /// `id` 按这一段是哪一页取，不按排第几：前面的段藏起来时辅助工具的焦点仍跟着同一页。
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool) -> Self {
        let label = label.into();
        let div = div().id(id).role(Role::Tab).aria_label(label.clone()).aria_selected(selected);
        Self { div, label, count: None, selected }
    }

    /// 文字后面跟一个小号的数，为空时不画。数只是画出来的，要报给辅助工具时调用方另写进 `aria_description`。
    pub fn count(mut self, count: Option<usize>) -> Self {
        self.count = count;
        self
    }
}

impl RenderOnce for TabsTrigger {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, subtle, active, info) = (theme.foreground, theme.subtle_foreground, theme.tab_active, theme.info);
        let Self { mut div, label, count, selected } = self;
        let user = std::mem::take(div.style());
        let mut div = div
            .flex_1()
            .min_w_0()
            .h(scaled(26.))
            .px(px(6.))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .rounded(px(7.))
            .cursor_pointer()
            .map(|tab| {
                if selected {
                    tab.bg(active).text_color(fg).font_weight(FontWeight::MEDIUM)
                } else {
                    tab.text_color(subtle).hover(move |tab| tab.text_color(fg))
                }
            })
            .child(gpui::div().min_w_0().truncate().child(label))
            .children(count.map(|count| {
                gpui::div().flex_none().text_size(FontSize::Small).text_color(info).child(count.to_string())
            }));
        div.style().refine(&user);
        div
    }
}

impl Styled for TabsTrigger {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl InteractiveElement for TabsTrigger {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for TabsTrigger {}
