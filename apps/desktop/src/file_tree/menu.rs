//! 文件树的右键菜单，以及只在菜单和快捷键里用的几个操作：在访达里显示、在新标签页里开终端、
//! 复制路径。菜单作用在选中的那一项上，点在空白处时作用在根目录上；菜单本身见 `crate::ui::menu`。

use std::path::{Path, PathBuf};

use crate::ui::{
    actions::{Copy, Cut, Paste},
    menu::{MenuHost, MenuItem},
};
use gpui::{Action, ClipboardItem, Context, Pixels, Point, Window};

use crate::file_tree::{
    AddToGitignore, CopyPath, CopyRelativePath, DeleteFile, FileTree, FileTreeEvent, InsertFilePath, NewFile,
    NewFolder, OpenInTerminal, OpenSelectedFile, RenameFile, RevealInFinder,
};

impl FileTree {
    /// 在 `position` 弹出文件树的右键菜单，作用在当前选中的那一项上：选中文件、选中目录和点在
    /// 空白处各有不同。
    pub(super) fn open_file_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let selected = self.selected_entry();
        let can_paste = self.clipboard.is_some();
        let item = |key: &str, action: Box<dyn Action>| Some(MenuItem::new(rust_i18n::t!(key), action, true, cx));
        let mut items = vec![
            item("files.new_file", Box::new(NewFile)),
            item("files.new_folder", Box::new(NewFolder)),
            None,
            item("files.reveal", Box::new(RevealInFinder)),
            item("files.open_in_terminal", Box::new(OpenInTerminal)),
        ];
        if let Some((_, false)) = selected {
            items.extend([
                None,
                item("files.preview", Box::new(OpenSelectedFile)),
                item("files.insert_path", Box::new(InsertFilePath)),
            ]);
        }
        items.push(None);
        if selected.is_some() {
            items.extend([item("menu.cut", Box::new(Cut)), item("menu.copy", Box::new(Copy))]);
        }
        items.push(Some(MenuItem::new(rust_i18n::t!("menu.paste"), Box::new(Paste), can_paste, cx)));
        items.extend([
            None,
            item("files.copy_path", Box::new(CopyPath)),
            item("files.copy_relative_path", Box::new(CopyRelativePath)),
        ]);
        if self.gitignore_target().is_some() {
            items.extend([None, item("files.add_to_gitignore", Box::new(AddToGitignore))]);
        }
        if selected.is_some() {
            items.extend([
                None,
                item("files.rename", Box::new(RenameFile)),
                item("files.delete", Box::new(DeleteFile)),
            ]);
        }
        let target = self.focus.clone();
        self.open_context_menu(position, items, target, cx);
    }

    /// 选中的那一项，没选中时是根目录。
    fn selected_or_root(&self) -> PathBuf {
        self.selected_entry().map_or_else(|| self.files_root(), |(path, _)| path)
    }

    pub(super) fn reveal_in_finder(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        cx.reveal_path(&self.selected_or_root());
    }

    pub(super) fn insert_file_path(&mut self, _: &InsertFilePath, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, _)) = self.selected_entry() {
            cx.emit(FileTreeEvent::InsertPath(path));
        }
    }

    /// 在当前 workspace 里开一个新标签页，终端的目录是 `target_dir`。
    pub(super) fn open_in_terminal(&mut self, _: &OpenInTerminal, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(FileTreeEvent::OpenTerminal(self.target_dir()));
    }

    pub(super) fn copy_path(&mut self, _: &CopyPath, _: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.selected_or_root().display().to_string()));
    }

    /// 相对文件树根目录的路径；根目录本身是 `.`。
    pub(super) fn copy_relative_path(&mut self, _: &CopyRelativePath, _: &mut Window, cx: &mut Context<Self>) {
        let (root, path) = (self.files_root(), self.selected_or_root());
        let rel = path.strip_prefix(&root).unwrap_or(&path);
        let text = if rel.as_os_str().is_empty() { Path::new(".") } else { rel };
        cx.write_to_clipboard(ClipboardItem::new_string(text.display().to_string()));
    }
}
