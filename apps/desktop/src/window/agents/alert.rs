//! agent 停下来要人处理时的提醒：系统通知和提示音。什么时候提醒由窗口判断（用户没在看那个
//! 分屏时），这里只管按配置发出去，以及点了通知之后交给窗口跳过去。
//!
//! 通知走系统的通知中心。没打包成 .app 的可执行文件里通知中心会直接抛异常让进程退出，即使
//! 构建脚本嵌进去的信息表让它也有 bundle id，所以只在 .app 里发。授权请求在第一次发通知时
//! 才由通知中心弹出。提示音用系统自带的声音，按名字播放，不打包音频文件。

use std::collections::HashSet;

use gpui::{App, Global, SystemNotification};
use runode_shared_types::agent::AgentKind;

use crate::config::AppConfig;

/// 要提醒的事。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alert {
    /// 干完了，用户还没看。
    Done,
    /// 等用户回答。
    Blocked,
}

/// 一次提醒的内容。
pub struct AgentAlert {
    pub kind: AgentKind,
    pub alert: Alert,
    /// 通知的标识：同一个分屏的新通知替换旧的，点通知时凭它找回分屏。
    pub tag: String,
    /// 正文里的 workspace 名和标签标题。
    pub workspace: String,
    pub tab: String,
}

/// 发过的通知，以及是否已经开始接收点击。
#[derive(Default)]
struct Notified {
    listening: bool,
    posted: HashSet<String>,
}

impl Global for Notified {}

/// 按配置提醒：被排除的 agent 什么都不做；配了声音就播放；开着通知又在 .app 里时发通知。
pub fn alert(alert: AgentAlert, cx: &mut App) {
    let config = cx.global::<AppConfig>().0.clone();
    if config.agent_notifications_exclude.contains(&alert.kind) {
        return;
    }
    let sound = match alert.alert {
        Alert::Done => &config.agent_done_sound,
        Alert::Blocked => &config.agent_blocked_sound,
    };
    if let Some(sound) = sound {
        crate::ui::sound::play(sound);
    }
    if config.agent_notifications && notifications_available() {
        post(alert, cx);
    }
}

/// 收回这个标识发过的通知：用户已经看到了那个分屏，或者分屏关掉了。没发过时什么都不做，
/// 也就不会在 .app 之外碰通知中心。
pub fn dismiss(tag: &str, cx: &mut App) {
    let posted = cx.try_global::<Notified>().is_some_and(|notified| notified.posted.contains(tag));
    if posted {
        cx.global_mut::<Notified>().posted.remove(tag);
        cx.dismiss_system_notification(tag);
    }
}

fn post(alert: AgentAlert, cx: &mut App) {
    let name = alert.kind.display_name();
    let title = match alert.alert {
        Alert::Done => rust_i18n::t!("agent.done_title", agent = name),
        Alert::Blocked => rust_i18n::t!("agent.blocked_title", agent = name),
    };
    let body = rust_i18n::t!("agent.notification_body", workspace = alert.workspace, tab = alert.tab);
    cx.default_global::<Notified>().posted.insert(alert.tag.clone());
    listen(cx);
    cx.show_system_notification(SystemNotification {
        tag: alert.tag.into(),
        title: title.into_owned().into(),
        body: body.into_owned().into(),
        actions: Vec::new(),
    });
}

/// 开始接收通知的点击，发第一条通知之前调；已经在接收时什么都不做。注册回调就会建起通知中心，
/// 所以第一次发通知时才注册。点的是更新的通知时交给 `update`，别的是 agent 的提醒，跳到那个分屏。
/// 回调只能有一个，后注册的会顶掉先注册的，所以都经这里。
pub fn listen(cx: &mut App) {
    if std::mem::replace(&mut cx.default_global::<Notified>().listening, true) {
        return;
    }
    cx.on_system_notification_response(|response, cx| {
        if response.tag == crate::update::NOTIFICATION_TAG {
            crate::update::on_notification(cx);
        } else {
            crate::window::reveal_notified(&response.tag, cx);
        }
    });
}

#[cfg(target_os = "macos")]
fn notifications_available() -> bool {
    crate::about::in_app_bundle()
}

#[cfg(not(target_os = "macos"))]
fn notifications_available() -> bool {
    false
}
