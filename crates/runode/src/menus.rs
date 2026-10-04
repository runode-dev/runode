//! macOS 菜单栏、快捷键，以及不属于某个终端视图的应用级动作。
//!
//! 菜单项上显示的快捷键由 GPUI 从键位绑定里反查，所以每个动作只在
//! `bind_keys` 里绑一次，菜单和键盘自动保持一致。

use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, actions};

use crate::{
    terminal_view::{Copy, DecreaseFontSize, IncreaseFontSize, Paste, ResetFontSize},
    workspace::{CloseTab, NewTab, NextTab, PreviousTab, SelectLastTab, SelectTab},
};

actions!(
    runode,
    [
        About,
        ReloadConfiguration,
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        NewWindow,
        CloseWindow,
        Minimize,
        Zoom,
        ToggleFullScreen,
    ]
);

pub fn install(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-shift-,", ReloadConfiguration, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-n", NewWindow, None),
        KeyBinding::new("cmd-shift-w", CloseWindow, None),
        KeyBinding::new("cmd-t", NewTab, Some("Workspace")),
        KeyBinding::new("cmd-w", CloseTab, Some("Workspace")),
        KeyBinding::new("cmd-}", NextTab, Some("Workspace")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Workspace")),
        KeyBinding::new("cmd-{", PreviousTab, Some("Workspace")),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("Workspace")),
        KeyBinding::new("cmd-9", SelectLastTab, Some("Workspace")),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("cmd-c", Copy, Some("Terminal")),
        KeyBinding::new("cmd-v", Paste, Some("Terminal")),
        KeyBinding::new("cmd-=", IncreaseFontSize, Some("Terminal")),
        KeyBinding::new("cmd-+", IncreaseFontSize, Some("Terminal")),
        KeyBinding::new("cmd--", DecreaseFontSize, Some("Terminal")),
        KeyBinding::new("cmd-0", ResetFontSize, Some("Terminal")),
    ]);
    // Cmd-1 到 Cmd-8 切到对应标签，Cmd-9 是最后一个。
    cx.bind_keys((1..=8).map(|n| KeyBinding::new(&format!("cmd-{n}"), SelectTab(n - 1), Some("Workspace"))));

    cx.on_action(|_: &About, _| crate::about::show());
    cx.on_action(|_: &ReloadConfiguration, cx| crate::config::reload(cx));
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &NewWindow, cx| crate::open_window(cx));
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, |w| w.remove_window()));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |w| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |w| w.zoom_window()));
    cx.on_action(|_: &ToggleFullScreen, cx| with_active_window(cx, |w| w.toggle_fullscreen()));

    cx.set_menus([
        // macOS 总把第一个菜单当作应用菜单，标题显示为应用名。
        Menu::new("runode").items([
            MenuItem::action("About runode", About),
            MenuItem::separator(),
            MenuItem::action("Reload Configuration", ReloadConfiguration),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide runode", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit runode", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Tab", NewTab),
            MenuItem::action("New Window", NewWindow),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        // 用 os_action 挂到系统的复制/粘贴选择器上，菜单栏的 Edit 才会被 macOS 识别，
        // 系统也会自动补上「听写」「表情与符号」等项。
        Menu::new("Edit").items([
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
        ]),
        Menu::new("View").items([
            MenuItem::action("Increase Font Size", IncreaseFontSize),
            MenuItem::action("Decrease Font Size", DecreaseFontSize),
            MenuItem::action("Reset Font Size", ResetFontSize),
            MenuItem::separator(),
            MenuItem::action("Toggle Full Screen", ToggleFullScreen),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Show Previous Tab", PreviousTab),
            MenuItem::action("Show Next Tab", NextTab),
        ]),
    ]);
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut gpui::Window)) {
    if let Some(window) = cx.active_window() {
        window.update(cx, |_, window, _| f(window)).ok();
    }
}
