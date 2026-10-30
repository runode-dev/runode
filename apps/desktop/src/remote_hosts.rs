//! 经 SSH 连的远端主机在界面这边要办的事：连上一台主机后开始办那台机器上的命令行转来的请求，把那台
//! 主机上断开着的终端接回来。连接本身在 `host_client::remote`。

use std::collections::HashSet;

use futures::StreamExt as _;
use gpui::App;

use crate::{
    host_client::remote::{self, HostId, RemoteEvent},
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
        RemoteEvent::Status(_) => {}
        // shortcut: 还没有问用户的对话框，丢掉即取消，要输密码、确认主机密钥的主机连不上，等 SSH 主机的界面一起做。
        RemoteEvent::Ask(ask) => {
            tracing::warn!("ssh to {} asked {:?}, which cannot be answered yet", ask.host, ask.prompt)
        }
    }
}
