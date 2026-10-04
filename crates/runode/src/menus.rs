//! macOS 菜单栏、快捷键，以及不属于某个终端视图的应用级动作。
//!
//! 菜单项上显示的快捷键由 GPUI 从键位绑定里反查，所以每个动作只在
//! `bind_keys` 里绑一次，菜单和键盘自动保持一致。

use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, actions};

use crate::{
    search_bar::{Cut, EndSearch, Redo, SearchNext, SearchPrevious, SearchSelection, StartSearch, Undo},
    terminal_view::{
        ClearScreen, Copy, DecreaseFontSize, IncreaseFontSize, Paste, PasteSelection,
        ResetFontSize, ScreenFile, ScrollPageDown, ScrollPageUp, ScrollToBottom, ScrollToSelection,
        ScrollToTop, SelectAll, SendText, WriteScreenFile, JumpToPrompt,
    },
    pane::Direction,
    workspace::{
        ClosePane, CloseTab, EqualizePanes, FocusNextPane, FocusPane, FocusPreviousPane, NewSplitDown,
        NewSplitRight, NewTab, NextTab, PreviousTab, ResizePane, SelectLastTab, SelectTab,
        TogglePaneZoom,
    },
};

actions!(
    runode,
    [
        About,
        OpenConfiguration,
        ReloadConfiguration,
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        NewWindow,
        CloseWindow,
        CloseAllWindows,
        Minimize,
        Zoom,
        ToggleFullScreen,
    ]
);

pub fn install(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", OpenConfiguration, None),
        KeyBinding::new("cmd-shift-,", ReloadConfiguration, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-n", NewWindow, None),
        KeyBinding::new("cmd-shift-w", CloseWindow, None),
        KeyBinding::new("cmd-shift-alt-w", CloseAllWindows, None),
        KeyBinding::new("cmd-t", NewTab, Some("Workspace")),
        KeyBinding::new("cmd-w", ClosePane, Some("Workspace")),
        KeyBinding::new("cmd-alt-w", CloseTab, Some("Workspace")),
        KeyBinding::new("cmd-d", NewSplitRight, Some("Workspace")),
        KeyBinding::new("cmd-shift-d", NewSplitDown, Some("Workspace")),
        KeyBinding::new("cmd-[", FocusPreviousPane, Some("Workspace")),
        KeyBinding::new("cmd-]", FocusNextPane, Some("Workspace")),
        KeyBinding::new("cmd-alt-left", FocusPane(Direction::Left), Some("Workspace")),
        KeyBinding::new("cmd-alt-right", FocusPane(Direction::Right), Some("Workspace")),
        KeyBinding::new("cmd-alt-up", FocusPane(Direction::Up), Some("Workspace")),
        KeyBinding::new("cmd-alt-down", FocusPane(Direction::Down), Some("Workspace")),
        KeyBinding::new("ctrl-cmd-left", ResizePane(Direction::Left), Some("Workspace")),
        KeyBinding::new("ctrl-cmd-right", ResizePane(Direction::Right), Some("Workspace")),
        KeyBinding::new("ctrl-cmd-up", ResizePane(Direction::Up), Some("Workspace")),
        KeyBinding::new("ctrl-cmd-down", ResizePane(Direction::Down), Some("Workspace")),
        KeyBinding::new("ctrl-cmd-=", EqualizePanes, Some("Workspace")),
        KeyBinding::new("cmd-shift-enter", TogglePaneZoom, Some("Workspace")),
        KeyBinding::new("cmd-}", NextTab, Some("Workspace")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Workspace")),
        KeyBinding::new("cmd-{", PreviousTab, Some("Workspace")),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("Workspace")),
        KeyBinding::new("cmd-9", SelectLastTab, Some("Workspace")),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("cmd-enter", ToggleFullScreen, None),
        KeyBinding::new("cmd-c", Copy, Some("Terminal")),
        KeyBinding::new("cmd-v", Paste, Some("Terminal")),
        KeyBinding::new("cmd-shift-v", PasteSelection, Some("Terminal")),
        KeyBinding::new("cmd-a", SelectAll, Some("Terminal")),
        KeyBinding::new("cmd-k", ClearScreen, Some("Terminal")),
        KeyBinding::new("cmd-home", ScrollToTop, Some("Terminal")),
        KeyBinding::new("cmd-end", ScrollToBottom, Some("Terminal")),
        KeyBinding::new("cmd-pageup", ScrollPageUp, Some("Terminal")),
        KeyBinding::new("cmd-pagedown", ScrollPageDown, Some("Terminal")),
        KeyBinding::new("cmd-j", ScrollToSelection, Some("Terminal")),
        KeyBinding::new("cmd-up", JumpToPrompt(-1), Some("Terminal")),
        KeyBinding::new("cmd-down", JumpToPrompt(1), Some("Terminal")),
        KeyBinding::new("cmd-shift-up", JumpToPrompt(-1), Some("Terminal")),
        KeyBinding::new("cmd-shift-down", JumpToPrompt(1), Some("Terminal")),
        // 行编辑：跳到行首、行尾，删到行首，按词左右移动。
        KeyBinding::new("cmd-left", SendText("\x01"), Some("Terminal")),
        KeyBinding::new("cmd-right", SendText("\x05"), Some("Terminal")),
        KeyBinding::new("cmd-backspace", SendText("\x15"), Some("Terminal")),
        KeyBinding::new("alt-left", SendText("\x1bb"), Some("Terminal")),
        KeyBinding::new("alt-right", SendText("\x1bf"), Some("Terminal")),
        // 搜索栏里回车找下一个，Esc 关掉；它不在 `Terminal` 上下文里，剪切复制粘贴、全选和撤销也要单独绑。
        KeyBinding::new("enter", SearchNext, Some("SearchBar")),
        KeyBinding::new("shift-enter", SearchPrevious, Some("SearchBar")),
        KeyBinding::new("escape", EndSearch, Some("SearchBar")),
        KeyBinding::new("cmd-g", SearchNext, Some("SearchBar")),
        KeyBinding::new("cmd-shift-g", SearchPrevious, Some("SearchBar")),
        KeyBinding::new("cmd-shift-f", EndSearch, Some("SearchBar")),
        KeyBinding::new("cmd-c", Copy, Some("SearchBar")),
        KeyBinding::new("cmd-v", Paste, Some("SearchBar")),
        KeyBinding::new("cmd-a", SelectAll, Some("SearchBar")),
        KeyBinding::new("cmd-x", Cut, Some("SearchBar")),
        KeyBinding::new("cmd-z", Undo, Some("SearchBar")),
        KeyBinding::new("cmd-shift-z", Redo, Some("SearchBar")),
        KeyBinding::new("cmd-f", StartSearch, Some("Terminal")),
        KeyBinding::new("cmd-e", SearchSelection, Some("Terminal")),
        KeyBinding::new("cmd-g", SearchNext, Some("Terminal")),
        KeyBinding::new("cmd-shift-g", SearchPrevious, Some("Terminal")),
        KeyBinding::new("cmd-shift-f", EndSearch, Some("Terminal")),
        // 点回终端后搜索栏还开着，Esc 照样关掉它；没在搜索时 Esc 照常发给程序。
        KeyBinding::new("escape", EndSearch, Some("Terminal && searching")),
        KeyBinding::new("ctrl-shift-cmd-j", WriteScreenFile(ScreenFile::CopyPath), Some("Terminal")),
        KeyBinding::new("cmd-shift-j", WriteScreenFile(ScreenFile::PastePath), Some("Terminal")),
        KeyBinding::new("cmd-shift-alt-j", WriteScreenFile(ScreenFile::Open), Some("Terminal")),
        KeyBinding::new("cmd-=", IncreaseFontSize, Some("Terminal")),
        KeyBinding::new("cmd-+", IncreaseFontSize, Some("Terminal")),
        KeyBinding::new("cmd--", DecreaseFontSize, Some("Terminal")),
        KeyBinding::new("cmd-0", ResetFontSize, Some("Terminal")),
    ]);
    // Cmd-1 到 Cmd-8 切到对应标签，Cmd-9 是最后一个。
    cx.bind_keys((1..=8).map(|n| KeyBinding::new(&format!("cmd-{n}"), SelectTab(n - 1), Some("Workspace"))));

    cx.on_action(|_: &About, _| crate::about::show());
    cx.on_action(|_: &OpenConfiguration, cx| crate::config::open(cx));
    cx.on_action(|_: &ReloadConfiguration, cx| crate::config::reload(cx));
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &NewWindow, cx| crate::open_window(cx));
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, |w| w.remove_window()));
    cx.on_action(|_: &CloseAllWindows, cx| {
        for window in cx.windows() {
            window.update(cx, |_, window, _| window.remove_window()).ok();
        }
    });
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |w| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |w| w.zoom_window()));
    cx.on_action(|_: &ToggleFullScreen, cx| with_active_window(cx, |w| w.toggle_fullscreen()));

    cx.set_menus([
        // macOS 总把第一个菜单当作应用菜单，标题显示为应用名。
        Menu::new("Runode").items([
            MenuItem::action("About Runode", About),
            MenuItem::separator(),
            MenuItem::action("Open Configuration", OpenConfiguration),
            MenuItem::action("Reload Configuration", ReloadConfiguration),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Runode", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Runode", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Tab", NewTab),
            MenuItem::action("New Window", NewWindow),
            MenuItem::separator(),
            MenuItem::action("Split Right", NewSplitRight),
            MenuItem::action("Split Down", NewSplitDown),
            MenuItem::separator(),
            MenuItem::action("Close", ClosePane),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Close Window", CloseWindow),
            MenuItem::action("Close All Windows", CloseAllWindows),
        ]),
        // 用 os_action 挂到系统的复制/粘贴选择器上，菜单栏的 Edit 才会被 macOS 识别，
        // 系统也会自动补上「听写」「表情与符号」等项。
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::action("Paste Selection", PasteSelection),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find…", StartSearch),
            MenuItem::action("Find Next", SearchNext),
            MenuItem::action("Find Previous", SearchPrevious),
            MenuItem::action("Use Selection for Find", SearchSelection),
            MenuItem::separator(),
            MenuItem::action("Clear Screen", ClearScreen),
        ]),
        Menu::new("View").items([
            MenuItem::action("Increase Font Size", IncreaseFontSize),
            MenuItem::action("Decrease Font Size", DecreaseFontSize),
            MenuItem::action("Reset Font Size", ResetFontSize),
            MenuItem::separator(),
            MenuItem::action("Jump to Previous Prompt", JumpToPrompt(-1)),
            MenuItem::action("Jump to Next Prompt", JumpToPrompt(1)),
            MenuItem::separator(),
            MenuItem::action("Toggle Full Screen", ToggleFullScreen),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Show Previous Tab", PreviousTab),
            MenuItem::action("Show Next Tab", NextTab),
            MenuItem::separator(),
            MenuItem::action("Select Previous Split", FocusPreviousPane),
            MenuItem::action("Select Next Split", FocusNextPane),
            MenuItem::action("Zoom Split", TogglePaneZoom),
            MenuItem::action("Equalize Splits", EqualizePanes),
        ]),
    ]);
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut gpui::Window)) {
    if let Some(window) = cx.active_window() {
        window.update(cx, |_, window, _| f(window)).ok();
    }
}
