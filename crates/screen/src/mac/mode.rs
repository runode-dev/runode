//! macOS 上换屏幕的显示模式：CoreGraphics 列出各种模式，按「只对本程序」（`ForAppOnly`）换，进程一退出系统就
//! 换回原来的。

use std::{collections::HashMap, io};

use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_graphics::{
    CGBeginDisplayConfiguration, CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration,
    CGConfigureDisplayWithDisplayMode, CGConfigureOption, CGDisplayConfigRef, CGDisplayCopyAllDisplayModes,
    CGDisplayCopyDisplayMode, CGDisplayMode, kCGDisplayShowDuplicateLowResolutionModes,
};

use crate::DisplayMode;

/// 屏幕 `id` 的全部模式，HiDPI 和同尺寸的低分辨率模式都在里面。
fn all(id: u32) -> Vec<CFRetained<CGDisplayMode>> {
    // SAFETY: 读 CoreGraphics 导出的常量。
    let key: &CFString = unsafe { kCGDisplayShowDuplicateLowResolutionModes };
    let value: &CFType = CFBoolean::new(true);
    let options = CFDictionary::<CFString, CFType>::from_slices(&[key], &[value]);
    // SAFETY: 选项字典的键和值都是这个函数认的类型。
    let Some(modes) = (unsafe { CGDisplayCopyAllDisplayModes(id, Some(options.as_opaque())) }) else {
        return Vec::new();
    };
    // SAFETY: 这个数组里放的都是 `CGDisplayMode`。
    let modes: CFRetained<CFArray<CGDisplayMode>> = unsafe { CFRetained::cast_unchecked(modes) };
    modes.to_vec()
}

fn describe(mode: &CGDisplayMode) -> DisplayMode {
    let mode = Some(mode);
    DisplayMode {
        width: CGDisplayMode::width(mode) as u32,
        height: CGDisplayMode::height(mode) as u32,
        pixel_width: CGDisplayMode::pixel_width(mode) as u32,
        pixel_height: CGDisplayMode::pixel_height(mode) as u32,
    }
}

pub(crate) fn modes(id: u32) -> Vec<DisplayMode> {
    let mut best: HashMap<(u32, u32), DisplayMode> = HashMap::new();
    for mode in all(id).iter().filter(|mode| CGDisplayMode::is_usable_for_desktop_gui(Some(mode))) {
        let mode = describe(mode);
        let slot = best.entry((mode.width, mode.height)).or_insert(mode);
        if mode.pixel_width > slot.pixel_width {
            *slot = mode;
        }
    }
    best.into_values().collect()
}

pub(crate) fn current(id: u32) -> Option<DisplayMode> {
    CGDisplayCopyDisplayMode(id).map(|mode| describe(&mode))
}

/// 换成和 `wanted` 一样的模式，刷新率尽量和现在的一样，没有就取最高的。
pub(crate) fn apply(id: u32, wanted: &DisplayMode) -> io::Result<()> {
    let rate = CGDisplayCopyDisplayMode(id).map(|mode| CGDisplayMode::refresh_rate(Some(&mode)));
    let chosen = all(id)
        .into_iter()
        .filter(|mode| CGDisplayMode::is_usable_for_desktop_gui(Some(mode)) && describe(mode) == *wanted)
        .max_by(|a, b| {
            let score = |mode: &CGDisplayMode| {
                let refresh = CGDisplayMode::refresh_rate(Some(mode));
                (Some(refresh) == rate, refresh)
            };
            let (a, b) = (score(a), score(b));
            a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the display has no such mode"))?;
    let mut config: CGDisplayConfigRef = std::ptr::null_mut();
    // SAFETY: 输出参数指向本地变量；拿到的配置要么交出去要么取消。
    unsafe {
        let status = CGBeginDisplayConfiguration(&mut config);
        if status.0 != 0 {
            return Err(io::Error::other(format!("cannot begin the display configuration ({})", status.0)));
        }
        let status = CGConfigureDisplayWithDisplayMode(config, id, Some(&chosen), None);
        if status.0 != 0 {
            CGCancelDisplayConfiguration(config);
            return Err(io::Error::other(format!("cannot configure the display mode ({})", status.0)));
        }
        let status = CGCompleteDisplayConfiguration(config, CGConfigureOption::ForAppOnly);
        if status.0 != 0 {
            return Err(io::Error::other(format!("cannot change the display mode ({})", status.0)));
        }
    }
    Ok(())
}

pub(crate) fn reset(id: u32, original: &DisplayMode) -> io::Result<()> {
    apply(id, original)
}
