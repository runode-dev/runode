//! 铺满窗口的看板：派给 agent 的任务按列排开（待办、进行中、需要你、完成），和它平级的另一页列出
//! 定时。完成那一列只有最近七天的，再早的在记录页上翻（`History`），从完成那一列的列头和各个定时进去，
//! 后者只列那个定时开出的。数据由宿主持有，这边经 `store` 读写；新建、编辑任务和定时的对话框在 `dialog`。
//!
//! 卡片在哪一列由任务和它会话里 agent 的状态推出来（`runode_protocol::todo::column`），拖拽就是做那一列
//! 的动作（`drop_action`）：拖进进行中是开始，拖进完成是标完成，从完成拖出是取消完成，同一列里上下拖调
//! 先后；办不到的放下不接，卡片留在原处。辅助工具拖不了：动作另有按钮，调先后另登记了加减（上移、下移）。
//!
//! 看板是一个单独的视图（`Kanban`），app 打开时建、收起时丢掉，一个窗口最多一个。它和手机端引导页一样盖住
//! 标签和分屏，摆在哪由 app 管。和宿主说话经 app 启动时交进来的 `Host`（`watch`），数据全 app 一份。各
//! workspace 的目录和名字、新建时默认的目录、定时能不能在后台跑由 app 每次画之前经 `set_input` 交进来；卡片上
//! agent 的状态标记和标签、侧栏上的一样，由 app 画（`AgentMarks`）。按了关闭、Esc（`KanbanEvent::Close`），
//! 卡片的「打开终端」（`KanbanEvent::OpenSession`：把任务的会话接进窗口）交给 app 办。对话框盖住整个窗口，
//! 由 app 画在窗口最上层（`Kanban::dialog`）。
//!
//! 卡片换了列时从原来的位置飞到新的位置，落下后淡入、描一圈强调色再褪掉；拖着放下的不飞，只描边。
//! 每帧记下各卡片画在哪（`Geometry`），下一帧发现换了列就照上一帧的位置起飞。开始任务时顶上居中弹出
//! 提示条（`Toast`），等宿主回话后说开始了没有；别的请求没办成时也在这里说原因。

mod dialog;
mod store;

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::ui::{
    a11y::Press,
    badge::Badge,
    base_name,
    button::{Button, ButtonSize, ButtonVariant},
    font_size::{self, FontSize, scaled},
    icons::{ARROW_LEFT_ICON, BRANCH_ICON, CHECK_ICON, CLOSE_ICON, PENCIL_ICON, PLUS_ICON, TERMINAL_ICON, TRASH_ICON},
    scrollbar::scrollbar,
    spinner::spinner,
    tabs::{drag_chip, drop_marker},
    theme::{AGENT_BLOCKED_COLOR, AGENT_DONE_COLOR, ActiveTheme as _},
    tooltip::tooltip,
};
use gpui::{
    AccessibleAction, Animation, AnimationExt as _, AnyElement, App, Axis, Bounds, BoxShadow, Context, Div, ElementId,
    Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent, Pixels, Render, Role, ScrollHandle,
    SharedString, Stateful, Subscription, Window, canvas, div, ease_out_quint, point, prelude::*, px, svg,
};
use runode_protocol::{
    ClientMsg, HostMsg, SessionId,
    todo::{Column, DropAction, Schedule, Todo, When, column, drop_action},
};
use runode_types::agent::Agent;

pub use dialog::TodoDialog;
pub use store::{Host, Step, needs_you, todo_sessions, watch};

use dialog::{DialogEvent, Editing};
use store::TodoData;

/// 一列最窄这么宽（基准字号下的点数）；窗口放不下四列时横着滚。
const COLUMN_MIN_WIDTH: f32 = 240.;
/// 定时那一页里每个定时下面最多列它最近开出的几张任务。
const RECENT_RUNS: usize = 3;
/// 强调色：落下后的描边、需要你那一列的计数。
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

/// app 每次画看板之前交进来的东西，见 `Kanban::set_input`。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Input {
    /// 各 workspace 的目录和名字：卡片、定时的目录正好是哪个 workspace 的目录时写它的名字，否则写目录名。
    pub workspaces: Vec<(PathBuf, SharedString)>,
    /// 当前 workspace 的目录：新建任务、定时时先填它。
    pub dir: PathBuf,
    /// 定时在 app 退出后也照跑（打开了 `terminal-host`，宿主登录时自启）；否则定时那一页上说一句。
    pub resident: bool,
}

/// 卡片上 agent 的状态标记，和标签、侧栏上的一样由 app 画，见 `Kanban::new`。
#[derive(Clone, Copy)]
pub struct AgentMarks {
    /// 画 `agent` 的状态标记；`id` 给它转圈用。只在画的时候调。
    pub render: fn(Agent, ElementId, &App) -> AnyElement,
    /// 报给辅助工具的说法：agent 的名字和状态，标记本身只是图形。
    pub describe: fn(Agent) -> String,
}

/// 要 app 办的事。
pub enum KanbanEvent {
    /// 收起看板：按了关闭或 Esc。
    Close,
    /// 换了一页（侧栏的入口、定时的运行记录点回任务页），侧栏上的入口跟着换高亮。
    PageChanged,
    /// 切到任务 `todo` 的会话 `session`：已经在哪个分屏里就切过去，否则接进窗口，看板随之收起。
    OpenSession { session: SessionId, todo: Box<Todo> },
}

/// 开着的看板，见模块说明。
pub struct Kanban {
    focus: FocusHandle,
    page: Page,
    columns_scroll: ScrollHandle,
    schedules_scroll: ScrollHandle,
    history_scroll: ScrollHandle,
    history: History,
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
    input: Input,
    marks: AgentMarks,
    /// 开着的新建、编辑对话框。
    dialog: Option<(Entity<TodoDialog>, Subscription)>,
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

enum ToastState {
    Starting,
    Started,
    /// 没办成，带着要显示的话；不自己收起，等用户关掉或被新的顶掉。
    Failed(String),
}

/// 看板的几页：任务页、定时页侧栏上各有一个入口，记录页从完成那一列的列头和定时进去。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Todos,
    Schedules,
    History,
}

/// 记录页翻到的完成了的任务（`ClientMsg::ListDoneTodos`），只在这个看板里用，不放进全 app 的数据。
#[derive(Default)]
struct History {
    /// 只看这个定时开出的；为空时看全部。
    schedule: Option<String>,
    /// 完成得晚的在前。
    todos: Vec<Todo>,
    /// 宿主说后面还有。
    more: bool,
    /// 第一页回来了：没回来之前不说没有记录。
    loaded: bool,
    /// 有一问还没回来：这时不翻下一页，从头重问的还没回来时照旧的张数往后翻会重了、漏了。
    loading: bool,
    error: Option<String>,
    /// 第几次问，回来晚了的旧答案不要。
    serial: u64,
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
}

impl DraggedTodo {
    fn drop_into(&self, column: Column) -> Option<DropAction> {
        self.drops[column_index(column)]
    }
}

impl Render for DraggedTodo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scale = font_size::scale(cx);
        drag_chip(px(COLUMN_MIN_WIDTH * scale), px(32. * scale), self.title.clone(), cx.theme().mix(0.06), cx)
    }
}

/// 按显示的东西（任务、定时的标识）取的元素 id。
fn named(prefix: &str, id: &str) -> ElementId {
    ElementId::Name(format!("{prefix}-{id}").into())
}

fn column_index(column: Column) -> usize {
    Column::ALL.iter().position(|c| *c == column).unwrap_or(0)
}

/// 卡片 `dragged` 放在 `column` 列第 `ix` 张（`target`）上时挪到谁前面，交给 `Kanban::drop_todo`。同一列里
/// 往下拖时落点提示画在目标下边，挪到它后面：所有任务里紧跟在它后面的那张前面，没有时为空（排到这一列最后）；
/// 别的时候挪到它前面。
fn drop_before(dragged: &DraggedTodo, column: Column, ix: usize, target: &str, todos: &[Todo]) -> Option<String> {
    if dragged.from != column || dragged.ix >= ix {
        return Some(target.to_owned());
    }
    let at = todos.iter().position(|todo| todo.id == target)?;
    todos.get(at + 1).map(|todo| todo.id.clone())
}

/// 定时 `schedule` 最近开的 `RECENT_RUNS` 个任务，新的在前。按建的先后挑，不按看板上的先后：卡片拖动过就不一样了。
fn recent_runs<'a>(todos: &'a [Todo], schedule: &str) -> Vec<&'a Todo> {
    let mut runs: Vec<&Todo> = todos.iter().filter(|todo| todo.schedule.as_deref() == Some(schedule)).collect();
    runs.sort_by_key(|todo| std::cmp::Reverse(todo.created));
    runs.truncate(RECENT_RUNS);
    runs
}

/// 现在的 Unix 时间，秒。
fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

/// 需要你的计数：实心的徽标，一眼看得到。看板上那一列的列头和侧栏的看板入口都用。
pub fn needs_you_badge(count: usize) -> Badge {
    Badge::new(count.to_string()).bg(gpui::rgb(ACCENT)).text_color(gpui::white())
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

impl EventEmitter<KanbanEvent> for Kanban {}

impl Focusable for Kanban {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Kanban {
    /// 新开的看板，`schedules` 时在定时页上，否则在任务页上。卡片上 agent 的状态标记照 `marks` 画。
    pub fn new(marks: AgentMarks, schedules: bool, cx: &mut Context<Self>) -> Self {
        store::register(cx.weak_entity(), cx);
        Self {
            focus: cx.focus_handle(),
            page: if schedules { Page::Schedules } else { Page::Todos },
            columns_scroll: ScrollHandle::new(),
            schedules_scroll: ScrollHandle::new(),
            history_scroll: ScrollHandle::new(),
            history: History::default(),
            geometry: Rc::default(),
            seen: None,
            moves: HashMap::new(),
            dropped: None,
            hovered: None,
            toast: None,
            serial: 0,
            input: Input::default(),
            marks,
            dialog: None,
        }
    }

    /// 换上 workspace 的目录和名字、新建时的目录、定时能不能在后台跑，见 `Input`。只记下来，不重画：app 在画的
    /// 时候调，返回变了没有，变了由 app 等这一帧画完再通知看板重画（看板是缓存的视图）。
    pub fn set_input(&mut self, input: Input) -> bool {
        let changed = self.input != input;
        self.input = input;
        changed
    }

    /// 显示着的是定时那一页：app 只在这时去查定时能不能在后台跑（`Input::resident`）。
    pub fn showing_schedules(&self) -> bool {
        self.page == Page::Schedules
    }

    /// 开着的新建、编辑对话框。它盖住整个窗口，app 画在窗口的最上层，开着时把下面的界面藏起来不给辅助工具读。
    pub fn dialog(&self) -> Option<Entity<TodoDialog>> {
        self.dialog.as_ref().map(|(dialog, _)| dialog.clone())
    }

    /// 换到定时页（`schedules`）或任务页：app 从侧栏的入口打开时调。
    pub fn show_schedules(&mut self, schedules: bool, cx: &mut Context<Self>) {
        self.set_page(if schedules { Page::Schedules } else { Page::Todos }, cx);
    }

    /// 换到任务页，弹出新建任务的对话框：app 从侧栏的入口或快捷键打开时调。
    pub fn new_todo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_page(Page::Todos, cx);
        self.open_dialog(false, None, window, cx);
    }

    /// 换到记录页，只看定时 `schedule` 开出的（为空时看全部），从头翻。
    fn show_history(&mut self, schedule: Option<String>, cx: &mut Context<Self>) {
        self.history = History { schedule, serial: self.history.serial, ..History::default() };
        self.set_page(Page::History, cx);
        self.load_history(false, cx);
    }

    /// 看板变了、记录页上办成了事：开着记录页时从头重翻。
    /// shortcut: 翻过的几页丢掉、只重读第一页，翻得很深时改完会跳回顶上；嫌烦再按已翻的张数重读。
    pub(crate) fn reload_history(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::History {
            self.load_history(false, cx);
        }
    }

    /// 问宿主记录的下一页（`more`），或者从头问。
    fn load_history(&mut self, more: bool, cx: &mut Context<Self>) {
        if more && self.history.loading {
            return;
        }
        let offset = if more { u32::try_from(self.history.todos.len()).unwrap_or(u32::MAX) } else { 0 };
        let schedule = self.history.schedule.clone();
        self.history.serial += 1;
        self.history.loading = true;
        let serial = self.history.serial;
        let answer = store::ask(cx, Box::new(move |req| ClientMsg::ListDoneTodos { req, schedule, offset }));
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |this, cx| {
                let history = &mut this.history;
                if history.serial != serial {
                    return;
                }
                history.loading = false;
                match answer {
                    Ok(HostMsg::DoneTodos { todos, more: rest, .. }) => {
                        if !more {
                            history.todos.clear();
                        }
                        history.todos.extend(todos);
                        history.more = rest;
                        history.loaded = true;
                        history.error = None;
                    }
                    Ok(other) => history.error = Some(format!("unexpected answer from the host: {other:?}")),
                    Err(err) => history.error = Some(format!("{err:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page == page {
            return;
        }
        self.page = page;
        self.seen = None;
        cx.emit(KanbanEvent::PageChanged);
        cx.notify();
    }

    /// 打开新建（`editing` 为空）或编辑任务、定时的对话框。
    fn open_dialog(&mut self, schedule: bool, editing: Option<Editing>, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.input.dir.clone();
        let dialog = cx.new(|cx| TodoDialog::new(schedule, editing, dir, window, cx));
        let events = cx.subscribe_in(&dialog, window, |this, _, event: &DialogEvent, window, cx| {
            this.dialog = None;
            if let DialogEvent::Saved { start: Some(id) } = event {
                this.start_todo(id.clone(), cx);
            }
            // 焦点在对话框的输入框里，对话框一丢就没了着落，看板的 Esc 也收不到：关掉、存好了都交回看板。
            window.focus(&this.focus, cx);
            cx.notify();
        });
        self.dialog = Some((dialog, events));
        cx.notify();
    }

    /// 依次发几条看板的请求，前一条没办成就不发后面的。办成了重翻记录：完成了七天以上的任务改了，看板的数据不变，
    /// 不会因此重翻。
    fn send_todos(steps: Vec<Step>, cx: &mut Context<Self>) {
        let sent = store::send(cx, steps);
        cx.spawn(async move |this, cx| {
            let sent = sent.await;
            this.update(cx, |this, cx| match sent {
                Ok(()) => this.reload_history(cx),
                Err(reason) => {
                    let text = rust_i18n::t!("kanban.toast.request_failed", reason = reason).into_owned();
                    this.show_toast(ToastState::Failed(text), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 发开始任务的几条请求，顶上弹出提示条，等宿主回话后说开始了没有；开始了的过一会儿收起。
    fn send_start(&mut self, steps: Vec<Step>, cx: &mut Context<Self>) {
        let sent = store::send(cx, steps);
        let serial = self.show_toast(ToastState::Starting, cx);
        cx.spawn(async move |this, cx| {
            if let Err(reason) = sent.await {
                let text = rust_i18n::t!("kanban.toast.failed", reason = reason).into_owned();
                this.update(cx, |this, cx| this.show_toast(ToastState::Failed(text), cx)).ok();
                return;
            }
            this.update(cx, |this, cx| this.set_toast(serial, Some(ToastState::Started), cx)).ok();
            cx.background_executor().timer(TOAST_LINGER).await;
            this.update(cx, |this, cx| this.set_toast(serial, None, cx)).ok();
        })
        .detach();
    }

    /// 弹出一个新的提示条，顶掉原来的；返回它的编号。
    fn show_toast(&mut self, state: ToastState, cx: &mut Context<Self>) -> u64 {
        self.serial += 1;
        self.toast = Some(Toast { serial: self.serial, state });
        cx.notify();
        self.serial
    }

    /// 换掉或收起第 `serial` 个提示条；它已经被新的顶掉、或者关掉了就不动。
    fn set_toast(&mut self, serial: u64, state: Option<ToastState>, cx: &mut Context<Self>) {
        if self.toast.as_ref().is_some_and(|toast| toast.serial == serial) {
            self.toast = state.map(|state| Toast { serial, state });
            cx.notify();
        }
    }

    /// 卡片拖到 `to` 列放下：先做那一列的动作，再挪到 `before` 前面（放在列里的空白处时为空，不挪；同一列里
    /// 放在空白处时挪到这一列的最后）。
    fn drop_todo(&mut self, dragged: &DraggedTodo, to: Column, before: Option<String>, cx: &mut Context<Self>) {
        let Some(action) = dragged.drop_into(to) else { return };
        let id = dragged.id.clone();
        // 同一列里调先后不换列，记下了就一直留着，这张卡片以后换列时会被当成拖着放下的、不飞。
        if action != DropAction::Reorder {
            self.dropped = Some(id.clone());
        }
        let mut steps: Vec<Step> = Vec::new();
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
            None if action == DropAction::Reorder => Some(Self::after_column(to, cx)),
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
    fn after_column(column: Column, cx: &App) -> Option<String> {
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
        let Some(todo) = data.todos.iter().chain(&self.history.todos).find(|todo| todo.id == id) else { return };
        let alive = todo.session.is_some_and(|session| data.sessions.get(&session).is_some_and(|info| !info.exited));
        if !alive {
            Self::send_todos(vec![Box::new(move |req| ClientMsg::RemoveTodo { req, id })], cx);
            return;
        }
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &rust_i18n::t!("kanban.remove_title", title = todo.spec.title.as_str()),
            Some(&rust_i18n::t!("kanban.remove_detail")),
            &[&*rust_i18n::t!("kanban.remove"), &*rust_i18n::t!("kanban.cancel")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update(cx, |_, cx| {
                    Self::send_todos(vec![Box::new(move |req| ClientMsg::RemoveTodo { req, id })], cx)
                })
                .ok();
            }
        })
        .detach();
    }

    fn remove_schedule(&mut self, id: String, cx: &mut Context<Self>) {
        Self::send_todos(vec![Box::new(move |req| ClientMsg::RemoveSchedule { req, id })], cx);
    }

    /// 请 app 切到任务会话，见 `KanbanEvent::OpenSession`。
    fn open_session(session: SessionId, todo: &Todo, cx: &mut Context<Self>) {
        cx.emit(KanbanEvent::OpenSession { session, todo: Box::new(todo.clone()) });
    }

    /// 和上一帧比，记下换了列的卡片，从上一帧画的位置起飞；飞完、描边褪完的清掉。
    fn track_moves(&mut self, cx: &App) {
        if self.page != Page::Todos {
            return;
        }
        let current: HashMap<String, Column> = Column::ALL
            .into_iter()
            .zip(columns(store::data(cx), unix_now()))
            .flat_map(|(column, cards)| cards.into_iter().map(move |todo| (todo.id.clone(), column)))
            .collect();
        self.moves.retain(|id, moving| moving.at.elapsed() < FLY + LAND && current.contains_key(id));
        let mut geometry = self.geometry.borrow_mut();
        geometry.cards.retain(|id, _| current.contains_key(id));
        if let Some(seen) = &self.seen {
            for (id, column) in &current {
                if seen.get(id).is_none_or(|old| old == column) {
                    continue;
                }
                let dropped = self.dropped.as_ref() == Some(id);
                if dropped {
                    self.dropped = None;
                }
                self.serial += 1;
                let from = if dropped { None } else { geometry.cards.get(id).copied() };
                self.moves.insert(id.clone(), Move { from, serial: self.serial, at: Instant::now() });
            }
        }
        drop(geometry);
        self.seen = Some(current);
    }

    /// 正在换列的卡片：从上一帧的位置飞到这一列里它现在画的位置，盖在各列上面。
    fn render_flights(&self, data: &TodoData, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let now = unix_now();
        let geometry = self.geometry.borrow();
        let origin = geometry.board.origin;
        let mut flights = Vec::new();
        for (id, moving) in &self.moves {
            let Some(from) = moving.from.filter(|_| moving.at.elapsed() < FLY) else { continue };
            let Some(todo) = data.todos.iter().find(|todo| &todo.id == id) else { continue };
            // 新位置要等这一列排过一次版才知道，头一帧先停在原处。
            let to = geometry.cards.get(id).copied().unwrap_or(from);
            let (from, to) = (from.origin - origin, Bounds { origin: to.origin - origin, size: to.size });
            let width = (moving.from.map_or(to.size.width, |from| from.size.width), to.size.width);
            let face = self
                .card_face(todo, data, now, named("todo-flight-agent", id), None, None, cx)
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
    fn render_toast(&self, toast: &Toast, cx: &mut Context<Self>) -> AnyElement {
        let text = match &toast.state {
            ToastState::Starting => rust_i18n::t!("kanban.toast.starting").into_owned(),
            ToastState::Started => rust_i18n::t!("kanban.toast.started").into_owned(),
            ToastState::Failed(text) => text.clone(),
        };
        // 前景色作底、背景色写字，和后面的看板反着，深色浅色主题下都分得开。
        let (toast_bg, toast_fg) = (cx.theme().mix_inverse(0.15), cx.theme().background);
        let icon = match toast.state {
            ToastState::Starting => {
                let (frames, frame_time) = TOAST_SPINNER;
                div().child(spinner("todo-toast-spinner", frames, frame_time)).into_any_element()
            }
            ToastState::Started => {
                svg().path(CHECK_ICON).size(px(12.)).text_color(gpui::rgb(AGENT_DONE_COLOR)).into_any_element()
            }
            // 细线的叉在反色的提示条上看不清，又和右边的关闭按钮一个样：红底圆点里放白叉。
            ToastState::Failed(_) => div()
                .size(px(14.))
                .rounded_full()
                .bg(cx.theme().destructive)
                .flex()
                .items_center()
                .justify_center()
                .child(svg().path(CLOSE_ICON).size(px(8.)).text_color(gpui::white()))
                .into_any_element(),
        };
        let serial = toast.serial;
        let close_label = rust_i18n::t!("kanban.toast.close").into_owned();
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
            .hover(move |button| button.bg(toast_fg.opacity(0.15)))
            .child(svg().path(CLOSE_ICON).size(px(10.)).text_color(toast_fg.opacity(0.7)))
            .on_press(cx, move |this, _, cx| this.set_toast(serial, None, cx));
        let pill = div()
            .id("todo-toast")
            .role(Role::Label)
            .aria_label(text.clone())
            .flex()
            .items_center()
            .gap(px(8.))
            // 长原因在中间折行，不铺满一行盖住标题栏两头的按钮。
            .max_w_full()
            .max_w(scaled(560.))
            .min_h(scaled(32.))
            .py(px(6.))
            .pl(px(12.))
            .pr(px(8.))
            .rounded(px(10.))
            .bg(toast_bg)
            .text_color(toast_fg)
            .shadow(card_shadow(None, true))
            .child(div().flex_none().w(px(14.)).flex().justify_center().child(icon))
            .child(div().min_w_0().child(text))
            .child(close);
        div()
            .absolute()
            .top(px(10.))
            .left_0()
            .w_full()
            .px(px(16.))
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

    fn render_columns(&self, data: &TodoData, a11y: bool, cx: &mut Context<Self>) -> Div {
        let now = unix_now();
        let grouped = columns(data, now);
        let columns: Vec<_> = Column::ALL
            .iter()
            .zip(&grouped)
            .map(|(column, cards)| self.render_column(*column, cards, data, now, a11y, cx))
            .collect();
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("todo-columns")
                    .size_full()
                    .overflow_x_scroll()
                    .track_scroll(&self.columns_scroll)
                    .flex()
                    .gap(px(12.))
                    .children(columns),
            )
            .child(scrollbar("todo-columns-scrollbar", self.columns_scroll.clone(), Axis::Horizontal))
    }

    fn render_column(
        &self,
        column: Column,
        cards: &[&Todo],
        data: &TodoData,
        now: u64,
        a11y: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let fg = cx.theme().foreground;
        let name = column_name(column);
        let count = rust_i18n::t!("kanban.count", count = cards.len()).into_owned();
        let rows: Vec<_> = cards
            .iter()
            .enumerate()
            .map(|(ix, todo)| self.render_card(todo, column, ix, data, now, a11y, cx))
            .collect();
        let empty = cards.is_empty().then(|| {
            div().flex_1().flex().items_center().justify_center().child(label_row(
                ("todo-column-empty", column_index(column)),
                rust_i18n::t!("kanban.empty").into_owned(),
                fg.opacity(0.4),
            ))
        });
        // 需要你那一列有卡片时计数是个实心的徽标，一眼看得到。
        let badge = column == Column::NeedsYou && !cards.is_empty();
        let tally = if badge {
            needs_you_badge(cards.len()).into_any_element()
        } else {
            div()
                .flex_none()
                .text_size(FontSize::Tiny)
                .text_color(fg.opacity(0.5))
                .child(cards.len().to_string())
                .into_any_element()
        };
        let window_hint = (column == Column::Done).then(|| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(FontSize::Small)
                .text_color(fg.opacity(0.4))
                .font_weight(FontWeight::NORMAL)
                .child(rust_i18n::t!("kanban.done_window").into_owned())
                .child(div().flex_1())
                .child(history_link("todo-done-history").on_press(cx, |this, _, cx| this.show_history(None, cx)))
        });
        let accept_bg = cx.theme().mix(0.1);
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
            .bg(cx.theme().mix(0.05))
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
        primary: Option<Button>,
        cx: &mut Context<Self>,
    ) -> Div {
        let (fg, bg) = (cx.theme().foreground, cx.theme().background);
        let session = todo.session.and_then(|id| data.sessions.get(&id));
        let agent = session.and_then(|info| info.meta.agent);
        let place = self.place_name(&todo.spec.dir);
        let schedule = todo.schedule.as_ref().and_then(|id| data.schedules.iter().find(|s| &s.id == id));
        let footer: Vec<String> = [
            agent.is_none().then(|| todo.spec.agent.clone()),
            todo.started.map(|at| short_time(at, now)),
            schedule.map(|s| rust_i18n::t!("kanban.from_schedule", name = s.spec.title.as_str()).into_owned()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let muted = fg.opacity(0.55);
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
                .child(svg().path(BRANCH_ICON).flex_none().size(px(11.)).text_color(fg.opacity(0.45)))
                .child(div().min_w_0().truncate().child(branch))
        });
        let bottom = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .min_h(scaled(22.))
            .when_some(agent, |row, agent| row.child((self.marks.render)(agent, agent_id, cx)))
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
            .bg(bg)
            .border_1()
            .border_color(fg.opacity(0.08))
            .shadow(card_shadow(None, false))
            .child(top)
            .children(branch)
            .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(todo.spec.title.clone()))
            .child(bottom)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_card(
        &self,
        todo: &Todo,
        column: Column,
        ix: usize,
        data: &TodoData,
        now: u64,
        a11y: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let session = todo.session.and_then(|id| data.sessions.get(&id));
        let agent = session.and_then(|info| info.meta.agent);
        let place = self.place_name(&todo.spec.dir);
        let schedule = todo.schedule.as_ref().and_then(|id| data.schedules.iter().find(|s| &s.id == id));
        let status = agent.map_or_else(|| todo.spec.agent.clone(), self.marks.describe);
        let started =
            todo.started.map(|at| rust_i18n::t!("kanban.started_at", time = short_time(at, now)).into_owned());
        let from = schedule.map(|s| rust_i18n::t!("kanban.from_schedule", name = s.spec.title.as_str()).into_owned());
        let line2: Vec<String> = [Some(place), todo.branch.clone()].into_iter().flatten().collect();
        let line3: Vec<String> = [Some(status), started, from].into_iter().flatten().collect();
        let number = if todo.number > 0 { format!("#{}", todo.number) } else { String::new() };
        let description =
            [number, line2.join(" · "), line3.join(" · "), rust_i18n::t!("kanban.reorder_hint").into_owned()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("。");
        let drops: [Option<DropAction>; 4] = Column::ALL.map(|to| drop_action(todo, session, to, now));
        let dragged =
            DraggedTodo { id: todo.id.clone(), from: column, ix, drops, title: todo.spec.title.clone().into() };
        let id = todo.id.clone();
        let open_session = todo.session.filter(|_| session.is_some());
        // 这一列最该做的事是右下角的主按钮，别的收在右上角，鼠标移到卡片上（或者辅助工具开着）才露出来。
        let primary = if todo.started.is_none() && todo.done_at.is_none() {
            let id = id.clone();
            Some(
                Button::new("todo-start", rust_i18n::t!("kanban.start").into_owned())
                    .variant(ButtonVariant::Default)
                    .with_size(ButtonSize::Sm)
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .on_press(cx, move |this, _, cx| this.start_todo(id.clone(), cx)),
            )
        } else if column == Column::NeedsYou {
            Some(match open_session {
                Some(session_id) => {
                    let todo = todo.clone();
                    Button::new("todo-open", rust_i18n::t!("kanban.open").into_owned())
                        .variant(ButtonVariant::Default)
                        .with_size(ButtonSize::Sm)
                        .font_weight(FontWeight::MEDIUM)
                        .cursor_pointer()
                        .on_press(cx, move |_, _, cx| Self::open_session(session_id, &todo, cx))
                }
                None => {
                    let id = id.clone();
                    Button::new("todo-done", rust_i18n::t!("kanban.finish").into_owned())
                        .variant(ButtonVariant::Default)
                        .with_size(ButtonSize::Sm)
                        .font_weight(FontWeight::MEDIUM)
                        .cursor_pointer()
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
        let shown = a11y || self.hovered.as_ref() == Some(&todo.id);
        let mut buttons: Vec<Button> = Vec::new();
        if shown {
            if let Some(session_id) = open_session.filter(|_| primary_id != Some("open")) {
                let todo = todo.clone();
                let label = rust_i18n::t!("kanban.open").into_owned();
                buttons.push(
                    icon_button("todo-open", TERMINAL_ICON, label)
                        .on_press(cx, move |_, _, cx| Self::open_session(session_id, &todo, cx)),
                );
            }
            if primary_id != Some("done") {
                let done = todo.done_at.is_none();
                let label = if done { rust_i18n::t!("kanban.finish") } else { rust_i18n::t!("kanban.reopen") };
                let label = label.into_owned();
                let id = id.clone();
                buttons.push(
                    icon_button("todo-done", CHECK_ICON, label)
                        .on_press(cx, move |this, _, cx| this.finish_todo(id.clone(), done, cx)),
                );
            }
            {
                let todo = todo.clone();
                let label = rust_i18n::t!("kanban.edit").into_owned();
                buttons.push(icon_button("todo-edit", PENCIL_ICON, label).on_press(cx, move |this, window, cx| {
                    this.open_dialog(false, Some(Editing::Todo(todo.clone())), window, cx)
                }));
            }
            {
                let id = id.clone();
                let label = rust_i18n::t!("kanban.remove_todo").into_owned();
                buttons.push(
                    icon_button("todo-remove", TRASH_ICON, label)
                        .on_press(cx, move |this, window, cx| this.remove_todo(id.clone(), window, cx)),
                );
            }
        }
        let corner = (!buttons.is_empty()).then(|| div().flex_none().flex().gap(px(2.)).my(px(-4.)).children(buttons));
        let face = self.card_face(todo, data, now, named("todo-agent", &todo.id), corner, primary, cx);
        let geometry = self.geometry.clone();
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
                let now = if *hovered { Some(hover_id.clone()) } else { None };
                if *hovered || this.hovered.as_ref() == Some(&hover_id) {
                    this.hovered = now;
                    cx.notify();
                }
            }))
            .relative()
            .cursor_grab()
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedTodo>(move |style, dragged, _, cx| {
                if dragged.drop_into(column).is_none() {
                    return style;
                }
                // 跨列放下时插在这张前面，提示画在上边。
                let from = if dragged.from == column { dragged.ix } else { usize::MAX };
                drop_marker(style, from, ix, Axis::Vertical, cx)
            })
            .on_drop(cx.listener({
                let target = todo.id.clone();
                move |this, dragged: &DraggedTodo, _, cx| {
                    let before = drop_before(dragged, column, ix, &target, &store::data(cx).todos);
                    this.drop_todo(dragged, column, before, cx);
                }
            }))
            .child(
                // 记下这张卡片画在哪，换列时从这里起飞。
                canvas(move |bounds, _, _| _ = geometry.borrow_mut().cards.insert(bounds_id, bounds), |_, (), _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            );
        let Some(moving) = self.moves.get(&todo.id) else { return card.into_any_element() };
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
        self.input
            .workspaces
            .iter()
            .find(|(workspace, _)| workspace == dir)
            .map_or_else(|| base_name(dir), |(_, name)| name.to_string())
    }

    fn render_schedules(&self, data: &TodoData, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let now = unix_now();
        let hint = (!self.input.resident).then(|| {
            label_row("todo-schedules-hint", rust_i18n::t!("kanban.schedules_need_host").into_owned(), fg.opacity(0.6))
        });
        let rows: Vec<_> =
            data.schedules.iter().map(|schedule| self.render_schedule(schedule, data, now, cx)).collect();
        let empty = rows.is_empty().then(|| {
            label_row("todo-schedules-empty", rust_i18n::t!("kanban.no_schedules").into_owned(), fg.opacity(0.45))
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
                        .aria_label(rust_i18n::t!("kanban.schedules").into_owned())
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.schedules_scroll)
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .children(rows)
                        .children(empty),
                )
                .child(scrollbar("todo-schedules-scrollbar", self.schedules_scroll.clone(), Axis::Vertical)),
        )
    }

    fn render_schedule(&self, schedule: &Schedule, data: &TodoData, now: u64, cx: &mut Context<Self>) -> Stateful<Div> {
        let fg = cx.theme().foreground;
        let hover_bg = cx.theme().mix(0.06);
        let rule = describe_when(&schedule.when);
        let next = schedule.next_fire.map_or_else(
            || rust_i18n::t!("kanban.no_next").into_owned(),
            |at| rust_i18n::t!("kanban.next", time = short_time(at, now)).into_owned(),
        );
        let place = self.place_name(&schedule.spec.dir);
        let worktree = schedule.spec.worktree.then(|| rust_i18n::t!("kanban.worktree_on").into_owned());
        let details: Vec<String> =
            [Some(place), Some(schedule.spec.agent.clone()), worktree].into_iter().flatten().collect();
        let runs = recent_runs(&data.todos, &schedule.id);
        let mut run_rows: Vec<AnyElement> = runs
            .iter()
            .map(|todo| {
                let session = todo.session.and_then(|id| data.sessions.get(&id));
                let column = column(todo, session, now);
                let when = todo.started.map(|at| short_time(at, now)).unwrap_or_default();
                let label = run_label(column_name(column), &when);
                let todo = (*todo).clone();
                let row = div()
                    .id(named("todo-schedule-run", &todo.id))
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .text_size(FontSize::Small)
                    .text_color(fg.opacity(0.7))
                    .hover(move |row| row.bg(hover_bg))
                    .cursor_pointer()
                    .child(label);
                match todo.session.filter(|_| session.is_some()) {
                    Some(session) => row.on_press(cx, move |_, _, cx| Self::open_session(session, &todo, cx)),
                    None => row.on_press(cx, |this, _, cx| this.set_page(Page::Todos, cx)),
                }
                .into_any_element()
            })
            .collect();
        {
            let id = schedule.id.clone();
            run_rows.push(
                history_link(named("todo-schedule-history", &schedule.id))
                    .on_press(cx, move |this, _, cx| this.show_history(Some(id.clone()), cx))
                    .into_any_element(),
            );
        }
        let edit = {
            let schedule = schedule.clone();
            let label = rust_i18n::t!("kanban.edit_schedule").into_owned();
            icon_button("todo-schedule-edit", PENCIL_ICON, label).on_press(cx, move |this, window, cx| {
                this.open_dialog(true, Some(Editing::Schedule(schedule.clone())), window, cx)
            })
        };
        let remove = {
            let id = schedule.id.clone();
            let label = rust_i18n::t!("kanban.remove_schedule").into_owned();
            icon_button("todo-schedule-remove", TRASH_ICON, label)
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
            .bg(cx.theme().mix(0.03))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).child(schedule.spec.title.clone()))
                    .child(
                        div().text_size(FontSize::Small).text_color(fg.opacity(0.75)).child(format!("{rule} · {next}")),
                    )
                    .child(
                        div()
                            .text_size(FontSize::Small)
                            .text_color(fg.opacity(0.55))
                            .truncate()
                            .child(details.join(" · ")),
                    )
                    .child(div().pt(px(4.)).flex().flex_wrap().items_center().gap(px(4.)).children(run_rows)),
            )
            .child(div().flex_none().flex().items_start().gap(px(2.)).child(edit).child(remove))
    }

    /// 记录页：完成了的任务一行一张，完成得晚的在前，翻到底下再要下一页。
    fn render_history(&self, data: &TodoData, title: String, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let history = &self.history;
        let error = history.error.clone().map(|error| label_row("todo-history-error", error, fg.opacity(0.75)));
        let rows: Vec<_> = history.todos.iter().map(|todo| self.render_done(todo, data, cx)).collect();
        let empty = (history.loaded && rows.is_empty()).then(|| {
            label_row("todo-history-empty", rust_i18n::t!("kanban.no_history").into_owned(), fg.opacity(0.45))
        });
        let more = history.more.then(|| {
            div().flex_none().flex().justify_center().child(
                Button::new("todo-history-more", rust_i18n::t!("kanban.load_more").into_owned())
                    .variant(ButtonVariant::Secondary)
                    .with_size(ButtonSize::Sm)
                    .cursor_pointer()
                    .on_press(cx, |this, _, cx| this.load_history(true, cx)),
            )
        });
        div().size_full().flex().flex_col().gap(px(8.)).children(error).child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("todo-history")
                        .role(Role::List)
                        .aria_label(title)
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.history_scroll)
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .children(rows)
                        .children(empty)
                        .children(more),
                )
                .child(scrollbar("todo-history-scrollbar", self.history_scroll.clone(), Axis::Vertical)),
        )
    }

    /// 记录页上的一张任务：标题，下面是编号、目录、agent、分支、完成的时间和来自哪个定时；右边打开终端（会话还在时）、
    /// 取消完成、删除。
    fn render_done(&self, todo: &Todo, data: &TodoData, cx: &mut Context<Self>) -> Stateful<Div> {
        let fg = cx.theme().foreground;
        // 只看一个定时的记录时不再逐行写来自它。
        let schedule = todo
            .schedule
            .as_ref()
            .filter(|_| self.history.schedule.is_none())
            .and_then(|id| data.schedules.iter().find(|s| &s.id == id));
        let details: Vec<String> = [
            (todo.number > 0).then(|| format!("#{}", todo.number)),
            Some(self.place_name(&todo.spec.dir)),
            Some(todo.spec.agent.clone()),
            todo.branch.clone(),
            todo.done_at.map(|at| rust_i18n::t!("kanban.done_at", time = dialog::full_time(at)).into_owned()),
            schedule.map(|s| rust_i18n::t!("kanban.from_schedule", name = s.spec.title.as_str()).into_owned()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut buttons: Vec<Button> = Vec::new();
        if let Some(session) = todo.session.filter(|id| data.sessions.contains_key(id)) {
            let todo = todo.clone();
            let label = rust_i18n::t!("kanban.open").into_owned();
            buttons.push(
                icon_button("todo-open", TERMINAL_ICON, label)
                    .on_press(cx, move |_, _, cx| Self::open_session(session, &todo, cx)),
            );
        }
        {
            let id = todo.id.clone();
            let label = rust_i18n::t!("kanban.reopen").into_owned();
            buttons.push(
                icon_button("todo-done", CHECK_ICON, label)
                    .on_press(cx, move |this, _, cx| this.finish_todo(id.clone(), false, cx)),
            );
        }
        {
            let id = todo.id.clone();
            let label = rust_i18n::t!("kanban.remove_todo").into_owned();
            buttons.push(
                icon_button("todo-remove", TRASH_ICON, label)
                    .on_press(cx, move |this, window, cx| this.remove_todo(id.clone(), window, cx)),
            );
        }
        div()
            .id(named("todo-history-row", &todo.id))
            .role(Role::ListItem)
            .aria_label(todo.spec.title.clone())
            .aria_description(details.join("。"))
            .flex_none()
            .p(px(12.))
            .flex()
            .gap(px(12.))
            .rounded(px(8.))
            .bg(cx.theme().mix(0.03))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).child(todo.spec.title.clone()))
                    .child(
                        div()
                            .text_size(FontSize::Small)
                            .text_color(fg.opacity(0.55))
                            .truncate()
                            .child(details.join(" · ")),
                    ),
            )
            .child(div().flex_none().flex().items_start().gap(px(2.)).children(buttons))
    }
}

impl Render for Kanban {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.track_moves(cx);
        let a11y = window.is_a11y_active();
        let data = store::data(cx).clone();
        let data = &data;
        let fg = cx.theme().foreground;
        let title = match self.page {
            Page::Todos => rust_i18n::t!("kanban.title"),
            Page::Schedules => rust_i18n::t!("kanban.schedules"),
            Page::History => {
                match self.history.schedule.as_ref().and_then(|id| data.schedules.iter().find(|s| &s.id == id)) {
                    Some(schedule) => rust_i18n::t!("kanban.history_of", name = schedule.spec.title.as_str()),
                    None => rust_i18n::t!("kanban.history"),
                }
            }
        }
        .into_owned();
        // 记录页回到进来的那一页：只看一个定时的回定时页。
        let back = (self.page == Page::History).then(|| {
            let label = rust_i18n::t!("kanban.back").into_owned();
            let to = if self.history.schedule.is_some() { Page::Schedules } else { Page::Todos };
            icon_button("kanban-back", ARROW_LEFT_ICON, label).on_press(cx, move |this, _, cx| this.set_page(to, cx))
        });
        // 新建任务的入口在侧栏上，这里只给定时页放新建定时。
        let new_schedule = (self.page == Page::Schedules).then(|| {
            Button::new("kanban-new-schedule", rust_i18n::t!("kanban.new_schedule").into_owned())
                .variant(ButtonVariant::Secondary)
                .icon(PLUS_ICON)
                .icon_size(12.)
                .h_auto()
                .px(px(10.))
                .py(px(4.))
                .cursor_pointer()
                .on_press(cx, |this, window, cx| this.open_dialog(true, None, window, cx))
        });
        let close_label = rust_i18n::t!("kanban.close").into_owned();
        let close =
            icon_button("kanban-close", CLOSE_ICON, close_label).on_press(cx, |_, _, cx| cx.emit(KanbanEvent::Close));
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .children(back)
            .child(
                div()
                    .id("kanban-title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(title.clone())
                    .text_size(FontSize::Title)
                    .font_weight(FontWeight::BOLD)
                    .child(title.clone()),
            )
            .child(div().flex_1())
            .children(new_schedule)
            .child(close);
        let error = data.error.clone().map(|error| label_row("kanban-error", error, fg.opacity(0.75)));
        let (content, flights) = match self.page {
            Page::Todos => (self.render_columns(data, a11y, cx).into_any_element(), self.render_flights(data, cx)),
            Page::Schedules => (self.render_schedules(data, cx).into_any_element(), Vec::new()),
            Page::History => (self.render_history(data, title.clone(), cx).into_any_element(), Vec::new()),
        };
        let toast = self.toast.as_ref().map(|toast| self.render_toast(toast, cx));
        let geometry = self.geometry.clone();
        div()
            .id("kanban")
            .role(Role::Group)
            .aria_label(title)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    cx.emit(KanbanEvent::Close);
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(16.))
            .text_size(FontSize::Body)
            .text_color(fg)
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
            .children(toast)
    }
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

/// 列头圆点的颜色；`fg` 是主题的前景色。
fn column_color(column: Column, fg: Hsla) -> Hsla {
    match column {
        Column::Todo => fg.opacity(0.35),
        Column::InProgress => gpui::rgb(IN_PROGRESS_COLOR).into(),
        Column::NeedsYou => gpui::rgb(AGENT_BLOCKED_COLOR).into(),
        Column::Done => gpui::rgb(AGENT_DONE_COLOR).into(),
    }
}

/// 看板上的图标按钮：`label` 是辅助工具读的名字，也是悬停的提示。
fn icon_button(id: impl Into<ElementId>, icon: &'static str, label: String) -> Button {
    Button::new(id, label.clone())
        .icon(icon)
        .icon_size(13.)
        .with_size(ButtonSize::Icon)
        .cursor_pointer()
        .tooltip(tooltip(label, None))
}

/// 进记录页的链接，放在完成那一列的列头和各个定时下面；按下做什么由调用方接上。
fn history_link(id: impl Into<ElementId>) -> Button {
    Button::new(id, rust_i18n::t!("kanban.all_history").into_owned())
        .variant(ButtonVariant::Link)
        .with_size(ButtonSize::Sm)
        .cursor_pointer()
}

/// 只起提示作用的一行字，报给辅助工具。
fn label_row(id: impl Into<gpui::ElementId>, text: String, color: Hsla) -> Stateful<Div> {
    div().id(id).role(Role::Label).aria_label(text.clone()).text_size(FontSize::Small).text_color(color).child(text)
}

fn column_name(column: Column) -> String {
    match column {
        Column::Todo => rust_i18n::t!("kanban.column.todo"),
        Column::InProgress => rust_i18n::t!("kanban.column.in_progress"),
        Column::NeedsYou => rust_i18n::t!("kanban.column.needs_you"),
        Column::Done => rust_i18n::t!("kanban.column.done"),
    }
    .into_owned()
}

/// 定时的规则给人看的写法：「每天 09:30 · 周一、周三」「每 15 分钟」「一次 · 10-13 14:00」。
fn describe_when(when: &When) -> String {
    match *when {
        When::Once { at } => rust_i18n::t!("kanban.when.once", time = dialog::full_time(at)).into_owned(),
        When::Daily { hour, minute, days } => {
            let time = format!("{hour:02}:{minute:02}");
            if days == 0 {
                rust_i18n::t!("kanban.when.daily", time = time).into_owned()
            } else {
                let names: Vec<String> = (0..7).filter(|day| days & (1 << day) != 0).map(dialog::weekday).collect();
                rust_i18n::t!("kanban.when.weekly", time = time, days = names.join(" ")).into_owned()
            }
        }
        When::Every { minutes } if minutes % 60 == 0 => {
            rust_i18n::t!("kanban.when.every_hours", count = minutes / 60).into_owned()
        }
        When::Every { minutes } => rust_i18n::t!("kanban.when.every_minutes", count = minutes).into_owned(),
        When::Unknown => rust_i18n::t!("kanban.when.unknown").into_owned(),
    }
}

/// 定时页里一次运行的标签：所在的列和开始的时间；还没开始（`when` 为空）时只有列名。
fn run_label(column: String, when: &str) -> String {
    if when.is_empty() { column } else { format!("{column} · {when}") }
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

    /// 同一列里往下拖挪到目标后面，往上拖、跨列挪到目标前面，和落点提示画的一样。
    #[test]
    fn drops_land_where_the_marker_is() {
        let todos = [todo("a", None, None), todo("b", None, None), todo("c", None, None)];
        let dragged = |ix: usize, from: Column| DraggedTodo {
            id: todos[ix].id.clone(),
            from,
            ix,
            drops: [Some(DropAction::Reorder); 4],
            title: SharedString::default(),
        };
        let before = |ix: usize, from: Column, onto: usize| {
            drop_before(&dragged(ix, from), Column::Todo, onto, &todos[onto].id, &todos)
        };
        // a 拖到 b 上：b、a、c；拖到 c 上：排到最后。
        assert_eq!(before(0, Column::Todo, 1), Some("c".into()));
        assert_eq!(before(0, Column::Todo, 2), None);
        // c 拖到 a 上：c、a、b；从别的列拖来插在目标前面。
        assert_eq!(before(2, Column::Todo, 0), Some("a".into()));
        assert_eq!(before(0, Column::Done, 1), Some("b".into()));
    }

    /// 最近开的几个按建的先后挑，不管看板上拖成了什么顺序。
    #[test]
    fn recent_runs_go_by_creation() {
        let run =
            |id: &str, created: u64| Todo { created, schedule: Some("s".into()), ..todo(id, Some(created), None) };
        // 拖动过：最早的 a 排到了最后。
        let todos = [run("d", 4), run("c", 3), todo("x", None, None), run("b", 2), run("e", 5), run("a", 1)];
        let ids: Vec<_> = recent_runs(&todos, "s").iter().map(|todo| todo.id.as_str()).collect();
        assert_eq!(ids, ["e", "d", "c"]);
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

    /// 还没开始的运行没有时间，标签里不留多余的分隔点。
    #[test]
    fn run_labels_skip_a_missing_time() {
        assert_eq!(run_label("Todo".into(), ""), "Todo");
        assert_eq!(run_label("Done".into(), "09:30"), "Done · 09:30");
    }
}
