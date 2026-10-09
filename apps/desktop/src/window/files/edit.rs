//! 文件树里改文件：就地新建和改名的输入框，移到废纸篓，剪切复制粘贴，以及拖到目录上挪位置。
//! 改完当场重读相关的目录并选中改出来的那一项，不等监听到改动。

use std::path::{Path, PathBuf};

use gpui::{AnyElement, Context, PromptLevel, ScrollStrategy, Window, div, img, prelude::*, px};
use runode_shared_types::color::Rgb;

use super::{
    AddToGitignore, DeleteFile, NewFile, NewFolder, RenameFile,
    ops::{self, OpError},
};
use crate::{
    ui::file_icons::{file_icon, folder_icon},
    window::{WindowView, inline_edit::InlineEdit, model::base_name, project::follow_move},
};

/// 剪切或复制下来等着粘贴的文件或目录。
#[derive(Clone)]
pub(in crate::window) struct FileClipboard {
    pub path: PathBuf,
    /// 剪切的粘贴时挪过去，粘贴一次就清掉；复制的可以一直粘贴。
    pub cut: bool,
}

enum EditTarget {
    /// 在 `dir` 里新建，输入框插在它的第一项。
    New { dir: PathBuf, is_dir: bool },
    /// 改 `path` 的名字，输入框换掉那一行的名字。
    Rename { path: PathBuf, is_dir: bool },
}

/// 文件树里正在新建或改名的输入框。
pub(in crate::window) struct FileEdit {
    target: EditTarget,
    edit: InlineEdit,
}

/// 改 `name` 失败时提示里的说明。
fn error_text(err: &OpError, name: &str) -> String {
    match err {
        OpError::Exists => rust_i18n::t!("files.exists", name = name).into_owned(),
        OpError::IntoItself => rust_i18n::t!("files.into_itself", name = name).into_owned(),
        OpError::Io(err) if err.kind() == std::io::ErrorKind::Unsupported => {
            rust_i18n::t!("files.unsupported").into_owned()
        }
        OpError::Io(err) => err.to_string(),
    }
}

impl WindowView {
    pub(super) fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.target_dir();
        self.start_file_edit(EditTarget::New { dir, is_dir: false }, window, cx);
    }

    pub(super) fn new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.target_dir();
        self.start_file_edit(EditTarget::New { dir, is_dir: true }, window, cx);
    }

    pub(super) fn rename_file(&mut self, _: &RenameFile, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, is_dir)) = self.selected_entry() {
            self.start_file_edit(EditTarget::Rename { path, is_dir }, window, cx);
        }
    }

    /// 正在改 `path` 的名字时是那个输入框。
    pub(super) fn renaming(&self, path: &Path) -> Option<&InlineEdit> {
        let edit = self.file_edit.as_ref()?;
        matches!(&edit.target, EditTarget::Rename { path: renaming, .. } if renaming == path).then_some(&edit.edit)
    }

    /// 新建时输入框占的那一行：插在第几行、缩进几层。目录的那一行找不到时放在最上面，
    /// 总要看得见。
    pub(super) fn new_entry_row(&self) -> Option<(usize, usize)> {
        let EditTarget::New { dir, .. } = &self.file_edit.as_ref()?.target else {
            return None;
        };
        let rows = &self.workspace().project.file_rows;
        let row = rows.iter().position(|row| row.path == *dir).filter(|_| *dir != self.files_root());
        Some(row.map_or((0, 0), |ix| (ix + 1, rows[ix].depth + 1)))
    }

    /// 新建时插进文件树的那一行，图标跟着输入的名字变。
    pub(super) fn render_new_entry_row(
        &self,
        depth: usize,
        font_size: f32,
        fg: Rgb,
        bg: Rgb,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(FileEdit { target: EditTarget::New { is_dir, .. }, edit }) = &self.file_edit else {
            return div().into_any_element();
        };
        let name = edit.field.read(cx).query();
        let icon = if *is_dir { folder_icon(name, false) } else { file_icon(name) };
        Self::file_row_shell("new-file-entry", depth, font_size, fg)
            // 点在输入框里不要传到文件树，免得清掉选中。
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(div().flex_none().w(px(font_size)))
                    .child(img(icon).flex_none().size(px(font_size + 2.)))
                    .child(edit.render(px(font_size + 6.), fg, bg).flex_1().min_w_0()),
            )
            .into_any_element()
    }

    /// 打开新建或改名的输入框。改名时文件只选中主名，直接打字时扩展名留着。
    fn start_file_edit(&mut self, target: EditTarget, window: &mut Window, cx: &mut Context<Self>) {
        // 正在改别的时先把那个改完。
        self.finish_file_edit(true, window, cx);
        self.file_menu = None;
        let (text, select) = match &target {
            EditTarget::New { dir, .. } => {
                self.with_tree(|project, root, filter| {
                    if dir != root && !project.expanded_dirs.contains(dir) {
                        project.toggle_dir(dir, root, filter);
                    }
                });
                (String::new(), 0)
            }
            EditTarget::Rename { path, is_dir } => {
                let name = base_name(path);
                let stem = if *is_dir { None } else { name.rfind('.').filter(|&dot| dot > 0) };
                let select = stem.unwrap_or(name.len());
                (name, select)
            }
        };
        let edit = InlineEdit::new(text, select, Self::finish_file_edit, window, cx);
        self.file_edit = Some(FileEdit { target, edit });
        let row = self.new_entry_row().map(|(ix, _)| ix).or_else(|| self.workspace().project.selected_row());
        if let Some(row) = row {
            self.workspace_mut().project.files_scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// 关掉输入框；`commit` 时按输入的名字新建或改名，空的不算。
    fn finish_file_edit(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(FileEdit { target, edit }) = self.file_edit.take() else {
            return;
        };
        let name = edit.text(cx);
        edit.release_focus(&self.files_focus, window, cx);
        cx.notify();
        if !commit || name.is_empty() {
            return;
        }
        match target {
            EditTarget::New { dir, is_dir } => self.create_entry(&dir, &name, is_dir, window, cx),
            EditTarget::Rename { path, .. } => self.rename_entry(&path, &name, window, cx),
        }
    }

    fn create_entry(&mut self, dir: &Path, name: &str, is_dir: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = ops::child_path(dir, name) else {
            self.show_invalid_name(name, window, cx);
            return;
        };
        if let Err(err) = ops::create(&path, is_dir) {
            self.show_file_error(error_text(&err, name), window, cx);
            return;
        }
        self.relist_and_reveal(vec![dir.to_path_buf()], &path, cx);
        // 新建的文件直接开成固定标签，接着就能看。
        if !is_dir {
            self.open_preview(&path, true, cx);
        }
    }

    /// 改名只改名字，不挪到别的目录。
    fn rename_entry(&mut self, path: &Path, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(to) = path.parent().and_then(|parent| ops::child_name(parent, name)) else {
            self.show_invalid_name(name, window, cx);
            return;
        };
        if to == path {
            return;
        }
        match ops::rename(path, &to) {
            Ok(()) => self.after_move(path, &to, cx),
            Err(err) => self.show_file_error(error_text(&err, name), window, cx),
        }
    }

    /// 当场重读 `dirs`，再选中 `path`、滚到它。
    fn relist_and_reveal(&mut self, dirs: Vec<PathBuf>, path: &Path, cx: &mut Context<Self>) {
        self.with_tree(|project, root, filter| {
            project.relist(dirs, root, filter);
            project.reveal_file(path, root, filter);
        });
        cx.notify();
    }

    /// `from` 改了名或挪到了 `to`：展开的目录、选中的路径和预览的文件跟过去，重读两边的目录。
    fn after_move(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        self.workspace_mut().project.moved(from, to);
        if let Some(clip) = &mut self.file_clipboard
            && let Some(path) = follow_move(&clip.path, from, to)
        {
            clip.path = path;
        }
        let dirs = [from.parent(), to.parent()].into_iter().flatten().map(Path::to_path_buf).collect();
        self.relist_and_reveal(dirs, to, cx);
        self.move_previews(from, to, cx);
    }

    /// 选中的那一项先问一下，再在后台移到废纸篓：外置卷上可能要复制一遍。
    pub(super) fn delete_file(&mut self, _: &DeleteFile, window: &mut Window, cx: &mut Context<Self>) {
        self.file_menu = None;
        let Some((path, is_dir)) = self.selected_entry() else {
            return;
        };
        let name = base_name(&path);
        let title = rust_i18n::t!("files.delete_title", name = name);
        let detail =
            if is_dir { rust_i18n::t!("files.delete_dir_detail") } else { rust_i18n::t!("files.delete_detail") };
        let answer = window.prompt(
            PromptLevel::Warning,
            &title,
            Some(&detail),
            &[&*rust_i18n::t!("files.delete_confirm"), &*rust_i18n::t!("files.cancel")],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let result = cx.background_spawn({
                let path = path.clone();
                async move { ops::trash(&path) }
            });
            let result = result.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => this.after_delete(&path, window, cx),
                Err(err) => this.show_file_error(error_text(&err, &name), window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// `path` 移到了废纸篓：关掉它和它下面的文件的预览标签，选中原来那个位置上的行，接着按删除键
    /// 可以一路删下去。
    fn after_delete(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        self.retain_previews(|_, preview| !preview.path.starts_with(path), window, cx);
        if self.file_clipboard.as_ref().is_some_and(|clip| clip.path.starts_with(path)) {
            self.file_clipboard = None;
        }
        self.with_tree(|project, root, filter| {
            let ix = project.selected_row();
            project.removed(path);
            project.selected = None;
            project.relist(path.parent().map(Path::to_path_buf).into_iter().collect(), root, filter);
            let last = project.file_rows.len().checked_sub(1);
            if let Some(ix) = ix.zip(last).and_then(|(ix, last)| project.select_row(ix.min(last))) {
                project.files_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            }
        });
        window.focus(&self.files_focus, cx);
        cx.notify();
    }

    /// 选中项所在的仓库（子仓库里的算子仓库）的根目录、相对它的路径和是不是目录；不在仓库里、
    /// 已经被忽略、或者选中的就是仓库根目录时为空。
    pub(super) fn gitignore_target(&self) -> Option<(PathBuf, PathBuf, bool)> {
        let (path, is_dir) = self.selected_entry()?;
        let git = self.workspace().project.git.as_ref()?;
        let rel = path.strip_prefix(&git.main.root).ok()?;
        if git.is_ignored(rel) {
            return None;
        }
        let (repo, rel) = git.locate(rel);
        (!rel.as_os_str().is_empty()).then(|| (repo.root.clone(), rel.to_path_buf(), is_dir))
    }

    /// 写进 `.gitignore` 后当场重读，选中的那一项马上标成被忽略。
    pub(super) fn add_to_gitignore(&mut self, _: &AddToGitignore, window: &mut Window, cx: &mut Context<Self>) {
        self.file_menu = None;
        let Some((root, rel, is_dir)) = self.gitignore_target() else {
            return;
        };
        match ops::add_to_gitignore(&root, &rel, is_dir) {
            Ok(()) => self.refresh_project(cx),
            Err(err) => self.show_file_error(error_text(&err, ".gitignore"), window, cx),
        }
    }

    /// 剪切（`cut`）或复制选中的那一项，等着粘贴。
    pub(super) fn copy_file(&mut self, cut: bool, cx: &mut Context<Self>) {
        self.file_menu = None;
        if let Some((path, _)) = self.selected_entry() {
            self.file_clipboard = Some(FileClipboard { path, cut });
            cx.notify();
        }
    }

    /// 粘贴到 `target_dir`。
    pub(super) fn paste_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.file_menu = None;
        if let Some(clip) = self.file_clipboard.clone() {
            let mut dir = self.target_dir();
            // 选中的就是复制下来的目录时，副本放在它旁边，不然成了放进它自己里面。
            if dir == clip.path
                && let Some(parent) = clip.path.parent()
            {
                dir = parent.to_path_buf();
            }
            self.transfer(clip.path, dir, clip.cut, window, cx);
        }
    }

    /// 从文件树拖出来的 `src` 放到了目录 `dir` 上：问一下再挪过去。放回原来的目录或者放进
    /// 它自己下面时什么都不做。
    pub(super) fn drop_file(&mut self, src: &Path, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if src.parent() == Some(dir.as_path()) || dir.starts_with(src) {
            return;
        }
        let title = rust_i18n::t!("files.move_title", name = base_name(src), dir = base_name(&dir));
        let answer = window.prompt(
            PromptLevel::Info,
            &title,
            None,
            &[&*rust_i18n::t!("files.move_confirm"), &*rust_i18n::t!("files.cancel")],
            cx,
        );
        let src = src.to_path_buf();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            this.update_in(cx, |this, window, cx| this.transfer(src, dir, true, window, cx)).ok();
        })
        .detach();
    }

    /// 在后台把 `src` 挪进（`cut`）或复制进 `dir`，目录可能很大；完了选中挪过去或复制出来的那一项。
    fn transfer(&mut self, src: PathBuf, dir: PathBuf, cut: bool, window: &mut Window, cx: &mut Context<Self>) {
        let job = cx.background_spawn({
            let (src, dir) = (src.clone(), dir.clone());
            async move { if cut { ops::move_into(&src, &dir) } else { ops::copy_into(&src, &dir) } }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(dest) if cut => {
                    if this.file_clipboard.as_ref().is_some_and(|clip| clip.cut && clip.path == src) {
                        this.file_clipboard = None;
                    }
                    if dest != src {
                        this.after_move(&src, &dest, cx);
                    }
                }
                Ok(dest) => this.relist_and_reveal(vec![dir], &dest, cx),
                Err(err) => this.show_file_error(error_text(&err, &base_name(&src)), window, cx),
            })
            .ok();
        })
        .detach();
    }

    fn show_invalid_name(&self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.show_file_error(rust_i18n::t!("files.invalid_name", name = name).into_owned(), window, cx);
    }

    fn show_file_error(&self, detail: String, window: &mut Window, cx: &mut Context<Self>) {
        // 只有一个按钮，不用等回答。
        drop(window.prompt(
            PromptLevel::Warning,
            &rust_i18n::t!("files.failed"),
            Some(&detail),
            &[&*rust_i18n::t!("files.ok")],
            cx,
        ));
    }
}
