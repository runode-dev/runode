//! 右键菜单，以及文件树里只在菜单和快捷键里用的几个操作：在访达里显示、在新标签页里开终端、
//! 复制路径。文件树的菜单作用在选中的那一项上，点在空白处时作用在根目录上；预览标签、Git 面板、
//! 状态栏和标题栏的命令按钮也用这个菜单。

use std::path::{Path, PathBuf};

use gpui::{
    Action, Anchor, AnyElement, App, ClipboardItem, Context, Div, FocusHandle, KeyDownEvent, MouseButton, Pixels,
    Point, ScrollHandle, SharedString, Stateful, Window, anchored, deferred, div, point, prelude::*, px, relative, svg,
};
use runode_shared_types::color::Rgb;

use super::{
    CopyPath, CopyRelativePath, DeleteFile, InsertFilePath, NewFile, NewFolder, OpenInTerminal, OpenSelectedFile,
    RenameFile, RevealInFinder,
};
use crate::{
    assets::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON},
    ui::{
        actions::{Copy, Cut, Paste},
        hsla,
        tooltip::{shortcut_text, tooltip},
    },
    window::{TITLEBAR_HEIGHT, WindowView},
};

/// 菜单离窗口边缘至少留这么宽。
const MENU_MARGIN: f32 = 8.;
/// 按钮下面弹出的菜单离按钮这么远。
const DROPDOWN_GAP: f32 = 4.;
const MENU_WIDTH: f32 = 240.;

/// 菜单里的一项：点了把 `action` 派发给菜单的 `target`，和按快捷键走同一条路；没有 `action` 的
/// 只是一行字，点不了。`buttons` 是右边的图标按钮和它们派发的动作：整行点不了时一直显示，整行能点
/// 时只在选中这一行时显示。
pub(in crate::window) struct MenuItem {
    label: String,
    action: Option<Box<dyn Action>>,
    shortcut: Option<SharedString>,
    enabled: bool,
    buttons: Vec<(MenuButton, Box<dyn Action>)>,
    /// 画在字前面的图标。
    icon: Option<&'static str>,
    /// 勾选项：`Some` 时左边留一列，`true` 时打勾。
    checked: Option<bool>,
    /// 点了菜单不关、焦点不动：分组标题这类改了菜单本身的项。
    keep_open: bool,
    /// 鼠标停在这一行上时显示的完整说明，行里的字截断了也看得全。
    tooltip: Option<SharedString>,
}

/// 菜单项右边的图标按钮：图标和悬停时的说明；`keep_open` 时按了菜单不关、焦点不动；选中这一行时按
/// Cmd 加 `cmd_key` 也按它。
pub(in crate::window) struct MenuButton {
    pub icon: &'static str,
    pub tooltip: SharedString,
    pub keep_open: bool,
    pub cmd_key: Option<&'static str>,
}

impl MenuItem {
    /// 鼠标停在这一行上时显示 `text`。
    pub(in crate::window) fn with_tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// 右边再加一个图标按钮，按了派发 `action`。
    pub(in crate::window) fn with_button(mut self, button: MenuButton, action: Box<dyn Action>) -> Self {
        self.buttons.push((button, action));
        self
    }
}

/// 菜单里的一项，快捷键在这时查，查的是这一刻的键位表。
pub(in crate::window) fn menu_item(key: &str, action: Box<dyn Action>, enabled: bool, cx: &App) -> MenuItem {
    let shortcut = shortcut_text(action.as_ref(), cx);
    MenuItem {
        label: rust_i18n::t!(key).into_owned(),
        action: Some(action),
        shortcut,
        enabled,
        buttons: Vec::new(),
        icon: None,
        checked: None,
        keep_open: false,
        tooltip: None,
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
        buttons: Vec::new(),
        icon: Some(icon),
        checked: Some(checked),
        keep_open: false,
        tooltip: None,
    }
}

/// 菜单里写好了字的一项，`detail` 淡淡地写在右边快捷键的位置。整行点不了，有 `button` 时点右边的
/// 图标按钮派发它的动作。
pub(in crate::window) fn text_item(
    label: String,
    detail: Option<SharedString>,
    button: Option<(MenuButton, Box<dyn Action>)>,
) -> MenuItem {
    MenuItem {
        label,
        action: None,
        shortcut: detail,
        enabled: true,
        buttons: button.into_iter().collect(),
        icon: None,
        checked: None,
        keep_open: false,
        tooltip: None,
    }
}

/// 写好了字、点整行派发 `action` 的一项，`detail` 淡淡地写在右边快捷键的位置；没有 `action` 的
/// 灰着，当小标题用。
pub(in crate::window) fn labeled_item(
    label: String,
    detail: Option<SharedString>,
    action: Option<Box<dyn Action>>,
) -> MenuItem {
    MenuItem {
        label,
        enabled: action.is_some(),
        action,
        shortcut: detail,
        buttons: Vec::new(),
        icon: None,
        checked: None,
        keep_open: false,
        tooltip: None,
    }
}

/// 可以收起的一组的标题，前面画展开或收起（`folded`）的箭头，`detail` 写在右边。点了派发 `action`，
/// 菜单不关，由动作的处理方用 `replace_menu_items` 换上收起或展开后的各项。
pub(in crate::window) fn group_item(
    label: String,
    detail: Option<SharedString>,
    folded: bool,
    action: Box<dyn Action>,
) -> MenuItem {
    MenuItem {
        label,
        action: Some(action),
        shortcut: detail,
        enabled: true,
        buttons: Vec::new(),
        icon: Some(if folded { CHEVRON_RIGHT_ICON } else { CHEVRON_DOWN_ICON }),
        checked: None,
        keep_open: true,
        tooltip: None,
    }
}

/// 打开着的右键菜单：右键按下的位置，打开时就定下的各项（`None` 是分隔线），以及点了以后
/// 先把焦点交给谁、再派发动作。没有位置的是按钮下面弹出的菜单，由按钮自己画。
///
/// 菜单开着时焦点在它自己身上，上下键选、回车派发、Esc 关掉；不这样的话上下键和回车先被
/// 文件树这类地方绑定的动作拿走。
pub(in crate::window) struct FileMenu {
    position: Option<Point<Pixels>>,
    items: Vec<Option<MenuItem>>,
    target: FocusHandle,
    focus: FocusHandle,
    /// 刚打开，下次画窗口时把焦点给菜单：打开菜单的地方大多拿不到 `Window`。
    focus_pending: bool,
    /// 选中的那一项，鼠标悬停和上下键都改它。
    highlighted: Option<usize>,
    scroll: ScrollHandle,
}

impl FileMenu {
    fn new(position: Option<Point<Pixels>>, items: Vec<Option<MenuItem>>, target: FocusHandle, cx: &mut App) -> Self {
        let focus = cx.focus_handle();
        Self { position, items, target, focus, focus_pending: true, highlighted: None, scroll: ScrollHandle::new() }
    }

    /// 第 `ix` 项能选：整行点了有动作。
    fn selectable(&self, ix: usize) -> bool {
        self.items[ix].as_ref().is_some_and(|item| item.enabled && item.action.is_some())
    }

    /// 选中往下（`down`）或往上数的下一个能选的项，到头了绕回来。
    fn move_highlight(&mut self, down: bool) {
        let n = self.items.len();
        let next = (1..=n)
            .map(|step| match (self.highlighted, down) {
                (Some(ix), true) => (ix + step) % n,
                (Some(ix), false) => (ix + n - step) % n,
                (None, true) => step - 1,
                (None, false) => n - step,
            })
            .find(|&ix| self.selectable(ix));
        if let Some(ix) = next {
            self.highlighted = Some(ix);
            self.scroll.scroll_to_item(ix);
        }
    }
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
        self.file_menu = Some(FileMenu::new(Some(position), items, target, cx));
        cx.notify();
    }

    /// 弹出挂在按钮下面的菜单，按钮用 `render_dropdown` 把它画在自己下面。给了 `select_after` 时
    /// 先选中它后面第一个能选的项（后面没有就绕回来），从键盘打开时回车就能用。
    pub(in crate::window) fn open_dropdown(
        &mut self,
        items: Vec<Option<MenuItem>>,
        target: FocusHandle,
        select_after: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let mut menu = FileMenu::new(None, items, target, cx);
        if let Some(ix) = select_after {
            menu.highlighted = Some(ix);
            menu.move_highlight(true);
        }
        self.file_menu = Some(menu);
        cx.notify();
    }

    /// 刚打开的菜单拿到焦点。每次画窗口时调。
    pub(in crate::window) fn focus_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = &mut self.file_menu
            && std::mem::take(&mut menu.focus_pending)
        {
            window.focus(&menu.focus, cx);
        }
    }

    /// 关掉菜单；焦点还在菜单上时还给打开它之前的地方。
    pub(in crate::window) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.file_menu.take() {
            if menu.focus.is_focused(window) {
                window.focus(&menu.target, cx);
            }
            cx.notify();
        }
    }

    /// 关掉菜单，焦点给 `target`，再派发 `action`：和按快捷键走同一条路。
    /// 换掉开着的菜单的各项，选中的位置尽量不变。
    pub(in crate::window) fn replace_menu_items(&mut self, items: Vec<Option<MenuItem>>, cx: &mut Context<Self>) {
        if let Some(menu) = &mut self.file_menu {
            menu.items = items;
            // 选中的那一项没了（删掉了一条）时，选中它后面能选的那一项。
            if let Some(ix) = menu.highlighted.filter(|&ix| ix >= menu.items.len() || !menu.selectable(ix)) {
                menu.highlighted = Some(ix.min(menu.items.len().saturating_sub(1)));
                menu.move_highlight(true);
            }
            cx.notify();
        }
    }

    /// 点了或回车第 `ix` 项：`keep_open` 的直接派发，菜单和焦点都不动；别的先关掉菜单。
    fn activate_menu_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = &mut self.file_menu else {
            return;
        };
        menu.highlighted = Some(ix);
        let item = menu.items[ix].as_ref().filter(|item| item.enabled);
        let Some((action, keep_open)) =
            item.and_then(|item| Some((item.action.as_ref()?.boxed_clone(), item.keep_open)))
        else {
            return;
        };
        if keep_open {
            window.dispatch_action(action, cx);
            cx.notify();
        } else {
            self.run_menu_action(action.as_ref(), window, cx);
        }
    }

    /// 按第 `ix` 项右边的第 `button` 个按钮。
    fn press_menu_button(&mut self, ix: usize, button: usize, window: &mut Window, cx: &mut Context<Self>) {
        let button = self.file_menu.as_ref().and_then(|menu| menu.items[ix].as_ref()?.buttons.get(button));
        let Some((keep_open, action)) = button.map(|(button, action)| (button.keep_open, action.boxed_clone())) else {
            return;
        };
        if keep_open {
            window.dispatch_action(action, cx);
            cx.notify();
        } else {
            self.run_menu_action(action.as_ref(), window, cx);
        }
    }

    fn run_menu_action(&mut self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.file_menu.take() else {
            return;
        };
        window.focus(&menu.target, cx);
        window.dispatch_action(action.boxed_clone(), cx);
        cx.notify();
    }

    fn menu_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = &mut self.file_menu else {
            return;
        };
        let modifiers = event.keystroke.modifiers;
        if modifiers.platform && !modifiers.shift && !modifiers.alt && !modifiers.control {
            let key = event.keystroke.key.as_str();
            let pressed = menu.highlighted.and_then(|ix| {
                let buttons = &menu.items[ix].as_ref()?.buttons;
                Some((ix, buttons.iter().position(|(button, _)| button.cmd_key == Some(key))?))
            });
            if let Some((ix, button)) = pressed {
                cx.stop_propagation();
                self.press_menu_button(ix, button, window, cx);
            }
            return;
        }
        if modifiers.modified() {
            return;
        }
        match event.keystroke.key.as_str() {
            "up" | "down" => {
                menu.move_highlight(event.keystroke.key == "down");
                cx.notify();
            }
            "enter" => {
                if let Some(ix) = menu.highlighted {
                    self.activate_menu_item(ix, window, cx);
                }
            }
            "escape" => self.close_menu(window, cx),
            _ => return,
        }
        cx.stop_propagation();
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
    pub(in crate::window) fn render_file_menu(
        &self,
        fg: Rgb,
        bg: Rgb,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let position = self.file_menu.as_ref()?.position?;
        let max_height = f32::from(window.viewport_size().height) - MENU_MARGIN * 2.;
        let list = self.render_menu_list(max_height, fg, bg, cx)?;
        Some(
            deferred(anchored().position(position).snap_to_window_with_margin(px(MENU_MARGIN)).child(list))
                .with_priority(1)
                .into_any_element(),
        )
    }

    /// 挂在按钮下面的菜单，按钮把它作为子元素：右上角对着按钮的右下角，往下让出一点；放不下时
    /// 贴着窗口边挪进来。按钮在标题栏里，菜单最高到窗口底边。
    pub(in crate::window) fn render_dropdown(
        &self,
        fg: Rgb,
        bg: Rgb,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.dropdown_open() {
            return None;
        }
        let max_height = f32::from(window.viewport_size().height) - TITLEBAR_HEIGHT - DROPDOWN_GAP - MENU_MARGIN;
        let list = self.render_menu_list(max_height, fg, bg, cx)?;
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

    /// 菜单本身，最高 `max_height`，超出窗口时才在里面滚动；点到菜单外面就关掉。
    fn render_menu_list(&self, max_height: f32, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let menu = self.file_menu.as_ref()?;
        let hover_bg = hsla(bg.mix(fg, 0.12));
        // 选中的行底色已经是 `hover_bg`，行里按钮悬停时再深一些。
        let button_hover_bg = hsla(bg.mix(fg, 0.22));
        let menu_bg = hsla(bg.mix(fg, 0.06));
        let fg_rgb = fg;
        let fg = hsla(fg);
        // 有勾选项时每一行左边都留出打勾的那一列，字对齐。
        let check_column = menu.items.iter().flatten().any(|item| item.checked.is_some());
        let items = menu.items.iter().enumerate().map(|(ix, item)| {
            let Some(item) = item else {
                return div().flex_none().h(px(1.)).mx(px(6.)).my(px(4.)).bg(fg.opacity(0.12)).into_any_element();
            };
            let row_action = item.enabled && item.action.is_some();
            let highlighted = menu.highlighted == Some(ix);
            // 整行能点的只在选中时露出按钮，不然每行行尾都摆一个。
            let buttons = (item.enabled && (!row_action || highlighted)).then_some(&item.buttons);
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
                .when(highlighted, |row| row.bg(hover_bg))
                .when_some(item.tooltip.clone(), |row, text| row.tooltip(tooltip(text, None, fg_rgb, bg)))
                .when(row_action, |row| {
                    row.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if let Some(menu) = this.file_menu.as_mut().filter(|_| *hovered) {
                            menu.highlighted = Some(ix);
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.activate_menu_item(ix, window, cx);
                        }),
                    )
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
                .children(buttons.filter(|buttons| !buttons.is_empty()).map(|buttons| {
                    div().flex_none().mr(px(-6.)).flex().gap(px(2.)).children(buttons.iter().enumerate().map(
                        |(b, (button, _))| {
                            div()
                                .id(("file-menu-button", ix * 4 + b))
                                .flex_none()
                                .size(px(18.))
                                .rounded(px(4.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|button| button.bg(button_hover_bg))
                                .tooltip(tooltip(button.tooltip.clone(), None, fg_rgb, bg))
                                .child(svg().path(button.icon).size(px(12.)).text_color(fg.opacity(0.6)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.press_menu_button(ix, b, window, cx);
                                    }),
                                )
                        },
                    ))
                }))
                .into_any_element()
        });
        let list = div()
            .id("file-menu")
            .track_focus(&menu.focus)
            .on_key_down(cx.listener(Self::menu_key))
            .track_scroll(&menu.scroll)
            .w(px(MENU_WIDTH))
            .max_h(px(max_height))
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
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_menu(window, cx)));
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
