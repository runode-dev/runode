//! 菜单：右键菜单（ContextMenu，`MenuHost::open_context_menu` 打开、`render_context_menu` 画）和按钮下面
//! 弹出的菜单（DropdownMenu，`open_dropdown` 打开、按钮用 `render_dropdown_menu` 画在自己下面）共用的菜单项
//! （`MenuItem`）、键盘和鼠标的操作、画法。文件树、预览标签、Git 面板、状态栏和标题栏的命令按钮都用它；
//! 菜单开着时存在哪个视图里由 `MenuHost` 说，同一时刻只开一个。颜色在画的时候从主题读。

use std::collections::HashMap;

use gpui::{
    Action, Anchor, AnyElement, App, Context, Div, ElementId, FocusHandle, KeyDownEvent, MouseButton, Pixels, Point,
    Role, ScrollHandle, SharedString, Window, anchored, deferred, div, point, prelude::*, px, relative, svg,
};

use crate::{
    a11y::{A11yPress, Disable},
    button::{Button, ButtonSize},
    font_size::scaled,
    icons::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON},
    popover::Popover,
    theme::ActiveTheme as _,
    tooltip::{shortcut_text, tooltip},
};

/// 菜单离窗口边缘至少留这么宽。
const MENU_MARGIN: f32 = 8.;
/// 按钮下面弹出的菜单离按钮这么远。
const DROPDOWN_GAP: f32 = 4.;
/// 菜单的宽度，按基准字号 12 的点数写，跟界面字号一起缩放。
const MENU_WIDTH: f32 = 240.;

/// 菜单里的一项：点了把 `action` 派发给菜单的 `target`，和按快捷键走同一条路；没有 `action` 的
/// 只是一行字，点不了。`buttons` 是右边的图标按钮和它们派发的动作：整行点不了时一直显示，整行能点
/// 时只在选中这一行时显示。
pub struct MenuItem {
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
pub struct MenuButton {
    pub icon: &'static str,
    pub tooltip: SharedString,
    pub keep_open: bool,
    pub cmd_key: Option<&'static str>,
}

impl MenuItem {
    /// 菜单里写着 `label` 的一项，快捷键在这时查，查的是这一刻的键位表。
    pub fn new(label: impl Into<String>, action: Box<dyn Action>, enabled: bool, cx: &App) -> Self {
        let shortcut = shortcut_text(action.as_ref(), cx);
        Self {
            label: label.into(),
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

    /// 可以勾选的一项，有 `icon` 时字前面画它；点了派发 `action`，打不打勾由派发后的状态决定，下次打开菜单时再查。
    pub fn checkbox(label: String, icon: Option<&'static str>, checked: bool, action: Box<dyn Action>) -> Self {
        Self {
            label,
            action: Some(action),
            shortcut: None,
            enabled: true,
            buttons: Vec::new(),
            icon,
            checked: Some(checked),
            keep_open: false,
            tooltip: None,
        }
    }

    /// 写好了字、点整行派发 `action` 的一项，`detail` 淡淡地写在右边快捷键的位置；没有 `action` 的
    /// 灰着，当小标题用。
    pub fn label(label: String, detail: Option<SharedString>, action: Option<Box<dyn Action>>) -> Self {
        Self {
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
    pub fn group(label: String, detail: Option<SharedString>, folded: bool, action: Box<dyn Action>) -> Self {
        Self {
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

    /// 鼠标停在这一行上时显示 `text`。
    pub fn with_tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    /// 字前面画 `icon`。
    pub fn with_icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// 右边再加一个图标按钮，按了派发 `action`。
    pub fn with_button(mut self, button: MenuButton, action: Box<dyn Action>) -> Self {
        self.buttons.push((button, action));
        self
    }
}

/// 打开着的右键菜单：右键按下的位置，打开时就定下的各项（`None` 是分隔线），以及点了以后
/// 先把焦点交给谁、再派发动作。没有位置的是按钮下面弹出的菜单，由按钮自己画。菜单放在哪个视图里由
/// `MenuHost` 说。
///
/// 菜单开着时焦点在它自己身上，上下键选、回车派发、Esc 关掉；不这样的话上下键和回车先被
/// 文件树这类地方绑定的动作拿走。
pub struct Menu {
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

impl Menu {
    fn new(position: Option<Point<Pixels>>, items: Vec<Option<MenuItem>>, target: FocusHandle, cx: &mut App) -> Self {
        let focus = cx.focus_handle();
        Self { position, items, target, focus, focus_pending: true, highlighted: None, scroll: ScrollHandle::new() }
    }

    /// 选中往下（`down`）或往上数的下一个能选的项，到头了绕回来。
    fn move_highlight(&mut self, down: bool) {
        // 一项能选的都没有（比如换成了空的）时不选，回车、Cmd 加键才不会按到不在了的项。
        let next = next_selectable(&self.items, self.highlighted, down);
        self.highlighted = next;
        if let Some(ix) = next {
            self.scroll.scroll_to_item(ix);
        }
    }
}

/// 第 `ix` 项能选：整行点了有动作。
fn selectable(items: &[Option<MenuItem>], ix: usize) -> bool {
    items.get(ix).and_then(Option::as_ref).is_some_and(|item| item.enabled && item.action.is_some())
}

/// 从第 `from` 项往下（`down`）或往上数的下一个能选的项，到头了绕回来；没有 `from` 时从头或尾数起。
fn next_selectable(items: &[Option<MenuItem>], from: Option<usize>, down: bool) -> Option<usize> {
    let n = items.len();
    (1..=n)
        .map(|step| match (from, down) {
            (Some(ix), true) => (ix + step) % n,
            (Some(ix), false) => (ix + n - step) % n,
            (None, true) => step - 1,
            (None, false) => n - step,
        })
        .find(|&ix| selectable(items, ix))
}

/// 换了各项后原来选中的第 `ix` 项不能选了（那一项删掉了）时改选哪一项：它后面能选的那一项；删的是最后
/// 一项、`ix` 越界时选新的最后一项（不能选时往上找），不绕回最上面。
fn highlight_after_replace(items: &[Option<MenuItem>], ix: usize) -> Option<usize> {
    match ix {
        _ if selectable(items, ix) => Some(ix),
        _ if ix >= items.len() => next_selectable(items, Some(items.len()), false),
        _ => next_selectable(items, Some(ix), true),
    }
}

/// 菜单各行的元素 id：按所在的组和字取，不按行号，后台重列、前面多了几行时辅助工具的焦点和按下才不会
/// 跑到别的项上。组是开头和每条分隔线后面第一项的字（命令菜单里就是按来源分的组标题），不同的组里同名的项
/// （Makefile 和 package.json 都有 build）才分得开；同一组里同名的几项按第几次出现再区分。说明、选中和
/// 收起这些会变的不算进去。
fn row_ids(items: &[Option<MenuItem>]) -> Vec<ElementId> {
    let mut section = "";
    let mut seen = HashMap::<String, usize>::new();
    let mut after_separator = true;
    items
        .iter()
        .map(|item| {
            let key = match item {
                Some(item) => {
                    if std::mem::take(&mut after_separator) {
                        section = &item.label;
                    }
                    format!("{section}\u{1f}{}", item.label)
                }
                None => {
                    after_separator = true;
                    "\u{1f}separator".to_owned()
                }
            };
            let count = seen.entry(key.clone()).or_default();
            let key = if *count > 0 { format!("{key}\u{1f}{count}") } else { key };
            *count += 1;
            ElementId::Name(format!("file-menu-{key}").into())
        })
        .collect()
}

/// `items` 里和原来第 `near` 项 `old` 是同一项的那一项：整行派发的动作一样，其中右边各按钮派发的动作也一样、
/// 字也一样的优先。只比整行的动作不够：两个名字不同的自定义命令整行跑的可能是同一条命令，删除、编辑按钮
/// 带着名字才分得开。一样像的有几项时取离第 `near` 项最近的。
fn find_item(items: &[Option<MenuItem>], near: usize, old: &MenuItem) -> Option<usize> {
    let action = old.action.as_deref()?;
    let same_buttons = |item: &MenuItem| {
        item.buttons.len() == old.buttons.len()
            && item.buttons.iter().zip(&old.buttons).all(|((_, a), (_, b))| a.partial_eq(b.as_ref()))
    };
    items
        .iter()
        .enumerate()
        .filter_map(|(ix, item)| Some((ix, item.as_ref()?)))
        .filter(|(_, item)| item.action.as_deref().is_some_and(|a| a.partial_eq(action)))
        .min_by_key(|(ix, item)| (!same_buttons(item), item.label != old.label, ix.abs_diff(near)))
        .map(|(ix, _)| ix)
}

/// 开菜单的视图：菜单的状态存在它身上（`menu_slot`），这里给它开、关、画菜单的写法。
pub trait MenuHost: Sized + 'static {
    /// 开着的菜单存在哪。
    fn menu_slot(&mut self) -> &mut Option<Menu>;

    /// 开着的菜单。
    fn menu(&self) -> Option<&Menu>;

    /// 在 `position` 弹出 `items`，点了的那项先把焦点交给 `target` 再派发。
    fn open_context_menu(
        &mut self,
        position: Point<Pixels>,
        items: Vec<Option<MenuItem>>,
        target: FocusHandle,
        cx: &mut Context<Self>,
    ) {
        *self.menu_slot() = Some(Menu::new(Some(position), items, target, cx));
        cx.notify();
    }

    /// 弹出挂在按钮下面的菜单，按钮用 `render_dropdown_menu` 把它画在自己下面。给了 `select_after` 时
    /// 先选中它后面第一个能选的项（后面没有就绕回来），从键盘打开时回车就能用。
    fn open_dropdown(
        &mut self,
        items: Vec<Option<MenuItem>>,
        target: FocusHandle,
        select_after: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let mut menu = Menu::new(None, items, target, cx);
        if let Some(ix) = select_after {
            menu.highlighted = Some(ix);
            menu.move_highlight(true);
        }
        *self.menu_slot() = Some(menu);
        cx.notify();
    }

    /// 刚打开的菜单拿到焦点。每次画窗口时调。
    fn focus_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.menu_slot()
            && std::mem::take(&mut menu.focus_pending)
        {
            window.focus(&menu.focus, cx);
        }
    }

    /// 关掉菜单；焦点还在菜单上时还给打开它之前的地方。
    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = self.menu_slot().take() {
            if menu.focus.is_focused(window) {
                window.focus(&menu.target, cx);
            }
            cx.notify();
        }
    }

    /// 关掉菜单，焦点给 `target`，再派发 `action`：和按快捷键走同一条路。
    /// 换掉开着的菜单的各项，选中的位置尽量不变。
    fn replace_menu_items(&mut self, items: Vec<Option<MenuItem>>, cx: &mut Context<Self>) {
        if let Some(menu) = self.menu_slot() {
            let old_items = std::mem::replace(&mut menu.items, items);
            // 选中的那一项还在时跟着它走：前面多了、少了几项（比如后台重列了命令）时行号变了，留着行号回车会
            // 按到别的项上。
            let old = menu.highlighted.and_then(|ix| Some((ix, old_items.get(ix)?.as_ref()?)));
            if let Some(ix) = old.and_then(|(ix, old)| find_item(&menu.items, ix, old)) {
                menu.highlighted = Some(ix);
                menu.scroll.scroll_to_item(ix);
            }
            // 选中的那一项没了（删掉了一条）时，选中它后面能选的那一项，删的是最后一项时选新的最后一项。
            if let Some(ix) = menu.highlighted {
                menu.highlighted = highlight_after_replace(&menu.items, ix);
                if let Some(ix) = menu.highlighted {
                    menu.scroll.scroll_to_item(ix);
                }
            }
            cx.notify();
        }
    }

    /// 点了或回车第 `ix` 项：`keep_open` 的直接派发，菜单和焦点都不动；别的先关掉菜单。
    fn activate_menu_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu_slot() else {
            return;
        };
        // `ix` 来自上一帧画的闭包，菜单之后经 `replace_menu_items` 换成更短的列表时它可能已越界。
        if ix >= menu.items.len() {
            return;
        }
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
        // `ix` 可能是菜单换成更短列表之前的行号，取不到就不按。
        let button = self.menu().and_then(|menu| menu.items.get(ix)?.as_ref()?.buttons.get(button));
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
        let Some(menu) = self.menu_slot().take() else {
            return;
        };
        window.focus(&menu.target, cx);
        window.dispatch_action(action.boxed_clone(), cx);
        cx.notify();
    }

    fn menu_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu_slot() else {
            return;
        };
        let modifiers = event.keystroke.modifiers;
        if modifiers.platform && !modifiers.shift && !modifiers.alt && !modifiers.control {
            let key = event.keystroke.key.as_str();
            let pressed = menu.highlighted.and_then(|ix| {
                let buttons = &menu.items.get(ix)?.as_ref()?.buttons;
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

    /// 挂在按钮下面的菜单开着。
    fn dropdown_open(&self) -> bool {
        self.menu().is_some_and(|menu| menu.position.is_none())
    }

    /// 右键菜单，盖在窗口最上层，左上角对着右键按下的位置。
    fn render_context_menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let position = self.menu()?.position?;
        let max_height = f32::from(window.viewport_size().height) - MENU_MARGIN * 2.;
        let list = self.render_menu_list(max_height, window, cx)?;
        Some(
            deferred(anchored().position(position).snap_to_window_with_margin(px(MENU_MARGIN)).child(list))
                .with_priority(1)
                .into_any_element(),
        )
    }

    /// 挂在按钮下面的菜单，按钮把它作为子元素：右上角对着按钮的右下角，往下让出一点；放不下时
    /// 贴着窗口边挪进来。按钮下沿离窗口顶 `button_bottom`，菜单最高到窗口底边。
    fn render_dropdown_menu(&self, button_bottom: f32, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if !self.dropdown_open() {
            return None;
        }
        let max_height = f32::from(window.viewport_size().height) - button_bottom - DROPDOWN_GAP - MENU_MARGIN;
        let list = self.render_menu_list(max_height, window, cx)?;
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
    fn render_menu_list(&self, max_height: f32, window: &Window, cx: &mut Context<Self>) -> Option<Popover> {
        let menu = self.menu()?;
        // 辅助工具悬停不了，行里的按钮在它读着时一直露出来，不然按不到。
        let a11y = window.is_a11y_active();
        let theme = cx.theme();
        let hover_bg = theme.mix(0.12);
        // 选中的行底色已经是 `hover_bg`，行里按钮悬停时再深一些。
        let button_hover_bg = theme.mix(0.22);
        let fg = theme.popover_foreground;
        // 有勾选项时每一行左边都留出打勾的那一列，字对齐。
        let check_column = menu.items.iter().flatten().any(|item| item.checked.is_some());
        let ids = row_ids(&menu.items);
        let items = menu.items.iter().enumerate().map(|(ix, item)| {
            let Some(item) = item else {
                return div().flex_none().h(px(1.)).mx(px(6.)).my(px(4.)).bg(fg.opacity(0.12)).into_any_element();
            };
            let row_action = item.enabled && item.action.is_some();
            let highlighted = menu.highlighted == Some(ix);
            // 整行能点的只在选中时露出按钮，不然每行行尾都摆一个；整行没有动作的（灰着的小标题）一直露出，
            // 有动作却不可用的不露。
            let disabled = item.action.is_some() && !item.enabled;
            let buttons = (!disabled && (!row_action || highlighted || a11y)).then_some(&item.buttons);
            let view = cx.entity().downgrade();
            div()
                .id(ids[ix].clone())
                // 没有动作的一行是小标题；灰着的项报成不可用。
                .map(|row| match item.checked {
                    _ if item.action.is_none() => row.role(Role::Label),
                    Some(checked) => row.role(Role::MenuItemCheckBox).aria_toggled(checked.into()),
                    None => row.role(Role::MenuItem),
                })
                .aria_label(item.label.clone())
                .aria_disabled(disabled)
                .when_some(item.shortcut.clone(), |row, shortcut| row.aria_description(shortcut))
                // 菜单拿着焦点，选中的那一项报成辅助工具眼里的焦点。
                .when(highlighted, |row| row.aria_active_descendant())
                .flex_none()
                .h(scaled(24.))
                .px(px(10.))
                .mx(px(4.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .gap(px(16.))
                .text_color(if item.enabled { fg } else { fg.opacity(0.35) })
                .when(highlighted, |row| row.bg(hover_bg))
                .when_some(item.tooltip.clone(), |row, text| row.tooltip(tooltip(text, None)))
                .when(row_action, |row| {
                    row.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        // 上一帧的 `ix` 在菜单换成更短的列表后可能已越界，越界的不选。
                        let menu = this.menu_slot().as_mut().filter(|menu| *hovered && ix < menu.items.len());
                        if let Some(menu) = menu {
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
                    // 只有按下的处理时辅助工具按不到，另外登记。
                    .on_a11y_press(view, move |this, window, cx| this.activate_menu_item(ix, window, cx))
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
                            let view = cx.entity().downgrade();
                            // 菜单项高亮着时底色已经亮了一档，悬停再亮一些。
                            Button::new(("file-menu-button", b), button.tooltip.clone())
                                .icon(button.icon)
                                .icon_size(12.)
                                .with_size(ButtonSize::IconSm)
                                .size(px(18.))
                                .hover(|button| button.bg(button_hover_bg))
                                .tooltip(tooltip(button.tooltip.clone(), None))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.press_menu_button(ix, b, window, cx);
                                    }),
                                )
                                .on_a11y_press(view, move |this, window, cx| this.press_menu_button(ix, b, window, cx))
                        },
                    ))
                }))
                .into_any_element()
        });
        let list = Popover::new("file-menu")
            .role(Role::Menu)
            .track_focus(&menu.focus)
            .on_key_down(cx.listener(Self::menu_key))
            .track_scroll(&menu.scroll)
            .w(scaled(MENU_WIDTH))
            .max_h(px(max_height))
            .overflow_y_scroll()
            .py(px(4.))
            .flex()
            .flex_col()
            .rounded(px(6.))
            .shadow_lg()
            .children(items)
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_menu(window, cx)));
        Some(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    gpui::actions!(menu_test, [First, Second, Third]);

    fn item(action: Box<dyn Action>) -> Option<MenuItem> {
        Some(MenuItem::label(String::new(), None, Some(action)))
    }

    /// 换上的各项里找原来选中的那一项：按派发的动作找，不按行号；前面多了一项也找得到，没了时找不到，
    /// 有几项一样时取离原来的位置最近的。
    #[test]
    fn finds_the_highlighted_item_by_action() {
        let items = [None, item(Box::new(Third)), item(Box::new(First)), item(Box::new(Second))];
        let old = |action: Box<dyn Action>| item(action).unwrap();
        assert_eq!(find_item(&items, 1, &old(Box::new(First))), Some(2));
        assert_eq!(find_item(&items, 3, &old(Box::new(Second))), Some(3));
        let without = [item(Box::new(First)), item(Box::new(Third))];
        assert_eq!(find_item(&without, 1, &old(Box::new(Second))), None);
        let twice = [item(Box::new(First)), item(Box::new(Second)), item(Box::new(Third)), item(Box::new(First))];
        assert_eq!(find_item(&twice, 2, &old(Box::new(First))), Some(3));
        assert_eq!(find_item(&twice, 1, &old(Box::new(First))), Some(0));
    }

    /// 整行动作一样、按钮的动作或字不一样的两项（跑同一条命令的两个自定义命令）分得开：上面多了一行、
    /// 原来那项挪下去时，不选到挪到它原来位置上的另一项。字改了也先认按钮；按钮也对不上时只比整行的动作。
    #[test]
    fn tells_apart_items_with_the_same_action() {
        let button = || MenuButton { icon: "", tooltip: SharedString::default(), keep_open: true, cmd_key: None };
        let task = |name: &str, delete: Box<dyn Action>| {
            Some(MenuItem::label(name.to_owned(), None, Some(Box::new(First))).with_button(button(), delete))
        };
        let old = task("b", Box::new(Third)).unwrap();
        // 原来是 [a, b]，选中第 1 项 b；重列后上面多了一行，b 挪到第 2 项，a 到了第 1 项。
        let items = [item(Box::new(Second)), task("a", Box::new(Second)), task("b", Box::new(Third))];
        assert_eq!(find_item(&items, 1, &old), Some(2));
        let renamed = [item(Box::new(Second)), task("a", Box::new(Second)), task("c", Box::new(Third))];
        assert_eq!(find_item(&renamed, 1, &old), Some(2));
        let neither = [item(Box::new(Second)), task("a", Box::new(Second)), task("c", Box::new(Second))];
        assert_eq!(find_item(&neither, 1, &old), Some(1));
    }

    /// 删掉了选中的一项后改选的项：删的是最后一项（`ix` 越界）时选新的最后一项，它不能选时往上找；
    /// `ix` 还在、那一行不能选时往后找；空了不选。
    #[test]
    fn highlight_after_replace_stays_near_the_removed_item() {
        let items = [item(Box::new(First)), None, item(Box::new(Second)), item(Box::new(Third))];
        assert_eq!(highlight_after_replace(&items, 4), Some(3));
        let disabled = Some(MenuItem::label(String::new(), None, None));
        let last_disabled = [item(Box::new(First)), item(Box::new(Second)), disabled, None];
        assert_eq!(highlight_after_replace(&last_disabled, 4), Some(1));
        assert_eq!(highlight_after_replace(&items, 1), Some(2));
        assert_eq!(highlight_after_replace(&[], 0), None);
    }

    /// 菜单行的 id 按所在的组和字取：不同组里同名的项不撞，前面插进一组后原来各项的 id 不变，同一组里
    /// 同名的两项也不撞。
    #[test]
    fn row_ids_follow_section_and_label() {
        let group = |name: &str| Some(MenuItem::group(name.to_owned(), None, false, Box::new(First)));
        let task = |name: &str| Some(MenuItem::label(name.to_owned(), None, Some(Box::new(Second))));
        let items = [task("add"), None, group("Makefile"), task("build"), None, group("package.json"), task("build")];
        let ids = row_ids(&items);
        assert_ne!(ids[3], ids[6]);
        let more = [
            task("add"),
            None,
            group("mine"),
            task("build"),
            None,
            group("Makefile"),
            task("build"),
            None,
            group("package.json"),
            task("build"),
        ];
        let moved = row_ids(&more);
        assert_eq!((&moved[5], &moved[6], &moved[9]), (&ids[2], &ids[3], &ids[6]));
        let twice = row_ids(&[group("mine"), task("build"), task("build")]);
        assert_ne!(twice[1], twice[2]);
    }
}
