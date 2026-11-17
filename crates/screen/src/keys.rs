//! 按美式键盘把文字拆成按键，给只认真的键码、不认只带字符的输入事件的地方（密码框、虚拟机）一个键一个键地敲。

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
    text.chars().map(key_stroke).collect()
}

/// 一次 `ScreenInput::Text` 最多注入的字符数（按 `text_chars` 归并换行后数）。注入在收输入的线程上逐字同步做，
/// 同一条连接上之后的输入和控制消息都等它做完，所以多出的丢掉，不让一大段文字卡住它们。
#[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
pub(crate) const MAX_TEXT_CHARS: usize = 1000;

/// 要注入的文字逐个字符，`\r\n` 和单独的 `\r` 都换成一个 `\n`，每个换行只敲一次回车。最多出
/// `MAX_TEXT_CHARS` 个，超出时记一条警告、丢掉其余的。
#[cfg(any(target_os = "macos", windows, target_os = "linux", test))]
pub(crate) fn text_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    let mut chars = text.chars().peekable();
    let mut left = MAX_TEXT_CHARS;
    std::iter::from_fn(move || {
        let c = chars.next()?;
        if left == 0 {
            tracing::warn!("the text to type is longer than {MAX_TEXT_CHARS} characters, dropping the rest");
            chars = "".chars().peekable();
            return None;
        }
        left -= 1;
        if c != '\r' {
            return Some(c);
        }
        chars.next_if_eq(&'\n');
        Some('\n')
    })
}

/// 美式键盘上敲出 `c` 的键和要不要按住 Shift，敲不出时返回 `None`。
pub(crate) fn key_stroke(c: char) -> Option<(Key, bool)> {
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

    #[test]
    fn line_breaks_become_one_newline() {
        let chars = |text: &str| text_chars(text).collect::<String>();
        assert_eq!(chars("a\r\nb\rc\nd\r\r\n\n"), "a\nb\nc\nd\n\n\n");
        assert_eq!(chars("\r"), "\n");
        assert_eq!(chars("\t你"), "\t你");
        assert_eq!(chars(""), "");
    }

    #[test]
    fn long_text_is_cut_at_the_limit() {
        assert_eq!(text_chars(&"a".repeat(MAX_TEXT_CHARS)).count(), MAX_TEXT_CHARS);
        assert_eq!(text_chars(&"你".repeat(MAX_TEXT_CHARS + 5)).count(), MAX_TEXT_CHARS);
        let lines = text_chars(&"\r\n".repeat(MAX_TEXT_CHARS + 1)).collect::<String>();
        assert_eq!(lines, "\n".repeat(MAX_TEXT_CHARS));
        let long = "a".repeat(MAX_TEXT_CHARS + 2);
        let mut cut = text_chars(&long).skip(MAX_TEXT_CHARS);
        assert_eq!(cut.next(), None);
        assert_eq!(cut.next(), None);
    }
}
