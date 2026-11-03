//! 任务页：按列排开的卡片，拖拽换列、调先后，换了列的卡片飞到新位置，卡片上的 PR 那一行。

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::ui::{
    a11y::Press,
    badge::{Badge, BadgeVariant},
    button::{Button, ButtonSize, ButtonVariant},
    font_size::{self, FontSize, scaled},
    hsla,
    icons::{
        BRANCH_ICON, CHECK_ICON, DISCARD_ICON, GIT_ICON, PENCIL_ICON, PLUS_ICON, TERMINAL_ICON, TRASH_ICON,
        github_actions::{FAILURE_ICON, PENDING_ICON, SUCCESS_ICON},
    },
    scrollbar::scrollbar,
    tabs::{drag_chip, drop_marker},
    theme::{ADDED, AGENT_BLOCKED_COLOR, AGENT_DONE_COLOR, ActiveTheme as _, MODIFIED, REMOVED},
};
use gpui::{
    AccessibleAction, Animation, AnimationExt as _, AnyElement, App, Axis, Bounds, Context, Div, ElementId, FontWeight,
    Hsla, Pixels, Render, Role, SharedString, Stateful, Window, canvas, div, ease_out_quint, prelude::*, px, svg,
};
use runode_protocol::{
    ClientMsg,
    todo::{Column, DropAction, Todo, column, drop_action},
};
use runode_types::agent::Agent;

use super::{
    Kanban, KanbanEvent, Page, RING_WIDTH, card_shadow, column_name,
    dialog::Editing,
    history_link, icon_button, label_row, named, needs_you_badge,
    store::{self, Step, TodoData},
    when::{short_time, unix_now},
};
use crate::gh::{Checks, PullRequest};

/// 一列最窄这么宽（基准字号下的点数）；窗口放不下四列时横着滚。
const COLUMN_MIN_WIDTH: f32 = 168.;
/// 进行中那一列的圆点。
const IN_PROGRESS_COLOR: u32 = 0x3B82F6;
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

/// 按列分好的任务，列里保持宿主给的先后。
fn columns(data: &TodoData, now: u64) -> [Vec<&Todo>; 4] {
    let mut columns: [Vec<&Todo>; 4] = Default::default();
    for todo in &data.todos {
        let session = todo.session.and_then(|id| data.sessions.get(&id));
        columns[column_index(column(todo, session, now))].push(todo);
    }
    columns
}

impl Kanban {
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

    /// 和上一帧比，记下换了列的卡片，从上一帧画的位置起飞；飞完、描边褪完的清掉。
    pub(super) fn track_moves(&mut self, cx: &App) {
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

    pub(super) fn render_columns(&self, data: &TodoData, a11y: bool, cx: &mut Context<Self>) -> Div {
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
        // 窄的一栏里标题、计数、说明和链接挤不下一行，只列最近几天这句放在链接的悬停提示里。
        let window_hint = (column == Column::Done).then(|| {
            let window = rust_i18n::t!("kanban.done_window").into_owned();
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .justify_end()
                .text_size(FontSize::Small)
                .font_weight(FontWeight::NORMAL)
                .child(
                    history_link("todo-done-history")
                        .aria_description(window.clone())
                        .tooltip(crate::ui::tooltip::tooltip(window, None))
                        .on_press(cx, |this, _, cx| this.show_history(None, cx)),
                )
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
        let from = schedule.map(|s| rust_i18n::t!("kanban.from_schedule", name = s.spec.title.as_str()).into_owned());
        let footer: Vec<String> = [
            agent.is_none().then(|| todo.spec.agent.clone()),
            todo.started.map(|at| short_time(at, now)),
            from.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();
        // 卡片上只写时间，窄列里放得下；悬停时说清是什么时间。
        let footer_tip: Vec<String> = [
            agent.is_none().then(|| todo.spec.agent.clone()),
            todo.started.map(|at| rust_i18n::t!("kanban.started_at", time = short_time(at, now)).into_owned()),
            from,
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
            // 名字至少留这么宽：悬停时右上角多出几个按钮，`~` 这样的短名字照样整个露出来，长的露个开头。
            .child(div().min_w(scaled(20.)).truncate().child(place))
            .child(div().flex_1())
            .when(self.in_review(todo, data, now), |row| {
                row.child(Badge::new(rust_i18n::t!("kanban.in_review").into_owned()).variant(BadgeVariant::Secondary))
            })
            // 没有编号（0）时不写。
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
        let pull = self.pulls.get(&todo.id).map(|pr| pull_row(pr, &todo.id, cx));
        // 放不下时右边的按钮折到下一行靠右，不把 agent 和时间挤成省略号。
        let bottom = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .min_h(scaled(22.))
            .when_some(agent, |row, agent| row.child((self.marks.render)(agent, agent_id, cx)))
            .child(
                div()
                    .id(named("todo-footer", &todo.id))
                    .flex_grow(1.)
                    .min_w_0()
                    .truncate()
                    .text_size(FontSize::Small)
                    .text_color(muted)
                    .when(!footer_tip.is_empty(), |text| {
                        text.tooltip(crate::ui::tooltip::tooltip(footer_tip.join(" · "), None))
                    })
                    .child(footer.join(" · ")),
            )
            .children(primary.map(|primary| div().ml_auto().child(primary)));
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
            .children(pull)
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .line_clamp(2)
                    .font_weight(FontWeight::MEDIUM)
                    .child(todo.spec.title.clone()),
            )
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
        let place = self.place_name(&todo.spec.dir);
        let schedule = todo.schedule.as_ref().and_then(|id| data.schedules.iter().find(|s| &s.id == id));
        let status = agent.map_or_else(|| todo.spec.agent.clone(), self.marks.describe);
        let started =
            todo.started.map(|at| rust_i18n::t!("kanban.started_at", time = short_time(at, now)).into_owned());
        let from = schedule.map(|s| rust_i18n::t!("kanban.from_schedule", name = s.spec.title.as_str()).into_owned());
        let pull = self.pulls.get(&todo.id).map(pull_summary);
        let line2: Vec<String> = [Some(place), todo.branch.clone(), pull].into_iter().flatten().collect();
        let review = self.in_review(todo, data, now).then(|| rust_i18n::t!("kanban.in_review").into_owned());
        let line3: Vec<String> = [review, Some(status), started, from].into_iter().flatten().collect();
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
            if todo.session.is_some() && primary_id != Some("open") {
                let todo = todo.clone();
                let label = rust_i18n::t!("kanban.open").into_owned();
                buttons.push(
                    icon_button("todo-open", TERMINAL_ICON, label)
                        .on_press(cx, move |this, _, cx| this.open_todo(&todo, cx)),
                );
            }
            if todo.worktree_dir.is_some() && todo.branch.is_some() && !self.pulls.contains_key(&todo.id) {
                let creating = self.creating_pulls.contains(&todo.id);
                let todo = todo.clone();
                let label = rust_i18n::t!("kanban.create_pr").into_owned();
                buttons.push(
                    icon_button("todo-create-pr", GIT_ICON, label)
                        .disabled(creating)
                        .on_press(cx, move |this, _, cx| this.create_pull_request(&todo, cx)),
                );
            }
            if primary_id != Some("done") {
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
                    icon_button(button_id, icon, label)
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
        let corner = (!buttons.is_empty()).then(|| div().flex().gap(px(2.)).children(buttons));
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
}

/// 目录名首字母的小方块。
fn place_badge(place: &str) -> Div {
    let (initial, color) = crate::ui::avatar::letter_avatar(place);
    div()
        .flex_none()
        .size(scaled(16.))
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .bg(crate::ui::hsla(color))
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
}
