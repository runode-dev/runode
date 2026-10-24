//! 把手机发来的 `ScreenInput` 变成系统的鼠标和键盘事件（`SendInput`），程序收到的和真的键鼠一样；
//! 双击由系统按两次按下的间隔自己认。

use runode_types::{
    input::{Key, MouseButton},
    screen::{ScreenInput, ScreenMods},
};
use windows::Win32::{
    Foundation::RECT,
    UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
            KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
            MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
            MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
            MOUSEINPUT, MapVirtualKeyW, SendInput, VIRTUAL_KEY,
        },
        WindowsAndMessaging::{
            GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
        },
    },
};

/// 滚轮转一格（`WHEEL_DELTA`）大致滚过的点数：系统默认一格滚三行。
const POINTS_PER_NOTCH: i32 = 48;
const WHEEL_DELTA: i32 = 120;

/// 往一块屏幕上注入输入，记着按住的鼠标键。丢掉时松开还按着的鼠标键：断线或换屏时手机来不及发松开，
/// 不松就一直按着。
pub struct Injector {
    /// 这块屏幕在虚拟桌面里的位置，物理像素；找不到这块屏幕时是主屏。
    rect: RECT,
    pressed: Vec<MouseButton>,
}

impl Injector {
    /// `display` 是 `ScreenInput` 的坐标所在的屏幕，和采集的是同一块。
    pub fn new(display: u32) -> Self {
        let rect = super::display::find(display).map(|output| output.rect).unwrap_or_else(|_| {
            let primary = crate::Display::main();
            RECT { left: 0, top: 0, right: primary.pixel_width as i32, bottom: primary.pixel_height as i32 }
        });
        Self { rect, pressed: Vec::new() }
    }

    pub fn inject(&mut self, input: &ScreenInput) {
        match input {
            ScreenInput::MouseMove { x, y } => send(&[self.mouse(*x, *y, MOUSE_EVENT_FLAGS(0))]),
            ScreenInput::MouseButton { button, down, x, y, mods } => {
                self.pressed.retain(|held| held != button);
                if *down {
                    self.pressed.push(*button);
                }
                let click = self.mouse(*x, *y, button_flag(*button, *down));
                // 按下时带着的修饰键（Shift 加点击之类）只在按下那一刻按住。
                let held = if *down { modifier_keys(*mods) } else { Vec::new() };
                let mut events: Vec<INPUT> = held.iter().map(|&vk| key(vk, true)).collect();
                events.push(click);
                events.extend(held.iter().rev().map(|&vk| key(vk, false)));
                send(&events);
            }
            ScreenInput::Scroll { dx, dy } => {
                // 正的 `dy` 是内容往下走，就是往上滚，滚轮的正方向；横向的正方向是往右滚，内容往左走。
                let wheel = |delta: i32, flag| INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            mouseData: (delta * WHEEL_DELTA / POINTS_PER_NOTCH) as u32,
                            dwFlags: flag,
                            ..Default::default()
                        },
                    },
                };
                let mut events = Vec::new();
                if *dy != 0 {
                    events.push(wheel(*dy, MOUSEEVENTF_WHEEL));
                }
                if *dx != 0 {
                    events.push(wheel(-*dx, MOUSEEVENTF_HWHEEL));
                }
                send(&events);
            }
            ScreenInput::Key { key: pressed, mods } => {
                let Some(vk) = virtual_key(*pressed) else { return };
                // 像真的键盘一样先按下修饰键、最后松开。
                let held = modifier_keys(*mods);
                let mut events: Vec<INPUT> = held.iter().map(|&vk| key(vk, true)).collect();
                events.extend([key(vk, true), key(vk, false)]);
                events.extend(held.iter().rev().map(|&vk| key(vk, false)));
                send(&events);
            }
            ScreenInput::Text { text } => {
                let events: Vec<INPUT> =
                    text.encode_utf16().flat_map(|unit| [false, true].map(|up| unicode(unit, up))).collect();
                send(&events);
            }
        }
    }

    /// 挪到屏幕上比例是 (`x`, `y`) 的地方的鼠标事件，再带上 `flags`。
    fn mouse(&self, x: f64, y: f64, flags: MOUSE_EVENT_FLAGS) -> INPUT {
        let (dx, dy) = to_virtual_desk(&self.rect, &virtual_screen(), x, y);
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK | flags,
                    ..Default::default()
                },
            },
        }
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        let events: Vec<INPUT> = std::mem::take(&mut self.pressed)
            .into_iter()
            .map(|button| INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: button_flag(button, false), ..Default::default() } },
            })
            .collect();
        send(&events);
    }
}

fn send(events: &[INPUT]) {
    if events.is_empty() {
        return;
    }
    // SAFETY: 数组和长度来自同一个切片。
    unsafe { SendInput(events, size_of::<INPUT>() as i32) };
}

/// 整个虚拟桌面（所有屏幕拼起来）的位置和大小，物理像素。
fn virtual_screen() -> RECT {
    // SAFETY: 只读系统指标。
    unsafe {
        let (left, top) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        RECT {
            left,
            top,
            right: left + GetSystemMetrics(SM_CXVIRTUALSCREEN),
            bottom: top + GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

/// 屏幕 `rect` 上的比例换成 `SendInput` 的绝对坐标：整个虚拟桌面 `desk` 映到 0 到 65535。越界的比例
/// 贴到这块屏幕的边上。
fn to_virtual_desk(rect: &RECT, desk: &RECT, x: f64, y: f64) -> (i32, i32) {
    let axis = |fraction: f64, start: i32, end: i32, desk_start: i32, desk_end: i32| {
        let pixel = start as f64 + fraction.clamp(0.0, 1.0) * f64::from((end - start - 1).max(0));
        let span = f64::from((desk_end - desk_start - 1).max(1));
        ((pixel - f64::from(desk_start)) * 65535.0 / span).round() as i32
    };
    (axis(x, rect.left, rect.right, desk.left, desk.right), axis(y, rect.top, rect.bottom, desk.top, desk.bottom))
}

fn button_flag(button: MouseButton, down: bool) -> MOUSE_EVENT_FLAGS {
    match (button, down) {
        (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
        (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
        (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
        (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
        (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
        (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
    }
}

/// 一个键按下或松开，带上它的扫描码：有的程序（游戏、远程桌面客户端）只看扫描码。
fn key(vk: u16, down: bool) -> INPUT {
    // SAFETY: 只查键盘布局。
    let scan = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) } as u16;
    let mut flags = if down { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP };
    if extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, ..Default::default() },
        },
    }
}

/// 不管键盘布局输入一个 UTF-16 码元。
fn unicode(unit: u16, up: bool) -> INPUT {
    let flags = if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE };
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wScan: unit, dwFlags: flags, ..Default::default() } },
    }
}

/// 扩展键：方向键和它们上面那六个键，以及 Windows 键，扫描码前要带 E0。
fn extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B)
}

/// 按着的修饰键各自的虚拟键码；Command 当 Windows 键。
fn modifier_keys(mods: ScreenMods) -> Vec<u16> {
    [(mods.ctrl, 0xA2), (mods.alt, 0xA4), (mods.shift, 0xA0), (mods.cmd, 0x5B)]
        .into_iter()
        .filter_map(|(held, vk)| held.then_some(vk))
        .collect()
}

/// Windows 的虚拟键码（`VK_*`），按美式布局的键位。
fn virtual_key(key: Key) -> Option<u16> {
    Some(match key {
        Key::Unidentified => return None,
        Key::A => 0x41,
        Key::B => 0x42,
        Key::C => 0x43,
        Key::D => 0x44,
        Key::E => 0x45,
        Key::F => 0x46,
        Key::G => 0x47,
        Key::H => 0x48,
        Key::I => 0x49,
        Key::J => 0x4A,
        Key::K => 0x4B,
        Key::L => 0x4C,
        Key::M => 0x4D,
        Key::N => 0x4E,
        Key::O => 0x4F,
        Key::P => 0x50,
        Key::Q => 0x51,
        Key::R => 0x52,
        Key::S => 0x53,
        Key::T => 0x54,
        Key::U => 0x55,
        Key::V => 0x56,
        Key::W => 0x57,
        Key::X => 0x58,
        Key::Y => 0x59,
        Key::Z => 0x5A,
        Key::Digit0 => 0x30,
        Key::Digit1 => 0x31,
        Key::Digit2 => 0x32,
        Key::Digit3 => 0x33,
        Key::Digit4 => 0x34,
        Key::Digit5 => 0x35,
        Key::Digit6 => 0x36,
        Key::Digit7 => 0x37,
        Key::Digit8 => 0x38,
        Key::Digit9 => 0x39,
        Key::Enter => 0x0D,
        Key::Tab => 0x09,
        Key::Space => 0x20,
        Key::Backspace => 0x08,
        Key::Escape => 0x1B,
        Key::Delete => 0x2E,
        Key::Insert => 0x2D,
        Key::Home => 0x24,
        Key::End => 0x23,
        Key::PageUp => 0x21,
        Key::PageDown => 0x22,
        Key::ArrowLeft => 0x25,
        Key::ArrowUp => 0x26,
        Key::ArrowRight => 0x27,
        Key::ArrowDown => 0x28,
        Key::F1 => 0x70,
        Key::F2 => 0x71,
        Key::F3 => 0x72,
        Key::F4 => 0x73,
        Key::F5 => 0x74,
        Key::F6 => 0x75,
        Key::F7 => 0x76,
        Key::F8 => 0x77,
        Key::F9 => 0x78,
        Key::F10 => 0x79,
        Key::F11 => 0x7A,
        Key::F12 => 0x7B,
        Key::Minus => 0xBD,
        Key::Equal => 0xBB,
        Key::BracketLeft => 0xDB,
        Key::BracketRight => 0xDD,
        Key::Backslash => 0xDC,
        Key::Semicolon => 0xBA,
        Key::Quote => 0xDE,
        Key::Comma => 0xBC,
        Key::Period => 0xBE,
        Key::Slash => 0xBF,
        Key::Backquote => 0xC0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 副屏在主屏左边、原点是负数时也映得对，越界的比例贴到这块屏幕的边上。
    #[test]
    fn maps_fractions_into_the_virtual_desk() {
        let desk = RECT { left: -1920, top: 0, right: 2560, bottom: 1440 };
        let left = RECT { left: -1920, top: 0, right: 0, bottom: 1080 };
        assert_eq!(to_virtual_desk(&left, &desk, 0.0, 0.0), (0, 0));
        assert_eq!(to_virtual_desk(&left, &desk, 2.0, 1.0), to_virtual_desk(&left, &desk, 1.0, 1.0));
        let (x, _) = to_virtual_desk(&left, &desk, 1.0, 0.0);
        assert_eq!(x, (1919.0 * 65535.0 / 4479.0f64).round() as i32);
    }

    #[test]
    fn virtual_keys_follow_the_us_layout() {
        assert_eq!(virtual_key(Key::A), Some(0x41));
        assert_eq!(virtual_key(Key::Enter), Some(0x0D));
        assert_eq!(virtual_key(Key::ArrowUp), Some(0x26));
        assert_eq!(virtual_key(Key::Unidentified), None);
        assert!(extended(0x26) && !extended(0x41));
    }
}
