//! 快捷键的写法：默认绑定能解析、触发键的规范写法、动作和参数的校验，以及用户绑定怎么覆盖、
//! 解绑和清空默认绑定。

use runode_config::{
    Keybind,
    keybind::{ACTIONS, Action, DEFAULTS, ScreenFile, format_trigger, parse, parse_action, parse_trigger, resolve},
};
use runode_shared_types::pane::Direction;

#[test]
fn defaults_parse_and_action_names_are_unique() {
    for default in DEFAULTS {
        let binds = parse(default).unwrap_or_else(|err| panic!("{default}: {err}"));
        assert!(binds.iter().all(|b| matches!(b, Keybind::Bind { .. })), "{default}");
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
    assert_eq!(parse_trigger("cmd+minus").unwrap(), "cmd--");
    assert!(parse_trigger("cmd+key_a-b").is_err());
}

/// 录下来的 GPUI 写法写回配置的写法，再解析回去是同一个触发键。
#[test]
fn recorded_keystrokes_format_back() {
    for (gpui, config) in [
        ("cmd-shift-t", "shift+cmd+t"),
        ("ctrl-a n", "ctrl+a>n"),
        ("cmd-=", "cmd+equal"),
        ("cmd--", "cmd+minus"),
        ("alt-pageup", "alt+page_up"),
        ("cmd-,", "cmd+,"),
        ("f5", "f5"),
    ] {
        assert_eq!(format_trigger(gpui).as_deref(), Some(config), "{gpui}");
        let normalized = parse_trigger(config).unwrap();
        assert_eq!(format_trigger(&normalized).as_deref(), Some(config));
    }
    for default in DEFAULTS.iter().filter(|d| !d.contains("digit")) {
        let keys = parse_trigger(default.split_once('=').unwrap().0).unwrap();
        let formatted = format_trigger(&keys).unwrap_or_else(|| panic!("{default}"));
        assert_eq!(parse_trigger(&formatted).unwrap(), keys);
    }
    assert_eq!(format_trigger("fn-f1"), None);
    assert_eq!(format_trigger(""), None);
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
fn actions_carry_their_parameters() {
    assert_eq!(parse_action("goto_tab:3"), Ok(Action::GotoTab(2)));
    assert_eq!(parse_action("goto_split:previous"), Ok(Action::FocusPreviousSplit));
    assert_eq!(parse_action("goto_split:top"), Ok(Action::FocusSplit(Direction::Up)));
    assert_eq!(parse_action("resize_split:left,10"), Ok(Action::ResizeSplit(Direction::Left)));
    assert_eq!(parse_action("jump_to_prompt:-1"), Ok(Action::JumpToPrompt(-1)));
    assert_eq!(parse_action(r"text:\x01"), Ok(Action::SendText("\x01".into())));
    assert_eq!(parse_action("esc:b"), Ok(Action::SendText("\x1bb".into())));
    assert_eq!(parse_action("csi:A"), Ok(Action::SendText("\x1b[A".into())));
    assert_eq!(parse_action("run_task:npm: dev"), Ok(Action::RunTask("npm: dev".into())));
    assert!(parse_action("run_task").is_err());
    assert_eq!(parse_action("write_screen_file:open"), Ok(Action::WriteScreenFile(ScreenFile::Open)));
    assert_eq!(parse_action("increase_font_size:1"), Ok(Action::IncreaseFontSize));
}

#[test]
fn user_keybinds_override_unbind_and_clear() {
    let keybinds: Vec<_> =
        ["super+t=new_window", "cmd+w=unbind", "ctrl+a>c=new_tab"].iter().flat_map(|k| parse(k).unwrap()).collect();
    let table = resolve(&keybinds);
    let lookup = |keys: &str| table.iter().filter(|(k, _)| k == keys).map(|(_, a)| a.clone()).collect::<Vec<_>>();
    assert_eq!(lookup("cmd-t"), [Action::NewWindow]);
    assert!(lookup("cmd-w").is_empty());
    assert_eq!(lookup("ctrl-a c"), [Action::NewTab]);
    assert_eq!(lookup("cmd-q"), [Action::Quit]);

    let table = resolve(&[parse("clear").unwrap(), parse("cmd+q=quit").unwrap()].concat());
    assert_eq!(table, [("cmd-q".to_owned(), Action::Quit)]);
}

/// 默认绑定转换后与原来写死的 GPUI 写法一致，升级后快捷键不变。
#[test]
fn defaults_keep_gpui_keystrokes() {
    let table = resolve(&[]);
    let has = |keys: &str, action: Action| table.iter().any(|(k, a)| k == keys && *a == action);
    assert!(has("shift-cmd-,", Action::ReloadConfig));
    assert!(has("alt-cmd-h", Action::HideOthers));
    assert!(has("alt-shift-cmd-w", Action::CloseAllWindows));
    assert!(has("cmd-}", Action::NextTab));
    assert!(has("cmd-+", Action::IncreaseFontSize));
    assert!(has("cmd--", Action::DecreaseFontSize));
    assert!(has("ctrl-cmd-=", Action::EqualizeSplits));
    assert!(has("cmd-pageup", Action::ScrollPageUp));
    assert!(has("ctrl-shift-cmd-j", Action::WriteScreenFile(ScreenFile::CopyPath)));
}

/// `digit` 一条顶九条：按的数字就是第几个，9 是最后一个；解绑也一次解九个。
#[test]
fn digit_binds_one_to_nine() {
    let table = resolve(&parse("ctrl+digit=goto_tab").unwrap());
    let lookup = |keys: &str| table.iter().find(|(k, _)| k == keys).map(|(_, a)| a.clone());
    assert_eq!(lookup("ctrl-1"), Some(Action::GotoTab(0)));
    assert_eq!(lookup("ctrl-8"), Some(Action::GotoTab(7)));
    assert_eq!(lookup("ctrl-9"), Some(Action::LastTab));
    assert_eq!(lookup("cmd-3"), Some(Action::GotoTab(2)));
    assert_eq!(lookup("alt-9"), Some(Action::LastWorkspace));

    let table = resolve(&parse("cmd+digit=unbind").unwrap());
    assert!(
        !table.iter().any(|(k, _)| k.starts_with("cmd-") && k.ends_with(|c: char| c.is_ascii_digit()) && k != "cmd-0")
    );
    assert!(parse("cmd+digit>n=new_tab").is_err());
    assert!(parse("cmd+t=goto_tab").is_err());
}
