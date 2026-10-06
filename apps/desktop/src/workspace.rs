//! 一个窗口：左侧列出 workspace 的侧栏，顶部的标签栏，标签里的分屏，以及右侧可以打开的
//! 预览栏、Git 面板和文件树。workspace 对应一个项目目录，各有一组标签；窗口的布局随改随存，下次启动时
//! 恢复。
//!
//! 这里是窗口的根视图 `WindowView`、窗口绑定的动作，以及把各部分拼起来的渲染。其余按职责分在
//! 子模块里：workspace、标签和分屏的数据与增删切换（`model`）、动作的处理（`actions`）、
//! agent 的状态标记、提醒和跳转（`agents`）、列出所有 agent 的浮层（`agent_picker`）、
//! 标签里的分屏（`panes`）、标题栏和标签（`titlebar`）、侧栏（`sidebar`）、右侧的预览栏、
//! Git 面板和文件树（`project`、`preview`、`git_panel`、`files`），侧栏和文件树共用的就地输入框
//! （`inline_edit`），存档（`persistence`），侧栏里没在窗口里显示的后台会话（`background`），
//! 退出和关窗口时会话怎么办（`quit`），以及别的进程经宿主请 app 开终端、切到某个终端、问各个
//! 终端摆在哪（`remote`、`layout_report`），以及一次在当前分屏旁开几个分屏（`arrange`）。

mod actions;
mod agent_picker;
mod agents;
mod arrange;
mod background;
mod files;
mod git_panel;
mod inline_edit;
mod layout_report;
mod model;
mod panes;
mod persistence;
mod preview;
mod project;
mod quit;
mod remote;
mod sidebar;
mod titlebar;

use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc, time::Instant};

use futures::StreamExt as _;
use gpui::{
    Action, App, Context, EntityId, FocusHandle, Focusable, MouseButton, MouseDownEvent, Render, ScrollHandle,
    SharedString, Subscription, Task, Window, WindowBounds, actions, div, prelude::*, px,
};
use runode_shared_types::pane::{Axis, Direction, SplitId};

pub(crate) use agents::reveal_notified;
pub use arrange::ArrangePanes;
pub use background::watch as watch_background;
pub use files::{
    CollapseSelectedFile, CopyPath, CopyRelativePath, DeleteFile, ExpandSelectedFile, FocusTerminal, OpenSelectedFile,
    RenameFile, RevealInFinder, SelectFirstFile, SelectLastFile, SelectNextFile, SelectPreviousFile,
};
pub use persistence::{install, saved_window_options};
pub use quit::{close_all_windows, close_window, end_sessions_in_menu, quit, quit_and_end_sessions, should_close};
pub use remote::serve_requests;
pub use titlebar::titlebar_options;

use crate::{
    config::AppConfig,
    persist::SavedWindow,
    prespawn::Prespawned,
    terminal_view::{TerminalView, hsla},
};
use model::{PaneLayout, Workspace, WorkspaceId, home_dir};
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
        /// 选一个目录，在里面新建 workspace；已经有这个目录的 workspace 时切过去。
        NewWorkspace,
        /// 关掉当前 workspace，里面的终端都随之结束。
        CloseWorkspace,
        RenameWorkspace,
        NextWorkspace,
        PreviousWorkspace,
        SelectLastWorkspace,
        ToggleSidebar,
        /// 显示或隐藏右侧的 Git 面板。
        ToggleGit,
        /// 显示或隐藏右侧的文件树。
        ToggleFiles,
        /// 打开或关掉列出所有窗口里 agent 的浮层。
        GotoAgent,
        /// 跳到下一个要处理的 agent：先等回答的，再干完了没看的。
        NextAgent
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
    /// 预览栏、Git 面板和文件树左边的分隔线，拖动改变它们的宽度。
    Preview,
    Git,
    Files,
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
    /// 右侧的文件树是否显示；拖动过宽度时是那个宽度，没拖过时用默认宽度。
    files_shown: bool,
    files_width: Option<f32>,
    /// Git 面板是否显示，拖动过宽度时是那个宽度。
    git_shown: bool,
    git_width: Option<f32>,
    /// Git 面板里改动的文件以树形式查看，否则是列表；整个窗口一个设置。
    git_tree: bool,
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
    /// 文件树或预览标签的右键菜单，文件树里正在新建或改名的输入框，以及剪切或复制下来等着粘贴的
    /// 文件。
    file_menu: Option<files::FileMenu>,
    file_edit: Option<files::FileEdit>,
    file_clipboard: Option<files::FileClipboard>,
    renaming: Option<Renaming>,
    /// 开着的 agent 列表。
    agent_picker: Option<agent_picker::AgentPicker>,
    /// 开着的排列分屏浮层。
    arrange_picker: Option<arrange::ArrangePicker>,
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
        persistence::track(cx);
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
            files_shown: false,
            files_width: None,
            git_shown: false,
            git_width: None,
            git_tree: false,
            git_focus: cx.focus_handle(),
            branch_picker: None,
            preview_width: None,
            preview_focus: cx.focus_handle(),
            show_ignored: false,
            files_focus: cx.focus_handle(),
            file_menu: None,
            file_edit: None,
            file_clipboard: None,
            renaming: None,
            agent_picker: None,
            arrange_picker: None,
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

    /// 按存档恢复的窗口；一个终端都没恢复出来时和新窗口一样。
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
        self.insert_workspace(self.workspaces.len(), dir, view, window, cx);
    }
}

impl Focusable for WindowView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.tab().focused_view().focus_handle(cx)
    }
}

impl Render for WindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.tab().focused_view().clone();
        let (fg, bg) = view.update(cx, |view, _| view.colors());
        let fullscreen = window.is_fullscreen();
        let tab_count = self.workspace().tabs.len();
        let show_tabs = tab_count > 1;
        let panes = self.render_panes(fg, bg, window, cx);
        let drag = self.dragging_divider.map(|divider| self.render_divider_drag(divider, cx));
        let sidebar = self.sidebar_visible().then(|| self.render_sidebar(fg, bg, fullscreen, cx));
        // 标题栏透明后内容铺到红绿灯下面，顶部这条要能拖动窗口、双击缩放。红绿灯和侧栏开关
        // 落在侧栏上或者全屏时没有红绿灯，标题栏不用让位；全屏又只有一个标签时不留这一条。
        let left_inset = if fullscreen || sidebar.is_some() { 0. } else { sidebar::SIDEBAR_TOGGLE_INSET };
        let sidebar_toggle = (!fullscreen).then(|| self.render_sidebar_toggle(fg, bg, cx));
        let sidebar_width = if sidebar.is_some() { self.sidebar_width() } else { 0. };
        let sidebar_handle = sidebar.is_some().then(|| self.render_sidebar_handle(cx));
        let widths = self.right_panel_widths(f32::from(window.viewport_size().width));
        let font = view.read(cx).font_family();
        let preview_shown = self.preview_shown();
        let preview = self.render_preview_panel(widths.preview, !self.files_shown && !self.git_shown, fg, bg, font, cx);
        let git = self.git_shown.then(|| self.render_git_panel(widths.git, !self.files_shown, fg, bg, window, cx));
        let files = self.files_shown.then(|| self.render_files_panel(widths.files, fg, bg, cx));
        let file_menu = self.render_file_menu(fg, bg, cx);
        let agent_picker = self.render_agent_picker(fg, bg, window, cx);
        let arrange_picker = self.render_arrange_picker(fg, bg, cx);
        let branch_picker = self.render_branch_picker(fg, bg, cx);
        let right_handles = [
            preview_shown
                .then(|| self.render_right_handle(Divider::Preview, widths.preview + widths.git + widths.files, cx)),
            self.git_shown.then(|| self.render_right_handle(Divider::Git, widths.git + widths.files, cx)),
            self.files_shown.then(|| self.render_right_handle(Divider::Files, widths.files, cx)),
        ];
        let titlebar_shown = !fullscreen || show_tabs;
        // 右侧面板的开关按钮：面板都收着时落在标题栏右端，标题栏给它们让位；打开着时落在
        // 面板顶上。全屏又只有一个标签、面板也都收着时没有地方放，不画。
        let right_inset = if titlebar_shown && !self.project_visible() { project::PANEL_TOGGLES_INSET } else { 0. };
        let panel_toggles = (titlebar_shown || self.project_visible()).then(|| self.render_panel_toggles(fg, bg, cx));
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
                .children((0..tab_count).map(|ix| self.render_tab(ix, tab_width, fg, bg, cx)).collect::<Vec<_>>());
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
            // 没有侧栏和右侧面板时标题在整个窗口里居中。
            let view = view.read(cx);
            let title = SharedString::from(view.title().to_owned());
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
                    .child(titled(title, self.tab().mark(cx), "window-agent", fg).flex_1()),
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
        div()
            .id("window")
            .key_context("Window")
            .on_action(cx.listener(Self::new_tab))
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
            .on_action(cx.listener(Self::toggle_files))
            .on_action(cx.listener(Self::goto_agent))
            .on_action(cx.listener(Self::next_agent))
            .on_action(cx.listener(Self::arrange_panes))
            .map(|window| Self::bind_git_actions(window, cx))
            .relative()
            .size_full()
            .flex()
            .bg(hsla(bg))
            .children(sidebar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .children(titlebar)
                    .child(div().relative().flex_1().min_h_0().child(panes)),
            )
            .children(preview)
            .children(git)
            .children(files)
            .children(sidebar_handle)
            .children(right_handles.into_iter().flatten())
            .children(sidebar_toggle)
            .children(panel_toggles)
            .children(drag)
            .children(file_menu)
            .children(agent_picker)
            .children(arrange_picker)
            .children(branch_picker)
    }
}
