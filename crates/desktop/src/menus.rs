//! macOS 菜单栏、快捷键，以及不属于某个终端视图的应用级动作。
//!
//! 菜单项上显示的快捷键由 GPUI 从键位绑定里反查，快捷键只在 `keybinds` 里绑，
//! 菜单和键盘自动保持一致。

use gpui::{App, Menu, MenuItem, OsAction, SystemMenuType, actions};

use crate::{
    search_bar::{Cut, Redo, SearchNext, SearchPrevious, SearchSelection, StartSearch, Undo},
    terminal_view::{
        ClearScreen, Copy, DecreaseFontSize, IncreaseFontSize, JumpToPrompt, Paste, PasteSelection,
        ResetFontSize, SelectAll,
    },
    workspace::{
        ClosePane, CloseTab, CloseWorkspace, EqualizePanes, FocusNextPane, FocusPreviousPane, GotoAgent, NewSplitDown,
        NewSplitRight, NewTab, NewWorkspace, NextAgent, NextTab, NextWorkspace, PreviousTab, PreviousWorkspace,
        RenameWorkspace, ToggleChanges, ToggleFiles, TogglePaneZoom, ToggleSidebar,
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
    cx.on_action(|_: &About, _| crate::about::show());
    cx.on_action(|_: &OpenConfiguration, cx| crate::config::open(cx));
    cx.on_action(|_: &ReloadConfiguration, cx| crate::config::reload(cx));
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &NewWindow, cx| crate::window::open_window(cx, None));
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, |w| w.remove_window()));
    cx.on_action(|_: &CloseAllWindows, cx| {
        for window in cx.windows() {
            window.update(cx, |_, window, _| window.remove_window()).ok();
        }
    });
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |w| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |w| w.zoom_window()));
    cx.on_action(|_: &ToggleFullScreen, cx| with_active_window(cx, |w| w.toggle_fullscreen()));

    // 装快捷键时会顺带设置菜单。
    crate::keybinds::install(cx);
}

fn tr(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}

/// 设置菜单栏。菜单项上的快捷键在这时从键位表里查，换了绑定要重新调用。
pub fn set_menus(cx: &mut App) {
    cx.set_menus([
        // macOS 总把第一个菜单当作应用菜单，标题显示为应用名。
        Menu::new("Runode").items([
            MenuItem::action(tr("menu.about"), About),
            MenuItem::separator(),
            MenuItem::action(tr("menu.open_config"), OpenConfiguration),
            MenuItem::action(tr("menu.reload_config"), ReloadConfiguration),
            MenuItem::separator(),
            MenuItem::os_submenu(tr("menu.services"), SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(tr("menu.hide"), Hide),
            MenuItem::action(tr("menu.hide_others"), HideOthers),
            MenuItem::action(tr("menu.show_all"), ShowAll),
            MenuItem::separator(),
            MenuItem::action(tr("menu.quit"), Quit),
        ]),
        Menu::new(tr("menu.file")).items([
            MenuItem::action(tr("menu.new_tab"), NewTab),
            MenuItem::action(tr("menu.new_workspace"), NewWorkspace),
            MenuItem::action(tr("menu.new_window"), NewWindow),
            MenuItem::separator(),
            MenuItem::action(tr("menu.split_right"), NewSplitRight),
            MenuItem::action(tr("menu.split_down"), NewSplitDown),
            MenuItem::separator(),
            MenuItem::action(tr("menu.close"), ClosePane),
            MenuItem::action(tr("menu.close_tab"), CloseTab),
            MenuItem::action(tr("menu.close_workspace"), CloseWorkspace),
            MenuItem::action(tr("menu.close_window"), CloseWindow),
            MenuItem::action(tr("menu.close_all_windows"), CloseAllWindows),
        ]),
        // 用 os_action 挂到系统的复制/粘贴选择器上，菜单栏的 Edit 才会被 macOS 识别，
        // 系统也会自动补上「听写」「表情与符号」等项。
        Menu::new(tr("menu.edit")).items([
            MenuItem::os_action(tr("menu.undo"), Undo, OsAction::Undo),
            MenuItem::os_action(tr("menu.redo"), Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action(tr("menu.cut"), Cut, OsAction::Cut),
            MenuItem::os_action(tr("menu.copy"), Copy, OsAction::Copy),
            MenuItem::os_action(tr("menu.paste"), Paste, OsAction::Paste),
            MenuItem::action(tr("menu.paste_selection"), PasteSelection),
            MenuItem::os_action(tr("menu.select_all"), SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action(tr("menu.find"), StartSearch),
            MenuItem::action(tr("menu.find_next"), SearchNext),
            MenuItem::action(tr("menu.find_previous"), SearchPrevious),
            MenuItem::action(tr("menu.find_selection"), SearchSelection),
            MenuItem::separator(),
            MenuItem::action(tr("menu.clear_screen"), ClearScreen),
        ]),
        Menu::new(tr("menu.view")).items([
            MenuItem::action(tr("menu.increase_font_size"), IncreaseFontSize),
            MenuItem::action(tr("menu.decrease_font_size"), DecreaseFontSize),
            MenuItem::action(tr("menu.reset_font_size"), ResetFontSize),
            MenuItem::separator(),
            MenuItem::action(tr("menu.previous_prompt"), JumpToPrompt(-1)),
            MenuItem::action(tr("menu.next_prompt"), JumpToPrompt(1)),
            MenuItem::separator(),
            MenuItem::action(tr("menu.toggle_sidebar"), ToggleSidebar),
            MenuItem::action(tr("menu.toggle_changes"), ToggleChanges),
            MenuItem::action(tr("menu.toggle_files"), ToggleFiles),
            MenuItem::action(tr("menu.toggle_full_screen"), ToggleFullScreen),
        ]),
        Menu::new(tr("menu.window")).items([
            MenuItem::action(tr("menu.minimize"), Minimize),
            MenuItem::action(tr("menu.zoom"), Zoom),
            MenuItem::separator(),
            MenuItem::action(tr("menu.previous_tab"), PreviousTab),
            MenuItem::action(tr("menu.next_tab"), NextTab),
            MenuItem::separator(),
            MenuItem::action(tr("menu.previous_workspace"), PreviousWorkspace),
            MenuItem::action(tr("menu.next_workspace"), NextWorkspace),
            MenuItem::action(tr("menu.rename_workspace"), RenameWorkspace),
            MenuItem::separator(),
            MenuItem::action(tr("menu.previous_split"), FocusPreviousPane),
            MenuItem::action(tr("menu.next_split"), FocusNextPane),
            MenuItem::action(tr("menu.zoom_split"), TogglePaneZoom),
            MenuItem::action(tr("menu.equalize_splits"), EqualizePanes),
            MenuItem::separator(),
            MenuItem::action(tr("menu.goto_agent"), GotoAgent),
            MenuItem::action(tr("menu.next_agent"), NextAgent),
        ]),
    ]);
    #[cfg(target_os = "macos")]
    fix_native_menus(4);
}

/// 补上 GPUI 设置菜单时漏掉的两件事。`windows_menu` 是窗口菜单的位置。
#[cfg(target_os = "macos")]
fn fix_native_menus(windows_menu: isize) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(bar) = app.mainMenu() else {
        return;
    };
    // GPUI 只在菜单名恰好是 "Window" 时把它设成系统的窗口菜单（系统会往里加窗口列表），
    // 翻译过的标题它认不出来，按位置补上：应用、File、Edit、View 之后的第五个。
    if let Some(menu) = bar.itemAtIndex(windows_menu).and_then(|item| item.submenu()) {
        app.setWindowsMenu(Some(&menu));
    }
    fix_key_equivalents(&bar);
}

/// GPUI 把回车和 Tab 的键名原样当成菜单快捷键，系统只认第一个字符，⇧⌘↩ 就成了 ⇧⌘E，
/// 按 ⇧⌘E 真会触发菜单项。这里换成系统的按键字符。
#[cfg(target_os = "macos")]
fn fix_key_equivalents(menu: &objc2_app_kit::NSMenu) {
    use objc2_foundation::NSString;

    for item in menu.itemArray().iter() {
        let native = match item.keyEquivalent().to_string().as_str() {
            "enter" => Some("\r"),
            "tab" => Some("\t"),
            _ => None,
        };
        if let Some(native) = native {
            item.setKeyEquivalent(&NSString::from_str(native));
        }
        if let Some(submenu) = item.submenu() {
            fix_key_equivalents(&submenu);
        }
    }
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut gpui::Window)) {
    if let Some(window) = cx.active_window() {
        window.update(cx, |_, window, _| f(window)).ok();
    }
}
