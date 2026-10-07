//! 在共用的数据类型和 libghostty 的类型之间转换：对外只用前者，调 libghostty 时才换。

use libghostty_vt::{key, style::RgbColor, terminal::CursorStyle};
use runode_shared_types::{
    color::Rgb,
    input::{Key, Mods},
    settings,
};

pub(super) fn rgb(color: RgbColor) -> Rgb {
    Rgb(color.r, color.g, color.b)
}

pub(crate) fn ghostty_rgb(Rgb(r, g, b): Rgb) -> RgbColor {
    RgbColor { r, g, b }
}

pub(crate) fn ghostty_cursor_style(style: settings::CursorStyle) -> CursorStyle {
    match style {
        settings::CursorStyle::Block => CursorStyle::Block,
        settings::CursorStyle::BlockHollow => CursorStyle::BlockHollow,
        settings::CursorStyle::Bar => CursorStyle::Bar,
        settings::CursorStyle::Underline => CursorStyle::Underline,
    }
}

pub(crate) fn ghostty_mods(mods: Mods) -> key::Mods {
    let mut out = key::Mods::empty();
    for (on, flag) in [
        (mods.shift, key::Mods::SHIFT),
        (mods.ctrl, key::Mods::CTRL),
        (mods.alt, key::Mods::ALT),
        (mods.right_alt, key::Mods::ALT_SIDE),
    ] {
        if on {
            out |= flag;
        }
    }
    out
}

/// 两边的枚举同名，只写一遍名字；生成的是一个 `match`，`Key` 加了变体这里照样编译不过。
macro_rules! same_name_keys {
    ($($name:ident),* $(,)?) => {
        pub(crate) fn ghostty_key(key: Key) -> key::Key {
            match key {
                $(Key::$name => key::Key::$name,)*
            }
        }
    };
}

same_name_keys! {
    Unidentified, A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9, Minus,
    Equal, BracketLeft, BracketRight, Backslash, Semicolon, Quote, Comma, Period, Slash,
    Backquote, Space, Enter, Tab, Backspace, Escape, Delete, Insert, Home, End, PageUp,
    PageDown, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, F1, F2, F3, F4, F5, F6, F7, F8, F9,
    F10, F11, F12,
}
