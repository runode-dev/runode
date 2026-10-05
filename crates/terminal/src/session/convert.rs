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

pub(super) fn ghostty_mods(mods: Mods) -> key::Mods {
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

pub(super) fn ghostty_key(key: Key) -> key::Key {
    match key {
        Key::Unidentified => key::Key::Unidentified,
        Key::A => key::Key::A,
        Key::B => key::Key::B,
        Key::C => key::Key::C,
        Key::D => key::Key::D,
        Key::E => key::Key::E,
        Key::F => key::Key::F,
        Key::G => key::Key::G,
        Key::H => key::Key::H,
        Key::I => key::Key::I,
        Key::J => key::Key::J,
        Key::K => key::Key::K,
        Key::L => key::Key::L,
        Key::M => key::Key::M,
        Key::N => key::Key::N,
        Key::O => key::Key::O,
        Key::P => key::Key::P,
        Key::Q => key::Key::Q,
        Key::R => key::Key::R,
        Key::S => key::Key::S,
        Key::T => key::Key::T,
        Key::U => key::Key::U,
        Key::V => key::Key::V,
        Key::W => key::Key::W,
        Key::X => key::Key::X,
        Key::Y => key::Key::Y,
        Key::Z => key::Key::Z,
        Key::Digit0 => key::Key::Digit0,
        Key::Digit1 => key::Key::Digit1,
        Key::Digit2 => key::Key::Digit2,
        Key::Digit3 => key::Key::Digit3,
        Key::Digit4 => key::Key::Digit4,
        Key::Digit5 => key::Key::Digit5,
        Key::Digit6 => key::Key::Digit6,
        Key::Digit7 => key::Key::Digit7,
        Key::Digit8 => key::Key::Digit8,
        Key::Digit9 => key::Key::Digit9,
        Key::Minus => key::Key::Minus,
        Key::Equal => key::Key::Equal,
        Key::BracketLeft => key::Key::BracketLeft,
        Key::BracketRight => key::Key::BracketRight,
        Key::Backslash => key::Key::Backslash,
        Key::Semicolon => key::Key::Semicolon,
        Key::Quote => key::Key::Quote,
        Key::Comma => key::Key::Comma,
        Key::Period => key::Key::Period,
        Key::Slash => key::Key::Slash,
        Key::Backquote => key::Key::Backquote,
        Key::Space => key::Key::Space,
        Key::Enter => key::Key::Enter,
        Key::Tab => key::Key::Tab,
        Key::Backspace => key::Key::Backspace,
        Key::Escape => key::Key::Escape,
        Key::Delete => key::Key::Delete,
        Key::Insert => key::Key::Insert,
        Key::Home => key::Key::Home,
        Key::End => key::Key::End,
        Key::PageUp => key::Key::PageUp,
        Key::PageDown => key::Key::PageDown,
        Key::ArrowUp => key::Key::ArrowUp,
        Key::ArrowDown => key::Key::ArrowDown,
        Key::ArrowLeft => key::Key::ArrowLeft,
        Key::ArrowRight => key::Key::ArrowRight,
        Key::F1 => key::Key::F1,
        Key::F2 => key::Key::F2,
        Key::F3 => key::Key::F3,
        Key::F4 => key::Key::F4,
        Key::F5 => key::Key::F5,
        Key::F6 => key::Key::F6,
        Key::F7 => key::Key::F7,
        Key::F8 => key::Key::F8,
        Key::F9 => key::Key::F9,
        Key::F10 => key::Key::F10,
        Key::F11 => key::Key::F11,
        Key::F12 => key::Key::F12,
    }
}
