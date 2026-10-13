//! 可配置的快捷键：配置文件里写 `keybind = 触发键=动作`。
//!
//! 默认绑定和动作表都只在这里写一次：`DEFAULTS` 用的就是配置文件的写法，和用户写的
//! 走同一个解析器；配置模板也从这两张表生成，新增动作或默认绑定会自动出现在模板里，
//! 动作的英文说明写在表里，界面显示的翻译在桌面 app 的 locales 里。这里只管写法和校验，解析出的 `Action` 由界面
//! 换成具体的动作并决定在哪些上下文里生效。
//! 搜索框里的文字编辑键（回车、Esc、剪切、撤销等）属于输入框本身，不开放配置。

use runode_types::pane::Direction;

use crate::Keybind;

/// 解析好的动作，参数已校验并换成具体的值。
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    About,
    Quit,
    OpenSettings,
    OpenConfig,
    ReloadConfig,
    Hide,
    HideOthers,
    ShowAll,
    NewWindow,
    CloseWindow,
    CloseAllWindows,
    Minimize,
    ToggleMaximize,
    ToggleFullscreen,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    LastTab,
    /// 第几个标签，从 0 数。
    GotoTab(usize),
    NewSplitRight,
    NewSplitDown,
    CloseSurface,
    FocusPreviousSplit,
    FocusNextSplit,
    FocusSplit(Direction),
    ResizeSplit(Direction),
    EqualizeSplits,
    ToggleSplitZoom,
    /// 把当前分屏收到终端区底部，只剩标题条；已经折叠过的展开。
    ToggleSplitFold,
    /// 在当前分屏旁边一次开出几个新终端，先选方向和个数。
    ArrangeSplits,
    NewWorkspace,
    CloseWorkspace,
    RenameWorkspace,
    NextWorkspace,
    PreviousWorkspace,
    LastWorkspace,
    /// 第几个工作区，从 0 数。
    GotoWorkspace(usize),
    ToggleSidebar,
    ToggleStatusBar,
    ToggleGit,
    ToggleFiles,
    ToggleGitHubActions,
    /// 打开或关掉标题栏上的命令菜单。
    ToggleTasks,
    /// 跑命令菜单里叫这个名字的命令，和在菜单里点它一样。
    RunTask(String),
    /// 列出所有窗口里的 agent，选一个跳过去。
    GotoAgent,
    /// 不弹列表，直接跳到下一个要处理的 agent：先等回答的，再干完了没看的。
    NextAgent,
    /// 打开或收起铺满窗口的任务板。
    ToggleTodoBoard,
    Copy,
    Paste,
    PasteSelection,
    SelectAll,
    ClearScreen,
    /// 停在提示符上的 shell 换成一个新的，重新读用户配置。
    ReloadShell,
    ScrollToTop,
    ScrollToBottom,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToSelection,
    /// 往前（负数）或往后（正数）跳几个提示符，不为 0。
    JumpToPrompt(isize),
    /// 发给程序的文字，转义已展开。
    SendText(String),
    StartSearch,
    SearchSelection,
    SearchNext,
    SearchPrevious,
    EndSearch,
    WriteScreenFile(ScreenFile),
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
}

/// 把屏幕和回滚内容写进临时文件之后做什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenFile {
    CopyPath,
    PastePath,
    Open,
}

/// 一个可绑定的动作。
pub struct ActionSpec {
    pub name: &'static str,
    /// 英文的说明，配置模板和设置页用；翻译在界面的 locales 里，键是 `action.<name>`。
    pub doc: &'static str,
    /// 参数的写法，`None` 表示不带参数。
    pub param: Option<&'static str>,
    parse: fn(Option<&str>) -> Result<Action, String>,
}

fn plain(param: Option<&str>, action: Action) -> Result<Action, String> {
    match param {
        None => Ok(action),
        Some(_) => Err("this action takes no parameter".into()),
    }
}

macro_rules! plain {
    ($name:literal, $doc:literal, $action:expr) => {
        ActionSpec { name: $name, doc: $doc, param: None, parse: |param| plain(param, $action) }
    };
}

fn required(param: Option<&str>) -> Result<&str, String> {
    param.filter(|p| !p.is_empty()).ok_or_else(|| "this action needs a parameter".into())
}

fn direction(param: Option<&str>) -> Result<Direction, String> {
    match required(param)? {
        "left" => Ok(Direction::Left),
        "right" => Ok(Direction::Right),
        "up" | "top" => Ok(Direction::Up),
        "down" | "bottom" => Ok(Direction::Down),
        _ => Err("expected left, right, up or down".into()),
    }
}

/// 字号步长固定，`increase_font_size:1` 这类写法里的数值只校验不使用。
fn optional_amount(param: Option<&str>) -> Result<(), String> {
    match param {
        None => Ok(()),
        Some(p) => p.trim().parse::<f32>().map(drop).map_err(|_| "expected a number".into()),
    }
}

pub static ACTIONS: &[ActionSpec] = &[
    plain!("about", "About Runode", Action::About),
    plain!("quit", "Quit", Action::Quit),
    plain!("open_settings", "Open settings", Action::OpenSettings),
    plain!("open_config", "Open the configuration file", Action::OpenConfig),
    plain!("reload_config", "Reload the configuration", Action::ReloadConfig),
    plain!("hide", "Hide Runode", Action::Hide),
    plain!("hide_others", "Hide other apps", Action::HideOthers),
    plain!("show_all", "Show all apps", Action::ShowAll),
    plain!("new_window", "New window", Action::NewWindow),
    plain!("close_window", "Close the window", Action::CloseWindow),
    plain!("close_all_windows", "Close all windows", Action::CloseAllWindows),
    plain!("minimize", "Minimize the window", Action::Minimize),
    plain!("toggle_maximize", "Zoom the window", Action::ToggleMaximize),
    plain!("toggle_fullscreen", "Toggle full screen", Action::ToggleFullscreen),
    plain!("new_tab", "New tab", Action::NewTab),
    plain!("close_tab", "Close the tab", Action::CloseTab),
    plain!("next_tab", "Next tab", Action::NextTab),
    plain!("previous_tab", "Previous tab", Action::PreviousTab),
    plain!("last_tab", "Last tab", Action::LastTab),
    ActionSpec {
        name: "goto_tab",
        doc: "The Nth tab, counting from 1",
        param: Some("N"),
        parse: |param| match required(param)?.parse::<usize>() {
            Ok(n) if n >= 1 => Ok(Action::GotoTab(n - 1)),
            _ => Err("expected a tab number starting from 1".into()),
        },
    },
    ActionSpec {
        name: "new_split",
        doc: "Split to the right or down",
        param: Some("right|down"),
        parse: |param| match required(param)? {
            "right" => Ok(Action::NewSplitRight),
            "down" => Ok(Action::NewSplitDown),
            _ => Err("expected right or down".into()),
        },
    },
    plain!("close_surface", "Close the current split, or the tab when it is the only one", Action::CloseSurface),
    ActionSpec {
        name: "goto_split",
        doc: "Focus the previous, next, or adjacent split in a direction",
        param: Some("previous|next|left|right|up|down"),
        parse: |param| match param {
            Some("previous") => Ok(Action::FocusPreviousSplit),
            Some("next") => Ok(Action::FocusNextSplit),
            _ => Ok(Action::FocusSplit(direction(param)?)),
        },
    },
    ActionSpec {
        name: "resize_split",
        doc: "Move the nearest divider in this direction by a fixed step",
        param: Some("left|right|up|down"),
        // 兼容 `resize_split:left,10` 的写法，数值只校验不使用。
        parse: |param| {
            let (dir, amount) = match param.and_then(|p| p.split_once(',')) {
                Some((dir, amount)) => (Some(dir), Some(amount)),
                None => (param, None),
            };
            optional_amount(amount)?;
            Ok(Action::ResizeSplit(direction(dir)?))
        },
    },
    plain!("equalize_splits", "Make all splits the same size", Action::EqualizeSplits),
    plain!("toggle_split_zoom", "Zoom or restore the current split", Action::ToggleSplitZoom),
    plain!(
        "toggle_split_fold",
        "Collapse the current split to the bottom of the tab, or expand it again",
        Action::ToggleSplitFold
    ),
    plain!(
        "arrange_splits",
        "Open a few new terminals beside the current split in one go, picking the direction and how many",
        Action::ArrangeSplits
    ),
    plain!("new_workspace", "Choose a directory and open a workspace there", Action::NewWorkspace),
    plain!("close_workspace", "Close the workspace and all of its tabs", Action::CloseWorkspace),
    plain!("rename_workspace", "Rename the workspace in the sidebar", Action::RenameWorkspace),
    plain!("next_workspace", "Next workspace", Action::NextWorkspace),
    plain!("previous_workspace", "Previous workspace", Action::PreviousWorkspace),
    plain!("last_workspace", "Last workspace", Action::LastWorkspace),
    ActionSpec {
        name: "goto_workspace",
        doc: "The Nth workspace, counting from 1",
        param: Some("N"),
        parse: |param| match required(param)?.parse::<usize>() {
            Ok(n) if n >= 1 => Ok(Action::GotoWorkspace(n - 1)),
            _ => Err("expected a workspace number starting from 1".into()),
        },
    },
    plain!("toggle_sidebar", "Show or hide the workspace sidebar", Action::ToggleSidebar),
    plain!("toggle_status_bar", "Show or hide the status bar at the bottom of the window", Action::ToggleStatusBar),
    plain!(
        "toggle_git",
        "Show or hide the Git panel on the right to stage, commit, switch branches, sync and stash",
        Action::ToggleGit
    ),
    plain!(
        "toggle_files",
        "Show or hide the file tree on the right; double-click a file to open it in the preview",
        Action::ToggleFiles
    ),
    plain!(
        "toggle_github_actions",
        "Show or hide the GitHub Actions page on the right, with the workflow runs of the current branch, workflows, secrets and variables, read and changed through gh",
        Action::ToggleGitHubActions
    ),
    plain!(
        "toggle_tasks",
        "Open or close the tasks menu in the title bar; pick with the arrow keys and press Enter to run",
        Action::ToggleTasks
    ),
    plain!(
        "toggle_todo_board",
        "Open or close the full-window todo board: tasks handed to agents in columns by their state, and the schedules that start them",
        Action::ToggleTodoBoard
    ),
    ActionSpec {
        name: "run_task",
        doc: "Run the task with this name from the tasks menu (your own, a Makefile target or a package.json script), as if clicked there; with duplicate names the one higher in the menu wins",
        param: Some("NAME"),
        parse: |param| Ok(Action::RunTask(required(param)?.to_owned())),
    },
    plain!("goto_agent", "List the agents in every window and jump to the one you pick", Action::GotoAgent),
    plain!(
        "next_agent",
        "Jump to the next agent that needs you, those waiting for an answer first, then those that finished",
        Action::NextAgent
    ),
    plain!("copy_to_clipboard", "Copy", Action::Copy),
    plain!("paste_from_clipboard", "Paste", Action::Paste),
    plain!("paste_from_selection", "Paste the selection", Action::PasteSelection),
    plain!("select_all", "Select all", Action::SelectAll),
    plain!("clear_screen", "Clear the screen and the scrollback", Action::ClearScreen),
    plain!(
        "reload_shell",
        "Restart the shell at the prompt so it reads its configuration again; the shell integration stays",
        Action::ReloadShell
    ),
    plain!("scroll_to_top", "Scroll to the top", Action::ScrollToTop),
    plain!("scroll_to_bottom", "Scroll to the bottom", Action::ScrollToBottom),
    plain!("scroll_page_up", "Scroll up a page", Action::ScrollPageUp),
    plain!("scroll_page_down", "Scroll down a page", Action::ScrollPageDown),
    plain!("scroll_to_selection", "Scroll to the selection", Action::ScrollToSelection),
    ActionSpec {
        name: "jump_to_prompt",
        doc: "Jump to the previous (negative) or next (positive) prompt",
        param: Some("N"),
        parse: |param| match required(param)?.parse::<isize>() {
            Ok(n) if n != 0 => Ok(Action::JumpToPrompt(n)),
            _ => Err("expected a non-zero number".into()),
        },
    },
    ActionSpec {
        name: "text",
        doc: "Send text to the program; escapes such as \\x1b, \\n, \\t, \\u{...} work",
        param: Some("TEXT"),
        parse: |param| Ok(Action::SendText(unescape(required(param)?)?)),
    },
    ActionSpec {
        name: "esc",
        doc: "Send Esc followed by the text",
        param: Some("TEXT"),
        parse: |param| Ok(Action::SendText(format!("\x1b{}", required(param)?))),
    },
    ActionSpec {
        name: "csi",
        doc: "Send Esc [ followed by the text",
        param: Some("TEXT"),
        parse: |param| Ok(Action::SendText(format!("\x1b[{}", required(param)?))),
    },
    plain!("start_search", "Open the search bar", Action::StartSearch),
    plain!("search_selection", "Search for the selected text", Action::SearchSelection),
    ActionSpec {
        name: "navigate_search",
        doc: "Next or previous search match",
        param: Some("next|previous"),
        parse: |param| match required(param)? {
            "next" => Ok(Action::SearchNext),
            "previous" => Ok(Action::SearchPrevious),
            _ => Err("expected next or previous".into()),
        },
    },
    plain!("end_search", "Close the search bar", Action::EndSearch),
    ActionSpec {
        name: "write_screen_file",
        doc: "Write the screen and scrollback to a temporary file, then copy its path, paste its path, or open it",
        param: Some("copy|paste|open"),
        parse: |param| {
            let file = match required(param)? {
                "copy" => ScreenFile::CopyPath,
                "paste" => ScreenFile::PastePath,
                "open" => ScreenFile::Open,
                _ => return Err("expected copy, paste or open".into()),
            };
            Ok(Action::WriteScreenFile(file))
        },
    },
    ActionSpec {
        name: "increase_font_size",
        doc: "Increase the font size by a fixed step; N can be omitted",
        param: Some("N"),
        parse: |param| optional_amount(param).map(|()| Action::IncreaseFontSize),
    },
    ActionSpec {
        name: "decrease_font_size",
        doc: "Decrease the font size by a fixed step; N can be omitted",
        param: Some("N"),
        parse: |param| optional_amount(param).map(|()| Action::DecreaseFontSize),
    },
    plain!("reset_font_size", "Reset the font size", Action::ResetFontSize),
];

/// 默认绑定，写法与配置文件里 `keybind =` 的值相同。
pub static DEFAULTS: &[&str] = &[
    "cmd+q=quit",
    "cmd+,=open_settings",
    "cmd+shift+,=reload_config",
    "cmd+h=hide",
    "alt+cmd+h=hide_others",
    "cmd+n=new_window",
    "cmd+shift+w=close_window",
    "cmd+shift+alt+w=close_all_windows",
    "cmd+m=minimize",
    "ctrl+cmd+f=toggle_fullscreen",
    "cmd+enter=toggle_fullscreen",
    "cmd+t=new_tab",
    "cmd+w=close_surface",
    "cmd+alt+w=close_tab",
    "cmd+}=next_tab",
    "ctrl+tab=next_tab",
    "cmd+{=previous_tab",
    "ctrl+shift+tab=previous_tab",
    "cmd+digit=goto_tab",
    "cmd+d=new_split:right",
    "cmd+shift+d=new_split:down",
    "cmd+[=goto_split:previous",
    "cmd+]=goto_split:next",
    "cmd+alt+left=goto_split:left",
    "cmd+alt+right=goto_split:right",
    "cmd+alt+up=goto_split:up",
    "cmd+alt+down=goto_split:down",
    "ctrl+cmd+left=resize_split:left",
    "ctrl+cmd+right=resize_split:right",
    "ctrl+cmd+up=resize_split:up",
    "ctrl+cmd+down=resize_split:down",
    "ctrl+cmd+equal=equalize_splits",
    "cmd+shift+enter=toggle_split_zoom",
    "ctrl+cmd+minus=toggle_split_fold",
    "cmd+shift+l=arrange_splits",
    "cmd+shift+n=new_workspace",
    "ctrl+cmd+]=next_workspace",
    "ctrl+cmd+[=previous_workspace",
    "alt+digit=goto_workspace",
    "cmd+b=toggle_sidebar",
    // 占了 macOS 惯用的「查找上一个」：终端里的绑定比窗口的优先，两个都留着时按下去永远是搜索。
    // 搜索框里 Shift+回车照样跳到上一个。
    "cmd+shift+g=toggle_git",
    "cmd+shift+e=toggle_files",
    "cmd+shift+i=toggle_github_actions",
    "cmd+shift+r=toggle_tasks",
    "cmd+shift+b=toggle_todo_board",
    "cmd+shift+a=goto_agent",
    "cmd+alt+a=next_agent",
    "cmd+c=copy_to_clipboard",
    "cmd+v=paste_from_clipboard",
    "cmd+shift+v=paste_from_selection",
    "cmd+a=select_all",
    "cmd+k=clear_screen",
    "cmd+home=scroll_to_top",
    "cmd+end=scroll_to_bottom",
    "cmd+page_up=scroll_page_up",
    "cmd+page_down=scroll_page_down",
    "cmd+j=scroll_to_selection",
    "cmd+up=jump_to_prompt:-1",
    "cmd+down=jump_to_prompt:1",
    "cmd+shift+up=jump_to_prompt:-1",
    "cmd+shift+down=jump_to_prompt:1",
    // 行编辑：跳到行首、行尾，删到行首，按词左右移动。
    r"cmd+left=text:\x01",
    r"cmd+right=text:\x05",
    r"cmd+backspace=text:\x15",
    "alt+left=esc:b",
    "alt+right=esc:f",
    "cmd+f=start_search",
    "cmd+e=search_selection",
    "cmd+g=navigate_search:next",
    "cmd+shift+f=end_search",
    "ctrl+shift+cmd+j=write_screen_file:copy",
    "cmd+shift+j=write_screen_file:paste",
    "cmd+shift+alt+j=write_screen_file:open",
    "cmd+equal=increase_font_size:1",
    "cmd+plus=increase_font_size:1",
    "cmd+minus=decrease_font_size:1",
    "cmd+0=reset_font_size",
];

/// 触发键里代表数字 1 到 9 的键名，配置和 GPUI 的写法相同。
const DIGIT: &str = "digit";

/// 解析一条 `keybind` 的值。触发键转成 GPUI 的写法，动作解析成 `Action`。
///
/// 最后一个键写 `digit` 时一条顶九条，数字 1 到 9 各绑一次：`goto_tab`、`goto_workspace` 不带
/// 参数时按的数字就是第几个，9 是最后一个；别的动作九个数字都绑同一个。
pub fn parse(value: &str) -> Result<Vec<Keybind>, String> {
    if value == "clear" {
        return Ok(vec![Keybind::Clear]);
    }
    // 触发键里不会出现 `=`（等号键写作 equal），第一个 `=` 就是分隔。
    let (trigger, action) = value.split_once('=').ok_or("expected TRIGGER=ACTION or clear")?;
    let keys = parse_trigger(trigger.trim())?;
    let action = action.trim();
    let Some(prefix) = keys.strip_suffix(DIGIT) else {
        if keys.contains(DIGIT) {
            return Err("digit only works as the last key".into());
        }
        return Ok(vec![bind(keys, action)?]);
    };
    (1..=9)
        .map(|n| {
            let action = match (action, n) {
                ("goto_tab", 9) => "last_tab".to_owned(),
                ("goto_workspace", 9) => "last_workspace".to_owned(),
                ("goto_tab" | "goto_workspace", n) => format!("{action}:{n}"),
                _ => action.to_owned(),
            };
            bind(format!("{prefix}{n}"), &action)
        })
        .collect()
}

fn bind(keys: String, action: &str) -> Result<Keybind, String> {
    if action == "unbind" {
        return Ok(Keybind::Unbind(keys));
    }
    Ok(Keybind::Bind { keys, action: parse_action(action)? })
}

/// 解析 `动作` 或 `动作:参数`。
pub fn parse_action(action: &str) -> Result<Action, String> {
    let (name, param) = match action.split_once(':') {
        Some((name, param)) => (name, Some(param)),
        None => (action, None),
    };
    let spec = ACTIONS.iter().find(|a| a.name == name).ok_or_else(|| format!("unknown action: {name}"))?;
    (spec.parse)(param)
}

/// `cmd+shift+t`、`ctrl+a>n` 这类触发键转成 GPUI 的 `shift-cmd-t`、`ctrl-a n`。
/// 修饰键按固定顺序输出，同一个组合不管怎么写都得到同一个字符串，便于覆盖和解绑。
pub fn parse_trigger(trigger: &str) -> Result<String, String> {
    let mut strokes = Vec::new();
    for stroke in trigger.split('>') {
        let mut parts: Vec<&str> = stroke.split('+').map(str::trim).collect();
        let key = parts.pop().filter(|k| !k.is_empty()).ok_or("missing key")?;
        let (mut ctrl, mut alt, mut shift, mut cmd) = (false, false, false, false);
        for part in parts {
            match part {
                "ctrl" | "control" => ctrl = true,
                "alt" | "opt" | "option" => alt = true,
                "shift" => shift = true,
                "cmd" | "command" | "super" => cmd = true,
                _ => return Err(format!("unknown modifier: {part}")),
            }
        }
        let mut out = String::new();
        for (on, name) in [(ctrl, "ctrl-"), (alt, "alt-"), (shift, "shift-"), (cmd, "cmd-")] {
            if on {
                out.push_str(name);
            }
        }
        let name = key_name(key)?;
        // GPUI 的写法用 `-` 连接修饰键和键，键名里再出现 `-` 就分不开了；单独的减号键除外。
        if name != "-" && name.contains('-') {
            return Err(format!("invalid key: {key}"));
        }
        out.push_str(&name);
        strokes.push(out);
    }
    Ok(strokes.join(" "))
}

/// `parse_trigger` 的逆运算：GPUI 写法的触发键（比如录下来的按键 `shift-cmd-t`、`ctrl-a n`）
/// 写成配置文件的写法 `shift+cmd+t`、`ctrl+a>n`。配置里写不出来的（带 fn 键、键名认不出）为
/// `None`。
pub fn format_trigger(keys: &str) -> Option<String> {
    const MODIFIERS: [&str; 4] = ["ctrl", "alt", "shift", "cmd"];
    let mut strokes = Vec::new();
    for stroke in keys.split(' ').filter(|s| !s.is_empty()) {
        let mut rest = stroke;
        let mut on = [false; 4];
        // 键本身可能是 `-`，所以一个个剥掉认得的修饰键前缀，剩下的就是键；修饰键按 `parse_trigger`
        // 的顺序写出，GPUI 录下来的 `cmd-shift-t` 写成 `shift+cmd+t`。
        while let Some(ix) = MODIFIERS
            .iter()
            .position(|m| rest.strip_prefix(m).and_then(|r| r.strip_prefix('-')).is_some_and(|key| !key.is_empty()))
        {
            on[ix] = true;
            rest = &rest[MODIFIERS[ix].len() + 1..];
        }
        let key = match rest {
            "=" => "equal",
            "+" => "plus",
            "-" => "minus",
            "pageup" => "page_up",
            "pagedown" => "page_down",
            " " => "space",
            key => key,
        };
        let mut parts: Vec<&str> = MODIFIERS.iter().zip(on).filter(|(_, on)| *on).map(|(m, _)| *m).collect();
        parts.push(key);
        strokes.push(parts.join("+"));
    }
    let trigger = strokes.join(">");
    // 带 `fn-` 或键名认不出的，配置里写不出来。
    (!trigger.is_empty() && parse_trigger(&trigger).is_ok()).then_some(trigger)
}

/// 有名字的键：(配置里的写法, GPUI 的写法)。
pub const NAMED_KEYS: &[(&str, &str)] = &[
    ("arrow_up", "up"),
    ("up", "up"),
    ("arrow_down", "down"),
    ("down", "down"),
    ("arrow_left", "left"),
    ("left", "left"),
    ("arrow_right", "right"),
    ("right", "right"),
    ("page_up", "pageup"),
    ("pageup", "pageup"),
    ("page_down", "pagedown"),
    ("pagedown", "pagedown"),
    ("home", "home"),
    ("end", "end"),
    ("insert", "insert"),
    ("delete", "delete"),
    ("enter", "enter"),
    ("return", "enter"),
    ("escape", "escape"),
    ("esc", "escape"),
    ("backspace", "backspace"),
    ("tab", "tab"),
    ("space", "space"),
    ("equal", "="),
    ("plus", "+"),
    ("minus", "-"),
    ("comma", ","),
    ("period", "."),
    ("slash", "/"),
    ("backslash", "\\"),
    ("semicolon", ";"),
    ("quote", "'"),
    ("backquote", "`"),
    ("grave", "`"),
    ("bracket_left", "["),
    ("bracket_right", "]"),
];

fn key_name(key: &str) -> Result<String, String> {
    let key = key.to_lowercase();
    if key == DIGIT {
        return Ok(key);
    }
    if let Some((_, named)) = NAMED_KEYS.iter().find(|(name, _)| *name == key) {
        return Ok((*named).to_owned());
    }
    if let Some(rest) = key.strip_prefix("digit_").or_else(|| key.strip_prefix("key_")) {
        return Ok(rest.to_owned());
    }
    let is_function_key = key.strip_prefix('f').is_some_and(|n| n.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)));
    if is_function_key || key.chars().count() == 1 {
        return Ok(key);
    }
    Err(format!("unknown key: {key}"))
}

/// 展开 `text:` 里的转义：`\xHH`、`\u{...}`、`\n`、`\r`、`\t`、`\\`、`\"`、`\'`。
fn unescape(text: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(c @ ('\\' | '"' | '\'')) => out.push(c),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                let byte = u8::from_str_radix(&hex, 16).map_err(|_| "expected \\xHH")?;
                out.push(char::from(byte));
            }
            Some('u') => {
                let rest = chars.as_str();
                let body = rest.strip_prefix('{').and_then(|r| r.split_once('}')).map(|(b, _)| b);
                let code = body
                    .and_then(|b| u32::from_str_radix(b, 16).ok())
                    .and_then(char::from_u32)
                    .ok_or("expected \\u{HEX}")?;
                out.push(code);
                chars = rest[body.unwrap_or_default().len() + 2..].chars();
            }
            _ => return Err("unknown escape".into()),
        }
    }
    Ok(out)
}

/// 默认绑定叠上配置里的 `keybind`，得到最终的 (触发键, 动作) 列表。同一个触发键只保留
/// 最后一次绑定。
pub fn resolve(keybinds: &[Keybind]) -> Vec<(String, Action)> {
    let mut table: Vec<(String, Action)> = Vec::new();
    let defaults = DEFAULTS.iter().flat_map(|d| parse(d).expect("default keybind parses"));
    for keybind in defaults.chain(keybinds.iter().cloned()) {
        match keybind {
            Keybind::Clear => table.clear(),
            Keybind::Unbind(keys) => table.retain(|(k, _)| *k != keys),
            Keybind::Bind { keys, action } => {
                table.retain(|(k, _)| *k != keys);
                table.push((keys, action));
            }
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_text() {
        assert_eq!(unescape(r"\x1bb").unwrap(), "\x1bb");
        assert_eq!(unescape(r"a\nb\t\\").unwrap(), "a\nb\t\\");
        assert_eq!(unescape(r"\u{263a}!").unwrap(), "☺!");
        assert!(unescape(r"\q").is_err());
    }
}
