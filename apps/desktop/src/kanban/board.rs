//! 任务页：按列排开的卡片，拖拽换列、调先后，换了列的卡片飞到新位置，卡片上的 PR 那一行。

use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::ui::{
    a11y::{Press, StaticText as _},
    badge::{Badge, BadgeVariant},
    button::{Button, ButtonSize, ButtonVariant},
    font_size::{self, FontSize, scaled},
    hsla,
    icons::{
        BRANCH_ICON, CHECK_ICON, CLOCK_ICON, DISCARD_ICON, FOLDER_ICON, GIT_ICON, PENCIL_ICON, PLAY_ICON, PLUS_ICON,
        TERMINAL_ICON, TRASH_ICON,
        github_actions::{FAILURE_ICON, PENDING_ICON, SUCCESS_ICON},
    },
    scrollbar::scrollbar,
    status_mark::{MarkKind, status_mark},
    tabs::{drag_chip, drop_marker},
    theme::{ADDED, ActiveTheme as _, MODIFIED, REMOVED},
};
use gpui::{
    AccessibleAction, Animation, AnimationExt as _, AnyElement, App, Axis, Bounds, Context, Div, ElementId, FontWeight,
    Pixels, Render, Role, SharedString, Stateful, Window, canvas, div, ease_out_quint, prelude::*, px, relative, svg,
};
use runode_agent::prompt::Prompt;
use runode_protocol::{
    ClientMsg, SessionId,
    todo::{Column, DropAction, Outcome, Todo, column, drop_action},
};
use runode_types::agent::Agent;

use super::{
    Kanban, KanbanEvent, Page, RING_WIDTH, agent_text, card_shadow, column_name,
    dialog::Editing,
    history_link, icon_button, label_row, named,
    store::{self, Step, TodoData},
    when::{limit_text, limit_used, short_time, unix_now, until_next_minute},
};
use crate::{
    agent_prompts,
    gh::{Checks, PullRequest},
    git_panel::ago::{ago, instant_of, waited},
    host_client::hosts::HostId,
    i18n::action_for,
};

/// 一列最窄这么宽（基准字号下的点数）；窗口放不下四列时横着滚。
const COLUMN_MIN_WIDTH: f32 = 168.;
/// 列头状态标记的边长。
const COLUMN_ICON_SIZE: f32 = 14.;
/// 列的底往前景色偏多少：比页面略浮起一档，卡片（`Theme::surface`）再浮一档。
const COLUMN_TINT: f32 = 0.04;
/// 换列的卡片飞多久。
const FLY: Duration = Duration::from_millis(380);
/// 落下后淡入、描边褪掉要多久。
const LAND: Duration = Duration::from_millis(1200);
/// 飞着的时候新位置上留的影子有多透明。
const GHOST_OPACITY: f32 = 0.3;
/// 飞到这一段（占飞行时间的比例）以后，飞的卡片淡出、新位置上的影子淡入。
const CROSSFADE: f32 = 0.6;

/// 窗口坐标下的位置。
#[derive(Default)]
pub(super) struct Geometry {
    pub(super) board: Bounds<Pixels>,
    cards: HashMap<String, Bounds<Pixels>>,
}

/// 一张换了列的卡片。
pub(super) struct Move {
    /// 从哪儿飞来；拖着放下的为空，只描边不飞。
    from: Option<Bounds<Pixels>>,
    serial: u64,
    at: Instant,
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

/// 按列分好的任务，列里保持宿主给的先后；`place` 不为空时只要目录正好是它的任务。
fn columns<'a>(data: &'a TodoData, now: u64, place: Option<&Path>) -> [Vec<&'a Todo>; 4] {
    let mut columns: [Vec<&Todo>; 4] = Default::default();
    for todo in data.todos.iter().filter(|todo| place.is_none_or(|dir| todo.spec.dir == dir)) {
        let session = todo.session.and_then(|id| data.sessions.get(&id));
        columns[column_index(column(todo, session, now))].push(todo);
    }
    columns
}

/// 进行中那一列有几张卡片，不管看板按哪个项目筛选。
pub(super) fn running(data: &TodoData, now: u64) -> usize {
    columns(data, now, None)[column_index(Column::InProgress)].len()
}

impl Kanban {
    /// 按项目筛选后进行中那一列卡片的会话，按卡片的先后：卡片在这一列时会话一定还在。
    pub(super) fn running_sessions(&self, data: &TodoData) -> Vec<SessionId> {
        let grouped = columns(data, unix_now(), self.place_filter.as_deref());
        grouped[column_index(Column::InProgress)].iter().filter_map(|todo| todo.session).collect()
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
            None if action == DropAction::Reorder => Some(Self::after_column(to, self.place_filter.as_deref(), cx)),
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

    /// 排到 `column` 这一列（按 `place` 筛过）最后要挪到谁前面：所有任务里紧跟在这一列最后一张后面的那张，
    /// 没有时为空。
    fn after_column(column: Column, place: Option<&Path>, cx: &App) -> Option<String> {
        let data = store::data(cx);
        let last = columns(data, unix_now(), place)[column_index(column)].last().map(|todo| todo.id.clone())?;
        let ix = data.todos.iter().position(|todo| todo.id == last)?;
        data.todos.get(ix + 1).map(|todo| todo.id.clone())
    }

    /// 辅助工具调先后：在同一列里和上一张（`up`）或下一张换位置。
    fn nudge_todo(&mut self, id: &str, up: bool, cx: &mut Context<Self>) {
        let data = store::data(cx);
        let Some(column) = columns(data, unix_now(), self.place_filter.as_deref())
            .into_iter()
            .find(|cards| cards.iter().any(|todo| todo.id == id))
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

    /// 和上一帧比，记下换了列的卡片，从上一帧画的位置起飞；飞完、描边褪完的清掉。
    pub(super) fn track_moves(&mut self, cx: &App) {
        if self.page != Page::Todos {
            return;
        }
        let current: HashMap<String, Column> = Column::ALL
            .into_iter()
            .zip(columns(store::data(cx), unix_now(), self.place_filter.as_deref()))
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
    pub(super) fn render_flights(&self, data: &TodoData, cx: &mut Context<Self>) -> Vec<AnyElement> {
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
            let text = self.card_text(todo, data, now, cx);
            let face = self
                .card_face(todo, &text, data, now, named("todo-flight-agent", id), None, Vec::new(), cx)
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

    pub(super) fn render_columns(&self, data: &TodoData, a11y: bool, cx: &mut Context<Self>) -> Div {
        let now = unix_now();
        let grouped = columns(data, now, self.place_filter.as_deref());
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
            // 完成那一栏只列最近几天的，空着时照实说。
            let text =
                if column == Column::Done { rust_i18n::t!("kanban.empty_done") } else { rust_i18n::t!("kanban.empty") };
            // 待办空着时就地给个新建的入口，不用回侧栏找。
            let add = (column == Column::Todo).then(|| {
                Button::new("todo-column-new", rust_i18n::t!("kanban.new_todo").into_owned())
                    .variant(ButtonVariant::Ghost)
                    .with_size(ButtonSize::Sm)
                    .icon(PLUS_ICON)
                    .icon_size(12.)
                    .cursor_pointer()
                    .on_press(cx, |_, _, cx| cx.emit(KanbanEvent::NewTodo))
            });
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .child(label_row(("todo-column-empty", column_index(column)), text.into_owned(), fg.opacity(0.4)))
                .children(add)
        });
        let theme = cx.theme();
        let (muted, warning_text) = (theme.muted_foreground, theme.warning_text);
        // 需要你那一列有卡片时整列描一圈琥珀色的边、列名也用琥珀色。
        let waiting = column == Column::NeedsYou && !cards.is_empty();
        let border = if waiting { theme.warning.opacity(0.6) } else { theme.border };
        let tally = div().flex_none().text_color(muted).child(cards.len().to_string());
        // 列头右端：完成那一列是进记录页的链接，待办那一列是新建任务的按钮。
        let trailing = match column {
            Column::Done => {
                // 窄的一栏里标题、计数、说明和链接挤不下一行，只列最近几天这句放在链接的悬停提示里。
                let window = rust_i18n::t!("kanban.done_window").into_owned();
                Some(
                    history_link("todo-done-history", cx)
                        .aria_description(window.clone())
                        .tooltip(crate::ui::tooltip::tooltip(window, None))
                        .on_press(cx, |this, _, cx| this.show_history(None, cx))
                        .into_any_element(),
                )
            }
            Column::Todo => Some(
                icon_button("todo-column-add", PLUS_ICON, rust_i18n::t!("kanban.new_todo").into_owned())
                    .on_press(cx, |_, _, cx| cx.emit(KanbanEvent::NewTodo))
                    .into_any_element(),
            ),
            Column::InProgress | Column::NeedsYou => None,
        };
        let trailing = trailing.map(|trailing| {
            div().flex_1().min_w_0().flex().items_center().justify_end().text_size(FontSize::Small).child(trailing)
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
            .rounded(px(12.))
            .bg(cx.theme().mix(COLUMN_TINT))
            .border_1()
            .border_color(border)
            .child(
                div()
                    .flex_none()
                    .h(scaled(40.))
                    .px(px(14.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(column_icon(column, cards, data, cx))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .when(waiting, |name| name.text_color(warning_text))
                            .child(name),
                    )
                    .child(tally)
                    .children(trailing),
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

    /// 卡片上随列和状态变的几句话（`CardText`），画卡片和报给辅助工具共用。
    fn card_text(&self, todo: &Todo, data: &TodoData, now: u64, cx: &App) -> CardText {
        let session = todo.session.and_then(|id| data.sessions.get(&id));
        let column = column(todo, session, now);
        let agent = agent_text(&todo.spec.agent);
        let failed = todo.outcome == Some(Outcome::Failed);
        let prompt = todo
            .session
            .filter(|_| column == Column::NeedsYou && session.is_some() && !failed)
            .and_then(|id| agent_prompts::prompt(&(HostId::Local, id), cx));
        let when = match column {
            Column::NeedsYou => self.waiting_since(todo, data).map(|at| {
                let time = waited(Instant::now().saturating_duration_since(at).as_secs());
                (rust_i18n::t!("inbox.waited", time = time).into_owned(), true)
            }),
            Column::Done => todo.done_at.map(|at| (ago(i64::try_from(at).unwrap_or(i64::MAX)), false)),
            Column::Todo | Column::InProgress => None,
        };
        let footer = match column {
            _ if failed => rust_i18n::t!("kanban.not_finished", agent = agent),
            Column::InProgress if todo.worktree_dir.is_some() => rust_i18n::t!("kanban.in_worktree", agent = agent),
            Column::Done => match todo.started.zip(todo.done_at) {
                Some((started, done)) => {
                    rust_i18n::t!("kanban.took", agent = agent, time = waited(done.saturating_sub(started)))
                }
                None => agent.into(),
            },
            _ => agent.into(),
        }
        .into_owned();
        let limit = todo.spec.time_limit;
        let progress = limit.zip(todo.started).filter(|_| column == Column::InProgress).map(|(limit, started)| {
            let ran = now.saturating_sub(started);
            Progress {
                ran: rust_i18n::t!("kanban.ran_for", time = waited(ran)).into_owned(),
                limit: rust_i18n::t!("kanban.limit", time = limit_text(limit)).into_owned(),
                used: limit_used(ran, limit),
            }
        });
        let queued = (todo.queued && todo.started.is_none()).then(|| {
            // 等的任务不在看板上（早就完成了）时只写排队中。
            match todo.after.as_ref().and_then(|after| data.todos.iter().find(|other| &other.id == after)) {
                Some(after) => rust_i18n::t!("kanban.queued_after_line", number = after.number).into_owned(),
                None => rust_i18n::t!("kanban.queued").into_owned(),
            }
        });
        let excerpt = todo
            .session
            .filter(|_| column == Column::InProgress)
            .and_then(|id| self.excerpts.get(id))
            .map(<[String]>::to_vec);
        CardText {
            place: self.place_name(&todo.spec.dir),
            when,
            footer,
            limit: limit.filter(|_| column == Column::Todo).map(limit_text),
            progress,
            queued,
            failed,
            prompt,
            excerpt,
        }
    }

    /// 离卡片上下一处按分钟变的字（需要你的等了多久、进行中的时限进度）还有多久；没有这样的卡片时为空。
    pub(super) fn minute_wait(&self, data: &TodoData) -> Option<Duration> {
        let now = unix_now();
        let instant = Instant::now();
        let [_, running, needs_you, _] = columns(data, now, self.place_filter.as_deref());
        let ran = running
            .into_iter()
            .filter(|todo| todo.spec.time_limit.is_some())
            .filter_map(|todo| todo.started)
            .map(|started| until_next_minute(Duration::from_secs(now.saturating_sub(started))));
        let waits = needs_you
            .into_iter()
            .filter_map(|todo| self.waiting_since(todo, data))
            .map(|at| until_next_minute(instant.saturating_duration_since(at)));
        ran.chain(waits).min()
    }

    /// 需要你的卡片从什么时候起等：宿主报的 `SessionMeta::agent_since_ms`，没报时用 `Input::agent_changed`；
    /// 都没有时为空，卡片上不写等了多久。
    fn waiting_since(&self, todo: &Todo, data: &TodoData) -> Option<Instant> {
        let id = todo.session?;
        match data.sessions.get(&id).and_then(|session| session.meta.agent_since_ms) {
            Some(since) => Some(instant_of(since)),
            None => self.input.agent_changed.get(&id).copied(),
        }
    }

    /// 卡片的样子，看板上的和飞着的共用：第一行是目录，右边是等了多久或完成在多久以前和编号，`corner` 浮在
    /// 右上角；下面依次是分支、PR、标题、agent 的提问和回答的按钮、终端摘录、时限的进度、结果和排队的说法；最后一行是
    /// agent 怎么样了，`actions`
    /// 摆在右下角。`agent_id` 给 agent 标记用（它转圈要 id）。
    #[allow(clippy::too_many_arguments)]
    fn card_face(
        &self,
        todo: &Todo,
        text: &CardText,
        data: &TodoData,
        now: u64,
        agent_id: ElementId,
        corner: Option<Div>,
        actions: Vec<Button>,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme();
        let (bg, border, control_border) = (theme.surface, theme.border, theme.control_border);
        let (subtle, muted, warning_text) = (theme.subtle_foreground, theme.muted_foreground, theme.warning_text);
        let (success, destructive, info, track) = (theme.success, theme.destructive, theme.info, theme.mix(0.12));
        let well = theme.background;
        let mono = crate::ui::font::mono(cx);
        let session = todo.session.and_then(|id| data.sessions.get(&id));
        let agent = session.and_then(|info| info.meta.agent);
        let schedule = todo.schedule.as_ref().and_then(|id| data.schedules.iter().find(|s| &s.id == id));
        // 开始的时间和来自哪个定时不占卡片上的地方，悬停在最后一行上时说。
        let footer_tip: Vec<String> = [
            todo.started.map(|at| rust_i18n::t!("kanban.started_at", time = short_time(at, now)).into_owned()),
            schedule.map(|s| rust_i18n::t!("kanban.from_schedule", name = s.spec.title.as_str()).into_owned()),
        ]
        .into_iter()
        .flatten()
        .collect();
        // agent 报的结果（或者宿主读下的屏幕）只露第一行，悬停看全文；没做成的标时钟、红字，做完的标对勾。
        let result = todo.result.as_deref().and_then(|result| {
            let first = result.lines().find(|line| !line.trim().is_empty())?.trim().to_owned();
            let icon =
                if text.failed { Some((CLOCK_ICON, destructive)) } else { todo.done_at.map(|_| (CHECK_ICON, success)) };
            Some(
                div()
                    .id(named("todo-result", &todo.id))
                    .aria_static_text(first.clone())
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .text_color(if text.failed { destructive } else { subtle })
                    .tooltip(crate::ui::tooltip::tooltip(result.to_owned(), None))
                    .children(icon.map(|(icon, color)| svg().path(icon).flex_none().size(px(12.)).text_color(color)))
                    .child(div().min_w_0().truncate().child(first)),
            )
        });
        let top = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .h(scaled(20.))
            .text_size(FontSize::Body)
            .text_color(subtle)
            .child(svg().path(FOLDER_ICON).flex_none().size(px(12.)).text_color(subtle))
            // 名字至少留这么宽：悬停时右上角多出几个按钮，`~` 这样的短名字照样整个露出来，长的露个开头。
            .child(div().min_w(scaled(20.)).truncate().child(text.place.clone()))
            .child(div().flex_1())
            .when(self.in_review(todo, data, now), |row| {
                row.child(Badge::new(rust_i18n::t!("kanban.in_review").into_owned()).variant(BadgeVariant::Secondary))
            })
            .when_some(text.when.clone(), |row, (when, waiting)| {
                row.child(div().flex_none().when(waiting, |when| when.text_color(warning_text)).child(when))
            })
            // 没有编号（0）时不写。
            .when(todo.number > 0, |row| {
                row.child(div().flex_none().text_color(muted).child(format!("#{}", todo.number)))
            });
        // 分支单独一行，和目录挤在一行时窄列里两个都截得认不出。
        let branch = todo.branch.clone().map(|branch| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .min_w_0()
                .text_size(FontSize::Small)
                .text_color(subtle)
                .child(svg().path(BRANCH_ICON).flex_none().size(px(11.)).text_color(muted))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .when_some(mono.clone(), |name, mono| name.font_family(mono))
                        .child(branch),
                )
        });
        let pull = self.pulls.get(&todo.id).map(|pr| pull_row(pr, &todo.id, cx));
        let asked = text.prompt.as_ref().zip(todo.session).map(|(prompt, id)| {
            let open = todo.clone();
            agent_prompts::render_prompt(
                &(HostId::Local, id),
                prompt,
                &todo.spec.title,
                TERMINAL_ICON,
                rust_i18n::t!("kanban.open").into_owned(),
                move |_, _, cx| Self::open_session(id, &open, cx),
                cx,
            )
        });
        // 每行放不下时截断：摘录只让人看一眼 agent 在干什么，全文在终端里。
        let excerpt = text.excerpt.as_ref().map(|lines| {
            div()
                .id(named("todo-excerpt", &todo.id))
                .aria_static_text(lines.join("\n"))
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(6.))
                .bg(well)
                .border_1()
                .border_color(border)
                .text_size(FontSize::Small)
                .text_color(subtle)
                .when_some(mono.clone(), |excerpt, mono| excerpt.font_family(mono))
                .children(lines.iter().map(|line| div().min_w_0().truncate().child(line.clone())))
        });
        let progress = text.progress.as_ref().map(|progress| {
            div()
                .id(named("todo-progress", &todo.id))
                .role(Role::ProgressIndicator)
                .aria_label(rust_i18n::t!("kanban.dialog.time_limit").into_owned())
                .aria_value(format!("{}。{}", progress.ran, progress.limit))
                .aria_min_numeric_value(0.)
                .aria_max_numeric_value(1.)
                .aria_numeric_value(f64::from(progress.used))
                .flex()
                .flex_col()
                .gap(px(5.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_color(subtle)
                        .child(div().min_w_0().truncate().child(progress.ran.clone()))
                        .child(div().ml_auto().flex_none().child(progress.limit.clone())),
                )
                .child(
                    div()
                        .h(px(3.))
                        .rounded_full()
                        .bg(track)
                        .child(div().h_full().w(relative(progress.used)).rounded_full().bg(info)),
                )
        });
        let queued = text.queued.clone().map(|line| {
            div()
                .id(named("todo-queued", &todo.id))
                .aria_static_text(line.clone())
                .flex()
                .items_center()
                .gap(px(6.))
                .min_w_0()
                .text_color(subtle)
                .child(div().flex_none().child("↳"))
                .child(div().min_w_0().truncate().child(line))
        });
        let limit = text
            .limit
            .clone()
            .map(|limit| Badge::new(limit).variant(BadgeVariant::Secondary).icon(CLOCK_ICON).h(scaled(20.)).px(px(7.)));
        // 放不下时右边的按钮折到下一行靠右，不把 agent 挤成省略号。
        let bottom = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .min_h(scaled(26.))
            .when_some(agent, |row, agent| row.child((self.marks.render)(agent, agent_id, cx)))
            .child(
                div()
                    .id(named("todo-footer", &todo.id))
                    .min_w_0()
                    .truncate()
                    .text_color(subtle)
                    .when(!footer_tip.is_empty(), |footer| {
                        footer.tooltip(crate::ui::tooltip::tooltip(footer_tip.join("\n"), None))
                    })
                    .child(text.footer.clone()),
            )
            .children(limit)
            .when(!actions.is_empty(), |row| {
                row.child(div().ml_auto().flex().items_center().gap(px(6.)).children(actions))
            });
        // 悬停时的操作按钮浮在右上角、盖住编号和目录名，不占顶上那一行：窄列里三五个按钮排进去会把卡片撑破。
        let corner = corner.map(|corner| {
            corner
                .absolute()
                .top(px(6.))
                .right(px(8.))
                .p(px(2.))
                .rounded(px(6.))
                .bg(bg)
                .shadow(card_shadow(None, false))
        });
        div()
            .relative()
            .flex_none()
            .px(px(14.))
            .py(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(10.))
            .bg(bg)
            .border_1()
            .map(|card| match text.queued {
                Some(_) => card.border_dashed().border_color(control_border),
                None => card.border_color(border),
            })
            .child(top)
            .children(branch)
            .children(pull)
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .line_clamp(2)
                    .text_size(FontSize::Title)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(todo.spec.title.clone()),
            )
            .children(asked)
            .children(excerpt)
            .children(progress)
            .children(result)
            .children(queued)
            .child(bottom)
            .children(corner)
    }

    /// 待评审：在「需要你」那一列、分支上的 PR 开着、agent 也没在等回答，要用户去看的是 PR。
    fn in_review(&self, todo: &Todo, data: &TodoData, now: u64) -> bool {
        let session = todo.session.and_then(|id| data.sessions.get(&id));
        column(todo, session, now) == Column::NeedsYou
            && !session.and_then(|info| info.meta.agent).is_some_and(Agent::is_blocked)
            && self.pulls.get(&todo.id).is_some_and(|pr| pr.state == "OPEN" && !pr.is_draft)
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
        let text = self.card_text(todo, data, now, cx);
        let schedule = todo.schedule.as_ref().and_then(|id| data.schedules.iter().find(|s| &s.id == id));
        let started =
            todo.started.map(|at| rust_i18n::t!("kanban.started_at", time = short_time(at, now)).into_owned());
        let from = schedule.map(|s| rust_i18n::t!("kanban.from_schedule", name = s.spec.title.as_str()).into_owned());
        let review = self.in_review(todo, data, now).then(|| rust_i18n::t!("kanban.in_review").into_owned());
        let number = (todo.number > 0).then(|| format!("#{}", todo.number));
        let limit = text.limit.as_ref().map(|limit| rust_i18n::t!("kanban.limit", time = limit).into_owned());
        let progress = text.progress.as_ref().map(|progress| format!("{}。{}", progress.ran, progress.limit));
        let description = [
            number,
            Some(text.place.clone()),
            todo.branch.clone(),
            self.pulls.get(&todo.id).map(pull_summary),
            review,
            text.when.as_ref().map(|(when, _)| when.clone()),
            text.prompt
                .as_ref()
                .and_then(|prompt| prompt.question.last())
                .map(|question| rust_i18n::t!("prompts.asking", question = question).into_owned()),
            Some(text.footer.clone()),
            agent.map(self.marks.describe),
            limit,
            progress,
            text.queued.clone(),
            started,
            from,
            Some(rust_i18n::t!("kanban.reorder_hint").into_owned()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("。");
        let drops: [Option<DropAction>; 4] = Column::ALL.map(|to| drop_action(todo, session, to, now));
        let dragged =
            DraggedTodo { id: todo.id.clone(), from: column, ix, drops, title: todo.spec.title.clone().into() };
        let id = todo.id.clone();
        let open_session = todo.session.filter(|_| session.is_some());
        // 这一列最该做的事是右下角的按钮，别的收在右上角，鼠标移到卡片上（或者辅助工具开着）才露出来；右下角已经
        // 能打开终端、标为完成时右上角不再放。
        let mut actions: Vec<Button> = Vec::new();
        let (mut opens, mut finishes) = (false, false);
        // 辅助工具读到的按钮名带上卡片的标题，各张卡片上的同一种按钮才分得清。
        let title = todo.spec.title.as_str();
        let action = |button_id: &'static str, key: &str| {
            let label = rust_i18n::t!(key).into_owned();
            Button::new(button_id, label.clone()).aria_label(action_for(&label, title))
        };
        if todo.started.is_none() && todo.done_at.is_none() {
            let id = id.clone();
            actions.push(
                action("todo-start", "kanban.start")
                    .variant(ButtonVariant::Outline)
                    .with_size(ButtonSize::Sm)
                    .icon(PLAY_ICON)
                    .icon_size(10.)
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .on_press(cx, move |this, _, cx| this.start_todo(id.clone(), cx)),
            );
        } else if text.failed {
            if let Some(session_id) = open_session {
                let todo = todo.clone();
                actions.push(
                    action("todo-view-terminal", "kanban.view_terminal")
                        .variant(ButtonVariant::Ghost)
                        .with_size(ButtonSize::Sm)
                        .cursor_pointer()
                        .on_press(cx, move |_, _, cx| Self::open_session(session_id, &todo, cx)),
                );
            }
            let todo = todo.clone();
            actions.push(
                action("todo-continue", "kanban.continue")
                    .variant(ButtonVariant::Outline)
                    .with_size(ButtonSize::Sm)
                    .cursor_pointer()
                    .on_press(cx, move |this, _, cx| this.open_todo(&todo, cx)),
            );
            opens = true;
        } else if matches!(column, Column::InProgress | Column::NeedsYou) {
            match open_session {
                // 打开终端的按钮在提问的部件里。
                Some(_) if text.prompt.is_some() => opens = true,
                Some(session_id) => {
                    let todo = todo.clone();
                    actions.push(
                        action("todo-open", "kanban.open")
                            .variant(ButtonVariant::Outline)
                            .with_size(ButtonSize::Sm)
                            .cursor_pointer()
                            .on_press(cx, move |_, _, cx| Self::open_session(session_id, &todo, cx)),
                    );
                    opens = true;
                }
                None if column == Column::NeedsYou => {
                    let id = id.clone();
                    actions.push(
                        action("todo-done", "kanban.finish")
                            .variant(ButtonVariant::Outline)
                            .with_size(ButtonSize::Sm)
                            .cursor_pointer()
                            .on_press(cx, move |this, _, cx| this.finish_todo(id.clone(), true, cx)),
                    );
                    finishes = true;
                }
                None => {}
            }
        }
        let shown = a11y || self.hovered.as_ref() == Some(&todo.id);
        let mut buttons: Vec<Button> = Vec::new();
        if shown {
            if todo.session.is_some() && !opens {
                let todo = todo.clone();
                let label = rust_i18n::t!("kanban.open").into_owned();
                buttons.push(
                    icon_button("todo-open", TERMINAL_ICON, label.clone())
                        .aria_label(action_for(&label, title))
                        .on_press(cx, move |this, _, cx| this.open_todo(&todo, cx)),
                );
            }
            if todo.worktree_dir.is_some() && todo.branch.is_some() && !self.pulls.contains_key(&todo.id) {
                let creating = self.creating_pulls.contains(&todo.id);
                let todo = todo.clone();
                let label = rust_i18n::t!("kanban.create_pr").into_owned();
                buttons.push(
                    icon_button("todo-create-pr", GIT_ICON, label.clone())
                        .aria_label(action_for(&label, title))
                        .disabled(creating)
                        .on_press(cx, move |this, _, cx| this.create_pull_request(&todo, cx)),
                );
            }
            if !finishes {
                let done = todo.done_at.is_none();
                // 完成了的再按是重新打开，换成撤回的箭头，和「标为完成」的勾分开。
                let (button_id, icon, label) = if done {
                    ("todo-done", CHECK_ICON, rust_i18n::t!("kanban.finish"))
                } else {
                    ("todo-reopen", DISCARD_ICON, rust_i18n::t!("kanban.reopen"))
                };
                let label = label.into_owned();
                let id = id.clone();
                buttons.push(
                    icon_button(button_id, icon, label.clone())
                        .aria_label(action_for(&label, title))
                        .on_press(cx, move |this, _, cx| this.finish_todo(id.clone(), done, cx)),
                );
            }
            {
                let todo = todo.clone();
                let label = rust_i18n::t!("kanban.edit").into_owned();
                buttons.push(
                    icon_button("todo-edit", PENCIL_ICON, label.clone())
                        .aria_label(action_for(&label, title))
                        .on_press(cx, move |this, window, cx| {
                            this.open_dialog(false, Some(Editing::Todo(todo.clone())), window, cx)
                        }),
                );
            }
            {
                let id = id.clone();
                let label = rust_i18n::t!("kanban.remove_todo").into_owned();
                buttons.push(
                    icon_button("todo-remove", TRASH_ICON, label.clone())
                        .aria_label(action_for(&label, title))
                        .on_press(cx, move |this, window, cx| this.remove_todo(id.clone(), window, cx)),
                );
            }
        }
        let corner = (!buttons.is_empty()).then(|| div().flex().gap(px(2.)).children(buttons));
        let face = self.card_face(todo, &text, data, now, named("todo-agent", &todo.id), corner, actions, cx);
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
}

/// 卡片上随列和状态变的几句话。
struct CardText {
    /// 目录给人看的名字。
    place: String,
    /// 编号前面的时间，和它要不要用琥珀色：需要你的写等了多久，完成的写完成在多久以前。
    when: Option<(String, bool)>,
    /// 最后一行：agent，和它这一次干得怎么样。
    footer: String,
    /// 待办的卡片上写的时限。
    limit: Option<String>,
    /// 进行中、有时限的卡片上的进度。
    progress: Option<Progress>,
    /// 排着队的说法。
    queued: Option<String>,
    /// agent 报了没做成。
    failed: bool,
    /// 需要你的、会话还在的卡片上认出的 agent 提问。
    prompt: Option<Arc<Prompt>>,
    /// 进行中的卡片上，任务会话屏幕底部最后几行有字的输出；没读到时为空。
    excerpt: Option<Vec<String>>,
}

/// 时限的进度：跑了多久、时限多长，用掉了几成（0 到 1）。
struct Progress {
    ran: String,
    limit: String,
    used: f32,
}

/// 列头的标记：待办是空心圆，别的列是那一列状态的 `status_mark`。进行中的盲文格只在列里有 agent 在干活时转
/// （那时卡片上的标记已经在转），不给缓存着的看板添一个常驻的动画。
fn column_icon(column: Column, cards: &[&Todo], data: &TodoData, cx: &App) -> AnyElement {
    match column {
        Column::Todo => div()
            .flex_none()
            .size(px(COLUMN_ICON_SIZE))
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(12.)).rounded_full().border(px(1.5)).border_color(cx.theme().muted_foreground))
            .into_any_element(),
        Column::InProgress => {
            let spin = cards.iter().any(|todo| {
                let session = todo.session.and_then(|id| data.sessions.get(&id));
                session.and_then(|info| info.meta.agent).is_some_and(Agent::is_working)
            });
            status_mark("todo-column-working", MarkKind::Working { spin }, COLUMN_ICON_SIZE, cx)
        }
        Column::NeedsYou => {
            status_mark("todo-column-waiting", MarkKind::Waiting { blink: false }, COLUMN_ICON_SIZE, cx)
        }
        Column::Done => status_mark("todo-column-done", MarkKind::Done, COLUMN_ICON_SIZE, cx),
    }
}

/// PR 的说法：编号，不是开着的写状态，开着的写检查和评审。
fn pull_summary(pr: &PullRequest) -> String {
    let state = match pr.state.as_str() {
        "MERGED" => Some(rust_i18n::t!("kanban.pr.merged")),
        "CLOSED" => Some(rust_i18n::t!("kanban.pr.closed")),
        _ if pr.is_draft => Some(rust_i18n::t!("kanban.pr.draft")),
        _ => None,
    };
    let open = pr.state == "OPEN";
    let checks = match pr.checks() {
        Checks::Passing => Some(rust_i18n::t!("kanban.pr.checks_passing")),
        Checks::Failing => Some(rust_i18n::t!("kanban.pr.checks_failing")),
        Checks::Pending => Some(rust_i18n::t!("kanban.pr.checks_pending")),
        Checks::None => None,
    };
    let review = match pr.review_decision.as_str() {
        "APPROVED" => Some(rust_i18n::t!("kanban.pr.approved")),
        "CHANGES_REQUESTED" => Some(rust_i18n::t!("kanban.pr.changes_requested")),
        "REVIEW_REQUIRED" => Some(rust_i18n::t!("kanban.pr.review_required")),
        _ => None,
    };
    let number = rust_i18n::t!("kanban.pr.number", number = pr.number);
    [Some(number), state, checks.filter(|_| open), review.filter(|_| open)]
        .into_iter()
        .flatten()
        .map(|part| part.into_owned())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 卡片上 PR 那一行：检查的图标（开着时）和 `pull_summary`，按下在浏览器里打开 PR。
fn pull_row(pr: &PullRequest, todo: &str, cx: &mut Context<Kanban>) -> Stateful<Div> {
    let fg = cx.theme().foreground;
    let icon = match (pr.state.as_str(), pr.checks()) {
        ("OPEN", Checks::Passing) => svg().path(SUCCESS_ICON).text_color(hsla(ADDED)),
        ("OPEN", Checks::Failing) => svg().path(FAILURE_ICON).text_color(hsla(REMOVED)),
        ("OPEN", Checks::Pending) => svg().path(PENDING_ICON).text_color(hsla(MODIFIED)),
        _ => svg().path(GIT_ICON).text_color(fg.opacity(0.45)),
    };
    let summary = pull_summary(pr);
    let url = pr.url.clone();
    div()
        .id(named("todo-pr", todo))
        .role(Role::Link)
        .aria_label(rust_i18n::t!("kanban.pr.open_in_browser", pr = summary.as_str()).into_owned())
        .flex()
        .items_center()
        .gap(px(4.))
        .min_w_0()
        .text_size(FontSize::Small)
        .text_color(fg.opacity(0.55))
        .cursor_pointer()
        .hover(move |row| row.text_color(fg))
        .child(icon.flex_none().size(px(11.)))
        .child(div().min_w_0().truncate().child(summary))
        .on_press(cx, move |_, _, cx| cx.open_url(&url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kanban::tests::todo;

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

    /// 按项目筛选只留目录正好是它的任务（子目录里的不算），各列照旧分。
    #[test]
    fn columns_filter_by_place() {
        let mut elsewhere = todo("b", Some(1), None);
        elsewhere.spec.dir = "/other".into();
        let mut nested = todo("d", None, None);
        nested.spec.dir = "/src/sub".into();
        let data = TodoData {
            todos: vec![todo("a", None, None), elsewhere, todo("c", None, Some(5)), nested],
            ..TodoData::default()
        };
        let ids = |cards: &Vec<&Todo>| cards.iter().map(|todo| todo.id.clone()).collect::<Vec<_>>();
        let src = columns(&data, 1_000, Some(Path::new("/src")));
        assert_eq!(src.each_ref().map(ids), [vec!["a"], vec![], vec![], vec!["c"]]);
        let other = columns(&data, 1_000, Some(Path::new("/other")));
        assert_eq!(other.each_ref().map(ids), [vec![], vec![], vec!["b"], vec![]]);
        assert_eq!(columns(&data, 1_000, None).iter().map(Vec::len).sum::<usize>(), 4);
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
        let grouped = columns(&data, 1_000, None);
        assert_eq!(grouped.each_ref().map(ids), [vec!["a", "d"], vec![], vec!["b"], vec!["c"]]);
    }
}
