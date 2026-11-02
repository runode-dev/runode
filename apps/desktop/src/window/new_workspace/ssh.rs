//! 新建工作区对话框里和远端主机有关的部分：上面的主机选择（本机、连过的主机和配对的电脑，或者新加一台）、选了
//! 远端主机后连上它、一级级浏览那台机器的目录，在选好的目录里开工作区，或者在它下面建出新目录再开。新加 SSH 主机
//! 在这里，配对别的电脑在 `paired`。

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use gpui::{
    Action, AnyElement, App, Context, Div, Entity, FocusHandle, Focusable as _, MouseButton, Role, SharedString,
    Subscription, Window, div, img, prelude::*, px, svg,
};
use runode_files::ops::{self, OpError};
use runode_protocol::{ClientMsg, HostMsg};
use runode_ssh::Destination;

use crate::ui::picker::matches_query;

use super::{Source, clone_command, paired::AddPairedMachine, repo_name};
use crate::window::{Dropdown, TITLEBAR_HEIGHT, WindowView, files::Quoting, status_bar::host_list::dot_color};
use crate::{
    host_client::{
        hosts::{self, HostId, Status},
        paired,
    },
    project_host::ProjectHost,
    remote_hosts::{default_name, display_name, has_display_name, status_text},
    ui::{
        a11y::{A11yPress as _, Press, StaticText},
        button::{Button, ButtonSize, ButtonVariant},
        file_icons::folder_icon,
        font_size::{FontSize, scaled},
        icons::{ARROW_UP_ICON, CHEVRON_DOWN_ICON, MONITOR_ICON, PLUS_ICON, SERVER_ICON},
        input::{Input, InputEvent, InputState},
        menu::{MenuHost as _, MenuItem},
        theme::ActiveTheme as _,
        tooltip::tooltip,
    },
};

/// 列远端目录最多等这么久。
const LIST_TIMEOUT: Duration = Duration::from_secs(10);
/// 添加主机时列出的主机最多这么多个。
const MAX_HOSTS: usize = 8;
/// 主机菜单的宽度，按基准字号下的点数写：主机名和连接状态写得长。
const HOST_MENU_WIDTH: f32 = 320.;
/// 列目录的请求序号，整个进程递增：重新浏览后，先前发出的回话也对不上新的序号。
static LIST_REQUEST: AtomicU64 = AtomicU64::new(0);

/// 主机菜单里选了一台主机。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct PickWorkspaceHost(HostId);

/// 主机菜单里的「添加 SSH 主机」。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct AddSshHost;

/// 点了「添加 SSH 主机」后展开的一栏：填主机，下面列着 `~/.ssh/config` 里的主机。
pub(super) struct AddHost {
    field: Entity<InputState>,
    _events: Subscription,
    /// `~/.ssh/config` 里的主机，读好之前为空。
    hosts: Vec<String>,
}

impl AddHost {
    pub(super) fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.field.focus_handle(cx)
    }
}

/// 远端的目录浏览。
pub(super) struct Browse {
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

/// 菜单里一行写得下的连接状态：重连、连不上时不带原因。
fn short_status(status: &Status) -> String {
    match status {
        Status::Waiting { .. } => rust_i18n::t!("workspace.host_state_waiting").into_owned(),
        Status::Stopped { .. } => rust_i18n::t!("workspace.host_state_stopped").into_owned(),
        status => status_text(status),
    }
}

impl WindowView {
    /// 开着的下拉菜单是新建工作区对话框的主机菜单。
    fn host_menu_open(&self) -> bool {
        self.dropdown_shown(Dropdown::WorkspaceHost)
    }

    /// 各窗口里远端工作区的主机、这次运行里连过的主机和配对过的电脑，当前窗口的在前。
    fn known_hosts(&self, cx: &App) -> Vec<HostId> {
        let mut known: Vec<HostId> = Vec::new();
        let others = crate::window::windows(cx).into_iter().filter_map(|window| window.read(cx).ok());
        let found = self
            .workspaces
            .iter()
            .map(|workspace| workspace.host.clone())
            .chain(others.flat_map(|view| view.workspaces.iter().map(|w| w.host.clone()).collect::<Vec<_>>()))
            .chain(hosts::remotes())
            .chain(paired::machines().into_iter().map(|machine| HostId::Paired(machine.id)));
        for host in found {
            if !host.is_local() && !known.contains(&host) {
                known.push(host);
            }
        }
        known
    }

    /// 主机菜单的各项：添加远程主机（子菜单里是各种连法：SSH、Runode 配对），分隔线，本机和认识的各台主机，
    /// 选着的那台打勾。
    fn host_menu_items(&self, cx: &App) -> Vec<Option<MenuItem>> {
        let Some(dialog) = &self.new_workspace else {
            return Vec::new();
        };
        let ssh = MenuItem::label(rust_i18n::t!("workspace.host_add").into_owned(), None, Some(Box::new(AddSshHost)))
            .with_description(rust_i18n::t!("workspace.host_add_hint").into_owned());
        let pair =
            MenuItem::label(rust_i18n::t!("workspace.host_pair").into_owned(), None, Some(Box::new(AddPairedMachine)))
                .with_description(rust_i18n::t!("workspace.host_pair_hint").into_owned());
        let add =
            MenuItem::submenu(rust_i18n::t!("workspace.host_add_remote").into_owned(), vec![Some(ssh), Some(pair)])
                .with_icon(PLUS_ICON)
                .with_description(rust_i18n::t!("workspace.host_add_remote_hint").into_owned());
        let local = MenuItem::checkbox(
            rust_i18n::t!("workspace.host_local").into_owned(),
            None,
            dialog.host.is_local(),
            Box::new(PickWorkspaceHost(HostId::Local)),
        )
        .with_description(rust_i18n::t!("workspace.host_local_hint").into_owned());
        let mut known = self.known_hosts(cx);
        if !dialog.host.is_local() && !known.contains(&dialog.host) {
            known.push(dialog.host.clone());
        }
        let remotes = known.into_iter().map(|host| {
            let status = hosts::status_of(&host);
            let checked = dialog.host == host;
            let state = if matches!(host, HostId::Paired(_)) {
                rust_i18n::t!("workspace.host_paired_hint", status = short_status(&status))
            } else {
                rust_i18n::t!("workspace.host_remote_hint", status = short_status(&status))
            }
            .into_owned();
            // 起了显示名的主机，悬停时另写上连接目标。
            let tooltip = if has_display_name(&host, cx) {
                format!("{} · {}", default_name(&host), status_text(&status))
            } else {
                status_text(&status)
            };
            let icon = if matches!(host, HostId::Paired(_)) { MONITOR_ICON } else { SERVER_ICON };
            Some(
                MenuItem::checkbox(display_name(&host, cx), Some(icon), checked, Box::new(PickWorkspaceHost(host)))
                    .with_description(state)
                    .with_tooltip(tooltip),
            )
        });
        [Some(add), None, Some(local)].into_iter().chain(remotes).collect()
    }

    /// 在主机按钮下面弹出主机菜单；从键盘打开时选中第一项。
    fn open_host_menu(&mut self, from_keyboard: bool, cx: &mut Context<Self>) {
        let Some(dialog) = &self.new_workspace else {
            return;
        };
        let target = dialog.focus_handle();
        let items = self.host_menu_items(cx);
        let select_after = from_keyboard.then(|| items.len() - 1);
        self.show_dropdown(Dropdown::WorkspaceHost, items, target, select_after, cx);
        self.set_menu_width(HOST_MENU_WIDTH);
    }

    pub(super) fn pick_workspace_host(
        &mut self,
        action: &PickWorkspaceHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_workspace_host(action.0.clone(), window, cx);
    }

    /// 改在 `host` 上开：远端的开始连（之前停下的再试一次），换了主机时之前浏览的目录作废。
    pub(in crate::window) fn set_workspace_host(&mut self, host: HostId, window: &mut Window, cx: &mut Context<Self>) {
        hosts::ensure_connecting(&host);
        let Some(dialog) = &mut self.new_workspace else {
            return;
        };
        dialog.adding = None;
        dialog.pairing = None;
        if dialog.host != host {
            dialog.host = host;
            dialog.browse = None;
            dialog.error = None;
            // 本机上只有克隆在对话框里展开，别的交给系统的面板。
            if dialog.host.is_local() && dialog.source != Some(Source::Clone) {
                dialog.source = None;
            }
        }
        if let Some(source) = dialog.source {
            self.show_source(source, window, cx);
        } else {
            window.focus(&dialog.focus_handle(), cx);
        }
        cx.notify();
    }

    /// 展开「添加 SSH 主机」一栏，焦点给主机输入框，在后台读 `~/.ssh/config` 里的主机。
    pub(super) fn add_ssh_host(&mut self, _: &AddSshHost, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.new_workspace else {
            return;
        };
        dialog.pairing = None;
        if dialog.adding.is_none() {
            let field = cx.new(|cx| {
                InputState::new(String::new(), cx)
                    .with_placeholder(rust_i18n::t!("workspace.ssh_host").into_owned())
                    .with_label(rust_i18n::t!("workspace.host_add").into_owned())
            });
            let events = cx.subscribe_in(&field, window, |this, field, event: &InputEvent, window, cx| match event {
                InputEvent::Next => {
                    let name = field.read(cx).query().trim().to_owned();
                    this.connect_ssh(name, window, cx);
                }
                InputEvent::Dismiss => this.close_new_workspace(window, cx),
                InputEvent::Changed(_) => cx.notify(),
                InputEvent::Previous => {}
            });
            dialog.adding = Some(AddHost { field, _events: events, hosts: Vec::new() });
            let home = crate::window::model::home_dir();
            let job =
                cx.background_spawn(async move { home.map(|home| runode_ssh::list_hosts(&home)).unwrap_or_default() });
            cx.spawn(async move |this, cx| {
                let configured = job.await;
                this.update(cx, |this, cx| {
                    if let Some(adding) = this.new_workspace.as_mut().and_then(|dialog| dialog.adding.as_mut()) {
                        adding.hosts = configured;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        if let Some(adding) = &dialog.adding {
            window.focus(&adding.field.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// 收起「添加 SSH 主机」一栏，焦点还给对话框。
    fn cancel_add_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.new_workspace
            && dialog.adding.take().is_some()
        {
            window.focus(&dialog.focus_handle(), cx);
            cx.notify();
        }
    }

    /// 选上主机 `name` 并开始连；认不出这个写法时什么都不做。
    fn connect_ssh(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if Destination::parse(&name).is_none() {
            return;
        }
        self.set_workspace_host(HostId::Ssh(name), window, cx);
    }

    /// 主机 `host` 的连接状态变了：主机菜单开着时重列；对话框选着它、等它连上的话开始浏览目录。
    pub(in crate::window) fn new_workspace_host_changed(&mut self, host: &HostId, cx: &mut Context<Self>) {
        let Some(dialog) = &self.new_workspace else {
            return;
        };
        let selected = &dialog.host == host;
        if self.host_menu_open() {
            let items = self.host_menu_items(cx);
            self.replace_menu_items(items, cx);
        }
        if selected {
            self.start_browse(cx);
            cx.notify();
        }
    }

    /// 选着的远端主机连上了、展开了一种来源、还没开始浏览时，从远端的家目录开始浏览。
    pub(super) fn start_browse(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.new_workspace else {
            return;
        };
        if dialog.host.is_local()
            || dialog.source.is_none()
            || dialog.browse.is_some()
            || !matches!(hosts::status_of(&dialog.host), Status::Connected(_))
        {
            return;
        }
        let field =
            cx.new(|cx| InputState::new(String::new(), cx).with_label(rust_i18n::t!("workspace.ssh_dir").into_owned()));
        let events = cx.subscribe(&field, |this, field, event: &InputEvent, cx| {
            if let InputEvent::Next = event {
                let path = PathBuf::from(field.read(cx).query().trim());
                this.list_remote(Some(path), cx);
            }
        });
        dialog.browse = Some(Browse {
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
        let Some(dialog) = &mut self.new_workspace else {
            return;
        };
        let Some(browse) = dialog.browse.as_mut().filter(|_| !dialog.host.is_local()) else {
            return;
        };
        let host_id = dialog.host.clone();
        browse.loading = true;
        browse.request = LIST_REQUEST.fetch_add(1, Ordering::Relaxed) + 1;
        let request = browse.request;
        let link = hosts::link_of(&host_id);
        let job = cx.background_spawn(async move {
            link.request(|req| ClientMsg::ListDirs { req, path }, LIST_TIMEOUT).map_err(|err| err.to_string())
        });
        cx.spawn(async move |this, cx| {
            let listed = job.await;
            this.update(cx, |this, cx| {
                let Some(dialog) = &mut this.new_workspace else {
                    return;
                };
                let Some(browse) =
                    dialog.browse.as_mut().filter(|browse| dialog.host == host_id && browse.request == request)
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

    /// 在远端浏览到的目录里开工作区，关掉对话框。
    pub(super) fn open_remote_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.new_workspace else {
            return;
        };
        let Some(path) = dialog.browse.as_ref().and_then(|browse| browse.path.clone()) else {
            return;
        };
        let host = dialog.host.clone();
        self.new_workspace = None;
        self.open_remote_dir(&host, path, None, window, cx);
    }

    /// 在远端主机 `host` 的目录 `path` 上开工作区，名字按目录取；`command` 给了就在第一个终端里跑它。不跑命令、
    /// 那台主机上已经有这个目录的工作区时切过去，不另开一个。
    fn open_remote_dir(
        &mut self,
        host: &HostId,
        path: PathBuf,
        command: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if command.is_none()
            && let Some(ix) = self.workspaces.iter().position(|workspace| {
                workspace.host == *host && (workspace.dir == path || workspace.real_dir() == path)
            })
        {
            self.activate_workspace(ix, window, cx);
            return;
        }
        let Some(view) = self.spawn_terminal_on(host, Some(&path), None, window, cx) else {
            return;
        };
        if let Some(command) = command {
            view.update(cx, |view, cx| view.run_command(command, cx));
        }
        // 根目录没有目录名，用主机名。
        let label = path.file_name().is_none().then(|| default_name(host));
        self.insert_workspace(self.active + 1, path, label, view, window, cx);
    }

    /// 在远端浏览到的目录下按名字框建一个目录，在它上面开工作区；克隆（`Source::Clone`）时接着在第一个终端里
    /// 把地址框里的仓库克隆进去，名字空着时用仓库名。建不出来时原因写在对话框里，对话框还在。
    pub(super) fn create_remote_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.new_workspace else {
            return;
        };
        if dialog.host.is_local() {
            return;
        }
        let Some(parent) = dialog.browse.as_ref().and_then(|browse| browse.path.clone()) else {
            return;
        };
        let clone = dialog.source == Some(Source::Clone);
        let url = dialog.url_text(cx);
        if clone && url.is_empty() {
            return;
        }
        let typed = dialog.name_text(cx);
        let Some(dir_name) = (if typed.is_empty() && clone { repo_name(&url) } else { Some(typed) }) else {
            return;
        };
        if ops::child_path(Path::new("/"), &dir_name).is_none() {
            return;
        }
        let command = clone.then(|| {
            let os = match hosts::status_of(&dialog.host) {
                Status::Connected(connected) => connected.os,
                _ => String::new(),
            };
            clone_command(&url, Quoting::of_os(&os))
        });
        let host = dialog.host.clone();
        let dir = remote_child(&parent, &dir_name);
        dialog.error = None;
        let project = ProjectHost::of(&host);
        let job = {
            let dir = dir.clone();
            cx.background_spawn(async move { project.create(&dir, true) })
        };
        cx.spawn_in(window, async move |this, cx| {
            let created = job.await;
            this.update_in(cx, |this, window, cx| {
                // 建的时候对话框可能已经关了、换了主机。
                let Some(dialog) = this.new_workspace.as_mut().filter(|dialog| dialog.host == host) else {
                    return;
                };
                if let Err(err) = created {
                    dialog.error = Some(match err {
                        OpError::Exists => rust_i18n::t!("files.exists", name = dir_name).into_owned(),
                        OpError::NewlineInName => rust_i18n::t!("files.newline_in_name").into_owned(),
                        OpError::Io(err) => err.to_string(),
                        OpError::IntoItself => rust_i18n::t!("files.into_itself", name = dir_name).into_owned(),
                    });
                    cx.notify();
                    return;
                }
                this.new_workspace = None;
                this.open_remote_dir(&host, dir, command, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// 对话框上面的一行：「主机」，按下弹出主机菜单的按钮，选着远端主机时它的连接状态。
    pub(super) fn render_host_picker(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let dialog = self.new_workspace.as_ref()?;
        let fg = cx.theme().foreground;
        let open = self.host_menu_open();
        let name = display_name(&dialog.host, cx);
        let view = cx.entity().downgrade();
        let picker = Button::new(
            "new-workspace-host",
            rust_i18n::t!("workspace.host_button", host = name.as_str()).into_owned(),
        )
        .variant(ButtonVariant::Outline)
        .with_size(ButtonSize::Sm)
        .min_w_0()
        .cursor_pointer()
        .aria_expanded(open)
        .when(open, |button| button.bg(cx.theme().mix(0.10)))
        .child(div().min_w_0().truncate().font_weight(gpui::FontWeight::MEDIUM).child(name))
        .child(svg().flex_none().path(CHEVRON_DOWN_ICON).size(px(12.)).text_color(fg.opacity(0.6)))
        // 菜单开着时在捕获阶段就关掉、不再往下传：菜单自己的「点到外面就关」和下面再打开的都不跑。
        .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
            if this.host_menu_open() {
                cx.stop_propagation();
                this.close_menu(window, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.open_host_menu(false, cx);
            }),
        )
        .on_a11y_press(view, |this, window, cx| {
            if this.host_menu_open() {
                this.close_menu(window, cx);
            } else {
                this.open_host_menu(true, cx);
            }
        })
        // 菜单最高到窗口底边，按钮的下沿取对话框顶上这一行大致的位置。
        .when(open, |button| button.children(self.render_dropdown_menu_rightward(TITLEBAR_HEIGHT + 150., window, cx)));
        let status = (!dialog.host.is_local()).then(|| {
            let status = hosts::status_of(&dialog.host);
            let text = status_text(&status);
            let stopped = matches!(status, Status::Stopped { .. } | Status::Waiting { .. } | Status::Idle);
            let host = dialog.host.clone();
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_none().size(px(7.)).rounded_full().bg(dot_color(&status, fg)))
                .child(
                    div()
                        .id("new-workspace-host-status")
                        .role(Role::Status)
                        .aria_label(text.clone())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(fg.opacity(0.6))
                        .tooltip(tooltip(text.clone(), None))
                        .child(text),
                )
                .when(stopped, |line| {
                    line.child(
                        Button::new("ssh-retry", rust_i18n::t!("workspace.ssh_retry"))
                            .variant(ButtonVariant::Secondary)
                            .with_size(ButtonSize::Sm)
                            .cursor_pointer()
                            .on_press(cx, move |this, window, cx| this.set_workspace_host(host.clone(), window, cx)),
                    )
                })
        });
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(
                    div().flex_none().text_color(fg.opacity(0.6)).child(rust_i18n::t!("workspace.host").into_owned()),
                )
                .child(picker)
                .children(status),
        )
    }

    /// 「添加 SSH 主机」展开后的一栏：主机输入框和连接按钮，下面是 `~/.ssh/config` 里还没连过、对得上输入的主机。
    pub(super) fn render_add_host(&self, adding: &AddHost, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let field_bg = cx.theme().mix(0.03);
        let hover_bg = cx.theme().accent;
        let connect = Button::new("ssh-connect", rust_i18n::t!("workspace.ssh_connect"))
            .variant(ButtonVariant::Default)
            .h_auto()
            .py(px(5.))
            .cursor_pointer()
            .on_press(cx, |this, window, cx| {
                let adding = this.new_workspace.as_ref().and_then(|dialog| dialog.adding.as_ref());
                let name = adding.map(|adding| adding.field.read(cx).query().trim().to_owned()).unwrap_or_default();
                this.connect_ssh(name, window, cx);
            });
        let cancel = Button::new("ssh-cancel", rust_i18n::t!("workspace.host_add_cancel"))
            .variant(ButtonVariant::Ghost)
            .h_auto()
            .py(px(5.))
            .cursor_pointer()
            .on_press(cx, |this, window, cx| this.cancel_add_host(window, cx));
        let typed = adding.field.read(cx).query().to_owned();
        let known = self.known_hosts(cx);
        let hosts: Vec<AnyElement> = adding
            .hosts
            .iter()
            .filter(|host| !known.contains(&HostId::Ssh((*host).clone())))
            .filter(|host| matches_query(&[host], &typed))
            .take(MAX_HOSTS)
            .map(|host| {
                let name = host.clone();
                div()
                    .id(SharedString::from(format!("ssh-host-{host}")))
                    .role(Role::Button)
                    .aria_label(host.clone())
                    .flex_none()
                    .h(scaled(26.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(move |row| row.bg(hover_bg))
                    .child(div().flex_1().min_w_0().truncate().child(host.clone()))
                    .on_press(cx, move |this, window, cx| this.connect_ssh(name.clone(), window, cx))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(Input::new(&adding.field).flex_1().min_w_0().bg(field_bg))
                    .child(connect)
                    .child(cancel),
            )
            .child(
                div()
                    .text_size(FontSize::Small)
                    .text_color(fg.opacity(0.55))
                    .child(rust_i18n::t!("workspace.host_add_detail").into_owned()),
            )
            .when(!hosts.is_empty(), |body| {
                body.child(
                    div()
                        .text_size(FontSize::Small)
                        .text_color(fg.opacity(0.55))
                        .child(rust_i18n::t!("workspace.host_add_config").into_owned()),
                )
                .child(div().id("ssh-hosts").role(Role::List).flex().flex_col().children(hosts))
            })
    }

    /// 远端主机上展开的一种来源：目录浏览，克隆和新建另有名字框；主机还没连上时是连接状态。
    pub(super) fn render_remote_source(&self, source: Source, cx: &mut Context<Self>) -> Option<Div> {
        let dialog = self.new_workspace.as_ref().filter(|dialog| !dialog.host.is_local())?;
        let fg = cx.theme().foreground;
        let field_bg = cx.theme().mix(0.03);
        let hover_bg = cx.theme().accent;
        let body = div().flex().flex_col().gap(px(8.)).pl(px(48.)).pr(px(16.)).pb(px(12.));
        let Some(browse) = dialog.browse.as_ref() else {
            let text = rust_i18n::t!("workspace.remote_waiting", status = status_text(&hosts::status_of(&dialog.host)))
                .into_owned();
            return Some(body.child(
                div().id("ssh-waiting").aria_static_text(text.clone()).text_color(fg.opacity(0.55)).child(text),
            ));
        };
        // 列表限了高，行不收缩，多出来的滚动。
        let row = |id: SharedString, label: String, icon: AnyElement, dim: bool| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(label.clone())
                .flex_none()
                .h(scaled(26.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(move |row| row.bg(hover_bg))
                .child(icon)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(dim, |text| text.text_color(fg.opacity(0.55)))
                        .child(label),
                )
        };
        let up = browse.path.as_deref().and_then(Path::parent).map(Path::to_path_buf);
        let up_row = up.map(|parent| {
            let icon = svg().flex_none().path(ARROW_UP_ICON).size(px(14.)).text_color(fg.opacity(0.6));
            row("ssh-parent-dir".into(), rust_i18n::t!("workspace.ssh_up").into_owned(), icon.into_any_element(), false)
                .on_press(cx, move |this, _, cx| this.list_remote(Some(parent.clone()), cx))
                .into_any_element()
        });
        let dirs = browse.dirs.iter().map(|dir| {
            let next = browse.path.as_deref().map(|path| remote_child(path, dir));
            let icon = img(folder_icon(dir, false)).flex_none().size(px(16.));
            // 点开头的隐藏目录淡一些，常用的目录好找。
            row(format!("ssh-subdir-{dir}").into(), dir.clone(), icon.into_any_element(), dir.starts_with('.'))
                .on_press(cx, move |this, _, cx| this.list_remote(next.clone(), cx))
                .into_any_element()
        });
        let dirs: Vec<AnyElement> = dirs.collect();
        let label = |key: &str| {
            div().text_size(FontSize::Small).text_color(fg.opacity(0.55)).child(rust_i18n::t!(key).into_owned())
        };
        let open = (source == Source::Browse).then(|| {
            Button::new("ssh-open", rust_i18n::t!("workspace.ssh_open"))
                .variant(ButtonVariant::Default)
                .h_auto()
                .py(px(5.))
                .cursor_pointer()
                .on_press(cx, |this, window, cx| this.open_remote_workspace(window, cx))
        });
        let dir_label = if source == Source::Browse { "workspace.ssh_dir" } else { "workspace.remote_parent" };
        let name_row = dialog.name.as_ref().filter(|_| source != Source::Browse).map(|(field, _)| {
            let key = if source == Source::Clone { "workspace.remote_clone" } else { "workspace.remote_create" };
            div().flex().items_center().gap(px(8.)).child(Input::new(field).flex_1().min_w_0().bg(field_bg)).child(
                Button::new("ssh-create", rust_i18n::t!(key))
                    .variant(ButtonVariant::Default)
                    .h_auto()
                    .py(px(5.))
                    .cursor_pointer()
                    .on_press(cx, |this, window, cx| this.create_remote_workspace(window, cx)),
            )
        });
        let error = browse.error.clone().or_else(|| dialog.error.clone()).map(|error| {
            div().id("ssh-error").aria_static_text(error.clone()).text_color(fg.opacity(0.7)).child(error)
        });
        Some(
            body.child(label(dir_label))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(Input::new(&browse.field).flex_1().min_w_0().bg(field_bg))
                        .children(open),
                )
                .child(
                    div()
                        .id("ssh-dirs")
                        .role(Role::List)
                        .max_h(scaled(200.))
                        .p(px(4.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(fg.opacity(0.12))
                        .bg(field_bg)
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .when(browse.loading, |list| list.opacity(0.5))
                        .children(up_row)
                        .children(dirs),
                )
                .when(name_row.is_some(), |body| body.child(label("workspace.remote_name")))
                .children(name_row)
                .children(error),
        )
    }
}

/// 远端目录 `dir` 下的 `name`。远端的路径一律按 `/` 拼（Windows 远端也是）：Windows 上 `Path::join` 插的是 `\`。
fn remote_child(dir: &Path, name: &str) -> PathBuf {
    let mut path = dir.as_os_str().to_owned();
    if !dir.as_os_str().as_encoded_bytes().ends_with(b"/") {
        path.push("/");
    }
    path.push(name);
    path.into()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::remote_child;

    #[test]
    fn joins_remote_dirs_with_slashes() {
        assert_eq!(remote_child(Path::new("/home/u"), "src"), PathBuf::from("/home/u/src"));
        assert_eq!(remote_child(Path::new("/"), "etc"), PathBuf::from("/etc"));
    }
}
