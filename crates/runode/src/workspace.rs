//! 一个窗口：左侧列出 workspace 的侧栏，顶部的标签栏，以及标签里的分屏。workspace 对应
//! 一个项目目录，各有一组标签；窗口的布局随改随存，下次启动时恢复。

mod persistence;
mod sidebar;

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::Instant,
};

use gpui::{
    Action, Animation, AnimationExt, AnyElement, App, Bounds, BoxShadow, Context, CursorStyle, Div,
    ElementId, Entity, EntityId, FocusHandle, Focusable, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, PathPromptOptions, Pixels, Render, ScrollHandle, SharedString, Stateful,
    StyleRefinement, Subscription, TitlebarOptions, Window, WindowBounds, actions, canvas, div, point,
    prelude::*, px, relative,
};

pub use persistence::{install, saved_window_options};

use crate::{
    agent::{Agent, AgentState},
    pane::{self, Axis, Direction, Node, SplitId},
    persist::{self, SavedWindow},
    prespawn::Prespawned,
    search_bar::SearchField,
    session::Rgb,
    terminal_view::{DEFAULT_TITLE, TerminalEvent, TerminalView, hsla},
};

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
        ToggleSidebar
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
const TITLEBAR_HEIGHT: f32 = 28.;
/// 红绿灯按钮的直径。
const TRAFFIC_LIGHT_SIZE: f32 = 14.;
/// 红绿灯按钮的位置，竖直方向在标题栏里居中，和标签文字对齐；以及标题栏左侧给它们留出的宽度。
const TRAFFIC_LIGHTS_ORIGIN: (f32, f32) = (12., (TITLEBAR_HEIGHT - TRAFFIC_LIGHT_SIZE) / 2.);
const TRAFFIC_LIGHTS_WIDTH: f32 = 78.;
/// 标签上关闭按钮的边长。
const TAB_CLOSE_SIZE: f32 = 16.;
/// 标签右侧槽位的宽度，放快捷键提示或响铃标记；和关闭按钮一边一个，标题才在标签里居中。
const TAB_SIDE_SLOT: f32 = 20.;
/// 标签栏最右边新建标签按钮的宽度。
const NEW_TAB_BUTTON_WIDTH: f32 = 28.;
/// 标签最窄的宽度；标签多到挤不下时标签条改为横向滚动。
const TAB_MIN_WIDTH: f32 = 64.;
/// 比这窄的标签只留标题：快捷键提示收起，关闭按钮悬停时浮在标题左边。
const TAB_COMPACT_WIDTH: f32 = 120.;
/// 标题前面 agent 状态标记的宽度，固定下来标题才不会随转圈的字符左右跳。
const AGENT_MARK_WIDTH: f32 = 12.;
/// 键盘调整分屏大小时每次挪动的像素。
const RESIZE_STEP: f32 = 10.;
/// 分隔线两侧可以按住拖动的宽度。
const DIVIDER_GRAB_WIDTH: f32 = 6.;
/// 没有焦点的分屏蒙上一层背景色，这是蒙层的不透明度。
const UNFOCUSED_DIM: f32 = 0.3;

/// 窗口的标题栏设置：透明，终端背景一直铺到窗口顶部，标签栏画在红绿灯或侧栏右边。
pub fn titlebar_options() -> TitlebarOptions {
    TitlebarOptions {
        title: Some(DEFAULT_TITLE.into()),
        appears_transparent: true,
        traffic_light_position: Some(point(
            px(TRAFFIC_LIGHTS_ORIGIN.0),
            px(TRAFFIC_LIGHTS_ORIGIN.1),
        )),
    }
}

/// 标签的标识，标签挪动位置后不变。
type TabId = u64;

struct Tab {
    id: TabId,
    /// 分屏布局；只有一个终端时是单个叶子。
    root: Node<EntityId>,
    /// 这个标签里的终端，以及对它们事件的订阅。
    panes: HashMap<EntityId, (Entity<TerminalView>, Subscription)>,
    /// 当前（切走之前最后）获得焦点的终端。
    focused: EntityId,
    /// 当前终端放大占满整个标签，其他分屏暂时不画。
    zoomed: bool,
    /// 不在前台时响过铃，或者里面的 agent 停了下来，切过去后清掉。
    bell: bool,
}

impl Tab {
    fn focused_view(&self) -> &Entity<TerminalView> {
        &self.panes[&self.focused].0
    }
}

/// 上一帧各个终端和分屏节点在窗口里的位置，按方向切换和拖动分隔线时用。
#[derive(Default)]
struct PaneLayout {
    panes: HashMap<EntityId, Bounds<Pixels>>,
    splits: HashMap<SplitId, Bounds<Pixels>>,
}

/// 拖动中的标签：拖动时跟着鼠标画出来，放到另一个标签上时按 `id` 挪位置。
#[derive(Clone)]
struct DraggedTab {
    id: TabId,
    /// 开始拖动时所在的位置，用来决定落点提示画在目标标签的哪一边。
    ix: usize,
    title: SharedString,
    width: Pixels,
    fg: Hsla,
    bg: Hsla,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(self.width)
            .h(px(TITLEBAR_HEIGHT))
            .px(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .bg(self.bg)
            .border_1()
            .border_color(self.fg.opacity(0.15))
            .shadow(vec![BoxShadow {
                color: Hsla::black().opacity(0.3),
                offset: point(px(0.), px(2.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
                inset: false,
            }])
            .text_size(px(12.))
            .text_color(self.fg)
            .child(div().min_w_0().truncate().child(self.title.clone()))
    }
}

/// 居中的标题，前台是 agent 时前面加上它的状态标记。
fn titled(title: SharedString, agent: Option<Agent>, id: impl Into<ElementId>, fg: Hsla) -> Div {
    div()
        .min_w_0()
        .flex()
        .justify_center()
        .items_center()
        .gap(px(5.))
        .children(agent.map(|agent| agent_mark(agent, id, fg)))
        .child(div().min_w_0().truncate().child(title))
}

/// agent 的状态标记：工作中播放该 agent 自己的工作动画，空闲时是一个空心圆点。
fn agent_mark(agent: Agent, id: impl Into<ElementId>, fg: Hsla) -> AnyElement {
    let slot = div()
        .flex_none()
        .w(px(AGENT_MARK_WIDTH))
        .flex()
        .justify_center()
        .items_center();
    match agent.state {
        // 所有转圈的标记共用同一个时钟，同一种 agent 的几个标签一起转时步调一致。
        AgentState::Working => {
            let (frames, frame_time) = agent.kind.spinner();
            let period = frame_time * frames.len() as u32;
            slot.when_some(agent.kind.spinner_color(), |slot, color| slot.text_color(gpui::rgb(color)))
                .with_animation(
                    id,
                    Animation::new(period)
                        .repeat_synced()
                        .with_max_fps(2. / frame_time.as_secs_f32()),
                    move |slot, delta| {
                        let frame = (delta * frames.len() as f32) as usize;
                        slot.child(frames[frame.min(frames.len() - 1)])
                    },
                )
                .into_any_element()
        }
        AgentState::Idle => slot
            .child(
                div()
                    .size(px(6.))
                    .rounded_full()
                    .border_1()
                    .border_color(fg.opacity(0.6)),
            )
            .into_any_element(),
    }
}

/// 快捷键提示，从键位表里查，快捷键改了也跟着变：先找 `select` 的绑定，`is_last` 时再找
/// `last` 的。后加的绑定优先，显示最后一个。
fn shortcut_hint(select: &dyn Action, last: &dyn Action, is_last: bool, cx: &App) -> Option<SharedString> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let text = |action: &dyn Action| {
        keymap.bindings_for_action(action).next_back().map(|binding| {
            let strokes: Vec<_> = binding.keystrokes().iter().map(ToString::to_string).collect();
            SharedString::from(strokes.join(" "))
        })
    };
    text(select).or_else(|| if is_last { text(last) } else { None })
}

/// 第 `ix` 个标签的快捷键提示。默认是 ⌘1 到 ⌘8 对应前八个，⌘9 对应最后一个。
fn tab_shortcut(ix: usize, len: usize, cx: &App) -> Option<SharedString> {
    shortcut_hint(&SelectTab(ix), &SelectLastTab, ix + 1 == len, cx)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// 新 workspace 的名字：家目录叫 `~`；在 git 仓库里取仓库根的目录名，其余取目录名。
/// 往上找仓库根时不看家目录本身，免得家目录是个管配置文件的仓库时个个都叫用户名。
fn workspace_name(dir: &Path) -> String {
    let home = home_dir();
    if home.as_deref() == Some(dir) {
        return "~".into();
    }
    let root = dir
        .ancestors()
        .take_while(|d| Some(*d) != home.as_deref())
        .find(|d| d.join(".git").exists())
        .unwrap_or(dir);
    root.file_name()
        .map_or_else(|| dir.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// 侧栏里显示的目录，家目录写成 `~`。
fn display_dir(dir: &Path) -> String {
    if let Some(home) = home_dir()
        && let Ok(rest) = dir.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", rest.display());
    }
    dir.display().to_string()
}

/// workspace 的标识，挪动位置后不变。
type WorkspaceId = u64;

/// 一个项目目录，以及在其中打开的一组标签。
struct Workspace {
    id: WorkspaceId,
    /// 侧栏里显示的名字，新建时按目录取，可以改。
    name: SharedString,
    /// 项目目录：新终端取不到当前终端的目录时从这里开始。
    dir: PathBuf,
    /// 至少有一个；最后一个关掉时 workspace 跟着关。
    tabs: Vec<Tab>,
    active: usize,
    /// 标签条的滚动位置，切换标签时把当前标签滚进视野。
    tab_scroll: ScrollHandle,
}

impl Workspace {
    fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// 汇总各个终端前台的 agent：有在工作的算工作中，否则有空闲的算空闲。按标签和分屏的
    /// 顺序找，几种 agent 同时在时标记不会来回跳。
    fn agent(&self, cx: &App) -> Option<Agent> {
        let mut idle = None;
        for tab in &self.tabs {
            for id in tab.root.leaves() {
                let Some(agent) = tab.panes[&id].0.read(cx).agent() else {
                    continue;
                };
                if agent.is_working() {
                    return Some(agent);
                }
                idle.get_or_insert(agent);
            }
        }
        idle
    }

    /// 有标签响过铃或者里面的 agent 停了下来，还没切过去看。
    fn bell(&self) -> bool {
        self.tabs.iter().any(|tab| tab.bell)
    }

    fn pane_count(&self) -> usize {
        self.tabs.iter().map(|tab| tab.panes.len()).sum()
    }
}

/// 正在用鼠标拖动的分隔线。
#[derive(Clone, Copy)]
enum Divider {
    /// 分屏之间的分隔线。
    Split(SplitId, Axis),
    /// 侧栏右边的分隔线，拖动改变侧栏宽度。
    Sidebar,
}

/// 侧栏里正在改名的 workspace，以及改名用的输入框。
struct Renaming {
    id: WorkspaceId,
    field: Entity<SearchField>,
    _subscriptions: [Subscription; 2],
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
    renaming: Option<Renaming>,
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
}

impl WindowView {
    fn empty(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let bounds_watch = cx.observe_window_bounds(window, |this, window, cx| {
            this.bounds = window.window_bounds();
            this.display = Self::display_uuid(window, cx);
            this.save(cx);
        });
        persistence::track(cx);
        Self {
            workspaces: Vec::new(),
            active: 0,
            sidebar_shown: None,
            sidebar_width: None,
            sidebar_scroll: ScrollHandle::new(),
            renaming: None,
            next_id: 0,
            layout: Rc::default(),
            dragging_divider: None,
            bounds: window.window_bounds(),
            display: Self::display_uuid(window, cx),
            spawned: HashMap::new(),
            emptied: false,
            _bounds_watch: bounds_watch,
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
    pub fn restore(
        saved: SavedWindow,
        shell: Option<Prespawned>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn workspace(&self) -> &Workspace {
        &self.workspaces[self.active]
    }

    fn workspace_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active]
    }

    /// 窗口里正显示的标签。
    fn tab(&self) -> &Tab {
        self.workspace().active_tab()
    }

    fn tab_mut(&mut self) -> &mut Tab {
        let workspace = &mut self.workspaces[self.active];
        &mut workspace.tabs[workspace.active]
    }

    fn pane_entry(
        &mut self,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (EntityId, (Entity<TerminalView>, Subscription)) {
        let events = cx.subscribe_in(&view, window, Self::handle_terminal_event);
        (view.entity_id(), (view, events))
    }

    /// 记下刚开出来的终端从哪个目录开始，`start` 为空表示家目录。
    fn record_spawn(&mut self, view: &Entity<TerminalView>, start: Option<&Path>) {
        self.spawned.retain(|_, (at, _)| at.elapsed() < persistence::SHELL_STARTUP);
        let start = start.map(Path::to_path_buf).or_else(home_dir);
        self.spawned.insert(view.entity_id(), (Instant::now(), start));
    }

    /// 在 `cwd` 里启动一个终端，为空时在家目录；启动失败时记日志，返回 `None`。
    fn spawn_terminal(&mut self, cwd: Option<&Path>, window: &mut Window, cx: &mut App) -> Option<Entity<TerminalView>> {
        match TerminalView::spawn(cwd, window, cx) {
            Ok(view) => {
                self.record_spawn(&view, cwd);
                Some(view)
            }
            Err(err) => {
                tracing::error!("failed to start terminal session: {err:#}");
                None
            }
        }
    }

    /// 只有 `view` 一个终端的新标签。
    fn single_pane_tab(&mut self, view: Entity<TerminalView>, window: &mut Window, cx: &mut Context<Self>) -> Tab {
        let (id, entry) = self.pane_entry(view, window, cx);
        Tab {
            id: self.next_id(),
            root: Node::Leaf(id),
            panes: HashMap::from([(id, entry)]),
            focused: id,
            zoomed: false,
            bell: false,
        }
    }

    fn insert_tab(
        &mut self,
        ix: usize,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.single_pane_tab(view, window, cx);
        self.workspace_mut().tabs.insert(ix, tab);
        self.activate(ix, window, cx);
    }

    /// 在第 `ix` 个位置放一个目录是 `dir` 的新 workspace，`view` 是它的第一个终端，并切过去。
    fn insert_workspace(
        &mut self,
        ix: usize,
        dir: PathBuf,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.single_pane_tab(view, window, cx);
        let workspace = Workspace {
            id: self.next_id(),
            name: workspace_name(&dir).into(),
            dir,
            tabs: vec![tab],
            active: 0,
            tab_scroll: ScrollHandle::new(),
        };
        self.workspaces.insert(ix, workspace);
        self.activate_workspace(ix, window, cx);
    }

    /// 装着这个终端的 workspace 和标签。
    fn locate(&self, pane: EntityId) -> Option<(usize, usize)> {
        self.workspaces.iter().enumerate().find_map(|(wi, workspace)| {
            let ti = workspace.tabs.iter().position(|tab| tab.panes.contains_key(&pane))?;
            Some((wi, ti))
        })
    }

    /// 第 `wi` 个 workspace 的第 `ti` 个标签正显示在窗口里。
    fn is_shown(&self, wi: usize, ti: usize) -> bool {
        wi == self.active && ti == self.workspaces[wi].active
    }

    fn handle_terminal_event(
        &mut self,
        view: &Entity<TerminalView>,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = view.entity_id();
        let Some((wi, ti)) = self.locate(id) else {
            return;
        };
        let shown = self.is_shown(wi, ti);
        match event {
            TerminalEvent::TitleChanged => {
                if shown && self.tab().focused == id {
                    self.sync_window_title(window, cx);
                }
                cx.notify();
            }
            TerminalEvent::Focused => {
                let tab = &mut self.workspaces[wi].tabs[ti];
                if tab.focused != id {
                    tab.focused = id;
                    if shown {
                        self.sync_window_title(window, cx);
                    }
                    self.save(cx);
                    cx.notify();
                }
            }
            TerminalEvent::Bell => {
                window.play_system_bell();
                if !shown {
                    self.workspaces[wi].tabs[ti].bell = true;
                    cx.notify();
                }
            }
            TerminalEvent::AgentFinished => {
                if !shown {
                    self.workspaces[wi].tabs[ti].bell = true;
                    cx.notify();
                }
            }
            TerminalEvent::Exited => self.close_pane_by_id(id, window, cx),
        }
    }

    /// 切到当前 workspace 的第 `ix` 个标签。
    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace_mut();
        workspace.active = ix;
        workspace.tabs[ix].bell = false;
        workspace.tab_scroll.scroll_to_item(ix);
        self.start_shown(cx);
        window.focus(&self.tab().focused_view().focus_handle(cx), cx);
        self.sync_window_title(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// 启动显示中的标签里还没启动 shell 的终端（恢复布局时看不见的终端都等到这时）。
    fn start_shown(&mut self, cx: &mut Context<Self>) {
        let unstarted: Vec<_> = self
            .tab()
            .panes
            .values()
            .map(|(view, _)| view.clone())
            .filter(|view| !view.read(cx).started())
            .collect();
        for view in unstarted {
            let start = view.read(cx).cwd();
            view.update(cx, |view, cx| view.start(cx));
            // shell 现在才开始读启动配置，存档时这段时间仍记起始目录。
            self.record_spawn(&view, start.as_deref());
        }
    }

    /// 切到第 `ix` 个 workspace，显示它切走之前的那个标签。
    fn activate_workspace(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix;
        self.sidebar_scroll.scroll_to_item(ix);
        self.activate(self.workspaces[ix].active, window, cx);
    }

    fn sync_window_title(&self, window: &mut Window, cx: &App) {
        window.set_window_title(self.tab().focused_view().read(cx).title());
    }

    /// 把当前布局交给存档，有变化时稍后写进文件。
    fn save(&self, cx: &mut Context<Self>) {
        let snapshot = self.snapshot(cx);
        persistence::update(cx.weak_entity(), snapshot, cx);
    }

    /// 关掉第 `wi` 个 workspace 的第 `ti` 个标签；关掉的是它的当前标签时切到右边那个（没有
    /// 就左边）。workspace 里只剩这一个标签时关掉整个 workspace。
    fn close_tab_at(&mut self, wi: usize, ti: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspaces[wi].tabs.len() == 1 {
            self.close_workspace_at(wi, window, cx);
            return;
        }
        let workspace = &mut self.workspaces[wi];
        workspace.tabs.remove(ti);
        if ti < workspace.active || workspace.active == workspace.tabs.len() {
            workspace.active -= 1;
        }
        let active = workspace.active;
        if wi == self.active {
            self.activate(active, window, cx);
        } else {
            self.save(cx);
            cx.notify();
        }
    }

    fn close_tab_by_id(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.workspace().tabs.iter().position(|tab| tab.id == id) {
            self.close_tab_at(self.active, ix, window, cx);
        }
    }

    /// 关掉一个终端：它的兄弟分屏顶替上来，焦点交给兄弟一侧离它最近的终端；
    /// 标签里只剩它时关掉整个标签。
    fn close_pane_by_id(&mut self, pane: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((wi, ti)) = self.locate(pane) else {
            return;
        };
        let shown = self.is_shown(wi, ti);
        let tab = &mut self.workspaces[wi].tabs[ti];
        let Some(next) = tab.root.remove(pane) else {
            self.close_tab_at(wi, ti, window, cx);
            return;
        };
        tab.panes.remove(&pane);
        // 关的是别的分屏（比如后台的 shell 自己退出了）时，放大和焦点都不动。
        if tab.focused != pane {
            self.save(cx);
            cx.notify();
            return;
        }
        tab.focused = next;
        tab.zoomed = false;
        if shown {
            self.activate(ti, window, cx);
        } else {
            self.save(cx);
            cx.notify();
        }
    }

    /// 关掉第 `ix` 个 workspace，里面的终端都随之结束；关掉的是当前 workspace 时切到下面那个
    /// （没有就上面）。最后一个关掉时关窗口。
    fn close_workspace_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming.as_ref().is_some_and(|renaming| renaming.id == self.workspaces[ix].id) {
            self.renaming = None;
        }
        if self.workspaces.len() == 1 {
            self.emptied = true;
            window.remove_window();
            return;
        }
        self.workspaces.remove(ix);
        let active = if ix < self.active || self.active == self.workspaces.len() {
            self.active - 1
        } else {
            self.active
        };
        self.activate_workspace(active, window, cx);
    }

    /// 关掉第 `ix` 个 workspace；里面不止一个终端时先问一句，免得误点关掉一整个项目。
    fn confirm_close_workspace(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = &self.workspaces[ix];
        let count = workspace.pane_count();
        if count <= 1 {
            self.close_workspace_at(ix, window, cx);
            return;
        }
        let id = workspace.id;
        let title = rust_i18n::t!("workspace.close_title", name = workspace.name);
        let detail = rust_i18n::t!("workspace.close_detail", count = count);
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &title,
            Some(&detail),
            &[&*rust_i18n::t!("workspace.close_confirm"), &*rust_i18n::t!("workspace.close_cancel")],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            this.update_in(cx, |this, window, cx| {
                // 问的时候 workspace 可能已经挪了位置或者自己关掉了。
                if let Some(ix) = this.workspaces.iter().position(|workspace| workspace.id == id) {
                    this.close_workspace_at(ix, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 把拖动的标签挪到第 `to` 个位置，并切到它。
    fn move_tab(&mut self, id: TabId, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = &mut self.workspace_mut().tabs;
        let Some(from) = tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let tab = tabs.remove(from);
        let to = to.min(tabs.len());
        tabs.insert(to, tab);
        self.activate(to, window, cx);
    }

    /// 把拖动的 workspace 挪到第 `to` 个位置，并切到它。
    fn move_workspace(&mut self, id: WorkspaceId, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.workspaces.iter().position(|workspace| workspace.id == id) else {
            return;
        };
        let workspace = self.workspaces.remove(from);
        let to = to.min(self.workspaces.len());
        self.workspaces.insert(to, workspace);
        self.activate_workspace(to, window, cx);
    }

    /// 新终端从当前终端的 shell 所在目录开始。
    fn spawn_beside_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TerminalView>> {
        // 目录已经被删掉时 shell 起不来，退回 workspace 的目录，再退回家目录。
        let cwd = self.tab().focused_view().read(cx).cwd();
        let cwd = persist::start_dir(cwd.as_deref(), &self.workspace().dir);
        self.spawn_terminal(cwd.as_deref(), window, cx)
    }

    fn new_workspace(&mut self, _: &NewWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(rust_i18n::t!("workspace.choose").into_owned().into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| this.open_workspace(dir, window, cx)).ok();
        })
        .detach();
    }

    /// 切到目录是 `dir` 的 workspace，还没有时在当前 workspace 下面新建一个。
    fn open_workspace(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.workspaces.iter().position(|workspace| workspace.dir == dir) {
            self.activate_workspace(ix, window, cx);
            return;
        }
        if let Some(view) = self.spawn_terminal(Some(&dir), window, cx) {
            self.insert_workspace(self.active + 1, dir, view, window, cx);
        }
    }

    fn close_workspace(&mut self, _: &CloseWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_close_workspace(self.active, window, cx);
    }

    fn next_workspace(&mut self, _: &NextWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_workspace((self.active + 1) % self.workspaces.len(), window, cx);
    }

    fn previous_workspace(&mut self, _: &PreviousWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.workspaces.len();
        self.activate_workspace((self.active + len - 1) % len, window, cx);
    }

    fn select_workspace(&mut self, action: &SelectWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        if action.0 < self.workspaces.len() {
            self.activate_workspace(action.0, window, cx);
        }
    }

    fn select_last_workspace(&mut self, _: &SelectLastWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_workspace(self.workspaces.len() - 1, window, cx);
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_shown = Some(!self.sidebar_visible());
        self.save(cx);
        cx.notify();
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.spawn_beside_focused(window, cx) {
            self.insert_tab(self.workspace().active + 1, view, window, cx);
        }
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_at(self.active, self.workspace().active, window, cx);
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace();
        self.activate((workspace.active + 1) % workspace.tabs.len(), window, cx);
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace();
        let len = workspace.tabs.len();
        self.activate((workspace.active + len - 1) % len, window, cx);
    }

    fn select_tab(&mut self, action: &SelectTab, window: &mut Window, cx: &mut Context<Self>) {
        if action.0 < self.workspace().tabs.len() {
            self.activate(action.0, window, cx);
        }
    }

    fn select_last_tab(&mut self, _: &SelectLastTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(self.workspace().tabs.len() - 1, window, cx);
    }

    fn new_split_right(&mut self, _: &NewSplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Horizontal, window, cx);
    }

    fn new_split_down(&mut self, _: &NewSplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Axis::Vertical, window, cx);
    }

    /// 把当前终端一分为二，新终端放在右边或下边并获得焦点。
    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.spawn_beside_focused(window, cx) else {
            return;
        };
        let split_id = self.next_id();
        let (id, entry) = self.pane_entry(view, window, cx);
        let tab = self.tab_mut();
        tab.root.split(tab.focused, id, axis, split_id);
        tab.panes.insert(id, entry);
        tab.focused = id;
        tab.zoomed = false;
        self.activate(self.workspace().active, window, cx);
    }

    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.tab().focused;
        self.close_pane_by_id(focused, window, cx);
    }

    fn focus_next_pane(&mut self, _: &FocusNextPane, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_adjacent(1, window, cx);
    }

    fn focus_previous_pane(&mut self, _: &FocusPreviousPane, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_adjacent(-1, window, cx);
    }

    /// 按从左到右、从上到下的顺序切到后一个（`step` 为 1）或前一个终端，首尾相接。
    fn focus_adjacent(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab();
        let leaves = tab.root.leaves();
        let Some(at) = leaves.iter().position(|id| *id == tab.focused) else {
            return;
        };
        let next = leaves[(at as isize + step).rem_euclid(leaves.len() as isize) as usize];
        self.focus_pane_in_active_tab(next, window, cx);
    }

    fn focus_pane(&mut self, action: &FocusPane, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab();
        let layout = self.layout.borrow();
        let rect = |bounds: &Bounds<Pixels>| pane::Rect {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
        };
        let Some(from) = layout.panes.get(&tab.focused).map(rect) else {
            return;
        };
        let candidates = tab
            .root
            .leaves()
            .into_iter()
            .filter(|id| *id != tab.focused)
            .filter_map(|id| layout.panes.get(&id).map(|bounds| (id, rect(bounds))));
        let target = pane::neighbor(from, action.0, candidates);
        drop(layout);
        if let Some(target) = target {
            self.focus_pane_in_active_tab(target, window, cx);
        }
    }

    /// 切到当前标签里的另一个终端；放大着的话先恢复，否则看不到它。
    fn focus_pane_in_active_tab(&mut self, pane: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        tab.focused = pane;
        tab.zoomed = false;
        self.activate(self.workspace().active, window, cx);
    }

    fn resize_pane(&mut self, action: &ResizePane, _: &mut Window, cx: &mut Context<Self>) {
        let layout = self.layout.borrow();
        let horizontal = matches!(action.0, Direction::Left | Direction::Right);
        let size_of = |id: SplitId| {
            layout
                .splits
                .get(&id)
                .map(|bounds| f32::from(if horizontal { bounds.size.width } else { bounds.size.height }))
        };
        let workspace = &mut self.workspaces[self.active];
        let tab = &mut workspace.tabs[workspace.active];
        // 放大时其他分屏看不见，调了也看不出效果。
        let resized = !tab.zoomed && tab.root.resize(tab.focused, action.0, RESIZE_STEP, &size_of);
        drop(layout);
        if resized {
            self.save(cx);
            cx.notify();
        }
    }

    fn equalize_panes(&mut self, _: &EqualizePanes, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_mut().root.equalize();
        self.save(cx);
        cx.notify();
    }

    fn toggle_pane_zoom(&mut self, _: &TogglePaneZoom, _: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tab_mut();
        if !tab.root.is_leaf() {
            tab.zoomed = !tab.zoomed;
            self.save(cx);
            cx.notify();
        }
    }

    /// 当前标签的终端区：分屏树，或者放大着的那一个终端。
    fn render_panes(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.tab();
        if tab.zoomed || tab.root.is_leaf() {
            return self.render_leaf(tab, tab.focused, bg, cx);
        }
        self.render_node(tab, &tab.root, fg, bg, cx)
    }

    fn render_leaf(&self, tab: &Tab, id: EntityId, bg: Rgb, _: &mut Context<Self>) -> AnyElement {
        let view = tab.panes[&id].0.clone();
        let dimmed = !tab.zoomed && !tab.root.is_leaf() && id != tab.focused;
        let layout = self.layout.clone();
        div()
            .relative()
            .size_full()
            // 终端自己没变时复用上一帧画好的内容：标题栏和侧栏的转圈每一下都会重画整个窗口，
            // 不缓存的话每一下都要把所有终端格子重新排一遍。
            .child(view.cached(StyleRefinement::default().size_full()))
            .child(
                canvas(
                    move |bounds, _, _| {
                        layout.borrow_mut().panes.insert(id, bounds);
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            // 蒙层没有鼠标处理，点击照样落到下面的终端上。
            .when(dimmed, |leaf| {
                leaf.child(div().absolute().size_full().bg(hsla(bg).opacity(UNFOCUSED_DIM)))
            })
            .into_any_element()
    }

    fn render_node(&self, tab: &Tab, node: &Node<EntityId>, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> AnyElement {
        let split = match node {
            Node::Leaf(id) => return self.render_leaf(tab, *id, bg, cx),
            Node::Split(split) => split,
        };
        let (id, axis, ratio) = (split.id, split.axis, split.ratio);
        let horizontal = axis == Axis::Horizontal;
        let first = self.render_node(tab, &split.first, fg, bg, cx);
        let second = self.render_node(tab, &split.second, fg, bg, cx);
        let line = hsla(fg).opacity(0.15);
        let layout = self.layout.clone();
        div()
            .relative()
            .size_full()
            .flex()
            .when(!horizontal, |split| split.flex_col())
            .child(
                div()
                    .flex_none()
                    .map(|pane| if horizontal { pane.w(relative(ratio)).h_full() } else { pane.h(relative(ratio)).w_full() })
                    .min_w_0()
                    .min_h_0()
                    .child(first),
            )
            .child(div().flex_none().bg(line).map(|divider| {
                if horizontal { divider.w(px(1.)).h_full() } else { divider.h(px(1.)).w_full() }
            }))
            .child(div().flex_1().min_w_0().min_h_0().child(second))
            .child(
                canvas(
                    move |bounds, _, _| {
                        layout.borrow_mut().splits.insert(id, bounds);
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            // 分隔线只有一像素宽，在它两侧放一条透明的把手供拖动。
            .child(
                div()
                    .id(("divider", id))
                    .absolute()
                    .map(|handle| {
                        if horizontal {
                            handle
                                .top_0()
                                .h_full()
                                .w(px(DIVIDER_GRAB_WIDTH))
                                .left(relative(ratio))
                                .ml(px(-DIVIDER_GRAB_WIDTH / 2.))
                                .cursor(CursorStyle::ResizeLeftRight)
                        } else {
                            handle
                                .left_0()
                                .w_full()
                                .h(px(DIVIDER_GRAB_WIDTH))
                                .top(relative(ratio))
                                .mt(px(-DIVIDER_GRAB_WIDTH / 2.))
                                .cursor(CursorStyle::ResizeUpDown)
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            if event.click_count >= 2 {
                                // 双击分隔线让两边一样大。
                                this.tab_mut().root.set_ratio(id, 0.5);
                                this.save(cx);
                            } else {
                                this.dragging_divider = Some(Divider::Split(id, axis));
                            }
                            cx.notify();
                        }),
                    ),
            )
            .into_any_element()
    }

    /// 拖动分隔线期间盖在整个窗口上的一层：接住所有鼠标移动和松开，免得落进终端或侧栏。
    fn render_divider_drag(&self, divider: Divider, cx: &mut Context<Self>) -> Div {
        let vertical = matches!(divider, Divider::Split(_, Axis::Vertical));
        div()
            .absolute()
            .size_full()
            .occlude()
            .cursor(if vertical { CursorStyle::ResizeUpDown } else { CursorStyle::ResizeLeftRight })
            // 终端在窗口上监听移动和松开，这里拦下，拖动期间它们收不到。
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                cx.stop_propagation();
                let Some(divider) = this.dragging_divider else {
                    return;
                };
                // 在窗口外松开时收不到松开事件，回来时按键已经没按着了。
                if event.pressed_button != Some(MouseButton::Left) {
                    this.dragging_divider = None;
                    this.save(cx);
                    cx.notify();
                    return;
                }
                let (id, axis) = match divider {
                    Divider::Split(id, axis) => (id, axis),
                    Divider::Sidebar => {
                        this.resize_sidebar(f32::from(event.position.x));
                        cx.notify();
                        return;
                    }
                };
                let Some(bounds) = this.layout.borrow().splits.get(&id).copied() else {
                    return;
                };
                let ratio = if axis == Axis::Horizontal {
                    (event.position.x - bounds.origin.x) / bounds.size.width
                } else {
                    (event.position.y - bounds.origin.y) / bounds.size.height
                };
                this.tab_mut().root.set_ratio(id, ratio);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.dragging_divider = None;
                    this.save(cx);
                    cx.notify();
                }),
            )
    }

    fn render_tab(
        &self,
        ix: usize,
        width: Pixels,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let workspace = self.workspace();
        let tab = &workspace.tabs[ix];
        let id = tab.id;
        let view = tab.focused_view().read(cx);
        let title = SharedString::from(view.title().to_owned());
        let agent = view.agent();
        let active = ix == workspace.active;
        let active_bg = hsla(bg.mix(fg, 0.08));
        let hover_bg = hsla(bg.mix(fg, 0.04));
        let fg = hsla(fg);
        let group = SharedString::from(format!("tab-{ix}"));
        // 当前标签自己就是一块亮色，和它相邻的分隔线去掉，只是不画颜色，免得宽度跳动。
        let divider = if active || ix == workspace.active + 1 {
            fg.opacity(0.)
        } else {
            fg.opacity(0.12)
        };
        let dragged = DraggedTab {
            id,
            ix,
            title: title.clone(),
            width,
            fg,
            bg: active_bg,
        };
        let compact = width < px(TAB_COMPACT_WIDTH);
        let bell_dot = || div().size(px(6.)).rounded_full().bg(fg.opacity(0.8));
        // 右侧槽位：响铃标记优先，其次快捷键提示；紧凑时只在响铃时占一个圆点的宽度。
        let side_slot = if compact {
            tab.bell.then(|| div().flex_none().child(bell_dot()))
        } else {
            let content = if tab.bell {
                bell_dot().into_any_element()
            } else {
                div()
                    .text_size(px(11.))
                    .text_color(fg.opacity(0.35))
                    .children(tab_shortcut(ix, workspace.tabs.len(), cx))
                    .into_any_element()
            };
            Some(
                div()
                    .flex_none()
                    .w(px(TAB_SIDE_SLOT))
                    .flex()
                    .justify_end()
                    .items_center()
                    .child(content),
            )
        };
        div()
            .id(("tab", ix))
            .group(group.clone())
            .flex_none()
            .w(width)
            .h_full()
            .overflow_hidden()
            .flex()
            .items_center()
            .px(px(if compact { 4. } else { 8. }))
            .gap(px(4.))
            .when(ix > 0, |tab| tab.border_l_1().border_color(divider))
            .map(|tab| {
                if active {
                    tab.bg(active_bg).text_color(fg)
                } else {
                    tab.text_color(fg.opacity(0.55))
                        .hover(|tab| tab.bg(hover_bg).text_color(fg.opacity(0.8)))
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    // 标签铺满了标题栏，双击标签和双击空白标题栏一样缩放窗口。
                    if event.click_count >= 2 {
                        window.titlebar_double_click();
                    } else {
                        this.activate(ix, window, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_tab_by_id(id, window, cx);
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            // 落点提示画在目标标签靠近原位置的另一侧：往右拖插到它右边，往左拖插到它左边。
            .drag_over::<DraggedTab>(move |style, dragged, _, _| {
                let marker = fg.opacity(0.6);
                if dragged.ix < ix {
                    style.border_r_2().border_color(marker)
                } else if dragged.ix > ix {
                    style.border_l_2().border_color(marker)
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                this.move_tab(dragged.id, ix, window, cx);
            }))
            .child(
                div()
                    .id(("tab-close", ix))
                    .flex_none()
                    .size(px(TAB_CLOSE_SIZE))
                    .rounded(px(3.))
                    .flex()
                    .items_center()
                    .justify_center()
                    // 紧凑时平时宽度为零，悬停时才撑开，标题让出位置，两者不重叠。
                    // 不能用 display 切换：悬停状态在 prepaint 和 paint 之间可能变化，
                    // prepaint 时隐藏、paint 时显示会让 gpui 去画没 prepaint 过的子元素而 panic。
                    .map(|close| {
                        if compact {
                            close
                                .w_0()
                                .overflow_hidden()
                                .group_hover(group, |close| close.w(px(TAB_CLOSE_SIZE)))
                        } else {
                            close.invisible().group_hover(group, |close| close.visible())
                        }
                    })
                    .text_size(px(14.))
                    .text_color(fg.opacity(0.75))
                    .hover(|close| close.bg(fg.opacity(0.18)).text_color(fg))
                    .child("×")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_tab_by_id(id, window, cx);
                        }),
                    ),
            )
            .child(titled(title, agent, ("tab-agent", ix), fg).flex_1())
            .children(side_slot)
    }

    fn render_new_tab_button(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let hover_bg = hsla(bg.mix(fg, 0.04));
        let fg = hsla(fg);
        div()
            .id("new-tab")
            .flex_none()
            .w(px(NEW_TAB_BUTTON_WIDTH))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .border_l_1()
            .border_color(fg.opacity(0.12))
            .text_size(px(16.))
            .text_color(fg.opacity(0.55))
            .hover(|button| button.bg(hover_bg).text_color(fg))
            .child("+")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.new_tab(&NewTab, window, cx);
                }),
            )
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
        let panes = self.render_panes(fg, bg, cx);
        let drag = self.dragging_divider.map(|divider| self.render_divider_drag(divider, cx));
        let sidebar = self.sidebar_visible().then(|| self.render_sidebar(fg, bg, fullscreen, cx));
        // 标题栏透明后内容铺到红绿灯下面，顶部这条要能拖动窗口、双击缩放。红绿灯和侧栏开关
        // 落在侧栏上或者全屏时没有红绿灯，标题栏不用让位；全屏又只有一个标签时不留这一条。
        let left_inset = if fullscreen || sidebar.is_some() { 0. } else { sidebar::SIDEBAR_TOGGLE_INSET };
        let sidebar_toggle = (!fullscreen).then(|| self.render_sidebar_toggle(fg, bg, cx));
        let sidebar_width = if sidebar.is_some() { self.sidebar_width() } else { 0. };
        let sidebar_handle = sidebar.is_some().then(|| self.render_sidebar_handle(cx));
        let tabs: Vec<_> = if show_tabs {
            // 标签平分标题栏除去两头的宽度，但不窄于 `TAB_MIN_WIDTH`，挤不下就让标签条滚动；
            // 拖动时的预览也照这个宽度画。
            let tab_width = ((window.viewport_size().width
                - px(sidebar_width + left_inset + NEW_TAB_BUTTON_WIDTH))
                / tab_count as f32)
                .max(px(TAB_MIN_WIDTH));
            let strip = div()
                .id("tabs")
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .overflow_x_scroll()
                .track_scroll(&self.workspace().tab_scroll)
                .children(
                    (0..tab_count)
                        .map(|ix| self.render_tab(ix, tab_width, fg, bg, cx))
                        .collect::<Vec<_>>(),
                );
            vec![strip, self.render_new_tab_button(fg, bg, cx)]
        } else {
            // 只有一个标签时标题居中画在侧栏右边的整个宽度上；没有侧栏时右边留出和红绿灯
            // 一样宽的空白，标题在整个窗口里居中。
            let view = view.read(cx);
            let title = SharedString::from(view.title().to_owned());
            let fg = hsla(fg);
            vec![
                div()
                    .id("window-title")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pr(px(left_inset))
                    .flex()
                    .text_color(fg.opacity(0.55))
                    .child(titled(title, view.agent(), "window-agent", fg).flex_1()),
            ]
        };
        let titlebar = (!fullscreen || show_tabs).then(|| {
            div()
                .id("titlebar")
                .h(px(TITLEBAR_HEIGHT))
                .flex_none()
                .flex()
                .text_size(px(12.))
                .on_mouse_down(MouseButton::Left, |event, window, _| {
                    if event.click_count >= 2 {
                        window.titlebar_double_click();
                    } else {
                        window.start_window_move();
                    }
                })
                // 给红绿灯和侧栏开关让出的位置；后面跟着标签时右边画一条分隔线，和新建标签按钮
                // 左边那条对称。
                .child(div().flex_none().w(px(left_inset)).h_full().when(show_tabs && left_inset > 0., |inset| {
                    inset.border_r_1().border_color(hsla(fg).opacity(0.12))
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
            .children(sidebar_handle)
            .children(sidebar_toggle)
            .children(drag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_name_uses_the_repository_root() {
        let base = std::env::temp_dir().join(format!("runode-workspace-name-{}", std::process::id()));
        let sub = base.join("repo/crates/app");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(base.join("repo/.git")).unwrap();
        std::fs::create_dir_all(base.join("plain")).unwrap();
        assert_eq!(workspace_name(&sub), "repo");
        assert_eq!(workspace_name(&base.join("repo")), "repo");
        assert_eq!(workspace_name(&base.join("plain")), "plain");
        std::fs::remove_dir_all(&base).unwrap();
    }
}
