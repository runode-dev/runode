//! 窗口底部的状态栏，右边四块，点了在上面弹出各自的浮层：
//!
//! - 防止休眠：开启、agent 在干活时、关闭三种，记在配置的 `prevent-sleep` 里，要挡时拿着一个 `system_probe::Caffeinate`；
//! - 内存和终端数：runode 自己（app、单独跑的宿主和各个终端里的进程）一共占多少内存，所有窗口里有
//!   几个终端；浮层里按 workspace 列出这个窗口里各终端的 CPU 和内存；
//! - 端口：终端里跑的程序在监听几个 TCP 端口；浮层里列出来，点了在浏览器里打开，别的程序监听的收在
//!   「外部端口」里。当前工作区在 SSH 主机上时另列那台机器上在监听的端口，点了经主连接转到本机再打开，
//!   转着的能停掉（`host_client::ssh::forward`）。
//! - 远程主机：连过几台 SSH 主机，没有时不显示；浮层里列出各台的连接状态，能连上、断开，或者去设置页管理
//!   （`host_list`）。
//!
//! 左边是 GitHub Actions 页里固定到状态栏上的工作流，见 `render_pinned_workflows`；当前工作区在 SSH 主机上时
//! 后面跟着那台主机的连接状态，连着时写出心跳测的延迟（`RemoteHost::latency`）。
//!
//! 用户点了「检查更新…」以后，查和下载期间最左边还有一块转着圈的「正在检查更新…」，见 `update::status_label`。
//!
//! 数据全 app 一份（`Status`），每 `POLL_INTERVAL` 更新一次：有窗口在前台时现问进程表，在监听的端口隔
//! `PORTS_INTERVAL` 才问一次，状态栏没显示、或者上面藏起来的几块用不着的不问；都在后台时只看 agent
//! 在不在干活，好决定挡不挡休眠。终端靠 `SessionMeta::pid`（shell 的进程号）认领它的子孙进程和这些
//! 进程监听的端口。

pub(super) mod host_list;

use crate::ui::popover::Popover;
use crate::ui::theme::ActiveTheme as _;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use gpui::{
    AccessibleAction, Action, Anchor, AnyElement, App, Context, Div, Focusable, FontWeight, Global, Hsla, MouseButton,
    MouseDownEvent, Pixels, Point, PromptLevel, Role, SharedString, Stateful, Window, WindowHandle, anchored, deferred,
    div, point, prelude::*, px, svg,
};
use runode_config::{ConfigFile, PreventSleep, StatusItem};
use runode_types::{
    agent::{AgentKind, AgentState},
    color::Rgb,
};

use super::{PressDown, ToggleStatusBar, WindowView, cards, files::MenuItem, remote};
use crate::host_client::{
    hosts::{self, HostId},
    ssh,
};
use crate::project_host::ProjectHost;
use crate::system_probe::{self as probe, Caffeinate, Helper, POLL_INTERVAL, Port, Procs, Usage};
use crate::ui::a11y::A11yPress;
use crate::ui::button::{Button, ButtonSize};
use crate::ui::menu::MenuHost as _;
use crate::ui::panel::sync_icon;
use crate::{
    config::AppConfig,
    ui::icons::{
        CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, CLOSE_ICON, COFFEE_ICON, MEMORY_ICON, PLUG_ICON, SERVER_ICON, SHELL_ICON,
    },
    ui::{
        font_size::{FontSize, scaled},
        hsla,
        tooltip::tooltip,
    },
};

/// 隔多久重新问一次在监听的端口：状态栏上只是个小数字，`lsof` 又比 `ps` 慢得多；开着端口浮层时每次都问。
const PORTS_INTERVAL: Duration = Duration::from_secs(15);
pub(super) const STATUS_BAR_HEIGHT: f32 = 26.;
/// 浮层离状态栏和窗口边留的距离。
const POPOVER_GAP: f32 = 6.;
/// 资源浮层里 CPU 和内存两列的宽度。
const CPU_WIDTH: f32 = 56.;
const MEMORY_WIDTH: f32 = 84.;
/// 状态栏显示着没有，看配置的 `status-bar`。
pub(super) fn shown(cx: &App) -> bool {
    cx.global::<AppConfig>().0.status_bar
}

/// 状态栏这时占的高度，藏起来时是 0。
pub(super) fn height(cx: &App) -> f32 {
    if shown(cx) { STATUS_BAR_HEIGHT } else { 0. }
}

/// 生效中的绿点。
const ACTIVE_DOT: Rgb = Rgb(0x34, 0xc7, 0x59);
/// 正在连、等着重连的黄点。
const PENDING_DOT: Rgb = Rgb(0xff, 0xb3, 0x40);

/// 状态栏右键菜单里的一项：显示或隐藏 `item`，记在配置的 `status-bar-hidden` 里。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct ToggleStatusItem {
    item: StatusItem,
}

/// 状态栏上一块的图标和名字，右键菜单和设置页里用。
pub(crate) fn item_icon_and_title(item: StatusItem) -> (&'static str, String) {
    let (icon, key) = match item {
        StatusItem::Sleep => (COFFEE_ICON, "status.sleep_title"),
        StatusItem::Resources => (MEMORY_ICON, "status.resources_title"),
        StatusItem::Ports => (PLUG_ICON, "status.ports_title"),
        StatusItem::Hosts => (SERVER_ICON, "status.hosts_title"),
    };
    (icon, rust_i18n::t!(key).into_owned())
}

/// 状态栏上开着的浮层；端口的浮层记着「外部端口」展开没有。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StatusPopover {
    Sleep,
    Resources,
    Ports { external: bool },
    Hosts,
}

impl StatusPopover {
    fn same_kind(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }

    /// 弹出它的那一块的标题。
    fn title(self) -> String {
        let item = match self {
            Self::Sleep => StatusItem::Sleep,
            Self::Resources => StatusItem::Resources,
            Self::Ports { .. } => StatusItem::Ports,
            Self::Hosts => StatusItem::Hosts,
        };
        item_icon_and_title(item).1
    }
}

#[derive(Default)]
struct Status {
    procs: Procs,
    ports: Vec<Port>,
    /// 各窗口当前工作区所在的远端主机上在监听的端口；问不到的主机不在里面。
    remote_ports: HashMap<HostId, Vec<Port>>,
    /// 各窗口当前工作区所在的远端主机的延迟，毫秒；变了才重画状态栏。
    latencies: HashMap<HostId, u128>,
    /// 所有窗口里各个终端的 shell 的进程号。
    shells: HashSet<u32>,
    /// 宿主在 `Welcome` 里报的进程号，见 `host`。
    host: Option<u32>,
    terminals: usize,
    /// 有 agent 在干活。
    working: bool,
    /// 照着拿放 `caffeinate` 的模式，从配置同步过来（`sync_sleep`）。
    sleep: PreventSleep,
    caffeinate: Option<Caffeinate>,
    /// 拉不起防休眠的命令，已经记过日志。
    sleep_failed: bool,
    /// 防休眠的命令没拿到锁就退出了；不再要挡或者换了模式时清掉，再试。
    sleep_refused: bool,
}

impl Global for Status {}

impl Status {
    /// 单独一个进程跑的宿主的进程号；宿主跑在 app 里时为 `None`。不按 shell 的父进程认：升级接手后
    /// shell 的父进程（旧宿主）已经退出，父进程成了 1 号进程。
    fn host(&self) -> Option<u32> {
        self.host.filter(|&host| host != std::process::id())
    }

    /// runode 一共占的：app、宿主、各个终端的 shell 和 `Helper` 各自连同子孙进程，已经算在别的里面的
    /// 不重复算。shell 要单独算，因为升级接手后它们不在 app 或宿主的子孙里。
    fn total(&self) -> Usage {
        let roots: HashSet<u32> = std::iter::once(std::process::id())
            .chain(self.host())
            .chain(self.shells.iter().copied())
            .chain(self.procs.helpers().into_iter().map(|(_, pid)| pid))
            .collect();
        self.procs.forest(&roots)
    }

    /// 监听着端口的进程是哪个终端里的：返回那个终端的 shell 的进程号。
    fn port_owner(&self, port: &Port) -> Option<u32> {
        self.procs.owner(port.pid, |pid| self.shells.contains(&pid))
    }

    /// 照现在的模式和 agent 的状态拿起或放下 `Caffeinate`。
    fn apply_sleep(&mut self) {
        match self.caffeinate.as_mut().and_then(Caffeinate::exit) {
            None => {}
            // 被外面结束了的（比如 `killall caffeinate`）放掉，下面照要不要重拉，免得一直显示生效中。
            Some(Ok(())) => self.caffeinate = None,
            // 没拿到锁：放掉，要挡的理由不变时不再重拉，免得每次轮询都拉起一个、显示生效中、再退出。
            Some(Err(err)) => {
                self.caffeinate = None;
                self.sleep_refused = true;
                self.sleep_error(&err);
            }
        }
        let want = match self.sleep {
            PreventSleep::On => true,
            PreventSleep::Agent => self.working,
            PreventSleep::Off => false,
        };
        if want && self.caffeinate.is_none() && !self.sleep_refused {
            match Caffeinate::start() {
                Ok(caffeinate) => self.caffeinate = Some(caffeinate),
                Err(err) => self.sleep_error(&err.to_string()),
            }
        } else if !want {
            self.caffeinate = None;
            self.sleep_refused = false;
        }
    }

    /// 拉不起防休眠的命令时每次轮询都会再试一遍，只把第一回记成警告。
    fn sleep_error(&mut self, err: &str) {
        if self.sleep_failed {
            tracing::debug!("cannot prevent sleep: {err}");
        } else {
            self.sleep_failed = true;
            tracing::warn!("cannot prevent sleep: {err}");
        }
    }
}

/// 开始定时更新状态栏的数据，app 启动时调一次。
pub fn watch(cx: &mut App) {
    cx.default_global::<Status>();
    sync_sleep(cx);
    cx.observe_global::<AppConfig>(sync_sleep).detach();
    cx.spawn(async move |cx| {
        let mut ports_probed: Option<Instant> = None;
        loop {
            // 状态栏没显示、上面藏起来的几块不问：内存和端口都要进程树（端口靠它认是哪个终端的），端口还要
            // `lsof`；只剩防止休眠时什么都不问，agent 在不在干活从各窗口里看。
            let (active, procs, ports, shells, hosts) = cx.update(|cx| {
                let config = &cx.global::<AppConfig>().0;
                let windows = remote::windows(cx);
                let ports = config.status_bar && !config.status_bar_hidden.contains(&StatusItem::Ports);
                let procs = ports || config.status_bar && !config.status_bar_hidden.contains(&StatusItem::Resources);
                let ports_open = windows.iter().any(|handle| {
                    handle
                        .read(cx)
                        .is_ok_and(|window| matches!(window.status_popover, Some(StatusPopover::Ports { .. })))
                });
                let ports =
                    ports && (ports_open || ports_probed.is_none_or(|probed| probed.elapsed() >= PORTS_INTERVAL));
                let hosts = if ports { remote_hosts(&windows, cx) } else { Vec::new() };
                (cx.active_window().is_some(), procs, ports, terminals(&windows, cx).0, hosts)
            });
            let probed = if active && procs {
                if ports {
                    ports_probed = Some(Instant::now());
                }
                Some(
                    cx.background_executor()
                        .spawn(async move {
                            // 等宿主连好要阻塞，放在后台问。
                            let host = crate::host_client::link().host_pid();
                            let roots = shells.into_iter().chain([std::process::id()]).chain(host).collect();
                            Probed {
                                procs: probe::processes(&roots),
                                ports: ports.then(probe::listening_ports),
                                host,
                                remote: ports.then(|| probe_remote(hosts)),
                            }
                        })
                        .await,
                )
            } else {
                None
            };
            cx.update(|cx| tick(probed, cx));
            cx.background_executor().timer(POLL_INTERVAL).await;
        }
    })
    .detach();
}

/// 各窗口里终端的 shell 的进程号、终端数，以及有没有 agent 在干活。
fn terminals(windows: &[WindowHandle<WindowView>], cx: &App) -> (HashSet<u32>, usize, bool) {
    let (mut shells, mut terminals, mut working) = (HashSet::new(), 0, false);
    for handle in windows {
        let Ok(window) = handle.read(cx) else { continue };
        for view in window.terminal_views() {
            let view = view.read(cx);
            terminals += 1;
            shells.extend(view.local_pid());
            working |=
                view.agent().is_some_and(|agent| agent.kind != AgentKind::Other && agent.state == AgentState::Working);
        }
    }
    (shells, terminals, working)
}

/// 各窗口当前工作区所在、连着的远端主机。
fn remote_hosts(windows: &[WindowHandle<WindowView>], cx: &App) -> Vec<HostId> {
    let mut found = Vec::new();
    for handle in windows {
        let Ok(window) = handle.read(cx) else { continue };
        let host = &window.workspace().host;
        if !host.is_local() && !found.contains(host) && matches!(hosts::status_of(host), hosts::Status::Connected(_)) {
            found.push(host.clone());
        }
    }
    found
}

/// 问 `found` 这几台主机上在监听的端口；问不到的不算进来。会阻塞。
fn probe_remote(found: Vec<HostId>) -> HashMap<HostId, Vec<Port>> {
    found
        .into_iter()
        .filter_map(|host| match ProjectHost::of(&host).listening_ports() {
            Ok(ports) => Some((host, ports)),
            Err(err) => {
                tracing::debug!("cannot list the ports on {host:?}: {err}");
                None
            }
        })
        .collect()
}

/// 后台问到的；这次没问端口时 `ports` 和 `remote` 为 `None`，沿用上次的。
struct Probed {
    procs: Procs,
    ports: Option<Vec<Port>>,
    host: Option<u32>,
    remote: Option<HashMap<HostId, Vec<Port>>>,
}

/// 收下问到的进程和端口，从各窗口里数终端、看 agent，再决定挡不挡休眠；状态栏上的东西变了时重画各窗口的
/// 状态栏。
fn tick(probed: Option<Probed>, cx: &mut App) {
    let windows = remote::windows(cx);
    let (shells, terminals, working) = terminals(&windows, cx);
    let status = cx.default_global::<Status>();
    let before = (status.terminals, status.caffeinate.is_some());
    let mut changed = shells != status.shells;
    if let Some(probed) = probed {
        changed |= probed.procs != status.procs || probed.host != status.host;
        status.procs = probed.procs;
        status.host = probed.host;
        if let Some(ports) = probed.ports {
            changed |= ports != status.ports;
            status.ports = ports;
        }
        if let Some(remote) = probed.remote {
            changed |= remote != status.remote_ports;
            status.remote_ports = remote;
        }
    }
    let latencies: HashMap<HostId, u128> = remote_hosts(&windows, cx)
        .into_iter()
        .filter_map(|host| {
            let latency = hosts::latency(&host)?;
            Some((host, latency.as_millis()))
        })
        .collect();
    let status = cx.default_global::<Status>();
    changed |= latencies != status.latencies;
    status.latencies = latencies;
    status.shells = shells;
    status.terminals = terminals;
    status.working = working;
    status.apply_sleep();
    changed |= before != (status.terminals, status.caffeinate.is_some());
    if changed {
        redraw_status_bars(cx);
    }
}

/// 只叫各窗口的状态栏那一块重画（浮层也画在它里面），窗口里别的缓存的几块照搬上一帧。
pub(crate) fn redraw_status_bars(cx: &mut App) {
    for handle in remote::windows(cx) {
        let _ = handle.update(cx, |view, _, cx| {
            if let Some(slots) = &view.slots {
                slots.status_bar.update(cx, |_, cx| cx.notify());
            }
        });
    }
}

/// 把主机 `host` 上的 `port` 转到本机，在浏览器里打开；转不了时在最前面的窗口上说。
fn open_remote_port(host: HostId, port: u16, cx: &mut App) {
    let HostId::Ssh(name) = &host else { return };
    let Some(remote) = ssh::find(name) else { return };
    let job = cx.background_spawn(async move { ssh::forward::forward(&remote, port) });
    cx.spawn(async move |cx| {
        let forwarded = job.await;
        cx.update(|cx| {
            match forwarded {
                Ok(local) => cx.open_url(&format!("http://localhost:{local}")),
                Err(err) => {
                    tracing::warn!("cannot forward port {port} from {host:?}: {err:#}");
                    tell_forward_failed(&err.to_string(), cx);
                }
            }
            redraw_status_bars(cx);
        });
    })
    .detach();
}

/// 停掉主机 `host` 上 `port` 到本机的转发。
fn stop_remote_port(host: HostId, port: u16, cx: &mut App) {
    let HostId::Ssh(name) = &host else { return };
    let Some(remote) = ssh::find(name) else { return };
    let job = cx.background_spawn(async move { ssh::forward::stop(&remote, port) });
    cx.spawn(async move |cx| {
        let stopped = job.await;
        cx.update(|cx| {
            if let Err(err) = stopped {
                tracing::warn!("cannot stop forwarding port {port} from {host:?}: {err:#}");
                tell_forward_failed(&err.to_string(), cx);
            }
            redraw_status_bars(cx);
        });
    })
    .detach();
}

fn tell_forward_failed(detail: &str, cx: &mut App) {
    let Some(front) = remote::front_window(cx) else { return };
    let (title, ok) = (rust_i18n::t!("status.forward_failed"), rust_i18n::t!("ssh.ok"));
    let _ =
        front.update(cx, |_, window, cx| window.prompt(PromptLevel::Warning, &title, Some(detail), &[ok.as_ref()], cx));
}

/// 改写配置文件的 `prevent-sleep`，重载的配置经 `sync_sleep` 生效。
fn set_sleep(mode: PreventSleep, cx: &mut App) {
    if let Err(err) = crate::config::set("prevent-sleep", runode_config::Named::name(mode), cx) {
        tracing::warn!("could not write prevent-sleep: {err}");
    }
}

/// 配置的 `prevent-sleep` 变了就照新的模式拿起或放下 `Caffeinate`。
fn sync_sleep(cx: &mut App) {
    let mode = cx.global::<AppConfig>().0.prevent_sleep;
    let status = cx.default_global::<Status>();
    if status.sleep == mode {
        return;
    }
    status.sleep = mode;
    status.sleep_refused = false;
    status.apply_sleep();
    redraw_status_bars(cx);
}

/// 内存写成「875.4 MB」「1.25 GB」。
fn format_bytes(bytes: u64) -> String {
    let mb = bytes as f64 / (1024. * 1024.);
    if mb < 1024. { format!("{mb:.1} MB") } else { format!("{:.2} GB", mb / 1024.) }
}

fn format_cpu(cpu: f32) -> String {
    format!("{cpu:.1}%")
}

impl WindowView {
    pub(super) fn terminal_views(&self) -> impl Iterator<Item = &gpui::Entity<crate::terminal_view::TerminalView>> {
        self.panes().map(|(_, view)| view)
    }

    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let fg_h = cx.theme().foreground;
        let host = &self.workspace().host;
        let (sleep, effective, total, terminals, ours) = match cx.try_global::<Status>() {
            Some(status) => (
                status.sleep,
                status.caffeinate.is_some(),
                status.total(),
                status.terminals,
                status.ports.iter().filter(|port| status.port_owner(port).is_some()).count()
                    + status.remote_ports.get(host).map_or(0, Vec::len),
            ),
            None => (PreventSleep::Off, false, Usage::default(), 0, 0),
        };
        let icon = |path: &'static str| svg().flex_none().path(path).size(px(13.)).text_color(fg_h.opacity(0.6));
        let dot = div().flex_none().size(px(6.)).rounded_full().bg(if effective {
            hsla(ACTIVE_DOT)
        } else {
            fg_h.opacity(0.3)
        });
        // 状态栏上每块报成一个按钮，名字是这一块的标题，上面写的数放进说明：按钮的值辅助工具多半不念。
        let sleep_state =
            if effective { rust_i18n::t!("status.sleep_active") } else { rust_i18n::t!("status.sleep_inactive") };
        let sleep_item = self
            .status_item("status-sleep", StatusPopover::Sleep, cx)
            .aria_description(format!("{} · {sleep_state}", sleep_label(sleep)))
            .child(icon(COFFEE_ICON))
            .child(sleep_label(sleep))
            .child(dot);
        let resources_item = self
            .status_item("status-resources", StatusPopover::Resources, cx)
            .tooltip(tooltip(rust_i18n::t!("status.resources_tooltip"), None))
            .aria_description(format!(
                "{}: {} · {terminals}",
                rust_i18n::t!("status.resources_tooltip"),
                format_bytes(total.memory)
            ))
            .child(icon(MEMORY_ICON))
            .child(format_bytes(total.memory))
            .child(div().text_color(fg_h.opacity(0.35)).child("·"))
            .child(icon(SHELL_ICON))
            .child(terminals.to_string());
        let ports_item = self
            .status_item("status-ports", StatusPopover::Ports { external: false }, cx)
            .aria_description(ours.to_string())
            .child(icon(PLUG_ICON))
            .child(ours.to_string());
        let update = crate::update::status_label(cx).map(|label| {
            div()
                .id("status-update")
                .role(Role::Status)
                .aria_label(label.clone())
                .flex_none()
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(5.))
                .child(sync_icon("status-update-spinner", true, 13., fg_h.opacity(0.6)))
                .child(label)
        });
        let pinned = self.render_pinned_workflows(cx);
        let host_item = (!host.is_local()).then(|| {
            let status = hosts::status_of(host);
            let latency = cx.try_global::<Status>().and_then(|status| status.latencies.get(host).copied());
            let state = match &status {
                hosts::Status::Connected(_) => {
                    latency.map_or_else(|| rust_i18n::t!("status.host_connected").into_owned(), host_list::latency_text)
                }
                hosts::Status::Connecting(_) | hosts::Status::Waiting { .. } => {
                    rust_i18n::t!("status.host_connecting").into_owned()
                }
                hosts::Status::Idle | hosts::Status::Stopped { .. } => {
                    rust_i18n::t!("status.host_offline").into_owned()
                }
            };
            let color = host_list::dot_color(&status, fg_h);
            let name = crate::remote_hosts::display_name(host, cx);
            div()
                .id("status-host")
                .role(Role::Label)
                .aria_label(format!("{name} · {state}"))
                .flex_none()
                .h(px(STATUS_BAR_HEIGHT - 6.))
                .px(px(6.))
                .flex()
                .items_center()
                .gap(px(5.))
                .tooltip(tooltip(crate::remote_hosts::status_text(&status), None))
                .child(div().flex_none().size(px(6.)).rounded_full().bg(color))
                .child(name)
                .child(div().text_color(fg_h.opacity(0.5)).child(state))
        });
        let hosts_item = (!cx.global::<AppConfig>().0.status_bar_hidden.contains(&StatusItem::Hosts))
            .then(|| self.render_hosts_item(cx));
        let hidden = &cx.global::<AppConfig>().0.status_bar_hidden;
        let shown = |item| !hidden.contains(&item);
        div()
            .id("status-bar")
            .role(Role::Toolbar)
            .aria_label(rust_i18n::t!("status.bar"))
            .flex_none()
            .w_full()
            .h(px(STATUS_BAR_HEIGHT))
            .px(px(6.))
            .flex()
            .items_center()
            .gap(px(2.))
            .text_size(FontSize::Small)
            .text_color(fg_h.opacity(0.7))
            .when(!cards(cx), |bar| bar.border_t_1().border_color(cx.theme().border))
            // 右键选显示哪几块，或者藏起整条；几块全藏起来时这一条还在，照样能右键找回来。
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_status_menu(event.position, cx);
                }),
            )
            // 左边是 GitHub Actions 页里固定的工作流，其余几块靠右。
            .children(pinned)
            .children(host_item)
            .child(div().flex_1())
            .children(update)
            .when(shown(StatusItem::Sleep), |bar| bar.child(sleep_item))
            .when(shown(StatusItem::Resources), |bar| bar.child(resources_item))
            .when(shown(StatusItem::Ports), |bar| bar.child(ports_item))
            .children(hosts_item.flatten())
    }

    /// 在 `position` 弹出状态栏的右键菜单，勾着的是显示着的几块。
    fn open_status_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let hidden = cx.global::<AppConfig>().0.status_bar_hidden.clone();
        let items = StatusItem::ALL
            .into_iter()
            .map(|item| {
                let (icon, title) = item_icon_and_title(item);
                Some(MenuItem::checkbox(
                    title,
                    Some(icon),
                    !hidden.contains(&item),
                    Box::new(ToggleStatusItem { item }),
                ))
            })
            .chain([None, Some(MenuItem::new(rust_i18n::t!("status.hide_bar"), Box::new(ToggleStatusBar), true, cx))])
            .collect();
        let target = self.focus_handle(cx);
        self.open_context_menu(position, items, target, cx);
    }

    /// 显示或隐藏状态栏上的一块：改写配置文件的 `status-bar-hidden`，所有窗口跟着重载的配置一起变。
    pub(super) fn toggle_status_item(&mut self, action: &ToggleStatusItem, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = runode_config::config_path() else { return };
        // 读不出来时 `write_values` 会再读一遍并报错，这里当作没写过。
        let written = ConfigFile::read(&path).map(|file| file.values("status-bar-hidden")).unwrap_or_default();
        let values = hidden_lines(&written, &cx.global::<AppConfig>().0.status_bar_hidden, action.item);
        if let Err(err) = crate::config::write_values(&path, "status-bar-hidden", &values, cx) {
            tracing::warn!("could not write status-bar-hidden: {err}");
        }
    }

    /// 显示或隐藏整条状态栏：改写配置文件的 `status-bar`，所有窗口跟着重载的配置一起变。
    pub(super) fn toggle_status_bar(&mut self, _: &ToggleStatusBar, _: &mut Window, cx: &mut Context<Self>) {
        let value = if shown(cx) { "false" } else { "true" };
        if let Err(err) = crate::config::set("status-bar", value, cx) {
            tracing::warn!("could not write status-bar: {err}");
        }
    }

    /// 状态栏上的一块：点了弹出 `popover`，再点一下关掉；开着时把浮层挂在它上面。
    fn status_item(&self, id: &'static str, popover: StatusPopover, cx: &mut Context<Self>) -> Stateful<Div> {
        let open = self.status_popover.filter(|open| open.same_kind(popover));
        let content = open.map(|open| match open {
            StatusPopover::Sleep => self.render_sleep_popover(cx),
            StatusPopover::Resources => self.render_resources_popover(cx),
            StatusPopover::Ports { external } => self.render_ports_popover(external, cx),
            StatusPopover::Hosts => self.render_hosts_popover(cx),
        });
        let title = popover.title();
        // 浮层的左下角对着这一块的左上角，往上让出一点；放不下时贴着窗口边挪进来。
        let content = content.map(|content| {
            div().absolute().top_0().left_0().child(
                deferred(
                    anchored()
                        .anchor(Anchor::BottomLeft)
                        .offset(point(px(0.), px(-POPOVER_GAP)))
                        .snap_to_window_with_margin(px(POPOVER_GAP))
                        .child(
                            Popover::new("status-popover")
                                .role(Role::Dialog)
                                .aria_label(title.clone())
                                .shadow_lg()
                                .child(content)
                                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                    this.status_popover = None;
                                    cx.notify();
                                })),
                        ),
                )
                .with_priority(1),
            )
        });
        let view = cx.entity().downgrade();
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(title)
            .aria_expanded(open.is_some())
            .relative()
            .flex_none()
            .h(px(STATUS_BAR_HEIGHT - 6.))
            .px(px(6.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .gap(px(5.))
            .cursor_pointer()
            .when(open.is_some(), |item| item.bg(cx.theme().mix(0.07)))
            .hover(|item| item.bg(cx.theme().mix(0.10)))
            // 浮层开着时在捕获阶段就关掉、不再往下传：浮层自己的「点到外面就关」和下面再打开的都不跑。
            .capture_any_mouse_down(cx.listener(move |this, _, _, cx| {
                if this.status_popover.is_some_and(|open| open.same_kind(popover)) {
                    cx.stop_propagation();
                    this.status_popover = None;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.status_popover = Some(popover);
                    cx.notify();
                }),
            )
            // 鼠标的开、关分在捕获和冒泡两段，辅助工具按下时这里一起办：开着就关，关着就开。
            .on_a11y_press(view, move |this, _, cx| {
                let open = this.status_popover.is_some_and(|open| open.same_kind(popover));
                this.status_popover = (!open).then_some(popover);
                cx.notify();
            })
            .children(content)
    }

    fn render_sleep_popover(&self, cx: &mut Context<Self>) -> AnyElement {
        let fg_h = cx.theme().foreground;
        let (current, effective) = cx
            .try_global::<Status>()
            .map_or((PreventSleep::Off, false), |status| (status.sleep, status.caffeinate.is_some()));
        let state =
            if effective { rust_i18n::t!("status.sleep_active") } else { rust_i18n::t!("status.sleep_inactive") };
        // 先画好标题：下面各项的闭包要借着 `cx`。
        let header = popover_header(
            None,
            rust_i18n::t!("status.sleep_title").into_owned(),
            format!("{} · {state}", sleep_label(current)),
            fg_h,
            cx,
        );
        let options = [
            (PreventSleep::On, "status.sleep_on_detail"),
            (PreventSleep::Agent, "status.sleep_agent_detail"),
            (PreventSleep::Off, "status.sleep_off_detail"),
        ]
        .into_iter()
        .enumerate()
        .map(|(ix, (mode, detail))| {
            let selected = mode == current;
            div()
                .id(("sleep-mode", ix))
                .role(Role::RadioButton)
                .aria_label(sleep_label(mode))
                .aria_description(rust_i18n::t!(detail))
                .aria_toggled(selected.into())
                .mx(px(6.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(6.))
                .flex()
                .gap(px(8.))
                .when(selected, |row| row.bg(cx.theme().popover_selected))
                .hover(|row| row.bg(cx.theme().popover_hover))
                .child(
                    div()
                        .flex_none()
                        .w(px(8.))
                        .h(scaled(16.))
                        .flex()
                        .items_center()
                        .children(selected.then(|| div().size(px(6.)).rounded_full().bg(fg_h))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().font_weight(FontWeight::MEDIUM).child(sleep_label(mode)))
                        .child(
                            div()
                                .text_size(FontSize::Small)
                                .text_color(fg_h.opacity(0.55))
                                .child(rust_i18n::t!(detail).into_owned()),
                        ),
                )
                .on_press_down(cx, move |this, _, cx| {
                    this.status_popover = None;
                    cx.notify();
                    set_sleep(mode, cx);
                })
        });
        div()
            .w(px(300.))
            .pb(px(6.))
            .flex()
            .flex_col()
            .child(header)
            .child(div().h(px(6.)))
            .child(
                div()
                    .id("sleep-modes")
                    .role(Role::RadioGroup)
                    .aria_label(rust_i18n::t!("status.sleep_title"))
                    .flex()
                    .flex_col()
                    .children(options),
            )
            .into_any_element()
    }

    fn render_resources_popover(&self, cx: &mut Context<Self>) -> AnyElement {
        let fg_h = cx.theme().foreground;
        let Some(status) = cx.try_global::<Status>() else { return div().into_any_element() };
        let procs = &status.procs;
        let total = status.total();
        // 各行没有别的标识，按画出来的先后编号。
        let next_row = std::cell::Cell::new(0_usize);
        let row = |name: SharedString, usage: Usage, indent: bool, strong: bool| {
            let n = next_row.replace(next_row.get() + 1);
            div()
                .id(("resource-row", n))
                .role(Role::Label)
                .aria_label(format!("{name} · CPU {} · {}", format_cpu(usage.cpu), format_bytes(usage.memory)))
                .h(scaled(26.))
                .px(px(12.))
                .flex()
                .items_center()
                .when(strong, |row| row.font_weight(FontWeight::SEMIBOLD))
                .child(div().flex_1().min_w_0().truncate().when(indent, |name| name.pl(px(14.))).child(name))
                .child(
                    div()
                        .flex_none()
                        .w(scaled(CPU_WIDTH))
                        .text_right()
                        .text_color(fg_h.opacity(0.7))
                        .child(format_cpu(usage.cpu)),
                )
                .child(div().flex_none().w(scaled(MEMORY_WIDTH)).text_right().child(format_bytes(usage.memory)))
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        for workspace in &self.workspaces {
            let terminals: Vec<_> = workspace
                .tabs
                .iter()
                .flat_map(|tab| tab.panes.values())
                .map(|(view, _)| {
                    let view = view.read(cx);
                    let usage = view.local_pid().map_or(Usage::default(), |pid| procs.tree(pid));
                    (SharedString::from(view.title().to_owned()), usage)
                })
                .collect();
            if terminals.is_empty() {
                continue;
            }
            let sum = terminals.iter().fold(Usage::default(), |sum, (_, usage)| sum + *usage);
            rows.push(row(workspace.name.clone(), sum, false, true).into_any_element());
            rows.extend(terminals.into_iter().map(|(title, usage)| row(title, usage, true, false).into_any_element()));
        }
        if rows.is_empty() {
            rows.push(empty_note(rust_i18n::t!("status.no_terminals").into_owned(), fg_h).into_any_element());
        }
        let app = std::process::id();
        let helpers = procs.helpers().into_iter().map(|(helper, pid)| {
            let key = match helper {
                Helper::Speech => "status.speech",
                Helper::Screen => "status.screen",
            };
            (rust_i18n::t!(key).into_owned(), pid)
        });
        let own: Vec<_> = std::iter::once((rust_i18n::t!("status.app").into_owned(), app))
            .chain(status.host().map(|host| (rust_i18n::t!("status.host").into_owned(), host)))
            .chain(helpers)
            .map(|(name, pid)| (SharedString::from(name), procs.one(pid)))
            .collect();
        let own_sum = own.iter().fold(Usage::default(), |sum, (_, usage)| sum + *usage);
        rows.push(div().h(px(1.)).my(px(4.)).bg(cx.theme().border).into_any_element());
        rows.push(row("Runode".into(), own_sum, false, true).into_any_element());
        rows.extend(own.into_iter().map(|(name, usage)| row(name, usage, true, false).into_any_element()));
        let columns = div()
            .h(scaled(24.))
            .px(px(12.))
            .flex()
            .items_center()
            .text_size(FontSize::Small)
            .text_color(fg_h.opacity(0.5))
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().child(rust_i18n::t!("status.name").into_owned()))
            .child(div().flex_none().w(scaled(CPU_WIDTH)).text_right().child("CPU"))
            .child(
                div()
                    .flex_none()
                    .w(scaled(MEMORY_WIDTH))
                    .text_right()
                    .child(rust_i18n::t!("status.memory").into_owned()),
            );
        div()
            .w(px(400.))
            .flex()
            .flex_col()
            .child(popover_header(
                Some(MEMORY_ICON),
                rust_i18n::t!("status.resources_title").into_owned(),
                format!("{} · {}", format_cpu(total.cpu), format_bytes(total.memory)),
                fg_h,
                cx,
            ))
            .child(columns)
            .child(div().id("resources-rows").max_h(px(420.)).py(px(4.)).overflow_y_scroll().children(rows))
            .into_any_element()
    }

    /// 端口浮层里当前工作区所在的远端主机 `host` 那一节：标题和那台机器上在监听的端口，点了转到本机再打开，
    /// 转着的写出本机的端口、能停掉。
    fn render_remote_ports(&self, host: &HostId, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let fg_h = cx.theme().foreground;
        let hover_bg = cx.theme().popover_hover;
        let ports = cx.try_global::<Status>().and_then(|status| status.remote_ports.get(host).cloned());
        let forwarded = match host {
            HostId::Ssh(name) => ssh::forward::forwarded(name),
            _ => Vec::new(),
        };
        let title =
            rust_i18n::t!("status.remote_ports", host = crate::remote_hosts::display_name(host, cx)).into_owned();
        let mut rows = vec![
            div()
                .id("remote-ports")
                .role(Role::Heading)
                .aria_level(3)
                .aria_label(title.clone())
                .h(scaled(26.))
                .mx(px(6.))
                .px(px(8.))
                .flex()
                .items_center()
                .text_color(fg_h.opacity(0.55))
                .child(title)
                .into_any_element(),
        ];
        let ports = ports.unwrap_or_default();
        if ports.is_empty() {
            rows.push(empty_note(rust_i18n::t!("status.no_remote_ports").into_owned(), fg_h).into_any_element());
        }
        for port in ports {
            let local = forwarded.iter().find(|(remote, _)| *remote == port.port).map(|&(_, local)| local);
            let to = local.map(|local| rust_i18n::t!("status.forwarded", port = local).into_owned());
            let label = [Some(format!(":{}", port.port)), Some(port.command.clone()), to.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            let (open_host, a11y_host) = (host.clone(), host.clone());
            let stop = local.map(|_| {
                let (host, stop_label) = (host.clone(), rust_i18n::t!("status.stop_forward").into_owned());
                Button::new(("stop-forward", usize::from(port.port)), format!("{stop_label} :{}", port.port))
                    .icon(CLOSE_ICON)
                    .with_size(ButtonSize::IconXs)
                    .tooltip(tooltip(stop_label, None))
                    // 按下就办、不往外传：外面的端口行按下就转发并打开。
                    .on_press_down(cx, move |_, _, cx| stop_remote_port(host.clone(), port.port, cx))
            });
            rows.push(
                div()
                    .id(("remote-port", usize::from(port.port)))
                    .role(Role::Link)
                    .aria_label(label)
                    .aria_description(rust_i18n::t!("status.open_port"))
                    .h(scaled(26.))
                    .mx(px(6.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .hover(|row| row.bg(hover_bg))
                    .tooltip(tooltip(rust_i18n::t!("status.open_port"), None))
                    .child(
                        div()
                            .flex_none()
                            .w(scaled(52.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!(":{}", port.port)),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_color(fg_h.opacity(0.7)).child(port.command))
                    .children(to.map(|to| div().flex_none().text_color(fg_h.opacity(0.5)).child(to)))
                    .children(stop)
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        open_remote_port(open_host.clone(), port.port, cx);
                    })
                    .on_a11y_action(AccessibleAction::Click, move |_, _, cx| {
                        open_remote_port(a11y_host.clone(), port.port, cx)
                    })
                    .into_any_element(),
            );
        }
        rows.push(div().h(px(1.)).my(px(4.)).bg(cx.theme().border).into_any_element());
        rows
    }

    fn render_ports_popover(&self, external_open: bool, cx: &mut Context<Self>) -> AnyElement {
        let fg_h = cx.theme().foreground;
        let host = self.workspace().host.clone();
        let remote = if host.is_local() { Vec::new() } else { self.render_remote_ports(&host, cx) };
        let Some(status) = cx.try_global::<Status>() else { return div().into_any_element() };
        // 这个窗口里的终端在哪个 workspace；别的窗口里的终端监听的端口不写 workspace。
        let names: HashMap<u32, SharedString> = self
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace.tabs.iter().flat_map(|tab| tab.panes.values()).map(move |(view, _)| (view, workspace))
            })
            .filter_map(|(view, workspace)| Some((view.read(cx).local_pid()?, workspace.name.clone())))
            .collect();
        let (ours, external): (Vec<_>, Vec<_>) =
            status.ports.iter().map(|port| (port, status.port_owner(port))).partition(|(_, owner)| owner.is_some());
        let hover_bg = cx.theme().popover_hover;
        let view = cx.entity().downgrade();
        let port_row = |id: &'static str, port: &Port, workspace: Option<SharedString>| {
            let url = format!("http://localhost:{}", port.port);
            let open = url.clone();
            let label = [
                Some(format!(":{}", port.port)),
                Some(port.command.clone()),
                workspace.as_ref().map(ToString::to_string),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
            div()
                .id((id, usize::from(port.port)))
                .role(Role::Link)
                .aria_label(label)
                .aria_description(rust_i18n::t!("status.open_port"))
                .h(scaled(26.))
                .mx(px(6.))
                .px(px(8.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .hover(|row| row.bg(hover_bg))
                .tooltip(tooltip(rust_i18n::t!("status.open_port"), None))
                .child(
                    div().flex_none().w(scaled(52.)).font_weight(FontWeight::MEDIUM).child(format!(":{}", port.port)),
                )
                .child(div().flex_1().min_w_0().truncate().text_color(fg_h.opacity(0.7)).child(port.command.clone()))
                .children(
                    workspace.map(|name| {
                        div().flex_none().max_w(px(120.)).truncate().text_color(fg_h.opacity(0.5)).child(name)
                    }),
                )
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    cx.open_url(&url);
                })
                .on_a11y_action(AccessibleAction::Click, move |_, _, cx| cx.open_url(&open))
        };
        let mut body: Vec<AnyElement> = ours
            .iter()
            .map(|(port, owner)| {
                port_row("port", port, owner.and_then(|owner| names.get(&owner).cloned())).into_any_element()
            })
            .collect();
        if body.is_empty() {
            body.push(empty_note(rust_i18n::t!("status.no_ports").into_owned(), fg_h).into_any_element());
        }
        let external_header = div()
            .id("external-ports")
            .role(Role::Button)
            .aria_label(rust_i18n::t!("status.external_ports"))
            .aria_description(external.len().to_string())
            .aria_expanded(external_open)
            .h(scaled(28.))
            .mx(px(6.))
            .px(px(6.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .hover(|row| row.bg(hover_bg))
            .child(
                svg()
                    .path(if external_open { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
                    .size(px(12.))
                    .text_color(fg_h.opacity(0.6)),
            )
            .child(div().flex_1().child(rust_i18n::t!("status.external_ports").into_owned()))
            .child(div().text_color(fg_h.opacity(0.5)).child(external.len().to_string()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.status_popover = Some(StatusPopover::Ports { external: !external_open });
                    cx.notify();
                }),
            )
            // `status` 借着 `cx`，用不了 `PressDown`。
            .on_a11y_press(view, move |this, _, cx| {
                this.status_popover = Some(StatusPopover::Ports { external: !external_open });
                cx.notify();
            });
        let external_rows =
            external.iter().filter(|_| external_open).map(|(port, _)| port_row("external-port", port, None));
        div()
            .w(px(340.))
            .flex()
            .flex_col()
            .child(popover_header(
                Some(PLUG_ICON),
                rust_i18n::t!("status.ports_title").into_owned(),
                rust_i18n::t!("status.ports_summary", ours = ours.len(), external = external.len()).into_owned(),
                fg_h,
                cx,
            ))
            .child(
                div()
                    .id("ports-rows")
                    .max_h(px(420.))
                    .py(px(4.))
                    .overflow_y_scroll()
                    .children(remote)
                    .children(body)
                    .child(div().h(px(1.)).my(px(4.)).bg(cx.theme().border))
                    .child(external_header)
                    .children(external_rows),
            )
            .into_any_element()
    }
}

fn sleep_label(mode: PreventSleep) -> String {
    let key = match mode {
        PreventSleep::On => "status.sleep_on",
        PreventSleep::Agent => "status.sleep_agent",
        PreventSleep::Off => "status.sleep_off",
    };
    rust_i18n::t!(key).into_owned()
}

/// 浮层里没有内容时居中的一句说明。
fn empty_note(text: String, fg: Hsla) -> Stateful<Div> {
    div()
        .id("empty-note")
        .role(Role::Label)
        .aria_label(text.clone())
        .h(scaled(40.))
        .flex()
        .items_center()
        .justify_center()
        .text_color(fg.opacity(0.5))
        .child(text)
}

/// 浮层顶上的一行：图标和标题在左，`detail` 淡淡地写在右边，下面一条分隔线。
fn popover_header(icon: Option<&'static str>, title: String, detail: String, fg: Hsla, cx: &App) -> Stateful<Div> {
    div()
        .id("popover-header")
        .role(Role::Heading)
        .aria_label(title.clone())
        .aria_description(detail.clone())
        .h(scaled(36.))
        .px(px(12.))
        .flex()
        .items_center()
        .gap(px(8.))
        .border_b_1()
        .border_color(cx.theme().border)
        .children(icon.map(|icon| svg().flex_none().path(icon).size(px(14.)).text_color(fg.opacity(0.7))))
        .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(title))
        .child(div().flex_none().text_color(fg.opacity(0.55)).child(detail))
}

/// 显示或隐藏 `item` 之后 `status-bar-hidden` 要写回的各行：`written` 是 runode 配置文件里原来写的，`hidden`
/// 是各层叠出来的。写成一行、逗号隔开，和设置页一样按 `config::clear_first` 先清掉下面一层加的，不然
/// Ghostty 的配置里也写了的那一块取消不掉。
fn hidden_lines(written: &[String], hidden: &[StatusItem], item: StatusItem) -> Vec<String> {
    let current: Vec<String> = hidden.iter().map(|item| item.name().to_owned()).collect();
    let mut hidden = hidden.to_vec();
    match hidden.iter().position(|other| *other == item) {
        Some(ix) => {
            hidden.remove(ix);
        }
        None => hidden.push(item),
    }
    let values = if hidden.is_empty() {
        Vec::new()
    } else {
        vec![hidden.iter().map(|item| item.name()).collect::<Vec<_>>().join(", ")]
    };
    crate::config::clear_first(written, &current, values)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 右键菜单里取消隐藏的一块在 Ghostty 和 runode 的配置文件里都写了时，先写一行空值清掉 Ghostty 那一层的，
    /// 取消最后一块时只剩这一行空值；只是再藏一块时不清。
    #[test]
    fn toggling_an_item_clears_the_lower_layer_when_showing_it() {
        let lines = |values: &[&str]| values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
        let (a, b) = (StatusItem::ALL[0], StatusItem::ALL[1]);
        assert_eq!(hidden_lines(&lines(&[a.name()]), &[a], a), lines(&[""]));
        assert_eq!(hidden_lines(&lines(&[a.name()]), &[a, b], a), lines(&["", b.name()]));
        assert_eq!(hidden_lines(&[], &[a], b), lines(&[&format!("{}, {}", a.name(), b.name())]));
    }

    #[test]
    fn bytes_switch_to_gigabytes() {
        assert_eq!(format_bytes(875 * 1024 * 1024 + 400 * 1024), "875.4 MB");
        assert_eq!(format_bytes(1280 * 1024 * 1024), "1.25 GB");
    }
}
