//! 右键点终端弹出的菜单：拷贝、粘贴、全选、查找、清屏和分屏，派发给终端自己，分屏一路冒泡到窗口。

use gpui::{Action, Context, Pixels, Point, Window};

use super::{ClearScreen, StartSearch, TerminalView};
use crate::{
    i18n::{os_key, tr},
    ui::{
        actions::{Copy, Paste, SelectAll},
        menu::{Menu, MenuHost, MenuItem},
        tooltip::shortcut_text_in,
    },
    window::{NewSplitDown, NewSplitRight},
};

impl MenuHost for TerminalView {
    fn menu_slot(&mut self) -> &mut Option<Menu> {
        &mut self.menu
    }

    fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }
}

impl TerminalView {
    /// 在窗口坐标 `position` 弹出右键菜单；没有选区时拷贝不可用。快捷键写在终端里按的那个：拷贝、粘贴、全选
    /// 在终端外另绑着别的键。
    pub(super) fn open_terminal_menu(&mut self, position: Point<Pixels>, window: &Window, cx: &mut Context<Self>) {
        let selected = self.screen.shown().is_some_and(|session| session.has_selection());
        let focus = self.focus_handle.clone();
        let item = |label: String, action: Box<dyn Action>, enabled: bool| {
            let shortcut = shortcut_text_in(action.as_ref(), &focus, window);
            Some(MenuItem::new(label, action, enabled, cx).with_shortcut(shortcut))
        };
        let items = vec![
            item(tr(&os_key("copy")), Box::new(Copy), selected),
            item(tr("menu.paste"), Box::new(Paste), true),
            item(tr("menu.select_all"), Box::new(SelectAll), true),
            None,
            item(tr("menu.find"), Box::new(StartSearch), true),
            item(tr("menu.clear_screen"), Box::new(ClearScreen), true),
            None,
            item(tr("menu.split_right"), Box::new(NewSplitRight), true),
            item(tr("menu.split_down"), Box::new(NewSplitDown), true),
        ];
        self.open_context_menu(position, items, focus, cx);
    }
}
