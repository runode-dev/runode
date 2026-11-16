//! 记录页的检查记录那一段：一个定时每次跑检查命令、到点让路没跑的结果，新的在前，一页一页向宿主要来。

use crate::i18n::action_for;
use crate::ui::{
    a11y::{Press, StaticText as _},
    button::{Button, ButtonSize, ButtonVariant},
    font_size::FontSize,
    icons::{CHECK_ICON, CHEVRON_DOWN_ICON, CHEVRON_UP_ICON, CLOCK_ICON, CLOSE_ICON, MINUS_ICON, ZAP_ICON},
    scrollbar::scrollbar,
    theme::ActiveTheme as _,
};
use gpui::{AccessibleAction, App, Axis, Context, Div, Role, Stateful, Window, div, prelude::*, px, svg};
use runode_protocol::{
    ClientMsg, HostMsg,
    todo::{RunOutcome, ScheduleRunRecord},
};

use super::{Kanban, Page, ToastState, dialog, icon_button, label_row, named, store};

/// 命令输出 `output` 的最后一个非空行，去掉两头的空白；都是空行时为空。
pub(super) fn last_line(output: &str) -> Option<&str> {
    output.lines().map(str::trim).rfind(|line| !line.is_empty())
}

/// 一条检查记录的结果怎么说：没事、开了哪张任务、失败、为什么跳过。
fn outcome_text(record: &ScheduleRunRecord, locale: &str) -> String {
    let number = record.number.filter(|number| *number > 0);
    match (record.run.outcome, record.todo.is_some(), number) {
        (RunOutcome::Idle, ..) => rust_i18n::t!("kanban.run_record_idle", locale = locale),
        (RunOutcome::Fired, true, Some(number)) => {
            rust_i18n::t!("kanban.run_record_fired", locale = locale, number = number)
        }
        (RunOutcome::Fired, true, None) => rust_i18n::t!("kanban.run_record_fired_unnumbered", locale = locale),
        (RunOutcome::Fired, false, _) => rust_i18n::t!("kanban.run_record_fired_gone", locale = locale),
        (RunOutcome::Failed, ..) => rust_i18n::t!("kanban.run_record_failed", locale = locale),
        (RunOutcome::Skipped, true, Some(number)) => {
            rust_i18n::t!("kanban.run_record_skipped", locale = locale, number = number)
        }
        (RunOutcome::Skipped, true, None) => rust_i18n::t!("kanban.run_record_skipped_unnumbered", locale = locale),
        (RunOutcome::Skipped, false, _) => rust_i18n::t!("kanban.run_record_skipped_running", locale = locale),
        (RunOutcome::Unknown, ..) => rust_i18n::t!("kanban.run_record_unknown", locale = locale),
    }
    .into_owned()
}

impl Kanban {
    /// 问宿主在看的定时的检查记录的下一页（`more`），或者从头问。
    pub(super) fn load_runs(&mut self, more: bool, cx: &mut Context<Self>) {
        let Some(schedule) = self.history.schedule.clone() else { return };
        let Some((offset, serial)) = self.history.runs.start(more) else { return };
        let answer = store::ask(cx, Box::new(move |req| ClientMsg::ListScheduleRuns { req, schedule, offset }));
        cx.spawn(async move |this, cx| {
            let answer = match answer.await {
                Ok(HostMsg::ScheduleRuns { runs, more, .. }) => Ok((runs, more)),
                Ok(other) => Err(format!("unexpected answer from the host: {other:?}")),
                Err(err) => Err(format!("{err:#}")),
            };
            this.update(cx, |this, cx| {
                this.history.runs.finish(serial, more, answer);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 展开、收起检查记录 `id` 的整段输出；`expand` 为空时切换。
    fn expand_run(&mut self, id: u64, expand: Option<bool>, cx: &mut Context<Self>) {
        let expanded = &mut self.history.expanded;
        let expand = expand.unwrap_or(!expanded.contains(&id));
        let changed = if expand { expanded.insert(id) } else { expanded.remove(&id) };
        if changed {
            cx.notify();
        }
    }

    /// 打开检查记录里提到的任务 `id`：开始过的打开它的终端，还没开始的回到看板上去找。
    fn open_run_todo(&mut self, id: String, cx: &mut Context<Self>) {
        let asked = store::ask(cx, Box::new(move |req| ClientMsg::GetTodo { req, id }));
        cx.spawn(async move |this, cx| {
            let answer = asked.await;
            this.update(cx, |this, cx| match answer {
                Ok(HostMsg::Todo { todo, .. }) if todo.started.is_some() => this.open_todo(&todo, cx),
                Ok(HostMsg::Todo { .. }) => this.set_page(Page::Todos, cx),
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

    /// 检查记录一行一条，新的在前，翻到底下再要下一页。
    pub(super) fn render_runs(&self, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let pages = &self.history.runs;
        let error = pages.error.clone().map(|error| label_row("todo-runs-error", error, fg.opacity(0.75)));
        let rows: Vec<_> = pages.items.iter().map(|record| self.render_run(record, cx)).collect();
        let empty = (pages.loaded && rows.is_empty())
            .then(|| label_row("todo-runs-empty", rust_i18n::t!("kanban.no_runs").into_owned(), fg.opacity(0.45)));
        let more = pages.more.then(|| {
            div().flex_none().flex().justify_center().child(
                Button::new("todo-runs-more", rust_i18n::t!("kanban.load_more").into_owned())
                    .variant(ButtonVariant::Secondary)
                    .with_size(ButtonSize::Sm)
                    .cursor_pointer()
                    .on_press(cx, |this, _, cx| this.load_runs(true, cx)),
            )
        });
        div().flex_1().min_h_0().flex().flex_col().gap(px(8.)).children(error).child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("todo-runs")
                        .role(Role::List)
                        .aria_label(rust_i18n::t!("kanban.runs_tab").into_owned())
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.runs_scroll)
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .children(rows)
                        .children(empty)
                        .children(more),
                )
                .child(scrollbar("todo-runs-scrollbar", self.runs_scroll.clone(), Axis::Vertical)),
        )
    }

    /// 一条检查记录：左边的图标说结果，右边是时间和结果，下面是输出的最后一行（展开时是整段）；开出、挡住它的
    /// 任务还在时能打开那张任务，输出不止一行时能展开。
    fn render_run(&self, record: &ScheduleRunRecord, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let (fg, destructive, row_bg) = (theme.foreground, theme.destructive, theme.mix(0.03));
        let outcome = record.run.outcome;
        let failed = outcome == RunOutcome::Failed;
        let heading = format!("{} · {}", dialog::full_time(record.run.at), outcome_text(record, &rust_i18n::locale()));
        let output = record.run.output.trim_start_matches(['\n', '\r']).trim_end();
        let last = last_line(output);
        let multiline = output.lines().filter(|line| !line.trim().is_empty()).count() > 1;
        let expanded = multiline && self.history.expanded.contains(&record.id);
        let icon = match outcome {
            RunOutcome::Idle => CHECK_ICON,
            RunOutcome::Fired => ZAP_ICON,
            RunOutcome::Failed => CLOSE_ICON,
            RunOutcome::Skipped => MINUS_ICON,
            RunOutcome::Unknown => CLOCK_ICON,
        };
        // 图标和输出同色，失败的标红。
        let color = if failed { destructive } else { fg.opacity(0.55) };
        let output = if expanded {
            Some(
                div()
                    .id("todo-run-output")
                    .aria_static_text(output.to_owned())
                    .text_size(FontSize::Small)
                    .text_color(color)
                    .child(output.to_owned())
                    .into_any_element(),
            )
        } else {
            last.map(|line| {
                div().text_size(FontSize::Small).text_color(color).truncate().child(line.to_owned()).into_any_element()
            })
        };
        let mut buttons: Vec<Button> = Vec::new();
        let names_todo = matches!(outcome, RunOutcome::Fired | RunOutcome::Skipped);
        if let Some(id) = record.todo.clone().filter(|_| names_todo) {
            buttons.push(
                Button::new("todo-run-open", rust_i18n::t!("kanban.open_run_todo").into_owned())
                    .aria_label(action_for(&rust_i18n::t!("kanban.open_run_todo"), &heading))
                    .variant(ButtonVariant::Ghost)
                    .with_size(ButtonSize::Sm)
                    .cursor_pointer()
                    .on_press(cx, move |this, _, cx| this.open_run_todo(id.clone(), cx)),
            );
        }
        if multiline {
            let id = record.id;
            let a11y = |expand: bool, cx: &mut Context<Self>| {
                let view = cx.entity().downgrade();
                move |_: Option<&gpui::accesskit::ActionData>, _: &mut Window, cx: &mut App| {
                    view.update(cx, |this, cx| this.expand_run(id, Some(expand), cx)).ok();
                }
            };
            let (icon, tip) = if expanded {
                (CHEVRON_UP_ICON, rust_i18n::t!("kanban.hide_output"))
            } else {
                (CHEVRON_DOWN_ICON, rust_i18n::t!("kanban.show_output"))
            };
            // 名字不跟着展开收起变，状态由 `aria_expanded` 报。
            buttons.push(
                icon_button("todo-run-expand", icon, tip.into_owned())
                    .aria_label(action_for(&rust_i18n::t!("kanban.run_output"), &heading))
                    .aria_expanded(expanded)
                    .on_a11y_action(AccessibleAction::Expand, a11y(true, cx))
                    .on_a11y_action(AccessibleAction::Collapse, a11y(false, cx))
                    .on_press(cx, move |this, _, cx| this.expand_run(id, None, cx)),
            );
        }
        div()
            .id(named("todo-run", &record.id.to_string()))
            .role(Role::ListItem)
            .aria_label(heading.clone())
            .when_some(last, |row, line| row.aria_description(line.to_owned()))
            .flex_none()
            .p(px(12.))
            .flex()
            .items_start()
            .gap(px(10.))
            .rounded(px(8.))
            .bg(row_bg)
            .child(div().flex_none().pt(px(3.)).child(svg().path(icon).size(px(14.)).text_color(color)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(div().truncate().child(heading))
                    .children(output),
            )
            .child(div().flex_none().flex().items_center().gap(px(2.)).children(buttons))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runode_protocol::todo::ScheduleRun;

    /// 结果文案按结果、任务还在不在、有没有编号挑。
    #[test]
    fn outcome_texts_tell_what_happened() {
        let record = |outcome, todo: Option<&str>, number: Option<u64>| ScheduleRunRecord {
            id: 1,
            run: ScheduleRun { at: 1_800_000_000, outcome, output: String::new() },
            todo: todo.map(str::to_owned),
            number,
        };
        let text = |record: ScheduleRunRecord| outcome_text(&record, "zh-Hans");
        assert_eq!(text(record(RunOutcome::Idle, None, None)), "没事");
        assert_eq!(text(record(RunOutcome::Fired, Some("t"), Some(12))), "开了任务 #12");
        assert_eq!(text(record(RunOutcome::Fired, None, None)), "开了任务（已删除）");
        assert_eq!(text(record(RunOutcome::Failed, None, None)), "失败");
        assert_eq!(text(record(RunOutcome::Skipped, Some("t"), Some(3))), "跳过：任务 #3 还没做完");
        assert_eq!(text(record(RunOutcome::Skipped, None, None)), "跳过：上次的检查或任务还没结束");
        assert_eq!(text(record(RunOutcome::Skipped, Some("t"), None)), "跳过：上次开出的任务还没做完");
        assert_eq!(outcome_text(&record(RunOutcome::Fired, Some("t"), Some(12)), "en"), "Opened task #12");
    }

    /// 输出的最后一行跳过末尾的空行、去掉两头的空白。
    #[test]
    fn last_lines_skip_blank_tails() {
        assert_eq!(last_line("checking\n  gh: auth required \n\n"), Some("gh: auth required"));
        assert_eq!(last_line(" \n"), None);
    }
}
