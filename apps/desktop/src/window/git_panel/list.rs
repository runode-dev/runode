//! Git 面板中间的列表：每段的标题、改动的文件和储藏；面板底部图表的各行也由 `render_git_row` 画。各行的按钮平时藏着，鼠标移到行上才
//! 露出来，和 VSCode 一样。每一行都属于某个仓库，按钮作用到那个仓库。点文件在预览栏里看它的整篇
//! diff。

use std::{
    borrow::Cow,
    ops::Range,
    path::{Path, PathBuf},
};

use gpui::{
    AnyElement, Context, Div, ElementId, MouseButton, MouseDownEvent, Stateful, Window, div, img, prelude::*, px, svg,
};
use runode_git::{self as git, DiffSide, FileStatus, Section};
use runode_shared_types::color::Rgb;

use super::{
    DirOp, FileOp, GitDirAction, GitFileAction,
    rows::{DirOwner, GitPanel, GitRow, GitSection, section_of},
    run::StashOp,
};
use crate::{
    assets::{
        CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, DISCARD_ICON, MINUS_ICON, OPEN_FILE_ICON, PLUS_ICON, STASH_APPLY_ICON,
        STASH_POP_ICON, TRASH_ICON,
    },
    ui::{
        file_icons::{file_icon, folder_icon},
        hsla,
        tooltip::tooltip,
    },
    window::{WindowView, files::menu_item, model::base_name, preview::DiffTarget, project::status_color},
};

/// 每一行的高度：段标题、文件、提交都一样高，列表才能只画看得见的部分。
pub(super) const ROW_HEIGHT: f32 = 22.;
/// 文件比段标题往右缩进的宽度，树形式里每深一层再缩进这么多。
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
    /// 只有一个仓库时列表里看得见的那些行。
    pub(super) fn render_git_rows(
        &self,
        range: Range<usize>,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let project = &self.workspace().project;
        let Some(git) = &project.git else {
            return Vec::new();
        };
        let panel = &project.git_panel;
        range
            .filter_map(|ix| panel.rows.get(ix).map(|row| (ix, *row)))
            .map(|(ix, row)| self.render_git_row(ix, row, git, fg, bg, cx))
            .collect()
    }

    /// 第 `ix` 行；图表里的行 `ix` 是在 `GitPanel::graph_rows` 里的下标。块头不在这里画，见
    /// `render_repo_block`。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_git_row(
        &self,
        ix: usize,
        row: GitRow,
        git: &git::Repos,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(repo) = git.get(row.repo()) else {
            return div().into_any_element();
        };
        match row {
            GitRow::Repo(_) => div().into_any_element(),
            GitRow::Section(_, section) => self.render_git_section(ix, section, repo, fg, bg, cx),
            GitRow::File(_, section, fi) => self.render_git_file(ix, section, fi, repo, fg, bg, cx),
            GitRow::Stash(_, si) => self.render_git_stash(ix, si, repo, fg, bg, cx),
            GitRow::Dir(_, di) => match self.workspace().project.git_panel.dirs.get(di).map(|dir| dir.owner) {
                Some(DirOwner::Section(section)) => self.render_git_dir(ix, di, section, fg, bg, cx),
                Some(DirOwner::Commit(_)) => self.render_commit_dir(ix, di, fg, bg, cx),
                None => div().into_any_element(),
            },
            GitRow::Clean(_) => div()
                .flex_none()
                .h(px(ROW_HEIGHT))
                .w_full()
                .pl(px(8. + INDENT))
                .flex()
                .items_center()
                .italic()
                .text_color(hsla(fg).opacity(0.45))
                .child(rust_i18n::t!("panel.no_changes").into_owned())
                .into_any_element(),
            GitRow::Commit(_, ci) => self.render_commit(ix, repo, ci, fg, bg, cx),
            GitRow::CommitFile(_, ci, fi) => self.render_commit_file(ix, repo, ci, fi, fg, bg, cx),
            GitRow::CommitNote(_, _, note) => self.render_commit_note(ix, repo, note, fg, bg),
            GitRow::GraphNote(_, note) => self.render_graph_note(ix, repo, note, fg, bg, cx),
        }
    }

    /// 一行的外框：定高、横着排、鼠标移上去时底色变亮，按钮跟着露出来。提交图里的行也用它，
    /// 图紧贴左边（`pl` 给 0）。
    pub(super) fn git_row(&self, id: impl Into<ElementId>, pl: f32, fg: Rgb, bg: Rgb) -> Stateful<Div> {
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

    /// 行尾的按钮，平时藏着、不占宽，名字能排满整行；根目录是 `root` 的仓库有操作在跑时按不动。
    fn row_buttons(&self, root: &Path, buttons: Vec<RowButton>, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let enabled = self.workspace().project.git_panel.busy(root).is_none();
        let hover_bg = hsla(bg.mix(fg, 0.14));
        let icon_color = hsla(fg).opacity(if enabled { 0.75 } else { 0.3 });
        div()
            .flex_none()
            .hidden()
            .items_center()
            .gap(px(2.))
            .group_hover(ROW_GROUP, |buttons| buttons.flex())
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
        let root = git.root.clone();
        let (label, count) = match section {
            GitSection::Merge => (rust_i18n::t!("git.section.merge"), GitPanel::files(git, section).len()),
            GitSection::Staged => (rust_i18n::t!("git.section.staged"), GitPanel::files(git, section).len()),
            GitSection::Unstaged => (rust_i18n::t!("git.section.unstaged"), GitPanel::files(git, section).len()),
            GitSection::Stashes => (rust_i18n::t!("git.section.stashes"), git.info.stashes.len()),
        };
        let section_button = |icon, key: &'static str, stage: bool| {
            let root = root.clone();
            button(icon, rust_i18n::t!(key), move |this, window, cx| {
                this.git_section_action(&root, section, stage, window, cx);
            })
        };
        let buttons = match section {
            GitSection::Merge => vec![section_button(PLUS_ICON, "git.stage_all", true)],
            GitSection::Staged => vec![section_button(MINUS_ICON, "git.unstage_all", false)],
            GitSection::Unstaged => {
                vec![
                    section_button(DISCARD_ICON, "git.discard_all", false),
                    section_button(PLUS_ICON, "git.stage_all", true),
                ]
            }
            GitSection::Stashes => Vec::new(),
        };
        self.git_row(("git-section", ix), 0., fg, bg)
            .child(chevron(panel.section_expanded(&root, section), fg))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(label.into_owned()),
            )
            .child(self.row_buttons(&root, buttons, fg, bg, cx))
            .child(count_badge(count, fg, bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let project = &mut this.workspace_mut().project;
                    project.git_panel.toggle_section(&root, section, project.git.as_ref());
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
        let root = git.root.clone();
        let name = base_name(&file.path);
        // 以树形式查看时所在目录已经在上面的行里了，不再写。
        let panel = &self.workspace().project.git_panel;
        let depth = panel.depth.get(ix).copied().unwrap_or(0) as f32;
        let dir = if panel.tree {
            String::new()
        } else {
            file.path.parent().map(|dir| dir.display().to_string()).unwrap_or_default()
        };
        let action = |op| GitFileAction { repo: root.clone(), op, section, path: file.path.clone() };
        let file_button = |icon, key: &'static str, op| {
            let action = action(op);
            button(icon, rust_i18n::t!(key), move |this, window, cx| this.git_file_action(&action, window, cx))
        };
        let mut buttons = vec![file_button(OPEN_FILE_ICON, "git.open_file", FileOp::Open)];
        match shown_in {
            GitSection::Merge => buttons.push(file_button(PLUS_ICON, "git.mark_resolved", FileOp::Stage)),
            GitSection::Staged => buttons.push(file_button(MINUS_ICON, "git.unstage", FileOp::Unstage)),
            _ => {
                // 子模块那一条丢不掉，不给丢弃的按钮。
                if !file.gitlink {
                    buttons.push(file_button(DISCARD_ICON, "git.discard", FileOp::Discard));
                }
                buttons.push(file_button(PLUS_ICON, "git.stage", FileOp::Stage));
            }
        }
        let path = file.path.clone();
        let dim = hsla(fg).opacity(0.5);
        let deleted = file.status == FileStatus::Deleted;
        let side = if section == Section::Staged { DiffSide::Index } else { DiffSide::Worktree };
        let target = DiffTarget { root: root.clone(), rel: path.clone(), old_rel: file.old_path.clone(), side };
        self.git_row(("git-file", ix), INDENT * (depth + 1.), fg, bg)
            // 树形式里目录行有箭头，文件行空出同样宽，名字才对得齐。
            .when(panel.tree, |row| row.child(div().flex_none().w(px(12.))))
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
            .child(self.row_buttons(&root, buttons, fg, bg, cx))
            .child(status_letter(file.status))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.click_diff(target.clone(), event.click_count, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_git_file_menu(event.position, root.clone(), section, shown_in, path.clone(), cx);
                }),
            )
            .into_any_element()
    }

    /// 以树形式查看时工作区改动里的一个目录：可以展开收起，悬停时有暂存、取消暂存或丢掉这个目录
    /// 下所有改动的按钮，右键菜单同样。
    pub(super) fn render_git_dir(
        &self,
        ix: usize,
        di: usize,
        section: GitSection,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = &self.workspace().project.git_panel;
        let Some(dir) = panel.dirs.get(di) else {
            return div().into_any_element();
        };
        let depth = panel.depth.get(ix).copied().unwrap_or(0) as f32;
        let (root, path) = (dir.root.clone(), dir.path.clone());
        let action = |op| GitDirAction { repo: root.clone(), section, dir: path.clone(), op };
        let dir_button = |icon, key: &'static str, op| {
            let action = action(op);
            button(icon, rust_i18n::t!(key), move |this, window, cx| this.git_dir_action(&action, window, cx))
        };
        let buttons = match section {
            GitSection::Merge => vec![dir_button(PLUS_ICON, "git.mark_resolved", DirOp::Stage)],
            GitSection::Staged => vec![dir_button(MINUS_ICON, "git.unstage", DirOp::Unstage)],
            _ => vec![
                dir_button(DISCARD_ICON, "git.discard", DirOp::Discard),
                dir_button(PLUS_ICON, "git.stage", DirOp::Stage),
            ],
        };
        let last = base_name(&path);
        let menu = action(DirOp::Stage);
        self.git_row(("git-dir", ix), INDENT * (depth + 1.), fg, bg)
            .child(chevron(dir.expanded, fg))
            .child(img(folder_icon(&last, dir.expanded)).flex_none().size(px(14.)))
            .child(div().flex_1().min_w_0().truncate().child(dir.name.clone()))
            .child(self.row_buttons(&root, buttons, fg, bg, cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    let project = &mut this.workspace_mut().project;
                    project.git_panel.toggle_dir(di, project.git.as_ref());
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_git_dir_menu(event.position, menu.clone(), cx);
                }),
            )
            .into_any_element()
    }

    /// 目录的右键菜单，`base` 里除了 `op` 都是这个目录的。
    fn open_git_dir_menu(&mut self, position: gpui::Point<gpui::Pixels>, base: GitDirAction, cx: &mut Context<Self>) {
        let enabled = self.workspace().project.git_panel.busy(&base.repo).is_none();
        let item = |key: &str, op| Some(menu_item(key, Box::new(GitDirAction { op, ..base.clone() }), enabled, cx));
        let items = match base.section {
            GitSection::Merge => vec![item("git.mark_resolved", DirOp::Stage)],
            GitSection::Staged => vec![item("git.unstage", DirOp::Unstage)],
            _ => vec![item("git.stage", DirOp::Stage), item("git.discard", DirOp::Discard)],
        };
        let target = self.git_focus.clone();
        self.open_menu(position, items, target, cx);
    }

    /// 根目录是 `root` 的仓库里那个文件的右键菜单。
    fn open_git_file_menu(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        root: PathBuf,
        section: Section,
        shown_in: GitSection,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let enabled = self.workspace().project.git_panel.busy(&root).is_none();
        let gitlink = self.git_file(&root, section, &path).is_some_and(|file| file.gitlink);
        let item = |key: &str, op, enabled| {
            let action = GitFileAction { repo: root.clone(), op, section, path: path.clone() };
            Some(menu_item(key, Box::new(action), enabled, cx))
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
                items.push(item("git.stage", FileOp::Stage, enabled));
                if !gitlink {
                    items.push(item("git.discard", FileOp::Discard, enabled));
                }
            }
        }
        let target = self.git_focus.clone();
        self.open_menu(position, items, target, cx);
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
        let root = git.root.clone();
        let stash_button = |icon, key: &'static str, op: StashOp| {
            let root = root.clone();
            button(icon, rust_i18n::t!(key), move |this, window, cx| {
                this.git_stash_action(&root, index, op, window, cx)
            })
        };
        let buttons = vec![
            stash_button(STASH_APPLY_ICON, "git.stash_apply", StashOp::Apply),
            stash_button(STASH_POP_ICON, "git.stash_pop", StashOp::Pop),
            stash_button(TRASH_ICON, "git.stash_drop", StashOp::Drop),
        ];
        self.git_row(("git-stash", ix), INDENT, fg, bg)
            .child(div().flex_none().text_size(px(11.)).text_color(hsla(fg).opacity(0.5)).child(format!("#{index}")))
            .child(div().flex_1().min_w_0().truncate().child(stash.message.clone()))
            .child(self.row_buttons(&root, buttons, fg, bg, cx))
            .into_any_element()
    }
}

/// 段标题和块头上的个数，圆角的小底子。
pub(super) fn count_badge(count: usize, fg: Rgb, bg: Rgb) -> Div {
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
        .text_color(hsla(fg).opacity(0.5))
        .child(count.to_string())
}

/// 行尾的状态字母，按状态上色。
pub(super) fn status_letter(status: FileStatus) -> Div {
    div().flex_none().w(px(12.)).flex().justify_center().text_color(hsla(status_color(status))).child(status.letter())
}

pub(super) fn chevron(expanded: bool, fg: Rgb) -> gpui::Svg {
    svg()
        .flex_none()
        .path(if expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
        .size(px(12.))
        .text_color(hsla(fg).opacity(0.45))
}
