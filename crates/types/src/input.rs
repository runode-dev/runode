//! 交给终端的键盘和鼠标输入，与界面框架无关。

/// 接近物理键位的按键。字母、数字和符号键按美式布局里产生它的那个键命名；别的布局上没有
/// 对应键位的字符用 `Unidentified`，字符本身由 `KeyInput::text` 带出。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Unidentified,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Minus,
    Equal,
    BracketLeft,
    BracketRight,
    Backslash,
    Semicolon,
    Quote,
    Comma,
    Period,
    Slash,
    Backquote,
    Space,
    Enter,
    Tab,
    Backspace,
    Escape,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// 按着的修饰键。Command 键留给应用自己的快捷键，不交给终端，所以这里没有它。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// 按着的 Alt（Option）是右边那个，供 `macos-option-as-alt` 区分左右键。
    pub right_alt: bool,
}

/// 一次按键。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub key: Key,
    pub mods: Mods,
    /// 平台生成 `text` 时已经用掉的修饰键。
    pub consumed_mods: Mods,
    /// 不带修饰键时该键产生的字符（没有则为 '\0'）。
    pub unshifted: char,
    pub text: Option<String>,
}

/// 上报给程序的鼠标按键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// 上报给程序的鼠标动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    /// 指针移动；按着键时是拖动。
    Motion,
}

/// 用键盘调整选区时，选区活动的一端往哪里挪。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionAdjust {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// 写成文字的一个组合键，命令行发控制键时用：`ctrl-c`、`alt-b`、`shift-tab`、`up`、`f5`。
/// 修饰键写在前面，用 `-` 连接，可以叠加（`ctrl-alt-x`）；键名不分大小写：
///
/// - 字母 `a`–`z`、数字 `0`–`9`，以及美式键盘上不按 Shift 打出的符号 `` - = [ ] \ ; ' , . / ` ``
///   （`ctrl--` 是 Ctrl 加减号）；
/// - `esc`、`tab`、`enter`、`backspace`、`delete`、`insert`、`space`、`up`、`down`、`left`、
///   `right`、`home`、`end`、`pageup`、`pagedown`、`f1`–`f12`。
///
/// 按 Shift 才打得出的符号（`?`、`!` 这类）不是键，当文字打出去。同一个组合键连按几下写成
/// `down*3`，见 `parse_keys`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyChord {
    pub key: Key,
    pub mods: Mods,
}

/// `down*3` 这种写法最多连按几下，免得一个笔误写出成千上万个按键。
pub const MAX_KEY_REPEAT: u32 = 1000;

/// 有名字的键和它们的写法；第一个是 `Display` 用的，后面的是也认的别名。
const NAMED_KEYS: &[(Key, &[&str])] = &[
    (Key::Escape, &["esc", "escape"]),
    (Key::Tab, &["tab"]),
    (Key::Enter, &["enter", "return"]),
    (Key::Backspace, &["backspace"]),
    (Key::Delete, &["delete", "del"]),
    (Key::Insert, &["insert"]),
    (Key::Space, &["space"]),
    (Key::ArrowUp, &["up"]),
    (Key::ArrowDown, &["down"]),
    (Key::ArrowLeft, &["left"]),
    (Key::ArrowRight, &["right"]),
    (Key::Home, &["home"]),
    (Key::End, &["end"]),
    (Key::PageUp, &["pageup"]),
    (Key::PageDown, &["pagedown"]),
    (Key::F1, &["f1"]),
    (Key::F2, &["f2"]),
    (Key::F3, &["f3"]),
    (Key::F4, &["f4"]),
    (Key::F5, &["f5"]),
    (Key::F6, &["f6"]),
    (Key::F7, &["f7"]),
    (Key::F8, &["f8"]),
    (Key::F9, &["f9"]),
    (Key::F10, &["f10"]),
    (Key::F11, &["f11"]),
    (Key::F12, &["f12"]),
];

/// 一个字符就是名字的键：美式键盘上不按 Shift 时它打出的字符。
const CHAR_KEYS: &[(Key, char)] = &[
    (Key::A, 'a'),
    (Key::B, 'b'),
    (Key::C, 'c'),
    (Key::D, 'd'),
    (Key::E, 'e'),
    (Key::F, 'f'),
    (Key::G, 'g'),
    (Key::H, 'h'),
    (Key::I, 'i'),
    (Key::J, 'j'),
    (Key::K, 'k'),
    (Key::L, 'l'),
    (Key::M, 'm'),
    (Key::N, 'n'),
    (Key::O, 'o'),
    (Key::P, 'p'),
    (Key::Q, 'q'),
    (Key::R, 'r'),
    (Key::S, 's'),
    (Key::T, 't'),
    (Key::U, 'u'),
    (Key::V, 'v'),
    (Key::W, 'w'),
    (Key::X, 'x'),
    (Key::Y, 'y'),
    (Key::Z, 'z'),
    (Key::Digit0, '0'),
    (Key::Digit1, '1'),
    (Key::Digit2, '2'),
    (Key::Digit3, '3'),
    (Key::Digit4, '4'),
    (Key::Digit5, '5'),
    (Key::Digit6, '6'),
    (Key::Digit7, '7'),
    (Key::Digit8, '8'),
    (Key::Digit9, '9'),
    (Key::Minus, '-'),
    (Key::Equal, '='),
    (Key::BracketLeft, '['),
    (Key::BracketRight, ']'),
    (Key::Backslash, '\\'),
    (Key::Semicolon, ';'),
    (Key::Quote, '\''),
    (Key::Comma, ','),
    (Key::Period, '.'),
    (Key::Slash, '/'),
    (Key::Backquote, '`'),
];

impl Key {
    /// 美式键盘上不按 Shift 时这个键打出的字符；不打字的键（方向键、F1 这类，以及回车、Tab、
    /// 退格这些控制键）为 `None`。空格键是空格。
    pub fn unshifted_char(self) -> Option<char> {
        if self == Key::Space {
            return Some(' ');
        }
        CHAR_KEYS.iter().find(|(key, _)| *key == self).map(|(_, c)| *c)
    }

    /// 按着 Shift 时这个键在美式键盘上打出的字符；不打字的键为 `None`。
    pub fn shifted_char(self) -> Option<char> {
        let c = self.unshifted_char()?;
        Some(match c {
            'a'..='z' => c.to_ascii_uppercase(),
            '1' => '!',
            '2' => '@',
            '3' => '#',
            '4' => '$',
            '5' => '%',
            '6' => '^',
            '7' => '&',
            '8' => '*',
            '9' => '(',
            '0' => ')',
            '-' => '_',
            '=' => '+',
            '[' => '{',
            ']' => '}',
            '\\' => '|',
            ';' => ':',
            '\'' => '"',
            ',' => '<',
            '.' => '>',
            '/' => '?',
            '`' => '~',
            other => other,
        })
    }
}

impl std::str::FromStr for KeyChord {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut mods = Mods::default();
        let mut rest = s;
        // 修饰键的前缀后面还得剩下键名：`ctrl-` 本身不是组合键，`-` 是减号键。
        loop {
            let lower = rest.to_ascii_lowercase();
            let Some((prefix, slot)) =
                [("ctrl-", &mut mods.ctrl), ("alt-", &mut mods.alt), ("shift-", &mut mods.shift)]
                    .into_iter()
                    .find(|(prefix, _)| lower.starts_with(prefix) && lower.len() > prefix.len())
            else {
                break;
            };
            if std::mem::replace(slot, true) {
                return Err(format!("{s:?} names the same modifier twice"));
            }
            rest = &rest[prefix.len()..];
        }
        let lower = rest.to_ascii_lowercase();
        let named = NAMED_KEYS.iter().find(|(_, names)| names.contains(&lower.as_str())).map(|(key, _)| *key);
        let mut chars = lower.chars();
        let single = match (chars.next(), chars.next()) {
            (Some(c), None) => CHAR_KEYS.iter().find(|(_, k)| *k == c).map(|(key, _)| *key),
            _ => None,
        };
        let key = named.or(single).ok_or_else(|| {
            if rest.is_empty() {
                "an empty key".into()
            } else {
                format!(
                    "unknown key {rest:?} in {s:?}; keys are a-z, 0-9, - = [ ] \\ ; ' , . / `, esc, tab, enter, \
                     backspace, delete, insert, space, up, down, left, right, home, end, pageup, pagedown and \
                     f1-f12, after ctrl-, alt- or shift-"
                )
            }
        })?;
        Ok(Self { key, mods })
    }
}

impl std::fmt::Display for KeyChord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (on, prefix) in [(self.mods.ctrl, "ctrl-"), (self.mods.alt, "alt-"), (self.mods.shift, "shift-")] {
            if on {
                f.write_str(prefix)?;
            }
        }
        if let Some((_, names)) = NAMED_KEYS.iter().find(|(key, _)| *key == self.key) {
            return f.write_str(names[0]);
        }
        match CHAR_KEYS.iter().find(|(key, _)| *key == self.key) {
            Some((_, c)) => write!(f, "{c}"),
            None => f.write_str("unidentified"),
        }
    }
}

/// 一项按键的写法：一个组合键（见 `KeyChord`），后面可以跟 `*N` 表示连按 N 下（`down*3`，
/// N 从 1 到 `MAX_KEY_REPEAT`）。返回展开后的每一下。
pub fn parse_keys(spec: &str) -> Result<Vec<KeyChord>, String> {
    // `*` 不是哪个键的名字，最后一个 `*` 后面是次数。
    let (chord, times) = match spec.rsplit_once('*') {
        Some((chord, times)) => {
            let times = times
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=MAX_KEY_REPEAT).contains(n))
                .ok_or_else(|| format!("{spec:?}: the count after * is 1 to {MAX_KEY_REPEAT}"))?;
            (chord, times)
        }
        None => (spec, 1),
    };
    let chord: KeyChord = chord.parse()?;
    Ok(vec![chord; times as usize])
}
