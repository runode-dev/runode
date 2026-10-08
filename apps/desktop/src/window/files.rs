//! 右侧的文件树：当前终端所在仓库或目录下的文件，图标按文件类型，名字按 git 状态着色，行尾标出状态。
//! 单击目录展开收起，双击文件在预览栏里打开，也可以配置成单击打开、双击把路径打进终端。
//!
//! 点过文件树后它拿到焦点，方向键移动选中的行，各种快捷键作用在选中的那一项上。右键菜单在
//! `menu`；就地新建、改名，删除、剪切复制粘贴和拖动挪位置在 `edit`，它们落到文件系统上的
//! 操作在 `ops`。底部列着能跑的项目命令，在 `tasks`。

mod edit;
mod menu;
mod ops;
mod tasks;

use std::{
    borrow::Cow,
    ops::Range,
    path::{Path, PathBuf},
};

use gpui::{
    Action, AnyElement, Axis, ClickEvent, Context, Div, Focusable, Hsla, MouseButton, MouseDownEvent, Pixels, Render,
    ScrollStrategy, SharedString, Stateful, Window, actions, div, img, prelude::*, px, svg, uniform_list,
};
use runode_config::PreviewClick;
use runode_shared_types::color::Rgb;

pub(super) use edit::{FileClipboard, FileEdit};
pub(super) use menu::{FileMenu, MenuButton, MenuItem, check_item, menu_item, text_item};
pub(super) use tasks::is_task_file;

use super::{
    WindowView,
    project::{Decoration, Project, added_label, panel_shell, panel_title, removed_label, status_color},
    titlebar::{drag_chip, icon_toggle},
};
use crate::{
    assets::{
        CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, COLLAPSE_ALL_ICON, EYE_ICON, EYE_OFF_ICON, NEW_FILE_ICON,
        NEW_FOLDER_ICON,
    },
    config::AppConfig,
    ui::{
        actions::{Copy, Cut, Paste},
        file_icons::{file_icon, folder_icon},
        hsla,
        scrollbar::scrollbar,
        tooltip::tooltip,
    },
};

actions!(
    runode,
    [
        /// 文件树里选中上一项、下一项、第一项和最后一项。
        SelectPreviousFile,
        SelectNextFile,
        SelectFirstFile,
        SelectLastFile,
        /// 收起选中的目录，已经收着或者选中的是文件时跳到上一级目录。
        CollapseSelectedFile,
        /// 展开选中的目录，已经展开时跳到它的第一项。
        ExpandSelectedFile,
        /// 选中的目录展开或收起，文件在预览栏里打开。
        OpenSelectedFile,
        /// 把选中项的路径打进终端。
        InsertFilePath,
        /// 在选中的目录或文件所在的目录里新建文件、文件夹。
        NewFile,
        NewFolder,
        RenameFile,
        /// 把选中的文件或目录移到废纸篓。
        DeleteFile,
        /// 在访达里显示选中的文件或目录。
        RevealInFinder,
        /// 开一个新标签页，终端的目录是选中的目录或文件所在的目录。
        OpenInTerminal,
        /// 把选中项的绝对路径、相对文件树根目录的路径复制到剪贴板。
        CopyPath,
        CopyRelativePath,
        /// 收起文件树里所有展开的目录。
        CollapseAllFiles,
        /// 焦点从文件树交回终端。
        FocusTerminal
    ]
);

/// 行高比字号多出的部分。
const ROW_EXTRA_HEIGHT: f32 = 10.;
/// 每深一层往右缩进的宽度。
const INDENT: f32 = 12.;
/// 行的左边距。
const ROW_PADDING: f32 = 4.;
/// 标题下面那行工具栏的高度。
const TOOLBAR_HEIGHT: f32 = 32.;
/// 工具栏按钮的边长。
const TOOLBAR_BUTTON_SIZE: f32 = 24.;
/// 剪切下来等着粘贴的行画得淡一些。
const CUT_OPACITY: f32 = 0.5;

/// 打进 shell 不用引号也不用转义的常见字符。
fn is_shell_plain(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '@' | '%' | ':' | ',' | '=')
}

/// 打进 shell 的路径：只含常见字符时原样，否则用单引号括起来。开头是 `=` 或 `%` 时也括起来，
/// zsh 会把 `=foo` 展开成命令的路径。
pub(super) fn shell_quote(text: &str) -> String {
    let plain = !text.is_empty() && !text.starts_with(['=', '%']) && text.chars().all(is_shell_plain);
    if plain { text.to_owned() } else { format!("'{}'", text.replace('\'', r"'\''")) }
}

/// Claude Code、Codex 会把粘贴进来的图片路径换成图片附件，这几种扩展名两边都认。
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| IMAGE_EXTENSIONS.iter().any(|image| ext.eq_ignore_ascii_case(image)))
}

/// 和 `shell_quote` 一样只放过常见字符，其余字符前面加反斜杠，像访达把文件拖进终端时那样。
/// 图片路径用这种写法：Claude Code 按「空格后跟 `/`」拆开几个路径，引号只在整段两头时才去掉，
/// 几个带引号的路径连在一起它就认不出来了；反斜杠转义它和 Codex 都能还原。
fn shell_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for (i, c) in text.chars().enumerate() {
        let plain = is_shell_plain(c) && !(i == 0 && matches!(c, '=' | '%'));
        if !plain {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// 从文件树拖出来的一项：放到终端上把路径打进去，放到文件树的目录上挪进那个目录。
#[derive(Clone)]
pub(super) struct DraggedFile {
    pub path: PathBuf,
    name: SharedString,
    width: Pixels,
    height: Pixels,
    fg: Hsla,
    bg: Hsla,
}

impl Render for DraggedFile {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_chip(self.width, self.height, self.name.clone(), self.fg, self.bg)
    }
}

impl WindowView {
    /// 文件树的根目录：终端所在的仓库或目录，还没读过时是 workspace 的目录。
    pub(super) fn files_root(&self) -> PathBuf {
        let workspace = self.workspace();
        workspace.project.root.clone().unwrap_or_else(|| workspace.dir.clone())
    }

    /// 选中的那一项和它是不是目录；没选中时为空。
    fn selected_entry(&self) -> Option<(PathBuf, bool)> {
        let project = &self.workspace().project;
        let path = project.selected.clone()?;
        let is_dir = match project.selected_row() {
            Some(ix) => project.file_rows[ix].is_dir,
            None => path.is_dir(),
        };
        Some((path, is_dir))
    }

    /// 新建、粘贴和开终端落在哪个目录：选中的目录，选中文件所在的目录，没选中时是根目录。
    fn target_dir(&self) -> PathBuf {
        match self.selected_entry() {
            Some((path, true)) => path,
            Some((path, false)) => path.parent().map_or_else(|| self.files_root(), Path::to_path_buf),
            None => self.files_root(),
        }
    }

    pub(super) fn render_files_panel(&self, width: f32, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let workspace = self.workspace();
        // 显示的是终端所在的仓库或目录，不一定是 workspace 的目录。
        let name = match workspace.project.root.as_deref().and_then(Path::file_name) {
            Some(name) if workspace.project.root.as_ref() != Some(&workspace.dir) => {
                SharedString::from(name.to_string_lossy().into_owned())
            }
            _ => workspace.name.clone(),
        };
        let show_ignored = self.show_ignored;
        // 工具栏上的图标按钮。按下时不往外传：外层的文件树按下时会把焦点抢回去，新建时刚交给
        // 输入框的焦点就丢了。
        type Handler = fn(&mut WindowView, &mut Window, &mut Context<WindowView>);
        let button = |id,
                      icon,
                      on,
                      text: Cow<'static, str>,
                      action: Option<&dyn Action>,
                      handler: Handler,
                      cx: &mut Context<Self>| {
            icon_toggle(id, icon, 14., on, fg, bg)
                .flex_none()
                .size(px(TOOLBAR_BUTTON_SIZE))
                .tooltip(tooltip(text, action, fg, bg))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        handler(this, window, cx);
                    }),
                )
        };
        let new_file = button(
            "new-file",
            NEW_FILE_ICON,
            false,
            rust_i18n::t!("files.new_file"),
            Some(&NewFile),
            |this, window, cx| this.new_file(&NewFile, window, cx),
            cx,
        );
        let new_folder = button(
            "new-folder",
            NEW_FOLDER_ICON,
            false,
            rust_i18n::t!("files.new_folder"),
            Some(&NewFolder),
            |this, window, cx| this.new_folder(&NewFolder, window, cx),
            cx,
        );
        let collapse_all = button(
            "collapse-all",
            COLLAPSE_ALL_ICON,
            false,
            rust_i18n::t!("files.collapse_all"),
            Some(&CollapseAllFiles),
            |this, window, cx| this.collapse_all_files(&CollapseAllFiles, window, cx),
            cx,
        );
        let (icon, text) = if show_ignored {
            (EYE_ICON, rust_i18n::t!("tooltip.hide_ignored"))
        } else {
            (EYE_OFF_ICON, rust_i18n::t!("tooltip.show_ignored"))
        };
        let ignored_toggle =
            button("toggle-ignored", icon, show_ignored, text, None, |this, _, cx| this.toggle_show_ignored(cx), cx);
        let header = panel_title().child(div().flex_1().min_w_0().truncate().text_color(hsla(fg)).child(name));
        // 标题下面一行：左边是没提交的改动一共加减了多少行，右边是新建、全部收起和显示忽略
        // 文件的按钮。
        let dirty = workspace.project.git.as_ref().filter(|git| !git.is_clean());
        let toolbar = div()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .when_some(dirty, |toolbar, git| {
                toolbar.child(added_label(git.added())).child(removed_label(git.removed()))
            })
            .child(div().flex_1())
            .child(
                div().flex().gap(px(4.)).child(new_file).child(new_folder).child(collapse_all).child(ignored_toggle),
            );
        let font_size = cx.global::<AppConfig>().0.file_tree_font_size;
        let new_entry = self.new_entry_row();
        let count = workspace.project.file_rows.len() + usize::from(new_entry.is_some());
        let list = uniform_list(
            "files",
            count,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                let focused = this.files_focus.contains_focused(window, cx);
                this.render_file_rows(range, new_entry, font_size, focused, fg, bg, cx)
            }),
        )
        .track_scroll(&workspace.project.files_scroll)
        .size_full()
        .p(px(4.))
        // 点在行下面的空白处：取消选中，右键弹出对根目录的菜单。
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.workspace_mut().project.selected = None;
                cx.notify();
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                window.focus(&this.files_focus, cx);
                this.workspace_mut().project.selected = None;
                this.open_file_menu(event.position, cx);
            }),
        );
        let scroll = workspace.project.files_scroll.0.borrow().base_handle.clone();
        let list = div().flex_1().min_h_0().relative().child(list).child(scrollbar(
            "files-scroll",
            scroll,
            Axis::Vertical,
            hsla(fg),
        ));
        panel_shell("files-panel", width, fg, bg, cx)
            // 新建或改名时输入框在文件树里面，方向键这些归输入框。
            .key_context(if self.file_edit.is_some() { "FileTree editing" } else { "FileTree" })
            .track_focus(&self.files_focus)
            .on_action(cx.listener(Self::select_previous_file))
            .on_action(cx.listener(Self::select_next_file))
            .on_action(cx.listener(Self::select_first_file))
            .on_action(cx.listener(Self::select_last_file))
            .on_action(cx.listener(Self::collapse_selected_file))
            .on_action(cx.listener(Self::expand_selected_file))
            .on_action(cx.listener(Self::open_selected_file))
            .on_action(cx.listener(Self::collapse_all_files))
            .on_action(cx.listener(Self::focus_terminal))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::new_folder))
            .on_action(cx.listener(Self::rename_file))
            .on_action(cx.listener(Self::delete_file))
            .on_action(cx.listener(Self::reveal_in_finder))
            .on_action(cx.listener(Self::insert_file_path))
            .on_action(cx.listener(Self::open_in_terminal))
            .on_action(cx.listener(Self::copy_path))
            .on_action(cx.listener(Self::copy_relative_path))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy_file(false, cx)))
            .on_action(cx.listener(|this, _: &Cut, _, cx| this.copy_file(true, cx)))
            .on_action(cx.listener(|this, _: &Paste, window, cx| this.paste_file(window, cx)))
            // 拖到空白处：挪到根目录。
            .on_drop(cx.listener(|this, dragged: &DraggedFile, window, cx| {
                let root = this.files_root();
                this.drop_file(&dragged.path, root, window, cx);
            }))
            .bg(hsla(bg.mix(fg, 0.03)))
            .text_size(px(font_size))
            .child(self.render_panel_tabs(fg, bg, cx))
            .child(header)
            .child(toolbar)
            .child(list)
            .children(self.render_tasks(font_size, fg, bg, cx))
    }

    /// 文件树的行。行高、箭头和图标跟着字号 `font_size` 一起缩放。新建时输入框插在
    /// `new_entry`（即 `new_entry_row`）那一行，后面的行往下错一行。
    #[allow(clippy::too_many_arguments)]
    fn render_file_rows(
        &self,
        range: Range<usize>,
        new_entry: Option<(usize, usize)>,
        font_size: f32,
        focused: bool,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        range
            .filter_map(|ix| match new_entry {
                Some((at, depth)) if ix == at => Some(self.render_new_entry_row(depth, font_size, fg, bg, cx)),
                Some((at, _)) if ix > at => self.render_file_row(ix - 1, font_size, focused, fg, bg, cx),
                _ => self.render_file_row(ix, font_size, focused, fg, bg, cx),
            })
            .collect()
    }

    /// 缩进：每一层一格，格子中间一条竖线，对准那一层的箭头，上下相邻的行连成一条。
    fn indent_guides(depth: usize, fg: Rgb) -> impl Iterator<Item = Div> {
        let guide = hsla(fg).opacity(0.2);
        (0..depth).map(move |_| {
            div().flex_none().w(px(INDENT)).h_full().flex().justify_center().child(div().w(px(1.)).h_full().bg(guide))
        })
    }

    /// 文件树的一行：外框、缩进和底色，里面放箭头、图标和名字。
    fn file_row_shell(id: impl Into<gpui::ElementId>, depth: usize, font_size: f32, fg: Rgb) -> Stateful<Div> {
        div()
            .id(id)
            .flex_none()
            .h(px(font_size + ROW_EXTRA_HEIGHT))
            // 占满整行宽度，名字后面的空白处也能点、也有悬停底色。
            .w_full()
            .pl(px(ROW_PADDING))
            .pr(px(6.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .overflow_hidden()
            .children(Self::indent_guides(depth, fg))
    }

    /// 第 `ix` 个 `FileRow`；`focused` 是文件树有没有焦点，有焦点时选中的行更醒目。
    fn render_file_row(
        &self,
        ix: usize,
        font_size: f32,
        focused: bool,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let project = &self.workspace().project;
        let row = project.file_rows.get(ix)?;
        let selected_bg = hsla(bg.mix(fg, if focused { 0.16 } else { 0.10 }));
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let drop_bg = hsla(bg.mix(fg, 0.20));
        let drag_bg = hsla(bg.mix(fg, 0.10));
        let color = match row.decoration {
            Decoration::None | Decoration::Ignored => hsla(fg).opacity(0.85),
            Decoration::Status(status) | Decoration::ContainsChanges(status) => hsla(status_color(status)),
        };
        let selected = project.selected.as_ref() == Some(&row.path);
        let cut = self.file_clipboard.as_ref().is_some_and(|clip| clip.cut && clip.path == row.path);
        let chevron = row.is_dir.then(|| {
            svg()
                .path(if row.expanded { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
                .size(px(font_size))
                .text_color(hsla(fg).opacity(0.5))
        });
        // 并成一行的目录按最里层的名字挑图标。
        let last = row.path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
        let icon = if row.is_dir { folder_icon(&last, row.expanded) } else { file_icon(&last) };
        // 行尾的 git 标记：文件写状态字母，含改动的目录画一个点。
        let badge = match row.decoration {
            Decoration::Status(status) => Some(status.letter()),
            Decoration::ContainsChanges(_) => Some("•"),
            Decoration::None | Decoration::Ignored => None,
        };
        let badge = badge.map(|text| div().flex_none().pl(px(6.)).text_color(color).child(text));
        let renaming = self.renaming(&row.path);
        let name: AnyElement = match renaming {
            Some(edit) => edit.render(px(font_size + 6.), fg, bg).flex_1().min_w_0().into_any_element(),
            None => div().min_w_0().truncate().text_color(color).child(row.name.clone()).into_any_element(),
        };
        // 改名时在输入框里拖选文字，不能把整行拖走。
        let dragged = renaming.is_none().then(|| DraggedFile {
            path: row.path.clone(),
            name: row.name.clone(),
            width: px(180.),
            height: px(font_size + ROW_EXTRA_HEIGHT),
            fg: hsla(fg),
            bg: drag_bg,
        });
        let path = row.path.clone();
        let is_dir = row.is_dir;
        // 放到目录上挪进这个目录，放到文件上挪进文件所在的目录。
        let drop_dir =
            if is_dir { path.clone() } else { path.parent().map_or_else(|| self.files_root(), Path::to_path_buf) };
        Some(
            Self::file_row_shell(("file", ix), row.depth, font_size, fg)
                .when(row.decoration == Decoration::Ignored, |item| item.opacity(0.45))
                .when(cut, |item| item.opacity(CUT_OPACITY))
                .map(|item| if selected { item.bg(selected_bg) } else { item.hover(|item| item.bg(hover_bg)) })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(div().flex_none().w(px(font_size)).flex().items_center().children(chevron))
                        .child(img(icon).flex_none().size(px(font_size + 2.)))
                        .child(name),
                )
                .children(badge)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener({
                        let path = path.clone();
                        move |this, _: &MouseDownEvent, window, cx| {
                            // 点在改名输入框里时交给输入框挪光标、选字。
                            cx.stop_propagation();
                            if this.renaming(&path).is_some() {
                                return;
                            }
                            window.focus(&this.files_focus, cx);
                            this.workspace_mut().project.selected = Some(path.clone());
                            cx.notify();
                        }
                    }),
                )
                // 按下只选中，松开才展开目录、打开预览：拖动时 GPUI 不发点击，拖走的文件不会被打开。
                .on_click(cx.listener({
                    let path = path.clone();
                    move |this, event: &ClickEvent, _, cx| {
                        if this.renaming(&path).is_none() {
                            this.click_file(&path, is_dir, event.click_count(), cx);
                        }
                    }
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.files_focus, cx);
                        this.workspace_mut().project.selected = Some(path.clone());
                        this.open_file_menu(event.position, cx);
                    }),
                )
                .when_some(dragged, |row, dragged| {
                    row.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                })
                .drag_over::<DraggedFile>(move |style, _, _, _| style.bg(drop_bg))
                .on_drop(cx.listener(move |this, dragged: &DraggedFile, window, cx| {
                    this.drop_file(&dragged.path, drop_dir.clone(), window, cx);
                }))
                .into_any_element(),
        )
    }

    /// 点击（按下又松开、没拖动）选中，目录同时展开或收起。文件按配置的 `PreviewClick`
    /// 在预览栏里打开：单击打开时单击开成临时标签，双击固定下来；双击打开时双击开成固定标签。
    fn click_file(&mut self, path: &Path, is_dir: bool, clicks: usize, cx: &mut Context<Self>) {
        self.workspace_mut().project.selected = Some(path.to_path_buf());
        if is_dir {
            self.with_tree(|project, root, show_ignored| project.toggle_dir(path, root, show_ignored));
            cx.notify();
            return;
        }
        let single = cx.global::<AppConfig>().0.file_tree_preview_click == PreviewClick::Single;
        if clicks >= 2 {
            self.open_preview(path, true, cx);
        } else if single {
            self.open_preview(path, false, cx);
        }
        cx.notify();
    }

    /// 拿文件树的根目录和是否显示被忽略的文件，对 `Project` 做 `f`。
    pub(super) fn with_tree<R>(&mut self, f: impl FnOnce(&mut Project, &Path, bool) -> R) -> R {
        let (root, show_ignored) = (self.files_root(), self.show_ignored);
        f(&mut self.workspace_mut().project, &root, show_ignored)
    }

    /// 键盘移动选中的行：`f` 返回新选中的位置，滚到那里，关掉右键菜单。
    fn move_selection(&mut self, f: impl FnOnce(&mut Project, &Path, bool) -> Option<usize>, cx: &mut Context<Self>) {
        self.with_tree(|project, root, show_ignored| {
            if let Some(ix) = f(project, root, show_ignored) {
                project.files_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            }
        });
        self.file_menu = None;
        cx.notify();
    }

    fn select_previous_file(&mut self, _: &SelectPreviousFile, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(|project, _, _| project.select_step(-1), cx);
    }

    fn select_next_file(&mut self, _: &SelectNextFile, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(|project, _, _| project.select_step(1), cx);
    }

    fn select_first_file(&mut self, _: &SelectFirstFile, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(|project, _, _| project.select_row(0), cx);
    }

    fn select_last_file(&mut self, _: &SelectLastFile, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(|project, _, _| project.select_row(project.file_rows.len().checked_sub(1)?), cx);
    }

    fn collapse_selected_file(&mut self, _: &CollapseSelectedFile, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(Project::select_out, cx);
    }

    fn expand_selected_file(&mut self, _: &ExpandSelectedFile, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(Project::select_in, cx);
    }

    fn open_selected_file(&mut self, _: &OpenSelectedFile, _: &mut Window, cx: &mut Context<Self>) {
        let Some((path, is_dir)) = self.selected_entry() else {
            return;
        };
        if is_dir {
            self.move_selection(
                |project, root, show_ignored| {
                    project.toggle_dir(&path, root, show_ignored);
                    None
                },
                cx,
            );
        } else {
            self.open_preview(&path, false, cx);
            // 预览栏刚打开时文件树变窄，选中的行可能被挤出视野。
            self.move_selection(|project, _, _| project.selected_row(), cx);
        }
    }

    fn collapse_all_files(&mut self, _: &CollapseAllFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(
            |project, root, show_ignored| {
                project.collapse_all(root, show_ignored);
                None
            },
            cx,
        );
    }

    fn focus_terminal(&mut self, _: &FocusTerminal, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_menu.take().is_some() {
            cx.notify();
            return;
        }
        window.focus(&self.focus_handle(cx), cx);
    }

    /// 把从文件树或访达拖来的 `paths` 放到终端 `pane` 上：切到那个终端，把路径一个个打进去。
    /// 一个路径粘贴一次，因为 Codex 只在一次粘贴里正好是一个路径时才把它当图片。
    pub(super) fn drop_paths_on_pane(
        &mut self,
        pane: gpui::EntityId,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_pane_in_active_tab(pane, window, cx);
        for path in paths {
            self.insert_path(path, None, window, cx);
        }
    }

    /// 把 `path` 打进当前终端，在它的目录下时写相对路径，后面带上行号 `line` 和一个空格，
    /// 再把焦点交回终端。图片写绝对路径：Claude Code 只把绝对路径认成图片附件。
    pub(super) fn insert_path(&mut self, path: &Path, line: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.focused_view().cloned() else {
            return;
        };
        let cwd = view.read(cx).cwd();
        let text = if line.is_none() && is_image(path) {
            let absolute = match &cwd {
                Some(cwd) if path.is_relative() => cwd.join(path),
                _ => path.to_path_buf(),
            };
            shell_escape(&absolute.display().to_string())
        } else {
            // 终端报的目录和面板里的路径可能一个经过符号链接、一个没有，对不上时都换成真实路径再比。
            let relative = |path: &Path, cwd: &Path| {
                path.strip_prefix(cwd).ok().map(Path::to_path_buf).or_else(|| {
                    let (path, cwd) = (path.canonicalize().ok()?, cwd.canonicalize().ok()?);
                    path.strip_prefix(cwd).ok().map(Path::to_path_buf)
                })
            };
            let rel = cwd.as_deref().and_then(|cwd| relative(path, cwd)).filter(|rel| !rel.as_os_str().is_empty());
            let mut text = rel.as_deref().unwrap_or(path).display().to_string();
            if let Some(line) = line {
                text = format!("{text}:{line}");
            }
            shell_quote(&text)
        };
        view.update(cx, |view, cx| view.paste_text(format!("{text} "), window, cx));
        window.focus(&view.focus_handle(cx), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_paths_for_the_shell() {
        assert_eq!(shell_quote("src/main.rs:12"), "src/main.rs:12");
        assert_eq!(shell_quote("my file.txt"), "'my file.txt'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("文件.md"), "文件.md");
        assert_eq!(shell_quote("=foo"), "'=foo'");
        assert_eq!(shell_quote("a=b"), "a=b");
    }

    #[test]
    fn escapes_image_paths_with_backslashes() {
        assert_eq!(shell_escape("/tmp/shot.png"), "/tmp/shot.png");
        assert_eq!(shell_escape("/tmp/Screen Shot (1).png"), r"/tmp/Screen\ Shot\ \(1\).png");
        assert_eq!(shell_escape("/tmp/it's.png"), r"/tmp/it\'s.png");
        assert_eq!(shell_escape("/tmp/截图.png"), "/tmp/截图.png");
        assert_eq!(shell_escape("=a=b"), r"\=a=b");
    }

    #[test]
    fn recognizes_images_by_extension() {
        assert!(is_image(Path::new("/tmp/a.PNG")));
        assert!(is_image(Path::new("b.jpeg")));
        assert!(!is_image(Path::new("c.svg")));
        assert!(!is_image(Path::new("png")));
    }
}
