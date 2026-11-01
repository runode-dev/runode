//! 远程桌面不碰界面的部分：被看的电脑上采集一块屏幕、编成低延迟的硬件 HEVC 或 H.264（`Capture`），把观看端
//! （手机或桌面 app）发来的鼠标和键盘输入注入系统（`Injector`）；桌面 app 当观看端时把画面解回来
//! （`Decoder`）。传输和协议不在这里。
//!
//! macOS 上 ScreenCaptureKit 采集，VideoToolbox 编解码，CoreGraphics 注入；采集要「屏幕录制」的授权，
//! 注入要「辅助功能」的授权，都按调用进程的责任进程记（`permissions`）。Windows 上 DXGI 桌面复制
//! 采集，Media Foundation 编解码，`SendInput` 注入，不用授权。Linux 上只采集和注入，不解码，按屏幕上的图形会话
//! 挑 X11、Mutter 或 portal（`linux`）。

#[cfg(target_os = "macos")]
mod awake;
#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
mod decoder;
#[cfg(target_os = "macos")]
mod encoder;
#[cfg(target_os = "macos")]
mod input;
mod keys;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod mode;
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
mod nal;
#[cfg(any(windows, target_os = "linux", test))]
mod nv12;
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
mod refine;
#[cfg(windows)]
mod win;

#[cfg(target_os = "macos")]
pub use awake::KeepAwake;
#[cfg(target_os = "macos")]
pub use capture::{Capture, Keyframes};
#[cfg(target_os = "macos")]
pub use decoder::{Decoded, Decoder, PixelBuffer};
#[cfg(target_os = "macos")]
pub use input::Injector;
pub use keys::key_strokes;
#[cfg(target_os = "linux")]
pub use linux::{Capture, Injector, KeepAwake, Keyframes, adopt_session};
#[cfg(windows)]
pub use win::{Capture, Decoded, Decoder, Injector, KeepAwake, Keyframes, console_user, in_service_session};

pub use runode_types::screen::{Codec, VideoFrame};

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

/// 单帧的上限按这么长一段时间算：任意这么长的一段里编出的字节数不超过平均码率的 `BURST_HEADROOM` 倍。关键帧和
/// 画面大变时的帧因此摊到后面几帧，不会一帧就把网络堵上几百毫秒。
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
const BURST_WINDOW: std::time::Duration = std::time::Duration::from_millis(150);
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
const BURST_HEADROOM: f64 = 1.5;
/// 画面静止后补清楚（`Refiner`）时码率放大的倍数。
#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
const REFINE_BOOST: f64 = 4.;

/// `Capture` 交出的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Frame(VideoFrame),
    /// 采集停了，不会再有帧：授权被收回、屏幕拔掉这类，带系统给的原因。
    Stopped(String),
}

/// 怎么采集。
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// 画面里画上鼠标指针。
    pub cursor: bool,
    /// 观看端解得了的编码，想要的排前面，不能为空。用排在前面、又能低延迟编的那种；都做不到低延迟时用排在前面、
    /// 编得了的那种。
    pub codecs: Vec<Codec>,
}

/// 一块屏幕。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Display {
    /// macOS 上是 `CGDirectDisplayID`，Windows 上是显卡和它的输出的序号（`win::display`），Linux 上由后端定（RandR
    /// 的屏幕名、接口名的散列或 PipeWire 节点）。
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

    /// 主屏。
    #[cfg(target_os = "linux")]
    pub fn main() -> Self {
        Self::all().swap_remove(0)
    }

    /// 开着的各块屏幕，主屏排第一；列不出时是凑的一块 1920×1080，开始采集时再报错。
    #[cfg(target_os = "linux")]
    pub fn all() -> Vec<Self> {
        let mut displays = linux::displays();
        if displays.is_empty() {
            displays.push(Self { id: 0, width: 1920, height: 1080, pixel_width: 1920, pixel_height: 1080 });
        }
        displays
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

/// 屏幕的一种显示模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DisplayMode {
    /// 按它排版的尺寸：macOS 上是点数，Windows 上是像素数。
    pub width: u32,
    pub height: u32,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

/// 被 `set_display_mode` 换过的屏幕和它们第一次被换之前的模式。
static ORIGINAL_MODES: Mutex<Option<HashMap<u32, DisplayMode>>> = Mutex::new(None);

/// 屏幕 `id` 能换成的显示模式：一种排版尺寸一个，同样尺寸有 HiDPI 的取它，从大到小。Linux 上不换模式，是空的。
#[cfg(target_os = "linux")]
pub fn display_modes(_id: u32) -> Vec<DisplayMode> {
    Vec::new()
}

/// Linux 上不换显示模式：换回去（`mode` 为空）什么都不做，换成别的报 `Unsupported`。
#[cfg(target_os = "linux")]
pub fn set_display_mode(_id: u32, mode: Option<&DisplayMode>) -> std::io::Result<()> {
    match mode {
        Some(_) => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "display modes cannot change on Linux")),
        None => Ok(()),
    }
}

#[cfg(any(target_os = "macos", windows))]
pub fn display_modes(id: u32) -> Vec<DisplayMode> {
    #[cfg(target_os = "macos")]
    let mut modes = mode::modes(id);
    #[cfg(windows)]
    let mut modes = win::modes(id);
    modes.sort_by_key(|mode| std::cmp::Reverse((mode.width, mode.height, mode.pixel_width)));
    modes
}

/// 把屏幕 `id` 换成 `mode`，为空时换回第一次换之前的模式。换的模式不写进系统设置；macOS 上进程退出（崩了也
/// 算）系统就换回原来的，Windows 上不会，要别的进程调 `reset_display_mode`。
#[cfg(any(target_os = "macos", windows))]
pub fn set_display_mode(id: u32, mode: Option<&DisplayMode>) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    use crate::mode as platform;
    #[cfg(windows)]
    use crate::win as platform;
    let mut originals = ORIGINAL_MODES.lock().unwrap_or_else(PoisonError::into_inner);
    let originals = originals.get_or_insert_default();
    match mode {
        Some(mode) => {
            // 读不到现在的模式就不换：换了以后没法换回去。
            let current = match originals.get(&id) {
                Some(original) => *original,
                None => platform::current(id)
                    .ok_or_else(|| std::io::Error::other(format!("cannot read the current mode of display {id}")))?,
            };
            platform::apply(id, mode)?;
            originals.entry(id).or_insert(current);
        }
        None => {
            if let Some(original) = originals.get(&id).copied() {
                platform::reset(id, &original)?;
                originals.remove(&id);
            }
        }
    }
    Ok(())
}

/// 把屏幕 `id` 换回系统设置里的模式，不管是哪个进程换的：Windows 上进程退出时系统不会自己换回去，换过模式的
/// 进程没换回去就退出时由别的进程调它。
#[cfg(windows)]
pub fn reset_display_mode(id: u32) -> std::io::Result<()> {
    win::reset_to_registry(id)
}

/// 屏幕 `id` 的模式被 `set_display_mode` 换过、还没换回去。
pub fn display_mode_changed(id: u32) -> bool {
    ORIGINAL_MODES.lock().unwrap_or_else(PoisonError::into_inner).as_ref().is_some_and(|map| map.contains_key(&id))
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

/// 图形会话是不是锁着屏（logind 的 `LockedHint`）。查不到时当成没锁。
#[cfg(target_os = "linux")]
pub fn screen_locked() -> bool {
    linux::screen_locked()
}

/// 这台电脑的屏幕上有本用户登录着的图形桌面，远程桌面看得了：macOS 和 Windows 上总是有，Linux 上要有本用户的
/// X11 或 Wayland 会话，没装桌面的服务器上没有。
pub fn desktop_available() -> bool {
    #[cfg(target_os = "linux")]
    return linux::session::exists();
    #[cfg(not(target_os = "linux"))]
    cfg!(any(target_os = "macos", windows))
}

/// 之后要授权时可以再在这台电脑的屏幕上问一次。Linux 上走 xdg-desktop-portal 时，屏幕上的人拒绝了或没理会，
/// 在下次调这个之前列屏幕、采集、注入都不再弹框，直接报上次的错；别的系统上什么都不做。
pub fn ask_again() {
    #[cfg(target_os = "linux")]
    linux::ask_again();
}

/// 这个进程的责任进程有没有拿到授权。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Permissions {
    /// 「屏幕录制」，`Capture` 要它。
    pub screen_capture: bool,
    /// 「辅助功能」或「发送事件」，`Injector` 要其中一项。
    pub post_event: bool,
}

#[cfg(target_os = "macos")]
pub fn permissions() -> Permissions {
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

/// 没拿到的授权各请求一次，返回请求后的状态：第一次请求时系统弹框，之后只能用户到系统设置里开。
/// 「辅助功能」用带提示的 `AXIsProcessTrustedWithOptions` 请求，它弹的对话框带「打开系统设置」，也会把程序
/// 列进辅助功能的列表；`CGRequestPostEventAccess` 在新系统上不弹框，只记一条被拒的授权。
/// `AXIsProcessTrustedWithOptions` 每调一次都弹框，一个进程里只带提示调一次。
#[cfg(target_os = "macos")]
pub fn request_permissions() -> Permissions {
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

/// Windows 和 Linux 上采集和注入都不用系统设置里的授权；Wayland 的 portal 在开始采集时自己问用户。
#[cfg(any(windows, target_os = "linux"))]
pub fn permissions() -> Permissions {
    Permissions { screen_capture: true, post_event: true }
}

#[cfg(any(windows, target_os = "linux"))]
pub fn request_permissions() -> Permissions {
    permissions()
}
