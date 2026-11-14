//! 窗口左侧的 workspace 列表：切换、拖动排序、改名、关闭和新建；顶上是找 agent、新建任务的一行，下面是在等你
//! 回答的 agent（`inbox`），最下面是导航（`nav`），列表和导航之间是后台会话（`background`）。workspace 按项目
//! （`project_group`）归组；有 workspace 在别的主机上时先按主机分组，每组一个组头，能收起、在那台主机上新建。
//! 每行一行字：状态标记和名字，右边是 agent 的名字，没有 agent 时是和项目不同的当前分支（`repo`）。

pub(super) mod inbox;
mod nav;
mod project_group;
mod repo;

pub(super) use project_group::RepoProject;
use std::path::PathBuf;

use crate::WorkspaceId;
use crate::ui::a11y::RevealOnHover;
use crate::ui::button::{Button, ButtonSize, Toggle};
use crate::ui::icons::system_logos;
use crate::ui::panel::split_handle;
use crate::ui::theme::ActiveTheme as _;
use gpui::{
    AccessibleAction, Action, AnyElement, App, Axis, Context, Div, ExternalPaths, Focusable, FontWeight, Hsla,
    Modifiers, MouseButton, MouseDownEvent, Orientation, Render, Role, SharedString, Stateful, Window,
    WindowControlArea, div, prelude::*, px, relative, svg,
};

use super::{
    AGENT_MARK_SIZE_SMALL, CARD_GAP, CloseWorkspace, DIVIDER_GRAB_WIDTH, DIVIDER_STEP, Divider, NewTab, NewWorkspace,
    PressDown, RenameTarget, RenameWorkspace, Renaming, SelectLastWorkspace, SelectWorkspace, TAB_CLOSE_SIZE,
    TITLEBAR_HEIGHT, TRAFFIC_LIGHTS_WIDTH, ToggleSidebar, WindowView,
    agents::{Mark, Status as AgentStatus, logo::agent_logo},
    background, cards, drag_window,
    inline_edit::InlineEdit,
    pane_move::DraggedPane,
    titlebar::{agent_mark, drag_chip, drop_marker, shortcut_hint},
};
use crate::ui::a11y::A11yPress;
use crate::ui::menu::{MenuHost as _, MenuItem};
use crate::{
    host_client::hosts::{self, HostId, Status},
    remote_hosts::{self, status_text},
    ui::icons::{
        CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, MONITOR_ICON, PENCIL_ICON, PLUS_ICON, SERVER_ICON, SIDEBAR_ICON,
    },
    ui::{
        font_size::{self, FontSize, scaled},
        hsla,
        status_mark::{MarkKind, status_mark},
        theme::{ADDED, MODIFIED, REMOVED},
        tooltip::tooltip,
    },
};
pub(super) use repo::RepoBadge;

/// 侧栏的默认宽度，比红绿灯宽得多，红绿灯落在侧栏顶上。
const SIDEBAR_WIDTH: f32 = 240.;
/// 拖动侧栏宽度的范围；最窄也要放得下红绿灯。
const SIDEBAR_MIN_WIDTH: f32 = 140.;
const SIDEBAR_MAX_WIDTH: f32 = 480.;
/// 红绿灯右边收起、展开侧栏的按钮。
const SIDEBAR_TOGGLE_WIDTH: f32 = 28.;
const SIDEBAR_TOGGLE_HEIGHT: f32 = 24.;
/// 开关按钮离窗口左边多远：紧挨着红绿灯；全屏时没有红绿灯，只留一点边。别的系统用窗口管理器的标题栏，
/// 窗口里也没有红绿灯。
fn sidebar_toggle_left(fullscreen: bool) -> f32 {
    if fullscreen || !cfg!(target_os = "macos") { 8. } else { TRAFFIC_LIGHTS_WIDTH }
}
/// 侧栏收着时标题栏左边让出的宽度：红绿灯（全屏时没有）和开关按钮，再空一点才到标签，图标离两边差不多远。
pub(super) fn sidebar_toggle_inset(fullscreen: bool) -> f32 {
    sidebar_toggle_left(fullscreen) + SIDEBAR_TOGGLE_WIDTH + 8.
}
/// 侧栏的底色：卡片样式下侧栏直接画在外框上。
fn sidebar_background(cx: &App) -> Hsla {
    if cards(cx) { cx.theme().frame } else { cx.theme().mix(0.03) }
}

/// workspace 行右边的 agent 图标，一个 agent 一个，单色的 logo 染成 `fg`。右下角的圆点是那个 agent 的状态：
/// 等回答琥珀色，在干活蓝色，干完了没看绿色，空闲不画；圆点描一圈 `ring`，压在图标上也分得开。认不出是哪家的
/// agent 画成提示符。图标定宽、不能截断，所以按 `room`（可用的宽度）少画几个，画不下的并进「+N」，至少画一个。
fn agent_icons(agents: &[Mark], room: f32, ring: Hsla, fg: Hsla, cx: &App) -> Div {
    let theme = cx.theme();
    let size = px(AGENT_ICON_SIZE);
    // 「+N」按每个字符约一个等宽字宽估。
    let more_width = |more: usize| {
        if more == 0 { 0. } else { AGENT_ICON_GAP + (more.to_string().len() + 1) as f32 * 7. * font_size::scale(cx) }
    };
    let fits = |shown: usize| {
        let icons = shown as f32 * AGENT_ICON_SIZE + shown.saturating_sub(1) as f32 * AGENT_ICON_GAP;
        icons + more_width(agents.len() - shown) <= room
    };
    let shown = (1..=agents.len().min(MAX_AGENT_ICONS)).rev().find(|&shown| fits(shown)).unwrap_or(1);
    let icons = agents.iter().take(shown).map(|agent| {
        let logo = agent_logo(agent.kind, size, fg)
            .unwrap_or_else(|| status_mark("workspace-agent-other", MarkKind::Idle, AGENT_ICON_SIZE, cx));
        let dot = match agent.status {
            AgentStatus::Blocked => Some(theme.warning),
            AgentStatus::Working => Some(theme.info),
            AgentStatus::Done => Some(theme.success),
            AgentStatus::Idle => None,
        };
        let outer = AGENT_DOT_SIZE + AGENT_DOT_RING * 2.;
        div().relative().flex_none().flex().child(logo).children(dot.map(|color| {
            div()
                .absolute()
                .right(px(-outer / 2. + 1.))
                .bottom(px(-outer / 2. + 1.))
                .size(px(outer))
                .rounded_full()
                .bg(ring)
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(px(AGENT_DOT_SIZE)).rounded_full().bg(color))
        }))
    });
    let more = agents.len() - shown;
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(AGENT_ICON_GAP))
        .children(icons)
        .when(more > 0, |row| row.child(div().flex_none().text_size(FontSize::Small).child(format!("+{more}"))))
}

/// 侧栏顶上几段（搜索行、「需要你」、项目栏）之间的间距。
const SECTION_GAP: f32 = 18.;
/// 侧栏里各段离侧栏左右两边的留白。
const SIDEBAR_INSET: f32 = 6.;
/// 每个 workspace 一项，一行字。
const WORKSPACE_ROW_HEIGHT: f32 = 30.;
/// workspace 行左右的留白。
const WORKSPACE_ROW_PADDING: f32 = 8.;
/// workspace 行右边那格（分支或 agent 图标、快捷键提示、响铃）最多占行宽的比例，余下的留给名字。
const TRAILING_MAX_WIDTH: f32 = 0.5;
const TRAILING_GAP: f32 = 6.;
const BELL_DOT_SIZE: f32 = 6.;
/// 项目组头下面的 workspace 行往右缩进：左边的竖线离组头左边 `TREE_LINE_INSET`，行离竖线 `TREE_ROW_INSET`。
const TREE_LINE_INSET: f32 = 10.;
const TREE_ROW_INSET: f32 = 8.;
/// workspace 行右边最多画几个 agent 图标，多出来的写成「+N」。
const MAX_AGENT_ICONS: usize = 3;
const AGENT_ICON_SIZE: f32 = 14.;
/// 图标之间的间距，要放得下伸出图标的半个状态点。
const AGENT_ICON_GAP: f32 = 7.;
const AGENT_DOT_SIZE: f32 = 6.;
const AGENT_DOT_RING: f32 = 1.5;
/// 没选中的行的字色，导航和 workspace 行一样。
const ROW_TEXT_OPACITY: f32 = 0.78;
/// 主机组头和导航每行名字前的图标那一格，两处的文字才对齐。
const ROW_ICON_SIZE: f32 = 16.;
/// 改名输入框的高度。
const RENAME_FIELD_HEIGHT: f32 = 18.;
/// 按主机分组时每组顶上的组头。
const HOST_HEADER_HEIGHT: f32 = 22.;
/// 组头上连接状态的圆点。
const HOST_STATUS_DOT: f32 = 6.;
/// 叠在系统图标右上角的连接状态圆点，连同和图标隔开的那圈描边。
const HOST_STATUS_BADGE: f32 = 7.;

/// 快捷键要按着的修饰键，查法和 `shortcut_hint` 一样：先找 `select` 的绑定，`is_last` 时再找 `last` 的，
/// 都取最后一个绑定的第一键。
pub(super) fn shortcut_modifiers(select: &dyn Action, last: &dyn Action, is_last: bool, cx: &App) -> Option<Modifiers> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let modifiers = |action: &dyn Action| {
        keymap.bindings_for_action(action).next_back()?.keystrokes().first().map(|k| *k.modifiers())
    };
    modifiers(select).or_else(|| if is_last { modifiers(last) } else { None })
}

/// 侧栏写分支、目录时用的等宽字体。
pub(super) fn mono_font(cx: &App) -> SharedString {
    crate::ui::font::mono(cx).unwrap_or_default()
}

/// 拖动中的 workspace：拖动时跟着鼠标画出来，放到另一行上时按 `id` 挪位置。
#[derive(Clone)]
struct DraggedWorkspace {
    id: WorkspaceId,
    /// 开始拖动时所在的位置，用来决定落点提示画在目标行的上边还是下边。
    ix: usize,
    name: SharedString,
    /// 只能挪到同一台主机、同一个项目的组里。
    host: HostId,
    project: PathBuf,
    /// 侧栏当前的宽度，预览照这个宽度画。
    width: f32,
}

impl Render for DraggedWorkspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 底色和当前 workspace 那一行一样，宽度也照项目下缩进的行。
        let bg = cx.theme().tab_active;
        let width = self.width - 2. * SIDEBAR_INSET - TREE_LINE_INSET - TREE_ROW_INSET;
        drag_chip(px(width), px(WORKSPACE_ROW_HEIGHT * font_size::scale(cx)), self.name.clone(), bg, cx)
    }
}

impl WindowView {
    /// 用户手动收起或展开过就按那个来，否则多于一个 workspace 或者有后台会话时显示；改名时总要
    /// 显示。
    pub(super) fn sidebar_visible(&self) -> bool {
        self.renaming.is_some() || self.sidebar_shown.unwrap_or(self.workspaces.len() > 1 || background::any())
    }

    pub(super) fn sidebar_width(&self) -> f32 {
        self.sidebar_width.unwrap_or(SIDEBAR_WIDTH)
    }

    /// 拖动分隔线时侧栏的右边跟到窗口里的横坐标 `x`。
    pub(super) fn resize_sidebar(&mut self, x: f32) {
        self.sidebar_width = Some(x.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH));
    }

    /// 侧栏右边的分隔线只有一像素宽，在它两侧放一条透明的把手供拖动。盖在窗口的最上层，
    /// 伸进终端区的一半才能先于终端接到鼠标。
    pub(super) fn render_sidebar_handle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        // 卡片样式下分隔线落在侧栏和卡片之间的空隙中间。
        let spacing = if cards(cx) { CARD_GAP / 2. } else { 0. };
        let label = rust_i18n::t!("divider.sidebar").into_owned();
        let pressed = |this: &mut Self, double: bool, cx: &mut Context<Self>| {
            if double {
                // 双击恢复默认宽度。
                this.sidebar_width = None;
                this.save(cx);
            } else {
                this.dragging_divider = Some(Divider::Sidebar);
            }
        };
        let nudge = |this: &mut Self, delta: f32, _: &mut Window, cx: &mut Context<Self>| {
            this.resize_sidebar(this.sidebar_width() + delta * DIVIDER_STEP);
            this.save(cx);
        };
        split_handle("sidebar-divider", false, label, cx, pressed, nudge)
            .absolute()
            .top_0()
            .h_full()
            .left(px(self.sidebar_width() + spacing - DIVIDER_GRAB_WIDTH / 2.))
            .w(px(DIVIDER_GRAB_WIDTH))
    }

    /// 红绿灯右边收起、展开侧栏的按钮，侧栏收着时也在原处，不随侧栏跳动。
    pub(super) fn render_sidebar_toggle(&self, fullscreen: bool, cx: &mut Context<Self>) -> Toggle {
        let text = if self.sidebar_visible() {
            rust_i18n::t!("tooltip.hide_sidebar")
        } else {
            rust_i18n::t!("tooltip.show_sidebar")
        };
        let tooltip = tooltip(text.clone(), Some(&ToggleSidebar));
        // 外观一直是没按下的样子，只报给辅助工具侧栏开着没有。
        Toggle::new("sidebar-toggle", SIDEBAR_ICON, text)
            .aria_toggled(self.sidebar_visible().into())
            .absolute()
            .left(px(sidebar_toggle_left(fullscreen)))
            .top(px((TITLEBAR_HEIGHT - SIDEBAR_TOGGLE_HEIGHT) / 2.))
            .w(px(SIDEBAR_TOGGLE_WIDTH))
            .h(px(SIDEBAR_TOGGLE_HEIGHT))
            .tooltip(tooltip)
            // 压在拖窗口的区域上：不挡住的话 Windows 把这里当标题栏，按下被拿去拖窗口。
            .occlude()
            .on_press_down(cx, |this, window, cx| this.toggle_sidebar(&ToggleSidebar, window, cx))
    }

    pub(super) fn render_sidebar(&mut self, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let modifiers = window.modifiers();
        let inbox = self.render_inbox(window, cx);
        let rows = self.render_workspace_rows(modifiers, cx);
        let drop_bg = cx.theme().mix(0.08);
        div()
            .id("sidebar")
            .flex_none()
            .w(px(self.sidebar_width()))
            .h_full()
            .flex()
            .flex_col()
            .when(!cards(cx), |sidebar| sidebar.bg(sidebar_background(cx)).border_r_1().border_color(cx.theme().border))
            // 从访达拖来的东西悬在侧栏上时提示放下能开 workspace；放下由窗口根上的 `open_dropped_dirs` 办。
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_bg))
            .text_size(FontSize::Body)
            // 顶上这条放红绿灯（全屏时没有）和侧栏开关，和标题栏一样能拖动窗口、双击缩放，比标题栏
            // 多留一点，第一行不贴着开关。
            .child(
                div()
                    .flex_none()
                    .h(px(TITLEBAR_HEIGHT + 6.))
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(MouseButton::Left, drag_window),
            )
            .child(self.render_sidebar_search(cx))
            .children(inbox.map(|inbox| div().flex_none().mt(px(SECTION_GAP)).px(px(SIDEBAR_INSET)).child(inbox)))
            .child(self.render_projects_bar(cx).mt(px(SECTION_GAP)))
            .child(
                div()
                    .id("workspace-list")
                    .role(Role::TabList)
                    .aria_label(rust_i18n::t!("workspace.list"))
                    .aria_orientation(Orientation::Vertical)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .px(px(SIDEBAR_INSET))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(rows),
            )
            .children(self.render_background(cx))
            .child(self.render_sidebar_nav(cx))
    }

    /// 列表里的各行：按项目分段，每段前面是项目的组头（`render_project_groups`）。有在别的主机上的就先按主机
    /// 分组，每组前面是主机的组头，收起的主机组只留当前 workspace 那一行。要求同一台主机、同一个项目的
    /// workspace 在列表里挨着。
    fn render_workspace_rows(&self, modifiers: Modifiers, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let grouped = self.workspaces.iter().any(|workspace| !workspace.host.is_local());
        let mut rows = Vec::new();
        let mut start = 0;
        for group in self.workspaces.chunk_by(|a, b| a.host == b.host) {
            let range = start..start + group.len();
            start = range.end;
            if grouped {
                let host = &group[0].host;
                let collapsed = self.collapsed_hosts.contains(host);
                // 收起时藏起来的 workspace 里有要用户处理的，组头上提示一下。
                let attention =
                    collapsed && range.clone().any(|ix| ix != self.active && self.workspace_needs_attention(ix, cx));
                rows.push(self.render_host_header(host, range.start == 0, collapsed, attention, cx).into_any_element());
                if collapsed {
                    rows.extend(
                        range
                            .filter(|ix| *ix == self.active)
                            .map(|ix| self.render_row(ix, modifiers, cx).into_any_element()),
                    );
                    continue;
                }
            }
            rows.extend(self.render_project_groups(range, modifiers, cx));
        }
        rows
    }

    /// 一组的组头：主机的图标、名字和远端的连接状态，悬停时露出在这台主机上新建、给远端主机改名和收起的
    /// 按钮；按下收起、展开。
    fn render_host_header(
        &self,
        host: &HostId,
        first: bool,
        collapsed: bool,
        attention: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let fg = cx.theme().foreground;
        // 状态圆点的描边和侧栏同色。
        let background = sidebar_background(cx);
        let name = match host {
            HostId::Local => rust_i18n::t!("workspace.host_local_group").into_owned(),
            remote => remote_hosts::display_name(remote, cx),
        };
        // 起了显示名的主机，提示和辅助工具里另写上连接目标。
        let destination = remote_hosts::has_display_name(host, cx).then(|| remote_hosts::default_name(host));
        let renaming = self
            .renaming
            .as_ref()
            .filter(|renaming| matches!(&renaming.target, RenameTarget::Host(renaming) if host == renaming));
        let status = (!host.is_local()).then(|| hosts::status_of(host));
        let status_color = status.as_ref().map(|status| match status {
            Status::Connected(_) => hsla(ADDED),
            Status::Connecting(_) | Status::Waiting { .. } => hsla(MODIFIED),
            Status::Stopped { .. } => hsla(REMOVED),
            Status::Idle => fg.opacity(0.3),
        });
        let status_text = status.as_ref().map(status_text);
        // 按系统画 logo：本机看自己的，SSH 主机看上次连上时报的，没连上过的画成服务器；配对的电脑一律画成显示器，
        // 和 SSH 主机分得开，系统写在说明里。
        let system = remote_hosts::system(host, cx);
        let paired = matches!(host, HostId::Paired(_));
        let icon = match system {
            _ if paired => MONITOR_ICON,
            Some("macos") => system_logos::APPLE,
            Some("linux") => system_logos::LINUX,
            Some("windows") => system_logos::WINDOWS,
            _ => SERVER_ICON,
        };
        let system_name = system.and_then(remote_hosts::system_name).map(str::to_owned);
        let a11y_description = [
            paired.then(|| rust_i18n::t!("workspace.host_paired").into_owned()),
            system_name,
            destination.clone(),
            status_text.clone(),
            attention.then(|| rust_i18n::t!("workspace.host_attention").into_owned()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let tooltip_text = match (&destination, status_text) {
            (Some(destination), Some(status)) => Some(format!("{destination} · {status}")),
            (destination, status) => status.or_else(|| destination.clone()),
        };
        let key = SharedString::from(match host {
            HostId::Local => "host-local".to_owned(),
            HostId::Ssh(name) => format!("host-ssh-{name}"),
            HostId::Paired(id) => format!("host-paired-{id}"),
        });
        let new_label = rust_i18n::t!("workspace.new_on_host", host = name.as_str()).into_owned();
        let group = SharedString::from(format!("{key}-group"));
        let rename = (!host.is_local() && renaming.is_none()).then(|| {
            let rename_label = rust_i18n::t!("workspace.rename_host", host = name.as_str()).into_owned();
            let label = host.clone();
            Button::new(SharedString::from(format!("{key}-rename")), rename_label.clone())
                .with_size(ButtonSize::IconXs)
                .icon(PENCIL_ICON)
                .icon_size(12.)
                .tooltip(tooltip(rename_label, None))
                .opacity(0.)
                .group_hover(group.clone(), |button| button.opacity(1.))
                .on_press_down(cx, move |this, window, cx| this.start_rename_host(label.clone(), window, cx))
        });
        let toggle = {
            let host = host.clone();
            move |this: &mut Self, expand: Option<bool>, cx: &mut Context<Self>| {
                let collapsed = this.collapsed_hosts.contains(&host);
                if expand.is_some_and(|expand| expand != collapsed) {
                    return;
                }
                if collapsed {
                    this.collapsed_hosts.retain(|collapsed| *collapsed != host);
                } else {
                    this.collapsed_hosts.push(host.clone());
                }
                this.save(cx);
                cx.notify();
            }
        };
        let a11y = |expand: Option<bool>, cx: &mut Context<Self>| {
            let toggle = toggle.clone();
            let view = cx.entity().downgrade();
            move |_: Option<&gpui::accesskit::ActionData>, _: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| toggle(this, expand, cx)).ok();
            }
        };
        let expand = a11y(Some(true), cx);
        let collapse = a11y(Some(false), cx);
        let flip = a11y(None, cx);
        let press = toggle.clone();
        let new_on = host.clone();
        let editing = renaming.is_some();
        let warning = cx.theme().warning;
        div()
            .id(key.clone())
            .role(Role::Button)
            .aria_label(name.clone())
            .aria_expanded(!collapsed)
            .when(!a11y_description.is_empty(), |header| header.aria_description(a11y_description))
            .on_a11y_action(AccessibleAction::Expand, expand)
            .on_a11y_action(AccessibleAction::Collapse, collapse)
            .flex_none()
            .h(scaled(HOST_HEADER_HEIGHT))
            .group(group.clone())
            // 组和组之间多空一些，组头和自己的第一行挨近，一眼看出谁归谁。
            .mt(px(if first { 2. } else { 10. }))
            .mb(px(2.))
            .pl(px(8.))
            .pr(px(6.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(FontSize::Small)
            .text_color(fg.opacity(0.45))
            .hover(|header| header.text_color(fg.opacity(0.75)))
            .when(!editing, |header| header.when_some(tooltip_text, |header, text| header.tooltip(tooltip(text, None))))
            // 改名时按在输入框里是挪光标、选字，不收起。辅助工具的按下、展开、收起各自登记：用通用的按下
            // 会把展开、收起也当成按下再切一次。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if !editing {
                        press(this, None, cx);
                    }
                }),
            )
            .on_a11y_action(AccessibleAction::Click, flip)
            // 连接状态的圆点叠在系统图标的右上角，描一圈背景色和图标隔开。
            .child(
                div()
                    .flex_none()
                    .relative()
                    .size(px(ROW_ICON_SIZE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(svg().path(icon).size(px(12.)).text_color(fg.opacity(0.4)))
                    .children(status_color.map(|color| {
                        div()
                            .absolute()
                            .right(px(0.))
                            .top(px(0.))
                            .size(px(HOST_STATUS_BADGE))
                            .rounded_full()
                            .border_1()
                            .border_color(background)
                            .bg(color)
                    })),
            )
            // 名字长时截短。
            .child(div().flex_1().min_w_0().flex().items_center().gap(px(6.)).map(|line| {
                match renaming {
                    Some(renaming) => line.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(renaming.edit.render(px(RENAME_FIELD_HEIGHT * font_size::scale(cx)))),
                    ),
                    None => line.child(div().min_w_0().truncate().child(name)),
                }
            }))
            .when(attention, |header| {
                header.child(div().flex_none().size(px(HOST_STATUS_DOT)).rounded_full().bg(warning))
            })
            .children(rename)
            .when(!editing, |header| {
                header.child(
                    Button::new(SharedString::from(format!("{key}-new")), new_label.clone())
                        .with_size(ButtonSize::IconXs)
                        .icon(PLUS_ICON)
                        .icon_size(12.)
                        .tooltip(tooltip(new_label, None))
                        // 悬停在组头上才显出来，几个组头不一排排都是按钮；用透明度藏，辅助工具照样按得到。
                        .opacity(0.)
                        .group_hover(group.clone(), |button| button.opacity(1.))
                        .on_press_down(cx, move |this, window, cx| this.new_workspace_on(new_on.clone(), window, cx)),
                )
            })
            // 收起时箭头一直显示，提醒这组里还有藏着的行。
            .child(
                div()
                    .flex_none()
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(!collapsed, |chevron| chevron.opacity(0.).group_hover(group, |chevron| chevron.opacity(1.)))
                    .child(
                        svg()
                            .path(if collapsed { CHEVRON_RIGHT_ICON } else { CHEVRON_DOWN_ICON })
                            .size(px(12.))
                            .text_color(fg.opacity(0.35)),
                    ),
            )
    }

    /// 打开新建工作区的对话框，选好在 `host` 上开。
    fn new_workspace_on(&mut self, host: HostId, window: &mut Window, cx: &mut Context<Self>) {
        self.new_workspace(&NewWorkspace, window, cx);
        if self.new_workspace.is_some() {
            self.set_workspace_host(host, window, cx);
        }
    }

    fn render_row(&self, ix: usize, modifiers: Modifiers, cx: &mut Context<Self>) -> Stateful<Div> {
        let workspace = &self.workspaces[ix];
        let id = workspace.id;
        // 开着配对设备页、看板时高亮的是导航里的那一行。
        let active = ix == self.active && !matches!(self.page, Some(super::Page::Mobile(_) | super::Page::Kanban(_)));
        let theme = cx.theme();
        let (active_bg, hover_bg) = (theme.tab_active, theme.accent);
        let (muted, subtle) = (theme.muted_foreground, theme.subtle_foreground);
        let close_label = rust_i18n::t!("menu.close_workspace");
        let close_tooltip = tooltip(close_label.clone(), None);
        let fg = cx.theme().foreground;
        let group = SharedString::from(format!("workspace-{ix}"));
        let renaming = self.renaming.as_ref().filter(|renaming| renaming.target == RenameTarget::Workspace(id));
        let repo = &workspace.repo;
        let mark = workspace.mark(cx);
        let agents = workspace.agent_marks(cx);
        let live = !workspace.tabs.is_empty();
        let host = (!workspace.host.is_local()).then(|| {
            let status = hosts::status_of(&workspace.host);
            format!("{} {}", remote_hosts::display_name(&workspace.host, cx), status_text(&status))
        });
        // 目录不画出来，悬停时看提示；和名字一样（比如家目录的 `~`）时不提示。
        let dir = Some(workspace.host.display_dir(&workspace.dir)).filter(|dir| *dir != *workspace.name);
        // 名字之外报给辅助工具的：主机、分支、目录，以及只画成图形的各个 agent 和它们的状态、响铃。
        let a11y_description = [host, repo.branch.as_ref().map(ToString::to_string), dir.clone()]
            .into_iter()
            .flatten()
            .chain(agents.iter().map(|agent| agent.describe()))
            .chain((!live).then(|| rust_i18n::t!("workspace.no_terminals").into_owned()))
            .chain(workspace.bell().then(|| rust_i18n::t!("workspace.bell").into_owned()))
            .collect::<Vec<_>>()
            .join(" · ");
        // 名字前是状态：有 agent 时是它的标记，没有时开着终端是 shell 的提示符，一个终端都没有是空心圆点。当前
        // 行的光标不闪，用户正看着它。
        let blocked = mark.is_some_and(|mark| mark.status == AgentStatus::Blocked);
        let status = if mark.is_some() || live {
            agent_mark(mark, ("workspace-agent", id), AGENT_MARK_SIZE_SMALL, !active && blocked, cx)
        } else {
            div()
                .flex_none()
                .size(px(AGENT_MARK_SIZE_SMALL))
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(px(7.)).rounded_full().border_1().border_color(muted))
                .into_any_element()
        };
        let name: AnyElement = match renaming {
            Some(renaming) => renaming.edit.render(px(RENAME_FIELD_HEIGHT * font_size::scale(cx))).into_any_element(),
            None => div()
                .min_w_0()
                .truncate()
                .when(active, |name| name.font_weight(FontWeight::MEDIUM))
                .child(workspace.name.clone())
                .into_any_element(),
        };
        // 名字右边是各个 agent 的图标；没有 agent 时，分支和项目组头上写的不一样才用等宽字写出来。状态点的
        // 描边不跟着悬停的底色变：悬停时右边整个隐去。
        let ring = if active { active_bg } else { sidebar_background(cx) };
        let logo_fg = if active { fg } else { fg.opacity(ROW_TEXT_OPACITY) };
        // 右边那格最多占行宽的 `TRAILING_MAX_WIDTH`；行宽照项目下缩进的行估（竖线 1px），响铃的圆点另占一格。
        let row_width = self.sidebar_width()
            - 2. * SIDEBAR_INSET
            - TREE_LINE_INSET
            - 1.
            - TREE_ROW_INSET
            - 2. * WORKSPACE_ROW_PADDING;
        let room = row_width * TRAILING_MAX_WIDTH - if workspace.bell() { BELL_DOT_SIZE + TRAILING_GAP } else { 0. };
        let label = if agents.is_empty() {
            repo.branch
                .clone()
                .filter(|branch| {
                    self.main_worktree_branch(&workspace.host, workspace.project_dir())
                        .is_none_or(|(_, main)| main != branch)
                })
                .map(|branch| div().min_w_0().truncate().font_family(mono_font(cx)).child(branch))
        } else {
            Some(agent_icons(&agents, room, ring, logo_fg, cx))
        };
        // 按着快捷键的修饰键或者单按 ⌘ 时这里换成快捷键提示（按 ⌘ 是找快捷键时最先按的键）；后面是响铃的圆点。
        // 悬停时这些都让给关闭按钮。
        let is_last = ix + 1 == self.workspaces.len();
        let held = modifiers == Modifiers::command()
            || shortcut_modifiers(&SelectWorkspace(ix), &SelectLastWorkspace, is_last, cx) == Some(modifiers);
        let trailing = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(TRAILING_GAP))
            .text_size(FontSize::Small)
            .text_color(if active { subtle } else { muted })
            .group_hover(group.clone(), |trailing| trailing.invisible())
            .map(|trailing| {
                if held {
                    trailing.children(shortcut_hint(&SelectWorkspace(ix), &SelectLastWorkspace, is_last, cx))
                } else {
                    trailing.children(label)
                }
            })
            .when(workspace.bell(), |trailing| {
                trailing.child(div().flex_none().size(px(BELL_DOT_SIZE)).rounded_full().bg(fg.opacity(0.8)))
            });
        let dragged = DraggedWorkspace {
            id,
            ix,
            name: workspace.name.clone(),
            host: workspace.host.clone(),
            project: workspace.project_dir().to_path_buf(),
            width: self.sidebar_width(),
        };
        let group_key = (workspace.host.clone(), workspace.project_dir().to_path_buf());
        let drop_key = group_key.clone();
        let view = cx.entity().downgrade();
        div()
            // 改名时换一个 id：双击那次按下被 GPUI 记作待拖动，改名期间不挂 `on_drag`，松开时也就没人清它，
            // 换了 id 旧的元素状态连同这次按下一起丢掉，改完名不会一动鼠标就拖起整行。
            .id(if renaming.is_some() { ("workspace-renaming", id) } else { ("workspace", id) })
            .role(Role::Tab)
            .aria_label(workspace.name.clone())
            .aria_selected(active)
            .when(!a11y_description.is_empty(), |row| row.aria_description(a11y_description))
            .group(group.clone())
            .flex_none()
            .h(scaled(WORKSPACE_ROW_HEIGHT))
            .px(px(WORKSPACE_ROW_PADDING))
            .rounded(px(7.))
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(FontSize::Medium)
            .map(|row| {
                if active {
                    row.bg(active_bg).text_color(fg)
                } else {
                    row.text_color(fg.opacity(ROW_TEXT_OPACITY)).hover(|row| row.bg(hover_bg).text_color(fg))
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // 点在改名输入框里时交给输入框挪光标、选字。
                    if this.renaming.as_ref().is_some_and(|renaming| renaming.target == RenameTarget::Workspace(id)) {
                        return;
                    }
                    cx.stop_propagation();
                    // 按 id 找行号：画出这一行以后 workspace 可能已经增删、挪了位置（手机端、命令行开关的），
                    // 照画的时候的行号会切到别的 workspace 上，甚至越界。
                    let Some(ix) = this.workspaces.iter().position(|w| w.id == id) else { return };
                    if !this.leave_todo_page(window, cx) {
                        return;
                    }
                    if event.click_count >= 2 {
                        this.start_rename(ix, window, cx);
                    } else {
                        this.activate_workspace(ix, window, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_workspace_menu(id, event.position, window, cx);
                }),
            )
            // 按下要看双击，用不了 `PressDown`；辅助工具按下时就是单击切过去，改名走菜单里的「重命名」。
            .on_a11y_press(view, move |this, window, cx| {
                if let Some(ix) = this.workspaces.iter().position(|w| w.id == id)
                    && this.leave_todo_page(window, cx)
                {
                    this.activate_workspace(ix, window, cx);
                }
            })
            // 改名时在输入框里拖选文字，不能把整行拖走。
            .when(renaming.is_none(), |row| row.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone())))
            // 只能在同一台主机、同一个项目的组里挪。
            .drag_over::<DraggedWorkspace>(move |style, dragged, _, cx| {
                if (&dragged.host, &dragged.project) == (&group_key.0, &group_key.1) {
                    drop_marker(style, dragged.ix, ix, Axis::Vertical, cx)
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedWorkspace, window, cx| {
                if (&dragged.host, &dragged.project) == (&drop_key.0, &drop_key.1) {
                    this.move_workspace(dragged.id, ix, window, cx);
                }
            }))
            // 拖来的分屏：成为这个 workspace 排在最后的新标签。
            .drag_over::<DraggedPane>(move |style, _, _, cx| style.bg(cx.theme().ring.opacity(0.18)))
            .on_drop(cx.listener(move |this, dragged: &DraggedPane, window, cx| {
                this.drop_pane_as_tab(dragged.pane, id, window, cx);
            }))
            // 右键菜单从指针处弹出，指针还停在行上，提示会盖住菜单的第一项。
            .when_some(dir.filter(|_| self.menu().is_none()), |row, dir| row.tooltip(tooltip(dir, None)))
            .child(status)
            .child(div().flex_1().min_w_0().child(name))
            .child(
                div()
                    .flex_none()
                    .relative()
                    .h_full()
                    .min_w(px(TAB_CLOSE_SIZE))
                    .max_w(relative(TRAILING_MAX_WIDTH))
                    .flex()
                    .items_center()
                    .justify_end()
                    .child(trailing)
                    .child(
                        Button::new(("workspace-close", id), close_label)
                            .with_size(ButtonSize::IconXs)
                            .text_size(px(14.))
                            .child("×")
                            .absolute()
                            .right_0()
                            .top(relative(0.5))
                            .mt(px(-TAB_CLOSE_SIZE / 2.))
                            .reveal_on_hover(group)
                            .tooltip(close_tooltip)
                            .on_press_down(cx, move |this, window, cx| {
                                if let Some(ix) = this.workspaces.iter().position(|w| w.id == id) {
                                    this.confirm_close_workspace(ix, window, cx);
                                }
                            }),
                    ),
            )
    }

    /// 右键点了 workspace `id`：先切过去，再在 `position` 弹出新建标签、改名、关掉这几样的菜单，都对着它办。
    fn open_workspace_menu(
        &mut self,
        id: WorkspaceId,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 快捷键按切过去之前的焦点找：刚切过去的终端上一帧还没画，键位表里找不到它。
        let lookup = self.focus_handle(cx);
        let Some(ix) = self.workspaces.iter().position(|w| w.id == id) else { return };
        if !self.leave_todo_page(window, cx) {
            return;
        }
        self.activate_workspace(ix, window, cx);
        let item = |key: &str, action: Box<dyn Action>| {
            let shortcut = crate::ui::tooltip::shortcut_text_in(action.as_ref(), &lookup, window);
            Some(MenuItem::new(rust_i18n::t!(key), action, true, cx).with_shortcut(shortcut))
        };
        let items = vec![
            item("menu.new_tab", Box::new(NewTab)),
            item("menu.rename_workspace", Box::new(RenameWorkspace)),
            None,
            item("menu.close_workspace", Box::new(CloseWorkspace)),
        ];
        let focus = self.focus_handle(cx);
        self.open_context_menu(position, items, focus, cx);
    }

    pub(super) fn rename_workspace(&mut self, _: &RenameWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        self.start_rename(self.active, window, cx);
    }

    /// 在侧栏里把第 `ix` 个 workspace 的名字换成输入框，原名全选，直接打字就替换掉。
    fn start_rename(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_rename(true, window, cx);
        let workspace = &self.workspaces[ix];
        let id = workspace.id;
        let name = workspace.name.to_string();
        let select = name.len();
        let edit = InlineEdit::new(
            name,
            select,
            rust_i18n::t!("workspace.edit_name").into_owned(),
            Self::finish_rename,
            window,
            cx,
        );
        self.renaming = Some(Renaming { target: RenameTarget::Workspace(id), edit });
        self.sidebar_scroll.scroll_to_item(ix);
        cx.notify();
    }

    /// 在侧栏里把远端主机 `host` 组头上的名字换成输入框，现在的显示名全选。
    fn start_rename_host(&mut self, host: HostId, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_rename(true, window, cx);
        let text = remote_hosts::display_name(&host, cx);
        let select = text.len();
        let edit = InlineEdit::new(
            text,
            select,
            rust_i18n::t!("workspace.edit_host_name").into_owned(),
            Self::finish_rename,
            window,
            cx,
        );
        self.renaming = Some(Renaming { target: RenameTarget::Host(host), edit });
        cx.notify();
    }

    /// 结束改名；`commit` 时用输入框里的名字。workspace 的名字空的不算；主机的显示名清空时改回默认的名字。
    fn finish_rename(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        let name = renaming.edit.text(cx);
        match &renaming.target {
            RenameTarget::Workspace(id) => {
                if commit
                    && !name.is_empty()
                    && let Some(ix) = self.workspaces.iter().position(|w| w.id == *id)
                {
                    self.set_workspace_name(ix, name.into(), window, cx);
                }
            }
            RenameTarget::Host(host) => {
                if commit {
                    remote_hosts::set_display_name(host, &name, cx);
                }
            }
        }
        renaming.edit.release_focus(&self.focus_handle(cx), window, cx);
        cx.notify();
    }

    /// 把第 `ix` 个 workspace 改名为 `name`；没有终端时窗口标题用的是它的名字，跟着改。
    pub(super) fn set_workspace_name(
        &mut self,
        ix: usize,
        name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspaces[ix].name = name;
        if ix == self.active && self.tab().is_none() {
            window.set_window_title(&self.workspace().name);
        }
        self.save(cx);
        cx.notify();
    }
}
