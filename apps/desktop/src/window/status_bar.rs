//! 窗口底部的状态栏，右边三块，点了在上面弹出各自的浮层：
//!
//! - 防止休眠：开启、agent 在干活时、关闭三种（`SleepMode`），要挡时拿着一个 `probe::Caffeinate`；
//! - 内存和终端数：runode 自己（app、单独跑的宿主和各个终端里的进程）一共占多少内存，所有窗口里有
//!   几个终端；浮层里按 workspace 列出这个窗口里各终端的 CPU 和内存；
//! - 端口：终端里跑的程序在监听几个 TCP 端口；浮层里列出来，点了在浏览器里打开，别的程序监听的收在
//!   「外部端口」里。
//!
//! 数据全 app 一份（`Status`），每 `POLL_INTERVAL` 更新一次：有窗口在前台时现问 `ps` 和 `lsof`，
//! 状态栏上藏起来的几块用不着的不问；都在后台时只看 agent 在不在干活，好决定挡不挡休眠。终端靠
//! `SessionMeta::pid`（shell 的进程号）认领它的子孙进程和这些进程监听的端口。

mod probe;

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use gpui::{
    Action, Anchor, AnyElement, App, Context, Div, Focusable, FontWeight, Global, Hsla, MouseButton, MouseDownEvent,
    Pixels, Point, SharedString, Stateful, Window, anchored, deferred, div, point, prelude::*, px, svg,
};
use runode_config::StatusItem;
use runode_shared_types::{
    agent::{AgentKind, AgentState},
    color::Rgb,
};

use super::{
    ToggleStatusBar, WindowView, cards, divider_color,
    files::{check_item, menu_item},
    remote,
};
use crate::{
    assets::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, COFFEE_ICON, MEMORY_ICON, PLUG_ICON, SHELL_ICON},
    config::AppConfig,
    ui::{hsla, tooltip::tooltip},
};
use probe::{Caffeinate, Port, Procs, Usage};

/// 隔多久重新问一次系统。
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// 状态栏的高度。
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

/// 状态栏右键菜单里的一项：显示或隐藏 `item`，记在配置的 `status-bar-hidden` 里。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct ToggleStatusItem {
    item: StatusItem,
}

/// 状态栏上一块的图标和名字，右键菜单里用。
fn item_icon_and_title(item: StatusItem) -> (&'static str, String) {
    let (icon, key) = match item {
        StatusItem::Sleep => (COFFEE_ICON, "status.sleep_title"),
        StatusItem::Resources => (MEMORY_ICON, "status.resources_title"),
        StatusItem::Ports => (PLUG_ICON, "status.ports_title"),
    };
    (icon, rust_i18n::t!(key).into_owned())
}

/// 什么时候防止电脑休眠。不存档，每次启动都是关闭。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SleepMode {
    On,
    /// 有 agent 在干活时。
    Agent,
    #[default]
    Off,
}

/// 状态栏上开着的浮层；端口的浮层记着「外部端口」展开没有。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StatusPopover {
    Sleep,
    Resources,
    Ports { external: bool },
}

impl StatusPopover {
    fn same_kind(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }
}

#[derive(Default)]
struct Status {
    procs: Procs,
    ports: Vec<Port>,
    /// 所有窗口里各个终端的 shell 的进程号。
    shells: HashSet<u32>,
    /// 宿主在 `Welcome` 里报的进程号，见 `host`。
    host: Option<u32>,
    terminals: usize,
    /// 有 agent 在干活。
    working: bool,
    sleep: SleepMode,
    caffeinate: Option<Caffeinate>,
}

impl Global for Status {}

impl Status {
    /// 单独一个进程跑的宿主的进程号；宿主跑在 app 里时为 `None`。不按 shell 的父进程认：升级接手后
    /// shell 的父进程（旧宿主）已经退出，父进程成了 1 号进程。
    fn host(&self) -> Option<u32> {
        self.host.filter(|&host| host != std::process::id())
    }

    /// runode 一共占的：app、宿主和各个终端的 shell 各自连同子孙进程，已经算在别的里面的不重复算。
    /// shell 要单独算，因为升级接手后它们不在 app 或宿主的子孙里。
    fn total(&self) -> Usage {
        let roots: HashSet<u32> =
            std::iter::once(std::process::id()).chain(self.host()).chain(self.shells.iter().copied()).collect();
        self.procs.forest(&roots)
    }

    /// 监听着端口的进程是哪个终端里的：返回那个终端的 shell 的进程号。
    fn port_owner(&self, port: &Port) -> Option<u32> {
        self.procs.owner(port.pid, |pid| self.shells.contains(&pid))
    }

    /// 照现在的模式和 agent 的状态拿起或放下 `Caffeinate`。
    fn apply_sleep(&mut self) {
        let want = match self.sleep {
            SleepMode::On => true,
            SleepMode::Agent => self.working,
            SleepMode::Off => false,
        };
        if want && self.caffeinate.is_none() {
            match Caffeinate::start() {
                Ok(caffeinate) => self.caffeinate = Some(caffeinate),
                Err(err) => tracing::warn!("cannot prevent sleep: {err}"),
            }
        } else if !want {
            self.caffeinate = None;
        }
    }
}

/// 开始定时更新状态栏的数据，app 启动时调一次。
pub fn watch(cx: &mut App) {
    cx.default_global::<Status>();
    cx.spawn(async move |cx| {
        loop {
            // 状态栏上藏起来的几块不问：内存和端口都要进程树（端口靠它认是哪个终端的），端口还要 `lsof`；
            // 只剩防止休眠时什么都不问，agent 在不在干活从各窗口里看。
            let (active, procs, ports) = cx.update(|cx| {
                let hidden = &cx.global::<AppConfig>().0.status_bar_hidden;
                let ports = !hidden.contains(&StatusItem::Ports);
                let procs = ports || !hidden.contains(&StatusItem::Resources);
                (cx.active_window().is_some(), procs, ports)
            });
            let probed = if active && procs {
                Some(
                    cx.background_executor()
                        .spawn(async move {
                            // 等宿主连好要阻塞，放在后台问。
                            let ports = if ports { probe::listening_ports() } else { Vec::new() };
                            (probe::processes(), ports, crate::host_client::link().host_pid())
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

/// 收下问到的进程和端口，从各窗口里数终端、看 agent，再决定挡不挡休眠，重画各窗口。
fn tick(probed: Option<(Procs, Vec<Port>, Option<u32>)>, cx: &mut App) {
    let windows = remote::windows(cx);
    let (mut shells, mut terminals, mut working) = (HashSet::new(), 0, false);
    for handle in &windows {
        let Ok(window) = handle.read(cx) else { continue };
        for view in window.terminal_views() {
            let view = view.read(cx);
            terminals += 1;
            shells.extend(view.meta().pid);
            working |=
                view.agent().is_some_and(|agent| agent.kind != AgentKind::Other && agent.state == AgentState::Working);
        }
    }
    let status = cx.default_global::<Status>();
    let before = (status.terminals, status.caffeinate.is_some());
    let mut changed = probed.is_some();
    if let Some((procs, ports, host)) = probed {
        status.procs = procs;
        status.ports = ports;
        status.host = host;
    }
    status.shells = shells;
    status.terminals = terminals;
    status.working = working;
    status.apply_sleep();
    changed |= before != (status.terminals, status.caffeinate.is_some());
    if changed {
        for handle in windows {
            let _ = handle.update(cx, |_, _, cx| cx.notify());
        }
    }
}

fn set_sleep(mode: SleepMode, cx: &mut App) {
    let status = cx.default_global::<Status>();
    status.sleep = mode;
    status.apply_sleep();
    cx.refresh_windows();
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
    /// 这个窗口里所有的终端。
    fn terminal_views(&self) -> impl Iterator<Item = &gpui::Entity<crate::terminal_view::TerminalView>> {
        self.workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.panes.values())
            .map(|(view, _)| view)
    }

    /// 窗口底部的状态栏。
    pub(super) fn render_status_bar(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let fg_h = hsla(fg);
        let (sleep, effective, total, terminals, ours) = match cx.try_global::<Status>() {
            Some(status) => (
                status.sleep,
                status.caffeinate.is_some(),
                status.total(),
                status.terminals,
                status.ports.iter().filter(|port| status.port_owner(port).is_some()).count(),
            ),
            None => (SleepMode::Off, false, Usage::default(), 0, 0),
        };
        let icon = |path: &'static str| svg().flex_none().path(path).size(px(13.)).text_color(fg_h.opacity(0.6));
        let dot = div().flex_none().size(px(6.)).rounded_full().bg(if effective {
            hsla(ACTIVE_DOT)
        } else {
            fg_h.opacity(0.3)
        });
        let sleep_item = self
            .status_item("status-sleep", StatusPopover::Sleep, fg, bg, cx)
            .child(icon(COFFEE_ICON))
            .child(sleep_label(sleep))
            .child(dot);
        let resources_item = self
            .status_item("status-resources", StatusPopover::Resources, fg, bg, cx)
            .tooltip(tooltip(rust_i18n::t!("status.resources_tooltip"), None, fg, bg))
            .child(icon(MEMORY_ICON))
            .child(format_bytes(total.memory))
            .child(div().text_color(fg_h.opacity(0.35)).child("·"))
            .child(icon(SHELL_ICON))
            .child(terminals.to_string());
        let ports_item = self
            .status_item("status-ports", StatusPopover::Ports { external: false }, fg, bg, cx)
            .child(icon(PLUG_ICON))
            .child(ours.to_string());
        let hidden = &cx.global::<AppConfig>().0.status_bar_hidden;
        let shown = |item| !hidden.contains(&item);
        div()
            .flex_none()
            .h(px(STATUS_BAR_HEIGHT))
            .px(px(6.))
            .flex()
            .items_center()
            .justify_end()
            .gap(px(2.))
            .text_size(px(11.))
            .text_color(fg_h.opacity(0.7))
            .when(!cards(cx), |bar| bar.border_t_1().border_color(divider_color(fg_h)))
            // 右键选显示哪几块，或者藏起整条；几块全藏起来时这一条还在，照样能右键找回来。
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_status_menu(event.position, cx);
                }),
            )
            .when(shown(StatusItem::Sleep), |bar| bar.child(sleep_item))
            .when(shown(StatusItem::Resources), |bar| bar.child(resources_item))
            .when(shown(StatusItem::Ports), |bar| bar.child(ports_item))
    }

    /// 在 `position` 弹出状态栏的右键菜单，勾着的是显示着的几块。
    fn open_status_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let hidden = cx.global::<AppConfig>().0.status_bar_hidden.clone();
        let items = StatusItem::ALL
            .into_iter()
            .map(|item| {
                let (icon, title) = item_icon_and_title(item);
                Some(check_item(title, Some(icon), !hidden.contains(&item), Box::new(ToggleStatusItem { item })))
            })
            .chain([None, Some(menu_item("status.hide_bar", Box::new(ToggleStatusBar), true, cx))])
            .collect();
        let target = self.focus_handle(cx);
        self.open_menu(position, items, target, cx);
    }

    /// 显示或隐藏状态栏上的一块：改写配置文件的 `status-bar-hidden`，所有窗口跟着重载的配置一起变。
    pub(super) fn toggle_status_item(&mut self, action: &ToggleStatusItem, _: &mut Window, cx: &mut Context<Self>) {
        let mut hidden = cx.global::<AppConfig>().0.status_bar_hidden.clone();
        match hidden.iter().position(|item| *item == action.item) {
            Some(ix) => {
                hidden.remove(ix);
            }
            None => hidden.push(action.item),
        }
        let values: Vec<String> = if hidden.is_empty() {
            Vec::new()
        } else {
            vec![hidden.iter().map(|item| item.name()).collect::<Vec<_>>().join(", ")]
        };
        let Some(path) = runode_config::config_path() else { return };
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
    fn status_item(
        &self,
        id: &'static str,
        popover: StatusPopover,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let open = self.status_popover.filter(|open| open.same_kind(popover));
        let content = open.map(|open| match open {
            StatusPopover::Sleep => self.render_sleep_popover(fg, bg, cx),
            StatusPopover::Resources => self.render_resources_popover(fg, cx),
            StatusPopover::Ports { external } => self.render_ports_popover(external, fg, bg, cx),
        });
        let popover_bg = hsla(bg.mix(fg, 0.04));
        let border = hsla(fg).opacity(0.15);
        // 浮层的左下角对着这一块的左上角，往上让出一点；放不下时贴着窗口边挪进来。
        let content = content.map(|content| {
            div().absolute().top_0().left_0().child(
                deferred(
                    anchored()
                        .anchor(Anchor::BottomLeft)
                        .offset(point(px(0.), px(-POPOVER_GAP)))
                        .snap_to_window_with_margin(px(POPOVER_GAP))
                        .child(
                            div()
                                .id("status-popover")
                                .rounded(px(8.))
                                .border_1()
                                .border_color(border)
                                .bg(popover_bg)
                                .shadow_lg()
                                .text_size(px(12.))
                                .text_color(hsla(fg))
                                .occlude()
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
        div()
            .id(id)
            .relative()
            .flex_none()
            .h(px(STATUS_BAR_HEIGHT - 6.))
            .px(px(6.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .gap(px(5.))
            .cursor_pointer()
            .when(open.is_some(), |item| item.bg(hsla(bg.mix(fg, 0.07))))
            .hover(|item| item.bg(hsla(bg.mix(fg, 0.10))))
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
            .children(content)
    }

    fn render_sleep_popover(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> AnyElement {
        let fg_h = hsla(fg);
        let (current, effective) = cx
            .try_global::<Status>()
            .map_or((SleepMode::Off, false), |status| (status.sleep, status.caffeinate.is_some()));
        let state =
            if effective { rust_i18n::t!("status.sleep_active") } else { rust_i18n::t!("status.sleep_inactive") };
        let options = [
            (SleepMode::On, "status.sleep_on_detail"),
            (SleepMode::Agent, "status.sleep_agent_detail"),
            (SleepMode::Off, "status.sleep_off_detail"),
        ]
        .into_iter()
        .enumerate()
        .map(|(ix, (mode, detail))| {
            let selected = mode == current;
            div()
                .id(("sleep-mode", ix))
                .mx(px(6.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(6.))
                .flex()
                .gap(px(8.))
                .when(selected, |row| row.bg(hsla(bg.mix(fg, 0.07))))
                .hover(|row| row.bg(hsla(bg.mix(fg, 0.10))))
                .child(
                    div()
                        .flex_none()
                        .w(px(8.))
                        .h(px(16.))
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
                                .text_size(px(11.))
                                .text_color(fg_h.opacity(0.55))
                                .child(rust_i18n::t!(detail).into_owned()),
                        ),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.status_popover = None;
                        set_sleep(mode, cx);
                    }),
                )
        });
        div()
            .w(px(300.))
            .pb(px(6.))
            .flex()
            .flex_col()
            .child(popover_header(
                None,
                rust_i18n::t!("status.sleep_title").into_owned(),
                format!("{} · {state}", sleep_label(current)),
                fg_h,
            ))
            .child(div().h(px(6.)))
            .children(options)
            .into_any_element()
    }

    fn render_resources_popover(&self, fg: Rgb, cx: &mut Context<Self>) -> AnyElement {
        let fg_h = hsla(fg);
        let Some(status) = cx.try_global::<Status>() else { return div().into_any_element() };
        let procs = &status.procs;
        let total = status.total();
        let row = |name: SharedString, usage: Usage, indent: bool, strong: bool| {
            div()
                .h(px(26.))
                .px(px(12.))
                .flex()
                .items_center()
                .when(strong, |row| row.font_weight(FontWeight::SEMIBOLD))
                .child(div().flex_1().min_w_0().truncate().when(indent, |name| name.pl(px(14.))).child(name))
                .child(
                    div()
                        .flex_none()
                        .w(px(CPU_WIDTH))
                        .text_right()
                        .text_color(fg_h.opacity(0.7))
                        .child(format_cpu(usage.cpu)),
                )
                .child(div().flex_none().w(px(MEMORY_WIDTH)).text_right().child(format_bytes(usage.memory)))
        };
        let mut rows: Vec<AnyElement> = Vec::new();
        for workspace in &self.workspaces {
            let terminals: Vec<_> = workspace
                .tabs
                .iter()
                .flat_map(|tab| tab.panes.values())
                .map(|(view, _)| {
                    let view = view.read(cx);
                    let usage = view.meta().pid.map_or(Usage::default(), |pid| procs.tree(pid));
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
            rows.push(
                div()
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(fg_h.opacity(0.5))
                    .child(rust_i18n::t!("status.no_terminals").into_owned())
                    .into_any_element(),
            );
        }
        let app = std::process::id();
        let own: Vec<_> = std::iter::once((rust_i18n::t!("status.app").into_owned(), app))
            .chain(status.host().map(|host| (rust_i18n::t!("status.host").into_owned(), host)))
            .map(|(name, pid)| (SharedString::from(name), procs.one(pid)))
            .collect();
        let own_sum = own.iter().fold(Usage::default(), |sum, (_, usage)| sum + *usage);
        rows.push(div().h(px(1.)).my(px(4.)).bg(divider_color(fg_h)).into_any_element());
        rows.push(row("Runode".into(), own_sum, false, true).into_any_element());
        rows.extend(own.into_iter().map(|(name, usage)| row(name, usage, true, false).into_any_element()));
        let columns = div()
            .h(px(24.))
            .px(px(12.))
            .flex()
            .items_center()
            .text_size(px(11.))
            .text_color(fg_h.opacity(0.5))
            .border_b_1()
            .border_color(divider_color(fg_h))
            .child(div().flex_1().child(rust_i18n::t!("status.name").into_owned()))
            .child(div().flex_none().w(px(CPU_WIDTH)).text_right().child("CPU"))
            .child(
                div().flex_none().w(px(MEMORY_WIDTH)).text_right().child(rust_i18n::t!("status.memory").into_owned()),
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
            ))
            .child(columns)
            .child(div().id("resources-rows").max_h(px(420.)).py(px(4.)).overflow_y_scroll().children(rows))
            .into_any_element()
    }

    fn render_ports_popover(&self, external_open: bool, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> AnyElement {
        let fg_h = hsla(fg);
        let Some(status) = cx.try_global::<Status>() else { return div().into_any_element() };
        // 这个窗口里的终端在哪个 workspace；别的窗口里的终端监听的端口不写 workspace。
        let names: HashMap<u32, SharedString> = self
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace.tabs.iter().flat_map(|tab| tab.panes.values()).map(move |(view, _)| (view, workspace))
            })
            .filter_map(|(view, workspace)| Some((view.read(cx).meta().pid?, workspace.name.clone())))
            .collect();
        let (ours, external): (Vec<_>, Vec<_>) =
            status.ports.iter().map(|port| (port, status.port_owner(port))).partition(|(_, owner)| owner.is_some());
        let hover_bg = hsla(bg.mix(fg, 0.10));
        let port_row = |id: &'static str, port: &Port, workspace: Option<SharedString>| {
            let url = format!("http://localhost:{}", port.port);
            div()
                .id((id, usize::from(port.port)))
                .h(px(26.))
                .mx(px(6.))
                .px(px(8.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .hover(|row| row.bg(hover_bg))
                .tooltip(tooltip(rust_i18n::t!("status.open_port"), None, fg, bg))
                .child(div().flex_none().w(px(52.)).font_weight(FontWeight::MEDIUM).child(format!(":{}", port.port)))
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
        };
        let mut body: Vec<AnyElement> = ours
            .iter()
            .map(|(port, owner)| {
                port_row("port", port, owner.and_then(|owner| names.get(&owner).cloned())).into_any_element()
            })
            .collect();
        if body.is_empty() {
            body.push(
                div()
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(fg_h.opacity(0.5))
                    .child(rust_i18n::t!("status.no_ports").into_owned())
                    .into_any_element(),
            );
        }
        let external_header = div()
            .id("external-ports")
            .h(px(28.))
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
            );
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
            ))
            .child(
                div()
                    .id("ports-rows")
                    .max_h(px(420.))
                    .py(px(4.))
                    .overflow_y_scroll()
                    .children(body)
                    .child(div().h(px(1.)).my(px(4.)).bg(divider_color(fg_h)))
                    .child(external_header)
                    .children(external_rows),
            )
            .into_any_element()
    }
}

fn sleep_label(mode: SleepMode) -> String {
    let key = match mode {
        SleepMode::On => "status.sleep_on",
        SleepMode::Agent => "status.sleep_agent",
        SleepMode::Off => "status.sleep_off",
    };
    rust_i18n::t!(key).into_owned()
}

/// 浮层顶上的一行：图标和标题在左，`detail` 淡淡地写在右边，下面一条分隔线。
fn popover_header(icon: Option<&'static str>, title: String, detail: String, fg: Hsla) -> Div {
    div()
        .h(px(36.))
        .px(px(12.))
        .flex()
        .items_center()
        .gap(px(8.))
        .border_b_1()
        .border_color(divider_color(fg))
        .children(icon.map(|icon| svg().flex_none().path(icon).size(px(14.)).text_color(fg.opacity(0.7))))
        .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(title))
        .child(div().flex_none().text_color(fg.opacity(0.55)).child(detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_switch_to_gigabytes() {
        assert_eq!(format_bytes(875 * 1024 * 1024 + 400 * 1024), "875.4 MB");
        assert_eq!(format_bytes(1280 * 1024 * 1024), "1.25 GB");
    }
}
