//! 窗口里的远端主机：哪些主机还用着（`hosts_in_use`），主机的连接状态变了时让断开着的终端的提示、侧栏和
//! 新建工作区对话框跟着重画。

use std::collections::HashSet;

use gpui::{App, Context, WeakEntity};

use super::WindowView;
use crate::host_client::remote::HostId;

/// 用着的远端主机：哪个窗口里有工作区、终端在那台机器上，或者新建工作区的对话框选着它。`this` 是问的
/// 那个窗口：GPUI 画一个窗口、给它派发事件时，`windows` 里读不到它自己。
pub(super) fn hosts_in_use(this: &WeakEntity<WindowView>, cx: &App) -> HashSet<String> {
    let mut hosts = HashSet::new();
    let current = this.upgrade();
    let others = super::windows(cx).into_iter().filter_map(|window| window.read(cx).ok());
    for view in others.chain(current.as_ref().map(|view| view.read(cx))) {
        hosts.extend(view.workspaces.iter().filter_map(|workspace| workspace.host.label().map(str::to_owned)));
        hosts.extend(view.terminal_views().filter_map(|terminal| terminal.read(cx).host().label().map(str::to_owned)));
        hosts.extend(view.new_workspace.as_ref().and_then(|dialog| dialog.host.label().map(str::to_owned)));
    }
    hosts
}

impl WindowView {
    /// 主机 `name` 的连接状态变了：它上面断开着的终端重画提示，侧栏的主机标识跟着变淡变亮。
    pub(crate) fn remote_status_changed(&mut self, name: &str, cx: &mut Context<Self>) {
        let host = HostId::Ssh(name.to_owned());
        for view in self.terminal_views() {
            if view.read(cx).host() == &host && view.read(cx).lost() {
                view.update(cx, |_, cx| cx.notify());
            }
        }
        if self.workspaces.iter().any(|workspace| workspace.host == host) {
            cx.notify();
        }
        self.new_workspace_host_changed(&host, cx);
    }
}
