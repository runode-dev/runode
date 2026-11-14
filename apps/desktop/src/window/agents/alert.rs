//! agent 停下来要人处理时的提醒：系统通知和提示音。什么时候提醒由窗口判断（用户没在看那个
//! 分屏时），这里只管按配置发出去，以及点了通知之后交给窗口跳过去。
//!
//! 通知经 `alerts` 发。提示音用系统自带的声音，按名字播放，不打包音频文件，只有 macOS 上有。

use std::collections::HashSet;

use gpui::{App, Global};
use runode_types::agent::AgentKind;

use crate::{alerts, config::AppConfig};

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

/// 发过、还没收回的 agent 提醒的标识。
#[derive(Default)]
struct Notified {
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
        crate::sound::play(sound);
    }
    if config.agent_notifications && alerts::notifications_available() {
        post(alert, cx);
    }
}

/// 程序自己要求发的通知（OSC 9、OSC 777），开着通知又在 .app 里时发；同一个分屏的新通知替换旧的。
/// 不记进 `posted`：agent 的状态变了、用户看过了都不收回，留在通知中心里。
pub fn program(tag: String, title: String, body: String, cx: &mut App) {
    if cx.global::<AppConfig>().0.agent_notifications {
        alerts::notify(tag, title, body, cx);
    }
}

/// 设置页里的「发一条试试」：播放「干完了」的提示音，在 .app 里发一条测试通知。第一次发时
/// 系统会弹出授权框。点它时标识认不出分屏，什么都不做。
pub fn test(cx: &mut App) {
    if let Some(sound) = &cx.global::<AppConfig>().0.agent_done_sound {
        crate::sound::play(sound);
    }
    let title = rust_i18n::t!("agent.test_title").into_owned();
    alerts::notify("runode-test".into(), title, rust_i18n::t!("agent.test_body").into_owned(), cx);
}

/// 这个标识发过通知、还没收回。
pub fn posted(tag: &str, cx: &App) -> bool {
    cx.try_global::<Notified>().is_some_and(|notified| notified.posted.contains(tag))
}

/// 收回这个标识发过的通知：用户已经看到了那个分屏，或者分屏关掉了。没发过时什么都不做，
/// 也就不会在 .app 之外碰通知中心。
pub fn dismiss(tag: &str, cx: &mut App) {
    if posted(tag, cx) {
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
    alerts::notify(alert.tag, title.into_owned(), body.into_owned(), cx);
}
