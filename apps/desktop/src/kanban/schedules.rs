//! 定时页：各个定时的规则、下次什么时候跑、最近开出的几张任务，编辑和删除。

use crate::ui::{
    a11y::Press,
    font_size::FontSize,
    icons::{PENCIL_ICON, TRASH_ICON},
    scrollbar::scrollbar,
    theme::ActiveTheme as _,
};
use gpui::{AnyElement, Axis, Context, Div, FontWeight, Role, Stateful, Window, div, prelude::*, px};
use runode_protocol::{
    ClientMsg,
    todo::{Schedule, Todo, column},
};

use super::{
    Kanban, Page, column_name,
    dialog::Editing,
    history_link, icon_button, label_row, named,
    store::{self, TodoData},
    when::{describe_when, run_label, short_time, unix_now},
};

/// 定时那一页里每个定时下面最多列它最近开出的几张任务。
const RECENT_RUNS: usize = 3;

/// 定时 `schedule` 最近开的 `RECENT_RUNS` 个任务，新的在前。按建的先后挑，不按看板上的先后：卡片拖动过就不一样了。
fn recent_runs<'a>(todos: &'a [Todo], schedule: &str) -> Vec<&'a Todo> {
    let mut runs: Vec<&Todo> = todos.iter().filter(|todo| todo.schedule.as_deref() == Some(schedule)).collect();
    runs.sort_by_key(|todo| std::cmp::Reverse(todo.created));
    runs.truncate(RECENT_RUNS);
    runs
}

impl Kanban {
    /// 删掉定时，先问一句：宿主那边是硬删，定时的提示词和规则找不回来。
    fn remove_schedule(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let data = store::data(cx);
        let Some(schedule) = data.schedules.iter().find(|schedule| schedule.id == id) else { return };
        let answer = crate::prompt::show(
            window,
            gpui::PromptLevel::Warning,
            &rust_i18n::t!("kanban.remove_title", title = schedule.spec.title.as_str()),
            Some(&rust_i18n::t!("kanban.remove_schedule_detail")),
            &[
                gpui::PromptButton::new(rust_i18n::t!("kanban.remove")),
                gpui::PromptButton::cancel(rust_i18n::t!("kanban.cancel")),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update(cx, |_, cx| {
                    Self::send_todos(vec![Box::new(move |req| ClientMsg::RemoveSchedule { req, id })], cx)
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn render_schedules(&self, data: &TodoData, cx: &mut Context<Self>) -> Div {
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
        let hover_bg = cx.theme().accent;
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
                .on_press(cx, move |this, window, cx| this.remove_schedule(id.clone(), window, cx))
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kanban::tests::todo;

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
}
