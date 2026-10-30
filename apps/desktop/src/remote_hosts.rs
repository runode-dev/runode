//! 经 SSH 连的远端主机在界面这边要办的事：连上一台主机后开始办那台机器上的命令行转来的请求，把那台
//! 主机上断开着的终端接回来；ssh 要问用户时交给最前面的窗口弹框。

use std::collections::HashSet;

use futures::StreamExt as _;
use gpui::App;

use crate::{
    host_client::remote::{self, HostId, Phase, RemoteEvent, Status},
    terminal_view, window,
};

/// 开始收远端主机的事件，要在恢复窗口之前调。
pub fn install(cx: &mut App) {
    let Some(mut events) = remote::events() else {
        return;
    };
    cx.spawn(async move |cx| {
        let mut served = HashSet::new();
        while let Some(event) = events.next().await {
            cx.update(|cx| handle(event, &mut served, cx));
        }
    })
    .detach();
}

fn handle(event: RemoteEvent, served: &mut HashSet<String>, cx: &mut App) {
    match event {
        RemoteEvent::Connected { host, alive } => {
            if served.insert(host.clone())
                && let Some(remote) = remote::remote(&host)
            {
                window::serve(HostId::Ssh(host.clone()), remote.link.clone(), cx);
            }
            terminal_view::remote_reconnected(HostId::Ssh(host), alive, cx);
        }
        RemoteEvent::Status(host) => {
            for front in window::windows(cx) {
                front.update(cx, |view, _, cx| view.remote_status_changed(&host, cx)).ok();
            }
        }
        // 问在最前面的窗口上；一个窗口都没有时丢掉，算取消。
        RemoteEvent::Ask(ask) => match window::front_window(cx) {
            Some(front) => {
                front.update(cx, |view, window, cx| view.show_ask(ask, window, cx)).ok();
            }
            None => tracing::info!("no window to ask {:?} for ssh to {}", ask.prompt, ask.host),
        },
    }
}

/// 连接状态在界面上的说法。
pub fn status_text(status: &Status) -> String {
    let percent = |done: u64, total: u64| (done * 100).checked_div(total).map(|p| format!("{p}%")).unwrap_or_default();
    match status {
        Status::Idle => rust_i18n::t!("ssh.status_idle").into_owned(),
        Status::Connecting(Phase::Authenticating) => rust_i18n::t!("ssh.status_authenticating").into_owned(),
        Status::Connecting(Phase::Probing) => rust_i18n::t!("ssh.status_probing").into_owned(),
        Status::Connecting(Phase::Downloading { done, total }) => {
            rust_i18n::t!("ssh.status_downloading", percent = percent(*done, *total)).into_owned()
        }
        Status::Connecting(Phase::Uploading { done, total }) => {
            rust_i18n::t!("ssh.status_uploading", percent = percent(*done, *total)).into_owned()
        }
        Status::Connecting(Phase::Starting) => rust_i18n::t!("ssh.status_starting").into_owned(),
        Status::Connected(_) => rust_i18n::t!("ssh.status_connected").into_owned(),
        Status::Waiting { attempt, error, .. } => {
            rust_i18n::t!("ssh.status_waiting", attempt = attempt, error = error.as_str()).into_owned()
        }
        Status::Stopped { error, .. } => rust_i18n::t!("ssh.status_stopped", error = error.as_str()).into_owned(),
    }
}
