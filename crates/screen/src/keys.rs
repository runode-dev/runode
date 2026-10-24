//! 按美式键盘把文字拆成按键：锁屏的密码框不认只带字符的输入事件（`ScreenInput::Text`），只认真的
//! 键码，所以密码得一个键一个键地敲。

use runode_types::input::Key;

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
const DIGITS: [Key; 10] = [
    Key::Digit0,
    Key::Digit1,
    Key::Digit2,
    Key::Digit3,
    Key::Digit4,
    Key::Digit5,
    Key::Digit6,
    Key::Digit7,
    Key::Digit8,
    Key::Digit9,
];

/// 按美式键盘把 `text` 拆成一串按键，每个带着要不要按住 Shift。有美式键盘上敲不出的字符时返回 `None`。
pub fn key_strokes(text: &str) -> Option<Vec<(Key, bool)>> {
    text.chars()
        .map(|c| {
            Some(match c {
                'a'..='z' => (LETTERS[c as usize - 'a' as usize], false),
                'A'..='Z' => (LETTERS[c as usize - 'A' as usize], true),
                '0'..='9' => (DIGITS[c as usize - '0' as usize], false),
                ')' => (Key::Digit0, true),
                '!' => (Key::Digit1, true),
                '@' => (Key::Digit2, true),
                '#' => (Key::Digit3, true),
                '$' => (Key::Digit4, true),
                '%' => (Key::Digit5, true),
                '^' => (Key::Digit6, true),
                '&' => (Key::Digit7, true),
                '*' => (Key::Digit8, true),
                '(' => (Key::Digit9, true),
                ' ' => (Key::Space, false),
                '-' | '_' => (Key::Minus, c == '_'),
                '=' | '+' => (Key::Equal, c == '+'),
                '[' | '{' => (Key::BracketLeft, c == '{'),
                ']' | '}' => (Key::BracketRight, c == '}'),
                '\\' | '|' => (Key::Backslash, c == '|'),
                ';' | ':' => (Key::Semicolon, c == ':'),
                '\'' | '"' => (Key::Quote, c == '"'),
                ',' | '<' => (Key::Comma, c == '<'),
                '.' | '>' => (Key::Period, c == '>'),
                '/' | '?' => (Key::Slash, c == '?'),
                '`' | '~' => (Key::Backquote, c == '~'),
                _ => return None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_become_us_key_strokes() {
        assert_eq!(
            key_strokes("aZ9!_ ~"),
            Some(vec![
                (Key::A, false),
                (Key::Z, true),
                (Key::Digit9, false),
                (Key::Digit1, true),
                (Key::Minus, true),
                (Key::Space, false),
                (Key::Backquote, true),
            ])
        );
        assert_eq!(key_strokes("密码"), None);
        assert_eq!(key_strokes("é"), None);
    }
}
