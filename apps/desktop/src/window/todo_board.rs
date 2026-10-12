//! 铺满窗口的任务板：派给 agent 的任务按列排开（待办、进行中、需要你、完成），和它平级的另一页列出
//! 定时。数据由宿主持有，这边经 `store` 读写；新建、编辑任务和定时的对话框在 `dialog`。
//!
//! 卡片在哪一列由任务和它会话里 agent 的状态推出来（`runode_protocol::todo::column`），拖拽就是做那一列
//! 的动作（`drop_action`）：拖进进行中是开始，拖进完成是标完成，从完成拖出是取消完成，同一列里上下拖调
//! 先后；办不到的放下不接，卡片留在原处。辅助工具拖不了：动作另有按钮，调先后另登记了加减（上移、下移）。
//!
//! 看板和手机端引导页一样盖住标签和分屏：切到任何一个标签或 workspace、按 Esc 就收起。卡片的「打开终端」
//! 把任务的会话接进它目录所在的 workspace 的一个新标签，已经在哪个分屏里时切过去。

mod dialog;
pub(super) mod store;

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use gpui::{
    AccessibleAction, AnyElement, Axis, Context, Div, ElementId, FocusHandle, Focusable as _, FontWeight, Hsla,
    KeyDownEvent, Render, Role, ScrollHandle, SharedString, Stateful, Window, div, prelude::*, px, svg,
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
    titlebar::{drag_chip, drop_marker, styled_agent_mark},
};
use crate::{
    assets::{BRANCH_ICON, CHECK_ICON, CLOSE_ICON, PENCIL_ICON, PLAY_ICON, PLUS_ICON, TERMINAL_ICON, TRASH_ICON},
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

/// 开着的任务板。
pub(super) struct TodoBoard {
    pub(super) focus: FocusHandle,
    page: Page,
    columns_scroll: ScrollHandle,
    schedules_scroll: ScrollHandle,
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

    /// 打开任务板，焦点给它；马上重问一次任务会话的状态，不等下一轮。
    pub(super) fn show_todo_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mobile = None;
        self.drop_settings(cx);
        let board = self.todo_board.get_or_insert_with(|| TodoBoard {
            focus: cx.focus_handle(),
            page: Page::Todos,
            columns_scroll: ScrollHandle::new(),
            schedules_scroll: ScrollHandle::new(),
        });
        window.focus(&board.focus, cx);
        store::refresh_sessions(cx).detach();
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
            cx.notify();
        }
    }

    /// 依次发几条任务板的请求，前一条没办成就不发后面的。
    fn send_todos(steps: Vec<store::Step>, cx: &mut Context<Self>) {
        store::send(cx, steps);
    }

    /// 卡片拖到 `to` 列放下：先做那一列的动作，再挪到 `before` 前面（放在列里的空白处时为空，不挪；同一列里
    /// 放在空白处时挪到这一列的最后）。
    fn drop_todo(&mut self, dragged: &DraggedTodo, to: Column, before: Option<String>, cx: &mut Context<Self>) {
        let Some(action) = dragged.drop_into(to) else { return };
        let id = dragged.id.clone();
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
        Self::send_todos(steps, cx);
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
        Self::send_todos(vec![Box::new(move |req| ClientMsg::StartTodo { req, id })], cx);
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
                cx.update(|cx| store::send(cx, vec![Box::new(move |req| ClientMsg::RemoveTodo { req, id })]));
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
        let page = self.render_todo_board(fg, bg, cx).into_any_element();
        self.render_page_body(page, fg, bg, window, cx)
    }

    fn render_todo_board(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
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
        let content = match board.page {
            Page::Todos => self.render_columns(board, data, fg, bg, cx).into_any_element(),
            Page::Schedules => self.render_schedules(board, data, fg, bg, cx).into_any_element(),
        };
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
            .size_full()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(16.))
            .text_size(FontSize::Body)
            .text_color(hsla(fg))
            .child(header)
            .children(error)
            .child(div().flex_1().min_h_0().child(content))
    }

    fn render_columns(&self, board: &TodoBoard, data: &TodoData, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let now = unix_now();
        let grouped = columns(data, now);
        let columns: Vec<_> = Column::ALL
            .iter()
            .zip(&grouped)
            .map(|(column, cards)| self.render_column(*column, cards, data, now, fg, bg, cx))
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
        column: Column,
        cards: &[&Todo],
        data: &TodoData,
        now: u64,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let name = column_name(column);
        let count = rust_i18n::t!("todo_board.count", count = cards.len()).into_owned();
        let rows: Vec<_> = cards
            .iter()
            .enumerate()
            .map(|(ix, todo)| self.render_card(todo, column, ix, data, now, fg, bg, cx))
            .collect();
        let empty = cards.is_empty().then(|| {
            label_row(
                ("todo-column-empty", column_index(column)),
                rust_i18n::t!("todo_board.empty").into_owned(),
                hsla(fg).opacity(0.4),
            )
        });
        let accept_bg = hsla(bg.mix(fg, 0.08));
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
            .rounded(px(8.))
            .bg(hsla(bg.mix(fg, 0.03)))
            .child(
                div()
                    .flex_none()
                    .h(scaled(32.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(name)
                    .child(
                        div()
                            .text_size(FontSize::Small)
                            .text_color(hsla(fg).opacity(0.5))
                            .child(cards.len().to_string()),
                    ),
            )
            .child(
                div()
                    .id(("todo-column-body", column_index(column)))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.))
                    .pb(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
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

    #[allow(clippy::too_many_arguments)]
    fn render_card(
        &self,
        todo: &Todo,
        column: Column,
        ix: usize,
        data: &TodoData,
        now: u64,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
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
        let line2: Vec<String> = [Some(place.clone()), todo.branch.clone()].into_iter().flatten().collect();
        let line3: Vec<String> = [Some(status), started, from].into_iter().flatten().collect();
        let description = [line2.join(" · "), line3.join(" · "), rust_i18n::t!("todo_board.reorder_hint").into_owned()]
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
        let mut buttons: Vec<Stateful<Div>> = Vec::new();
        if todo.started.is_none() && todo.done_at.is_none() {
            let id = id.clone();
            buttons.push(
                icon_button("todo-start", PLAY_ICON, rust_i18n::t!("todo_board.start").into_owned(), fg, bg)
                    .on_press(cx, move |this, _, cx| this.start_todo(id.clone(), cx)),
            );
        }
        if let Some(session_id) = todo.session.filter(|_| session.is_some()) {
            let todo = todo.clone();
            buttons.push(
                icon_button("todo-open", TERMINAL_ICON, rust_i18n::t!("todo_board.open").into_owned(), fg, bg)
                    .on_press(cx, move |this, window, cx| this.open_todo_session(session_id, &todo, window, cx)),
            );
        }
        {
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
                icon_button("todo-edit", PENCIL_ICON, rust_i18n::t!("todo_board.edit").into_owned(), fg, bg).on_press(
                    cx,
                    move |this, window, cx| {
                        this.open_todo_dialog(false, Some(dialog::Editing::Todo(todo.clone())), window, cx)
                    },
                ),
            );
        }
        {
            let id = id.clone();
            buttons.push(
                icon_button("todo-remove", TRASH_ICON, rust_i18n::t!("todo_board.remove_todo").into_owned(), fg, bg)
                    .on_press(cx, move |this, window, cx| this.remove_todo(id.clone(), window, cx)),
            );
        }
        let small = |parts: Vec<String>| {
            (!parts.is_empty()).then(|| {
                div().text_size(FontSize::Small).text_color(fg_h.opacity(0.6)).truncate().child(parts.join(" · "))
            })
        };
        let branch_line = (!line2.is_empty()).then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(FontSize::Small)
                .text_color(fg_h.opacity(0.6))
                .min_w_0()
                .child(div().truncate().child(place.clone()))
                .children(todo.branch.clone().map(|branch| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(3.))
                        .min_w_0()
                        .child(svg().path(BRANCH_ICON).flex_none().size(px(11.)).text_color(fg_h.opacity(0.5)))
                        .child(div().truncate().child(branch))
                }))
        });
        let (up_id, down_id) = (id.clone(), id);
        div()
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
            .flex_none()
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .rounded(px(8.))
            .bg(hsla(bg))
            .border_1()
            .border_color(fg_h.opacity(0.1))
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
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .when_some(mark, |row, mark| {
                        row.child(styled_agent_mark(mark, named("todo-agent", &todo.id), fg_h, super::cards(cx)))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(todo.spec.title.clone()),
                    ),
            )
            .children(branch_line)
            .children(small(line3))
            .child(div().pt(px(2.)).flex().gap(px(2.)).children(buttons))
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
