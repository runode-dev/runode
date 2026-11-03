//! 铺满窗口的看板：派给 agent 的任务按列排开（待办、进行中、需要你、完成），和它平级的另一页列出
//! 定时。完成那一列只放近期完成的，再早的在记录页上翻（`History`），从完成那一列的列头和各个定时进去，
//! 后者只列那个定时开出的。数据由宿主持有，这边经 `store` 读写；新建、编辑任务和定时的对话框在 `dialog`，任务的
//! 那个占满看板，是平级的又一页。
//!
//! 卡片在哪一列由任务和它会话里 agent 的状态推出来（`runode_protocol::todo::column`），拖拽就是做那一列
//! 的动作（`drop_action`）：拖进进行中是开始，拖进完成是标完成，从完成拖出是取消完成，同一列里上下拖调
//! 先后；办不到的放下不接，卡片留在原处。辅助工具拖不了：动作另有按钮，调先后另登记了加减（上移、下移）。
//!
//! 看板是一个单独的视图（`Kanban`），app 打开时建、收起时丢掉，一个窗口最多一个。它和配对设备页一样盖住
//! 标签和分屏，摆在哪由 app 管。和宿主说话经 app 启动时交进来的 `Host`（`watch`），数据全 app 一份。各
//! workspace 的目录和名字、新建时默认的目录、定时能不能在后台跑由 app 每次画之前经 `set_input` 交进来；卡片上
//! agent 的状态标记和标签、侧栏上的一样，由 app 画（`AgentMarks`）。按了关闭、Esc（`KanbanEvent::Close`），
//! 卡片的「打开终端」（`KanbanEvent::OpenSession`：把任务的会话接进窗口）交给 app 办，会话没了时先请宿主另开
//! 一个接着 agent 上次的对话（`open_todo`）。对话框盖住整个窗口，由 app 画在窗口最上层（`Kanban::dialog`）。
//!
//! 卡片换了列时从原来的位置飞到新的位置，落下后淡入、描一圈强调色再褪掉；拖着放下的不飞，只描边。
//! 每帧记下各卡片画在哪（`Geometry`），下一帧发现换了列就照上一帧的位置起飞。开始任务时顶上居中弹出
//! 提示条（`Toast`），等宿主回话后说开始了没有；别的请求没办成时也在这里说原因。

mod board;
mod dialog;
mod history;
mod pulls;
mod schedules;
mod skills;
mod store;
mod templates;
mod when;

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use crate::ui::{
    a11y::{Press, StaticText},
    badge::Badge,
    base_name,
    button::{Button, ButtonSize, ButtonVariant},
    font_size::{FontSize, scaled},
    icons::{ARROW_LEFT_ICON, CHECK_ICON, CLOSE_ICON, PLUS_ICON},
    spinner::spinner,
    theme::{AGENT_DONE_COLOR, ActiveTheme as _},
    tooltip::tooltip,
};
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, BoxShadow, Context, Div, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent, Render, Role, ScrollHandle, SharedString, Stateful,
    Subscription, Window, canvas, div, ease_out_quint, point, prelude::*, px, svg,
};
use runode_protocol::{
    ClientMsg, HostMsg, SessionId,
    todo::{Column, Todo},
};
use runode_types::agent::Agent;

pub use dialog::TodoDialog;
pub use store::{Host, Step, needs_you, todo_sessions, watch};

use board::{Geometry, Move};
use dialog::{DialogEvent, Editing};
use history::History;

/// 强调色：落下后的描边、需要你那一列的计数。
const ACCENT: u32 = 0x4F46E5;
/// 落下后描的那圈强调色有多宽。
const RING_WIDTH: f32 = 1.5;
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
    /// 找 agent 用的 PATH：对话框向 agent 问模型清单时用。
    pub shell_path: Option<std::ffi::OsString>,
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
    /// 点了看板里的新建任务：和侧栏的入口一样打开新建任务那一页，上回写到一半的接着写。
    NewTodo,
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
    /// 看板只为新建任务那一页开着：取消时整个收起，存好了才露出看板。
    alone: bool,
    /// 各任务分支上的 PR，见 `pulls`。
    pulls: pulls::Pulls,
    /// PR 合并后标成完成过的任务，见 `pulls`。
    merged: std::collections::HashSet<String>,
    /// 正在推送分支、开 PR 的任务。
    creating_pulls: std::collections::HashSet<String>,
    _poll_pulls: gpui::Task<()>,
}

/// 顶上的提示条。
struct Toast {
    serial: u64,
    state: ToastState,
}

enum ToastState {
    Starting,
    /// 正在推送任务的分支、开 PR。
    Creating,
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

/// 按显示的东西（任务、定时的标识）取的元素 id。
fn named(prefix: &str, id: &str) -> ElementId {
    ElementId::Name(format!("{prefix}-{id}").into())
}

/// 需要你的计数：实心的徽标，一眼看得到。看板上那一列的列头和侧栏的看板入口都用。
pub fn needs_you_badge(count: usize) -> Badge {
    Badge::new(count.to_string()).bg(gpui::rgb(ACCENT)).text_color(gpui::white())
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
            alone: false,
            pulls: pulls::Pulls::new(),
            merged: std::collections::HashSet::new(),
            creating_pulls: std::collections::HashSet::new(),
            _poll_pulls: pulls::poll(cx),
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

    /// 开着的定时的新建、编辑对话框。它盖住整个窗口，app 画在窗口的最上层，开着时把下面的界面藏起来不给辅助工具读。
    /// 任务的是看板里的一页，看板自己画，不在这里。
    pub fn dialog(&self, cx: &App) -> Option<Entity<TodoDialog>> {
        self.dialog.as_ref().map(|(dialog, _)| dialog.clone()).filter(|dialog| !dialog.read(cx).page)
    }

    /// 关掉新建、编辑任务那一页，焦点交回看板；开着的不是那一页时什么都不做，为假。
    pub fn close_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.dialog.as_ref().is_some_and(|(dialog, _)| dialog.read(cx).page) {
            return false;
        }
        self.dialog = None;
        self.alone = false;
        window.focus(&self.focus, cx);
        cx.notify();
        true
    }

    /// 开着新建、编辑的对话框或那一页：写到一半，换页、收起看板会丢掉。
    pub fn editing(&self) -> bool {
        self.dialog.is_some()
    }

    /// 换到定时页（`schedules`）或任务页：app 从侧栏的入口打开时调。
    pub fn show_schedules(&mut self, schedules: bool, cx: &mut Context<Self>) {
        self.set_page(if schedules { Page::Schedules } else { Page::Todos }, cx);
    }

    /// 换到任务页，打开新建任务那一页，有收起来的（`draft`）就接着写：app 从侧栏的入口或快捷键打开时调。
    /// `alone` 是看板只为这一页才开的，取消时发 `KanbanEvent::Close`。
    pub fn new_todo(
        &mut self,
        alone: bool,
        draft: Option<Entity<TodoDialog>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_page(Page::Todos, cx);
        match draft {
            Some(dialog) => {
                let focus = dialog.read(cx).title_focus(cx);
                window.focus(&focus, cx);
                self.show_dialog(dialog, window, cx);
            }
            None => self.open_dialog(false, None, window, cx),
        }
        self.alone = alone;
    }

    /// 开着的是写到一半、能收起来留着的新建任务那一页（`TodoDialog::draft`）。
    pub fn drafting(&self, cx: &App) -> bool {
        self.dialog.as_ref().is_some_and(|(dialog, _)| dialog.read(cx).draft())
    }

    /// 取下写到一半的新建任务那一页，露出看板：用户要去别处时 app 收起来留着。
    pub fn take_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TodoDialog>> {
        let (dialog, _) = self.dialog.take_if(|(dialog, _)| dialog.read(cx).draft())?;
        self.alone = false;
        window.focus(&self.focus, cx);
        cx.notify();
        Some(dialog)
    }

    /// 显示着的是只为新建任务开的那一页，看板还没露出来。
    pub fn creating(&self) -> bool {
        self.alone
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
        let Input { workspaces, dir, shell_path, .. } = self.input.clone();
        let dialog = cx.new(|cx| TodoDialog::new(schedule, editing, dir, workspaces, shell_path, window, cx));
        self.show_dialog(dialog, window, cx);
    }

    fn show_dialog(&mut self, dialog: Entity<TodoDialog>, window: &mut Window, cx: &mut Context<Self>) {
        let events = cx.subscribe_in(&dialog, window, |this, _, event: &DialogEvent, window, cx| {
            this.dialog = None;
            match event {
                DialogEvent::Dismissed if std::mem::take(&mut this.alone) => return cx.emit(KanbanEvent::Close),
                DialogEvent::Saved { start: Some(id) } => this.start_todo(id.clone(), cx),
                _ => {}
            }
            this.alone = false;
            // 焦点在对话框的输入框里，对话框一丢就没了着落，看板的 Esc 也收不到：关掉、存好了都交回看板。
            window.focus(&this.focus, cx);
            cx.notify();
        });
        self.dialog = Some((dialog, events));
        cx.notify();
    }

    /// 依次发几条看板的请求，前一条没办成就不发后面的。办成了重翻记录：完成已久（不在完成那一列）的任务改了，看板的数据不变，
    /// 不会因此重翻。
    fn send_todos(steps: Vec<Step>, cx: &mut Context<Self>) {
        Self::send_todos_then(steps, |_| {}, cx);
    }

    /// 和 `send_todos` 一样，没办成时先做 `failed`。
    fn send_todos_then(steps: Vec<Step>, failed: impl FnOnce(&mut Self) + 'static, cx: &mut Context<Self>) {
        let sent = store::send(cx, steps);
        cx.spawn(async move |this, cx| {
            let sent = sent.await;
            this.update(cx, |this, cx| match sent {
                Ok(()) => this.reload_history(cx),
                Err(reason) => {
                    failed(this);
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

    fn start_todo(&mut self, id: String, cx: &mut Context<Self>) {
        self.send_start(vec![Box::new(move |req| ClientMsg::StartTodo { req, id })], cx);
    }

    fn finish_todo(&mut self, id: String, done: bool, cx: &mut Context<Self>) {
        Self::send_todos(
            vec![Box::new(move |req| ClientMsg::UpdateTodo { req, id, spec: None, done: Some(done) })],
            cx,
        );
    }

    /// 删掉任务；它的会话还在（会话留着，在后台照常跑）或开始了还没做完时先问一句。
    fn remove_todo(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let data = store::data(cx);
        let Some(todo) = data.todos.iter().chain(&self.history.todos).find(|todo| todo.id == id) else { return };
        let alive = todo.session.is_some_and(|session| data.sessions.get(&session).is_some_and(|info| !info.exited));
        // 开始了还没做完的删了就接不上 agent 上次的对话，先问；没开始的和做完的直接删。
        let unfinished = todo.started.is_some() && todo.done_at.is_none();
        if !alive && !unfinished {
            Self::send_todos(vec![Box::new(move |req| ClientMsg::RemoveTodo { req, id })], cx);
            return;
        }
        let detail = if alive {
            rust_i18n::t!("kanban.remove_detail")
        } else {
            rust_i18n::t!("kanban.remove_unfinished_detail")
        };
        let answer = crate::prompt::show(
            window,
            gpui::PromptLevel::Warning,
            &rust_i18n::t!("kanban.remove_title", title = todo.spec.title.as_str()),
            Some(&detail),
            &[
                gpui::PromptButton::new(rust_i18n::t!("kanban.remove")),
                gpui::PromptButton::cancel(rust_i18n::t!("kanban.cancel")),
            ],
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

    /// 请 app 切到任务会话，见 `KanbanEvent::OpenSession`。
    fn open_session(session: SessionId, todo: &Todo, cx: &mut Context<Self>) {
        cx.emit(KanbanEvent::OpenSession { session, todo: Box::new(todo.clone()) });
    }

    /// 打开开始过的任务的终端：会话还在就切过去，没了（终端关了）就请宿主另开一个接着 agent 上次的对话
    /// （`ClientMsg::ResumeTodo`），开好了再切过去。
    fn open_todo(&mut self, todo: &Todo, cx: &mut Context<Self>) {
        if let Some(session) = todo.session.filter(|id| store::data(cx).sessions.contains_key(id)) {
            Self::open_session(session, todo, cx);
            return;
        }
        let id = todo.id.clone();
        let asked = store::ask(cx, Box::new(move |req| ClientMsg::ResumeTodo { req, id }));
        let todo = todo.clone();
        cx.spawn(async move |this, cx| {
            let answer = asked.await;
            this.update(cx, |this, cx| match answer {
                Ok(HostMsg::Opened { id, .. }) => Self::open_session(id, &todo, cx),
                answer => {
                    let reason = match answer {
                        Err(err) => format!("{err:#}"),
                        Ok(other) => format!("unexpected answer from the host: {other:?}"),
                    };
                    let text = rust_i18n::t!("kanban.toast.request_failed", reason = reason).into_owned();
                    this.show_toast(ToastState::Failed(text), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// 顶上居中的提示条：开始任务时转着圈，宿主回话后说开始了没有。
    fn render_toast(&self, toast: &Toast, cx: &mut Context<Self>) -> AnyElement {
        let text = match &toast.state {
            ToastState::Starting => rust_i18n::t!("kanban.toast.starting").into_owned(),
            ToastState::Creating => rust_i18n::t!("kanban.toast.creating_pr").into_owned(),
            ToastState::Started => rust_i18n::t!("kanban.toast.started").into_owned(),
            ToastState::Failed(text) => text.clone(),
        };
        // 前景色作底、背景色写字，和后面的看板反着，深色浅色主题下都分得开。
        let (toast_bg, toast_fg) = (cx.theme().mix_inverse(0.15), cx.theme().background);
        let icon = match toast.state {
            ToastState::Starting | ToastState::Creating => {
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
            .aria_static_text(text.clone())
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

    /// 目录给人看的名字：正好是哪个 workspace 的目录时是它的名字，否则是目录名。
    fn place_name(&self, dir: &Path) -> String {
        self.input
            .workspaces
            .iter()
            .find(|(workspace, _)| workspace == dir)
            .map_or_else(|| base_name(dir), |(_, name)| name.to_string())
    }
}

impl Render for Kanban {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 新建、编辑任务那一页占满看板；焦点在它的输入框里，Esc 由它自己管。
        if let Some((dialog, _)) = self.dialog.as_ref().filter(|(dialog, _)| dialog.read(cx).page) {
            return div()
                .id("kanban")
                .track_focus(&self.focus)
                .size_full()
                .p(px(16.))
                .text_size(FontSize::Body)
                .text_color(cx.theme().foreground)
                .child(dialog.clone())
                .into_any_element();
        }
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
        let close_label = match self.page {
            Page::Todos => rust_i18n::t!("kanban.close"),
            Page::Schedules => rust_i18n::t!("kanban.close_schedules"),
            Page::History => rust_i18n::t!("kanban.close_history"),
        }
        .into_owned();
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
            .into_any_element()
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
    div().id(id).aria_static_text(text.clone()).text_size(FontSize::Small).text_color(color).child(text)
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

#[cfg(test)]
mod tests {
    use runode_protocol::todo::{Todo, TodoSpec};

    /// 几页的测试共用的任务：只有标识、开始和完成的时间不同。
    pub(super) fn todo(id: &str, started: Option<u64>, done_at: Option<u64>) -> Todo {
        Todo {
            id: id.into(),
            number: 0,
            spec: TodoSpec {
                title: id.into(),
                prompt: String::new(),
                dir: "/src".into(),
                agent: "claude".into(),
                worktree: false,
                ..Default::default()
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
}
