//! 不挂在哪个视图上的弹框，和系统通知：更新、宿主出了事、agent 停下来这些要告诉用户的事经这里。
//!
//! 通知走系统的通知中心。macOS 上没打包成 .app 的可执行文件里通知中心会直接抛异常让进程退出，即使
//! 构建脚本嵌进去的信息表让它也有 bundle id，所以只在 .app 里发；授权请求在第一次发通知时才由通知中心
//! 弹出。Windows 和 Linux 上随时能发。

use gpui::{App, Global, PromptLevel, SystemNotification};

/// 是否已经开始接收通知的点击，以及系统设置里是否拒绝了 Runode 发通知（拒绝时通知照发但不会弹出来）、
/// 是不是正在问。
#[derive(Default)]
struct Notifications {
    listening: bool,
    #[cfg(target_os = "macos")]
    denied: bool,
    #[cfg(target_os = "macos")]
    asking: bool,
}

impl Global for Notifications {}

/// 在最前面的窗口上弹框，选了第几个按钮交给 `then`；没有窗口时不弹。从菜单派发时窗口正在处理
/// 动作，弹不了框，推迟到这一轮更新结束。
pub fn prompt(
    level: PromptLevel,
    title: &str,
    detail: &str,
    answers: &[&str],
    cx: &mut App,
    then: impl FnOnce(usize, &mut App) + 'static,
) {
    let (title, detail) = (title.to_owned(), detail.to_owned());
    let answers: Vec<String> = answers.iter().map(|answer| (*answer).to_owned()).collect();
    cx.defer(move |cx| {
        let Some(window) = cx.active_window().or_else(|| cx.windows().into_iter().next()) else { return };
        let labels: Vec<&str> = answers.iter().map(String::as_str).collect();
        let Ok(answer) = window.update(cx, |_, window, cx| window.prompt(level, &title, Some(&detail), &labels, cx))
        else {
            return;
        };
        cx.spawn(async move |cx| {
            if let Ok(answer) = answer.await {
                cx.update(|cx| then(answer, cx));
            }
        })
        .detach();
    });
}

/// 发一条系统通知，同一个 `tag` 的新通知替换旧的；通知中心用不了时不发。
pub fn notify(tag: String, title: String, body: String, cx: &mut App) {
    if !notifications_available() {
        return;
    }
    listen(cx);
    cx.show_system_notification(SystemNotification {
        tag: tag.into(),
        title: title.into(),
        body: body.into(),
        actions: Vec::new(),
    });
}

/// 开始接收通知的点击，发第一条通知之前调；已经在接收时什么都不做。注册回调就会建起通知中心，
/// 所以第一次发通知时才注册。点的是更新的通知时交给 `update`，别的是 agent 的提醒，跳到那个分屏。
/// 回调只能有一个，后注册的会顶掉先注册的，所以都经这里。
pub fn listen(cx: &mut App) {
    if std::mem::replace(&mut cx.default_global::<Notifications>().listening, true) {
        return;
    }
    cx.on_system_notification_response(|response, cx| {
        if response.tag == crate::update::NOTIFICATION_TAG {
            crate::update::offer_restart(cx);
        } else {
            crate::window::reveal_notified(&response.tag, cx);
        }
    });
}

#[cfg(target_os = "macos")]
pub fn notifications_available() -> bool {
    crate::about::in_app_bundle()
}

/// 系统设置里拒绝了 Runode 发通知。返回上次问到的结果（没问过、不在 .app 里时算没拒绝），同时在
/// 后台再问一次，结果变了就重画各窗口：用户在系统设置里改完回来，设置页跟着变。一旦拒绝过，
/// 系统不再弹授权框，只能去系统设置里打开。
#[cfg(target_os = "macos")]
pub fn notifications_denied(cx: &mut App) -> bool {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use futures::StreamExt as _;
    use objc2_user_notifications::{UNAuthorizationStatus, UNNotificationSettings, UNUserNotificationCenter};

    if !notifications_available() {
        return false;
    }
    let notified = cx.default_global::<Notifications>();
    let denied = notified.denied;
    if std::mem::replace(&mut notified.asking, true) {
        return denied;
    }
    let (tx, mut rx) = futures::channel::mpsc::unbounded();
    let completion = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        // SAFETY: 回调期间 settings 是有效的对象。
        let status = unsafe { settings.as_ref() }.authorizationStatus();
        tx.unbounded_send(status == UNAuthorizationStatus::Denied).ok();
    });
    UNUserNotificationCenter::currentNotificationCenter().getNotificationSettingsWithCompletionHandler(&completion);
    cx.spawn(async move |cx| {
        let now = rx.next().await;
        cx.update(|cx| {
            let notified = cx.global_mut::<Notifications>();
            notified.asking = false;
            if let Some(now) = now.filter(|now| *now != notified.denied) {
                notified.denied = now;
                cx.refresh_windows();
            }
        })
    })
    .detach();
    denied
}

#[cfg(not(target_os = "macos"))]
pub fn notifications_denied(_: &mut App) -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn notifications_available() -> bool {
    true
}
