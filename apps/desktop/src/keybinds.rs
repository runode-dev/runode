//! 装上快捷键：`keybind` 的写法、动作名和默认绑定在 `runode_config::keybind`，这里把解析好的
//! 动作换成 GPUI 的动作，定下各自在哪些上下文里生效，再加上搜索框和多行输入框自己的编辑键。

use gpui::{App, Global, KeyBinding};
use runode_config::{
    Keybind,
    keybind::{self, Action},
};

use crate::{
    config::AppConfig,
    menus::{
        About, CloseAllWindows, CloseWindow, Hide, HideOthers, Minimize, NewWindow, OpenConfiguration, Quit,
        ReloadConfiguration, ShowAll, ToggleFullScreen, Zoom,
    },
    terminal_view::{
        ClearScreen, Copy, DecreaseFontSize, IncreaseFontSize, JumpToPrompt, Paste, PasteSelection, ResetFontSize,
        ScrollPageDown, ScrollPageUp, ScrollToBottom, ScrollToSelection, ScrollToTop, SelectAll, SendText,
        WriteScreenFile,
    },
    ui::text_area::SubmitText,
    ui::text_field::{Cut, EndSearch, Redo, SearchNext, SearchPrevious, SearchSelection, StartSearch, Undo},
    workspace::{
        ArrangePanes, ClosePane, CloseTab, CloseWorkspace, CollapseSelectedFile, CopyPath, CopyRelativePath,
        DeleteFile, EqualizePanes, ExpandSelectedFile, FocusNextPane, FocusPane, FocusPreviousPane, FocusTerminal,
        GotoAgent, NewSplitDown, NewSplitRight, NewTab, NewWorkspace, NextAgent, NextTab, NextWorkspace,
        OpenSelectedFile, PreviousTab, PreviousWorkspace, RenameFile, RenameWorkspace, ResizePane, RevealInFinder,
        SelectFirstFile, SelectLastFile, SelectLastTab, SelectLastWorkspace, SelectNextFile, SelectPreviousFile,
        SelectTab, SelectWorkspace, ToggleFiles, ToggleGit, TogglePaneZoom, ToggleSidebar,
    },
};

/// 绑定生效的上下文，`None` 表示全局。
type Contexts = &'static [Option<&'static str>];

const GLOBAL: Contexts = &[None];
const WINDOW: Contexts = &[Some("Window")];
const TERMINAL: Contexts = &[Some("Terminal")];
/// 搜索框不在 `Terminal` 上下文里，复制粘贴和搜索导航在那里也要能用。
const TERMINAL_AND_SEARCH: Contexts = &[Some("Terminal"), Some("SearchBar")];
/// 预览栏里选中的行也能全选；多行输入框和搜索框一样能全选、复制、粘贴。
const SELECT_ALL: Contexts = &[Some("Terminal"), Some("SearchBar"), Some("TextArea"), Some("Preview")];
/// 预览栏里选中的行也能复制；文件树里复制、粘贴的是选中的文件。
const COPY: Contexts = &[Some("Terminal"), Some("SearchBar"), Some("TextArea"), Some("Preview"), Some("FileTree")];
const PASTE: Contexts = &[Some("Terminal"), Some("SearchBar"), Some("TextArea"), Some("FileTree")];
/// 文件树有焦点、又不在新建或改名时；改名时方向键这些归输入框。
const FILE_TREE: &str = "FileTree && !editing";

/// 配置里的动作对应的 GPUI 动作，以及它生效的上下文。
fn gpui_action(action: Action) -> (Box<dyn gpui::Action>, Contexts) {
    fn boxed(action: impl gpui::Action) -> Box<dyn gpui::Action> {
        Box::new(action)
    }
    match action {
        Action::About => (boxed(About), GLOBAL),
        Action::Quit => (boxed(Quit), GLOBAL),
        Action::OpenConfig => (boxed(OpenConfiguration), GLOBAL),
        Action::ReloadConfig => (boxed(ReloadConfiguration), GLOBAL),
        Action::Hide => (boxed(Hide), GLOBAL),
        Action::HideOthers => (boxed(HideOthers), GLOBAL),
        Action::ShowAll => (boxed(ShowAll), GLOBAL),
        Action::NewWindow => (boxed(NewWindow), GLOBAL),
        Action::CloseWindow => (boxed(CloseWindow), GLOBAL),
        Action::CloseAllWindows => (boxed(CloseAllWindows), GLOBAL),
        Action::Minimize => (boxed(Minimize), GLOBAL),
        Action::ToggleMaximize => (boxed(Zoom), GLOBAL),
        Action::ToggleFullscreen => (boxed(ToggleFullScreen), GLOBAL),
        Action::NewTab => (boxed(NewTab), WINDOW),
        Action::CloseTab => (boxed(CloseTab), WINDOW),
        Action::NextTab => (boxed(NextTab), WINDOW),
        Action::PreviousTab => (boxed(PreviousTab), WINDOW),
        Action::LastTab => (boxed(SelectLastTab), WINDOW),
        Action::GotoTab(n) => (boxed(SelectTab(n)), WINDOW),
        Action::NewSplitRight => (boxed(NewSplitRight), WINDOW),
        Action::NewSplitDown => (boxed(NewSplitDown), WINDOW),
        Action::CloseSurface => (boxed(ClosePane), WINDOW),
        Action::FocusPreviousSplit => (boxed(FocusPreviousPane), WINDOW),
        Action::FocusNextSplit => (boxed(FocusNextPane), WINDOW),
        Action::FocusSplit(direction) => (boxed(FocusPane(direction)), WINDOW),
        Action::ResizeSplit(direction) => (boxed(ResizePane(direction)), WINDOW),
        Action::EqualizeSplits => (boxed(EqualizePanes), WINDOW),
        Action::ToggleSplitZoom => (boxed(TogglePaneZoom), WINDOW),
        Action::ArrangeSplits => (boxed(ArrangePanes), WINDOW),
        Action::NewWorkspace => (boxed(NewWorkspace), WINDOW),
        Action::CloseWorkspace => (boxed(CloseWorkspace), WINDOW),
        Action::RenameWorkspace => (boxed(RenameWorkspace), WINDOW),
        Action::NextWorkspace => (boxed(NextWorkspace), WINDOW),
        Action::PreviousWorkspace => (boxed(PreviousWorkspace), WINDOW),
        Action::LastWorkspace => (boxed(SelectLastWorkspace), WINDOW),
        Action::GotoWorkspace(n) => (boxed(SelectWorkspace(n)), WINDOW),
        Action::ToggleSidebar => (boxed(ToggleSidebar), WINDOW),
        Action::ToggleGit => (boxed(ToggleGit), WINDOW),
        Action::ToggleFiles => (boxed(ToggleFiles), WINDOW),
        Action::GotoAgent => (boxed(GotoAgent), WINDOW),
        Action::NextAgent => (boxed(NextAgent), WINDOW),
        Action::Copy => (boxed(Copy), COPY),
        Action::Paste => (boxed(Paste), PASTE),
        Action::PasteSelection => (boxed(PasteSelection), TERMINAL),
        Action::SelectAll => (boxed(SelectAll), SELECT_ALL),
        Action::ClearScreen => (boxed(ClearScreen), TERMINAL),
        Action::ScrollToTop => (boxed(ScrollToTop), TERMINAL),
        Action::ScrollToBottom => (boxed(ScrollToBottom), TERMINAL),
        Action::ScrollPageUp => (boxed(ScrollPageUp), TERMINAL),
        Action::ScrollPageDown => (boxed(ScrollPageDown), TERMINAL),
        Action::ScrollToSelection => (boxed(ScrollToSelection), TERMINAL),
        Action::JumpToPrompt(n) => (boxed(JumpToPrompt(n)), TERMINAL),
        Action::SendText(text) => (boxed(SendText(text)), TERMINAL),
        Action::StartSearch => (boxed(StartSearch), TERMINAL),
        Action::SearchSelection => (boxed(SearchSelection), TERMINAL),
        Action::SearchNext => (boxed(SearchNext), TERMINAL_AND_SEARCH),
        Action::SearchPrevious => (boxed(SearchPrevious), TERMINAL_AND_SEARCH),
        Action::EndSearch => (boxed(EndSearch), TERMINAL_AND_SEARCH),
        Action::WriteScreenFile(file) => (boxed(WriteScreenFile(file)), TERMINAL),
        Action::IncreaseFontSize => (boxed(IncreaseFontSize), TERMINAL),
        Action::DecreaseFontSize => (boxed(DecreaseFontSize), TERMINAL),
        Action::ResetFontSize => (boxed(ResetFontSize), TERMINAL),
    }
}

/// 搜索框和多行输入框自己的编辑键，搜索时在终端里按 Esc 关掉搜索栏，以及文件树里的按键，不开放配置。
fn fixed_bindings() -> Vec<KeyBinding> {
    let tree = Some(FILE_TREE);
    vec![
        KeyBinding::new("enter", SearchNext, Some("SearchBar")),
        KeyBinding::new("shift-enter", SearchPrevious, Some("SearchBar")),
        KeyBinding::new("escape", EndSearch, Some("SearchBar")),
        KeyBinding::new("cmd-x", Cut, Some("SearchBar")),
        KeyBinding::new("cmd-z", Undo, Some("SearchBar")),
        KeyBinding::new("cmd-shift-z", Redo, Some("SearchBar")),
        // 点回终端后搜索栏还开着，Esc 照样关掉它；没在搜索时 Esc 照常发给程序。
        KeyBinding::new("escape", EndSearch, Some("Terminal && searching")),
        // 多行输入框里回车换行（输入框自己处理），cmd-enter 提交。默认配置里 cmd-enter 是全局的
        // 切换全屏，GPUI 把全局绑定当作和最深的上下文一样深，同样深时后加的优先，所以这条放在后面就盖过它。
        KeyBinding::new("cmd-enter", SubmitText, Some("TextArea")),
        KeyBinding::new("cmd-x", Cut, Some("TextArea")),
        KeyBinding::new("cmd-z", Undo, Some("TextArea")),
        KeyBinding::new("cmd-shift-z", Redo, Some("TextArea")),
        KeyBinding::new("up", SelectPreviousFile, tree),
        KeyBinding::new("down", SelectNextFile, tree),
        KeyBinding::new("home", SelectFirstFile, tree),
        KeyBinding::new("end", SelectLastFile, tree),
        KeyBinding::new("cmd-up", SelectFirstFile, tree),
        KeyBinding::new("left", CollapseSelectedFile, tree),
        KeyBinding::new("right", ExpandSelectedFile, tree),
        // 空格在预览栏里打开，回车改名。
        KeyBinding::new("space", OpenSelectedFile, tree),
        KeyBinding::new("cmd-down", OpenSelectedFile, tree),
        KeyBinding::new("enter", RenameFile, tree),
        KeyBinding::new("f2", RenameFile, tree),
        KeyBinding::new("cmd-backspace", DeleteFile, tree),
        KeyBinding::new("cmd-x", Cut, tree),
        KeyBinding::new("cmd-alt-r", RevealInFinder, tree),
        KeyBinding::new("cmd-alt-c", CopyPath, tree),
        KeyBinding::new("cmd-alt-shift-c", CopyRelativePath, tree),
        KeyBinding::new("escape", FocusTerminal, tree),
    ]
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
    for (keys, action) in keybind::resolve(&keybinds) {
        let (action, contexts) = gpui_action(action);
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

    /// 配置里的每个动作名都能换成 GPUI 动作，生效的上下文都是 GPUI 认的写法。默认绑定之外，
    /// 带参数的动作再按参数的写法各试一个值。
    #[test]
    fn every_action_name_builds() {
        let samples = ["", ":1", ":-1", ":right", ":previous", ":next", ":copy", ":x"];
        for spec in keybind::ACTIONS {
            let built: Vec<_> = samples
                .iter()
                .filter_map(|param| keybind::parse_action(&format!("{}{param}", spec.name)).ok())
                .map(gpui_action)
                .collect();
            assert!(!built.is_empty(), "no sample parameter builds {}", spec.name);
            for (action, contexts) in built {
                assert!(!contexts.is_empty(), "{} has no context", action.name());
                for context in contexts.iter().flatten() {
                    assert!(gpui::KeyBindingContextPredicate::parse(context).is_ok(), "{context}");
                }
            }
        }
        for (keys, action) in keybind::resolve(&[]) {
            let (action, contexts) = gpui_action(action);
            for context in contexts {
                let predicate = context.map(|c| gpui::KeyBindingContextPredicate::parse(c).unwrap().into());
                let loaded =
                    KeyBinding::load(&keys, action.boxed_clone(), predicate, false, None, &gpui::DummyKeyboardMapper);
                assert!(loaded.is_ok(), "{keys}");
            }
        }
    }

    /// 多行输入框里 cmd-enter 是提交，盖过默认配置里全局的 cmd-enter（切换全屏）；别处照旧切换全屏。
    #[test]
    fn cmd_enter_submits_in_text_area() {
        let mut bindings = Vec::new();
        for (keys, action) in keybind::resolve(&[]) {
            let (action, contexts) = gpui_action(action);
            for context in contexts {
                let predicate = context.map(|c| gpui::KeyBindingContextPredicate::parse(c).unwrap().into());
                bindings.extend(KeyBinding::load(
                    &keys,
                    action.boxed_clone(),
                    predicate,
                    false,
                    None,
                    &gpui::DummyKeyboardMapper,
                ));
            }
        }
        bindings.extend(fixed_bindings());
        let keymap = gpui::Keymap::new(bindings);
        let input = [gpui::Keystroke::parse("cmd-enter").unwrap()];
        let first = |contexts: &[&str]| {
            let stack: Vec<_> = contexts.iter().map(|c| gpui::KeyContext::parse(c).unwrap()).collect();
            keymap.bindings_for_input(&input, &stack).0.first().map(|binding| binding.action().name())
        };
        assert_eq!(first(&["Window", "TextArea"]), Some(gpui::Action::name(&SubmitText)));
        assert_eq!(first(&["Window", "Terminal"]), Some(gpui::Action::name(&ToggleFullScreen)));
    }

    /// 配置这边转出来的触发键 GPUI 都认：每个有名字的键、功能键、单个字符，各配上修饰键和按键序列。
    #[test]
    fn every_accepted_trigger_is_a_gpui_keystroke() {
        let mut keys: Vec<String> = keybind::NAMED_KEYS.iter().map(|(name, _)| (*name).to_owned()).collect();
        keys.extend((1..=24).map(|n| format!("f{n}")));
        keys.extend((b'!'..=b'~').filter(|&b| b != b'+' && b != b'>').map(|b| char::from(b).to_string()));
        keys.extend(["digit_1", "key_a", "key_a-b", "key_-"].map(str::to_owned));
        let mut accepted = 0;
        for key in &keys {
            for trigger in [key.clone(), format!("ctrl+shift+{key}"), format!("cmd+alt+{key}"), format!("ctrl+a>{key}")]
            {
                let Ok(keys) = keybind::parse_trigger(&trigger) else {
                    continue;
                };
                accepted += 1;
                for stroke in keys.split(' ') {
                    assert!(gpui::Keystroke::parse(stroke).is_ok(), "{trigger} gives {stroke}, which GPUI rejects");
                }
            }
        }
        assert!(accepted > 400, "{accepted}");
        assert!(keybind::parse_trigger("cmd+key_a-b").is_err());
    }
}
