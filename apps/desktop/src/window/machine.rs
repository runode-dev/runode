//! 卡片样式下标题栏左边的这台机器：电脑名和机型（「Ethan 的 MacBook Pro」「MacBook Pro」），
//! 笔记本和台式机各一个图标。启动时在后台读一次，读到后重画各窗口；读到之前不显示。

use std::sync::OnceLock;

use gpui::{App, Div, SharedString, div, prelude::*, px, svg};
use runode_shared_types::color::Rgb;

use crate::{
    assets::{DESKTOP_ICON, LAPTOP_ICON},
    ui::hsla,
};

/// 标题栏里这一块最宽这么宽，再长的电脑名截断。
pub(super) const MACHINE_MAX_WIDTH: f32 = 180.;

struct Machine {
    name: SharedString,
    model: SharedString,
    laptop: bool,
}

static MACHINE: OnceLock<Machine> = OnceLock::new();

/// 在后台读这台机器的名字和机型，读好后重画各窗口。
pub fn load(cx: &mut App) {
    let task = cx.background_spawn(async {
        let model = model_name();
        let laptop = model.starts_with("MacBook");
        Machine { name: runode_remote_access::host_name().into(), model: model.into(), laptop }
    });
    cx.spawn(async move |cx| {
        let machine = task.await;
        if MACHINE.set(machine).is_ok() {
            cx.update(|cx| cx.refresh_windows());
        }
    })
    .detach();
}

/// 标题栏左边的这一块：图标，右边上下两行是电脑名和机型。还没读到时为 `None`。
pub(super) fn render_machine(fg: Rgb) -> Option<Div> {
    let machine = MACHINE.get()?;
    let fg = hsla(fg);
    Some(
        div()
            .flex_none()
            .max_w(px(MACHINE_MAX_WIDTH))
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                svg()
                    .flex_none()
                    .path(if machine.laptop { LAPTOP_ICON } else { DESKTOP_ICON })
                    .size(px(16.))
                    .text_color(fg.opacity(0.6)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(12.))
                            .line_height(px(15.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(fg.opacity(0.85))
                            .child(machine.name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(10.))
                            .line_height(px(12.))
                            .text_color(fg.opacity(0.5))
                            .child(machine.model.clone()),
                    ),
            ),
    )
}

/// 机型的名字，去掉括号里的尺寸和芯片：「MacBook Pro (16-inch, M5 Max)」是「MacBook Pro」。
fn short_model(product: &str) -> &str {
    product.split(" (").next().unwrap_or(product).trim()
}

/// 机型标识（`hw.model`，比如 `MacBookPro16,1`）里的系列，较早的 Intel Mac 读不到产品名时用。
fn family_of(identifier: &str) -> &'static str {
    [
        ("MacBookPro", "MacBook Pro"),
        ("MacBookAir", "MacBook Air"),
        ("MacBook", "MacBook"),
        ("Macmini", "Mac mini"),
        ("MacPro", "Mac Pro"),
        ("iMac", "iMac"),
    ]
    .into_iter()
    .find_map(|(prefix, family)| identifier.starts_with(prefix).then_some(family))
    .unwrap_or("Mac")
}

#[cfg(target_os = "macos")]
fn model_name() -> String {
    if let Some(product) = iokit::product_name() {
        return short_model(&product).to_owned();
    }
    family_of(&iokit::hw_model().unwrap_or_default()).to_owned()
}

#[cfg(not(target_os = "macos"))]
fn model_name() -> String {
    family_of("").to_owned()
}

#[cfg(target_os = "macos")]
mod iokit {
    use std::ffi::{CString, c_char, c_void};

    type CFTypeRef = *const c_void;
    type CFIndex = isize;
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IORegistryEntryFromPath(main_port: u32, path: *const c_char) -> u32;
        fn IORegistryEntryCreateCFProperty(entry: u32, key: CFTypeRef, allocator: CFTypeRef, options: u32)
        -> CFTypeRef;
        fn IOObjectRelease(object: u32) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(allocator: CFTypeRef, string: *const c_char, encoding: u32) -> CFTypeRef;
        fn CFGetTypeID(object: CFTypeRef) -> usize;
        fn CFDataGetTypeID() -> usize;
        fn CFDataGetLength(data: CFTypeRef) -> CFIndex;
        fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
        fn CFRelease(object: CFTypeRef);
    }

    /// 设备树里的产品名，比如「MacBook Pro (16-inch, M5 Max)」；Apple 芯片的 Mac 才有。
    pub(super) fn product_name() -> Option<String> {
        let path = CString::new("IODeviceTree:/product").ok()?;
        let key = CString::new("product-name").ok()?;
        // SAFETY: 主端口传 0 表示默认端口；路径和键都是以 NUL 结尾的 C 字符串。拿到的注册表项和 CF
        // 对象都归调用方，用完释放；取字节前先确认是 CFData。
        unsafe {
            let entry = IORegistryEntryFromPath(0, path.as_ptr());
            if entry == 0 {
                return None;
            }
            let key = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), UTF8);
            let value = if key.is_null() {
                std::ptr::null()
            } else {
                let value = IORegistryEntryCreateCFProperty(entry, key, std::ptr::null(), 0);
                CFRelease(key);
                value
            };
            IOObjectRelease(entry);
            if value.is_null() {
                return None;
            }
            let name = (CFGetTypeID(value) == CFDataGetTypeID()).then(|| {
                let len = usize::try_from(CFDataGetLength(value)).unwrap_or(0);
                let bytes = std::slice::from_raw_parts(CFDataGetBytePtr(value), len);
                let end = bytes.iter().position(|&b| b == 0).unwrap_or(len);
                String::from_utf8_lossy(&bytes[..end]).into_owned()
            });
            CFRelease(value);
            name.filter(|name| !name.trim().is_empty())
        }
    }

    /// 机型标识，比如 `MacBookPro16,1`。
    pub(super) fn hw_model() -> Option<String> {
        let mut buf = [0u8; 128];
        let mut len = buf.len();
        let name = CString::new("hw.model").ok()?;
        // SAFETY: 缓冲是本地数组，长度如实给出，返回后 `len` 是写入的字节数。
        let ok =
            unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0) }
                == 0;
        ok.then(|| {
            let end = buf[..len].iter().position(|&b| b == 0).unwrap_or(len);
            String::from_utf8_lossy(&buf[..end]).into_owned()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_drops_size_and_chip() {
        assert_eq!(short_model("MacBook Pro (16-inch, M5 Max)"), "MacBook Pro");
        assert_eq!(short_model("Mac mini"), "Mac mini");
    }

    #[test]
    fn old_identifiers_map_to_a_family() {
        assert_eq!(family_of("MacBookPro16,1"), "MacBook Pro");
        assert_eq!(family_of("MacBookAir10,1"), "MacBook Air");
        assert_eq!(family_of("Macmini9,1"), "Mac mini");
        assert_eq!(family_of("Mac14,2"), "Mac");
    }
}
