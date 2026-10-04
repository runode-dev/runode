//! 可配置的快捷键：配置文件里写 `keybind = 触发键=动作`。
//!
//! 默认绑定和动作表都只在这里写一次：`DEFAULTS` 用的就是配置文件的写法，和用户写的
//! 走同一个解析器；配置模板也从这两张表生成，新增动作或默认绑定会自动出现在模板里，
//! 动作的说明在翻译的 `action.<name>` 里。
//! 搜索框里的文字编辑键（回车、Esc、剪切、撤销等）属于输入框本身，不开放配置。

use gpui::{Action, App, Global, KeyBinding};

use crate::{
    config::{AppConfig, Keybind},
    menus::{
        About, CloseAllWindows, CloseWindow, Hide, HideOthers, Minimize, NewWindow, OpenConfiguration,
        Quit, ReloadConfiguration, ShowAll, ToggleFullScreen, Zoom,
    },
    pane::Direction,
    search_bar::{Cut, EndSearch, Redo, SearchNext, SearchPrevious, SearchSelection, StartSearch, Undo},
    terminal_view::{
        ClearScreen, Copy, DecreaseFontSize, IncreaseFontSize, JumpToPrompt, Paste, PasteSelection,
        ResetFontSize, ScreenFile, ScrollPageDown, ScrollPageUp, ScrollToBottom, ScrollToSelection,
        ScrollToTop, SelectAll, SendText, WriteScreenFile,
    },
    workspace::{
        ClosePane, CloseTab, EqualizePanes, FocusNextPane, FocusPane, FocusPreviousPane, NewSplitDown,
        NewSplitRight, NewTab, NextTab, PreviousTab, ResizePane, SelectLastTab, SelectTab,
        TogglePaneZoom,
    },
};

/// 一个可绑定的动作。
pub struct ActionSpec {
    pub name: &'static str,
    /// 参数的写法，`None` 表示不带参数。说明在翻译的 `action.<name>` 里。
    pub param: Option<&'static str>,
    /// 绑定生效的上下文，`None` 表示全局。
    contexts: &'static [Option<&'static str>],
    build: fn(Option<&str>) -> Result<Box<dyn Action>, String>,
}

const GLOBAL: &[Option<&str>] = &[None];
const WORKSPACE: &[Option<&str>] = &[Some("Workspace")];
const TERMINAL: &[Option<&str>] = &[Some("Terminal")];
/// 搜索框不在 `Terminal` 上下文里，复制粘贴和搜索导航在那里也要能用。
const TERMINAL_AND_SEARCH: &[Option<&str>] = &[Some("Terminal"), Some("SearchBar")];

fn plain<A: Action + Clone>(action: A) -> impl Fn(Option<&str>) -> Result<Box<dyn Action>, String> {
    move |param| match param {
        None => Ok(action.boxed_clone()),
        Some(_) => Err("this action takes no parameter".into()),
    }
}

macro_rules! plain {
    ($name:literal, $contexts:expr, $action:expr) => {
        ActionSpec {
            name: $name,
            param: None,
            contexts: $contexts,
            build: |param| plain($action)(param),
        }
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
    plain!("about", GLOBAL, About),
    plain!("quit", GLOBAL, Quit),
    plain!("open_config", GLOBAL, OpenConfiguration),
    plain!("reload_config", GLOBAL, ReloadConfiguration),
    plain!("hide", GLOBAL, Hide),
    plain!("hide_others", GLOBAL, HideOthers),
    plain!("show_all", GLOBAL, ShowAll),
    plain!("new_window", GLOBAL, NewWindow),
    plain!("close_window", GLOBAL, CloseWindow),
    plain!("close_all_windows", GLOBAL, CloseAllWindows),
    plain!("minimize", GLOBAL, Minimize),
    plain!("toggle_maximize", GLOBAL, Zoom),
    plain!("toggle_fullscreen", GLOBAL, ToggleFullScreen),
    plain!("new_tab", WORKSPACE, NewTab),
    plain!("close_tab", WORKSPACE, CloseTab),
    plain!("next_tab", WORKSPACE, NextTab),
    plain!("previous_tab", WORKSPACE, PreviousTab),
    plain!("last_tab", WORKSPACE, SelectLastTab),
    ActionSpec {
        name: "goto_tab",
        param: Some("N"),
        contexts: WORKSPACE,
        build: |param| match required(param)?.parse::<usize>() {
            Ok(n) if n >= 1 => Ok(Box::new(SelectTab(n - 1))),
            _ => Err("expected a tab number starting from 1".into()),
        },
    },
    ActionSpec {
        name: "new_split",
        param: Some("right|down"),
        contexts: WORKSPACE,
        build: |param| match required(param)? {
            "right" => Ok(Box::new(NewSplitRight)),
            "down" => Ok(Box::new(NewSplitDown)),
            _ => Err("expected right or down".into()),
        },
    },
    plain!("close_surface", WORKSPACE, ClosePane),
    ActionSpec {
        name: "goto_split",
        param: Some("previous|next|left|right|up|down"),
        contexts: WORKSPACE,
        build: |param| match param {
            Some("previous") => Ok(Box::new(FocusPreviousPane)),
            Some("next") => Ok(Box::new(FocusNextPane)),
            _ => Ok(Box::new(FocusPane(direction(param)?))),
        },
    },
    ActionSpec {
        name: "resize_split",
        param: Some("left|right|up|down"),
        contexts: WORKSPACE,
        // 兼容 `resize_split:left,10` 的写法，数值只校验不使用。
        build: |param| {
            let (dir, amount) = match param.and_then(|p| p.split_once(',')) {
                Some((dir, amount)) => (Some(dir), Some(amount)),
                None => (param, None),
            };
            optional_amount(amount)?;
            Ok(Box::new(ResizePane(direction(dir)?)))
        },
    },
    plain!("equalize_splits", WORKSPACE, EqualizePanes),
    plain!("toggle_split_zoom", WORKSPACE, TogglePaneZoom),
    plain!("copy_to_clipboard", TERMINAL_AND_SEARCH, Copy),
    plain!("paste_from_clipboard", TERMINAL_AND_SEARCH, Paste),
    plain!("paste_from_selection", TERMINAL, PasteSelection),
    plain!("select_all", TERMINAL_AND_SEARCH, SelectAll),
    plain!("clear_screen", TERMINAL, ClearScreen),
    plain!("scroll_to_top", TERMINAL, ScrollToTop),
    plain!("scroll_to_bottom", TERMINAL, ScrollToBottom),
    plain!("scroll_page_up", TERMINAL, ScrollPageUp),
    plain!("scroll_page_down", TERMINAL, ScrollPageDown),
    plain!("scroll_to_selection", TERMINAL, ScrollToSelection),
    ActionSpec {
        name: "jump_to_prompt",
        param: Some("N"),
        contexts: TERMINAL,
        build: |param| match required(param)?.parse::<isize>() {
            Ok(n) if n != 0 => Ok(Box::new(JumpToPrompt(n))),
            _ => Err("expected a non-zero number".into()),
        },
    },
    ActionSpec {
        name: "text",
        param: Some("TEXT"),
        contexts: TERMINAL,
        build: |param| Ok(Box::new(SendText(unescape(required(param)?)?))),
    },
    ActionSpec {
        name: "esc",
        param: Some("TEXT"),
        contexts: TERMINAL,
        build: |param| Ok(Box::new(SendText(format!("\x1b{}", required(param)?)))),
    },
    ActionSpec {
        name: "csi",
        param: Some("TEXT"),
        contexts: TERMINAL,
        build: |param| Ok(Box::new(SendText(format!("\x1b[{}", required(param)?)))),
    },
    plain!("start_search", TERMINAL, StartSearch),
    plain!("search_selection", TERMINAL, SearchSelection),
    ActionSpec {
        name: "navigate_search",
        param: Some("next|previous"),
        contexts: TERMINAL_AND_SEARCH,
        build: |param| match required(param)? {
            "next" => Ok(Box::new(SearchNext)),
            "previous" => Ok(Box::new(SearchPrevious)),
            _ => Err("expected next or previous".into()),
        },
    },
    plain!("end_search", TERMINAL_AND_SEARCH, EndSearch),
    ActionSpec {
        name: "write_screen_file",
        param: Some("copy|paste|open"),
        contexts: TERMINAL,
        build: |param| {
            let file = match required(param)? {
                "copy" => ScreenFile::CopyPath,
                "paste" => ScreenFile::PastePath,
                "open" => ScreenFile::Open,
                _ => return Err("expected copy, paste or open".into()),
            };
            Ok(Box::new(WriteScreenFile(file)))
        },
    },
    ActionSpec {
        name: "increase_font_size",
        param: Some("N"),
        contexts: TERMINAL,
        build: |param| optional_amount(param).map(|()| IncreaseFontSize.boxed_clone()),
    },
    ActionSpec {
        name: "decrease_font_size",
        param: Some("N"),
        contexts: TERMINAL,
        build: |param| optional_amount(param).map(|()| DecreaseFontSize.boxed_clone()),
    },
    plain!("reset_font_size", TERMINAL, ResetFontSize),
];

/// 默认绑定，写法与配置文件里 `keybind =` 的值相同。
pub static DEFAULTS: &[&str] = &[
    "cmd+q=quit",
    "cmd+,=open_config",
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
    "cmd+1=goto_tab:1",
    "cmd+2=goto_tab:2",
    "cmd+3=goto_tab:3",
    "cmd+4=goto_tab:4",
    "cmd+5=goto_tab:5",
    "cmd+6=goto_tab:6",
    "cmd+7=goto_tab:7",
    "cmd+8=goto_tab:8",
    "cmd+9=last_tab",
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
    "cmd+shift+g=navigate_search:previous",
    "cmd+shift+f=end_search",
    "ctrl+shift+cmd+j=write_screen_file:copy",
    "cmd+shift+j=write_screen_file:paste",
    "cmd+shift+alt+j=write_screen_file:open",
    "cmd+equal=increase_font_size:1",
    "cmd+plus=increase_font_size:1",
    "cmd+minus=decrease_font_size:1",
    "cmd+0=reset_font_size",
];

/// 搜索框自己的编辑键，以及搜索时在终端里按 Esc 关掉搜索栏，不开放配置。
fn fixed_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("enter", SearchNext, Some("SearchBar")),
        KeyBinding::new("shift-enter", SearchPrevious, Some("SearchBar")),
        KeyBinding::new("escape", EndSearch, Some("SearchBar")),
        KeyBinding::new("cmd-x", Cut, Some("SearchBar")),
        KeyBinding::new("cmd-z", Undo, Some("SearchBar")),
        KeyBinding::new("cmd-shift-z", Redo, Some("SearchBar")),
        // 点回终端后搜索栏还开着，Esc 照样关掉它；没在搜索时 Esc 照常发给程序。
        KeyBinding::new("escape", EndSearch, Some("Terminal && searching")),
    ]
}

/// 解析一条 `keybind` 的值。触发键转成 GPUI 的写法，动作原样保留并校验过。
pub fn parse(value: &str) -> Result<Keybind, String> {
    if value == "clear" {
        return Ok(Keybind::Clear);
    }
    // 触发键里不会出现 `=`（等号键写作 equal），第一个 `=` 就是分隔。
    let (trigger, action) = value.split_once('=').ok_or("expected TRIGGER=ACTION or clear")?;
    let keys = parse_trigger(trigger.trim())?;
    let action = action.trim();
    if action == "unbind" {
        return Ok(Keybind::Unbind(keys));
    }
    build(action)?;
    Ok(Keybind::Bind { keys, action: action.to_owned() })
}

fn find(action: &str) -> Result<(&'static ActionSpec, Option<&str>), String> {
    let (name, param) = match action.split_once(':') {
        Some((name, param)) => (name, Some(param)),
        None => (action, None),
    };
    let spec = ACTIONS.iter().find(|a| a.name == name).ok_or_else(|| format!("unknown action: {name}"))?;
    Ok((spec, param))
}

fn build(action: &str) -> Result<(Box<dyn Action>, &'static [Option<&'static str>]), String> {
    let (spec, param) = find(action)?;
    Ok(((spec.build)(param)?, spec.contexts))
}

/// `cmd+shift+t`、`ctrl+a>n` 这类触发键转成 GPUI 的 `shift-cmd-t`、`ctrl-a n`。
/// 修饰键按固定顺序输出，同一个组合不管怎么写都得到同一个字符串，便于覆盖和解绑。
fn parse_trigger(trigger: &str) -> Result<String, String> {
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
        out.push_str(&key_name(key)?);
        gpui::Keystroke::parse(&out).map_err(|_| format!("invalid key: {key}"))?;
        strokes.push(out);
    }
    Ok(strokes.join(" "))
}

fn key_name(key: &str) -> Result<String, String> {
    let key = key.to_lowercase();
    let named = match key.as_str() {
        "arrow_up" | "up" => "up",
        "arrow_down" | "down" => "down",
        "arrow_left" | "left" => "left",
        "arrow_right" | "right" => "right",
        "page_up" | "pageup" => "pageup",
        "page_down" | "pagedown" => "pagedown",
        "home" => "home",
        "end" => "end",
        "insert" => "insert",
        "delete" => "delete",
        "enter" | "return" => "enter",
        "escape" | "esc" => "escape",
        "backspace" => "backspace",
        "tab" => "tab",
        "space" => "space",
        "equal" => "=",
        "plus" => "+",
        "minus" => "-",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        "backslash" => "\\",
        "semicolon" => ";",
        "quote" => "'",
        "backquote" | "grave" => "`",
        "bracket_left" => "[",
        "bracket_right" => "]",
        _ => {
            if let Some(rest) = key.strip_prefix("digit_").or_else(|| key.strip_prefix("key_")) {
                return Ok(rest.to_owned());
            }
            let is_function_key =
                key.strip_prefix('f').is_some_and(|n| n.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)));
            if is_function_key || key.chars().count() == 1 {
                return Ok(key);
            }
            return Err(format!("unknown key: {key}"));
        }
    };
    Ok(named.to_owned())
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
pub fn resolve(keybinds: &[Keybind]) -> Vec<(String, String)> {
    let mut table: Vec<(String, String)> = Vec::new();
    let defaults = DEFAULTS.iter().map(|d| parse(d).expect("default keybind parses"));
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

/// 上次装上的 `keybind` 和界面语言，配置重载但两者都没变时不必重绑、重设菜单。
struct Bound(Vec<Keybind>, String);

impl Global for Bound {}

/// 按当前配置装上快捷键，并在配置重载后跟着更新。
pub fn install(cx: &mut App) {
    bind(cx);
    cx.observe_global::<AppConfig>(bind).detach();
}

fn bind(cx: &mut App) {
    let keybinds = cx.global::<AppConfig>().0.keybinds.clone();
    let locale = crate::i18n::current();
    if cx.try_global::<Bound>().is_some_and(|bound| bound.0 == keybinds && bound.1 == locale) {
        return;
    }
    let mut bindings = Vec::new();
    for (keys, action) in resolve(&keybinds) {
        let (action, contexts) = build(&action).expect("keybind was validated when parsed");
        for context in contexts {
            let predicate = context.map(|c| gpui::KeyBindingContextPredicate::parse(c).unwrap().into());
            match KeyBinding::load(&keys, action.boxed_clone(), predicate, false, None, &gpui::DummyKeyboardMapper) {
                Ok(binding) => bindings.push(binding),
                Err(err) => tracing::warn!("keybind {keys}: {err}"),
            }
        }
    }
    // 固定绑定放在后面：同一上下文里后加的优先，搜索时 Esc 照样先关搜索栏；菜单上的快捷键
    // 取的是一个动作最早的绑定，这样「查找下一个」显示 ⌘G 而不是搜索框里的回车。
    bindings.extend(fixed_bindings());
    cx.clear_key_bindings();
    cx.bind_keys(bindings);
    // 菜单上显示的快捷键是设置菜单时从键位表里查的，换了绑定要重设一次。
    crate::menus::set_menus(cx);
    cx.set_global(Bound(keybinds, locale));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_and_every_action_builds() {
        for default in DEFAULTS {
            assert!(matches!(parse(default), Ok(Keybind::Bind { .. })), "{default}");
        }
        let names: Vec<_> = ACTIONS.iter().map(|a| a.name).collect();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(names.len(), unique.len());
    }

    #[test]
    fn triggers_normalize() {
        assert_eq!(parse_trigger("super+shift+t").unwrap(), "shift-cmd-t");
        assert_eq!(parse_trigger("shift+cmd+t").unwrap(), "shift-cmd-t");
        assert_eq!(parse_trigger("ctrl+a>n").unwrap(), "ctrl-a n");
        assert_eq!(parse_trigger("cmd+equal").unwrap(), "cmd-=");
        assert_eq!(parse_trigger("cmd+bracket_left").unwrap(), "cmd-[");
        assert_eq!(parse_trigger("opt+arrow_left").unwrap(), "alt-left");
        assert!(parse_trigger("hyper+t").is_err());
        assert!(parse_trigger("cmd+").is_err());
    }

    #[test]
    fn rejects_bad_actions() {
        assert!(parse("cmd+t=nope").is_err());
        assert!(parse("cmd+t=new_tab:1").is_err());
        assert!(parse("cmd+t=new_split:left").is_err());
        assert!(parse("cmd+t=goto_tab:0").is_err());
        assert!(parse("cmd+t").is_err());
        assert!(parse("cmd+t=resize_split:left,10").is_ok());
        assert!(parse("cmd+t=increase_font_size").is_ok());
    }

    #[test]
    fn unescapes_text() {
        assert_eq!(unescape(r"\x1bb").unwrap(), "\x1bb");
        assert_eq!(unescape(r"a\nb\t\\").unwrap(), "a\nb\t\\");
        assert_eq!(unescape(r"\u{263a}!").unwrap(), "☺!");
        assert!(unescape(r"\q").is_err());
    }

    #[test]
    fn user_keybinds_override_unbind_and_clear() {
        let keybinds = [
            parse("super+t=new_window").unwrap(),
            parse("cmd+w=unbind").unwrap(),
            parse("ctrl+a>c=new_tab").unwrap(),
        ];
        let table = resolve(&keybinds);
        let lookup = |keys: &str| table.iter().filter(|(k, _)| k == keys).map(|(_, a)| a.as_str()).collect::<Vec<_>>();
        assert_eq!(lookup("cmd-t"), ["new_window"]);
        assert!(lookup("cmd-w").is_empty());
        assert_eq!(lookup("ctrl-a c"), ["new_tab"]);
        assert_eq!(lookup("cmd-q"), ["quit"]);

        let table = resolve(&[parse("clear").unwrap(), parse("cmd+q=quit").unwrap()]);
        assert_eq!(table, [("cmd-q".to_owned(), "quit".to_owned())]);
    }

    /// 默认绑定转换后与原来写死的 GPUI 写法一致，升级后快捷键不变。
    #[test]
    fn defaults_keep_gpui_keystrokes() {
        let table = resolve(&[]);
        let has = |keys: &str, action: &str| table.iter().any(|(k, a)| k == keys && a == action);
        assert!(has("shift-cmd-,", "reload_config"));
        assert!(has("alt-cmd-h", "hide_others"));
        assert!(has("alt-shift-cmd-w", "close_all_windows"));
        assert!(has("cmd-}", "next_tab"));
        assert!(has("cmd-+", "increase_font_size:1"));
        assert!(has("cmd--", "decrease_font_size:1"));
        assert!(has("ctrl-cmd-=", "equalize_splits"));
        assert!(has("cmd-pageup", "scroll_page_up"));
        assert!(has("ctrl-shift-cmd-j", "write_screen_file:copy"));
    }
}
