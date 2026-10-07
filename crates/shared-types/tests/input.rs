//! 写成文字的组合键：解析、写回去、连按几下，以及写错时的说明。

use runode_shared_types::input::{Key, KeyChord, MAX_KEY_REPEAT, Mods, parse_keys};

fn chord(s: &str) -> KeyChord {
    s.parse().unwrap_or_else(|err| panic!("{s}: {err}"))
}

#[test]
fn chords_round_trip() {
    for written in [
        "a",
        "z",
        "0",
        "9",
        "-",
        "=",
        "[",
        "]",
        "\\",
        ";",
        "'",
        ",",
        ".",
        "/",
        "`",
        "esc",
        "tab",
        "enter",
        "backspace",
        "delete",
        "insert",
        "space",
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "pageup",
        "pagedown",
        "f1",
        "f12",
        "ctrl-c",
        "alt-b",
        "shift-tab",
        "ctrl-alt-shift-x",
        "ctrl--",
        "alt-.",
    ] {
        assert_eq!(chord(written).to_string(), written);
    }
}

#[test]
fn modifiers_and_names_ignore_case_and_order() {
    let ctrl_c = KeyChord { key: Key::C, mods: Mods { ctrl: true, ..Mods::default() } };
    assert_eq!(chord("ctrl-c"), ctrl_c);
    assert_eq!(chord("CTRL-C"), ctrl_c);
    assert_eq!(chord("shift-ctrl-Up"), chord("ctrl-shift-up"));
    assert_eq!(chord("shift-ctrl-up").to_string(), "ctrl-shift-up");
    assert_eq!(chord("escape"), chord("esc"));
    assert_eq!(chord("return").key, Key::Enter);
    assert_eq!(chord("-").key, Key::Minus);
}

#[test]
fn mistakes_are_explained() {
    for bad in ["", "ctrl-", "ctrl-ctrl-c", "?", "ctrl-?", "f13", "hyper-x", "cmd-c", "ab"] {
        let err = bad.parse::<KeyChord>().expect_err(bad);
        assert!(!err.is_empty(), "{bad}");
    }
    assert!("ctrl-ctrl-c".parse::<KeyChord>().unwrap_err().contains("twice"));
    assert!("f13".parse::<KeyChord>().unwrap_err().contains("unknown key"));
}

#[test]
fn a_count_repeats_the_chord() {
    assert_eq!(parse_keys("down*3").unwrap(), vec![chord("down"); 3]);
    assert_eq!(parse_keys("ctrl-c").unwrap(), vec![chord("ctrl-c")]);
    assert_eq!(parse_keys(&format!("up*{MAX_KEY_REPEAT}")).unwrap().len(), MAX_KEY_REPEAT as usize);
    for bad in ["down*0", "down*", "down*x", "down*-1", &format!("up*{}", MAX_KEY_REPEAT + 1), "nope*2", "*3"] {
        assert!(parse_keys(bad).is_err(), "{bad}");
    }
}

#[test]
fn keys_know_the_characters_they_type() {
    assert_eq!(Key::A.unshifted_char(), Some('a'));
    assert_eq!(Key::A.shifted_char(), Some('A'));
    assert_eq!(Key::Digit1.shifted_char(), Some('!'));
    assert_eq!(Key::Space.unshifted_char(), Some(' '));
    assert_eq!(Key::ArrowUp.unshifted_char(), None);
    assert_eq!(Key::Enter.shifted_char(), None);
}
