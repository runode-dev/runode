//! 把 GPUI 的按键翻译成 libghostty 的按键事件。
//!
//! GPUI 用键帽上印的字符命名按键（`key`），另外单独给出平台实际输入的文本
//! （`key_char`）。libghostty 要的是接近物理键位的键码加上这段文本，这样编码器
//! 才能按运行中程序的要求，选择传统编码、modifyOtherKeys 或 Kitty 编码。

use gpui::Keystroke;
use libghostty_vt::key::{Key, Mods};

use crate::session::KeyInput;

/// 终端完全不该收到的按键返回 `None`：Command 键留给应用自己的快捷键。
pub fn translate(keystroke: &Keystroke) -> Option<KeyInput> {
    let m = &keystroke.modifiers;
    if m.platform {
        return None;
    }
    let (key, unshifted) = key_code(&keystroke.key)?;

    let mut mods = Mods::empty();
    if m.shift {
        mods |= Mods::SHIFT;
    }
    if m.alt {
        mods |= Mods::ALT;
    }
    if m.control {
        mods |= Mods::CTRL;
    }

    // Control 组合键由按键本身编码；平台给出的文本是控制字符，编码器不能再原样发出。
    let text = keystroke
        .key_char
        .as_ref()
        .filter(|_| !m.control)
        .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
        .cloned();

    // Shift 和 Option 改变了产出的文本时（a → A，s → ß）算作「已消耗」，
    // 免得编码器再把它们报告一次。
    let mut consumed_mods = Mods::empty();
    if let Some(text) = &text {
        let base = unshifted.to_string();
        if *text != base {
            if m.shift {
                consumed_mods |= Mods::SHIFT;
            }
            if m.alt {
                consumed_mods |= Mods::ALT;
            }
        }
    }

    Some(KeyInput {
        key,
        mods,
        consumed_mods,
        unshifted,
        text,
    })
}

/// GPUI 键名对应的 libghostty 按键及其未按 Shift 时的字符。
/// 美式布局下需要 Shift 的符号，映射回产生它的那个键。
fn key_code(name: &str) -> Option<(Key, char)> {
    let key = match name {
        "enter" => return Some((Key::Enter, '\r')),
        "tab" => return Some((Key::Tab, '\t')),
        "space" => return Some((Key::Space, ' ')),
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        _ => {
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            return printable(c);
        }
    };
    Some((key, '\0'))
}

fn printable(c: char) -> Option<(Key, char)> {
    let lower = c.to_ascii_lowercase();
    let key = match lower {
        'a'..='z' => {
            const LETTERS: [Key; 26] = [
                Key::A,
                Key::B,
                Key::C,
                Key::D,
                Key::E,
                Key::F,
                Key::G,
                Key::H,
                Key::I,
                Key::J,
                Key::K,
                Key::L,
                Key::M,
                Key::N,
                Key::O,
                Key::P,
                Key::Q,
                Key::R,
                Key::S,
                Key::T,
                Key::U,
                Key::V,
                Key::W,
                Key::X,
                Key::Y,
                Key::Z,
            ];
            return Some((LETTERS[(lower as u8 - b'a') as usize], lower));
        }
        '0' | ')' => (Key::Digit0, '0'),
        '1' | '!' => (Key::Digit1, '1'),
        '2' | '@' => (Key::Digit2, '2'),
        '3' | '#' => (Key::Digit3, '3'),
        '4' | '$' => (Key::Digit4, '4'),
        '5' | '%' => (Key::Digit5, '5'),
        '6' | '^' => (Key::Digit6, '6'),
        '7' | '&' => (Key::Digit7, '7'),
        '8' | '*' => (Key::Digit8, '8'),
        '9' | '(' => (Key::Digit9, '9'),
        '-' | '_' => (Key::Minus, '-'),
        '=' | '+' => (Key::Equal, '='),
        '[' | '{' => (Key::BracketLeft, '['),
        ']' | '}' => (Key::BracketRight, ']'),
        '\\' | '|' => (Key::Backslash, '\\'),
        ';' | ':' => (Key::Semicolon, ';'),
        '\'' | '"' => (Key::Quote, '\''),
        ',' | '<' => (Key::Comma, ','),
        '.' | '>' => (Key::Period, '.'),
        '/' | '?' => (Key::Slash, '/'),
        '`' | '~' => (Key::Backquote, '`'),
        // 非美式布局的键：没有键码，但编码器仍会发送它的文本。
        other => (Key::Unidentified, other),
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn stroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    #[test]
    fn shifted_letter_consumes_shift() {
        let input = translate(&stroke("a", Some("A"), Modifiers::shift())).unwrap();
        assert_eq!(input.key, Key::A);
        assert_eq!(input.text.as_deref(), Some("A"));
        assert_eq!(input.consumed_mods, Mods::SHIFT);
        assert_eq!(input.unshifted, 'a');
    }

    #[test]
    fn control_drops_text() {
        let input = translate(&stroke("c", Some("c"), Modifiers::control())).unwrap();
        assert_eq!(input.key, Key::C);
        assert_eq!(input.text, None);
        assert!(input.mods.contains(Mods::CTRL));
    }

    #[test]
    fn command_is_reserved() {
        assert!(translate(&stroke("c", None, Modifiers::command())).is_none());
    }

    #[test]
    fn arrows_have_no_text() {
        let input = translate(&stroke("up", None, Modifiers::none())).unwrap();
        assert_eq!(input.key, Key::ArrowUp);
        assert_eq!(input.text, None);
    }
}
