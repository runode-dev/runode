//! 记录页：完成了的任务一页一页向宿主要来，完成得晚的在前，可以只看一个定时开出的。

use crate::ui::{
    a11y::Press,
    button::{Button, ButtonSize, ButtonVariant},
    font_size::FontSize,
    icons::{DISCARD_ICON, TERMINAL_ICON, TRASH_ICON},
    scrollbar::scrollbar,
    theme::ActiveTheme as _,
};
use gpui::{Axis, Context, Div, FontWeight, Role, Stateful, div, prelude::*, px};
use runode_protocol::{ClientMsg, HostMsg, todo::Todo};

use super::{
    Kanban, Page, dialog, icon_button, label_row, named,
    store::{self, TodoData},
};

/// 记录页翻到的完成了的任务（`ClientMsg::ListDoneTodos`），只在这个看板里用，不放进全 app 的数据。
#[derive(Default)]
pub(super) struct History {
    /// 只看这个定时开出的；为空时看全部。
    pub(super) schedule: Option<String>,
    /// 完成得晚的在前。
    pub(super) todos: Vec<Todo>,
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

impl Kanban {
    /// 换到记录页，只看定时 `schedule` 开出的（为空时看全部），从头翻。
    pub(super) fn show_history(&mut self, schedule: Option<String>, cx: &mut Context<Self>) {
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

    /// 记录页：完成了的任务一行一张，完成得晚的在前，翻到底下再要下一页。
    pub(super) fn render_history(&self, data: &TodoData, title: String, cx: &mut Context<Self>) -> Div {
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
        if todo.session.is_some() {
            let todo = todo.clone();
            let label = rust_i18n::t!("kanban.open").into_owned();
            buttons.push(
                icon_button("todo-open", TERMINAL_ICON, label)
                    .on_press(cx, move |this, _, cx| this.open_todo(&todo, cx)),
            );
        }
        {
            let id = todo.id.clone();
            let label = rust_i18n::t!("kanban.reopen").into_owned();
            buttons.push(
                icon_button("todo-reopen", DISCARD_ICON, label)
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
