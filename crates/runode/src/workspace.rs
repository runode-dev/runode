//! 一个窗口里的一组终端标签：顶部标签栏，新建、切换和关闭标签，以及标签里的分屏。

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use gpui::{
    Action, Animation, AnimationExt, AnyElement, App, Bounds, BoxShadow, Context, CursorStyle, Div,
    ElementId, Entity, EntityId, FocusHandle, Focusable, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, Pixels, Render, ScrollHandle, SharedString, Stateful, Subscription,
    TitlebarOptions, Window, actions, canvas, div, point, prelude::*, px, relative,
};

use crate::{
    agent::{Agent, AgentState},
    pane::{self, Axis, Direction, Node, SplitId},
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
        TogglePaneZoom
    ]
);

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

/// 透明标题栏的高度：终端内容从它下面开始，这一条用来拖动窗口，多个标签时也画在这里。
const TITLEBAR_HEIGHT: f32 = 28.;
/// 红绿灯按钮的位置，以及标题栏左侧给它们留出的宽度。
const TRAFFIC_LIGHTS_ORIGIN: (f32, f32) = (12., 10.);
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

/// 窗口的标题栏设置：透明，终端背景一直铺到窗口顶部，标签栏画在红绿灯右边。
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
            slot.with_animation(
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

/// 第 `ix` 个标签的快捷键提示，从键位表里查，快捷键改了也跟着变：先找切到这个标签的
/// 绑定，最后一个标签再找切到最后一个标签的。默认是 ⌘1 到 ⌘8 对应前八个，⌘9 对应最后一个。
fn tab_shortcut(ix: usize, len: usize, cx: &App) -> Option<SharedString> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    // 后加的绑定优先，显示最后一个。
    let text = |action: &dyn Action| {
        keymap.bindings_for_action(action).next_back().map(|binding| {
            let strokes: Vec<_> = binding.keystrokes().iter().map(ToString::to_string).collect();
            SharedString::from(strokes.join(" "))
        })
    };
    text(&SelectTab(ix)).or_else(|| if ix + 1 == len { text(&SelectLastTab) } else { None })
}

pub struct Workspace {
    /// 至少有一个；最后一个关掉时窗口跟着关。
    tabs: Vec<Tab>,
    active: usize,
    /// 标签条的滚动位置，切换标签时把当前标签滚进视野。
    tab_scroll: ScrollHandle,
    /// 标签和分屏节点的标识都从这里取。
    next_id: u64,
    layout: Rc<RefCell<PaneLayout>>,
    /// 正在用鼠标拖动的分隔线。
    dragging_divider: Option<(SplitId, Axis)>,
}

impl Workspace {
    pub fn new(first: Entity<TerminalView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            tab_scroll: ScrollHandle::new(),
            next_id: 0,
            layout: Rc::default(),
            dragging_divider: None,
        };
        this.insert_tab(0, first, window, cx);
        this
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn pane_entry(
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (EntityId, (Entity<TerminalView>, Subscription)) {
        let events = cx.subscribe_in(&view, window, Self::handle_terminal_event);
        (view.entity_id(), (view, events))
    }

    fn insert_tab(
        &mut self,
        ix: usize,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (id, entry) = Self::pane_entry(view, window, cx);
        let tab = Tab {
            id: self.next_id(),
            root: Node::Leaf(id),
            panes: HashMap::from([(id, entry)]),
            focused: id,
            zoomed: false,
            bell: false,
        };
        self.tabs.insert(ix, tab);
        self.activate(ix, window, cx);
    }

    /// 装着这个终端的标签。
    fn tab_of(&self, pane: EntityId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.panes.contains_key(&pane))
    }

    fn handle_terminal_event(
        &mut self,
        view: &Entity<TerminalView>,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = view.entity_id();
        let Some(ix) = self.tab_of(id) else {
            return;
        };
        match event {
            TerminalEvent::TitleChanged => {
                if ix == self.active && self.tabs[ix].focused == id {
                    self.sync_window_title(window, cx);
                }
                cx.notify();
            }
            TerminalEvent::Focused => {
                if self.tabs[ix].focused != id {
                    self.tabs[ix].focused = id;
                    if ix == self.active {
                        self.sync_window_title(window, cx);
                    }
                    cx.notify();
                }
            }
            TerminalEvent::Bell => {
                window.play_system_bell();
                if ix != self.active {
                    self.tabs[ix].bell = true;
                    cx.notify();
                }
            }
            TerminalEvent::AgentFinished => {
                if ix != self.active {
                    self.tabs[ix].bell = true;
                    cx.notify();
                }
            }
            TerminalEvent::Exited => self.close_pane_by_id(id, window, cx),
        }
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix;
        self.tabs[ix].bell = false;
        self.tab_scroll.scroll_to_item(ix);
        window.focus(&self.tabs[ix].focused_view().focus_handle(cx), cx);
        self.sync_window_title(window, cx);
        cx.notify();
    }

    fn sync_window_title(&self, window: &mut Window, cx: &App) {
        window.set_window_title(self.tabs[self.active].focused_view().read(cx).title());
    }

    /// 关掉第 `ix` 个标签；关掉的是当前标签时切到它右边那个（没有就左边）。
    fn close_tab_at(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() == 1 {
            window.remove_window();
            return;
        }
        self.tabs.remove(ix);
        let active = if ix < self.active || self.active == self.tabs.len() {
            self.active - 1
        } else {
            self.active
        };
        self.activate(active, window, cx);
    }

    fn close_tab_by_id(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|tab| tab.id == id) {
            self.close_tab_at(ix, window, cx);
        }
    }

    /// 关掉一个终端：它的兄弟分屏顶替上来，焦点交给兄弟一侧离它最近的终端；
    /// 标签里只剩它时关掉整个标签。
    fn close_pane_by_id(&mut self, pane: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tab_of(pane) else {
            return;
        };
        let tab = &mut self.tabs[ix];
        let Some(next) = tab.root.remove(pane) else {
            self.close_tab_at(ix, window, cx);
            return;
        };
        tab.panes.remove(&pane);
        // 关的是别的分屏（比如后台的 shell 自己退出了）时，放大和焦点都不动。
        if tab.focused != pane {
            cx.notify();
            return;
        }
        tab.focused = next;
        tab.zoomed = false;
        if ix == self.active {
            self.activate(ix, window, cx);
        } else {
            cx.notify();
        }
    }

    /// 把拖动的标签挪到第 `to` 个位置，并切到它。
    fn move_tab(&mut self, id: TabId, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let tab = self.tabs.remove(from);
        let to = to.min(self.tabs.len());
        self.tabs.insert(to, tab);
        self.activate(to, window, cx);
    }

    /// 新终端从当前终端的 shell 所在目录开始。
    fn spawn_beside_focused(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TerminalView>> {
        // 目录已经被删掉时 shell 起不来，退回家目录。
        let cwd = self.tabs[self.active].focused_view().read(cx).cwd().filter(|cwd| cwd.is_dir());
        match TerminalView::spawn(cwd.as_deref(), window, cx) {
            Ok(view) => Some(view),
            Err(err) => {
                tracing::error!("failed to start terminal session: {err:#}");
                None
            }
        }
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.spawn_beside_focused(window, cx) {
            self.insert_tab(self.active + 1, view, window, cx);
        }
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_at(self.active, window, cx);
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate((self.active + 1) % self.tabs.len(), window, cx);
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.tabs.len();
        self.activate((self.active + len - 1) % len, window, cx);
    }

    fn select_tab(&mut self, action: &SelectTab, window: &mut Window, cx: &mut Context<Self>) {
        if action.0 < self.tabs.len() {
            self.activate(action.0, window, cx);
        }
    }

    fn select_last_tab(&mut self, _: &SelectLastTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(self.tabs.len() - 1, window, cx);
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
        let (id, entry) = Self::pane_entry(view, window, cx);
        let tab = &mut self.tabs[self.active];
        tab.root.split(tab.focused, id, axis, split_id);
        tab.panes.insert(id, entry);
        tab.focused = id;
        tab.zoomed = false;
        self.activate(self.active, window, cx);
    }

    fn close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.tabs[self.active].focused;
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
        let tab = &self.tabs[self.active];
        let leaves = tab.root.leaves();
        let Some(at) = leaves.iter().position(|id| *id == tab.focused) else {
            return;
        };
        let next = leaves[(at as isize + step).rem_euclid(leaves.len() as isize) as usize];
        self.focus_pane_in_active_tab(next, window, cx);
    }

    fn focus_pane(&mut self, action: &FocusPane, window: &mut Window, cx: &mut Context<Self>) {
        let tab = &self.tabs[self.active];
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
        let tab = &mut self.tabs[self.active];
        tab.focused = pane;
        tab.zoomed = false;
        self.activate(self.active, window, cx);
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
        let tab = &mut self.tabs[self.active];
        // 放大时其他分屏看不见，调了也看不出效果。
        if !tab.zoomed && tab.root.resize(tab.focused, action.0, RESIZE_STEP, &size_of) {
            cx.notify();
        }
    }

    fn equalize_panes(&mut self, _: &EqualizePanes, _: &mut Window, cx: &mut Context<Self>) {
        self.tabs[self.active].root.equalize();
        cx.notify();
    }

    fn toggle_pane_zoom(&mut self, _: &TogglePaneZoom, _: &mut Window, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active];
        if !tab.root.is_leaf() {
            tab.zoomed = !tab.zoomed;
            cx.notify();
        }
    }

    /// 当前标签的终端区：分屏树，或者放大着的那一个终端。
    fn render_panes(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> AnyElement {
        let tab = &self.tabs[self.active];
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
            .child(view)
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
                                this.tabs[this.active].root.set_ratio(id, 0.5);
                            } else {
                                this.dragging_divider = Some((id, axis));
                            }
                            cx.notify();
                        }),
                    ),
            )
            .into_any_element()
    }

    /// 拖动分隔线期间盖在终端区上的一层：接住所有鼠标移动和松开，免得落进终端。
    fn render_divider_drag(&self, axis: Axis, cx: &mut Context<Self>) -> Div {
        div()
            .absolute()
            .size_full()
            .occlude()
            .cursor(if axis == Axis::Horizontal { CursorStyle::ResizeLeftRight } else { CursorStyle::ResizeUpDown })
            // 终端在窗口上监听移动和松开，这里拦下，拖动期间它们收不到。
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                cx.stop_propagation();
                let Some((id, axis)) = this.dragging_divider else {
                    return;
                };
                // 在窗口外松开时收不到松开事件，回来时按键已经没按着了。
                if event.pressed_button != Some(MouseButton::Left) {
                    this.dragging_divider = None;
                    cx.notify();
                    return;
                }
                let Some(bounds) = this.layout.borrow().splits.get(&id).copied() else {
                    return;
                };
                let ratio = if axis == Axis::Horizontal {
                    (event.position.x - bounds.origin.x) / bounds.size.width
                } else {
                    (event.position.y - bounds.origin.y) / bounds.size.height
                };
                this.tabs[this.active].root.set_ratio(id, ratio);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.dragging_divider = None;
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
        let tab = &self.tabs[ix];
        let id = tab.id;
        let view = tab.focused_view().read(cx);
        let title = SharedString::from(view.title().to_owned());
        let agent = view.agent();
        let active = ix == self.active;
        let active_bg = hsla(bg.mix(fg, 0.08));
        let hover_bg = hsla(bg.mix(fg, 0.04));
        let fg = hsla(fg);
        let group = SharedString::from(format!("tab-{ix}"));
        // 当前标签自己就是一块亮色，和它相邻的分隔线去掉，只是不画颜色，免得宽度跳动。
        let divider = if active || ix == self.active + 1 {
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
                    .children(tab_shortcut(ix, self.tabs.len(), cx))
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

impl Focusable for Workspace {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.tabs[self.active].focused_view().focus_handle(cx)
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.tabs[self.active].focused_view().clone();
        let (fg, bg) = view.update(cx, |view, _| view.colors());
        let fullscreen = window.is_fullscreen();
        let show_tabs = self.tabs.len() > 1;
        let panes = self.render_panes(fg, bg, cx);
        let drag = self.dragging_divider.map(|(_, axis)| self.render_divider_drag(axis, cx));
        // 标题栏透明后内容铺到红绿灯下面，顶部这条要能拖动窗口、双击缩放。
        // 全屏时没有红绿灯，只有一个标签时不留这一条。
        let left_inset = if fullscreen { 0. } else { TRAFFIC_LIGHTS_WIDTH };
        let tabs: Vec<_> = if show_tabs {
            // 标签平分标题栏除去两头的宽度，但不窄于 `TAB_MIN_WIDTH`，挤不下就让标签条滚动；
            // 拖动时的预览也照这个宽度画。
            let tab_width = ((window.viewport_size().width
                - px(left_inset + NEW_TAB_BUTTON_WIDTH))
                / self.tabs.len() as f32)
                .max(px(TAB_MIN_WIDTH));
            let strip = div()
                .id("tabs")
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .overflow_x_scroll()
                .track_scroll(&self.tab_scroll)
                .children(
                    (0..self.tabs.len())
                        .map(|ix| self.render_tab(ix, tab_width, fg, bg, cx))
                        .collect::<Vec<_>>(),
                );
            vec![strip, self.render_new_tab_button(fg, bg, cx)]
        } else {
            // 只有一个标签时标题居中画在整个窗口宽度上，右边留出和红绿灯一样宽的空白。
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
                .pl(px(left_inset))
                .text_size(px(12.))
                .on_mouse_down(MouseButton::Left, |event, window, _| {
                    if event.click_count >= 2 {
                        window.titlebar_double_click();
                    } else {
                        window.start_window_move();
                    }
                })
                .children(tabs)
        });
        div()
            .id("workspace")
            .key_context("Workspace")
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
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(bg))
            .children(titlebar)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(panes)
                    .children(drag),
            )
    }
}
