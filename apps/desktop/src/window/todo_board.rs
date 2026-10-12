//! 铺满窗口的任务板：派给 agent 的任务按列排开（待办、进行中、需要你、完成），和它平级的另一页列出
//! 定时。数据由宿主持有，这边经 `store` 读写；新建、编辑任务和定时的对话框在 `dialog`。
//!
//! 卡片在哪一列由任务和它会话里 agent 的状态推出来（`runode_protocol::todo::column`），拖拽就是做那一列
//! 的动作（`drop_action`）：拖进进行中是开始，拖进完成是标完成，从完成拖出是取消完成，同一列里上下拖调
//! 先后；办不到的放下不接，卡片留在原处。辅助工具拖不了：动作另有按钮，调先后另登记了加减（上移、下移）。
//!
//! 看板和手机端引导页一样盖住标签和分屏：切到任何一个标签或 workspace、按 Esc 就收起。卡片的「打开终端」
//! 把任务的会话接进它目录所在的 workspace 的一个新标签，已经在哪个分屏里时切过去。
//!
//! 卡片换了列时从原来的位置飞到新的位置，落下后淡入、描一圈强调色再褪掉；拖着放下的不飞，只描边。
//! 每帧记下各卡片画在哪（`Geometry`），下一帧发现换了列就照上一帧的位置起飞。开始任务时顶上居中弹出
//! 提示条（`Toast`），等宿主回话后说开始了没有。

mod dialog;
pub(super) mod store;

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gpui::{
    AccessibleAction, Animation, AnimationExt as _, AnyElement, Axis, Bounds, BoxShadow, Context, Div, ElementId,
    FocusHandle, Focusable as _, FontWeight, Hsla, KeyDownEvent, Pixels, Render, Role, ScrollHandle, SharedString,
    Stateful, Window, canvas, div, ease_out_quint, point, prelude::*, px, svg,
};
use runode_protocol::{
    ClientMsg, SessionId,
    todo::{Column, DropAction, Schedule, Todo, When, column, drop_action},
};
use runode_shared_types::color::Rgb;

pub(super) use dialog::TodoDialog;
use store::TodoData;

use super::{
    ToggleTodoBoard, WindowView,
    agents::{self, Mark},
    model::base_name,
    remote,
    titlebar::{AGENT_BLOCKED_COLOR, AGENT_DONE_COLOR, drag_chip, drop_marker, spinner, styled_agent_mark},
};
use crate::{
    assets::{BRANCH_ICON, CHECK_ICON, CLOSE_ICON, PENCIL_ICON, PLUS_ICON, TERMINAL_ICON, TRASH_ICON},
    config::AppConfig,
    terminal_view::TerminalView,
    ui::{
        a11y::Press,
        font_size::{FontSize, scaled},
        hsla,
        scrollbar::scrollbar,
        tooltip::tooltip,
    },
};

/// 一列最窄这么宽（基准字号下的点数）；窗口放不下四列时横着滚。
const COLUMN_MIN_WIDTH: f32 = 240.;
/// 定时那一页里每个定时下面最多列它最近开出的几张任务。
const RECENT_RUNS: usize = 3;
/// 强调色：主按钮、落下后的描边、需要你那一列的计数。
const ACCENT: u32 = 0x4F46E5;
/// 进行中那一列的圆点。
const IN_PROGRESS_COLOR: u32 = 0x3B82F6;
/// 换列的卡片飞多久。
const FLY: Duration = Duration::from_millis(380);
/// 落下后淡入、描边褪掉要多久。
const LAND: Duration = Duration::from_millis(1200);
/// 落下后描的那圈强调色有多宽。
const RING_WIDTH: f32 = 1.5;
/// 飞着的时候新位置上留的影子有多透明。
const GHOST_OPACITY: f32 = 0.3;
/// 飞到这一段（占飞行时间的比例）以后，飞的卡片淡出、新位置上的影子淡入。
const CROSSFADE: f32 = 0.6;
/// 「任务已开始」这类提示留多久。
const TOAST_LINGER: Duration = Duration::from_secs(2);
/// 提示条上转圈的几格。
const TOAST_SPINNER: (&[&str], Duration) =
    (&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"], Duration::from_millis(80));

/// 开着的任务板。
pub(super) struct TodoBoard {
    pub(super) focus: FocusHandle,
    page: Page,
    columns_scroll: ScrollHandle,
    schedules_scroll: ScrollHandle,
    /// 上一帧各卡片和看板画在哪，画的时候记下。
    geometry: Rc<RefCell<Geometry>>,
    /// 上一帧各任务在哪一列；刚打开、刚切回任务页时为空，这一帧不飞。
    seen: Option<HashMap<String, Column>>,
    /// 正在换列的卡片。
    moves: HashMap<String, Move>,
    /// 刚拖着放下的卡片，它换列时不飞。
    dropped: Option<String>,
    /// 鼠标在哪张卡片上，那张露出一排按钮。
    hovered: Option<String>,
    toast: Option<Toast>,
    /// 动画和提示条用的编号，每用一次加一，动画的元素 id 跟着换才会从头放。
    serial: u64,
}

/// 窗口坐标下的位置。
#[derive(Default)]
struct Geometry {
    board: Bounds<Pixels>,
    cards: HashMap<String, Bounds<Pixels>>,
}

/// 一张换了列的卡片。
struct Move {
    /// 从哪儿飞来；拖着放下的为空，只描边不飞。
    from: Option<Bounds<Pixels>>,
    serial: u64,
    at: Instant,
}

/// 顶上的提示条。
struct Toast {
    serial: u64,
    state: ToastState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToastState {
    Starting,
    Started,
    Failed,
}

/// 任务板上并列的两页。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Todos,
    Schedules,
}

/// 拖着的卡片：跟着鼠标画出来，放到别处时按 `id` 做那一列的动作。
#[derive(Clone)]
struct DraggedTodo {
    id: String,
    /// 拖起时所在的列和在列里的位置，落点提示据此画在目标卡片的上边还是下边。
    from: Column,
    ix: usize,
    /// 放进各列（按 `Column::ALL` 的顺序）时做什么；办不到的为空，那一列不接。
    drops: [Option<DropAction>; 4],
    title: SharedString,
    fg: Hsla,
    bg: Hsla,
}

impl DraggedTodo {
    fn drop_into(&self, column: Column) -> Option<DropAction> {
        self.drops[column_index(column)]
    }
}

impl Render for DraggedTodo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scale = crate::ui::font_size::scale(cx);
        drag_chip(px(COLUMN_MIN_WIDTH * scale), px(32. * scale), self.title.clone(), self.fg, self.bg)
    }
}

/// 按显示的东西（任务、定时的标识）取的元素 id。
fn named(prefix: &str, id: &str) -> ElementId {
    ElementId::Name(format!("{prefix}-{id}").into())
}

fn column_index(column: Column) -> usize {
    Column::ALL.iter().position(|c| *c == column).unwrap_or(0)
}

/// 现在的 Unix 时间，秒。
fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

/// 按列分好的任务，列里保持宿主给的先后。
fn columns(data: &TodoData, now: u64) -> [Vec<&Todo>; 4] {
    let mut columns: [Vec<&Todo>; 4] = Default::default();
    for todo in &data.todos {
        let session = todo.session.and_then(|id| data.sessions.get(&id));
        columns[column_index(column(todo, session, now))].push(todo);
    }
    columns
}

/// 任务会话该接进哪个 workspace（`dirs` 是各 workspace 的目录）：目录正好是任务的工作树、正好是任务的
/// 目录、是任务目录最深的上级（家目录不算，免得任务都塞进家目录那个），依次找；都没有时为空。
fn best_workspace(dirs: &[&Path], dir: &Path, worktree: Option<&Path>, home: Option<&Path>) -> Option<usize> {
    let exact = |wanted: &Path| dirs.iter().position(|candidate| *candidate == wanted);
    worktree.and_then(exact).or_else(|| exact(dir)).or_else(|| {
        dirs.iter()
            .enumerate()
            .filter(|(_, candidate)| dir.starts_with(candidate) && Some(**candidate) != home)
            .max_by_key(|(_, candidate)| candidate.components().count())
            .map(|(ix, _)| ix)
    })
}

impl WindowView {
    /// 打开或收起任务板。
    pub(super) fn toggle_todo_board(&mut self, _: &ToggleTodoBoard, window: &mut Window, cx: &mut Context<Self>) {
        if self.todo_board.is_some() {
            self.close_todo_board(window, cx);
        } else {
            self.show_todo_board(window, cx);
        }
    }

    /// 打开任务板，焦点给它；马上重问一次任务会话的状态，开着时接着轮询。
    pub(super) fn show_todo_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mobile = None;
        self.drop_settings(cx);
        let board = self.todo_board.get_or_insert_with(|| TodoBoard {
            focus: cx.focus_handle(),
            page: Page::Todos,
            columns_scroll: ScrollHandle::new(),
            schedules_scroll: ScrollHandle::new(),
            geometry: Rc::default(),
            seen: None,
            moves: HashMap::new(),
            dropped: None,
            hovered: None,
            toast: None,
            serial: 0,
        });
        window.focus(&board.focus, cx);
        store::poll_sessions(cx);
        cx.notify();
    }

    pub(super) fn close_todo_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.todo_board.take().is_some() {
            self.todo_dialog = None;
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    fn set_todo_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if let Some(board) = &mut self.todo_board {
            board.page = page;
            board.seen = None;
            cx.notify();
        }
    }

    /// 依次发几条任务板的请求，前一条没办成就不发后面的。
    fn send_todos(steps: Vec<store::Step>, cx: &mut Context<Self>) {
        store::send(cx, steps).detach();
    }

    /// 发开始任务的几条请求，顶上弹出提示条，等宿主回话后说开始了没有，过一会儿收起。
    fn send_start(&mut self, steps: Vec<store::Step>, cx: &mut Context<Self>) {
        let sent = store::send(cx, steps);
        let Some(board) = &mut self.todo_board else { return sent.detach() };
        board.serial += 1;
        let serial = board.serial;
        board.toast = Some(Toast { serial, state: ToastState::Starting });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let state = if sent.await { ToastState::Started } else { ToastState::Failed };
            this.update(cx, |this, cx| this.set_toast(serial, Some(state), cx)).ok();
            cx.background_executor().timer(TOAST_LINGER).await;
            this.update(cx, |this, cx| this.set_toast(serial, None, cx)).ok();
        })
        .detach();
    }

    /// 换掉或收起第 `serial` 个提示条；它已经被新的顶掉、或者关掉了就不动。
    fn set_toast(&mut self, serial: u64, state: Option<ToastState>, cx: &mut Context<Self>) {
        let Some(board) = &mut self.todo_board else { return };
        if board.toast.as_ref().is_some_and(|toast| toast.serial == serial) {
            board.toast = state.map(|state| Toast { serial, state });
            cx.notify();
        }
    }

    /// 卡片拖到 `to` 列放下：先做那一列的动作，再挪到 `before` 前面（放在列里的空白处时为空，不挪；同一列里
    /// 放在空白处时挪到这一列的最后）。
    fn drop_todo(&mut self, dragged: &DraggedTodo, to: Column, before: Option<String>, cx: &mut Context<Self>) {
        let Some(action) = dragged.drop_into(to) else { return };
        let id = dragged.id.clone();
        if let Some(board) = &mut self.todo_board {
            board.dropped = Some(id.clone());
        }
        let mut steps: Vec<store::Step> = Vec::new();
        {
            let id = id.clone();
            match action {
                DropAction::Reorder => {}
                DropAction::Start => steps.push(Box::new(move |req| ClientMsg::StartTodo { req, id })),
                DropAction::Finish | DropAction::Reopen => {
                    let done = action == DropAction::Finish;
                    steps.push(Box::new(move |req| ClientMsg::UpdateTodo { req, id, spec: None, done: Some(done) }));
                }
            }
        }
        let before = match before {
            Some(before) if before == id => None,
            Some(before) => Some(Some(before)),
            None if action == DropAction::Reorder => Some(self.after_column(to, cx)),
            None => None,
        };
        if let Some(before) = before {
            steps.push(Box::new(move |req| ClientMsg::MoveTodo { req, id, before }));
        }
        if action == DropAction::Start {
            self.send_start(steps, cx);
        } else {
            Self::send_todos(steps, cx);
        }
    }

    /// 排到 `column` 这一列最后要挪到谁前面：所有任务里紧跟在这一列最后一张后面的那张，没有时为空。
    fn after_column(&self, column: Column, cx: &Context<Self>) -> Option<String> {
        let data = store::data(cx);
        let last = columns(data, unix_now())[column_index(column)].last().map(|todo| todo.id.clone())?;
        let ix = data.todos.iter().position(|todo| todo.id == last)?;
        data.todos.get(ix + 1).map(|todo| todo.id.clone())
    }

    /// 辅助工具调先后：在同一列里和上一张（`up`）或下一张换位置。
    fn nudge_todo(&mut self, id: &str, up: bool, cx: &mut Context<Self>) {
        let data = store::data(cx);
        let Some(column) = columns(data, unix_now()).into_iter().find(|cards| cards.iter().any(|todo| todo.id == id))
        else {
            return;
        };
        let Some(ix) = column.iter().position(|todo| todo.id == id) else { return };
        let before = if up {
            let Some(previous) = ix.checked_sub(1).and_then(|ix| column.get(ix)) else { return };
            Some(previous.id.clone())
        } else {
            let Some(next) = column.get(ix + 1) else { return };
            // 挪到下一张后面：所有任务里紧跟在它后面的那张前面。
            let at = data.todos.iter().position(|todo| todo.id == next.id).map_or(data.todos.len(), |ix| ix + 1);
            data.todos.get(at).map(|todo| todo.id.clone())
        };
        let id = id.to_owned();
        Self::send_todos(vec![Box::new(move |req| ClientMsg::MoveTodo { req, id, before })], cx);
    }

    fn start_todo(&mut self, id: String, cx: &mut Context<Self>) {
        self.send_start(vec![Box::new(move |req| ClientMsg::StartTodo { req, id })], cx);
    }

    fn finish_todo(&mut self, id: String, done: bool, cx: &mut Context<Self>) {
        Self::send_todos(
            vec![Box::new(move |req| ClientMsg::UpdateTodo { req, id, spec: None, done: Some(done) })],
            cx,
        );
    }

    /// 删掉任务；它的会话还在时先问一句（会话留着，在后台照常跑）。
    fn remove_todo(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let data = store::data(cx);
        let Some(todo) = data.todos.iter().find(|todo| todo.id == id) else { return };
        let alive = todo.session.is_some_and(|session| data.sessions.get(&session).is_some_and(|info| !info.exited));
        if !alive {
            Self::send_todos(vec![Box::new(move |req| ClientMsg::RemoveTodo { req, id })], cx);
            return;
        }
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &rust_i18n::t!("todo_board.remove_title", title = todo.spec.title.as_str()),
            Some(&rust_i18n::t!("todo_board.remove_detail")),
            &[&*rust_i18n::t!("todo_board.remove"), &*rust_i18n::t!("todo_board.cancel")],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await.ok() == Some(0) {
                cx.update(|cx| store::send(cx, vec![Box::new(move |req| ClientMsg::RemoveTodo { req, id })]).detach());
            }
        })
        .detach();
    }

    fn remove_schedule(&mut self, id: String, cx: &mut Context<Self>) {
        Self::send_todos(vec![Box::new(move |req| ClientMsg::RemoveSchedule { req, id })], cx);
    }

    /// 切到任务会话：已经在哪个分屏里就切过去，否则接进它目录所在 workspace 的一个新标签（没有这样的
    /// workspace 时新开一个），看板随之收起。
    fn open_todo_session(&mut self, id: SessionId, todo: &Todo, window: &mut Window, cx: &mut Context<Self>) {
        let own = self
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| &tab.panes)
            .find_map(|(pane, (terminal, _))| (terminal.read(cx).session_id() == Some(id)).then_some(*pane));
        if let Some(pane) = own {
            self.todo_board = None;
            self.show_pane(pane, window, cx);
            return;
        }
        // 当前窗口正在更新，`find_session` 读不到它，找到的是别的窗口。
        if let Some((target, pane)) = remote::find_session(id, cx) {
            agents::reveal(target, pane, cx);
            return;
        }
        let cwd = todo.worktree_dir.as_deref().unwrap_or(&todo.spec.dir).to_owned();
        let view = match TerminalView::reattach(id, Some(&cwd), true, window, cx) {
            Ok(view) => view,
            Err(err) => {
                tracing::warn!("failed to open the session of todo {}: {err:#}", todo.id);
                return;
            }
        };
        self.todo_board = None;
        let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        let dirs: Vec<PathBuf> = self.workspaces.iter().map(|workspace| canonical(&workspace.dir)).collect();
        let dir_refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
        let home = super::model::home_dir().map(|home| canonical(&home));
        let worktree = todo.worktree_dir.as_deref().map(canonical);
        match best_workspace(&dir_refs, &canonical(&todo.spec.dir), worktree.as_deref(), home.as_deref()) {
            Some(ix) => {
                self.activate_workspace(ix, window, cx);
                self.insert_tab(self.workspace().active + 1, view, window, cx);
            }
            None => self.insert_workspace(self.active + 1, todo.spec.dir.clone(), None, view, window, cx),
        }
    }

    /// 侧栏顶上的入口，任务板开着时高亮。
    pub(super) fn render_todo_board_entry(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let label = rust_i18n::t!("todo_board.title").into_owned();
        let active = self.todo_board.is_some();
        super::mobile::page_entry("todo-board-entry", CHECK_ICON, label, active, fg, bg, cx, |this, window, cx| {
            this.show_todo_board(window, cx);
        })
    }

    /// 任务板开着时窗口的内容，摆法和手机端引导页一样。
    pub(super) fn render_todo_body(
        &mut self,
        fg: Rgb,
        bg: Rgb,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        self.track_moves(cx);
        let a11y = window.is_a11y_active();
        let page = self.render_todo_board(fg, bg, a11y, cx).into_any_element();
        self.render_page_body(page, fg, bg, window, cx)
    }

    /// 和上一帧比，记下换了列的卡片，从上一帧画的位置起飞；飞完、描边褪完的清掉。
    fn track_moves(&mut self, cx: &Context<Self>) {
        let Some(board) = &mut self.todo_board else { return };
        if board.page != Page::Todos {
            return;
        }
        let current: HashMap<String, Column> = Column::ALL
            .into_iter()
            .zip(columns(store::data(cx), unix_now()))
            .flat_map(|(column, cards)| cards.into_iter().map(move |todo| (todo.id.clone(), column)))
            .collect();
        board.moves.retain(|id, moving| moving.at.elapsed() < FLY + LAND && current.contains_key(id));
        let mut geometry = board.geometry.borrow_mut();
        geometry.cards.retain(|id, _| current.contains_key(id));
        if let Some(seen) = &board.seen {
            for (id, column) in &current {
                if seen.get(id).is_none_or(|old| old == column) {
                    continue;
                }
                let dropped = board.dropped.as_ref() == Some(id);
                if dropped {
                    board.dropped = None;
                }
                board.serial += 1;
                let from = if dropped { None } else { geometry.cards.get(id).copied() };
                board.moves.insert(id.clone(), Move { from, serial: board.serial, at: Instant::now() });
            }
        }
        drop(geometry);
        board.seen = Some(current);
    }

    fn render_todo_board(&self, fg: Rgb, bg: Rgb, a11y: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let board = self.todo_board.as_ref().expect("the todo board is open");
        let data = store::data(cx).clone();
        let data = &data;
        let title = rust_i18n::t!("todo_board.title").into_owned();
        let tab = |id: &'static str, page: Page, label: String, cx: &mut Context<Self>| {
            let selected = board.page == page;
            div()
                .id(id)
                .role(Role::Tab)
                .aria_label(label.clone())
                .aria_selected(selected)
                .px(px(12.))
                .py(px(4.))
                .rounded(px(6.))
                .cursor_pointer()
                .when(selected, |tab| tab.bg(hsla(bg.mix(fg, 0.12))).font_weight(FontWeight::SEMIBOLD))
                .when(!selected, |tab| {
                    tab.text_color(hsla(fg).opacity(0.65)).hover(|tab| tab.bg(hsla(bg.mix(fg, 0.06))))
                })
                .child(label)
                .on_press(cx, move |this, _, cx| this.set_todo_page(page, cx))
        };
        let schedules_label = rust_i18n::t!("todo_board.schedules").into_owned();
        let tabs = div()
            .id("todo-board-pages")
            .role(Role::TabList)
            .aria_label(title.clone())
            .flex()
            .gap(px(4.))
            .child(tab("todo-board-todos", Page::Todos, rust_i18n::t!("todo_board.todos").into_owned(), cx))
            .child(tab("todo-board-schedules", Page::Schedules, schedules_label, cx));
        let (new_id, new_label) = match board.page {
            Page::Todos => ("todo-board-new", rust_i18n::t!("todo_board.new_todo").into_owned()),
            Page::Schedules => ("todo-board-new-schedule", rust_i18n::t!("todo_board.new_schedule").into_owned()),
        };
        let page = board.page;
        let new = div()
            .id(new_id)
            .role(Role::Button)
            .aria_label(new_label.clone())
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(10.))
            .py(px(4.))
            .rounded(px(6.))
            .bg(hsla(bg.mix(fg, 0.12)))
            .hover(|button| button.bg(hsla(bg.mix(fg, 0.18))))
            .cursor_pointer()
            .child(svg().path(PLUS_ICON).size(px(12.)).text_color(hsla(fg)))
            .child(new_label)
            .on_press(cx, move |this, window, cx| this.open_todo_dialog(page == Page::Schedules, None, window, cx));
        let close_label = rust_i18n::t!("todo_board.close").into_owned();
        let close = icon_button("todo-board-close", CLOSE_ICON, close_label, fg, bg)
            .on_press(cx, |this, window, cx| this.close_todo_board(window, cx));
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .id("todo-board-title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(title.clone())
                    .text_size(FontSize::Title)
                    .font_weight(FontWeight::BOLD)
                    .child(title.clone()),
            )
            .child(tabs)
            .child(div().flex_1())
            .child(new)
            .child(close);
        let error = data.error.clone().map(|error| label_row("todo-board-error", error, hsla(fg).opacity(0.75)));
        let (content, flights) = match board.page {
            Page::Todos => (
                self.render_columns(board, data, fg, bg, a11y, cx).into_any_element(),
                self.render_flights(board, data, fg, bg, cx),
            ),
            Page::Schedules => (self.render_schedules(board, data, fg, bg, cx).into_any_element(), Vec::new()),
        };
        let geometry = board.geometry.clone();
        div()
            .id("todo-board")
            .role(Role::Group)
            .aria_label(title)
            .track_focus(&board.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    this.close_todo_board(window, cx);
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(16.))
            .text_size(FontSize::Body)
            .text_color(hsla(fg))
            .child(
                // 飞着的卡片按看板的左上角摆，这里记下看板画在哪。
                canvas(move |bounds, _, _| geometry.borrow_mut().board = bounds, |_, (), _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .child(header)
            .children(error)
            .child(div().flex_1().min_h_0().child(content))
            .children(flights)
            .children(board.toast.as_ref().map(|toast| self.render_toast(toast, fg, bg, cx)))
    }

    /// 正在换列的卡片：从上一帧的位置飞到这一列里它现在画的位置，盖在各列上面。
    fn render_flights(
        &self,
        board: &TodoBoard,
        data: &TodoData,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let now = unix_now();
        let geometry = board.geometry.borrow();
        let origin = geometry.board.origin;
        let mut flights = Vec::new();
        for (id, moving) in &board.moves {
            let Some(from) = moving.from.filter(|_| moving.at.elapsed() < FLY) else { continue };
            let Some(todo) = data.todos.iter().find(|todo| &todo.id == id) else { continue };
            // 新位置要等这一列排过一次版才知道，头一帧先停在原处。
            let to = geometry.cards.get(id).copied().unwrap_or(from);
            let (from, to) = (from.origin - origin, Bounds { origin: to.origin - origin, size: to.size });
            let width = (moving.from.map_or(to.size.width, |from| from.size.width), to.size.width);
            let face = self
                .card_face(todo, data, now, named("todo-flight-agent", id), None, None, fg, bg, cx)
                .absolute()
                .shadow(card_shadow(None, true));
            let ease = ease_out_quint();
            flights.push(
                face.with_animation(
                    ("todo-flight", moving.serial as usize),
                    Animation::new(FLY),
                    move |face, delta| {
                        let eased = ease(delta);
                        let lerp = |a: Pixels, b: Pixels| a + (b - a) * eased;
                        // 最后一段和新位置上的卡片交叉淡入淡出，不闪。
                        let opacity = ((1. - delta) / (1. - CROSSFADE)).min(1.);
                        face.left(lerp(from.x, to.origin.x))
                            .top(lerp(from.y, to.origin.y))
                            .w(lerp(width.0, width.1))
                            .opacity(opacity)
                    },
                )
                .into_any_element(),
            );
        }
        flights
    }

    /// 顶上居中的提示条：开始任务时转着圈，宿主回话后说开始了没有。
    fn render_toast(&self, toast: &Toast, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> AnyElement {
        let text = match toast.state {
            ToastState::Starting => rust_i18n::t!("todo_board.toast.starting"),
            ToastState::Started => rust_i18n::t!("todo_board.toast.started"),
            ToastState::Failed => rust_i18n::t!("todo_board.toast.failed"),
        }
        .into_owned();
        // 前景色作底、背景色写字，和后面的看板反着，深色浅色主题下都分得开。
        let (toast_bg, toast_fg) = (fg.mix(bg, 0.15), bg);
        let icon = match toast.state {
            ToastState::Starting => {
                let (frames, frame_time) = TOAST_SPINNER;
                div().child(spinner("todo-toast-spinner", frames, frame_time)).into_any_element()
            }
            ToastState::Started => {
                svg().path(CHECK_ICON).size(px(12.)).text_color(gpui::rgb(AGENT_DONE_COLOR)).into_any_element()
            }
            ToastState::Failed => {
                svg().path(CLOSE_ICON).size(px(12.)).text_color(gpui::rgb(AGENT_BLOCKED_COLOR)).into_any_element()
            }
        };
        let serial = toast.serial;
        let close_label = rust_i18n::t!("todo_board.toast.close").into_owned();
        let close = div()
            .id("todo-toast-close")
            .role(Role::Button)
            .aria_label(close_label)
            .flex_none()
            .size(px(18.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(move |button| button.bg(hsla(toast_fg).opacity(0.15)))
            .child(svg().path(CLOSE_ICON).size(px(10.)).text_color(hsla(toast_fg).opacity(0.7)))
            .on_press(cx, move |this, _, cx| this.set_toast(serial, None, cx));
        let pill = div()
            .id("todo-toast")
            .role(Role::Label)
            .aria_label(text.clone())
            .flex()
            .items_center()
            .gap(px(8.))
            .h(scaled(32.))
            .pl(px(12.))
            .pr(px(8.))
            .rounded(px(10.))
            .bg(hsla(toast_bg))
            .text_color(hsla(toast_fg))
            .shadow(card_shadow(None, true))
            .child(div().flex_none().w(px(14.)).flex().justify_center().child(icon))
            .child(div().min_w_0().truncate().child(text))
            .child(close);
        div()
            .absolute()
            .top(px(10.))
            .left_0()
            .w_full()
            .flex()
            .justify_center()
            .child(pill)
            .with_animation(
                ("todo-toast", serial as usize),
                Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                |toast, delta| toast.opacity(delta).mt(px(-8. * (1. - delta))),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_columns(
        &self,
        board: &TodoBoard,
        data: &TodoData,
        fg: Rgb,
        bg: Rgb,
        a11y: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let now = unix_now();
        let grouped = columns(data, now);
        let columns: Vec<_> = Column::ALL
            .iter()
            .zip(&grouped)
            .map(|(column, cards)| self.render_column(board, *column, cards, data, now, fg, bg, a11y, cx))
            .collect();
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("todo-columns")
                    .size_full()
                    .overflow_x_scroll()
                    .track_scroll(&board.columns_scroll)
                    .flex()
                    .gap(px(12.))
                    .children(columns),
            )
            .child(scrollbar("todo-columns-scrollbar", board.columns_scroll.clone(), Axis::Horizontal, hsla(fg)))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_column(
        &self,
        board: &TodoBoard,
        column: Column,
        cards: &[&Todo],
        data: &TodoData,
        now: u64,
        fg: Rgb,
        bg: Rgb,
        a11y: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let fg_h = hsla(fg);
        let name = column_name(column);
        let count = rust_i18n::t!("todo_board.count", count = cards.len()).into_owned();
        let rows: Vec<_> = cards
            .iter()
            .enumerate()
            .map(|(ix, todo)| self.render_card(board, todo, column, ix, data, now, fg, bg, a11y, cx))
            .collect();
        let empty = cards.is_empty().then(|| {
            div().flex_1().flex().items_center().justify_center().child(label_row(
                ("todo-column-empty", column_index(column)),
                rust_i18n::t!("todo_board.empty").into_owned(),
                fg_h.opacity(0.4),
            ))
        });
        // 需要你那一列有卡片时计数是个实心的徽标，一眼看得到。
        let badge = column == Column::NeedsYou && !cards.is_empty();
        let tally = div()
            .flex_none()
            .text_size(FontSize::Tiny)
            .when(badge, |tally| {
                tally
                    .min_w(scaled(16.))
                    .h(scaled(16.))
                    .px(px(4.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgb(ACCENT))
                    .text_color(gpui::white())
                    .font_weight(FontWeight::SEMIBOLD)
            })
            .when(!badge, |tally| tally.text_color(fg_h.opacity(0.5)))
            .child(cards.len().to_string());
        let window_hint = (column == Column::Done).then(|| {
            div()
                .text_size(FontSize::Small)
                .text_color(fg_h.opacity(0.4))
                .font_weight(FontWeight::NORMAL)
                .child(rust_i18n::t!("todo_board.done_window").into_owned())
        });
        let accept_bg = hsla(bg.mix(fg, 0.1));
        div()
            .id(("todo-column", column_index(column)))
            .role(Role::List)
            .aria_label(name.clone())
            .aria_description(count)
            .flex_1()
            .min_w(scaled(COLUMN_MIN_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .rounded(px(10.))
            .bg(hsla(bg.mix(fg, 0.05)))
            .child(
                div()
                    .flex_none()
                    .h(scaled(36.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(div().flex_none().size(px(7.)).rounded_full().bg(column_color(column, fg)))
                    .child(name)
                    .child(tally)
                    .children(window_hint),
            )
            .child(
                div()
                    .id(("todo-column-body", column_index(column)))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.))
                    // 顶上留出落下时描边的宽度，不然第一张卡片的描边上沿被滚动区裁掉；裁剪按整像素取，
                    // 正好留描边那么宽仍会切掉一截，所以多留一点。
                    .pt(px(RING_WIDTH * 2.))
                    .pb(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .rounded(px(10.))
                    .drag_over::<DraggedTodo>(move |style, dragged, _, _| {
                        if dragged.drop_into(column).is_some() { style.bg(accept_bg) } else { style }
                    })
                    .on_drop(
                        cx.listener(move |this, dragged: &DraggedTodo, _, cx| {
                            this.drop_todo(dragged, column, None, cx)
                        }),
                    )
                    .children(rows)
                    .children(empty),
            )
    }

    /// 卡片的样子，看板上的和飞着的共用：第一行是目录，`corner` 和编号摆在右上角；有分支时下面一行是
    /// 分支；再下面是标题；最后一行是 agent、开始的时间和来自哪个定时，`primary` 摆在右下角。`agent_id`
    /// 给 agent 标记用（它转圈要 id）。
    #[allow(clippy::too_many_arguments)]
    fn card_face(
        &self,
        todo: &Todo,
        data: &TodoData,
        now: u64,
        agent_id: ElementId,
        corner: Option<Div>,
        primary: Option<Stateful<Div>>,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Div {
        let fg_h = hsla(fg);
        let session = todo.session.and_then(|id| data.sessions.get(&id));
        let mark = session.and_then(|info| info.meta.agent).map(|agent| Mark::new(agent, false));
        let place = self.place_name(&todo.spec.dir);
        let schedule = todo.schedule.as_ref().and_then(|id| data.schedules.iter().find(|s| &s.id == id));
        let footer: Vec<String> = [
            mark.is_none().then(|| todo.spec.agent.clone()),
            todo.started.map(|at| short_time(at, now)),
            schedule.map(|s| rust_i18n::t!("todo_board.from_schedule", name = s.spec.title.as_str()).into_owned()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let muted = fg_h.opacity(0.55);
        let top = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .h(scaled(20.))
            .text_size(FontSize::Small)
            .text_color(muted)
            .child(place_badge(&place))
            .child(div().min_w_0().truncate().child(place))
            .child(div().flex_1())
            .children(corner)
            // 旧宿主没给编号（0）时不写。
            .when(todo.number > 0, |row| row.child(div().flex_none().child(format!("#{}", todo.number))));
        // 分支单独一行，和目录挤在一行时窄列里两个都截得认不出。
        let branch = todo.branch.clone().map(|branch| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .min_w_0()
                .text_size(FontSize::Small)
                .text_color(muted)
                .child(svg().path(BRANCH_ICON).flex_none().size(px(11.)).text_color(fg_h.opacity(0.45)))
                .child(div().min_w_0().truncate().child(branch))
        });
        let bottom = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .min_h(scaled(22.))
            .when_some(mark, |row, mark| row.child(styled_agent_mark(mark, agent_id, fg_h, super::cards(cx))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(FontSize::Small)
                    .text_color(muted)
                    .child(footer.join(" · ")),
            )
            .children(primary);
        div()
            .flex_none()
            .px(px(12.))
            .py(px(10.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .rounded(px(8.))
            .bg(hsla(bg))
            .border_1()
            .border_color(fg_h.opacity(0.08))
            .shadow(card_shadow(None, false))
            .child(top)
            .children(branch)
            .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(todo.spec.title.clone()))
            .child(bottom)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_card(
        &self,
        board: &TodoBoard,
        todo: &Todo,
        column: Column,
        ix: usize,
        data: &TodoData,
        now: u64,
        fg: Rgb,
        bg: Rgb,
        a11y: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fg_h = hsla(fg);
        let session = todo.session.and_then(|id| data.sessions.get(&id));
        let agent = session.and_then(|info| info.meta.agent);
        let mark = agent.map(|agent| Mark::new(agent, false));
        let place = self.place_name(&todo.spec.dir);
        let schedule = todo.schedule.as_ref().and_then(|id| data.schedules.iter().find(|s| &s.id == id));
        let status = mark.map_or_else(|| todo.spec.agent.clone(), |mark| mark.describe());
        let started =
            todo.started.map(|at| rust_i18n::t!("todo_board.started_at", time = short_time(at, now)).into_owned());
        let from =
            schedule.map(|s| rust_i18n::t!("todo_board.from_schedule", name = s.spec.title.as_str()).into_owned());
        let line2: Vec<String> = [Some(place), todo.branch.clone()].into_iter().flatten().collect();
        let line3: Vec<String> = [Some(status), started, from].into_iter().flatten().collect();
        let number = if todo.number > 0 { format!("#{}", todo.number) } else { String::new() };
        let description =
            [number, line2.join(" · "), line3.join(" · "), rust_i18n::t!("todo_board.reorder_hint").into_owned()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("。");
        let drops: [Option<DropAction>; 4] = Column::ALL.map(|to| drop_action(todo, session, to, now));
        let dragged = DraggedTodo {
            id: todo.id.clone(),
            from: column,
            ix,
            drops,
            title: todo.spec.title.clone().into(),
            fg: fg_h,
            bg: hsla(bg.mix(fg, 0.06)),
        };
        let id = todo.id.clone();
        let open_session = todo.session.filter(|_| session.is_some());
        // 这一列最该做的事是右下角的主按钮，别的收在右上角，鼠标移到卡片上（或者辅助工具开着）才露出来。
        let primary = if todo.started.is_none() && todo.done_at.is_none() {
            let id = id.clone();
            Some(
                primary_button("todo-start", rust_i18n::t!("todo_board.start").into_owned())
                    .on_press(cx, move |this, _, cx| this.start_todo(id.clone(), cx)),
            )
        } else if column == Column::NeedsYou {
            Some(match open_session {
                Some(session_id) => {
                    let todo = todo.clone();
                    primary_button("todo-open", rust_i18n::t!("todo_board.open").into_owned())
                        .on_press(cx, move |this, window, cx| this.open_todo_session(session_id, &todo, window, cx))
                }
                None => {
                    let id = id.clone();
                    primary_button("todo-done", rust_i18n::t!("todo_board.finish").into_owned())
                        .on_press(cx, move |this, _, cx| this.finish_todo(id.clone(), true, cx))
                }
            })
        } else {
            None
        };
        let primary_id = primary.as_ref().map(|_| match column {
            Column::NeedsYou if open_session.is_some() => "open",
            Column::NeedsYou => "done",
            _ => "start",
        });
        let shown = a11y || board.hovered.as_ref() == Some(&todo.id);
        let mut buttons: Vec<Stateful<Div>> = Vec::new();
        if shown {
            if let Some(session_id) = open_session.filter(|_| primary_id != Some("open")) {
                let todo = todo.clone();
                buttons.push(
                    icon_button("todo-open", TERMINAL_ICON, rust_i18n::t!("todo_board.open").into_owned(), fg, bg)
                        .on_press(cx, move |this, window, cx| this.open_todo_session(session_id, &todo, window, cx)),
                );
            }
            if primary_id != Some("done") {
                let done = todo.done_at.is_none();
                let label = if done { rust_i18n::t!("todo_board.finish") } else { rust_i18n::t!("todo_board.reopen") };
                let id = id.clone();
                buttons.push(
                    icon_button("todo-done", CHECK_ICON, label.into_owned(), fg, bg)
                        .on_press(cx, move |this, _, cx| this.finish_todo(id.clone(), done, cx)),
                );
            }
            {
                let todo = todo.clone();
                buttons.push(
                    icon_button("todo-edit", PENCIL_ICON, rust_i18n::t!("todo_board.edit").into_owned(), fg, bg)
                        .on_press(cx, move |this, window, cx| {
                            this.open_todo_dialog(false, Some(dialog::Editing::Todo(todo.clone())), window, cx)
                        }),
                );
            }
            {
                let id = id.clone();
                buttons.push(
                    icon_button(
                        "todo-remove",
                        TRASH_ICON,
                        rust_i18n::t!("todo_board.remove_todo").into_owned(),
                        fg,
                        bg,
                    )
                    .on_press(cx, move |this, window, cx| this.remove_todo(id.clone(), window, cx)),
                );
            }
        }
        let corner = (!buttons.is_empty()).then(|| div().flex_none().flex().gap(px(2.)).my(px(-4.)).children(buttons));
        let face = self.card_face(todo, data, now, named("todo-agent", &todo.id), corner, primary, fg, bg, cx);
        let geometry = board.geometry.clone();
        let (up_id, down_id, hover_id, bounds_id) = (id.clone(), id.clone(), id.clone(), id);
        let card = face
            .id(named("todo-card", &todo.id))
            .role(Role::ListItem)
            .aria_label(todo.spec.title.clone())
            .aria_description(description)
            .on_a11y_action(AccessibleAction::Decrement, {
                let view = cx.entity().downgrade();
                move |_, _, cx| {
                    view.update(cx, |this, cx| this.nudge_todo(&up_id, true, cx)).ok();
                }
            })
            .on_a11y_action(AccessibleAction::Increment, {
                let view = cx.entity().downgrade();
                move |_, _, cx| {
                    view.update(cx, |this, cx| this.nudge_todo(&down_id, false, cx)).ok();
                }
            })
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let Some(board) = &mut this.todo_board else { return };
                let now = if *hovered { Some(hover_id.clone()) } else { None };
                if *hovered || board.hovered.as_ref() == Some(&hover_id) {
                    board.hovered = now;
                    cx.notify();
                }
            }))
            .relative()
            .cursor_grab()
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedTodo>(move |style, dragged, _, _| {
                if dragged.drop_into(column).is_none() {
                    return style;
                }
                // 跨列放下时插在这张前面，提示画在上边。
                let from = if dragged.from == column { dragged.ix } else { usize::MAX };
                drop_marker(style, from, ix, Axis::Vertical, fg_h)
            })
            .on_drop(cx.listener({
                let before = todo.id.clone();
                move |this, dragged: &DraggedTodo, _, cx| this.drop_todo(dragged, column, Some(before.clone()), cx)
            }))
            .child(
                // 记下这张卡片画在哪，换列时从这里起飞。
                canvas(move |bounds, _, _| _ = geometry.borrow_mut().cards.insert(bounds_id, bounds), |_, (), _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            );
        let Some(moving) = board.moves.get(&todo.id) else { return card.into_any_element() };
        // 飞着的时候这里只留个影子，飞的卡片快到时和它交叉淡入，落下后描边停一会儿再褪掉。拖着放下的不飞，
        // 直接描边。
        let flew = moving.from.is_some();
        let duration = if flew { FLY + LAND } else { LAND };
        let landing_from = if flew { FLY.as_secs_f32() / duration.as_secs_f32() } else { 0. };
        card.with_animation(("todo-land", moving.serial as usize), Animation::new(duration), move |card, delta| {
            if delta < landing_from {
                let fading = ((delta / landing_from - CROSSFADE) / (1. - CROSSFADE)).max(0.);
                return card.opacity(GHOST_OPACITY + (1. - GHOST_OPACITY) * fading);
            }
            let landed = (delta - landing_from) / (1. - landing_from);
            let ring = if landed < 0.5 { 1. } else { 2. * (1. - landed) };
            card.shadow(card_shadow(Some(ring), false))
        })
        .into_any_element()
    }

    /// 目录给人看的名字：正好是哪个 workspace 的目录时是它的名字，否则是目录名。
    fn place_name(&self, dir: &Path) -> String {
        self.workspaces
            .iter()
            .find(|workspace| workspace.dir == dir)
            .map_or_else(|| base_name(dir), |workspace| workspace.name.to_string())
    }

    fn render_schedules(&self, board: &TodoBoard, data: &TodoData, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let fg_h = hsla(fg);
        let now = unix_now();
        let config = &cx.global::<AppConfig>().0;
        let resident = config.terminal_host
            && runode_autostart::is_installed(runode_autostart::Kind::Host, &runode_paths::Dirs::from_env());
        let hint = (!resident).then(|| {
            label_row(
                "todo-schedules-hint",
                rust_i18n::t!("todo_board.schedules_need_host").into_owned(),
                fg_h.opacity(0.6),
            )
        });
        let rows: Vec<_> =
            data.schedules.iter().map(|schedule| self.render_schedule(schedule, data, now, fg, bg, cx)).collect();
        let empty = rows.is_empty().then(|| {
            label_row("todo-schedules-empty", rust_i18n::t!("todo_board.no_schedules").into_owned(), fg_h.opacity(0.45))
        });
        div().size_full().flex().flex_col().gap(px(8.)).children(hint).child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("todo-schedules")
                        .role(Role::List)
                        .aria_label(rust_i18n::t!("todo_board.schedules").into_owned())
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&board.schedules_scroll)
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .children(rows)
                        .children(empty),
                )
                .child(scrollbar("todo-schedules-scrollbar", board.schedules_scroll.clone(), Axis::Vertical, fg_h)),
        )
    }

    fn render_schedule(
        &self,
        schedule: &Schedule,
        data: &TodoData,
        now: u64,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let fg_h = hsla(fg);
        let rule = describe_when(&schedule.when);
        let next = schedule.next_fire.map_or_else(
            || rust_i18n::t!("todo_board.no_next").into_owned(),
            |at| rust_i18n::t!("todo_board.next", time = short_time(at, now)).into_owned(),
        );
        let place = self.place_name(&schedule.spec.dir);
        let worktree = schedule.spec.worktree.then(|| rust_i18n::t!("todo_board.worktree_on").into_owned());
        let details: Vec<String> =
            [Some(place), Some(schedule.spec.agent.clone()), worktree].into_iter().flatten().collect();
        let runs: Vec<&Todo> = data
            .todos
            .iter()
            .filter(|todo| todo.schedule.as_ref() == Some(&schedule.id))
            .rev()
            .take(RECENT_RUNS)
            .collect();
        let run_rows: Vec<_> = runs
            .iter()
            .map(|todo| {
                let session = todo.session.and_then(|id| data.sessions.get(&id));
                let column = column(todo, session, now);
                let when = todo.started.map(|at| short_time(at, now)).unwrap_or_default();
                let label = format!("{} · {}", column_name(column), when);
                let todo = (*todo).clone();
                let row = div()
                    .id(named("todo-schedule-run", &todo.id))
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .text_size(FontSize::Small)
                    .text_color(fg_h.opacity(0.7))
                    .hover(|row| row.bg(hsla(bg.mix(fg, 0.06))))
                    .cursor_pointer()
                    .child(label);
                match todo.session.filter(|_| session.is_some()) {
                    Some(session) => {
                        row.on_press(cx, move |this, window, cx| this.open_todo_session(session, &todo, window, cx))
                    }
                    None => row.on_press(cx, |this, _, cx| this.set_todo_page(Page::Todos, cx)),
                }
            })
            .collect();
        let edit = {
            let schedule = schedule.clone();
            icon_button(
                "todo-schedule-edit",
                PENCIL_ICON,
                rust_i18n::t!("todo_board.edit_schedule").into_owned(),
                fg,
                bg,
            )
            .on_press(cx, move |this, window, cx| {
                this.open_todo_dialog(true, Some(dialog::Editing::Schedule(schedule.clone())), window, cx)
            })
        };
        let remove = {
            let id = schedule.id.clone();
            icon_button(
                "todo-schedule-remove",
                TRASH_ICON,
                rust_i18n::t!("todo_board.remove_schedule").into_owned(),
                fg,
                bg,
            )
            .on_press(cx, move |this, _, cx| this.remove_schedule(id.clone(), cx))
        };
        div()
            .id(named("todo-schedule", &schedule.id))
            .role(Role::ListItem)
            .aria_label(schedule.spec.title.clone())
            .aria_description(format!("{rule}。{next}。{}", details.join(" · ")))
            .flex_none()
            .p(px(12.))
            .flex()
            .gap(px(12.))
            .rounded(px(8.))
            .bg(hsla(bg.mix(fg, 0.03)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).child(schedule.spec.title.clone()))
                    .child(
                        div()
                            .text_size(FontSize::Small)
                            .text_color(fg_h.opacity(0.75))
                            .child(format!("{rule} · {next}")),
                    )
                    .child(
                        div()
                            .text_size(FontSize::Small)
                            .text_color(fg_h.opacity(0.55))
                            .truncate()
                            .child(details.join(" · ")),
                    )
                    .when(!run_rows.is_empty(), |info| {
                        info.child(div().pt(px(4.)).flex().flex_wrap().gap(px(4.)).children(run_rows))
                    }),
            )
            .child(div().flex_none().flex().items_start().gap(px(2.)).child(edit).child(remove))
    }
}

/// 只有图标的小按钮，名字（辅助工具读的、悬停提示）写它做什么。
fn icon_button(id: &'static str, icon: &'static str, label: String, fg: Rgb, bg: Rgb) -> Stateful<Div> {
    let fg_h = hsla(fg);
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .flex_none()
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .cursor_pointer()
        .hover(move |button| button.bg(hsla(bg.mix(fg, 0.08))))
        .tooltip(tooltip(label, None, fg, bg))
        .child(svg().path(icon).size(px(13.)).text_color(fg_h.opacity(0.7)))
}

/// 卡片右下角的主按钮：强调色的底，写它做什么。
fn primary_button(id: &'static str, label: String) -> Stateful<Div> {
    let accent = gpui::rgb(ACCENT);
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .flex_none()
        .h(scaled(22.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .bg(accent)
        .hover(move |button| button.bg(Hsla::from(accent).opacity(0.85)))
        .cursor_pointer()
        .text_size(FontSize::Small)
        .font_weight(FontWeight::MEDIUM)
        .text_color(gpui::white())
        .child(label)
}

/// 卡片的阴影：平时浅浅一层，`lifted`（飞着、提示条）时浮起来；`ring` 是落下后描的那圈强调色的浓淡。
fn card_shadow(ring: Option<f32>, lifted: bool) -> Vec<BoxShadow> {
    let (alpha, offset, blur) = if lifted { (0.18, 6., 18.) } else { (0.05, 1., 2.) };
    let mut shadows = vec![BoxShadow {
        color: Hsla::black().opacity(alpha),
        offset: point(px(0.), px(offset)),
        blur_radius: px(blur),
        spread_radius: px(0.),
        inset: false,
    }];
    shadows.extend(ring.filter(|ring| *ring > 0.).map(|ring| BoxShadow {
        color: Hsla::from(gpui::rgb(ACCENT)).opacity(ring),
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(RING_WIDTH),
        inset: false,
    }));
    shadows
}

/// 目录名首字母的小方块，颜色按名字取，同一个目录的卡片颜色一样。
fn place_badge(place: &str) -> Div {
    const COLORS: [u32; 6] = [0x10B981, 0x3B82F6, 0x8B5CF6, 0xF59E0B, 0xEC4899, 0x06B6D4];
    let initial = place.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    let hash = place.bytes().fold(0usize, |hash, byte| hash.wrapping_mul(31).wrapping_add(byte.into()));
    div()
        .flex_none()
        .size(scaled(16.))
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::rgb(COLORS[hash % COLORS.len()]))
        .text_size(FontSize::Tiny)
        .font_weight(FontWeight::BOLD)
        .text_color(gpui::white())
        .child(initial)
}

/// 列头圆点的颜色。
fn column_color(column: Column, fg: Rgb) -> Hsla {
    match column {
        Column::Todo => hsla(fg).opacity(0.35),
        Column::InProgress => gpui::rgb(IN_PROGRESS_COLOR).into(),
        Column::NeedsYou => gpui::rgb(AGENT_BLOCKED_COLOR).into(),
        Column::Done => gpui::rgb(AGENT_DONE_COLOR).into(),
    }
}

/// 只起提示作用的一行字，报给辅助工具。
fn label_row(id: impl Into<gpui::ElementId>, text: String, color: Hsla) -> Stateful<Div> {
    div().id(id).role(Role::Label).aria_label(text.clone()).text_size(FontSize::Small).text_color(color).child(text)
}

fn column_name(column: Column) -> String {
    match column {
        Column::Todo => rust_i18n::t!("todo_board.column.todo"),
        Column::InProgress => rust_i18n::t!("todo_board.column.in_progress"),
        Column::NeedsYou => rust_i18n::t!("todo_board.column.needs_you"),
        Column::Done => rust_i18n::t!("todo_board.column.done"),
    }
    .into_owned()
}

/// 定时的规则给人看的写法：「每天 09:30 · 周一、周三」「每 15 分钟」「一次 · 10-13 14:00」。
fn describe_when(when: &When) -> String {
    match *when {
        When::Once { at } => rust_i18n::t!("todo_board.when.once", time = dialog::full_time(at)).into_owned(),
        When::Daily { hour, minute, days } => {
            let time = format!("{hour:02}:{minute:02}");
            if days == 0 {
                rust_i18n::t!("todo_board.when.daily", time = time).into_owned()
            } else {
                let names: Vec<String> = (0..7)
                    .filter(|day| days & (1 << day) != 0)
                    .map(|day| crate::i18n::tr(&format!("todo_board.weekday.{day}")))
                    .collect();
                rust_i18n::t!("todo_board.when.weekly", time = time, days = names.join(" ")).into_owned()
            }
        }
        When::Every { minutes } if minutes % 60 == 0 => {
            rust_i18n::t!("todo_board.when.every_hours", count = minutes / 60).into_owned()
        }
        When::Every { minutes } => rust_i18n::t!("todo_board.when.every_minutes", count = minutes).into_owned(),
        When::Unknown => rust_i18n::t!("todo_board.when.unknown").into_owned(),
    }
}

/// 今天的写 `HH:MM`，别的日子写 `MM-DD HH:MM`，按本地时间。
fn short_time(at: u64, now: u64) -> String {
    let (Some(at), Some(today)) = (dialog::local(at), dialog::local(now)) else { return String::new() };
    if (at.year, at.month, at.day) == (today.year, today.month, today.day) {
        format!("{:02}:{:02}", at.hour, at.minute)
    } else {
        format!("{:02}-{:02} {:02}:{:02}", at.month, at.day, at.hour, at.minute)
    }
}

#[cfg(test)]
mod tests {
    use runode_protocol::todo::TodoSpec;

    use super::*;

    fn todo(id: &str, started: Option<u64>, done_at: Option<u64>) -> Todo {
        Todo {
            id: id.into(),
            number: 0,
            spec: TodoSpec {
                title: id.into(),
                prompt: String::new(),
                dir: "/src".into(),
                agent: "claude".into(),
                worktree: false,
            },
            created: 0,
            started,
            session: None,
            worktree_dir: None,
            branch: None,
            done_at,
            schedule: None,
            rank: 0,
        }
    }

    #[test]
    fn columns_keep_the_hosts_order() {
        let data = TodoData {
            todos: vec![
                todo("a", None, None),
                todo("b", Some(1), None),
                todo("c", None, Some(5)),
                todo("d", None, None),
            ],
            ..TodoData::default()
        };
        let ids = |cards: &Vec<&Todo>| cards.iter().map(|todo| todo.id.clone()).collect::<Vec<_>>();
        let grouped = columns(&data, 1_000);
        assert_eq!(grouped.each_ref().map(ids), [vec!["a", "d"], vec![], vec!["b"], vec!["c"]]);
    }

    #[test]
    fn picks_the_workspace_for_a_task_session() {
        let dirs =
            [Path::new("/Users/me"), Path::new("/src"), Path::new("/src/app"), Path::new("/src/app/.worktrees/fix")];
        let home = Some(Path::new("/Users/me"));
        // 工作树优先，其次正好是任务目录，再次最深的上级；家目录不算。
        assert_eq!(
            best_workspace(&dirs, Path::new("/src/app"), Some(Path::new("/src/app/.worktrees/fix")), home),
            Some(3)
        );
        assert_eq!(best_workspace(&dirs, Path::new("/src/app"), None, home), Some(2));
        assert_eq!(best_workspace(&dirs, Path::new("/src/app/web"), None, home), Some(2));
        assert_eq!(best_workspace(&dirs, Path::new("/Users/me/notes"), None, home), None);
        assert_eq!(best_workspace(&dirs, Path::new("/elsewhere"), None, home), None);
    }
}
