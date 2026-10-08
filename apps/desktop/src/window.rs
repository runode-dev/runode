//! 一个窗口：左侧列出 workspace 的侧栏，顶部的标签栏，标签里的分屏，以及右侧可以打开的
//! 预览栏、Git 面板和文件树。workspace 对应一个项目目录，各有一组标签；窗口的布局随改随存，下次启动时
//! 恢复。
//!
//! 这里是窗口的根视图 `WindowView`、窗口绑定的动作，以及把各部分拼起来的渲染。其余按职责分在
//! 子模块里：workspace、标签和分屏的数据与增删切换（`model`）、动作的处理（`actions`）、
//! agent 的状态标记、提醒和跳转（`agents`，系统通知和提示音在 `agents::alert`）、列出所有 agent
//! 的浮层（`agent_picker`）、标签里的分屏（`panes`）、标题栏和标签（`titlebar`）、侧栏（`sidebar`）、
//! 右侧的预览栏、Git 面板和文件树（`project`、`preview`、`git_panel`、`files`），侧栏和文件树共用的
//! 就地输入框（`inline_edit`），新建 workspace 的对话框（`new_workspace`），开窗口（`open`），存档（`persist`，存档文件的格式在
//! `persist::format`），侧栏里没在窗口里显示的后台会话（`background`），退出和关窗口时会话怎么办
//! （`quit`），侧栏顶上手机端入口打开的引导页（`mobile`），以及别的进程经宿主请 app 开终端、切到某个终端、问各个终端摆在哪（`remote`、
//! `layout_report`），一次在当前分屏旁开几个分屏（`arrange`），卡片样式下标题栏左边的这台
//! 机器（`machine`），以及窗口底部的状态栏（`status_bar`）。
//!
//! 窗口有两种样子，按配置的 `WindowStyle` 画：卡片样式（`render_cards_body`）和经典样式
//! （`render_classic_body`）。

mod actions;
mod agent_picker;
mod agents;
mod arrange;
mod background;
mod clipboard;
mod files;
mod git_panel;
mod inline_edit;
mod layout_report;
mod machine;
mod mobile;
mod model;
mod new_workspace;
mod open;
mod panes;
mod persist;
mod preview;
mod project;
mod quit;
mod remote;
mod sidebar;
mod status_bar;
mod tasks;
mod titlebar;

use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc, time::Instant};

use futures::StreamExt as _;
use gpui::{
    Action, AnyElement, App, BoxShadow, Context, EntityId, ExternalPaths, FocusHandle, Focusable, Hsla, MouseButton,
    MouseDownEvent, Render, ScrollHandle, SharedString, Subscription, Task, Window, WindowBounds, actions, div, point,
    prelude::*, px,
};
use runode_config::WindowStyle;
use runode_shared_types::{
    color::Rgb,
    pane::{Axis, Direction, SplitId},
};

pub(crate) use agents::{
    alert::{listen as listen_notifications, notifications_denied, test as test_notification},
    logo::FILES as AGENT_LOGO_FILES,
    reveal_notified,
};
pub use arrange::ArrangePanes;
pub use background::refresh as watch_background;
pub use files::{
    CollapseSelectedFile, CopyPath, CopyRelativePath, DeleteFile, ExpandSelectedFile, FocusTerminal, OpenSelectedFile,
    RenameFile, RevealInFinder, SelectFirstFile, SelectLastFile, SelectNextFile, SelectPreviousFile,
};
pub use machine::load as load_machine;
pub(crate) use open::{open_window, open_window_with};
pub use persist::{install, saved_window_options};
pub use quit::{
    close_all_windows, close_window, end_sessions_in_menu, quit, quit_and_end_sessions, quit_to_update, should_close,
    terminal_windows,
};
pub use remote::serve_requests;
pub use status_bar::watch as watch_status;
pub use titlebar::titlebar_options;

use crate::{config::AppConfig, prespawn::Prespawned, terminal_view::TerminalView, ui::hsla};
use model::{PaneLayout, Workspace, WorkspaceId, home_dir};
use persist::format::SavedWindow;
use titlebar::titled;

actions!(
    runode,
    [
        NewTab,
        CloseTab,
        NextTab,
        PreviousTab,
        SelectLastTab,
        NewSplitRight,
        NewSplitDown,
        /// 关掉当前分屏；标签里只剩它时关掉整个标签。
        ClosePane,
        FocusNextPane,
        FocusPreviousPane,
        EqualizePanes,
        TogglePaneZoom,
        /// 填名字、选目录，在里面新建 workspace；已经有这个目录的 workspace 时切过去（填了名字就改成它）。
        NewWorkspace,
        /// 关掉当前 workspace，里面的终端都随之结束。
        CloseWorkspace,
        RenameWorkspace,
        NextWorkspace,
        PreviousWorkspace,
        SelectLastWorkspace,
        ToggleSidebar,
        /// 显示或隐藏窗口底部的状态栏，记在配置的 `status-bar` 里。
        ToggleStatusBar,
        /// 显示或隐藏右侧的 Git 面板。
        ToggleGit,
        /// 显示或隐藏右侧的文件树。
        ToggleFiles,
        /// 打开或关掉标题栏上的项目命令菜单。
        ToggleTasks,
        /// 打开或关掉列出所有窗口里 agent 的浮层。
        GotoAgent,
        /// 跳到下一个要处理的 agent：先等回答的，再干完了没看的。
        NextAgent,
        /// 在窗口主区域打开手机端引导页：介绍、装 App、扫码配对。
        ShowMobile
    ]
);

/// 切换到第几个 workspace（从 0 数）。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct SelectWorkspace(pub usize);

/// 切换到第几个标签（从 0 数）。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct SelectTab(pub usize);

/// 焦点移到这个方向上相邻的分屏。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct FocusPane(pub Direction);

/// 把离当前分屏最近的分隔线往这个方向挪。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct ResizePane(pub Direction);

/// 透明标题栏的高度：终端内容从它下面开始，这一条用来拖动窗口，多个标签时也画在这里；
/// 显示侧栏时红绿灯落在侧栏顶上。
const TITLEBAR_HEIGHT: f32 = 36.;
/// 红绿灯按钮的直径。
const TRAFFIC_LIGHT_SIZE: f32 = 14.;
/// 红绿灯按钮的位置，竖直方向在标题栏里居中，和标签文字对齐；以及标题栏左侧给它们留出的宽度。
const TRAFFIC_LIGHTS_ORIGIN: (f32, f32) = (14., (TITLEBAR_HEIGHT - TRAFFIC_LIGHT_SIZE) / 2.);
const TRAFFIC_LIGHTS_WIDTH: f32 = 84.;
/// 标签上关闭按钮的边长。
const TAB_CLOSE_SIZE: f32 = 16.;
/// 标签栏最右边新建标签按钮的宽度。
const NEW_TAB_BUTTON_WIDTH: f32 = TITLEBAR_HEIGHT;
/// 标签最窄的宽度；标签多到挤不下时标签条改为横向滚动。
const TAB_MIN_WIDTH: f32 = 64.;
/// 标签最宽的宽度；标签少时不再拉宽，靠左排开，后面紧跟新建标签按钮。
const TAB_MAX_WIDTH: f32 = 200.;
/// 标题前面 agent 状态标记的宽度，固定下来标题才不会随转圈的字符左右跳。
const AGENT_MARK_WIDTH: f32 = 12.;
/// 分隔线两侧可以按住拖动的宽度。
const DIVIDER_GRAB_WIDTH: f32 = 6.;

/// 面板之间、标题下面和标签之间这些分隔线的颜色：前景色调淡，各处一样深。
fn divider_color(fg: gpui::Hsla) -> gpui::Hsla {
    fg.opacity(0.09)
}

/// 卡片样式下卡片的圆角，卡片之间、卡片到窗口边的间距，分屏顶上标题条的高度。
const CARD_RADIUS: f32 = 10.;
const CARD_GAP: f32 = 8.;
const PANE_HEADER_HEIGHT: f32 = 30.;
/// 卡片样式下标题栏里标签条的高度，标签是条里的胶囊，上下各留 2 点。
const TAB_TRACK_HEIGHT: f32 = 28.;

/// 窗口用卡片样式，见 `WindowStyle`。
fn cards(cx: &App) -> bool {
    cx.global::<AppConfig>().0.window_style == WindowStyle::Cards
}

/// 背景比前景亮，是浅色主题。
fn is_light(fg: Rgb, bg: Rgb) -> bool {
    let luma = |c: Rgb| 0.299 * f32::from(c.0) + 0.587 * f32::from(c.1) + 0.114 * f32::from(c.2);
    luma(bg) > luma(fg)
}

/// 卡片样式下窗口的底色，衬在卡片后面：浅色主题往前景色混一点，深色主题往黑色压一些，两种都比
/// 终端背景深一档。
fn frame_color(fg: Rgb, bg: Rgb) -> Rgb {
    if is_light(fg, bg) { bg.mix(fg, 0.06) } else { bg.mix(Rgb(0, 0, 0), 0.4) }
}

/// 卡片样式下标签条的底色，以及条里当前标签那颗胶囊的颜色。浅色主题里条介于外框
/// 和卡片之间、胶囊就是卡片的白；深色主题里卡片已经比外框亮，条用卡片的颜色，胶囊再亮一档。
fn tab_track_colors(fg: Rgb, bg: Rgb) -> (Rgb, Rgb) {
    if is_light(fg, bg) { (frame_color(fg, bg).mix(bg, 0.5), bg) } else { (bg, bg.mix(fg, 0.12)) }
}

/// 卡片的外观：终端背景色的圆角块，一圈淡淡的边和一点阴影，从外框上浮起来。
fn card(fg: Hsla, bg: Hsla) -> gpui::Div {
    div().rounded(px(CARD_RADIUS)).bg(bg).border_1().border_color(fg.opacity(0.08)).shadow(vec![BoxShadow {
        color: Hsla::black().opacity(0.06),
        offset: point(px(0.), px(1.)),
        blur_radius: px(3.),
        spread_radius: px(0.),
        inset: false,
    }])
}

/// 在标题栏这类能拖动窗口的地方按下鼠标：双击缩放窗口，否则开始拖动窗口。
fn drag_window(event: &MouseDownEvent, window: &mut Window, _: &mut App) {
    if event.click_count >= 2 {
        window.titlebar_double_click();
    } else {
        window.start_window_move();
    }
}

/// 正在用鼠标拖动的分隔线。
#[derive(Clone, Copy)]
enum Divider {
    /// 分屏之间的分隔线。
    Split(SplitId, Axis),
    /// 侧栏右边的分隔线，拖动改变侧栏宽度。
    Sidebar,
    /// 预览栏和右侧面板左边的分隔线，拖动改变它们的宽度。
    Preview,
    Panel,
    /// Git 面板底部图表上沿的分隔线，拖动改变图表的高度。
    GitGraph,
}

/// 侧栏里正在改名的 workspace，以及改名用的输入框。
struct Renaming {
    id: WorkspaceId,
    edit: inline_edit::InlineEdit,
}

/// 窗口的根视图。
pub struct WindowView {
    /// 至少有一个；最后一个关掉时窗口跟着关。
    workspaces: Vec<Workspace>,
    active: usize,
    /// 用户手动收起或展开过侧栏时是那个选择；没动过时多于一个 workspace 才显示。
    sidebar_shown: Option<bool>,
    /// 用户拖动过侧栏宽度时是那个宽度；没拖过时用默认宽度。
    sidebar_width: Option<f32>,
    /// 侧栏里 workspace 列表的滚动位置。
    sidebar_scroll: ScrollHandle,
    /// 右侧面板显示的是文件树还是 Git，收着时为空；拖动过宽度时是那个宽度，没拖过时用默认宽度。
    panel: Option<project::SidePanel>,
    /// 右侧面板上次显示的那一页，标题栏的开关按钮打开它。
    last_panel: project::SidePanel,
    panel_width: Option<f32>,
    /// Git 面板里改动的文件以树形式查看，否则是列表；整个窗口一个设置。
    git_tree: bool,
    /// Git 面板底部的图表收起来了；拖动过高度时是那个高度。整个窗口一个设置。
    git_graph_collapsed: bool,
    git_graph_height: Option<f32>,
    /// Git 面板的焦点：右键菜单的动作派发到这里；提交说明框在它里面。
    git_focus: FocusHandle,
    /// 开着的分支列表。
    branch_picker: Option<git_panel::BranchPicker>,
    /// 预览栏拖动过宽度时是那个宽度；预览栏在打开文件时出现，标签都关掉时收起，不存档。
    preview_width: Option<f32>,
    /// 预览栏的焦点：点了预览的文字后 cmd+c 复制选中的行。
    preview_focus: FocusHandle,
    /// 文件树里显示被 git 忽略的文件。
    show_ignored: bool,
    /// 文件树的焦点：点了文件树后方向键在里面移动选中的行。
    files_focus: FocusHandle,
    /// 文件树上面的搜索框和搜到的结果；整个窗口一个，换了 workspace 时按新的根目录重搜。
    file_search: files::FileSearch,
    /// 文件树或预览标签的右键菜单，文件树里正在新建或改名的输入框，以及剪切或复制下来等着粘贴的
    /// 文件。
    file_menu: Option<files::FileMenu>,
    file_edit: Option<files::FileEdit>,
    file_clipboard: Option<files::FileClipboard>,
    renaming: Option<Renaming>,
    /// 新建 workspace 的对话框。
    new_workspace: Option<new_workspace::NewWorkspaceDialog>,
    /// 添加项目命令的对话框。
    add_task: Option<tasks::AddTaskDialog>,
    /// 开着的手机端引导页，盖住标签和分屏。
    mobile: Option<mobile::MobilePage>,
    /// 当前 workspace 里没有标签时窗口的焦点，快捷键（新开标签等）照常派发得到。
    empty_focus: FocusHandle,
    /// 开着的 agent 列表。
    agent_picker: Option<agent_picker::AgentPicker>,
    /// 开着的排列分屏浮层。
    arrange_picker: Option<arrange::ArrangePicker>,
    /// 底部状态栏上开着的浮层。
    status_popover: Option<status_bar::StatusPopover>,
    /// 显示着驱动标记时，到最早的那个该消失的时候（`now_ms` 的毫秒数）重画的计时器，见
    /// `schedule_driver_redraw`。
    driver_redraw: Option<(u64, Task<()>)>,
    /// workspace、标签和分屏节点的标识都从这里取。
    next_id: u64,
    layout: Rc<RefCell<PaneLayout>>,
    dragging_divider: Option<Divider>,
    /// 窗口的位置和大小，以及所在屏幕的 UUID，存布局用：窗口关掉以后就问不到了。
    bounds: WindowBounds,
    display: Option<String>,
    /// 刚开出来的终端，开出来的时刻和起始目录：shell 启动时加载插件会临时切到别的目录，
    /// 这段时间里读到的目录不可信，存档时记起始目录。
    spawned: HashMap<EntityId, (Instant, Option<PathBuf>)>,
    /// 所有 workspace 都关掉了，窗口因此关闭；这样的窗口不留存档。
    emptied: bool,
    _bounds_watch: Subscription,
    /// 窗口切到前台时重读右侧面板的内容。
    _activation_watch: Subscription,
    /// 配置重载后重画，文件树的字号等跟着变。
    _config_watch: Subscription,
    /// 右侧面板显示时定时看终端换没换目录，监听不了目录时定时重读。
    _project_poll: Task<()>,
    /// 监听右侧面板在看的目录，事件发到 `project_events`，由 `_project_events` 收。
    project_watch: Option<project::ProjectWatch>,
    project_events: futures::channel::mpsc::UnboundedSender<Vec<PathBuf>>,
    _project_events: Task<()>,
}

impl WindowView {
    fn empty(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let bounds_watch = cx.observe_window_bounds(window, |this, window, cx| {
            this.bounds = window.window_bounds();
            this.display = Self::display_uuid(window, cx);
            this.save(cx);
        });
        let activation_watch = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.refresh_project(cx);
                this.refresh_preview_if_changed(cx);
                // 切回这个窗口就算看到了当前分屏。
                this.mark_seen(window, cx);
            }
        });
        let project_poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(project::POLL_INTERVAL).await;
                let alive = this.update_in(cx, |this, window, cx| {
                    if window.is_window_active() {
                        this.poll_project(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let (project_events, mut events) = futures::channel::mpsc::unbounded::<Vec<PathBuf>>();
        let project_events_task = cx.spawn_in(window, async move |this, cx| {
            while let Some(mut paths) = events.next().await {
                cx.background_executor().timer(project::WATCH_DEBOUNCE).await;
                while let Ok(more) = events.try_recv() {
                    paths.extend(more);
                }
                let alive = this.update_in(cx, |this, window, cx| {
                    this.project_changed(paths, window.is_window_active(), cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let config_watch = cx.observe_global::<AppConfig>(|_, cx| cx.notify());
        persist::track(cx);
        // 窗口关掉后它的分屏都没了，发过的通知点了也跳不过去，一并收回。
        cx.on_release(|view, cx| {
            let panes =
                view.workspaces.iter().flat_map(|workspace| &workspace.tabs).flat_map(|tab| tab.panes.keys().copied());
            agents::dismiss_alerts(panes.collect::<Vec<_>>(), cx);
        })
        .detach();
        Self {
            workspaces: Vec::new(),
            active: 0,
            sidebar_shown: None,
            sidebar_width: None,
            sidebar_scroll: ScrollHandle::new(),
            panel: None,
            last_panel: project::SidePanel::Files,
            panel_width: None,
            git_tree: false,
            git_graph_collapsed: false,
            git_graph_height: None,
            git_focus: cx.focus_handle(),
            branch_picker: None,
            preview_width: None,
            preview_focus: cx.focus_handle(),
            show_ignored: false,
            files_focus: cx.focus_handle(),
            file_search: files::FileSearch::new(window, cx),
            file_menu: None,
            file_edit: None,
            file_clipboard: None,
            renaming: None,
            new_workspace: None,
            add_task: None,
            mobile: None,
            empty_focus: cx.focus_handle(),
            agent_picker: None,
            arrange_picker: None,
            status_popover: None,
            driver_redraw: None,
            next_id: 0,
            layout: Rc::default(),
            dragging_divider: None,
            bounds: window.window_bounds(),
            display: Self::display_uuid(window, cx),
            spawned: HashMap::new(),
            emptied: false,
            _bounds_watch: bounds_watch,
            _activation_watch: activation_watch,
            _config_watch: config_watch,
            _project_poll: project_poll,
            project_watch: None,
            project_events,
            _project_events: project_events_task,
        }
    }

    fn display_uuid(window: &Window, cx: &App) -> Option<String> {
        Some(window.display(cx)?.uuid().ok()?.to_string())
    }

    /// 新窗口：一个 workspace，里面一个终端。`shell` 是启动时在家目录提前拉起的 shell，
    /// 没有时现启动一个。
    pub fn new(shell: Option<Prespawned>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(window, cx);
        this.push_default_workspace(shell, window, cx);
        this
    }

    /// 按存档恢复的窗口；一个 workspace 都没恢复出来时和新窗口一样。
    pub fn restore(saved: SavedWindow, shell: Option<Prespawned>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(window, cx);
        let shell = this.restore_workspaces(saved, shell, window, cx);
        if this.workspaces.is_empty() {
            this.push_default_workspace(shell, window, cx);
        }
        this
    }

    /// 在家目录开一个终端，放进新的 workspace。
    fn push_default_workspace(&mut self, shell: Option<Prespawned>, window: &mut Window, cx: &mut Context<Self>) {
        let view = match shell {
            Some(shell) => TerminalView::adopt(shell, window, cx),
            None => TerminalView::spawn(None, window, cx),
        }
        .unwrap_or_else(|err| panic!("failed to start terminal session: {err:#}"));
        self.record_spawn(&view, None);
        let dir = view.read(cx).cwd().or_else(home_dir).unwrap_or_else(|| PathBuf::from("/"));
        self.insert_workspace(self.workspaces.len(), dir, None, view, window, cx);
    }
}

impl WindowView {
    /// 窗口跟着走的前景色和背景色：有焦点的终端当前的配色，没有终端时按配置。
    fn colors(&self, cx: &mut App) -> (Rgb, Rgb) {
        match self.focused_view() {
            Some(view) => view.update(cx, |view, _| view.colors()),
            None => {
                let config = &cx.global::<AppConfig>().0;
                (config.foreground, config.background)
            }
        }
    }

    /// Git 面板和预览栏里代码用的字体，跟着终端；没有终端时取配置里的第一个。
    fn font_family(&self, cx: &App) -> SharedString {
        match self.focused_view() {
            Some(view) => view.read(cx).font_family(),
            None => cx.global::<AppConfig>().0.font_family.first().cloned().unwrap_or_default().into(),
        }
    }
}

impl Focusable for WindowView {
    /// 开着手机端引导页时是它；否则是有焦点的终端，当前 workspace 里没有终端时是窗口自己的。
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if let Some(page) = &self.mobile {
            return page.focus.clone();
        }
        match self.focused_view() {
            Some(view) => view.focus_handle(cx),
            None => self.empty_focus.clone(),
        }
    }
}

impl Render for WindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focus_menu(window, cx);
        let (fg, bg) = self.colors(cx);
        let (base, body) = if self.mobile.is_some() {
            (if cards(cx) { frame_color(fg, bg) } else { bg }, self.render_mobile_body(fg, bg, window, cx))
        } else if cards(cx) {
            (frame_color(fg, bg), self.render_cards_body(fg, bg, window, cx))
        } else {
            (bg, self.render_classic_body(fg, bg, window, cx))
        };
        let drag = self.dragging_divider.map(|divider| self.render_divider_drag(divider, cx));
        let file_menu = self.render_file_menu(fg, bg, window, cx);
        let agent_picker = self.render_agent_picker(fg, bg, window, cx);
        let arrange_picker = self.render_arrange_picker(fg, bg, cx);
        let branch_picker = self.render_branch_picker(fg, bg, cx);
        let new_workspace = self.render_new_workspace(fg, bg, cx);
        let add_task = self.render_add_task(fg, bg, cx);
        let status_bar = status_bar::shown(cx).then(|| self.render_status_bar(fg, bg, cx));
        div()
            .id("window")
            .key_context("Window")
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::revoke_device))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::select_tab))
            .on_action(cx.listener(Self::select_last_tab))
            .on_action(cx.listener(Self::new_split_right))
            .on_action(cx.listener(Self::new_split_down))
            .on_action(cx.listener(Self::close_pane))
            .on_action(cx.listener(Self::focus_next_pane))
            .on_action(cx.listener(Self::focus_previous_pane))
            .on_action(cx.listener(Self::focus_pane))
            .on_action(cx.listener(Self::resize_pane))
            .on_action(cx.listener(Self::equalize_panes))
            .on_action(cx.listener(Self::toggle_pane_zoom))
            .on_action(cx.listener(Self::new_workspace))
            .on_action(cx.listener(Self::close_workspace))
            .on_action(cx.listener(Self::rename_workspace))
            .on_action(cx.listener(Self::next_workspace))
            .on_action(cx.listener(Self::previous_workspace))
            .on_action(cx.listener(Self::select_workspace))
            .on_action(cx.listener(Self::select_last_workspace))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_git))
            .on_action(cx.listener(Self::toggle_status_item))
            .on_action(cx.listener(Self::run_task))
            .on_action(cx.listener(Self::add_task))
            .on_action(cx.listener(Self::toggle_tasks))
            .on_action(cx.listener(Self::toggle_task_group))
            .on_action(cx.listener(Self::toggle_status_bar))
            // 侧栏按着切换 workspace 的修饰键或 ⌘ 时才显示快捷键提示，按下、松开都要重画。挂在根上：修饰键的事件
            // 只沿焦点所在的路径传，侧栏不在这条路上。
            .on_modifiers_changed(cx.listener(|_, _, _, cx| cx.notify()))
            .on_action(cx.listener(Self::toggle_files))
            .on_action(cx.listener(Self::goto_agent))
            .on_action(cx.listener(Self::next_agent))
            .on_action(cx.listener(Self::show_mobile))
            .on_action(cx.listener(Self::arrange_panes))
            .map(|window| Self::bind_git_actions(window, cx))
            // 终端和新建对话框没接住的拖放落到这里：侧栏收着时拖到标题栏、空 workspace 上也能开。
            .on_drop(cx.listener(Self::open_dropped_dirs))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(base))
            .child(div().relative().flex_1().min_h_0().flex().children(body))
            .children(status_bar)
            .children(drag)
            .children(file_menu)
            .children(agent_picker)
            .children(arrange_picker)
            .children(branch_picker)
            .children(new_workspace)
            .children(add_task)
    }
}

impl WindowView {
    /// 从访达拖来的文件夹各开一个 workspace，已经开着的就切过去；文件不算。
    fn open_dropped_dirs(&mut self, dropped: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        for dir in dropped.paths().iter().filter(|path| path.is_dir()) {
            self.open_workspace(dir.clone(), None, window, cx);
        }
    }

    /// 经典样式：终端铺满窗口，侧栏、标题栏和右侧面板之间一条细线；只有一个标签时标题栏只写标题。
    fn render_classic_body(
        &mut self,
        fg: Rgb,
        bg: Rgb,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let fullscreen = window.is_fullscreen();
        let tab_count = self.workspace().tabs.len();
        let show_tabs = tab_count > 1;
        let panes = self.render_panes(fg, bg, window, cx);
        let sidebar = self.sidebar_visible().then(|| self.render_sidebar(fg, bg, window, cx));
        // 标题栏透明后内容铺到红绿灯下面，顶部这条要能拖动窗口、双击缩放。红绿灯和侧栏开关
        // 落在侧栏上或者全屏时没有红绿灯，标题栏不用让位；全屏又只有一个标签时不留这一条。
        let left_inset = if fullscreen || sidebar.is_some() { 0. } else { sidebar::SIDEBAR_TOGGLE_INSET };
        let sidebar_toggle = (!fullscreen).then(|| self.render_sidebar_toggle(fg, bg, cx));
        let sidebar_width = if sidebar.is_some() { self.sidebar_width() } else { 0. };
        let sidebar_handle = sidebar.is_some().then(|| self.render_sidebar_handle(cx));
        let widths = self.right_panel_widths(f32::from(window.viewport_size().width));
        let font = self.font_family(cx);
        let preview_shown = self.preview_shown();
        let preview = self.render_preview_panel(widths.preview, self.panel.is_none(), fg, bg, font, cx);
        let panel = self.render_side_panel(widths.panel, fg, bg, window, cx);
        let right_handles = [
            preview_shown.then(|| self.render_right_handle(Divider::Preview, widths.preview + widths.panel, cx)),
            self.panel.is_some().then(|| self.render_right_handle(Divider::Panel, widths.panel, cx)),
        ];
        let titlebar_shown = !fullscreen || show_tabs;
        // 右侧面板的开关按钮：右侧都收着时落在标题栏右端，标题栏给它让位；打开着时落在
        // 面板顶上。全屏又只有一个标签、右侧也都收着时没有地方放，不画。
        let right_inset = if titlebar_shown && !self.project_visible() { project::PANEL_TOGGLES_INSET } else { 0. };
        let panel_toggles = (titlebar_shown || self.project_visible()).then(|| {
            self.render_panel_toggles(fg, bg, window, cx)
                .absolute()
                .top(px((TITLEBAR_HEIGHT - project::TOGGLE_HEIGHT) / 2.))
                .right(px(project::TOGGLE_MARGIN))
        });
        let tabs: Vec<_> = if show_tabs {
            // 标签平分标题栏除去两头的宽度，限制在 `TAB_MIN_WIDTH` 到 `TAB_MAX_WIDTH` 之间，
            // 挤不下就让标签条滚动；拖动时的预览也照这个宽度画。
            let tab_width = ((window.viewport_size().width
                - px(sidebar_width + left_inset + NEW_TAB_BUTTON_WIDTH + right_inset + widths.total()))
                / tab_count as f32)
                .clamp(px(TAB_MIN_WIDTH), px(TAB_MAX_WIDTH));
            // 标签条只占标签本身的宽度，新建标签按钮紧跟在后面，剩下的空白留给拖动窗口。
            let strip = div()
                .id("tabs")
                .flex_initial()
                .min_w_0()
                .h_full()
                .flex()
                .overflow_x_scroll()
                .track_scroll(&self.workspace().tab_scroll)
                .children(
                    (0..tab_count).map(|ix| self.render_tab(ix, tab_width, false, fg, bg, cx)).collect::<Vec<_>>(),
                );
            let inset = div()
                .id("panel-toggles-inset")
                .flex_none()
                .w(px(right_inset))
                .h_full()
                .when(right_inset > 0., |inset| inset.border_l_1().border_color(divider_color(hsla(fg))));
            let spacer = div().id("tabs-spacer").flex_1().h_full();
            vec![strip, self.render_new_tab_button(fg, bg, cx), spacer, inset]
        } else {
            // 只有一个标签时标题居中画在侧栏和右侧面板之间；两头让出的宽度取大的那个，
            // 没有侧栏和右侧面板时标题在整个窗口里居中。没有标签时标题是 workspace 的名字。
            let title = match self.focused_view() {
                Some(view) => SharedString::from(view.read(cx).title().to_owned()),
                None => self.workspace().name.clone(),
            };
            let mark = self.tab().and_then(|tab| tab.mark(cx));
            let fg = hsla(fg);
            let inset = left_inset.max(right_inset);
            vec![
                div()
                    .id("window-title")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pl(px(inset - left_inset))
                    .pr(px(inset))
                    .flex()
                    .text_color(fg.opacity(0.55))
                    .child(titled(title, mark, "window-agent", fg).flex_1()),
            ]
        };
        let titlebar = titlebar_shown.then(|| {
            div()
                .id("titlebar")
                .h(px(TITLEBAR_HEIGHT))
                .flex_none()
                .flex()
                .text_size(px(12.))
                .on_mouse_down(MouseButton::Left, drag_window)
                // 给红绿灯和侧栏开关让出的位置；后面跟着标签时右边画一条分隔线，和新建标签按钮
                // 左边那条对称。
                .child(div().flex_none().w(px(left_inset)).h_full().when(show_tabs && left_inset > 0., |inset| {
                    inset.border_r_1().border_color(divider_color(hsla(fg)))
                }))
                .children(tabs)
        });
        let main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .children(titlebar)
            .child(div().relative().flex_1().min_h_0().child(panes));
        let mut body: Vec<AnyElement> = Vec::new();
        body.extend(sidebar.map(IntoElement::into_any_element));
        body.push(main.into_any_element());
        body.extend(preview.map(IntoElement::into_any_element));
        body.extend(panel.map(IntoElement::into_any_element));
        body.extend(sidebar_handle.map(IntoElement::into_any_element));
        body.extend(right_handles.into_iter().flatten().map(IntoElement::into_any_element));
        body.extend(sidebar_toggle.map(IntoElement::into_any_element));
        body.extend(panel_toggles.map(IntoElement::into_any_element));
        body
    }

    /// 卡片样式：窗口底色是比终端深一档的外框，分屏和右侧面板各是一张圆角卡片，卡片之间和卡片
    /// 到窗口边留出 `CARD_GAP`。标题栏横跨侧栏以外的整个宽度：左边是这台机器，中间是胶囊样式的
    /// 标签（只有一个标签时也画），右边是新建标签和右侧面板的开关。
    fn render_cards_body(&mut self, fg: Rgb, bg: Rgb, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let frame = frame_color(fg, bg);
        let fullscreen = window.is_fullscreen();
        let viewport = f32::from(window.viewport_size().width);
        let tab_count = self.workspace().tabs.len();
        let panes = self.render_panes(fg, bg, window, cx);
        let sidebar = self.sidebar_visible().then(|| self.render_sidebar(fg, bg, window, cx));
        let left_inset = if fullscreen || sidebar.is_some() { CARD_GAP } else { sidebar::SIDEBAR_TOGGLE_INSET };
        let sidebar_toggle = (!fullscreen).then(|| self.render_sidebar_toggle(fg, frame, cx));
        let sidebar_width = if sidebar.is_some() { self.sidebar_width() } else { 0. };
        let sidebar_handle = sidebar.is_some().then(|| self.render_sidebar_handle(cx));
        let widths = self.right_panel_widths(viewport);
        let font = self.font_family(cx);
        let preview = self.render_preview_panel(widths.preview, false, fg, bg, font, cx);
        let panel = self.render_side_panel(widths.panel, fg, bg, window, cx);
        let right_handles =
            [self.preview_shown().then_some(Divider::Preview), self.panel.is_some().then_some(Divider::Panel)]
                .into_iter()
                .flatten()
                .map(|divider| {
                    let right = self.right_divider_offset(divider, widths, true);
                    self.render_right_handle(divider, right, cx)
                })
                .collect::<Vec<_>>();
        let machine = self.render_machine(fg, sidebar_width + left_inset, cx);
        // 标签平分标签条，最窄 `TAB_MIN_WIDTH`，挤不下就让标签条滚动。这里估一个宽度，决定标签
        // 要不要收成紧凑的样子，拖动时的预览也照它画。
        let fixed = sidebar_width
            + left_inset
            + machine.as_ref().map_or(0., |_| machine::MACHINE_MAX_WIDTH + 12.)
            + NEW_TAB_BUTTON_WIDTH
            + project::PANEL_TOGGLES_INSET
            + widths.total();
        let tab_width = px(((viewport - fixed) / tab_count.max(1) as f32).max(TAB_MIN_WIDTH));
        let track_bg = hsla(tab_track_colors(fg, bg).0);
        let strip = div()
            .id("tabs")
            .flex_1()
            .min_w_0()
            .h(px(TAB_TRACK_HEIGHT))
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(TAB_TRACK_HEIGHT / 2. - 4.))
            .bg(track_bg)
            .flex()
            .items_center()
            .overflow_x_scroll()
            .track_scroll(&self.workspace().tab_scroll)
            .children((0..tab_count).map(|ix| self.render_tab(ix, tab_width, true, fg, bg, cx)).collect::<Vec<_>>());
        let titlebar = div()
            .id("titlebar")
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .pr(px(CARD_GAP))
            .text_size(px(12.))
            .on_mouse_down(MouseButton::Left, drag_window)
            .child(div().flex_none().w(px(left_inset)))
            .children(machine.map(|machine| machine.mr(px(12.))))
            .child(strip)
            .child(self.render_new_tab_button_card(fg, frame, cx))
            .child(self.render_panel_toggles(fg, frame, window, cx));
        let content = div()
            .flex_1()
            .min_h_0()
            .flex()
            .gap(px(CARD_GAP))
            .pl(px(CARD_GAP))
            .pr(px(CARD_GAP))
            .pb(px(CARD_GAP))
            .child(div().relative().flex_1().min_w_0().h_full().child(panes))
            .children(preview)
            .children(panel);
        let main = div().flex_1().min_w_0().h_full().flex().flex_col().child(titlebar).child(content);
        let mut body: Vec<AnyElement> = Vec::new();
        body.extend(sidebar.map(IntoElement::into_any_element));
        body.push(main.into_any_element());
        body.extend(sidebar_handle.map(IntoElement::into_any_element));
        body.extend(right_handles.into_iter().map(IntoElement::into_any_element));
        body.extend(sidebar_toggle.map(IntoElement::into_any_element));
        body
    }
}
