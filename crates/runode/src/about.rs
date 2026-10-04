//! 「关于 runode」面板。

/// 打开 macOS 标准关于面板。程序还没打包成 .app，面板读不到 Info.plist，
/// 所以名称、版本和说明都在这里显式传入。
#[cfg(target_os = "macos")]
pub fn show() {
    use objc2::{MainThreadMarker, rc::Retained, runtime::AnyObject};
    use objc2_app_kit::{
        NSAboutPanelOptionApplicationName, NSAboutPanelOptionApplicationVersion,
        NSAboutPanelOptionCredits, NSAboutPanelOptionVersion, NSApplication,
    };
    use objc2_foundation::{NSAttributedString, NSDictionary, NSString};

    // 菜单动作总在主线程上执行；拿不到标记说明调用方式出了错，宁可不显示也不要崩。
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("about panel requested off the main thread");
        return;
    };
    let name = NSString::from_str("runode");
    let version = NSString::from_str(env!("CARGO_PKG_VERSION"));
    let build = NSString::from_str(concat!("ghostty ", env!("RUNODE_GHOSTTY_COMMIT")));
    let credits = NSAttributedString::from_nsstring(&NSString::from_str(
        "面向 AI 编程 agent 的桌面工作台\n终端仿真：libghostty-vt（runode-dev/ghostty）\n界面：GPUI",
    ));
    let values: [Retained<AnyObject>; 4] = [
        name.into(),
        version.into(),
        build.into(),
        credits.into(),
    ];
    // SAFETY: 这些键都是 AppKit 导出的常量字符串，进程存活期间一直有效。
    let keys = unsafe {
        [
            NSAboutPanelOptionApplicationName,
            NSAboutPanelOptionApplicationVersion,
            NSAboutPanelOptionVersion,
            NSAboutPanelOptionCredits,
        ]
    };
    let values: Vec<&AnyObject> = values.iter().map(|v| &**v).collect();
    let options = NSDictionary::from_slices(&keys, &values);
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: 字典的键是 NSAboutPanelOptionKey，值的类型与各键的文档要求一致
    // （名称和版本为 NSString，Credits 为 NSAttributedString）。
    unsafe { app.orderFrontStandardAboutPanelWithOptions(&options) };
    app.activate();
}

#[cfg(not(target_os = "macos"))]
pub fn show() {}
