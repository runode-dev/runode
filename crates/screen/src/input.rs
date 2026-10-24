//! 把手机发来的 `ScreenInput` 变成系统的鼠标和键盘事件（`CGEvent`），发到 HID 层，程序收到的和真的
//! 键鼠一样。没有注入事件的授权时系统悄悄丢掉，见 `permissions`。

use std::time::{Duration, Instant};

use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGEventField, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton,
    CGScrollEventUnit,
};
use runode_types::{
    input::{Key, MouseButton},
    screen::{ScreenInput, ScreenMods},
};

/// 同一个键在这么久以内、`CLICK_SLOP` 点以内再按下算连击，系统据此认双击和三击。
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
const CLICK_SLOP: f64 = 4.0;
/// 一个键盘事件最多带这么多个 UTF-16 码元的文字，再长的拆成几个事件。
const TEXT_CHUNK: usize = 20;

/// 往一块屏幕上注入输入，记着按住的鼠标键（挪动时发拖动）和连击。丢掉时在最后的位置松开还按着的
/// 鼠标键：断线或换屏时手机来不及发松开，不松就一直按着。
pub struct Injector {
    display: u32,
    /// 按下的先后排。
    pressed: Vec<MouseButton>,
    /// 最后一个鼠标事件的全局坐标。
    at: CGPoint,
    clicks: Clicks,
}

impl Injector {
    /// `display` 是 `ScreenInput` 的坐标所在的屏幕，和采集的是同一块。
    pub fn new(display: u32) -> Self {
        Self { display, pressed: Vec::new(), at: CGPoint { x: 0.0, y: 0.0 }, clicks: Clicks::default() }
    }

    pub fn inject(&mut self, input: &ScreenInput) {
        match input {
            ScreenInput::MouseMove { x, y } => {
                let held = self.pressed.first().copied();
                let kind = match held {
                    None => CGEventType::MouseMoved,
                    Some(MouseButton::Left) => CGEventType::LeftMouseDragged,
                    Some(MouseButton::Right) => CGEventType::RightMouseDragged,
                    Some(MouseButton::Middle) => CGEventType::OtherMouseDragged,
                };
                self.at = self.point(*x, *y);
                post(CGEvent::new_mouse_event(None, kind, self.at, held.map_or(CGMouseButton::Left, cg_button)));
            }
            ScreenInput::MouseButton { button, down, x, y, mods } => {
                let at = self.point(*x, *y);
                let clicks =
                    if *down { self.clicks.press(*button, (at.x, at.y), Instant::now()) } else { self.clicks.count };
                self.press(*button, *down);
                self.at = at;
                let event = CGEvent::new_mouse_event(None, button_event(*button, *down), at, cg_button(*button));
                CGEvent::set_integer_value_field(event.as_deref(), CGEventField::MouseEventClickState, clicks);
                CGEvent::set_flags(event.as_deref(), flags(*mods));
                post(event);
            }
            ScreenInput::Scroll { dx, dy } => {
                post(CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 2, *dy, *dx, 0));
            }
            ScreenInput::Key { key, mods } => {
                let Some(code) = key_code(*key) else { return };
                // 像真的键盘一样先按下修饰键、最后松开：有的程序（锁屏的密码框也是）看修饰键本身有没有按着，
                // 不只看事件上的标志。
                let held = modifier_keys(*mods);
                let mut flags = CGEventFlags::empty();
                for &(modifier, flag) in &held {
                    flags |= flag;
                    post_key(modifier, true, flags);
                }
                post_key(code, true, flags);
                post_key(code, false, flags);
                for &(modifier, flag) in held.iter().rev() {
                    flags.remove(flag);
                    post_key(modifier, false, flags);
                }
            }
            ScreenInput::Text { text } => {
                for chunk in text_chunks(text) {
                    for down in [true, false] {
                        let event = CGEvent::new_keyboard_event(None, 0, down);
                        // SAFETY: 指针和长度来自同一个切片，调用期间有效。
                        unsafe {
                            CGEvent::keyboard_set_unicode_string(event.as_deref(), chunk.len() as _, chunk.as_ptr())
                        };
                        post(event);
                    }
                }
            }
        }
    }

    fn point(&self, x: f64, y: f64) -> CGPoint {
        to_screen(CGDisplayBounds(self.display), x, y)
    }

    fn press(&mut self, button: MouseButton, down: bool) {
        self.pressed.retain(|&held| held != button);
        if down {
            self.pressed.push(button);
        }
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        for button in std::mem::take(&mut self.pressed) {
            post(CGEvent::new_mouse_event(None, button_event(button, false), self.at, cg_button(button)));
        }
    }
}

fn button_event(button: MouseButton, down: bool) -> CGEventType {
    match (button, down) {
        (MouseButton::Left, true) => CGEventType::LeftMouseDown,
        (MouseButton::Left, false) => CGEventType::LeftMouseUp,
        (MouseButton::Right, true) => CGEventType::RightMouseDown,
        (MouseButton::Right, false) => CGEventType::RightMouseUp,
        (MouseButton::Middle, true) => CGEventType::OtherMouseDown,
        (MouseButton::Middle, false) => CGEventType::OtherMouseUp,
    }
}

/// 按 `TEXT_CHUNK` 分块，不把一个字符的代理对拆到两块里。
fn text_chunks(text: &str) -> Vec<Vec<u16>> {
    let mut chunks: Vec<Vec<u16>> = Vec::new();
    for c in text.chars() {
        let mut buf = [0; 2];
        let units = c.encode_utf16(&mut buf);
        match chunks.last_mut() {
            Some(chunk) if chunk.len() + units.len() <= TEXT_CHUNK => chunk.extend_from_slice(units),
            _ => chunks.push(units.to_vec()),
        }
    }
    chunks
}

fn post(event: Option<objc2_core_foundation::CFRetained<CGEvent>>) {
    CGEvent::post(CGEventTapLocation::HIDEventTap, event.as_deref());
}

/// 屏幕上的比例换成全局坐标（点），超出屏幕的贴到边上。
fn to_screen(bounds: CGRect, x: f64, y: f64) -> CGPoint {
    CGPoint {
        x: bounds.origin.x + x.clamp(0.0, 1.0) * (bounds.size.width - 1.0).max(0.0),
        y: bounds.origin.y + y.clamp(0.0, 1.0) * (bounds.size.height - 1.0).max(0.0),
    }
}

fn post_key(code: u16, down: bool, flags: CGEventFlags) {
    let event = CGEvent::new_keyboard_event(None, code, down);
    CGEvent::set_flags(event.as_deref(), flags);
    post(event);
}

/// 按着的修饰键各自的键码（`kVK_Command` 这些）和标志。
fn modifier_keys(mods: ScreenMods) -> Vec<(u16, CGEventFlags)> {
    [
        (mods.cmd, 0x37, CGEventFlags::MaskCommand),
        (mods.ctrl, 0x3B, CGEventFlags::MaskControl),
        (mods.alt, 0x3A, CGEventFlags::MaskAlternate),
        (mods.shift, 0x38, CGEventFlags::MaskShift),
    ]
    .into_iter()
    .filter_map(|(held, code, flag)| held.then_some((code, flag)))
    .collect()
}

fn cg_button(button: MouseButton) -> CGMouseButton {
    match button {
        MouseButton::Left => CGMouseButton::Left,
        MouseButton::Right => CGMouseButton::Right,
        MouseButton::Middle => CGMouseButton::Center,
    }
}

fn flags(mods: ScreenMods) -> CGEventFlags {
    let mut flags = CGEventFlags::empty();
    flags.set(CGEventFlags::MaskShift, mods.shift);
    flags.set(CGEventFlags::MaskControl, mods.ctrl);
    flags.set(CGEventFlags::MaskAlternate, mods.alt);
    flags.set(CGEventFlags::MaskCommand, mods.cmd);
    flags
}

#[derive(Default)]
struct Clicks {
    last: Option<(MouseButton, Instant, (f64, f64))>,
    count: i64,
}

impl Clicks {
    /// 按下一次，返回这是第几连击。
    fn press(&mut self, button: MouseButton, at: (f64, f64), now: Instant) -> i64 {
        let again = self.last.is_some_and(|(last, when, (x, y))| {
            last == button
                && now.duration_since(when) <= DOUBLE_CLICK
                && (at.0 - x).abs() <= CLICK_SLOP
                && (at.1 - y).abs() <= CLICK_SLOP
        });
        self.count = if again { self.count + 1 } else { 1 };
        self.last = Some((button, now, at));
        self.count
    }
}

/// macOS 的虚拟键码（`kVK_*`），按美式布局的键位。
fn key_code(key: Key) -> Option<u16> {
    Some(match key {
        Key::Unidentified => return None,
        Key::A => 0x00,
        Key::S => 0x01,
        Key::D => 0x02,
        Key::F => 0x03,
        Key::H => 0x04,
        Key::G => 0x05,
        Key::Z => 0x06,
        Key::X => 0x07,
        Key::C => 0x08,
        Key::V => 0x09,
        Key::B => 0x0B,
        Key::Q => 0x0C,
        Key::W => 0x0D,
        Key::E => 0x0E,
        Key::R => 0x0F,
        Key::Y => 0x10,
        Key::T => 0x11,
        Key::Digit1 => 0x12,
        Key::Digit2 => 0x13,
        Key::Digit3 => 0x14,
        Key::Digit4 => 0x15,
        Key::Digit6 => 0x16,
        Key::Digit5 => 0x17,
        Key::Equal => 0x18,
        Key::Digit9 => 0x19,
        Key::Digit7 => 0x1A,
        Key::Minus => 0x1B,
        Key::Digit8 => 0x1C,
        Key::Digit0 => 0x1D,
        Key::BracketRight => 0x1E,
        Key::O => 0x1F,
        Key::U => 0x20,
        Key::BracketLeft => 0x21,
        Key::I => 0x22,
        Key::P => 0x23,
        Key::Enter => 0x24,
        Key::L => 0x25,
        Key::J => 0x26,
        Key::Quote => 0x27,
        Key::K => 0x28,
        Key::Semicolon => 0x29,
        Key::Backslash => 0x2A,
        Key::Comma => 0x2B,
        Key::Slash => 0x2C,
        Key::N => 0x2D,
        Key::M => 0x2E,
        Key::Period => 0x2F,
        Key::Tab => 0x30,
        Key::Space => 0x31,
        Key::Backquote => 0x32,
        Key::Backspace => 0x33,
        Key::Escape => 0x35,
        Key::F5 => 0x60,
        Key::F6 => 0x61,
        Key::F7 => 0x62,
        Key::F3 => 0x63,
        Key::F8 => 0x64,
        Key::F9 => 0x65,
        Key::F11 => 0x67,
        Key::F10 => 0x6D,
        Key::F12 => 0x6F,
        Key::Insert => 0x72,
        Key::Home => 0x73,
        Key::PageUp => 0x74,
        Key::Delete => 0x75,
        Key::F4 => 0x76,
        Key::End => 0x77,
        Key::F2 => 0x78,
        Key::PageDown => 0x79,
        Key::F1 => 0x7A,
        Key::ArrowLeft => 0x7B,
        Key::ArrowRight => 0x7C,
        Key::ArrowDown => 0x7D,
        Key::ArrowUp => 0x7E,
    })
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    #[test]
    fn clicks_count_only_quick_nearby_presses_of_the_same_button() {
        let mut clicks = Clicks::default();
        let t = Instant::now();
        assert_eq!(clicks.press(MouseButton::Left, (10.0, 10.0), t), 1);
        assert_eq!(clicks.press(MouseButton::Left, (12.0, 11.0), t + Duration::from_millis(200)), 2);
        assert_eq!(clicks.press(MouseButton::Left, (12.0, 11.0), t + Duration::from_millis(400)), 3);
        assert_eq!(clicks.press(MouseButton::Right, (12.0, 11.0), t + Duration::from_millis(500)), 1);
        assert_eq!(clicks.press(MouseButton::Right, (40.0, 11.0), t + Duration::from_millis(600)), 1);
        assert_eq!(clicks.press(MouseButton::Right, (40.0, 11.0), t + Duration::from_secs(2)), 1);
    }

    /// 副屏的原点不在 (0, 0)，越界的比例贴边，不落到别的屏幕上。
    #[test]
    fn maps_fractions_into_the_display() {
        let bounds = CGRect { origin: CGPoint { x: -1000.0, y: 200.0 }, size: CGSize { width: 1001.0, height: 501.0 } };
        let p = to_screen(bounds, 0.5, 2.0);
        assert_eq!((p.x, p.y), (-500.0, 700.0));
        let p = to_screen(bounds, -1.0, 0.0);
        assert_eq!((p.x, p.y), (-1000.0, 200.0));
    }

    /// 松开一个键不影响别的键还按着，挪动时仍按拖动发。
    #[test]
    fn tracks_each_mouse_button_separately() {
        let mut injector = Injector::new(0);
        injector.press(MouseButton::Left, true);
        injector.press(MouseButton::Right, true);
        injector.press(MouseButton::Right, false);
        assert_eq!(injector.pressed, [MouseButton::Left]);
        injector.press(MouseButton::Left, false);
        assert!(injector.pressed.is_empty());
    }

    #[test]
    fn text_chunks_keep_surrogate_pairs_together() {
        let text = format!("{}😀a", "x".repeat(TEXT_CHUNK - 1));
        let chunks = text_chunks(&text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), TEXT_CHUNK - 1);
        assert_eq!(String::from_utf16(&chunks[1]).unwrap(), "😀a");
        assert_eq!(text_chunks(&"y".repeat(TEXT_CHUNK)).len(), 1);
        assert!(text_chunks("").is_empty());
    }

    #[test]
    fn key_codes_follow_the_us_layout() {
        assert_eq!(key_code(Key::A), Some(0x00));
        assert_eq!(key_code(Key::Enter), Some(0x24));
        assert_eq!(key_code(Key::ArrowUp), Some(0x7E));
        assert_eq!(key_code(Key::Unidentified), None);
    }
}
