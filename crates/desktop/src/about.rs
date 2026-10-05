//! 应用图标与「关于 Runode」面板。

/// 应用图标，由矢量源图渲染成 PNG 后编进二进制。
#[cfg(target_os = "macos")]
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// 设置 Dock 和关于面板用的应用图标。程序还没打包成 .app 时系统找不到图标文件，
/// 只能在启动时显式设置；打包后系统直接用 bundle 里的图标，这里什么都不做——
/// 解码这张大图要几十毫秒，不该拖慢启动。
#[cfg(target_os = "macos")]
pub fn install_icon() {
    use objc2::{AllocAnyThread as _, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    if in_app_bundle() {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("app icon set off the main thread");
        return;
    };
    let data = NSData::with_bytes(ICON_PNG);
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        tracing::warn!("failed to decode the app icon");
        return;
    };
    // SAFETY: 在主线程上调用，image 是有效的 NSImage。
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
}

/// 可执行文件是否在 `*.app/Contents/MacOS/` 里。
#[cfg(target_os = "macos")]
fn in_app_bundle() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let mut dirs = exe.ancestors().skip(1);
    let names = [dirs.next(), dirs.next(), dirs.next()];
    matches!(
        names.map(|dir| dir.and_then(|dir| dir.file_name()).and_then(|name| name.to_str())),
        [Some("MacOS"), Some("Contents"), Some(app)] if app.ends_with(".app")
    )
}

/// 打开 macOS 标准关于面板。名称、版本、版权来自构建脚本嵌进可执行文件的应用信息表，
/// 图标来自 bundle 或 `install_icon`，所以不需要传任何选项。
#[cfg(target_os = "macos")]
pub fn show() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    // 菜单动作总在主线程上执行；拿不到标记说明调用方式出了错，宁可不显示也不要崩。
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("about panel requested off the main thread");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.orderFrontStandardAboutPanel(None);
    app.activate();
}

#[cfg(not(target_os = "macos"))]
pub fn install_icon() {}

#[cfg(not(target_os = "macos"))]
pub fn show() {}
