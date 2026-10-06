//! 设置窗口里的控件：一行设置的排版，开关、分段选项、按钮、下拉按钮、输入框和色块。颜色取自
//! 终端的前景和背景，和终端窗口的界面一个色调。

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, Entity, Hsla, SharedString, Stateful, Window, div, prelude::*, px,
    svg,
};
use runode_config::Config;
use runode_shared_types::color::Rgb;

use super::SettingsView;
use crate::ui::{hsla, text_field::TextField};

/// 控件的高度。
pub(super) const CONTROL_HEIGHT: f32 = 26.;

#[derive(Clone, Copy)]
pub(super) struct Colors {
    /// 终端的前景、背景色，悬停提示要原样的颜色。
    pub fg_rgb: Rgb,
    pub bg_rgb: Rgb,
    pub fg: Hsla,
    pub bg: Hsla,
    /// 侧栏和浮层的底色。
    pub panel: Hsla,
    pub hover: Hsla,
    pub selected: Hsla,
    /// 输入框和按钮的底色。
    pub control: Hsla,
    pub border: Hsla,
    /// 开关打开时、正在录快捷键时的强调色：调色板里的蓝色。
    pub accent: Hsla,
    pub error: Hsla,
}

impl Colors {
    pub fn new(config: &Config) -> Self {
        let (fg, bg) = (config.foreground, config.background);
        let ansi = |ix: u8, fallback: Rgb| config.palette.iter().rfind(|(i, _)| *i == ix).map_or(fallback, |(_, c)| *c);
        Self {
            fg_rgb: fg,
            bg_rgb: bg,
            fg: hsla(fg),
            bg: hsla(bg),
            panel: hsla(bg.mix(fg, 0.035)),
            hover: hsla(bg.mix(fg, 0.07)),
            selected: hsla(bg.mix(fg, 0.12)),
            control: hsla(bg.mix(fg, 0.06)),
            border: hsla(fg).opacity(0.13),
            accent: hsla(ansi(4, Rgb(0x3b, 0x82, 0xf6))),
            error: hsla(ansi(1, Rgb(0xe0, 0x50, 0x50))),
        }
    }
}

type Handler = Box<dyn Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static>;

/// 点击时调 `f`。
pub(super) fn on_click(
    cx: &mut Context<SettingsView>,
    f: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static {
    let f: Handler = Box::new(f);
    cx.listener(move |this, _: &ClickEvent, window, cx| f(this, window, cx))
}

/// 一页里的小标题。
pub(super) fn section(title: impl Into<SharedString>, colors: Colors) -> Div {
    div()
        .pt(px(22.))
        .pb(px(6.))
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(colors.fg.opacity(0.55))
        .child(title.into())
}

/// 一行设置：左边是名字和说明，右边是控件；`reset` 是 runode 的配置文件里写了这一项时的恢复按钮，
/// `error` 是上次写回失败的原因。
pub(super) fn row(
    title: impl Into<SharedString>,
    hint: Option<SharedString>,
    control: impl IntoElement,
    reset: Option<AnyElement>,
    error: Option<String>,
    colors: Colors,
) -> Div {
    div()
        .py(px(10.))
        .flex()
        .items_center()
        .gap(px(16.))
        .border_b_1()
        .border_color(colors.border.opacity(0.6))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().child(title.into()))
                .children(hint.map(|hint| div().text_size(px(11.5)).text_color(colors.fg.opacity(0.55)).child(hint)))
                .children(error.map(|err| div().text_size(px(11.5)).text_color(colors.error).child(err))),
        )
        .child(div().flex_none().flex().items_center().gap(px(6.)).child(control).child(
            // 恢复按钮占的位置一直留着，有没有它控件都对齐。
            div().w(px(20.)).flex().justify_center().children(reset),
        ))
}

/// 恢复按钮：删掉 runode 配置文件里写的这一项。
pub(super) fn reset_button(
    id: impl Into<ElementId>,
    colors: Colors,
    cx: &mut Context<SettingsView>,
    f: impl Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>) + 'static,
) -> AnyElement {
    icon_button(id, "icons/refresh.svg", colors)
        .tooltip(crate::ui::tooltip::tooltip(
            rust_i18n::t!("settings.reset").into_owned(),
            None,
            colors.fg_rgb,
            colors.bg_rgb,
        ))
        .on_click(on_click(cx, f))
        .into_any_element()
}

pub(super) fn icon_button(id: impl Into<ElementId>, icon: &'static str, colors: Colors) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .hover(|button| button.bg(colors.hover))
        .child(svg().path(icon).size(px(12.)).text_color(colors.fg.opacity(0.6)))
}

/// 开关。
pub(super) fn switch(id: impl Into<ElementId>, on: bool, colors: Colors) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(34.))
        .h(px(20.))
        .flex_none()
        .rounded_full()
        .p(px(2.))
        .flex()
        .when(on, |track| track.justify_end())
        .bg(if on { colors.accent } else { colors.fg.opacity(0.18) })
        .child(div().size(px(16.)).rounded_full().bg(gpui::white()).shadow_sm())
}

/// 分段选项：几个挨在一起的按钮，选中的那个高亮。`options` 是 (值, 显示的文字)。
pub(super) fn segmented(
    id: &'static str,
    options: Vec<(String, SharedString)>,
    selected: Option<&str>,
    colors: Colors,
    cx: &mut Context<SettingsView>,
    pick: impl Fn(&mut SettingsView, &str, &mut Window, &mut Context<SettingsView>) + Clone + 'static,
) -> Div {
    div()
        .flex()
        .h(px(CONTROL_HEIGHT))
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(7.))
        .bg(colors.control)
        .border_1()
        .border_color(colors.border)
        .children(options.into_iter().enumerate().map(|(ix, (value, label))| {
            let on = selected == Some(value.as_str());
            let pick = pick.clone();
            div()
                .id((id, ix))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(5.))
                .text_size(px(12.))
                .map(|item| {
                    if on {
                        item.bg(colors.selected).text_color(colors.fg)
                    } else {
                        item.text_color(colors.fg.opacity(0.7)).hover(|item| item.bg(colors.hover))
                    }
                })
                .on_click(on_click(cx, move |this, window, cx| pick(this, &value, window, cx)))
                .child(label)
        }))
}

/// 普通按钮。
pub(super) fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, colors: Colors) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(CONTROL_HEIGHT))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .bg(colors.control)
        .border_1()
        .border_color(colors.border)
        .text_size(px(12.))
        .hover(|button| button.bg(colors.hover))
        .child(label.into())
}

/// 点开一个列表从里面挑的按钮：显示当前选的，右边一个下拉箭头。
pub(super) fn dropdown(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    width: f32,
    colors: Colors,
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(width))
        .h(px(CONTROL_HEIGHT))
        .pl(px(10.))
        .pr(px(6.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .bg(colors.control)
        .border_1()
        .border_color(colors.border)
        .text_size(px(12.))
        .hover(|button| button.bg(colors.hover))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .child(svg().flex_none().path("icons/chevron-down.svg").size(px(12.)).text_color(colors.fg.opacity(0.6)))
}

/// 输入框的外框。
pub(super) fn input_box(input: Entity<TextField>, width: f32, error: bool, colors: Colors) -> Div {
    div()
        .w(px(width))
        .h(px(CONTROL_HEIGHT))
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .bg(colors.control)
        .border_1()
        .border_color(if error { colors.error } else { colors.border })
        .text_size(px(12.))
        .child(div().flex_1().min_w_0().h_full().child(input))
}

/// 色块；`color` 为 `None` 时画一条斜线，表示没设、跟随别的颜色。
pub(super) fn swatch(color: Option<Hsla>, size: f32, colors: Colors) -> Div {
    let block = div().flex_none().size(px(size)).rounded(px(4.)).border_1().border_color(colors.border);
    match color {
        Some(color) => block.bg(color),
        None => block
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(size * 0.7))
            .text_color(colors.fg.opacity(0.4))
            .child("∕"),
    }
}
