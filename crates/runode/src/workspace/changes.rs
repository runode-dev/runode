//! 右侧的改动栏：工作区相对 HEAD 改了哪些文件，有暂存的改动时分成已暂存、未暂存两段，
//! 每段里按目录分组，逐个展开看改动的行。

use std::{ops::Range, path::Path};

use gpui::{
    AnyElement, Context, Div, MouseButton, MouseDownEvent, SharedString, Stateful, div, img, prelude::*, px, svg,
    uniform_list,
};
use runode_git::{LineKind, Section};
use runode_model::color::Rgb;

use super::{
    WindowView,
    project::{ADDED, DiffNote, DiffRow, REMOVED, status_color},
};
use crate::{
    assets::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON},
    file_icons::{file_icon, folder_icon},
    terminal_view::hsla,
};

/// 每一行的高度：文件、块头和改动的行一样高，列表才能只画看得见的部分。
const ROW_HEIGHT: f32 = 20.;
/// 行号一栏的宽度，五位数的行号也放得下。
const LINE_NUMBER_WIDTH: f32 = 44.;
/// 分段、目录分组和文件每深一层往右缩进的宽度。
const INDENT: f32 = 12.;

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
        if let Some(git) = project.git.as_ref().filter(|git| !git.is_clean()) {
            header = header
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(dim)
                        .child(rust_i18n::t!("panel.uncommitted", count = git.changed()).into_owned()),
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
            Some(git) if git.is_clean() => {
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
        // 分段时目录分组和文件都往里缩一层。
        let base = if project.split_sections() { INDENT } else { 0. };
        let row = || div().flex_none().h(px(ROW_HEIGHT)).w_full().flex().items_center().overflow_hidden();
        let chevron = |expanded: bool| {
            svg()
                .flex_none()
                .path(if expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
                .size(px(12.))
                .text_color(dim)
        };
        let icon = |path: &'static str| img(path).flex_none().size(px(14.));
        // 分段、目录分组和文件的标题行：点一下展开或收起。
        let header = |id: (&'static str, usize), pl: f32, shade: f32| {
            row()
                .id(id)
                .pl(px(8. + pl))
                .pr(px(8.))
                .gap(px(6.))
                .bg(hsla(bg.mix(fg, shade)))
                .text_color(hsla(fg))
                .hover(|row| row.bg(hsla(bg.mix(fg, shade + 0.04))))
        };
        range
            .filter_map(|ix| project.diff_rows.get(ix).map(|row| (ix, *row)))
            .map(|(ix, diff_row)| match diff_row {
                DiffRow::Section(section) => {
                    let (label, count) = match section {
                        Section::Staged => (rust_i18n::t!("panel.staged"), git.staged.len()),
                        Section::Unstaged => (rust_i18n::t!("panel.unstaged"), git.unstaged.len()),
                    };
                    header(("diff-section", ix), 0., 0.08)
                        .border_t_1()
                        .border_color(hsla(fg).opacity(0.10))
                        .child(chevron(project.section_expanded(section)))
                        .child(div().flex_none().font_weight(gpui::FontWeight::SEMIBOLD).child(label.into_owned()))
                        .child(div().flex_none().text_color(dim).child(count.to_string()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.workspace_mut().project.toggle_section(section);
                                cx.notify();
                            }),
                        )
                        .into_any_element()
                }
                DiffRow::Group(gi) => {
                    let group = &project.diff_groups[gi];
                    let (section, dir) = (group.section, group.dir.clone());
                    // 仓库根下的文件归在仓库名下面。
                    let name = dir.file_name().or(git.root.file_name()).map(|name| name.to_string_lossy()).unwrap_or_default();
                    let label =
                        if dir.as_os_str().is_empty() { name.to_string() } else { dir.display().to_string() };
                    header(("diff-group", ix), base, 0.04)
                        .child(chevron(group.expanded))
                        .child(icon(folder_icon(&name, group.expanded)))
                        // 路径长时留下结尾：最里层的目录最要紧。
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_start()
                                .child(label),
                        )
                        .child(div().flex_none().text_color(dim).child(group.files.to_string()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.workspace_mut().project.toggle_group(section, &dir);
                                cx.notify();
                            }),
                        )
                        .into_any_element()
                }
                DiffRow::File(section, fi) => {
                    let file = &git.files(section)[fi];
                    let path = file.path.clone();
                    let expanded = project.diff_expanded(section, file);
                    let file_name = |path: &Path| path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
                    let name = file_name(&file.path);
                    // 改名时写上原来的名字，换了目录的写原来的完整路径。
                    let label = match &file.old_path {
                        Some(old) if old.parent() == file.path.parent() => format!("{} → {name}", file_name(old)),
                        Some(old) => format!("{} → {name}", old.display()),
                        None => name.clone(),
                    };
                    header(("diff-file", ix), base + INDENT, 0.)
                        .child(chevron(expanded))
                        .child(icon(file_icon(&name)))
                        .child(
                            div()
                                .flex_none()
                                .w(px(10.))
                                .text_color(hsla(status_color(file.status)))
                                .child(file.status.letter()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_start()
                                .child(label),
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
                                this.workspace_mut().project.toggle_diff(section, &path);
                                cx.notify();
                            }),
                        )
                        .into_any_element()
                }
                DiffRow::Hunk(section, fi, hi) => row()
                    .px(px(8.))
                    .bg(hsla(bg.mix(fg, 0.03)))
                    .font_family(font.clone())
                    .text_color(dim)
                    .whitespace_nowrap()
                    .child(git.files(section)[fi].hunks[hi].header.clone())
                    .into_any_element(),
                DiffRow::Line(section, fi, hi, li) => {
                    let file = &git.files(section)[fi];
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
                DiffRow::Note(_, _, note) => {
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
