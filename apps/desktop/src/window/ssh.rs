//! 窗口里和 SSH 主机有关的界面：新建工作区对话框里选主机、连上它、一级级浏览远端的目录，选好在那里开
//! 工作区；主机的连接状态变了时，让断开着的终端的提示、侧栏和对话框跟着重画；哪些主机还用着（`hosts_in_use`）。

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Focusable as _, Role, SharedString, Subscription, WeakEntity,
    Window, div, prelude::*, px,
};
use runode_protocol::{ClientMsg, HostMsg};
use runode_ssh::Destination;

use super::WindowView;
use crate::{
    host_client::remote::{self, HostId, Status},
    remote_hosts::status_text,
    ui::{
        a11y::Press,
        button::{Button, ButtonVariant},
        font_size::{FontSize, scaled},
        input::{Input, InputEvent, InputState},
        theme::ActiveTheme as _,
    },
};

/// 列远端目录最多等这么久。
const LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// 主机清单最多列这么多个。
const MAX_HOSTS: usize = 8;

/// 对话框里展开的「SSH 主机」一栏。
pub(super) struct SshStep {
    field: Entity<InputState>,
    _events: Subscription,
    /// 最近用过的和 `~/.ssh/config` 里的主机，读好之前只有最近用过的。
    hosts: Vec<String>,
    /// 正在连或连上了的主机。
    host: Option<String>,
    /// 连上以后浏览远端目录。
    browse: Option<Browse>,
}

impl SshStep {
    pub(super) fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.field.focus_handle(cx)
    }
}

struct Browse {
    field: Entity<InputState>,
    _events: Subscription,
    /// 列出来的目录：规范化后的绝对路径。还没列到时为空。
    path: Option<PathBuf>,
    dirs: Vec<String>,
    loading: bool,
    error: Option<String>,
    /// 最近一次列目录的序号：先发的回来得晚时不盖掉后发的。
    request: u64,
}

/// 用着的远端主机：哪个窗口里有工作区、终端在那台机器上，或者新建工作区的对话框正连着它。`this` 是问的
/// 那个窗口：GPUI 画一个窗口、给它派发事件时，`windows` 里读不到它自己。
pub(super) fn hosts_in_use(this: &WeakEntity<WindowView>, cx: &App) -> HashSet<String> {
    let mut hosts = HashSet::new();
    let current = this.upgrade();
    let others = super::windows(cx).into_iter().filter_map(|window| window.read(cx).ok());
    for view in others.chain(current.as_ref().map(|view| view.read(cx))) {
        hosts.extend(view.workspaces.iter().filter_map(|workspace| workspace.host.label().map(str::to_owned)));
        hosts.extend(view.terminal_views().filter_map(|terminal| terminal.read(cx).host().label().map(str::to_owned)));
        hosts.extend(view.new_workspace.as_ref().and_then(|dialog| dialog.ssh.as_ref()?.host.clone()));
    }
    hosts
}

impl WindowView {
    /// 展开「SSH 主机」一栏，焦点给主机输入框；已经展开时只给焦点。
    pub(super) fn show_ssh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let recent = self.recent_ssh_hosts(cx);
        let Some(dialog) = &mut self.new_workspace else {
            return;
        };
        if dialog.ssh.is_none() {
            let field = cx.new(|cx| {
                InputState::new(String::new(), cx)
                    .with_placeholder(rust_i18n::t!("workspace.ssh_host").into_owned())
                    .with_label(rust_i18n::t!("workspace.ssh").into_owned())
            });
            let events = cx.subscribe_in(&field, window, |this, field, event: &InputEvent, window, cx| match event {
                InputEvent::Next => {
                    let name = field.read(cx).query().trim().to_owned();
                    this.connect_ssh(name, cx);
                }
                InputEvent::Dismiss => this.close_new_workspace(window, cx),
                InputEvent::Changed(_) => cx.notify(),
                InputEvent::Previous => {}
            });
            dialog.ssh = Some(SshStep { field, _events: events, hosts: recent, host: None, browse: None });
            let home = super::model::home_dir();
            let job =
                cx.background_spawn(async move { home.map(|home| runode_ssh::list_hosts(&home)).unwrap_or_default() });
            cx.spawn(async move |this, cx| {
                let configured = job.await;
                this.update(cx, |this, cx| {
                    let Some(step) = this.new_workspace.as_mut().and_then(|dialog| dialog.ssh.as_mut()) else {
                        return;
                    };
                    for host in configured {
                        if !step.hosts.contains(&host) {
                            step.hosts.push(host);
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        if let Some(step) = &dialog.ssh {
            window.focus(&step.field.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// 各窗口里远端工作区的主机，当前窗口的在前。
    fn recent_ssh_hosts(&self, cx: &App) -> Vec<String> {
        let mut hosts: Vec<String> = Vec::new();
        let others = super::windows(cx).into_iter().filter_map(|window| window.read(cx).ok());
        let names = self
            .workspaces
            .iter()
            .filter_map(|workspace| workspace.host.label().map(str::to_owned))
            .chain(others.flat_map(|view| view.workspaces.iter().filter_map(|w| w.host.label().map(str::to_owned))))
            .chain(remote::remotes().into_iter().map(|host| host.name.clone()));
        for name in names {
            if !hosts.contains(&name) {
                hosts.push(name);
            }
        }
        hosts
    }

    /// 连主机 `name`；认不出这个写法时什么都不做。之前停下的（要用户来办的）再试一次，已经连上的直接浏览目录。
    fn connect_ssh(&mut self, name: String, cx: &mut Context<Self>) {
        if Destination::parse(&name).is_none() {
            return;
        }
        let Some(host) = remote::remote(&name) else {
            return;
        };
        if matches!(host.status(), Status::Stopped { .. } | Status::Waiting { .. } | Status::Idle) {
            host.retry_now();
        }
        let Some(step) = self.new_workspace.as_mut().and_then(|dialog| dialog.ssh.as_mut()) else {
            return;
        };
        step.field.update(cx, |field, cx| field.set_query(name.clone(), cx));
        step.host = Some(name.clone());
        step.browse = None;
        if matches!(host.status(), Status::Connected(_)) {
            self.start_browse(cx);
        }
        cx.notify();
    }

    /// 主机 `name` 的连接状态变了：它上面断开着的终端重画提示；对话框正等它连上的话开始浏览目录。
    pub(crate) fn remote_status_changed(&mut self, name: &str, cx: &mut Context<Self>) {
        let host = HostId::Ssh(name.to_owned());
        for view in self.terminal_views() {
            if view.read(cx).host() == &host && view.read(cx).lost() {
                view.update(cx, |_, cx| cx.notify());
            }
        }
        // 侧栏的主机标识跟着连接状态变淡变亮。
        if self.workspaces.iter().any(|workspace| workspace.host == host) {
            cx.notify();
        }
        let Some(step) = self.new_workspace.as_ref().and_then(|dialog| dialog.ssh.as_ref()) else {
            return;
        };
        if step.host.as_deref() != Some(name) {
            return;
        }
        let connected = remote::remote(name).is_some_and(|host| matches!(host.status(), Status::Connected(_)));
        if connected && step.browse.is_none() {
            self.start_browse(cx);
        }
        cx.notify();
    }

    /// 连上了：从远端的家目录开始浏览。
    fn start_browse(&mut self, cx: &mut Context<Self>) {
        let Some(step) = self.new_workspace.as_mut().and_then(|dialog| dialog.ssh.as_mut()) else {
            return;
        };
        let field =
            cx.new(|cx| InputState::new(String::new(), cx).with_label(rust_i18n::t!("workspace.ssh_dir").into_owned()));
        let events = cx.subscribe(&field, |this, field, event: &InputEvent, cx| {
            if let InputEvent::Next = event {
                let path = PathBuf::from(field.read(cx).query().trim());
                this.list_remote(Some(path), cx);
            }
        });
        step.browse = Some(Browse {
            field,
            _events: events,
            path: None,
            dirs: Vec::new(),
            loading: false,
            error: None,
            request: 0,
        });
        self.list_remote(None, cx);
    }

    /// 在后台列远端的目录 `path`，为空时列家目录。
    fn list_remote(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(step) = self.new_workspace.as_mut().and_then(|dialog| dialog.ssh.as_mut()) else {
            return;
        };
        let (Some(name), Some(browse)) = (step.host.clone(), step.browse.as_mut()) else {
            return;
        };
        let Some(host) = remote::remote(&name) else {
            return;
        };
        browse.loading = true;
        browse.request += 1;
        let request = browse.request;
        let link = host.link.clone();
        let job = cx.background_spawn(async move {
            link.request(|req| ClientMsg::ListDirs { req, path }, LIST_TIMEOUT).map_err(|err| err.to_string())
        });
        cx.spawn(async move |this, cx| {
            let listed = job.await;
            this.update(cx, |this, cx| {
                let Some(step) = this.new_workspace.as_mut().and_then(|dialog| dialog.ssh.as_mut()) else {
                    return;
                };
                let Some(browse) = step
                    .browse
                    .as_mut()
                    .filter(|browse| step.host.as_deref() == Some(name.as_str()) && browse.request == request)
                else {
                    return;
                };
                browse.loading = false;
                match listed {
                    Ok(HostMsg::Dirs { path, dirs, .. }) => {
                        let text = path.to_string_lossy().into_owned();
                        browse.field.update(cx, |field, cx| field.set_query(text, cx));
                        browse.path = Some(path);
                        browse.dirs = dirs;
                        browse.error = None;
                    }
                    Ok(HostMsg::Error { message, .. }) | Err(message) => browse.error = Some(message),
                    Ok(other) => browse.error = Some(format!("unexpected reply {other:?}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 在连上的主机上浏览到的目录里开工作区，关掉对话框。
    fn open_ssh_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.new_workspace.as_ref().and_then(|dialog| dialog.ssh.as_ref()) else {
            return;
        };
        let (Some(name), Some(path)) = (step.host.clone(), step.browse.as_ref().and_then(|b| b.path.clone())) else {
            return;
        };
        self.new_workspace = None;
        let host = HostId::Ssh(name.clone());
        let Some(view) = self.spawn_terminal_on(&host, Some(&path), None, window, cx) else {
            return;
        };
        let label = path.file_name().map_or(name, |dir| dir.to_string_lossy().into_owned());
        self.insert_workspace(self.active + 1, path, Some(label), view, window, cx);
    }

    /// 对话框里「SSH 主机」展开后的内容。
    pub(super) fn render_ssh_step(&self, step: &SshStep, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let field_bg = cx.theme().mix(0.03);
        let hover_bg = cx.theme().mix(0.06);
        let row = |id: SharedString, label: String| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(label.clone())
                .h(scaled(26.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .cursor_pointer()
                .hover(move |row| row.bg(hover_bg))
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(label)
        };
        let small = |text: String| div().text_size(FontSize::Small).text_color(fg.opacity(0.55)).child(text);
        let connect = Button::new("ssh-connect", rust_i18n::t!("workspace.ssh_connect"))
            .variant(ButtonVariant::Secondary)
            .h_auto()
            .py(px(5.))
            .cursor_pointer()
            .on_press(cx, |this, _, cx| {
                let name = this.new_workspace.as_ref().and_then(|dialog| dialog.ssh.as_ref());
                let name = name.map(|step| step.field.read(cx).query().trim().to_owned()).unwrap_or_default();
                this.connect_ssh(name, cx);
            });
        let typed = step.field.read(cx).query().trim().to_lowercase();
        let mut body = div().flex().flex_col().gap(px(8.)).pl(px(48.)).pr(px(16.)).pb(px(12.)).child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(Input::new(&step.field).flex_1().min_w_0().bg(field_bg))
                .child(connect),
        );
        let Some(name) = &step.host else {
            let hosts: Vec<AnyElement> = step
                .hosts
                .iter()
                .filter(|host| typed.is_empty() || host.to_lowercase().contains(&typed))
                .take(MAX_HOSTS)
                .map(|host| {
                    let host = host.clone();
                    row(format!("ssh-host-{host}").into(), host.clone())
                        .on_press(cx, move |this, _, cx| this.connect_ssh(host.clone(), cx))
                        .into_any_element()
                })
                .collect();
            if !hosts.is_empty() {
                body = body
                    .child(small(rust_i18n::t!("workspace.ssh_hosts").into_owned()))
                    .child(div().id("ssh-hosts").role(Role::List).flex().flex_col().children(hosts));
            }
            return body;
        };
        let status = remote::remote(name).map_or(Status::Idle, |host| host.status());
        let Some(browse) = step.browse.as_ref().filter(|_| matches!(status, Status::Connected(_))) else {
            let text = status_text(&status);
            let stopped = matches!(status, Status::Stopped { .. } | Status::Waiting { .. });
            let name = name.clone();
            return body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().id("ssh-status").role(Role::Label).aria_label(text.clone()).flex_1().child(text))
                    .when(stopped, |line| {
                        line.child(
                            Button::new("ssh-retry", rust_i18n::t!("workspace.ssh_retry"))
                                .variant(ButtonVariant::Secondary)
                                .h_auto()
                                .py(px(5.))
                                .cursor_pointer()
                                .on_press(cx, move |this, _, cx| this.connect_ssh(name.clone(), cx)),
                        )
                    }),
            );
        };
        let up = browse.path.as_deref().and_then(Path::parent).map(Path::to_path_buf);
        let up_row = up.map(|parent| {
            row("ssh-parent-dir".into(), rust_i18n::t!("workspace.ssh_up").into_owned())
                .on_press(cx, move |this, _, cx| this.list_remote(Some(parent.clone()), cx))
                .into_any_element()
        });
        let dirs = browse.dirs.iter().map(|dir| {
            let next = browse.path.as_ref().map(|path| path.join(dir));
            row(format!("ssh-subdir-{dir}").into(), format!("{dir}/"))
                .on_press(cx, move |this, _, cx| this.list_remote(next.clone(), cx))
                .into_any_element()
        });
        let dirs: Vec<AnyElement> = dirs.collect();
        let open = Button::new("ssh-open", rust_i18n::t!("workspace.ssh_open"))
            .variant(ButtonVariant::Default)
            .h_auto()
            .py(px(5.))
            .cursor_pointer()
            .on_press(cx, |this, window, cx| this.open_ssh_workspace(window, cx));
        body.child(small(rust_i18n::t!("workspace.ssh_dir").into_owned()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(Input::new(&browse.field).flex_1().min_w_0().bg(field_bg))
                    .child(open),
            )
            .children(browse.error.clone().map(|error| {
                div()
                    .id("ssh-list-error")
                    .role(Role::Label)
                    .aria_label(error.clone())
                    .text_color(fg.opacity(0.7))
                    .child(error)
            }))
            .child(
                div()
                    .id("ssh-dirs")
                    .role(Role::List)
                    .max_h(scaled(200.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .when(browse.loading, |list| list.opacity(0.5))
                    .children(up_row)
                    .children(dirs),
            )
    }
}
