//! 记录页：完成了的任务一页一页向宿主要来，完成得晚的在前，可以只看一个定时开出的；只看一个带检查命令的定时时
//! 另有一段列它的检查记录（`runs`）。

use crate::i18n::action_for;
use crate::ui::{
    a11y::Press,
    button::{Button, ButtonSize, ButtonVariant},
    icons::{DISCARD_ICON, TERMINAL_ICON, TRASH_ICON},
    scrollbar::scrollbar,
    segmented::Segmented,
    theme::ActiveTheme as _,
};
use gpui::{App, Axis, Context, Div, FontWeight, Role, Stateful, div, prelude::*, px};
use runode_protocol::{
    ClientMsg, HostMsg,
    todo::{ScheduleRunRecord, Todo},
};
use std::collections::HashSet;

use super::{
    Kanban, Page, agent_badge, agent_text, detail_row, detail_text, dialog, icon_button, label_row, named,
    store::{self, TodoData},
};

/// 向宿主一页一页要来的一份记录。
pub(super) struct Pages<T> {
    pub(super) items: Vec<T>,
    /// 宿主说后面还有。
    pub(super) more: bool,
    /// 第一页回来了：没回来之前不说没有记录。
    pub(super) loaded: bool,
    /// 有一问还没回来：这时不翻下一页，从头重问的还没回来时照旧的条数往后翻会重了、漏了。
    loading: bool,
    pub(super) error: Option<String>,
    /// 第几次问，回来晚了的旧答案不要。
    serial: u64,
}

impl<T> Default for Pages<T> {
    fn default() -> Self {
        Self { items: Vec::new(), more: false, loaded: false, loading: false, error: None, serial: 0 }
    }
}

impl<T> Pages<T> {
    /// 清空重来；`serial` 接着数，清空前发出、还没回来的问答回来时才认得出是旧的。
    fn reset(&mut self) {
        *self = Self { serial: self.serial, ..Self::default() };
    }

    /// 要问下一页（`more`）或从头问：返回跳过几条和这一问的序号；翻下一页时上一问还没回来就为空。
    pub(super) fn start(&mut self, more: bool) -> Option<(u32, u64)> {
        if more && self.loading {
            return None;
        }
        let offset = if more { u32::try_from(self.items.len()).unwrap_or(u32::MAX) } else { 0 };
        self.serial += 1;
        self.loading = true;
        Some((offset, self.serial))
    }

    /// 第 `serial` 问回来了：`more` 是那一问是不是翻下一页，`answer` 是这一页和后面还有没有；回来晚了的不要。
    pub(super) fn finish(&mut self, serial: u64, more: bool, answer: Result<(Vec<T>, bool), String>) {
        if self.serial != serial {
            return;
        }
        self.loading = false;
        match answer {
            Ok((items, rest)) => {
                if !more {
                    self.items.clear();
                }
                self.items.extend(items);
                self.more = rest;
                self.loaded = true;
                self.error = None;
            }
            Err(err) => self.error = Some(err),
        }
    }
}

/// 记录页上看的是哪一段。
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum HistoryTab {
    /// 完成了的任务。
    #[default]
    Todos,
    /// 定时的检查记录。
    Runs,
}

/// 记录页翻到的记录，只在这个看板里用，不放进全 app 的数据。
#[derive(Default)]
pub(super) struct History {
    /// 只看这个定时的；为空时看全部完成了的任务。
    pub(super) schedule: Option<String>,
    /// 看的是哪一段；只在 `Kanban::runs_shown` 时算数，别的时候一律看完成了的任务。
    pub(super) tab: HistoryTab,
    /// 完成了的任务（`ClientMsg::ListDoneTodos`），完成得晚的在前。
    pub(super) todos: Pages<Todo>,
    /// 定时的检查记录（`ClientMsg::ListScheduleRuns`），新的在前。
    pub(super) runs: Pages<ScheduleRunRecord>,
    /// 展开了整段输出的检查记录（`ScheduleRunRecord::id`）。
    pub(super) expanded: HashSet<u64>,
}

impl Kanban {
    /// 换到记录页，只看定时 `schedule` 的（为空时看全部完成了的任务），从头翻。带检查命令的定时先看检查记录。
    pub(super) fn show_history(&mut self, schedule: Option<String>, cx: &mut Context<Self>) {
        let history = &mut self.history;
        history.schedule = schedule;
        history.todos.reset();
        history.runs.reset();
        history.expanded.clear();
        let runs = self.runs_shown(store::data(cx), cx);
        self.history.tab = if runs { HistoryTab::Runs } else { HistoryTab::Todos };
        self.set_page(Page::History, cx);
        self.load_history(false, cx);
    }

    /// 记录页上分不分段：只看一个定时、它有检查命令或跑过命令，连着的宿主也认得翻检查记录。
    pub(super) fn runs_shown(&self, data: &TodoData, cx: &App) -> bool {
        let Some(id) = &self.history.schedule else { return false };
        !store::host_below(cx, 6)
            && data
                .schedules
                .iter()
                .find(|schedule| &schedule.id == id)
                .is_some_and(|schedule| schedule.run.is_some() || schedule.last_run.is_some())
    }

    /// 现在看的是哪一段。
    fn history_tab(&self, cx: &App) -> HistoryTab {
        if self.runs_shown(store::data(cx), cx) { self.history.tab } else { HistoryTab::Todos }
    }

    /// 换到记录页的另一段，从头翻那一段。
    fn set_history_tab(&mut self, tab: HistoryTab, cx: &mut Context<Self>) {
        if self.history.tab == tab {
            return;
        }
        self.history.tab = tab;
        self.load_history(false, cx);
        cx.notify();
    }

    /// 看板变了、记录页上办成了事：开着记录页时从头重翻在看的那一段。
    /// shortcut: 翻过的几页丢掉、只重读第一页，翻得很深时改完会跳回顶上；嫌烦再按已翻的条数重读。
    pub(crate) fn reload_history(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::History {
            self.load_history(false, cx);
        }
    }

    /// 问宿主在看的那一段的下一页（`more`），或者从头问。
    fn load_history(&mut self, more: bool, cx: &mut Context<Self>) {
        match self.history_tab(cx) {
            HistoryTab::Todos => self.load_done_todos(more, cx),
            HistoryTab::Runs => self.load_runs(more, cx),
        }
    }

    /// 问宿主完成了的任务的下一页（`more`），或者从头问。
    fn load_done_todos(&mut self, more: bool, cx: &mut Context<Self>) {
        let Some((offset, serial)) = self.history.todos.start(more) else { return };
        let schedule = self.history.schedule.clone();
        let answer = store::ask(cx, Box::new(move |req| ClientMsg::ListDoneTodos { req, schedule, offset }));
        cx.spawn(async move |this, cx| {
            let answer = match answer.await {
                Ok(HostMsg::DoneTodos { todos, more, .. }) => Ok((todos, more)),
                Ok(other) => Err(format!("unexpected answer from the host: {other:?}")),
                Err(err) => Err(format!("{err:#}")),
            };
            this.update(cx, |this, cx| {
                this.history.todos.finish(serial, more, answer);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 记录页：只看一个带检查命令的定时时顶上分两段，检查记录和开出的任务；不然只有完成了的任务。
    pub(super) fn render_history(&self, data: &TodoData, title: String, cx: &mut Context<Self>) -> Div {
        let shown = self.runs_shown(data, cx);
        let tab = if shown { self.history.tab } else { HistoryTab::Todos };
        let tabs = shown.then(|| {
            Segmented::new("todo-history-tabs", rust_i18n::t!("kanban.history_tabs").into_owned())
                .option(
                    "todo-history-tab-runs",
                    rust_i18n::t!("kanban.runs_tab").into_owned(),
                    tab == HistoryTab::Runs,
                    cx,
                    |this, _, cx| this.set_history_tab(HistoryTab::Runs, cx),
                )
                .option(
                    "todo-history-tab-todos",
                    rust_i18n::t!("kanban.todos_tab").into_owned(),
                    tab == HistoryTab::Todos,
                    cx,
                    |this, _, cx| this.set_history_tab(HistoryTab::Todos, cx),
                )
                .flex_none()
                .self_start()
        });
        let body = match tab {
            HistoryTab::Todos => self.render_done_todos(data, title, cx),
            HistoryTab::Runs => self.render_runs(cx),
        };
        div().size_full().flex().flex_col().gap(px(8.)).children(tabs).child(body)
    }

    /// 完成了的任务一行一张，完成得晚的在前，翻到底下再要下一页。
    fn render_done_todos(&self, data: &TodoData, title: String, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let pages = &self.history.todos;
        let error = pages.error.clone().map(|error| label_row("todo-history-error", error, fg.opacity(0.75)));
        let rows: Vec<_> = pages.items.iter().map(|todo| self.render_done(todo, data, cx)).collect();
        let empty = (pages.loaded && rows.is_empty()).then(|| {
            label_row("todo-history-empty", rust_i18n::t!("kanban.no_history").into_owned(), fg.opacity(0.45))
        });
        let more = pages.more.then(|| {
            div().flex_none().flex().justify_center().child(
                Button::new("todo-history-more", rust_i18n::t!("kanban.load_more").into_owned())
                    .variant(ButtonVariant::Secondary)
                    .with_size(ButtonSize::Sm)
                    .cursor_pointer()
                    .on_press(cx, |this, _, cx| this.load_done_todos(true, cx)),
            )
        });
        div().flex_1().min_h_0().flex().flex_col().gap(px(8.)).children(error).child(
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
        // 各段和放不下时它截不截：编号和完成时间留全，项目、分支、来自哪个定时截断。
        let before = [
            ((todo.number > 0).then(|| format!("#{}", todo.number)), false),
            (Some(self.place_name(&todo.spec.dir)), true),
        ];
        let after = [
            (todo.branch.clone(), true),
            (todo.done_at.map(|at| rust_i18n::t!("kanban.done_at", time = dialog::full_time(at)).into_owned()), false),
            (schedule.map(|s| rust_i18n::t!("kanban.from_schedule", name = s.spec.title.as_str()).into_owned()), true),
        ];
        // 辅助工具读的说明里 agent 写名字，画出来的那一行 agent 是 logo。
        let details: Vec<String> = before
            .iter()
            .map(|(text, _)| text.clone())
            .chain([Some(agent_text(&todo.spec.agent))])
            .chain(after.iter().map(|(text, _)| text.clone()))
            .flatten()
            .collect();
        let part = |(text, shrinks): (Option<String>, bool)| {
            text.map(|text| if shrinks { detail_text(text) } else { div().flex_none().child(text).into_any_element() })
        };
        let color = fg.opacity(0.55);
        let line = detail_row(
            before
                .into_iter()
                .filter_map(part)
                .chain([agent_badge(&todo.spec.agent, color)])
                .chain(after.into_iter().filter_map(part)),
            color,
        );
        // 辅助工具读到的按钮名带上任务的标题，各条记录上的同一种按钮才分得清。
        let title = todo.spec.title.as_str();
        let mut buttons: Vec<Button> = Vec::new();
        if todo.session.is_some() {
            let todo = todo.clone();
            let label = rust_i18n::t!("kanban.open").into_owned();
            buttons.push(
                icon_button("todo-open", TERMINAL_ICON, label.clone())
                    .aria_label(action_for(&label, title))
                    .on_press(cx, move |this, _, cx| this.open_todo(&todo, cx)),
            );
        }
        {
            let id = todo.id.clone();
            let label = rust_i18n::t!("kanban.reopen").into_owned();
            buttons.push(
                icon_button("todo-reopen", DISCARD_ICON, label.clone())
                    .aria_label(action_for(&label, title))
                    .on_press(cx, move |this, _, cx| this.finish_todo(id.clone(), false, cx)),
            );
        }
        {
            let id = todo.id.clone();
            let label = rust_i18n::t!("kanban.remove_todo").into_owned();
            buttons.push(
                icon_button("todo-remove", TRASH_ICON, label.clone())
                    .aria_label(action_for(&label, title))
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
                    .child(line),
            )
            .child(div().flex_none().flex().items_start().gap(px(2.)).children(buttons))
    }
}
