//! 输入框加一列可选行的选择器共用的几样：挪选中行的键、按词过滤、浮在标题栏下方的外框。

use gpui::{App, Div, Entity, KeyDownEvent, Stateful, div, prelude::*, px};

use crate::{font_size::scaled, input::InputState, panel::TITLEBAR_HEIGHT, popover::Popover, theme::ActiveTheme as _};

/// 选择器顶上输入框那一行的高度，基准字号下的点数。
const INPUT_HEIGHT: f32 = 34.;

/// 选择列表里挪选中行的键：上下键和 ctrl-p、ctrl-n，往上是 -1，往下是 1。
pub fn nav_delta(event: &KeyDownEvent) -> Option<isize> {
    let keystroke = &event.keystroke;
    let m = &keystroke.modifiers;
    let ctrl = m.control && !m.alt && !m.shift && !m.platform;
    match keystroke.key.as_str() {
        "up" if !m.modified() => Some(-1),
        "down" if !m.modified() => Some(1),
        "p" if ctrl => Some(-1),
        "n" if ctrl => Some(1),
        _ => None,
    }
}

/// `query` 按空白分成几个词，每个词（不分大小写）都出现在 `fields` 的某一项里才算匹配；没有词时都匹配。
pub fn matches_query<S: AsRef<str>>(fields: &[S], query: &str) -> bool {
    let fields: Vec<String> = fields.iter().map(|field| field.as_ref().to_lowercase()).collect();
    query.split_whitespace().map(str::to_lowercase).all(|term| fields.iter().any(|field| field.contains(&term)))
}

/// 浮在标题栏下方正中的选择器：`panel` 是调用方定好 id、宽度和按键处理的外框，上面一行放输入框，
/// 下面是 `list`（调用方定好 id、最高多高、各行和滚动位置），多了竖着滚动。
pub fn picker_panel(panel: Popover, field: Entity<InputState>, list: Stateful<Div>, cx: &App) -> Div {
    let panel = panel
        .max_w_full()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .h(scaled(INPUT_HEIGHT))
                .px(px(10.))
                .flex()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(div().flex_1().min_w_0().h_full().child(field)),
        )
        .child(list.overflow_y_scroll().p(px(4.)).flex().flex_col().gap(px(1.)));
    // 外层铺满窗口宽度、只为把列表摆到正中，没有鼠标处理，不挡下面的点击。
    div().absolute().top(px(TITLEBAR_HEIGHT + 8.)).left_0().right_0().px(px(16.)).flex().justify_center().child(panel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_term_must_match_some_field() {
        let fields = ["Claude Code", "claude", "runode", "~/dev/runode"];
        assert!(matches_query(&fields, ""));
        assert!(matches_query(&fields, "CLAUDE"));
        assert!(matches_query(&fields, "claude dev/run"));
        assert!(!matches_query(&fields, "claude codex"));
    }
}
