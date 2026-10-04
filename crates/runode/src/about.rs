//! 应用图标与「关于 runode」面板。

/// 应用图标，由 assets 里的 icon.svg 渲染而来。
#[cfg(target_os = "macos")]
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// 设置 Dock 和关于面板用的应用图标。程序还没打包成 .app，系统找不到图标文件，
/// 只能在启动时显式设置。
#[cfg(target_os = "macos")]
pub fn install_icon() {
    use objc2::{AllocAnyThread as _, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

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

/// 打开 macOS 标准关于面板。名称、版本、版权来自嵌入的 Info.plist（见构建脚本），
/// 图标来自 `install_icon`，所以不需要传任何选项。
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
