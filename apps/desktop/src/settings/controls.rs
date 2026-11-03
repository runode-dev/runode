//! 设置页里的控件：一行设置的排版，分段选项、下拉按钮、输入框和色块（按钮用 `Button`，开关用 `Switch`）。颜色在 render
//! 里从窗口的主题（`Theme`）读，和终端窗口的界面一个色调。

use crate::ui::a11y::StaticText;
use crate::ui::select::SelectTrigger;
use crate::ui::theme::Theme;
use gpui::{AnyElement, Div, ElementId, Entity, Hsla, SharedString, Stateful, div, prelude::*, px};

pub(super) use crate::ui::a11y::Press;
use crate::ui::{
    font_size::{FontSize, scaled},
    input::{Input, InputState},
};

/// 控件的高度。
pub(super) const CONTROL_HEIGHT: f32 = 26.;

/// 一页里的小标题。
pub(super) fn section(title: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .pt(px(20.))
        .pb(px(10.))
        .pl(px(4.))
        .text_size(FontSize::Large)
        .text_color(theme.foreground.opacity(0.6))
        .child(title.into())
}

/// 把一页的各项按小标题分组，每组收进一张圆角卡片。
pub(super) struct Cards {
    page: Div,
    items: Vec<AnyElement>,
    theme: Theme,
}

impl Cards {
    pub fn new(page: Div, theme: &Theme) -> Self {
        Self { page, items: Vec::new(), theme: theme.clone() }
    }

    /// 结束上一组，另起一组，上面是小标题 `title`。
    pub fn section(&mut self, title: impl Into<SharedString>) {
        self.raw(section(title, &self.theme).into_any_element());
    }

    /// 结束上一组，`element` 不进卡片，直接放在页面上。
    pub fn raw(&mut self, element: AnyElement) {
        self.flush();
        self.page = std::mem::replace(&mut self.page, div()).child(element);
    }

    pub fn push(&mut self, item: AnyElement) {
        self.items.push(item);
    }

    pub fn finish(mut self) -> Div {
        self.flush();
        self.page
    }

    fn flush(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let items = std::mem::take(&mut self.items);
        // 每行底下都有分隔线，内容往下多出一像素让卡片裁掉最后一行的那条。
        let card = div()
            .mb(px(8.))
            .px(px(16.))
            .rounded(px(10.))
            .bg(self.theme.mix(0.045))
            .border_1()
            .border_color(self.theme.control_border.opacity(0.5))
            .overflow_hidden()
            .child(div().mb(px(-1.)).flex().flex_col().children(items));
        self.page = std::mem::replace(&mut self.page, div()).child(card);
    }
}

/// 一行设置：左边是名字和说明，右边是控件；`reset` 是 runode 的配置文件里写了这一项时的恢复按钮，
/// `error` 是上次写回失败的原因，报给辅助工具。
pub(super) fn row(
    title: impl Into<SharedString>,
    hint: Option<SharedString>,
    control: impl IntoElement,
    reset: Option<AnyElement>,
    error: Option<String>,
    theme: &Theme,
) -> Div {
    let title = title.into();
    let error_id = SharedString::from(format!("row-error-{title}"));
    div()
        .py(px(12.))
        .flex()
        .items_center()
        .gap(px(16.))
        .border_b_1()
        .border_color(theme.control_border.opacity(0.6))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(div().text_size(FontSize::Medium).font_weight(gpui::FontWeight::MEDIUM).child(title))
                .children(
                    hint.map(|hint| {
                        div().text_size(FontSize::Body).text_color(theme.foreground.opacity(0.55)).child(hint)
                    }),
                )
                .children(error.map(|err| error_text(error_id, err, theme))),
        )
        // 控件贴着右边，恢复按钮有的时候放在它左边。
        .child(div().flex_none().flex().items_center().gap(px(6.)).children(reset).child(control))
}

/// 上次写回失败的原因，用 `Role::Label` 报给辅助工具；`id` 按它属于哪一项取，不按错误文字取，几项同样的
/// 错误才不会撞 id。
pub(super) fn error_text(id: impl Into<ElementId>, err: String, theme: &Theme) -> Stateful<Div> {
    div().id(id).aria_static_text(err.clone()).text_size(FontSize::Small).text_color(theme.destructive).child(err)
}

/// 下拉按钮跟着界面字号最多放大到基准宽度的这么多倍：这一行左边的说明不跟着按钮让位，按钮再宽就把说明
/// 挤成窄窄一列。
const DROPDOWN_MAX_GROWTH: f32 = 1.3;

/// 点开一个列表从里面挑的按钮：显示当前选的，`leading` 放在它左边（比如主题的色块），右边一个
/// 下拉箭头。`width` 是基准字号下的点数，跟着界面字号缩放，有上限（`DROPDOWN_MAX_GROWTH`）。
pub(super) fn dropdown(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    leading: Option<AnyElement>,
    label: impl Into<SharedString>,
    width: f32,
    theme: &Theme,
) -> SelectTrigger {
    SelectTrigger::new(id, name, label)
        .when_some(leading, SelectTrigger::leading)
        .w(scaled(width))
        .max_w(px(width * DROPDOWN_MAX_GROWTH))
        .h(scaled(CONTROL_HEIGHT))
        .pl(px(10.))
        .pr(px(6.))
        .rounded(px(6.))
        .bg(theme.mix(0.06))
        .border_1()
        .border_color(theme.control_border)
        .text_size(FontSize::Body)
        .hover(|button| button.bg(theme.mix(0.07)))
}

/// 设置页的输入框：比别处的 `Input` 矮一些，底色和别的控件一样；`width` 是基准字号下的点数，跟着界面
/// 字号缩放，为 `None` 时占满剩下的宽度。
pub(super) fn input_box(input: &Entity<InputState>, width: Option<f32>, error: bool, theme: &Theme) -> Input {
    Input::new(input)
        .invalid(error)
        .map(|field| match width {
            Some(width) => field.w(scaled(width)),
            None => field.flex_1().min_w_0(),
        })
        .h(scaled(CONTROL_HEIGHT))
        .bg(theme.mix(0.06))
        .text_size(FontSize::Body)
}

/// 色块；`color` 为 `None` 时画一条斜线，表示没设、跟随别的颜色。
pub(super) fn swatch(color: Option<Hsla>, size: f32, theme: &Theme) -> Div {
    let block = div().flex_none().size(px(size)).rounded(px(4.)).border_1().border_color(theme.control_border);
    match color {
        Some(color) => block.bg(color),
        None => block
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(size * 0.7))
            .text_color(theme.foreground.opacity(0.4))
            .child("∕"),
    }
}
