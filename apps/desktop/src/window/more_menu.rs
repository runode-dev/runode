//! 没有菜单栏的系统（Windows、Linux）上标题栏右上角的「更多」按钮：按下弹出菜单，放 macOS 只在菜单栏的
//! 应用菜单和文件菜单里才有入口的几条命令（新窗口、设置、配置文件、装 skill、检查更新、关于、退出）。

use gpui::{Action, App, Context, Focusable as _, MouseButton, Window, prelude::*, px};

use super::{
    Dropdown, InstallSkill, TITLEBAR_HEIGHT, WindowView,
    project::{TOGGLE_HEIGHT, TOGGLE_WIDTH},
};
use crate::{
    i18n::tr,
    menus::{
        About, CheckForUpdates, NewWindow, OpenConfiguration, OpenSettings, Quit, QuitAndEndSessions,
        ReloadConfiguration,
    },
    ui::{
        a11y::A11yPress as _,
        button::Toggle,
        icons::MORE_ICON,
        menu::{MenuHost as _, MenuItem},
        tooltip::tooltip,
    },
};

/// 这个系统上标题栏有没有「更多」按钮：macOS 的这些命令在菜单栏里。
pub(super) const SHOWN: bool = !cfg!(target_os = "macos");

/// 菜单的各项。「退出并结束所有会话」和「检查更新」那一项的字跟菜单栏一样，打开菜单时才定。
fn items(cx: &App) -> Vec<Option<MenuItem>> {
    let item = |label: String, action: Box<dyn Action>| Some(MenuItem::new(label, action, true, cx));
    let mut items = vec![
        item(tr("menu.new_window"), Box::new(NewWindow)),
        None,
        item(tr("menu.settings"), Box::new(OpenSettings)),
        item(tr("menu.open_config"), Box::new(OpenConfiguration)),
        item(tr("menu.reload_config"), Box::new(ReloadConfiguration)),
        item(tr("menu.install_skill"), Box::new(InstallSkill)),
        None,
        item(crate::update::menu_label(cx), Box::new(CheckForUpdates)),
        item(tr("menu.about"), Box::new(About)),
        None,
        item(tr("menu.quit"), Box::new(Quit)),
    ];
    if crate::window::end_sessions_in_menu() {
        items.push(item(tr("menu.quit_end_sessions"), Box::new(QuitAndEndSessions)));
    }
    items
}

impl WindowView {
    /// 开着的下拉菜单是「更多」按钮的。
    fn more_menu_open(&self) -> bool {
        self.dropdown == Dropdown::More && self.dropdown_open()
    }

    /// 在「更多」按钮下面弹出菜单；从辅助工具按下时选中第一项。
    fn open_more_menu(&mut self, select_first: bool, cx: &mut Context<Self>) {
        let items = items(cx);
        let target = self.focus_handle(cx);
        // `open_dropdown` 选中给的那一项之后能选的第一项，从最后一项往后绕回第一项。
        let select_after = select_first.then(|| items.len() - 1);
        self.open_dropdown(items, target, select_after, cx);
        self.dropdown = Dropdown::More;
    }

    /// 标题栏右上角、命令按钮左边的「更多」按钮，菜单挂在它下面，开着时底色亮一些。
    pub(super) fn render_more_button(&self, window: &Window, cx: &mut Context<Self>) -> Toggle {
        let view = cx.entity().downgrade();
        let open = self.more_menu_open();
        let label = tr("tooltip.more");
        Toggle::new("more", MORE_ICON, label.clone())
            .pressed(open)
            .aria_expanded(open)
            .w(px(TOGGLE_WIDTH))
            .h(px(TOGGLE_HEIGHT))
            .tooltip(tooltip(label, None))
            // 菜单开着时在捕获阶段就关掉、不再往下传：菜单自己的「点到外面就关」和下面再打开的都不跑。
            .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                if this.more_menu_open() {
                    cx.stop_propagation();
                    this.close_menu(window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_more_menu(false, cx);
                }),
            )
            // 按下鼠标就开，没有 on_click，辅助工具的按下另外登记：开时选中第一项，开着时收起。
            .on_a11y_press(view, |this, window, cx| {
                if this.more_menu_open() {
                    this.close_menu(window, cx);
                } else {
                    this.open_more_menu(true, cx);
                }
            })
            .when(open, |button| button.children(self.render_dropdown_menu(TITLEBAR_HEIGHT, window, cx)))
    }
}
