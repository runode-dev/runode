//! macOS 菜单栏、快捷键，以及不属于某个终端视图的应用级动作。
//!
//! 菜单项上显示的快捷键由 GPUI 从键位绑定里反查，快捷键只在 `keybinds` 里绑，
//! 菜单和键盘自动保持一致。

use gpui::{App, Menu, MenuItem, OsAction, PromptLevel, SystemMenuType, actions};
use runode_cli::SetupTarget;

use crate::{
    search_bar::{Cut, Redo, SearchNext, SearchPrevious, SearchSelection, StartSearch, Undo},
    terminal_view::{
        ClearScreen, Copy, DecreaseFontSize, IncreaseFontSize, JumpToPrompt, Paste, PasteSelection, ResetFontSize,
        SelectAll,
    },
    workspace::{
        ArrangePanes, ClosePane, CloseTab, CloseWorkspace, EqualizePanes, FocusNextPane, FocusPreviousPane, GotoAgent,
        NewSplitDown, NewSplitRight, NewTab, NewWorkspace, NextAgent, NextTab, NextWorkspace, PreviousTab,
        PreviousWorkspace, RenameWorkspace, ToggleFiles, ToggleGit, TogglePaneZoom, ToggleSidebar,
    },
};

actions!(
    runode,
    [
        About,
        OpenConfiguration,
        ReloadConfiguration,
        Quit,
        /// 退出并结束宿主里所有的会话，包括没在窗口里显示的；只在退出会把会话留下时有用。
        QuitAndEndSessions,
        Hide,
        HideOthers,
        ShowAll,
        NewWindow,
        CloseWindow,
        CloseAllWindows,
        Minimize,
        Zoom,
        ToggleFullScreen,
        /// 给 Claude Code 和 Codex 装上 runode 命令行的使用说明（`runode setup`），先确认一句。
        InstallAgentIntegration,
    ]
);

pub fn install(cx: &mut App) {
    cx.on_action(|_: &About, _| crate::about::show());
    cx.on_action(|_: &OpenConfiguration, cx| crate::config::open(cx));
    cx.on_action(|_: &ReloadConfiguration, cx| crate::config::reload(cx));
    cx.on_action(|_: &Quit, cx| crate::workspace::quit(cx));
    cx.on_action(|_: &QuitAndEndSessions, cx| crate::workspace::quit_and_end_sessions(cx));
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &NewWindow, cx| crate::window::open_window(cx, None));
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(window) = cx.active_window() {
            crate::workspace::close_window(window, cx);
        }
    });
    cx.on_action(|_: &CloseAllWindows, cx| crate::workspace::close_all_windows(cx));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |w| w.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |w| w.zoom_window()));
    cx.on_action(|_: &ToggleFullScreen, cx| with_active_window(cx, |w| w.toggle_fullscreen()));
    // 从菜单派发时窗口正在处理这个动作，这时在它上面弹不了框，等这一轮更新结束再弹。
    cx.on_action(|_: &InstallAgentIntegration, cx| cx.defer(install_agent_integration));

    // 装快捷键时会顺带设置菜单。
    crate::keybinds::install(cx);
}

fn tr(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}

/// 设置菜单栏。菜单项上的快捷键在这时从键位表里查，换了绑定要重新调用。宿主怎么跑也在这时
/// 读，决定有没有「退出并结束所有会话」。
pub fn set_menus(cx: &mut App) {
    let mut quit = vec![MenuItem::action(tr("menu.quit"), Quit)];
    if crate::workspace::end_sessions_in_menu() {
        quit.push(MenuItem::action(tr("menu.quit_end_sessions"), QuitAndEndSessions));
    }
    cx.set_menus([
        // macOS 总把第一个菜单当作应用菜单，标题显示为应用名。
        Menu::new("Runode").items(
            [
                MenuItem::action(tr("menu.about"), About),
                MenuItem::separator(),
                MenuItem::action(tr("menu.open_config"), OpenConfiguration),
                MenuItem::action(tr("menu.reload_config"), ReloadConfiguration),
                MenuItem::action(tr("setup.menu"), InstallAgentIntegration),
                MenuItem::separator(),
                MenuItem::os_submenu(tr("menu.services"), SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(tr("menu.hide"), Hide),
                MenuItem::action(tr("menu.hide_others"), HideOthers),
                MenuItem::action(tr("menu.show_all"), ShowAll),
                MenuItem::separator(),
            ]
            .into_iter()
            .chain(quit),
        ),
        Menu::new(tr("menu.file")).items([
            MenuItem::action(tr("menu.new_tab"), NewTab),
            MenuItem::action(tr("menu.new_workspace"), NewWorkspace),
            MenuItem::action(tr("menu.new_window"), NewWindow),
            MenuItem::separator(),
            MenuItem::action(tr("menu.split_right"), NewSplitRight),
            MenuItem::action(tr("menu.split_down"), NewSplitDown),
            MenuItem::action(tr("layout.menu"), ArrangePanes),
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
            MenuItem::action(tr("menu.toggle_git"), ToggleGit),
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

/// 装给哪些 agent。
const SETUP_TARGETS: [SetupTarget; 2] = [SetupTarget::Claude, SetupTarget::Codex];

/// 问一句要不要装，列出会写的文件；装好后说装到了哪里，失败时说原因。
fn install_agent_integration(cx: &mut App) {
    let Some(home) = runode_paths::Dirs::from_env().home else {
        tracing::warn!("cannot install the agent integration: no home directory");
        return;
    };
    let Some(window) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else {
        return;
    };
    let paths = setup_paths(SETUP_TARGETS.iter().map(|target| runode_cli::setup_path(*target, &home)), &home);
    let title = rust_i18n::t!("setup.confirm_title");
    let detail = rust_i18n::t!("setup.confirm_detail", paths = paths);
    let answers = [&*rust_i18n::t!("setup.install"), &*rust_i18n::t!("setup.cancel")];
    let Ok(answer) =
        window.update(cx, |_, window, cx| window.prompt(PromptLevel::Info, &title, Some(&detail), &answers, cx))
    else {
        return;
    };
    cx.spawn(async move |cx| {
        if answer.await.ok() != Some(0) {
            return;
        }
        let installed: anyhow::Result<Vec<_>> =
            SETUP_TARGETS.iter().map(|target| runode_cli::setup(*target, &home)).collect();
        let (level, title, detail) = match installed {
            Ok(paths) => (
                PromptLevel::Info,
                rust_i18n::t!("setup.done_title"),
                rust_i18n::t!("setup.done_detail", paths = setup_paths(paths, &home)),
            ),
            Err(err) => {
                tracing::error!("failed to install the agent integration: {err:#}");
                (PromptLevel::Critical, rust_i18n::t!("setup.failed_title"), format!("{err:#}").into())
            }
        };
        let answer = window.update(cx, |_, window, cx| {
            window.prompt(level, &title, Some(&detail), &[&*rust_i18n::t!("setup.ok")], cx)
        });
        if let Ok(answer) = answer {
            let _ = answer.await;
        }
    })
    .detach();
}

/// 提示框里一行一个文件，`home` 底下的写成 `~/…`。
fn setup_paths(paths: impl IntoIterator<Item = std::path::PathBuf>, home: &std::path::Path) -> String {
    paths
        .into_iter()
        .map(|path| match path.strip_prefix(home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn with_active_window(cx: &mut App, f: impl FnOnce(&mut gpui::Window)) {
    if let Some(window) = cx.active_window() {
        window.update(cx, |_, window, _| f(window)).ok();
    }
}
