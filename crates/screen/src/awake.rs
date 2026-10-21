//! 有人在远程看时让屏幕亮着：显示器熄了以后 ScreenCaptureKit 找不到屏幕，采集开不起来、开着的也会停；
//! 熄着的屏幕也不在 `Display::all` 里。

use std::{
    thread,
    time::{Duration, Instant},
};

use objc2_core_foundation::CFString;
use objc2_core_graphics::{CGDisplayIsAsleep, CGMainDisplayID};

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(kind: &CFString, level: u32, name: &CFString, id: *mut u32) -> i32;
    fn IOPMAssertionDeclareUserActivity(name: &CFString, kind: u32, id: *mut u32) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

/// `kIOPMAssertionLevelOn`。
const LEVEL_ON: u32 = 255;
/// `kIOPMUserActiveLocal`：当成有人在本机动了一下，熄着的显示器亮起来。
const USER_ACTIVE_LOCAL: u32 = 0;
/// 唤醒显示器后最多等它这么久亮起来。
const WAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// 显示器刚亮时 ScreenCaptureKit 还要一会儿才看见它。
const WAKE_SETTLE: Duration = Duration::from_millis(300);

/// 拿着时显示器不因空闲熄掉；丢掉时放开。
pub struct KeepAwake {
    assertions: Vec<u32>,
}

impl KeepAwake {
    /// 唤醒熄着的显示器、等它亮起来（最多 `WAKE_TIMEOUT`），并拦住空闲熄屏。
    pub fn acquire() -> Self {
        let name = CFString::from_static_str("runode remote desktop");
        let (mut activity, mut prevent) = (0u32, 0u32);
        // SAFETY: 字符串在调用期间有效，输出参数指向本地变量。
        let woke = unsafe { IOPMAssertionDeclareUserActivity(&name, USER_ACTIVE_LOCAL, &mut activity) };
        // SAFETY: 同上。
        let held = unsafe {
            IOPMAssertionCreateWithName(
                &CFString::from_static_str("PreventUserIdleDisplaySleep"),
                LEVEL_ON,
                &name,
                &mut prevent,
            )
        };
        let assertions: Vec<u32> = [(woke, activity), (held, prevent)]
            .into_iter()
            .filter(|&(status, _)| status == 0)
            .map(|(_, id)| id)
            .collect();
        if held != 0 {
            tracing::warn!("cannot keep the display awake ({held})");
        }
        if CGDisplayIsAsleep(CGMainDisplayID()) {
            let deadline = Instant::now() + WAKE_TIMEOUT;
            while CGDisplayIsAsleep(CGMainDisplayID()) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(50));
            }
            thread::sleep(WAKE_SETTLE);
        }
        Self { assertions }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        for &id in &self.assertions {
            // SAFETY: 这些编号是上面拿到的，只放一次。
            unsafe { IOPMAssertionRelease(id) };
        }
    }
}
