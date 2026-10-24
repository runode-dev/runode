//! 远程桌面在电脑这边的部分：采集一块屏幕、编成低延迟的硬件 HEVC（`Capture`），把手机发来的
//! 鼠标和键盘输入注入系统（`Injector`）。传输和协议不在这里。
//!
//! macOS 上 ScreenCaptureKit 采集，VideoToolbox 编码，CoreGraphics 注入；采集要「屏幕录制」的授权，
//! 注入要「辅助功能」的授权，都按调用进程的责任进程记（`permissions`）。Windows 上 DXGI 桌面复制
//! 采集，Media Foundation 编码，`SendInput` 注入，不用授权。

#[cfg(target_os = "macos")]
mod awake;
#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
mod encoder;
#[cfg(target_os = "macos")]
mod input;
mod keys;
#[cfg(any(target_os = "macos", windows))]
mod nal;
#[cfg(windows)]
mod win;

#[cfg(target_os = "macos")]
pub use awake::KeepAwake;
#[cfg(target_os = "macos")]
pub use capture::{Capture, Keyframes};
#[cfg(target_os = "macos")]
pub use input::Injector;
pub use keys::key_strokes;
#[cfg(windows)]
pub use win::{Capture, Injector, KeepAwake, Keyframes};

pub use runode_types::screen::VideoFrame;

/// `Capture` 交出的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Frame(VideoFrame),
    /// 采集停了，不会再有帧：授权被收回、屏幕拔掉这类，带系统给的原因。
    Stopped(String),
}

/// 怎么采集。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// 采哪块屏幕，见 `Display`。
    pub display: u32,
    /// 画面的像素尺寸；和屏幕的宽高比不同时留黑边。
    pub width: u32,
    pub height: u32,
    /// 帧率上限。
    pub fps: u32,
    /// 平均码率，单位 bit/s。
    pub bitrate: u32,
}

/// 一块屏幕。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Display {
    /// macOS 上是 `CGDirectDisplayID`，Windows 上是显卡和它的输出的序号（`win::display`）。
    pub id: u32,
    /// 点数，`ScreenInput` 的坐标按它换算。
    pub width: u32,
    pub height: u32,
    /// 像素数，Retina 屏是点数的两倍，Windows 上按显示缩放比例算。
    pub pixel_width: u32,
    pub pixel_height: u32,
}

impl Display {
    /// 主屏（菜单栏所在的那块）。
    #[cfg(target_os = "macos")]
    pub fn main() -> Self {
        Self::with_id(objc2_core_graphics::CGMainDisplayID())
    }

    /// 开着的各块屏幕，主屏排第一；镜像出去的屏幕和它镜像的那块是同一个画面，不另列。
    #[cfg(target_os = "macos")]
    pub fn all() -> Vec<Self> {
        use objc2_core_graphics::{CGDisplayMirrorsDisplay, CGGetActiveDisplayList, CGMainDisplayID};
        const MAX: usize = 16;
        let mut ids = [0u32; MAX];
        let mut count = 0u32;
        // SAFETY: 输出参数指向本地数组和变量，长度如实给出。
        let status = unsafe { CGGetActiveDisplayList(MAX as u32, ids.as_mut_ptr(), &mut count) };
        let main = CGMainDisplayID();
        let mut displays: Vec<Self> = if status.0 == 0 {
            ids[..count as usize]
                .iter()
                .filter(|&&id| CGDisplayMirrorsDisplay(id) == 0)
                .map(|&id| Self::with_id(id))
                .collect()
        } else {
            Vec::new()
        };
        displays.sort_by_key(|display| display.id != main);
        if displays.is_empty() {
            displays.push(Self::with_id(main));
        }
        displays
    }

    /// 主屏（任务栏所在的那块）。
    #[cfg(windows)]
    pub fn main() -> Self {
        Self::all().swap_remove(0)
    }

    /// 开着的各块屏幕，主屏排第一；一块都列不出时是按系统报的主屏尺寸凑的一块。
    #[cfg(windows)]
    pub fn all() -> Vec<Self> {
        win::displays()
    }

    #[cfg(target_os = "macos")]
    fn with_id(id: u32) -> Self {
        use objc2_core_graphics::{CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayMode};
        let bounds = CGDisplayBounds(id);
        let mode = CGDisplayCopyDisplayMode(id);
        let (width, height) = (bounds.size.width as u32, bounds.size.height as u32);
        let pixel_width = CGDisplayMode::pixel_width(mode.as_deref()) as u32;
        let pixel_height = CGDisplayMode::pixel_height(mode.as_deref()) as u32;
        Self { id, width, height, pixel_width: pixel_width.max(width), pixel_height: pixel_height.max(height) }
    }
}

/// 电脑现在是不是锁着屏（登录窗口挡着桌面）。拿不到会话信息时当成没锁：这时也注入不了按键。
#[cfg(target_os = "macos")]
pub fn screen_locked() -> bool {
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
    let Some(session) = objc2_core_graphics::CGSessionCopyCurrentDictionary() else { return false };
    // SAFETY: 会话字典的键都是字符串，值按类型再查。
    let session: CFRetained<CFDictionary<CFString, CFType>> = unsafe { CFRetained::cast_unchecked(session) };
    session
        .get(&CFString::from_static_str("CGSSessionScreenIsLocked"))
        .and_then(|value| value.downcast::<CFBoolean>().ok())
        .is_some_and(|value| value.as_bool())
}

/// 电脑现在是不是锁着屏（锁屏界面在安全桌面上，采不到也注入不了）。查不到时当成没锁。
#[cfg(windows)]
pub fn screen_locked() -> bool {
    win::screen_locked()
}

/// 这个进程的责任进程有没有拿到授权。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Permissions {
    /// 「屏幕录制」，`Capture` 要它。
    pub screen_capture: bool,
    /// 「辅助功能」，`Injector` 要它。
    pub post_event: bool,
}

#[cfg(target_os = "macos")]
pub fn permissions() -> Permissions {
    use objc2_core_graphics::{CGPreflightPostEventAccess, CGPreflightScreenCaptureAccess};
    Permissions { screen_capture: CGPreflightScreenCaptureAccess(), post_event: CGPreflightPostEventAccess() }
}

/// 没拿到的授权各请求一次，返回请求后的状态：第一次请求时系统弹框，之后只能用户到系统设置里开。
#[cfg(target_os = "macos")]
pub fn request_permissions() -> Permissions {
    use objc2_core_graphics::{CGRequestPostEventAccess, CGRequestScreenCaptureAccess};
    let now = permissions();
    Permissions {
        screen_capture: now.screen_capture || CGRequestScreenCaptureAccess(),
        post_event: now.post_event || CGRequestPostEventAccess(),
    }
}

/// Windows 上采集和注入都不用授权。
#[cfg(windows)]
pub fn permissions() -> Permissions {
    Permissions { screen_capture: true, post_event: true }
}

#[cfg(windows)]
pub fn request_permissions() -> Permissions {
    permissions()
}
