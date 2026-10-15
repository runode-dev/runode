//! 把 GPUI 的按键翻译成交给终端的按键事件。
//!
//! GPUI 用键帽上印的字符命名按键（`key`），另外单独给出平台实际输入的文本
//! （`key_char`）。终端要的是接近物理键位的键码加上这段文本，这样编码器
//! 才能按运行中程序的要求，选择传统编码、modifyOtherKeys 或 Kitty 编码。

use gpui::Keystroke;
use runode_types::input::{Key, KeyChord, KeyInput, Mods};

/// 终端完全不该收到的按键返回 `None`：Command 键留给应用自己的快捷键。
pub fn translate(keystroke: &Keystroke) -> Option<KeyInput> {
    let m = &keystroke.modifiers;
    if m.platform {
        return None;
    }
    let (key, unshifted) = key_code(&keystroke.key)?;

    let mods = Mods { shift: m.shift, ctrl: m.control, alt: m.alt, right_alt: m.alt && right_option_down() };

    // Control 组合键由按键本身编码；平台给出的文本是控制字符，编码器不能再原样发出。
    let text = keystroke
        .key_char
        .as_ref()
        .filter(|_| !m.control)
        .filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
        .cloned();

    // Shift 和 Option 改变了产出的文本时（a → A，s → ß）算作「已消耗」，
    // 免得编码器再把它们报告一次。
    let mut consumed_mods = Mods::default();
    if let Some(text) = &text
        && *text != unshifted.to_string()
    {
        consumed_mods.shift = m.shift;
        consumed_mods.alt = m.alt;
    }

    let layout_option_as_alt = m.alt && layout_option_as_alt();
    Some(KeyInput { key, mods, consumed_mods, unshifted, text, layout_option_as_alt })
}

/// 当前的键盘布局下 Option 默认当不当 Alt：美式和 ABC 这类布局 Option 打出的 ∫、π 很少用，
/// 当 Alt；别的布局（德语的 @、法语的 | 都靠 Option）不当。输入法开着时读的是它底下的键盘布局：
/// 苹果自带的拼音、五笔、仓颉、注音底下是几个不在设置里露面的 QWERTY 布局，一样当 Alt。
#[cfg(target_os = "macos")]
fn layout_option_as_alt() -> bool {
    use std::ffi::c_void;

    use objc2_foundation::NSString;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        #[allow(non_upper_case_globals)]
        static kTISPropertyInputSourceID: *const c_void;
        fn TISCopyCurrentKeyboardLayoutInputSource() -> *mut c_void;
        fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
        fn CFRelease(cf: *const c_void);
    }

    const LAYOUTS: &[&str] = &[
        "com.apple.keylayout.US",
        "com.apple.keylayout.USInternational-PC",
        "com.apple.keylayout.USInternational",
        "com.apple.keylayout.ABC",
        // 苹果中文输入法底下的布局。
        "com.apple.keylayout.PinyinKeyboard",
        "com.apple.keylayout.TraditionalPinyinKeyboard",
        "com.apple.keylayout.WubihuaKeyboard",
        "com.apple.keylayout.TraditionalWubihuaKeyboard",
        "com.apple.keylayout.CangjieKeyboard",
        "com.apple.keylayout.ZhuyinBopomofo",
        "com.apple.keylayout.ZhuyinEten",
    ];
    // SAFETY: 拿到的输入源按 Copy 规则归我们，用完释放；属性值归输入源所有，只在释放前读，
    // 它是 CFString，和 NSString 免费桥接。
    unsafe {
        let source = TISCopyCurrentKeyboardLayoutInputSource();
        if source.is_null() {
            return false;
        }
        let id = TISGetInputSourceProperty(source, kTISPropertyInputSourceID).cast::<NSString>();
        let us = id.as_ref().is_some_and(|id| LAYOUTS.contains(&id.to_string().as_str()));
        CFRelease(source);
        us
    }
}

#[cfg(not(target_os = "macos"))]
fn layout_option_as_alt() -> bool {
    false
}

/// 当前按下的是不是右 Option。GPUI 的修饰键不分左右，这里读正在分发的
/// NSEvent 里的设备相关位（`NX_DEVICERALTKEYMASK`），供 `macos-option-as-alt`
/// 区分左右键。
#[cfg(target_os = "macos")]
fn right_option_down() -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    const NX_DEVICERALTKEYMASK: usize = 0x40;
    MainThreadMarker::new().is_some_and(|mtm| {
        NSApplication::sharedApplication(mtm)
            .currentEvent()
            .is_some_and(|event| event.modifierFlags().0 & NX_DEVICERALTKEYMASK != 0)
    })
}

#[cfg(not(target_os = "macos"))]
fn right_option_down() -> bool {
    false
}

/// GPUI 键名对应的按键及其未按 Shift 时的字符：键名按 `KeyChord` 的写法认，不打字的键的字符是
/// `'\0'`。美式布局下需要 Shift 的符号，映射回产生它的那个键。
fn key_code(name: &str) -> Option<(Key, char)> {
    // 回车和 Tab 交给编码器的是它们打出的控制字符。
    match name {
        "enter" => return Some((Key::Enter, '\r')),
        "tab" => return Some((Key::Tab, '\t')),
        _ => {}
    }
    if let Some(key) = plain_key(name) {
        return Some((key, key.unshifted_char().unwrap_or('\0')));
    }
    let mut chars = name.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let base = match c {
        ')' => '0',
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        '~' => '`',
        other => other,
    };
    // 非美式布局的键：没有键码，但编码器仍会发送它的文本。
    Some(plain_key(base.encode_utf8(&mut [0; 4])).map_or((Key::Unidentified, c), |key| (key, base)))
}

/// 不带修饰键的键名对应的键。
fn plain_key(name: &str) -> Option<Key> {
    name.parse::<KeyChord>().ok().filter(|chord| chord.mods == Mods::default()).map(|chord| chord.key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn stroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke { modifiers, key: key.into(), key_char: key_char.map(Into::into) }
    }

    #[test]
    fn shifted_letter_consumes_shift() {
        let input = translate(&stroke("a", Some("A"), Modifiers::shift())).unwrap();
        assert_eq!(input.key, Key::A);
        assert_eq!(input.text.as_deref(), Some("A"));
        assert_eq!(input.consumed_mods, Mods { shift: true, ..Mods::default() });
        assert_eq!(input.unshifted, 'a');
    }

    #[test]
    fn control_drops_text() {
        let input = translate(&stroke("c", Some("c"), Modifiers::control())).unwrap();
        assert_eq!(input.key, Key::C);
        assert_eq!(input.text, None);
        assert!(input.mods.ctrl);
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

    #[test]
    fn key_names_map_to_us_keys() {
        assert_eq!(key_code("escape"), Some((Key::Escape, '\0')));
        assert_eq!(key_code("space"), Some((Key::Space, ' ')));
        assert_eq!(key_code("enter"), Some((Key::Enter, '\r')));
        assert_eq!(key_code("A"), Some((Key::A, 'a')));
        assert_eq!(key_code(")"), Some((Key::Digit0, '0')));
        assert_eq!(key_code("\""), Some((Key::Quote, '\'')));
        assert_eq!(key_code("é"), Some((Key::Unidentified, 'é')));
        assert_eq!(key_code("ctrl-a"), None);
    }
}
