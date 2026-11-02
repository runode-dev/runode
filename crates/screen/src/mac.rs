//! macOS 上的远程桌面：ScreenCaptureKit 采集一块屏幕，VideoToolbox 编成硬件 HEVC 或 H.264（`Capture`）、把收到的
//! 画面解回来（`Decoder`），CoreGraphics 注入键鼠（`Injector`）、列屏幕、换显示模式、查锁屏和授权。

mod awake;
mod capture;
mod decoder;
mod encoder;
mod input;
mod mode;

pub use awake::KeepAwake;
pub use capture::{Capture, Keyframes};
pub use decoder::{Decoded, Decoder, PixelBuffer};
pub use input::Injector;
pub(crate) use mode::{apply, current, modes, reset};

use crate::{Display, Permissions};

pub(crate) fn main_display() -> Display {
    with_id(objc2_core_graphics::CGMainDisplayID())
}

pub(crate) fn displays() -> Vec<Display> {
    use objc2_core_graphics::{CGDisplayMirrorsDisplay, CGGetActiveDisplayList, CGMainDisplayID};
    const MAX: usize = 16;
    let mut ids = [0u32; MAX];
    let mut count = 0u32;
    // SAFETY: 输出参数指向本地数组和变量，长度如实给出。
    let status = unsafe { CGGetActiveDisplayList(MAX as u32, ids.as_mut_ptr(), &mut count) };
    let main = CGMainDisplayID();
    let mut displays: Vec<Display> = if status.0 == 0 {
        ids[..count as usize].iter().filter(|&&id| CGDisplayMirrorsDisplay(id) == 0).map(|&id| with_id(id)).collect()
    } else {
        Vec::new()
    };
    displays.sort_by_key(|display| display.id != main);
    if displays.is_empty() {
        displays.push(with_id(main));
    }
    displays
}

fn with_id(id: u32) -> Display {
    use objc2_core_graphics::{CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode};
    let bounds = CGDisplayBounds(id);
    let mode = CGDisplayCopyDisplayMode(id);
    let (width, height) = (bounds.size.width as u32, bounds.size.height as u32);
    let pixel_width = CGDisplayMode::pixel_width(mode.as_deref()) as u32;
    let pixel_height = CGDisplayMode::pixel_height(mode.as_deref()) as u32;
    Display { id, width, height, pixel_width: pixel_width.max(width), pixel_height: pixel_height.max(height) }
}

pub(crate) fn screen_locked() -> bool {
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
    let Some(session) = objc2_core_graphics::CGSessionCopyCurrentDictionary() else { return false };
    // SAFETY: 会话字典的键都是字符串，值按类型再查。
    let session: CFRetained<CFDictionary<CFString, CFType>> = unsafe { CFRetained::cast_unchecked(session) };
    session
        .get(&CFString::from_static_str("CGSSessionScreenIsLocked"))
        .and_then(|value| value.downcast::<CFBoolean>().ok())
        .is_some_and(|value| value.as_bool())
}

pub(crate) fn permissions() -> Permissions {
    use objc2_core_graphics::{CGPreflightPostEventAccess, CGPreflightScreenCaptureAccess};

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }

    // 「辅助功能」和「发送事件」（`CGRequestPostEventAccess` 要的）是 TCC 里的两项，有哪项都能注入键鼠。
    // `AXIsProcessTrusted` 每次现查，`CGPreflightPostEventAccess` 在进程里缓存结果、用户改了开关也不更新。
    // SAFETY: 无参数，只读系统的授权状态。
    let post_event = unsafe { AXIsProcessTrusted() } != 0 || CGPreflightPostEventAccess();
    Permissions { screen_capture: CGPreflightScreenCaptureAccess(), post_event }
}

/// 「辅助功能」用带提示的 `AXIsProcessTrustedWithOptions` 请求，它弹的对话框带「打开系统设置」，也会把程序
/// 列进辅助功能的列表；`CGRequestPostEventAccess` 在新系统上不弹框，只记一条被拒的授权。
/// `AXIsProcessTrustedWithOptions` 每调一次都弹框，一个进程里只带提示调一次。
pub(crate) fn request_permissions() -> Permissions {
    use std::sync::atomic::{AtomicBool, Ordering};

    use objc2_core_foundation::{CFBoolean, CFDictionary, CFString, CFType};
    use objc2_core_graphics::CGRequestScreenCaptureAccess;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrustedWithOptions(options: &CFDictionary) -> u8;
    }

    static PROMPTED: AtomicBool = AtomicBool::new(false);
    let now = permissions();
    let post_event = now.post_event
        || (!PROMPTED.swap(true, Ordering::Relaxed) && {
            let key = CFString::from_static_str("AXTrustedCheckOptionPrompt");
            let value: &CFType = CFBoolean::new(true);
            let options = CFDictionary::<CFString, CFType>::from_slices(&[&*key], &[value]);
            // SAFETY: 选项字典的键值是这个函数认的类型。
            unsafe { AXIsProcessTrustedWithOptions(options.as_opaque()) != 0 }
        });
    Permissions { screen_capture: now.screen_capture || CGRequestScreenCaptureAccess(), post_event }
}
