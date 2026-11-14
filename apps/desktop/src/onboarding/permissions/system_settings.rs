//! 系统设置的窗口在哪、什么时候退出。只读窗口服务器列出的进程号、层级和位置大小，不读窗口标题，所以用不着
//! 屏幕录制的授权。

use std::{
    ptr::NonNull,
    time::{Duration, Instant},
};

use block2::RcBlock;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use gpui::{AsyncApp, Bounds, DisplayId, Pixels, Point, point, px, size};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey, NSWorkspaceDidTerminateApplicationNotification,
};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGError, CGGetActiveDisplayList, CGMainDisplayID,
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID,
    kCGWindowBounds, kCGWindowLayer, kCGWindowOwnerPID,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSOperationQueue, NSString};

const BUNDLE_ID: &str = "com.apple.systempreferences";
/// 等窗口时隔多久查一次。
const POLL: Duration = Duration::from_millis(100);
/// 最多等多久。
const WAIT: Duration = Duration::from_secs(3);
/// 宽高都超过这么多才算主窗口，不算工具条、弹框这些小窗口。
const MIN_SIDE: f64 = 200.;
/// 最多认这么多块屏。
const MAX_DISPLAYS: usize = 16;

/// 系统设置的主窗口：在哪块屏上，相对那块屏左上角的位置大小。
#[derive(Clone, Copy, PartialEq)]
pub(super) struct SettingsWindow {
    pub(super) display: DisplayId,
    pub(super) bounds: Bounds<Pixels>,
}

/// 等系统设置的主窗口出来、停稳：每 `POLL` 在后台查一次，连续两次位置一样才算（打开有动画），`WAIT` 内没等到
/// 时为空。
pub(super) async fn wait(cx: &mut AsyncApp) -> Option<SettingsWindow> {
    let deadline = Instant::now() + WAIT;
    let mut last = None;
    while Instant::now() < deadline {
        let found = cx.background_executor().spawn(async { find() }).await;
        if found.is_some() && found == last {
            return found;
        }
        last = found;
        cx.background_executor().timer(POLL).await;
    }
    None
}

/// 系统设置在跑、主窗口在屏幕上时找到它。
fn find() -> Option<SettingsWindow> {
    let pid = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(BUNDLE_ID))
        .iter()
        .map(|app| app.processIdentifier())
        .find(|pid| *pid > 0)?;
    let frame = window_frame(i64::from(pid))?;
    let mut ids: [CGDirectDisplayID; MAX_DISPLAYS] = [0; MAX_DISPLAYS];
    let mut count = 0u32;
    // SAFETY: 两个指针都指着够大的本地变量。
    let error = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS as u32, ids.as_mut_ptr(), &mut count) };
    if error != CGError::Success {
        return None;
    }
    let displays: Vec<_> = ids[..count as usize].iter().map(|id| (*id, CGDisplayBounds(*id))).collect();
    let (display, bounds) = relative(frame, &displays)?;
    Some(SettingsWindow { display: DisplayId::new(u64::from(display)), bounds })
}

/// 进程 `pid` 在屏幕上第一个层级为 0、宽高都超过 `MIN_SIDE` 的窗口，CG 的全局坐标（原点在主屏左上角）。
fn window_frame(pid: i64) -> Option<CGRect> {
    let options = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID)?;
    // SAFETY: 窗口列表里每一项都是以 CFString 为键的字典。
    let windows: CFRetained<CFArray<CFDictionary<CFString, CFType>>> = unsafe { CFRetained::cast_unchecked(windows) };
    // SAFETY: 这几个键是 CoreGraphics 导出的常量。
    let (owner, layer, bounds) = unsafe { (kCGWindowOwnerPID, kCGWindowLayer, kCGWindowBounds) };
    windows.iter().find_map(|info| {
        let number = |key: &CFString| info.get(key)?.downcast::<CFNumber>().ok()?.as_i64();
        if number(owner) != Some(pid) || number(layer) != Some(0) {
            return None;
        }
        let bounds = info.get(bounds)?.downcast::<CFDictionary>().ok()?;
        let mut frame = CGRect::default();
        // SAFETY: `kCGWindowBounds` 的值是 `CGRectCreateDictionaryRepresentation` 的格式，`frame` 是本地变量。
        let read = unsafe { CGRectMakeWithDictionaryRepresentation(Some(&bounds), &mut frame) };
        (read && frame.size.width > MIN_SIDE && frame.size.height > MIN_SIDE).then_some(frame)
    })
}

/// 全局坐标的窗口 `frame` 落在 `displays`（屏幕编号和它的全局范围）里哪块屏上，按窗口中心认，和它相对那块屏
/// 左上角的位置大小。
fn relative(frame: CGRect, displays: &[(CGDirectDisplayID, CGRect)]) -> Option<(CGDirectDisplayID, Bounds<Pixels>)> {
    let center = CGPoint::new(frame.origin.x + frame.size.width / 2., frame.origin.y + frame.size.height / 2.);
    let (id, screen) = displays.iter().find(|(_, screen)| {
        (screen.origin.x..screen.origin.x + screen.size.width).contains(&center.x)
            && (screen.origin.y..screen.origin.y + screen.size.height).contains(&center.y)
    })?;
    let origin = point(px((frame.origin.x - screen.origin.x) as f32), px((frame.origin.y - screen.origin.y) as f32));
    Some((*id, Bounds::new(origin, size(px(frame.size.width as f32), px(frame.size.height as f32)))))
}

/// 屏幕 `display` 上相对左上角的 `at` 换成 AppKit 的屏幕坐标：原点在主屏左下角，y 朝上。
pub(super) fn appkit_point(display: DisplayId, at: Point<Pixels>) -> objc2_foundation::NSPoint {
    let screen = CGDisplayBounds(u64::from(display) as CGDirectDisplayID);
    let main = CGDisplayBounds(CGMainDisplayID());
    let x = screen.origin.x + f64::from(f32::from(at.x));
    let y = screen.origin.y + f64::from(f32::from(at.y));
    objc2_foundation::NSPoint::new(x, main.size.height - y)
}

/// 听着系统设置退出；丢掉就不再听。
pub(super) struct QuitWatch {
    center: Retained<NSNotificationCenter>,
    observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Drop for QuitWatch {
    fn drop(&mut self) {
        // SAFETY: `observer` 是同一个通知中心的 `addObserverForName:object:queue:usingBlock:` 交回的。
        unsafe { self.center.removeObserver(self.observer.as_ref()) };
    }
}

/// 开始听系统设置退出，每退出一次在返回的通道里收到一次；通知在主线程上送到。
pub(super) fn watch_quit() -> (QuitWatch, UnboundedReceiver<()>) {
    let (quit, quits) = unbounded();
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let block = RcBlock::new(move |notification: NonNull<NSNotification>| {
        // SAFETY: 通知在回调期间有效。
        let notification = unsafe { notification.as_ref() };
        // SAFETY: AppKit 导出的常量。
        let key = unsafe { NSWorkspaceApplicationKey };
        let settings = notification
            .userInfo()
            .and_then(|info| info.objectForKey(key))
            .and_then(|app| app.downcast::<NSRunningApplication>().ok())
            .and_then(|app| app.bundleIdentifier())
            .is_some_and(|id| id.to_string() == BUNDLE_ID);
        if settings {
            quit.unbounded_send(()).ok();
        }
    });
    // SAFETY: 通知名是 AppKit 导出的常量；块只往通道里发，可以在任何线程上跑，这里交给主队列。
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceDidTerminateApplicationNotification),
            None,
            Some(&NSOperationQueue::mainQueue()),
            &block,
        )
    };
    (QuitWatch { center, observer }, quits)
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(width, height))
    }

    #[test]
    fn relative_to_the_display_holding_the_center() {
        let displays = [(1, rect(0., 0., 1440., 900.)), (2, rect(-1920., -180., 1920., 1080.))];
        let (id, bounds) = relative(rect(-1000., -100., 700., 600.), &displays).unwrap();
        assert_eq!(id, 2);
        assert_eq!(bounds, Bounds::new(point(px(920.), px(80.)), size(px(700.), px(600.))));
        let (id, bounds) = relative(rect(100., 50., 700., 600.), &displays).unwrap();
        assert_eq!(id, 1);
        assert_eq!(bounds.origin, point(px(100.), px(50.)));
    }

    #[test]
    fn no_display_when_the_center_is_off_screen() {
        assert!(relative(rect(5000., 5000., 700., 600.), &[(1, rect(0., 0., 1440., 900.))]).is_none());
    }
}
