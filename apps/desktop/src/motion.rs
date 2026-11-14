//! 跟着系统的「减少动态效果」设置：读到的值交给 `ui::spinner::ReduceMotion`，会动的状态标记据此停下。

use gpui::App;

use crate::ui::spinner::ReduceMotion;

/// 重新读一次系统设置，变了就换掉 `ReduceMotion`、让所有窗口重画。启动时和窗口每次成为前台时调。
pub fn sync(cx: &mut App) {
    let reduce = system_reduces_motion();
    if cx.try_global::<ReduceMotion>().is_some_and(|current| current.0 == reduce) {
        return;
    }
    cx.set_global(ReduceMotion(reduce));
    cx.refresh_windows();
}

#[cfg(target_os = "macos")]
fn system_reduces_motion() -> bool {
    use objc2::msg_send;
    use objc2_app_kit::NSWorkspace;

    let workspace = NSWorkspace::sharedWorkspace();
    // SAFETY: `accessibilityDisplayShouldReduceMotion` 是 NSWorkspace 不带参数、返回 BOOL 的实例方法，
    // 哪个线程上都能调。
    unsafe { msg_send![&*workspace, accessibilityDisplayShouldReduceMotion] }
}

#[cfg(windows)]
fn system_reduces_motion() -> bool {
    use windows_sys::{
        Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW},
        core::BOOL,
    };

    let mut animate: BOOL = 1;
    // SAFETY: `SPI_GETCLIENTAREAANIMATION` 往 `pvparam` 指着的 BOOL 里写一次，`animate` 在调用期间一直有效。
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animate).cast(), 0) };
    ok != 0 && animate == 0
}

/// shortcut: Linux 不读桌面的动画设置，有人要再读 gsettings 的 enable-animations
#[cfg(not(any(target_os = "macos", windows)))]
fn system_reduces_motion() -> bool {
    false
}
