//! 定时页：各个定时的规则、下次什么时候跑、最近开出的几张任务，编辑和删除。

use crate::i18n::action_for;
use crate::menus::OpenSettings;
use crate::ui::{
    a11y::{Press, StaticText as _},
    button::{Button, ButtonSize, ButtonVariant},
    font_size::{FontSize, scaled},
    icons::{BAN_ICON, CHEVRON_RIGHT_ICON, CLOCK_ICON, PENCIL_ICON, PLAY_ICON, PLUS_ICON, TRASH_ICON},
    scrollbar::scrollbar,
    theme::ActiveTheme as _,
};
use gpui::{AnyElement, Axis, Context, Div, FontWeight, Role, Stateful, Window, div, prelude::*, px, svg};
use runode_protocol::{
    ClientMsg,
    todo::{Schedule, Todo, column, holds_schedule},
};

use super::{
    Kanban, Page, ToastState, agent_text, column_name,
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

/// 下一个到点的定时：没停用、会再到点的里面 `next_fire` 最早的。
fn next_schedule(schedules: &[Schedule]) -> Option<&Schedule> {
    schedules
        .iter()
        .filter(|schedule| !schedule.paused)
        .filter_map(|schedule| schedule.next_fire.map(|at| (at, schedule)))
        .min_by_key(|(at, _)| *at)
        .map(|(_, schedule)| schedule)
}

impl Kanban {
    /// 任务页底下那条：下一个定时的名字、什么时候、规则、在哪、交给谁，右边进定时页；没有会再到点的定时时为空。
    pub(super) fn render_next_schedule(&self, data: &TodoData, cx: &mut Context<Self>) -> Option<Div> {
        let schedule = next_schedule(&data.schedules)?;
        let at = schedule.next_fire?;
        let theme = cx.theme();
        let (fg, subtle, muted, surface, border) =
            (theme.foreground, theme.subtle_foreground, theme.muted_foreground, theme.surface, theme.border);
        let heading = rust_i18n::t!("kanban.next_schedule").into_owned();
        let time = short_time(at, unix_now());
        let line = rust_i18n::t!(
            "kanban.schedule_line",
            repeat = describe_when(&schedule.when),
            place = self.place_name(&schedule.spec.dir),
            agent = agent_text(&schedule.spec.agent)
        )
        .into_owned();
        let spoken = [heading.as_str(), schedule.spec.title.as_str(), time.as_str(), line.as_str()].join("。");
        let all = rust_i18n::t!("kanban.all_schedules", count = data.schedules.len()).into_owned();
        let all = Button::new("todo-all-schedules", all.clone())
            .variant(ButtonVariant::Ghost)
            .with_size(ButtonSize::Sm)
            .flex_none()
            .ml_auto()
            .gap(px(3.))
            .cursor_pointer()
            .child(all)
            .child(svg().path(CHEVRON_RIGHT_ICON).flex_none().size(px(11.)).text_color(muted))
            .on_press(cx, |this, _, cx| this.set_page(Page::Schedules, cx));
        Some(
            div()
                .flex_none()
                .h(scaled(44.))
                .px(px(14.))
                .flex()
                .items_center()
                .gap(px(12.))
                .rounded(px(12.))
                .bg(surface)
                .border_1()
                .border_color(border)
                .text_color(subtle)
                .child(svg().path(CLOCK_ICON).flex_none().size(px(14.)).text_color(subtle))
                .child(
                    div()
                        .id("todo-next-schedule")
                        .aria_static_text(spoken)
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(heading))
                        .child(div().min_w_0().truncate().child(schedule.spec.title.clone()))
                        .child(div().flex_none().text_color(fg).child(time))
                        .child(div().min_w_0().truncate().child(line)),
                )
                .child(all),
        )
    }

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
        // 要打开的两个开关都在设置的头一页，打开设置就看得到。
        let hint = (!self.input.resident).then(|| {
            let open =
                Button::new("todo-schedules-settings", rust_i18n::t!("kanban.schedules_open_settings").into_owned())
                    .variant(ButtonVariant::Link)
                    .with_size(ButtonSize::Sm)
                    .cursor_pointer()
                    .on_press(cx, |_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx));
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(2.))
                .child(label_row(
                    "todo-schedules-hint",
                    rust_i18n::t!("kanban.schedules_need_host").into_owned(),
                    fg.opacity(0.6),
                ))
                .child(open)
        });
        let rows: Vec<_> =
            data.schedules.iter().map(|schedule| self.render_schedule(schedule, data, now, cx)).collect();
        // 空着时说清定时是做什么的，就地给个新建的入口。
        let empty = rows.is_empty().then(|| {
            let add = Button::new("todo-schedules-new", rust_i18n::t!("kanban.new_schedule").into_owned())
                .variant(ButtonVariant::Ghost)
                .with_size(ButtonSize::Sm)
                .icon(PLUS_ICON)
                .icon_size(12.)
                .cursor_pointer()
                .on_press(cx, |this, window, cx| this.open_dialog(true, None, window, cx));
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .child(label_row(
                    "todo-schedules-empty",
                    rust_i18n::t!("kanban.no_schedules").into_owned(),
                    fg.opacity(0.6),
                ))
                .child(div().max_w(scaled(320.)).text_center().child(label_row(
                    "todo-schedules-empty-detail",
                    rust_i18n::t!("kanban.no_schedules_detail").into_owned(),
                    fg.opacity(0.45),
                )))
                .child(add)
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
        let next = match schedule.next_fire {
            _ if schedule.paused => rust_i18n::t!("kanban.paused").into_owned(),
            Some(at) => rust_i18n::t!("kanban.next", time = short_time(at, now)).into_owned(),
            None => rust_i18n::t!("kanban.no_next").into_owned(),
        };
        // 宿主到点时看的是没完成的里面最后开出的那张。
        let held = schedule.next_fire.is_some()
            && data
                .todos
                .iter()
                .filter(|todo| todo.schedule.as_deref() == Some(schedule.id.as_str()) && todo.done_at.is_none())
                .max_by_key(|todo| todo.created)
                .is_some_and(|todo| holds_schedule(todo, todo.session.and_then(|id| data.sessions.get(&id)), now));
        let next = if held { format!("{next} · {}", rust_i18n::t!("kanban.next_held")) } else { next };
        let place = self.place_name(&schedule.spec.dir);
        let worktree = schedule.spec.worktree.then(|| rust_i18n::t!("kanban.worktree_on").into_owned());
        let details: Vec<String> =
            [Some(place), Some(agent_text(&schedule.spec.agent)), worktree].into_iter().flatten().collect();
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
        // 辅助工具读到的按钮名带上定时的标题，各个定时下面的同一种按钮才分得清。
        let title = schedule.spec.title.as_str();
        {
            let id = schedule.id.clone();
            run_rows.push(
                history_link(named("todo-schedule-history", &schedule.id), cx)
                    .aria_label(action_for(&rust_i18n::t!("kanban.all_history"), title))
                    .on_press(cx, move |this, _, cx| this.show_history(Some(id.clone()), cx))
                    .into_any_element(),
            );
        }
        let edit = {
            let schedule = schedule.clone();
            let label = rust_i18n::t!("kanban.edit_schedule").into_owned();
            icon_button("todo-schedule-edit", PENCIL_ICON, label.clone())
                .aria_label(action_for(&label, title))
                .on_press(cx, move |this, window, cx| {
                    this.open_dialog(true, Some(Editing::Schedule(schedule.clone())), window, cx)
                })
        };
        let pause = {
            let (id, paused) = (schedule.id.clone(), !schedule.paused);
            let (icon, label) = if paused {
                (BAN_ICON, rust_i18n::t!("kanban.pause_schedule").into_owned())
            } else {
                (PLAY_ICON, rust_i18n::t!("kanban.resume_schedule").into_owned())
            };
            let name = action_for(&label, title);
            icon_button("todo-schedule-pause", icon, label.clone()).aria_label(name).on_press(cx, move |this, _, cx| {
                if store::host_below(cx, 3) {
                    let text = rust_i18n::t!("kanban.dialog.host_too_old_for", what = label.as_str()).into_owned();
                    this.show_toast(ToastState::Failed(text), cx);
                    return;
                }
                let id = id.clone();
                Self::send_todos(vec![Box::new(move |req| ClientMsg::PauseSchedule { req, id, paused })], cx)
            })
        };
        let remove = {
            let id = schedule.id.clone();
            let label = rust_i18n::t!("kanban.remove_schedule").into_owned();
            icon_button("todo-schedule-remove", TRASH_ICON, label.clone())
                .aria_label(action_for(&label, title))
                .on_press(cx, move |this, window, cx| this.remove_schedule(id.clone(), window, cx))
        };
        // 停用的三行字各调淡一档，一眼和还在排的分开；下面的记录链接照样能点，不跟着淡。
        let (title_alpha, rule_alpha, details_alpha) = if schedule.paused { (0.6, 0.5, 0.4) } else { (1., 0.75, 0.55) };
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
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(fg.opacity(title_alpha))
                            .child(schedule.spec.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(FontSize::Small)
                            .text_color(fg.opacity(rule_alpha))
                            .child(format!("{rule} · {next}")),
                    )
                    .child(
                        div()
                            .text_size(FontSize::Small)
                            .text_color(fg.opacity(details_alpha))
                            .truncate()
                            .child(details.join(" · ")),
                    )
                    .child(div().pt(px(4.)).flex().flex_wrap().items_center().gap(px(4.)).children(run_rows)),
            )
            .child(div().flex_none().flex().items_start().gap(px(2.)).child(pause).child(edit).child(remove))
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

    /// 下一个定时跳过停用的和不会再到点的。
    #[test]
    fn next_schedule_skips_paused_and_finished() {
        let schedule = |id: &str, next_fire: Option<u64>, paused: bool| Schedule {
            id: id.into(),
            spec: todo(id, None, None).spec,
            when: runode_protocol::todo::When::Every { minutes: 60 },
            last_fired: 0,
            next_fire,
            paused,
        };
        let schedules = [
            schedule("paused", Some(10), true),
            schedule("over", None, false),
            schedule("later", Some(30), false),
            schedule("soon", Some(20), false),
        ];
        assert_eq!(next_schedule(&schedules).map(|s| s.id.as_str()), Some("soon"));
        assert_eq!(next_schedule(&schedules[..2]).map(|s| s.id.as_str()), None);
    }
}
