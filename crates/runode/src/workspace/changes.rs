//! 右侧的改动栏：工作区相对 HEAD 改了哪些文件，逐个展开看改动的行。

use std::ops::Range;

use gpui::{
    AnyElement, Context, Div, MouseButton, MouseDownEvent, SharedString, Stateful, div, prelude::*, px, svg,
    uniform_list,
};

use super::{
    WindowView,
    project::{ADDED, DiffNote, DiffRow, REMOVED, status_color},
};
use crate::{
    assets::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON},
    git::LineKind,
    session::Rgb,
    terminal_view::hsla,
};

/// 每一行的高度：文件、块头和改动的行一样高，列表才能只画看得见的部分。
const ROW_HEIGHT: f32 = 20.;
/// 行号一栏的宽度，五位数的行号也放得下。
const LINE_NUMBER_WIDTH: f32 = 44.;

impl WindowView {
    pub(super) fn render_changes_panel(
        &self,
        width: f32,
        rightmost: bool,
        fg: Rgb,
        bg: Rgb,
        font: SharedString,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let project = &self.workspace().project;
        let dim = hsla(fg).opacity(0.5);
        let mut header = self
            .panel_header(rightmost, fg)
            .child(div().flex_none().text_color(hsla(fg)).child(rust_i18n::t!("panel.changes").into_owned()));
        if let Some(git) = project.git.as_ref().filter(|git| !git.files.is_empty()) {
            header = header
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(dim)
                        .child(rust_i18n::t!("panel.uncommitted", count = git.files.len()).into_owned()),
                )
                .child(div().flex_none().text_color(hsla(ADDED)).child(format!("+{}", git.added())))
                .child(div().flex_none().text_color(hsla(REMOVED)).child(format!("−{}", git.removed())));
        }
        let message = |text: String| {
            div().flex_1().flex().items_center().justify_center().px(px(16.)).text_color(dim).child(text)
        };
        let body: AnyElement = match &project.git {
            _ if !project.loaded => div().flex_1().into_any_element(),
            None => message(rust_i18n::t!("panel.not_repo").into_owned()).into_any_element(),
            Some(git) if git.files.is_empty() => {
                message(rust_i18n::t!("panel.no_changes").into_owned()).into_any_element()
            }
            Some(_) => uniform_list(
                "changes",
                project.diff_rows.len(),
                cx.processor(move |this, range: Range<usize>, _, cx| this.render_diff_rows(range, fg, bg, &font, cx)),
            )
            .track_scroll(&project.changes_scroll)
            .flex_1()
            .into_any_element(),
        };
        div()
            .id("changes-panel")
            .flex_none()
            .w(px(width))
            .h_full()
            .flex()
            .flex_col()
            .bg(hsla(bg))
            .border_l_1()
            .border_color(hsla(fg).opacity(0.12))
            .text_size(px(12.))
            .child(header)
            .child(body)
    }

    fn render_diff_rows(
        &self,
        range: Range<usize>,
        fg: Rgb,
        bg: Rgb,
        font: &SharedString,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let project = &self.workspace().project;
        let Some(git) = &project.git else {
            return Vec::new();
        };
        let dim = hsla(fg).opacity(0.45);
        let row = || div().flex_none().h(px(ROW_HEIGHT)).w_full().flex().items_center().overflow_hidden();
        range
            .filter_map(|ix| project.diff_rows.get(ix).map(|row| (ix, *row)))
            .map(|(ix, diff_row)| match diff_row {
                DiffRow::File(fi) => {
                    let file = &git.files[fi];
                    let path = file.path.clone();
                    let expanded = project.diff_expanded(file);
                    let name = match &file.old_path {
                        Some(old) => format!("{} → {}", old.display(), file.path.display()),
                        None => file.path.display().to_string(),
                    };
                    row()
                        .id(("diff-file", ix))
                        .px(px(8.))
                        .gap(px(6.))
                        .bg(hsla(bg.mix(fg, 0.06)))
                        .border_t_1()
                        .border_color(hsla(fg).opacity(0.10))
                        .text_color(hsla(fg))
                        .hover(|row| row.bg(hsla(bg.mix(fg, 0.10))))
                        .child(
                            svg()
                                .flex_none()
                                .path(if expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
                                .size(px(12.))
                                .text_color(dim),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(10.))
                                .text_color(hsla(status_color(file.status)))
                                .child(file.status.letter()),
                        )
                        // 路径长时留下结尾：文件名最要紧。
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_start()
                                .child(name),
                        )
                        .when(file.added > 0, |row| {
                            row.child(div().flex_none().text_color(hsla(ADDED)).child(format!("+{}", file.added)))
                        })
                        .when(file.removed > 0, |row| {
                            row.child(div().flex_none().text_color(hsla(REMOVED)).child(format!("−{}", file.removed)))
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.workspace_mut().project.toggle_diff(&path);
                                cx.notify();
                            }),
                        )
                        .into_any_element()
                }
                DiffRow::Hunk(fi, hi) => row()
                    .px(px(8.))
                    .bg(hsla(bg.mix(fg, 0.03)))
                    .font_family(font.clone())
                    .text_color(dim)
                    .whitespace_nowrap()
                    .child(git.files[fi].hunks[hi].header.clone())
                    .into_any_element(),
                DiffRow::Line(fi, hi, li) => {
                    let file = &git.files[fi];
                    let line = &file.hunks[hi].lines[li];
                    let (sign, line_bg, sign_color) = match line.kind {
                        LineKind::Added => ("+", Some(bg.mix(ADDED, 0.16)), hsla(ADDED)),
                        LineKind::Removed => ("-", Some(bg.mix(REMOVED, 0.16)), hsla(REMOVED)),
                        LineKind::Context => (" ", None, dim),
                    };
                    let number = |n: Option<u32>| {
                        div()
                            .flex_none()
                            .w(px(LINE_NUMBER_WIDTH))
                            .pr(px(8.))
                            .flex()
                            .justify_end()
                            .text_color(dim)
                            .children(n.map(|n| n.to_string()))
                    };
                    // 双击一行把「路径:行号」打进终端，给 agent 指出是哪一行。
                    let target = (git.root.join(&file.path), line.new.or(line.old));
                    row()
                        .id(("diff-line", ix))
                        .font_family(font.clone())
                        .when_some(line_bg, |row, line_bg| row.bg(hsla(line_bg)))
                        .child(number(line.old))
                        .child(number(line.new))
                        .child(div().flex_none().w(px(14.)).text_color(sign_color).child(sign))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_color(hsla(fg))
                                .child(SharedString::from(line.text.clone())),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                if event.click_count >= 2 {
                                    this.insert_path(&target.0, target.1, window, cx);
                                }
                            }),
                        )
                        .into_any_element()
                }
                DiffRow::Note(_, note) => {
                    let text = match note {
                        DiffNote::Binary => rust_i18n::t!("panel.binary"),
                        DiffNote::Truncated => rust_i18n::t!("panel.truncated"),
                        DiffNote::NoContent => rust_i18n::t!("panel.no_content"),
                    };
                    row()
                        .pl(px(LINE_NUMBER_WIDTH * 2. + 14.))
                        .italic()
                        .text_color(dim)
                        .child(text.into_owned())
                        .into_any_element()
                }
            })
            .collect()
    }
}
