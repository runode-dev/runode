//! X 的 keysym：X11、Mutter 和 portal 注入键盘都按它说。

use runode_types::{input::Key, screen::ScreenMods};

pub(crate) const SHIFT_L: u32 = 0xffe1;
pub(crate) const CONTROL_L: u32 = 0xffe3;
pub(crate) const ALT_L: u32 = 0xffe9;
pub(crate) const SUPER_L: u32 = 0xffeb;

/// 按着的修饰键各自的 keysym，按真键盘的习惯先 Ctrl、Alt、Shift，Command 当 Super。
pub(crate) fn modifiers(mods: ScreenMods) -> Vec<u32> {
    [(mods.ctrl, CONTROL_L), (mods.alt, ALT_L), (mods.shift, SHIFT_L), (mods.cmd, SUPER_L)]
        .into_iter()
        .filter_map(|(held, keysym)| held.then_some(keysym))
        .collect()
}

/// 键的 keysym，按美式布局的键位：字母取小写的，数字和符号取不按 Shift 的那个。
pub(crate) fn of_key(key: Key) -> Option<u32> {
    Some(match key {
        Key::Unidentified => return None,
        Key::A => 'a' as u32,
        Key::B => 'b' as u32,
        Key::C => 'c' as u32,
        Key::D => 'd' as u32,
        Key::E => 'e' as u32,
        Key::F => 'f' as u32,
        Key::G => 'g' as u32,
        Key::H => 'h' as u32,
        Key::I => 'i' as u32,
        Key::J => 'j' as u32,
        Key::K => 'k' as u32,
        Key::L => 'l' as u32,
        Key::M => 'm' as u32,
        Key::N => 'n' as u32,
        Key::O => 'o' as u32,
        Key::P => 'p' as u32,
        Key::Q => 'q' as u32,
        Key::R => 'r' as u32,
        Key::S => 's' as u32,
        Key::T => 't' as u32,
        Key::U => 'u' as u32,
        Key::V => 'v' as u32,
        Key::W => 'w' as u32,
        Key::X => 'x' as u32,
        Key::Y => 'y' as u32,
        Key::Z => 'z' as u32,
        Key::Digit0 => '0' as u32,
        Key::Digit1 => '1' as u32,
        Key::Digit2 => '2' as u32,
        Key::Digit3 => '3' as u32,
        Key::Digit4 => '4' as u32,
        Key::Digit5 => '5' as u32,
        Key::Digit6 => '6' as u32,
        Key::Digit7 => '7' as u32,
        Key::Digit8 => '8' as u32,
        Key::Digit9 => '9' as u32,
        Key::Minus => '-' as u32,
        Key::Equal => '=' as u32,
        Key::BracketLeft => '[' as u32,
        Key::BracketRight => ']' as u32,
        Key::Backslash => '\\' as u32,
        Key::Semicolon => ';' as u32,
        Key::Quote => '\'' as u32,
        Key::Comma => ',' as u32,
        Key::Period => '.' as u32,
        Key::Slash => '/' as u32,
        Key::Backquote => '`' as u32,
        Key::Space => ' ' as u32,
        Key::Enter => 0xff0d,
        Key::Tab => 0xff09,
        Key::Backspace => 0xff08,
        Key::Escape => 0xff1b,
        Key::Delete => 0xffff,
        Key::Insert => 0xff63,
        Key::Home => 0xff50,
        Key::End => 0xff57,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::ArrowLeft => 0xff51,
        Key::ArrowUp => 0xff52,
        Key::ArrowRight => 0xff53,
        Key::ArrowDown => 0xff54,
        Key::F1 => 0xffbe,
        Key::F2 => 0xffbf,
        Key::F3 => 0xffc0,
        Key::F4 => 0xffc1,
        Key::F5 => 0xffc2,
        Key::F6 => 0xffc3,
        Key::F7 => 0xffc4,
        Key::F8 => 0xffc5,
        Key::F9 => 0xffc6,
        Key::F10 => 0xffc7,
        Key::F11 => 0xffc8,
        Key::F12 => 0xffc9,
    })
}

/// 字符的 keysym：Latin-1 的可见字符就是码位，换行和制表是对应的键，别的 Unicode 字符是 `0x0100_0000` 加码位。
/// 控制字符没有 keysym。
pub(crate) fn of_char(ch: char) -> Option<u32> {
    let code = ch as u32;
    match ch {
        '\n' | '\r' => Some(0xff0d),
        '\t' => Some(0xff09),
        _ if ch.is_control() => None,
        _ if (0x20..=0x7e).contains(&code) || (0xa0..=0xff).contains(&code) => Some(code),
        _ => Some(0x0100_0000 | code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keys_and_characters() {
        assert_eq!(of_key(Key::A), Some(0x61));
        assert_eq!(of_key(Key::Enter), Some(0xff0d));
        assert_eq!(of_key(Key::Unidentified), None);
        assert_eq!(of_char('A'), Some(0x41));
        assert_eq!(of_char('é'), Some(0xe9));
        assert_eq!(of_char('中'), Some(0x0100_4e2d));
        assert_eq!(of_char('\n'), Some(0xff0d));
        assert_eq!(of_char('\u{7}'), None);
        let mods = ScreenMods { shift: true, ctrl: true, alt: false, cmd: true };
        assert_eq!(modifiers(mods), vec![CONTROL_L, SHIFT_L, SUPER_L]);
    }
}
