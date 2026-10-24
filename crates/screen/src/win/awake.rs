//! 有人在远程看时让屏幕亮着、电脑不睡：熄了屏桌面复制就不出新画面。

use windows::Win32::{
    System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState},
    UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput},
};

/// 拿着时屏幕不因空闲熄掉、电脑不睡；丢掉时放开。系统按线程记这个要求，所以要在拿它的线程上丢掉。
pub struct KeepAwake(());

impl KeepAwake {
    /// 唤醒熄着的屏幕，并拦住空闲熄屏和睡眠。
    pub fn acquire() -> Self {
        // SAFETY: 只改本线程的电源要求。
        if unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED) }.0 == 0 {
            tracing::warn!("cannot keep the display awake");
        }
        // 当成鼠标动了一下，熄着的屏幕亮起来；挪过去再挪回来，指针不动。
        let nudge = |dx| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dwFlags: MOUSEEVENTF_MOVE, ..Default::default() } },
        };
        // SAFETY: 数组和长度来自同一个切片。
        unsafe { SendInput(&[nudge(1), nudge(-1)], size_of::<INPUT>() as i32) };
        Self(())
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        // SAFETY: 同 `acquire`。
        unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
    }
}
