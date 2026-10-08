//! 右键菜单，以及文件树里只在菜单和快捷键里用的几个操作：在访达里显示、在新标签页里开终端、
//! 复制路径。文件树的菜单作用在选中的那一项上，点在空白处时作用在根目录上；预览标签、Git 面板、
//! 状态栏和标题栏的命令按钮也用这个菜单。

use std::path::{Path, PathBuf};

use gpui::{
    Action, Anchor, AnyElement, App, ClipboardItem, Context, Div, FocusHandle, MouseButton, Pixels, Point,
    SharedString, Stateful, Window, anchored, deferred, div, point, prelude::*, px, relative, svg,
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
/// 按钮下面弹出的菜单离按钮这么远。
const DROPDOWN_GAP: f32 = 4.;
const MENU_WIDTH: f32 = 240.;
/// 项多了在菜单里滚动。
const MENU_MAX_HEIGHT: f32 = 480.;

/// 菜单里的一项：点了把 `action` 派发给菜单的 `target`，和按快捷键走同一条路；没有 `action` 的
/// 只是一行字，点不了。有 `button` 时整行点不了，`action` 只挂在右边这个图标按钮上。
pub(in crate::window) struct MenuItem {
    label: String,
    action: Option<Box<dyn Action>>,
    shortcut: Option<SharedString>,
    enabled: bool,
    button: Option<MenuButton>,
    /// 画在字前面的图标。
    icon: Option<&'static str>,
    /// 勾选项：`Some` 时左边留一列，`true` 时打勾。
    checked: Option<bool>,
}

/// 菜单项右边的图标按钮：图标和悬停时的说明。
pub(in crate::window) struct MenuButton {
    pub icon: &'static str,
    pub tooltip: SharedString,
}

/// 菜单里的一项，快捷键在这时查，查的是这一刻的键位表。
pub(in crate::window) fn menu_item(key: &str, action: Box<dyn Action>, enabled: bool, cx: &App) -> MenuItem {
    let shortcut = shortcut_text(action.as_ref(), cx);
    MenuItem {
        label: rust_i18n::t!(key).into_owned(),
        action: Some(action),
        shortcut,
        enabled,
        button: None,
        icon: None,
        checked: None,
    }
}

/// 可以勾选的一项，字前面画 `icon`；点了派发 `action`，打不打勾由派发后的状态决定，下次打开菜单时再查。
pub(in crate::window) fn check_item(
    label: String,
    icon: &'static str,
    checked: bool,
    action: Box<dyn Action>,
) -> MenuItem {
    MenuItem {
        label,
        action: Some(action),
        shortcut: None,
        enabled: true,
        button: None,
        icon: Some(icon),
        checked: Some(checked),
    }
}

/// 菜单里写好了字的一项，`detail` 淡淡地写在右边快捷键的位置。整行点不了，有 `button` 时点右边的
/// 图标按钮派发它的动作。
pub(in crate::window) fn text_item(
    label: String,
    detail: Option<SharedString>,
    button: Option<(MenuButton, Box<dyn Action>)>,
) -> MenuItem {
    let (button, action) = button.unzip();
    MenuItem { label, action, shortcut: detail, enabled: true, button, icon: None, checked: None }
}

/// 写好了字、点整行派发 `action` 的一项，`detail` 淡淡地写在右边快捷键的位置；没有 `action` 的
/// 灰着，当小标题用。
pub(in crate::window) fn labeled_item(
    label: String,
    detail: Option<SharedString>,
    action: Option<Box<dyn Action>>,
) -> MenuItem {
    MenuItem { label, enabled: action.is_some(), action, shortcut: detail, button: None, icon: None, checked: None }
}

/// 打开着的右键菜单：右键按下的位置，打开时就定下的各项（`None` 是分隔线），以及点了以后
/// 先把焦点交给谁、再派发动作。没有位置的是按钮下面弹出的菜单，由按钮自己画。
pub(in crate::window) struct FileMenu {
    position: Option<Point<Pixels>>,
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
        self.file_menu = Some(FileMenu { position: Some(position), items, target });
        cx.notify();
    }

    /// 弹出挂在按钮下面的菜单，按钮用 `render_dropdown` 把它画在自己下面。
    pub(in crate::window) fn open_dropdown(
        &mut self,
        items: Vec<Option<MenuItem>>,
        target: FocusHandle,
        cx: &mut Context<Self>,
    ) {
        self.file_menu = Some(FileMenu { position: None, items, target });
        cx.notify();
    }

    /// 在 `position` 弹出的菜单开着。
    pub(in crate::window) fn menu_open_at(&self, position: Point<Pixels>) -> bool {
        self.file_menu.as_ref().is_some_and(|menu| menu.position == Some(position))
    }

    /// 挂在按钮下面的菜单开着。
    pub(in crate::window) fn dropdown_open(&self) -> bool {
        self.file_menu.as_ref().is_some_and(|menu| menu.position.is_none())
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

    /// 右键菜单，盖在窗口最上层，左上角对着右键按下的位置。
    pub(in crate::window) fn render_file_menu(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<AnyElement> {
        let position = self.file_menu.as_ref()?.position?;
        let list = self.render_menu_list(fg, bg, cx)?;
        Some(
            deferred(anchored().position(position).snap_to_window_with_margin(px(MENU_MARGIN)).child(list))
                .with_priority(1)
                .into_any_element(),
        )
    }

    /// 挂在按钮下面的菜单，按钮把它作为子元素：右上角对着按钮的右下角，往下让出一点；放不下时
    /// 贴着窗口边挪进来。
    pub(in crate::window) fn render_dropdown(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<Div> {
        if !self.dropdown_open() {
            return None;
        }
        let list = self.render_menu_list(fg, bg, cx)?;
        Some(
            div().absolute().bottom_0().right_0().child(
                deferred(
                    anchored()
                        .anchor(Anchor::TopRight)
                        .offset(point(px(0.), px(DROPDOWN_GAP)))
                        .snap_to_window_with_margin(px(MENU_MARGIN))
                        .child(list),
                )
                .with_priority(1),
            ),
        )
    }

    /// 菜单本身；点到菜单外面就关掉。
    fn render_menu_list(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let menu = self.file_menu.as_ref()?;
        let hover_bg = hsla(bg.mix(fg, 0.12));
        let menu_bg = hsla(bg.mix(fg, 0.06));
        let fg_rgb = fg;
        let fg = hsla(fg);
        // 有勾选项时每一行左边都留出打勾的那一列，字对齐。
        let check_column = menu.items.iter().flatten().any(|item| item.checked.is_some());
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
                .when(check_column, |row| {
                    row.child(
                        div().flex_none().w(px(10.)).mr(px(-8.)).children(item.checked.filter(|c| *c).map(|_| "✓")),
                    )
                })
                .children(
                    item.icon
                        .map(|icon| svg().flex_none().path(icon).size(px(13.)).mr(px(-8.)).text_color(fg.opacity(0.7))),
                )
                .child(div().flex_1().min_w_0().truncate().child(item.label.clone()))
                .children(
                    item.shortcut
                        .clone()
                        // 命令的说明可能很长，最多占一行的六成，多了截断。
                        .map(|shortcut| {
                            div()
                                .flex_none()
                                .max_w(relative(0.6))
                                .truncate()
                                .text_color(fg.opacity(0.45))
                                .child(shortcut)
                        }),
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
            .max_h(px(MENU_MAX_HEIGHT))
            .overflow_y_scroll()
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
        Some(list)
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
