//! 文件树里改文件：就地新建和改名的输入框，移到废纸篓，剪切复制粘贴，以及拖到目录上挪位置。
//! 改完当场重读相关的目录并选中改出来的那一项，不等监听到改动。

use std::path::{Path, PathBuf};

use crate::project_host::{ProjectHost, ProjectRepo};
use crate::ui::{
    base_name,
    file_icons::{file_icon, folder_icon},
    follow_move,
    inline_edit::InlineEdit,
};
use gpui::{AnyElement, Context, PromptButton, PromptLevel, Role, ScrollStrategy, Window, div, img, prelude::*, px};
use runode_files::ops::{self, OpError};

use crate::file_tree::{
    AddToGitignore, DeleteFile, FileTree, FileTreeEvent, NewFile, NewFolder, RenameFile, WorkspaceId, state::TreeState,
};

/// 剪切或复制下来等着粘贴的文件或目录。
#[derive(Clone)]
pub(crate) struct FileClipboard {
    /// 复制的是哪台机器上的（本机为空）：只能粘贴到同一台机器上的树里。
    pub host: Option<String>,
    pub path: PathBuf,
    /// 剪切的粘贴时挪过去，粘贴一次就清掉；复制的可以一直粘贴。
    pub cut: bool,
}

#[derive(Clone)]
enum EditTarget {
    /// 在 `dir` 里新建，输入框插在它的第一项。
    New { dir: PathBuf, is_dir: bool },
    /// 改 `path` 的名字，输入框换掉那一行的名字。
    Rename { path: PathBuf, is_dir: bool },
}

/// 文件树里正在新建或改名的输入框。
pub(crate) struct FileEdit {
    target: EditTarget,
    edit: InlineEdit,
}

/// 改 `name` 失败时提示里的说明。
fn error_text(err: &OpError, name: &str) -> String {
    match err {
        OpError::Exists => rust_i18n::t!("files.exists", name = name).into_owned(),
        OpError::IntoItself => rust_i18n::t!("files.into_itself", name = name).into_owned(),
        OpError::NewlineInName => rust_i18n::t!("files.newline_in_name").into_owned(),
        OpError::Io(err) => err.to_string(),
    }
}

impl FileTree {
    /// workspace `ws`（为空时是当前的）的文件在哪台机器上，写法同 `ProjectHost::name`。
    fn host_of(&self, ws: Option<WorkspaceId>) -> Option<String> {
        let state = ws.and_then(|ws| self.workspaces.get(&ws));
        state.map_or(self.input.project.name(), |state| state.project.name()).map(str::to_owned)
    }

    /// `key` 下按当前 workspace 的文件所在的那台机器的系统写的那一条（`i18n::os_key`）。
    pub(super) fn project_os_key(&self, key: &str, cx: &gpui::App) -> String {
        crate::i18n::os_key_on(key, self.project().system(cx))
    }

    /// 当前 workspace 的文件在哪台机器上。按 `Input` 取：还没读到目录时 `state` 是空的，那里记着的是本机。
    fn project(&self) -> ProjectHost {
        self.input.project.clone()
    }

    /// 剪贴板里有能粘贴到当前树里的：同一台机器上的。
    pub(super) fn can_paste(&self) -> bool {
        self.clipboard.as_ref().is_some_and(|clip| clip.host.as_deref() == self.input.project.name())
    }

    pub(super) fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.target_dir(cx);
        if self.shown_for_edit(&dir, cx) {
            self.start_file_edit(EditTarget::New { dir, is_dir: false }, window, cx);
        }
    }

    pub(super) fn new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.target_dir(cx);
        if self.shown_for_edit(&dir, cx) {
            self.start_file_edit(EditTarget::New { dir, is_dir: true }, window, cx);
        }
    }

    pub(super) fn rename_file(&mut self, _: &RenameFile, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, is_dir)) = self.selected_entry(cx)
            && self.shown_for_edit(&path, cx)
        {
            self.start_file_edit(EditTarget::Rename { path, is_dir }, window, cx);
        }
    }

    /// 搜索时选中的结果在树里不一定显示着，编辑会换回文件树：先展开到 `path`。它还不在树里（目录还在读，
    /// 或者被过滤藏着，比如没开显示点文件）时为假，这时不编辑：输入框看不见，按的键却在改名、建文件。
    /// 读回来以后再按一次就行。
    fn shown_for_edit(&mut self, path: &Path, cx: &mut Context<Self>) -> bool {
        if !self.searching(cx) {
            return true;
        }
        self.reveal_file(path, cx);
        *path == self.files_root() || self.state().rows.iter().any(|row| row.path == path)
    }

    /// 正在改 `path` 的名字时是那个输入框。
    pub(super) fn renaming(&self, path: &Path) -> Option<&InlineEdit> {
        let edit = self.edit.as_ref()?;
        matches!(&edit.target, EditTarget::Rename { path: renaming, .. } if renaming == path).then_some(&edit.edit)
    }

    /// 新建时输入框占的那一行：插在第几行、缩进几层。目录的那一行找不到时放在最上面，
    /// 总要看得见。
    pub(super) fn new_entry_row(&self) -> Option<(usize, usize)> {
        let EditTarget::New { dir, .. } = &self.edit.as_ref()?.target else {
            return None;
        };
        let rows = &self.state().rows;
        let row = rows.iter().position(|row| row.path == *dir).filter(|_| *dir != self.files_root());
        Some(row.map_or((0, 0), |ix| (ix + 1, rows[ix].depth + 1)))
    }

    /// 新建时插进文件树的那一行，图标跟着输入的名字变。
    pub(super) fn render_new_entry_row(&self, depth: usize, font_size: f32, cx: &Context<Self>) -> AnyElement {
        let Some(FileEdit { target: EditTarget::New { is_dir, .. }, edit }) = &self.edit else {
            return div().into_any_element();
        };
        let name = edit.field.read(cx).query();
        let icon = if *is_dir { folder_icon(name, false) } else { file_icon(name) };
        // 输入框报不了自己的名字（`InlineEdit` 建的 `InputState` 没有），这一行替它说是在新建什么。
        let label = if *is_dir { rust_i18n::t!("files.new_folder") } else { rust_i18n::t!("files.new_file") };
        Self::file_row_shell("new-file-entry", depth, font_size, cx)
            .role(Role::TreeItem)
            .aria_label(label.into_owned())
            .aria_level(depth + 1)
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
                    .child(edit.render(px(font_size + 6.)).flex_1().min_w_0()),
            )
            .into_any_element()
    }

    /// 打开新建或改名的输入框。改名时文件只选中主名，直接打字时扩展名留着。
    fn start_file_edit(&mut self, target: EditTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.start_file_edit_with(target, None, window, cx);
    }

    /// 和 `start_file_edit` 一样，`typed` 不为空时输入框里填它：新建、改名没成时把刚才输的名字放回去改。
    fn start_file_edit_with(
        &mut self,
        target: EditTarget,
        typed: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 正在改别的时先把那个改完。
        self.finish_file_edit(true, window, cx);
        self.menu = None;
        let (text, select) = match &target {
            EditTarget::New { dir, .. } => {
                self.with_tree(cx, |state, root, filter| {
                    if dir != root && !state.expanded.contains(dir) {
                        state.toggle_dir(dir, root, filter);
                    }
                });
                let name = typed.unwrap_or_default();
                let select = stem_end(&name, false);
                (name, select)
            }
            EditTarget::Rename { path, is_dir } => {
                let name = typed.unwrap_or_else(|| base_name(path));
                let select = stem_end(&name, *is_dir);
                (name, select)
            }
        };
        let edit = InlineEdit::new(
            text,
            select,
            rust_i18n::t!("workspace.edit_name").into_owned(),
            Self::finish_file_edit,
            window,
            cx,
        );
        self.edit = Some(FileEdit { target, edit });
        let row = self.new_entry_row().map(|(ix, _)| ix).or_else(|| self.state().selected_row());
        if let Some(row) = row {
            self.state_mut().scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// 关掉输入框；`commit` 时按输入的名字新建或改名，空的不算。
    fn finish_file_edit(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(FileEdit { target, edit }) = self.edit.take() else {
            return;
        };
        let name = edit.text(cx);
        edit.release_focus(&self.focus, window, cx);
        cx.notify();
        if !commit || name.is_empty() {
            return;
        }
        match target {
            EditTarget::New { dir, is_dir } => self.create_entry(&dir, &name, is_dir, window, cx),
            EditTarget::Rename { path, is_dir } => self.rename_entry(&path, is_dir, &name, window, cx),
        }
    }

    /// 新建或改名没成：弹框说为什么，关掉以后把输入框和刚才输的 `name` 放回去，改了再试。
    fn edit_failed(
        &self,
        target: EditTarget,
        name: String,
        title: &str,
        detail: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ws = self.ws;
        self.show_file_error_then(title, detail, window, cx, move |this, window, cx| {
            // 期间切到了别的 workspace 时不放回去：那棵树里没有这个输入框的位置。
            if this.ws == ws && this.edit.is_none() {
                this.start_file_edit_with(target, Some(name), window, cx);
            }
        });
    }

    fn create_entry(&mut self, dir: &Path, name: &str, is_dir: bool, window: &mut Window, cx: &mut Context<Self>) {
        let target = EditTarget::New { dir: dir.to_path_buf(), is_dir };
        let Some(path) = ops::child_path(dir, name) else {
            let detail = rust_i18n::t!("files.invalid_name", name = name).into_owned();
            self.edit_failed(target, name.to_owned(), "files.failed_new", detail, window, cx);
            return;
        };
        // 同 `transfer`：在后台建，网络卷挂住时不卡界面；完成时可能已经切走了，选中和定位的是发起的那个 workspace 的树。
        let (ws, dir, name) = (self.ws, dir.to_path_buf(), name.to_owned());
        let project = self.project();
        let job = cx.background_spawn({
            let path = path.clone();
            async move { project.create(&path, is_dir) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                if let Err(err) = result {
                    let detail = error_text(&err, &name);
                    this.edit_failed(target, name, "files.failed_new", detail, window, cx);
                    return;
                }
                this.relist_and_reveal(ws, vec![dir], &path, cx);
                // 新建的文件直接开成固定标签，接着就能看；已经切到别的 workspace 时不开到那边的预览栏里。
                if !is_dir && ws == this.ws {
                    cx.emit(FileTreeEvent::Open { path, line: None, hit: None, pin: true });
                }
            })
            .ok();
        })
        .detach();
    }

    /// 改名只改名字，不挪到别的目录。
    fn rename_entry(&mut self, path: &Path, is_dir: bool, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if ops::unchanged_name(path, name) {
            return;
        }
        let target = EditTarget::Rename { path: path.to_path_buf(), is_dir };
        let Some(to) = path.parent().and_then(|parent| ops::child_name(parent, name)) else {
            let detail = rust_i18n::t!("files.invalid_name", name = name).into_owned();
            self.edit_failed(target, name.to_owned(), "files.failed_rename", detail, window, cx);
            return;
        };
        // 同 `create_entry`，在后台改。
        let (ws, from, name) = (self.ws, path.to_path_buf(), name.to_owned());
        let project = self.project();
        let job = cx.background_spawn({
            let (from, to) = (from.clone(), to.clone());
            async move { project.rename(&from, &to) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => this.after_move(ws, &from, &to, cx),
                Err(err) => {
                    let detail = error_text(&err, &name);
                    this.edit_failed(target, name, "files.failed_rename", detail, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 当场重读 workspace `ws` 的树里的 `dirs`，再选中 `path`、滚到它。
    fn relist_and_reveal(&mut self, ws: Option<WorkspaceId>, dirs: Vec<PathBuf>, path: &Path, cx: &mut Context<Self>) {
        self.with_tree_of(ws, cx, |state, root, filter| {
            state.relist(dirs, root, filter);
            state.reveal_file(path, root, filter);
        });
        cx.notify();
    }

    /// workspace `ws` 里 `from` 改了名或挪到了 `to`：展开的目录、选中的路径和预览的文件跟过去，重读两边的目录，
    /// 选中挪过去的那一项。剪贴板整个窗口一份，别的 workspace 的树和预览也跟过去。
    fn after_move(&mut self, ws: Option<WorkspaceId>, from: &Path, to: &Path, cx: &mut Context<Self>) {
        self.with_tree_of(ws, cx, |state, _, _| state.moved(from, to));
        let host = self.host_of(ws);
        if let Some(clip) = &mut self.clipboard
            && clip.host == host
            && let Some(path) = follow_move(&clip.path, from, to)
        {
            clip.path = path;
        }
        let dirs: Vec<PathBuf> = [from.parent(), to.parent()].into_iter().flatten().map(Path::to_path_buf).collect();
        self.update_other_trees(ws, &dirs, cx, |state| state.moved(from, to));
        self.relist_and_reveal(ws, dirs, to, cx);
        cx.emit(FileTreeEvent::Moved { from: from.to_path_buf(), to: to.to_path_buf(), host });
    }

    /// 除了 `ws` 以外、文件和它在同一台机器上的各 workspace 的树做 `f`，再重读 `dirs` 里那棵树读过的目录：
    /// 剪贴板整个窗口一份，可以跨 workspace 挪、删，几个 workspace 也可能开在同一个目录下。不动它们选中的行。
    fn update_other_trees(
        &mut self,
        ws: Option<WorkspaceId>,
        dirs: &[PathBuf],
        cx: &mut Context<Self>,
        f: impl Fn(&mut TreeState),
    ) {
        let host = ws.and_then(|ws| self.workspaces.get(&ws)).map(|state| state.project.name().map(str::to_owned));
        let others: Vec<_> = self
            .workspaces
            .iter()
            .filter(|(id, state)| {
                Some(**id) != ws && host.as_ref().is_none_or(|host| state.project.name() == host.as_deref())
            })
            .map(|(id, _)| Some(*id))
            .collect();
        for id in others {
            self.with_tree_of(id, cx, |state, root, filter| {
                f(state);
                let listed = dirs.iter().filter(|dir| state.listings.contains_key(*dir)).cloned().collect();
                state.relist(listed, root, filter);
            });
        }
    }

    /// 选中的那一项先问一下，再在后台移到废纸篓：外置卷上可能要复制一遍。
    pub(super) fn delete_file(&mut self, _: &DeleteFile, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let Some((path, is_dir)) = self.selected_entry(cx) else {
            return;
        };
        let name = base_name(&path);
        let project = self.project();
        let detail = if is_dir { "files.delete_dir_detail" } else { "files.delete_detail" };
        let title = rust_i18n::t!(&self.project_os_key("files.delete_title", cx), name = name);
        let detail = crate::i18n::tr(&self.project_os_key(detail, cx));
        let confirm = crate::i18n::tr(&self.project_os_key("files.delete_confirm", cx));
        // 找不到回收站时的提示也按文件所在的那台机器说。
        let unsupported = crate::i18n::tr(&self.project_os_key("files.unsupported", cx));
        let answer = crate::prompt::show(
            window,
            PromptLevel::Warning,
            &title,
            Some(&detail),
            &[PromptButton::new(&confirm), PromptButton::cancel(rust_i18n::t!("files.cancel"))],
            cx,
        );
        // 完成时可能已经切到了别的 workspace，选中和定位的是发起的那个 workspace 的树（预览和别的树都跟着改）。
        let ws = self.ws;
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let result = cx.background_spawn({
                let path = path.clone();
                async move { project.trash(&path) }
            });
            let result = result.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => this.after_delete(ws, &path, window, cx),
                Err(OpError::Io(err)) if err.kind() == std::io::ErrorKind::Unsupported => {
                    this.show_file_error("files.failed_delete", unsupported, window, cx)
                }
                Err(err) => this.show_file_error("files.failed_delete", error_text(&err, &name), window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// workspace `ws` 里 `path` 移到了废纸篓：关掉各 workspace 里它和它下面的文件的预览标签，选中原来那个位置上
    /// 的行，接着按删除键可以一路删下去；别的 workspace 的树也去掉它。剪贴板整个窗口一份，不管是哪个 workspace
    /// 都清掉。
    fn after_delete(&mut self, ws: Option<WorkspaceId>, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let host = self.host_of(ws);
        if self.clipboard.as_ref().is_some_and(|clip| clip.host == host && clip.path.starts_with(path)) {
            self.clipboard = None;
        }
        cx.emit(FileTreeEvent::Removed { path: path.to_path_buf(), host });
        self.update_other_trees(ws, &[], cx, |state| state.removed(path));
        self.with_tree_of(ws, cx, |state, root, filter| {
            let ix = state.selected_row();
            state.removed(path);
            state.selected = None;
            state.relist(path.parent().map(Path::to_path_buf).into_iter().collect(), root, filter);
            let last = state.rows.len().checked_sub(1);
            if let Some(ix) = ix.zip(last).and_then(|(ix, last)| state.select_row(ix.min(last))) {
                state.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            }
        });
        if ws == self.ws {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// 选中项所在的仓库（子仓库里的算子仓库）的根目录、相对它的路径和是不是目录；不在仓库里、
    /// 已经被忽略、或者选中的就是仓库根目录时为空。
    pub(super) fn gitignore_target(&self, cx: &gpui::App) -> Option<(ProjectRepo, PathBuf, bool)> {
        let (path, is_dir) = self.selected_entry(cx)?;
        let git = self.state().git.as_ref()?;
        let rel = path.strip_prefix(&git.main.root).ok()?;
        if git.is_ignored(rel) {
            return None;
        }
        let (repo, rel) = git.locate(rel);
        (!rel.as_os_str().is_empty()).then(|| (self.input.project.repo(repo.repo()), rel.to_path_buf(), is_dir))
    }

    /// 写进 `.gitignore` 后当场重读，选中的那一项马上标成被忽略。git 跟踪着的写进去也不生效，先问要不要
    /// 不再跟踪它（文件留在磁盘上）。
    pub(super) fn add_to_gitignore(&mut self, _: &AddToGitignore, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let Some((repo, rel, is_dir)) = self.gitignore_target(cx) else {
            return;
        };
        let project = self.project();
        let tracked = cx.background_spawn({
            let (repo, rel) = (repo.clone(), rel.clone());
            async move { repo.is_tracked(&rel) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let untrack = tracked.await;
            if untrack {
                let Ok(answer) = this.update_in(cx, |_, window, cx| {
                    crate::prompt::show(
                        window,
                        PromptLevel::Warning,
                        &rust_i18n::t!("files.untrack_title", name = base_name(&rel)),
                        Some(&rust_i18n::t!("files.untrack_detail")),
                        &[
                            PromptButton::new(rust_i18n::t!("files.untrack_confirm")),
                            PromptButton::cancel(rust_i18n::t!("files.cancel")),
                        ],
                        cx,
                    )
                }) else {
                    return;
                };
                if answer.await.ok() != Some(0) {
                    return;
                }
            }
            let result = cx
                .background_spawn(async move {
                    project.add_to_gitignore(&repo.root, &rel, is_dir)?;
                    if untrack {
                        repo.untrack(&[rel])
                            .map_err(|err| OpError::Io(std::io::Error::other(crate::git_view::git_error_text(&err))))?;
                    }
                    Ok(())
                })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => cx.emit(FileTreeEvent::Refresh),
                Err(err) => this.show_file_error("files.failed_ignore", error_text(&err, ".gitignore"), window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 剪切（`cut`）或复制选中的那一项，等着粘贴。
    pub(super) fn copy_file(&mut self, cut: bool, cx: &mut Context<Self>) {
        self.menu = None;
        if let Some((path, _)) = self.selected_entry(cx) {
            self.clipboard = Some(FileClipboard { host: self.input.project.name().map(str::to_owned), path, cut });
            cx.notify();
        }
    }

    /// 粘贴到 `target_dir`。
    pub(super) fn paste_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if !self.can_paste() {
            return;
        }
        if let Some(clip) = self.clipboard.clone() {
            let mut dir = self.target_dir(cx);
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
        let answer = crate::prompt::show(
            window,
            PromptLevel::Info,
            &title,
            None,
            &[
                PromptButton::new(rust_i18n::t!("files.move_confirm")),
                PromptButton::cancel(rust_i18n::t!("files.cancel")),
            ],
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
        // 同 `delete_file`：完成时可能已经切走了，选中和定位的是发起的那个 workspace 的树，失败时也只重读它的目录。
        let (ws, project) = (self.ws, self.project());
        let job = cx.background_spawn({
            let (src, dir) = (src.clone(), dir.clone());
            async move { if cut { project.move_into(&src, &dir) } else { project.copy_into(&src, &dir) } }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(dest) if cut => {
                    if this.clipboard.as_ref().is_some_and(|clip| clip.cut && clip.path == src) {
                        this.clipboard = None;
                    }
                    if dest != src {
                        this.after_move(ws, &src, &dest, cx);
                    }
                }
                Ok(dest) => this.relist_and_reveal(ws, vec![dir], &dest, cx),
                Err(err) => {
                    // 失败也可能已经动了磁盘（移动了一半、复制被清掉），让树和磁盘对上。
                    this.with_tree_of(ws, cx, |state, root, filter| state.relist(vec![dir], root, filter));
                    cx.notify();
                    let title = if cut { "files.failed_move" } else { "files.failed_copy" };
                    this.show_file_error(title, error_text(&err, &base_name(&src)), window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 弹框说哪个操作没成（`title` 是标题的译文键）、为什么。
    fn show_file_error(&self, title: &str, detail: String, window: &mut Window, cx: &mut Context<Self>) {
        self.show_file_error_then(title, detail, window, cx, |_, _, _| {});
    }

    /// 和 `show_file_error` 一样，框关掉以后再做 `then`。
    fn show_file_error_then(
        &self,
        title: &str,
        detail: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let answer = crate::prompt::show(
            window,
            PromptLevel::Warning,
            &rust_i18n::t!(title),
            Some(&detail),
            &[PromptButton::cancel(rust_i18n::t!("files.ok"))],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let _ = answer.await;
            this.update_in(cx, |this, window, cx| then(this, window, cx)).ok();
        })
        .detach();
    }
}

/// 名字里选中的那一段的长度：文件只选主名，打字时扩展名留着；目录和没有扩展名的选全部。
fn stem_end(name: &str, is_dir: bool) -> usize {
    let stem = if is_dir { None } else { name.rfind('.').filter(|&dot| dot > 0) };
    stem.unwrap_or(name.len())
}
