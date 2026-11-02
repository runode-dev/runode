//! Mutter 和 xdg-desktop-portal 的远程桌面接口都按绝对坐标、evdev 的鼠标键码、离散的滚动格数和 keysym 注入：
//! 把 `ScreenInput` 拆成这几样（`RemoteInput`）的规矩只写在这里。

use std::time::Duration;

use runode_types::{input::MouseButton, screen::ScreenInput};

use super::keysym;

/// 能经远程桌面接口注入的一块屏幕。
pub(super) trait RemoteInput {
    type Error;

    /// 绝对坐标的范围：屏幕的宽高。
    fn extent(&self) -> (u32, u32);
    /// 指针挪到 `(x, y)`，按 `extent` 算。
    fn pointer_to(&self, x: f64, y: f64) -> Result<(), Self::Error>;
    /// evdev 键码是 `code` 的鼠标键按下或松开。
    fn button(&self, code: i32, down: bool) -> Result<(), Self::Error>;
    /// 滚 `steps` 格，朝下、朝右为正。
    fn scroll(&self, vertical: bool, steps: i32) -> Result<(), Self::Error>;
    fn keysym(&self, keysym: u32, down: bool) -> Result<(), Self::Error>;
}

/// evdev 里鼠标键的键码。
pub(super) fn button_code(button: MouseButton) -> i32 {
    match button {
        MouseButton::Left => 0x110,
        MouseButton::Right => 0x111,
        MouseButton::Middle => 0x112,
    }
}

/// 把 `input` 注入 `target`。鼠标键和组合键像真键盘一样先按下修饰键、做完再倒着松开。
pub(super) fn inject<T: RemoteInput>(target: &T, input: &ScreenInput) -> Result<(), T::Error> {
    let keys = |keysyms: &[u32], down: bool| -> Result<(), T::Error> {
        let ordered: Vec<u32> = if down { keysyms.to_vec() } else { keysyms.iter().rev().copied().collect() };
        ordered.into_iter().try_for_each(|keysym| target.keysym(keysym, down))
    };
    let move_to = |x: f64, y: f64| {
        let (width, height) = target.extent();
        let (width, height) = (f64::from(width), f64::from(height));
        target.pointer_to(x.clamp(0.0, 1.0) * (width - 1.0).max(0.0), y.clamp(0.0, 1.0) * (height - 1.0).max(0.0))
    };
    match input {
        ScreenInput::MouseMove { x, y } => move_to(*x, *y),
        ScreenInput::MouseButton { button, down, x, y, mods } => {
            move_to(*x, *y)?;
            let held = if *down { keysym::modifiers(*mods) } else { Vec::new() };
            keys(&held, true)?;
            target.button(button_code(*button), *down)?;
            keys(&held, false)
        }
        ScreenInput::Scroll { dx, dy } => {
            // 正的 `dy` 是内容往下走、往上滚，接口的格数朝下为正；横向同理。
            for (vertical, delta) in [(true, *dy), (false, *dx)] {
                if delta != 0 {
                    target.scroll(vertical, -delta.signum() * (delta.abs() / super::POINTS_PER_NOTCH).max(1))?;
                }
            }
            Ok(())
        }
        ScreenInput::Key { key, mods } => {
            let Some(keysym) = keysym::of_key(*key) else { return Ok(()) };
            let held = keysym::modifiers(*mods);
            keys(&held, true)?;
            target.keysym(keysym, true)?;
            target.keysym(keysym, false)?;
            keys(&held, false)
        }
        ScreenInput::Text { text } => text.chars().filter_map(keysym::of_char).try_for_each(|keysym| {
            target.keysym(keysym, true)?;
            target.keysym(keysym, false)
        }),
    }
}

/// `duration` 以后完成，在一个睡着的线程上计时。
pub(super) async fn sleep(duration: Duration) {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(duration);
        let _ = tx.send(());
    });
    let _ = rx.await;
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use runode_types::screen::ScreenMods;

    use super::*;

    #[derive(Default)]
    struct Recorder(RefCell<Vec<String>>);

    impl RemoteInput for Recorder {
        type Error = ();

        fn extent(&self) -> (u32, u32) {
            (101, 51)
        }

        fn pointer_to(&self, x: f64, y: f64) -> Result<(), ()> {
            self.0.borrow_mut().push(format!("move {x} {y}"));
            Ok(())
        }

        fn button(&self, code: i32, down: bool) -> Result<(), ()> {
            self.0.borrow_mut().push(format!("button {code:#x} {down}"));
            Ok(())
        }

        fn scroll(&self, vertical: bool, steps: i32) -> Result<(), ()> {
            self.0.borrow_mut().push(format!("scroll {vertical} {steps}"));
            Ok(())
        }

        fn keysym(&self, keysym: u32, down: bool) -> Result<(), ()> {
            self.0.borrow_mut().push(format!("key {keysym:#x} {down}"));
            Ok(())
        }
    }

    #[test]
    fn clicks_hold_modifiers_around_the_button() {
        let target = Recorder::default();
        let mods = ScreenMods { shift: true, ..ScreenMods::default() };
        let click = ScreenInput::MouseButton { button: MouseButton::Left, down: true, x: 2.0, y: 0.5, mods };
        inject(&target, &click).unwrap();
        assert_eq!(
            target.0.take(),
            ["move 100 25", "key 0xffe1 true", "button 0x110 true", "key 0xffe1 false"].map(String::from)
        );
    }

    #[test]
    fn scrolls_whole_notches_against_the_content() {
        let target = Recorder::default();
        inject(&target, &ScreenInput::Scroll { dx: 0, dy: 2 * super::super::POINTS_PER_NOTCH }).unwrap();
        inject(&target, &ScreenInput::Scroll { dx: -1, dy: 0 }).unwrap();
        assert_eq!(target.0.take(), ["scroll true -2", "scroll false 1"].map(String::from));
    }
}
