//! Git 面板下半部分的列表：每段的标题、改动的文件、展开后的块和行，以及储藏。各行的按钮平时
//! 藏着，鼠标移到行上才露出来，和 VSCode 一样。

use std::{borrow::Cow, ops::Range, path::Path};

use gpui::{
    AnyElement, Context, Div, ElementId, MouseButton, MouseDownEvent, SharedString, Stateful, Window, div, img,
    prelude::*, px, svg,
};
use runode_git_status::{self as git, FileDiff, FileStatus, HunkAction, Line, LineKind, Section, hunk_actionable};
use runode_shared_types::color::Rgb;

use super::{
    FileOp, GitFileAction,
    rows::{DiffNote, GitPanel, GitRow, GitSection, section_of},
    run::StashOp,
};
use crate::{
    assets::{
        CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, DISCARD_ICON, MINUS_ICON, OPEN_FILE_ICON, PLUS_ICON, STASH_APPLY_ICON,
        STASH_POP_ICON, TRASH_ICON,
    },
    file_icons::file_icon,
    terminal_view::hsla,
    tooltip::tooltip,
    workspace::{
        WindowView,
        files::menu_item,
        project::{ADDED, REMOVED, status_color},
    },
};

/// 每一行的高度：段标题、文件、块头和改动的行一样高，列表才能只画看得见的部分。
pub(super) const ROW_HEIGHT: f32 = 22.;
/// 行号一栏的宽度，五位数的行号也放得下。
const LINE_NUMBER_WIDTH: f32 = 44.;
/// 文件比段标题、块头比文件往右缩进的宽度。
const INDENT: f32 = 12.;
/// 鼠标移到行上才露出按钮，各行共用这个组名，按钮找的是离它最近的那一行。
const ROW_GROUP: &str = "git-row";

type Handler = Box<dyn Fn(&mut WindowView, &mut Window, &mut Context<WindowView>)>;

/// 行尾的一个图标按钮：图标、提示文字和按下时做的事。
struct RowButton {
    icon: &'static str,
    text: Cow<'static, str>,
    handler: Handler,
}

fn button(
    icon: &'static str,
    text: Cow<'static, str>,
    handler: impl Fn(&mut WindowView, &mut Window, &mut Context<WindowView>) + 'static,
) -> RowButton {
    RowButton { icon, text, handler: Box::new(handler) }
}

impl WindowView {
    /// 改动里的一行：旧行号、新行号、正负号和内容，加的和删的垫上底色。双击把「路径:行号」打进
    /// 终端，给 agent 指出是哪一行。`root` 是 `file` 的路径相对的仓库根。
    #[allow(clippy::too_many_arguments)]
    fn render_diff_line(
        &self,
        id: ElementId,
        height: f32,
        root: &Path,
        file: &FileDiff,
        line: &Line,
        fg: Rgb,
        bg: Rgb,
        font: &SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dim = hsla(fg).opacity(0.45);
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
        let target = (root.join(&file.path), line.new.or(line.old));
        div()
            .id(id)
            .flex_none()
            .h(px(height))
            .w_full()
            .flex()
            .items_center()
            .overflow_hidden()
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

    pub(super) fn render_git_rows(
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
        let panel = &project.git_panel;
        range
            .filter_map(|ix| panel.rows.get(ix).map(|row| (ix, *row)))
            .map(|(ix, row)| match row {
                GitRow::Section(section) => self.render_git_section(ix, section, git, fg, bg, cx),
                GitRow::File(section, fi) => self.render_git_file(ix, section, fi, git, fg, bg, cx),
                GitRow::Hunk(section, fi, hi) => self.render_git_hunk(ix, section, fi, hi, git, fg, bg, font, cx),
                GitRow::Line(section, fi, hi, li) => {
                    let file = &git.files(section)[fi];
                    let line = &file.hunks[hi].lines[li];
                    self.render_diff_line(("git-line", ix).into(), ROW_HEIGHT, &git.root, file, line, fg, bg, font, cx)
                }
                GitRow::Note(_, _, note) => diff_note_row(note, ROW_HEIGHT, fg),
                GitRow::Stash(si) => self.render_git_stash(ix, si, git, fg, bg, cx),
            })
            .collect()
    }

    /// 一行的外框：定高、横着排、鼠标移上去时底色变亮，按钮跟着露出来。
    fn git_row(&self, id: impl Into<ElementId>, pl: f32, fg: Rgb, bg: Rgb) -> Stateful<Div> {
        div()
            .id(id)
            .group(ROW_GROUP)
            .flex_none()
            .h(px(ROW_HEIGHT))
            .w_full()
            .pl(px(8. + pl))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .overflow_hidden()
            .text_color(hsla(fg))
            .hover(|row| row.bg(hsla(bg.mix(fg, 0.06))))
    }

    /// 行尾的按钮，平时藏着；有操作在跑时按不动。
    fn row_buttons(&self, buttons: Vec<RowButton>, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let enabled = self.workspace().project.git_panel.busy.is_none();
        let hover_bg = hsla(bg.mix(fg, 0.14));
        let icon_color = hsla(fg).opacity(if enabled { 0.75 } else { 0.3 });
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .invisible()
            .group_hover(ROW_GROUP, |buttons| buttons.visible())
            .children(buttons.into_iter().enumerate().map(|(bi, button)| {
                let handler = button.handler;
                div()
                    .id(("git-row-button", bi))
                    .flex_none()
                    .size(px(20.))
                    .rounded(px(3.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .tooltip(tooltip(button.text, None, fg, bg))
                    .child(svg().path(button.icon).size(px(14.)).text_color(icon_color))
                    .when(enabled, |button| {
                        button.hover(|button| button.bg(hover_bg)).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                handler(this, window, cx);
                            }),
                        )
                    })
            }))
    }

    fn render_git_section(
        &self,
        ix: usize,
        section: GitSection,
        git: &git::Snapshot,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = &self.workspace().project.git_panel;
        let (label, count) = match section {
            GitSection::Merge => (rust_i18n::t!("git.section.merge"), GitPanel::files(git, section).len()),
            GitSection::Staged => (rust_i18n::t!("git.section.staged"), GitPanel::files(git, section).len()),
            GitSection::Unstaged => (rust_i18n::t!("git.section.unstaged"), GitPanel::files(git, section).len()),
            GitSection::Stashes => (rust_i18n::t!("git.section.stashes"), git.info.stashes.len()),
        };
        let buttons = match section {
            GitSection::Merge => vec![button(PLUS_ICON, rust_i18n::t!("git.stage_all"), |this, window, cx| {
                this.git_section_action(GitSection::Merge, true, window, cx);
            })],
            GitSection::Staged => vec![button(MINUS_ICON, rust_i18n::t!("git.unstage_all"), |this, window, cx| {
                this.git_section_action(GitSection::Staged, false, window, cx);
            })],
            GitSection::Unstaged => vec![
                button(DISCARD_ICON, rust_i18n::t!("git.discard_all"), |this, window, cx| {
                    this.git_section_action(GitSection::Unstaged, false, window, cx);
                }),
                button(PLUS_ICON, rust_i18n::t!("git.stage_all"), |this, window, cx| {
                    this.git_section_action(GitSection::Unstaged, true, window, cx);
                }),
            ],
            GitSection::Stashes => Vec::new(),
        };
        let dim = hsla(fg).opacity(0.5);
        self.git_row(("git-section", ix), 0., fg, bg)
            .child(chevron(panel.section_expanded(section), fg))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(label.into_owned()),
            )
            .child(self.row_buttons(buttons, fg, bg, cx))
            .child(
                div()
                    .flex_none()
                    .min_w(px(18.))
                    .h(px(16.))
                    .px(px(5.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(hsla(bg.mix(fg, 0.12)))
                    .text_size(px(10.))
                    .text_color(dim)
                    .child(count.to_string()),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let project = &mut this.workspace_mut().project;
                    project.git_panel.toggle_section(section, project.git.as_ref());
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_git_file(
        &self,
        ix: usize,
        section: Section,
        fi: usize,
        git: &git::Snapshot,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let file = &git.files(section)[fi];
        let shown_in = section_of(file, section);
        let expanded = self.workspace().project.git_panel.file_expanded(section, &file.path);
        let name = file_name(&file.path);
        let dir = file.path.parent().map(|dir| dir.display().to_string()).unwrap_or_default();
        let action = |op| GitFileAction { op, section, path: file.path.clone() };
        let file_button = |icon, key: &'static str, op| {
            let action = action(op);
            button(icon, rust_i18n::t!(key), move |this, window, cx| this.git_file_action(&action, window, cx))
        };
        let mut buttons = vec![file_button(OPEN_FILE_ICON, "git.open_file", FileOp::Open)];
        match shown_in {
            GitSection::Merge => buttons.push(file_button(PLUS_ICON, "git.mark_resolved", FileOp::Stage)),
            GitSection::Staged => buttons.push(file_button(MINUS_ICON, "git.unstage", FileOp::Unstage)),
            _ => {
                buttons.push(file_button(DISCARD_ICON, "git.discard", FileOp::Discard));
                buttons.push(file_button(PLUS_ICON, "git.stage", FileOp::Stage));
            }
        }
        let path = file.path.clone();
        let dim = hsla(fg).opacity(0.5);
        let deleted = file.status == FileStatus::Deleted;
        self.git_row(("git-file", ix), INDENT, fg, bg)
            .child(chevron(expanded, fg))
            .child(img(file_icon(&name)).flex_none().size(px(14.)))
            .child(
                div()
                    .flex_initial()
                    .min_w_0()
                    .truncate()
                    .when(deleted, |name| name.line_through().text_color(dim))
                    .child(name),
            )
            .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(dim).child(dir))
            .child(self.row_buttons(buttons, fg, bg, cx))
            .child(
                div()
                    .flex_none()
                    .w(px(12.))
                    .flex()
                    .justify_center()
                    .text_color(hsla(status_color(file.status)))
                    .child(file.status.letter()),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener({
                    let path = path.clone();
                    move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        if event.click_count >= 2 {
                            let action = GitFileAction { op: FileOp::Open, section, path: path.clone() };
                            this.git_file_action(&action, window, cx);
                            return;
                        }
                        let project = &mut this.workspace_mut().project;
                        project.git_panel.toggle_file(section, &path, project.git.as_ref());
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_git_file_menu(event.position, section, shown_in, path.clone(), cx);
                }),
            )
            .into_any_element()
    }

    /// 文件的右键菜单。
    fn open_git_file_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        section: Section,
        shown_in: GitSection,
        path: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        let enabled = self.workspace().project.git_panel.busy.is_none();
        let item = |key: &str, op, enabled| {
            Some(menu_item(key, Box::new(GitFileAction { op, section, path: path.clone() }), enabled, cx))
        };
        let mut items = vec![
            item("git.open_file", FileOp::Open, true),
            item("files.reveal", FileOp::Reveal, true),
            item("files.insert_path", FileOp::InsertPath, true),
            None,
        ];
        match shown_in {
            GitSection::Merge => items.push(item("git.mark_resolved", FileOp::Stage, enabled)),
            GitSection::Staged => items.push(item("git.unstage", FileOp::Unstage, enabled)),
            _ => {
                items.extend([item("git.stage", FileOp::Stage, enabled), item("git.discard", FileOp::Discard, enabled)])
            }
        }
        let target = self.git_focus.clone();
        self.open_menu(position, items, target, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_git_hunk(
        &self,
        ix: usize,
        section: Section,
        fi: usize,
        hi: usize,
        git: &git::Snapshot,
        fg: Rgb,
        bg: Rgb,
        font: &SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let file = &git.files(section)[fi];
        let mut buttons = Vec::new();
        if hunk_actionable(file) {
            let hunk_button = |icon, key: &'static str, action: HunkAction| {
                let path = file.path.clone();
                button(icon, rust_i18n::t!(key), move |this, window, cx| {
                    this.git_hunk_action(section, path.clone(), hi, action, window, cx);
                })
            };
            match section {
                Section::Staged => buttons.push(hunk_button(MINUS_ICON, "git.unstage_hunk", HunkAction::Unstage)),
                Section::Unstaged => {
                    buttons.push(hunk_button(DISCARD_ICON, "git.discard_hunk", HunkAction::Discard));
                    buttons.push(hunk_button(PLUS_ICON, "git.stage_hunk", HunkAction::Stage));
                }
            }
        }
        self.git_row(("git-hunk", ix), INDENT * 2., fg, bg)
            .bg(hsla(bg.mix(fg, 0.03)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(font.clone())
                    .text_color(hsla(fg).opacity(0.45))
                    .child(file.hunks[hi].header.clone()),
            )
            .child(self.row_buttons(buttons, fg, bg, cx))
            .into_any_element()
    }

    fn render_git_stash(
        &self,
        ix: usize,
        si: usize,
        git: &git::Snapshot,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stash = &git.info.stashes[si];
        let index = stash.index;
        let stash_button = |icon, key: &'static str, op: StashOp| {
            button(icon, rust_i18n::t!(key), move |this, window, cx| this.git_stash_action(index, op, window, cx))
        };
        let buttons = vec![
            stash_button(STASH_APPLY_ICON, "git.stash_apply", StashOp::Apply),
            stash_button(STASH_POP_ICON, "git.stash_pop", StashOp::Pop),
            stash_button(TRASH_ICON, "git.stash_drop", StashOp::Drop),
        ];
        self.git_row(("git-stash", ix), INDENT, fg, bg)
            .child(div().flex_none().text_size(px(11.)).text_color(hsla(fg).opacity(0.5)).child(format!("#{index}")))
            .child(div().flex_1().min_w_0().truncate().child(stash.message.clone()))
            .child(self.row_buttons(buttons, fg, bg, cx))
            .into_any_element()
    }
}

fn chevron(expanded: bool, fg: Rgb) -> gpui::Svg {
    svg()
        .flex_none()
        .path(if expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
        .size(px(12.))
        .text_color(hsla(fg).opacity(0.45))
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// 改动里不显示行的说明：二进制文件、改动太多、内容没变。和行的内容对齐。
fn diff_note_row(note: DiffNote, height: f32, fg: Rgb) -> AnyElement {
    let text = match note {
        DiffNote::Binary => rust_i18n::t!("panel.binary"),
        DiffNote::Truncated => rust_i18n::t!("panel.truncated"),
        DiffNote::NoContent => rust_i18n::t!("panel.no_content"),
    };
    div()
        .flex_none()
        .h(px(height))
        .w_full()
        .flex()
        .items_center()
        .overflow_hidden()
        .pl(px(LINE_NUMBER_WIDTH * 2. + 14.))
        .italic()
        .text_color(hsla(fg).opacity(0.45))
        .child(text.into_owned())
        .into_any_element()
}
