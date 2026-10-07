//! 右键菜单，以及文件树里只在菜单和快捷键里用的几个操作：在访达里显示、在新标签页里开终端、
//! 复制路径。文件树的菜单作用在选中的那一项上，点在空白处时作用在根目录上；预览标签也用这个菜单。

use std::path::{Path, PathBuf};

use gpui::{
    Action, AnyElement, App, ClipboardItem, Context, FocusHandle, MouseButton, Pixels, Point, SharedString, Window,
    anchored, deferred, div, prelude::*, px, svg,
};
use runode_shared_types::color::Rgb;

use super::{
    CopyPath, CopyRelativePath, DeleteFile, InsertFilePath, NewFile, NewFolder, OpenInTerminal, OpenSelectedFile,
    RenameFile, RevealInFinder,
};
use crate::{
    ui::{
        actions::{Copy, Cut, Paste},
        hsla,
        tooltip::{shortcut_text, tooltip},
    },
    window::WindowView,
};

/// 菜单离窗口边缘至少留这么宽。
const MENU_MARGIN: f32 = 8.;
const MENU_WIDTH: f32 = 240.;

/// 菜单里的一项：点了把 `action` 派发给菜单的 `target`，和按快捷键走同一条路；没有 `action` 的
/// 只是一行字，点不了。有 `button` 时整行点不了，`action` 只挂在右边这个图标按钮上。
pub(in crate::window) struct MenuItem {
    label: String,
    action: Option<Box<dyn Action>>,
    shortcut: Option<SharedString>,
    enabled: bool,
    button: Option<MenuButton>,
}

/// 菜单项右边的图标按钮：图标和悬停时的说明。
pub(in crate::window) struct MenuButton {
    pub icon: &'static str,
    pub tooltip: SharedString,
}

/// 菜单里的一项，快捷键在这时查，查的是这一刻的键位表。
pub(in crate::window) fn menu_item(key: &str, action: Box<dyn Action>, enabled: bool, cx: &App) -> MenuItem {
    let shortcut = shortcut_text(action.as_ref(), cx);
    MenuItem { label: rust_i18n::t!(key).into_owned(), action: Some(action), shortcut, enabled, button: None }
}

/// 菜单里写好了字的一项，`detail` 淡淡地写在右边快捷键的位置。整行点不了，有 `button` 时点右边的
/// 图标按钮派发它的动作。
pub(in crate::window) fn text_item(
    label: String,
    detail: Option<SharedString>,
    button: Option<(MenuButton, Box<dyn Action>)>,
) -> MenuItem {
    let (button, action) = button.unzip();
    MenuItem { label, action, shortcut: detail, enabled: true, button }
}

/// 打开着的右键菜单：右键按下的位置，打开时就定下的各项（`None` 是分隔线），以及点了以后
/// 先把焦点交给谁、再派发动作。
pub(in crate::window) struct FileMenu {
    position: Point<Pixels>,
    items: Vec<Option<MenuItem>>,
    target: FocusHandle,
}

impl WindowView {
    /// 在 `position` 弹出 `items`，点了的那项先把焦点交给 `target` 再派发。
    pub(in crate::window) fn open_menu(
        &mut self,
        position: Point<Pixels>,
        items: Vec<Option<MenuItem>>,
        target: FocusHandle,
        cx: &mut Context<Self>,
    ) {
        self.file_menu = Some(FileMenu { position, items, target });
        cx.notify();
    }

    /// 在 `position` 弹出的菜单开着。
    pub(in crate::window) fn menu_open_at(&self, position: Point<Pixels>) -> bool {
        self.file_menu.as_ref().is_some_and(|menu| menu.position == position)
    }

    /// 在 `position` 弹出文件树的右键菜单，作用在当前选中的那一项上：选中文件、选中目录和点在
    /// 空白处各有不同。
    pub(super) fn open_file_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let selected = self.selected_entry();
        let can_paste = self.file_clipboard.is_some();
        let item = |key: &str, action: Box<dyn Action>| Some(menu_item(key, action, true, cx));
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
        items.push(item("menu.paste", Box::new(Paste)).map(|paste| MenuItem { enabled: can_paste, ..paste }));
        items.extend([
            None,
            item("files.copy_path", Box::new(CopyPath)),
            item("files.copy_relative_path", Box::new(CopyRelativePath)),
        ]);
        if selected.is_some() {
            items.extend([
                None,
                item("files.rename", Box::new(RenameFile)),
                item("files.delete", Box::new(DeleteFile)),
            ]);
        }
        let target = self.files_focus.clone();
        self.open_menu(position, items, target, cx);
    }

    /// 右键菜单，盖在窗口最上层；点到菜单外面就关掉。
    pub(in crate::window) fn render_file_menu(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.file_menu.as_ref()?;
        let hover_bg = hsla(bg.mix(fg, 0.12));
        let menu_bg = hsla(bg.mix(fg, 0.06));
        let fg_rgb = fg;
        let fg = hsla(fg);
        let items = menu.items.iter().enumerate().map(|(ix, item)| {
            let Some(item) = item else {
                return div().flex_none().h(px(1.)).mx(px(6.)).my(px(4.)).bg(fg.opacity(0.12)).into_any_element();
            };
            let action = item.action.as_ref().filter(|_| item.enabled).map(|action| action.boxed_clone());
            let target = menu.target.clone();
            let dispatch = |action: Box<dyn Action>| {
                let target = target.clone();
                cx.listener(move |this: &mut Self, _: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.file_menu = None;
                    window.focus(&target, cx);
                    window.dispatch_action(action.boxed_clone(), cx);
                    cx.notify();
                })
            };
            let (row_action, button) = match &item.button {
                Some(button) => (None, action.map(|action| (button, action))),
                None => (action, None),
            };
            div()
                .id(("file-menu", ix))
                .flex_none()
                .h(px(24.))
                .px(px(10.))
                .mx(px(4.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .gap(px(16.))
                .text_color(if item.enabled { fg } else { fg.opacity(0.35) })
                .when_some(row_action, |row, action| {
                    row.hover(|row| row.bg(hover_bg)).on_mouse_down(MouseButton::Left, dispatch(action))
                })
                .child(div().flex_1().min_w_0().truncate().child(item.label.clone()))
                .children(
                    item.shortcut
                        .clone()
                        .map(|shortcut| div().flex_none().text_color(fg.opacity(0.45)).child(shortcut)),
                )
                .children(button.map(|(button, action)| {
                    div()
                        .id(("file-menu-button", ix))
                        .flex_none()
                        .size(px(18.))
                        .mr(px(-6.))
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|button| button.bg(hover_bg))
                        .tooltip(tooltip(button.tooltip.clone(), None, fg_rgb, bg))
                        .child(svg().path(button.icon).size(px(12.)).text_color(fg.opacity(0.6)))
                        .on_mouse_down(MouseButton::Left, dispatch(action))
                }))
                .into_any_element()
        });
        let list = div()
            .id("file-menu")
            .w(px(MENU_WIDTH))
            .py(px(4.))
            .flex()
            .flex_col()
            .rounded(px(6.))
            .border_1()
            .border_color(fg.opacity(0.15))
            .bg(menu_bg)
            .shadow_lg()
            .text_size(px(12.))
            .occlude()
            .children(items)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.file_menu = None;
                cx.notify();
            }));
        Some(
            deferred(anchored().position(menu.position).snap_to_window_with_margin(px(MENU_MARGIN)).child(list))
                .with_priority(1)
                .into_any_element(),
        )
    }

    /// 选中的那一项，没选中时是根目录。
    fn selected_or_root(&self) -> PathBuf {
        self.selected_entry().map_or_else(|| self.files_root(), |(path, _)| path)
    }

    pub(super) fn reveal_in_finder(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        cx.reveal_path(&self.selected_or_root());
    }

    pub(super) fn insert_file_path(&mut self, _: &InsertFilePath, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, _)) = self.selected_entry() {
            self.insert_path(&path, None, window, cx);
        }
    }

    /// 在当前 workspace 里开一个新标签页，终端的目录是 `target_dir`。
    pub(super) fn open_in_terminal(&mut self, _: &OpenInTerminal, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.target_dir();
        if let Some(view) = self.spawn_terminal(Some(&dir), window, cx) {
            self.insert_tab(self.workspace().active + 1, view, window, cx);
        }
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
