//! 定时页：各个定时的规则、下次什么时候跑、最近开出的几张任务，编辑和删除。

use crate::i18n::action_for;
use crate::menus::OpenSettings;
use crate::ui::{
    a11y::{Press, StaticText as _},
    button::{Button, ButtonSize, ButtonVariant},
    font_size::{FontSize, scaled},
    icons::{
        BAN_ICON, CHEVRON_RIGHT_ICON, CLOCK_ICON, FOLDER_ICON, PENCIL_ICON, PLAY_ICON, PLUS_ICON, TRASH_ICON, ZAP_ICON,
    },
    scrollbar::scrollbar,
    theme::ActiveTheme as _,
};
use gpui::{AnyElement, Axis, Context, Div, FontWeight, Role, Stateful, Window, div, prelude::*, px, svg};
use runode_protocol::{
    ClientMsg,
    todo::{RunOutcome, Schedule, Todo, column, holds_schedule},
};
use std::path::{Path, PathBuf};

use super::{
    Kanban, Page, TOAST_LINGER, ToastState, agent_badge, agent_text, column_name,
    dialog::Editing,
    history_link, icon_button, label_row, named,
    runs::last_line,
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

/// 定时按跑在哪个目录分组：是 `workspaces` 之一的按它在里面的先后排在前面，别的目录按路径排在后面；组里照原来的先后。
fn group_by_place<'a>(schedules: &'a [Schedule], workspaces: &[PathBuf]) -> Vec<(&'a Path, Vec<&'a Schedule>)> {
    let mut groups: Vec<(&Path, Vec<&Schedule>)> = Vec::new();
    for schedule in schedules {
        let dir = schedule.spec.dir.as_path();
        match groups.iter_mut().find(|(place, _)| *place == dir) {
            Some((_, group)) => group.push(schedule),
            None => groups.push((dir, vec![schedule])),
        }
    }
    groups.sort_by_key(|(dir, _)| match workspaces.iter().position(|workspace| workspace == dir) {
        Some(at) => (0, at, PathBuf::new()),
        None => (1, 0, dir.to_path_buf()),
    });
    groups
}

/// 交给它们跑的是后面的脚本，卡片上写脚本的名字。
const INTERPRETERS: &[&str] = &["sh", "bash", "zsh", "dash", "powershell", "pwsh", "python", "python3", "node"];

/// 命令 `command` 跑的程序的文件名（去掉引号和目录）：第一个词是解释器时换成它跑的脚本，即 `-File` 后面的词，
/// 没有 `-File` 时是第一个不以 `-` 开头的词。
fn program_name(command: &str) -> String {
    let words = command_words(command);
    let file_name = |word: &str| Path::new(word).file_name().and_then(|name| name.to_str()).unwrap_or(word).to_owned();
    let Some(first) = words.first() else { return String::new() };
    let program = file_name(first);
    let stem = program.strip_suffix(".exe").unwrap_or(&program).to_ascii_lowercase();
    if !INTERPRETERS.contains(&stem.as_str()) {
        return program;
    }
    let rest = &words[1..];
    // `-c`（含 `-ec` 这样合写的短参数）、`-Command`、node 的 `-e`、`-p` 后面是一段脚本正文，不是文件，
    // 写不出名字，写解释器本身。
    let inline = |word: &String| {
        let short = word
            .strip_prefix('-')
            .filter(|flags| !flags.is_empty() && flags.len() <= 4 && flags.chars().all(|ch| ch.is_ascii_lowercase()));
        short.is_some_and(|flags| flags.contains('c') || (stem == "node" && (flags == "e" || flags == "p")))
            || word.eq_ignore_ascii_case("-Command")
    };
    if rest.iter().any(inline) {
        return program;
    }
    let script = match rest.iter().position(|word| word.eq_ignore_ascii_case("-File")) {
        Some(at) => rest.get(at + 1),
        None => rest.iter().find(|word| !word.starts_with('-')),
    };
    script.map_or(program, |script| file_name(script))
}

/// `command` 按空白分成的词，单引号或双引号括起的部分算在一个词里、去掉引号。
fn command_words(command: &str) -> Vec<String> {
    let (mut words, mut word, mut quote, mut started) = (Vec::new(), String::new(), None, false);
    for ch in command.chars() {
        match (quote, ch) {
            (Some(open), _) if ch == open => quote = None,
            (Some(_), _) => word.push(ch),
            (None, '"' | '\'') => (quote, started) = (Some(ch), true),
            (None, _) if ch.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, _) => (started, _) = (true, word.push(ch)),
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// 定时卡片上先跑的命令那一行：跑哪个程序，正在跑的接上「正在跑」，跑过的再接上一次的结果；为真的是上一次失败了。
/// 没有命令时为空。
fn run_line_in(schedule: &Schedule, now: u64, locale: &str) -> Option<(String, bool)> {
    let command = schedule.run.as_deref()?;
    let program = rust_i18n::t!("kanban.run_command", locale = locale, program = program_name(command)).into_owned();
    if schedule.running {
        return Some((format!("{program} · {}", rust_i18n::t!("kanban.run_running", locale = locale)), false));
    }
    let Some(last) = &schedule.last_run else { return Some((program, false)) };
    let time = short_time(last.at, now);
    let output = last_line(&last.output);
    let result = match (last.outcome, output) {
        (RunOutcome::Idle, _) => rust_i18n::t!("kanban.run_idle", locale = locale, time = time),
        (RunOutcome::Fired, _) => rust_i18n::t!("kanban.run_fired", locale = locale, time = time),
        (RunOutcome::Failed, Some(output)) => {
            rust_i18n::t!("kanban.run_failed", locale = locale, time = time, output = output)
        }
        (RunOutcome::Failed, None) => rust_i18n::t!("kanban.run_failed_bare", locale = locale, time = time),
        (RunOutcome::Skipped | RunOutcome::Unknown, _) => {
            rust_i18n::t!("kanban.run_unknown", locale = locale, time = time)
        }
    };
    Some((format!("{program} · {result}"), last.outcome == RunOutcome::Failed))
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

    /// 请宿主立即触发一次定时 `id`，不改它下次到点的时刻；`program` 是它先跑的程序名，没有时为空。
    /// 办成了弹出提示条，过一会儿收起。
    fn fire_schedule_now(&mut self, id: String, program: Option<String>, cx: &mut Context<Self>) {
        let sent = store::send(cx, vec![Box::new(move |req| ClientMsg::FireSchedule { req, id })]);
        cx.spawn(async move |this, cx| {
            if let Err(reason) = sent.await {
                let text = rust_i18n::t!("kanban.toast.request_failed", reason = reason).into_owned();
                this.update(cx, |this, cx| this.show_toast(ToastState::Failed(text), cx)).ok();
                return;
            }
            let text = match program {
                Some(program) => rust_i18n::t!("kanban.toast.fired_run", program = program),
                None => rust_i18n::t!("kanban.toast.fired"),
            }
            .into_owned();
            let Ok(serial) = this.update(cx, |this, cx| this.show_toast(ToastState::Done(text), cx)) else { return };
            cx.background_executor().timer(TOAST_LINGER).await;
            this.update(cx, |this, cx| this.set_toast(serial, None, cx)).ok();
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
                .w_full()
                .max_w(scaled(880.))
                .mx_auto()
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
        let workspaces: Vec<PathBuf> = self.input.workspaces.iter().map(|(dir, _)| dir.clone()).collect();
        let groups = group_by_place(&data.schedules, &workspaces);
        // 定时都在一个项目里时不分组，整页是一个列表，项目名写在各个定时的说明里；多个项目时每个项目一组：
        // 组头写项目名，下面是这个项目自己的列表。
        let grouped = groups.len() > 1;
        let muted = cx.theme().muted_foreground;
        let mut rows: Vec<AnyElement> = Vec::new();
        for (dir, schedules) in &groups {
            let items = schedules.iter().map(|schedule| self.render_schedule(schedule, data, now, !grouped, cx));
            if !grouped {
                rows.extend(items.map(IntoElement::into_any_element));
                continue;
            }
            let name = self.place_name(dir);
            let count = rust_i18n::t!("kanban.schedule_count", count = schedules.len()).into_owned();
            let key = dir.to_string_lossy();
            let heading = div()
                .id(named("todo-schedule-group-heading", &key))
                .role(Role::Heading)
                .aria_level(2)
                .aria_label(format!("{name} · {count}"))
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(4.))
                .child(svg().path(FOLDER_ICON).flex_none().size(px(13.)).text_color(muted))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(FontSize::Body)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .child(name.clone()),
                )
                .child(div().flex_none().text_size(FontSize::Small).text_color(muted).child(count));
            let list = div()
                .id(named("todo-schedule-group-list", &key))
                .role(Role::List)
                .aria_label(name.clone())
                .flex()
                .flex_col()
                .gap(px(8.))
                .children(items);
            rows.push(
                div()
                    .id(named("todo-schedule-group", &key))
                    .role(Role::Group)
                    .aria_label(name)
                    .when(!rows.is_empty(), |group| group.pt(px(12.)))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(heading)
                    .child(list)
                    .into_any_element(),
            );
        }
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
                        .role(if grouped { Role::Group } else { Role::List })
                        .aria_label(rust_i18n::t!("kanban.schedules").into_owned())
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.schedules_scroll)
                        .flex()
                        .flex_col()
                        .children((!rows.is_empty()).then(|| {
                            // 宽窗口里卡片不铺满：标题、下一次的时间和右边的按钮离得太远，眼睛要横扫整屏。
                            div().w_full().max_w(scaled(880.)).mx_auto().flex().flex_col().gap(px(8.)).children(rows)
                        }))
                        .children(empty),
                )
                .child(scrollbar("todo-schedules-scrollbar", self.schedules_scroll.clone(), Axis::Vertical)),
        )
    }

    /// `show_place` 为真时说明里写上项目名。
    fn render_schedule(
        &self,
        schedule: &Schedule,
        data: &TodoData,
        now: u64,
        show_place: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let fg = cx.theme().foreground;
        let hover_bg = cx.theme().accent;
        let rule = describe_when(&schedule.when);
        let next = match schedule.next_fire {
            _ if schedule.paused => rust_i18n::t!("kanban.paused").into_owned(),
            Some(at) => rust_i18n::t!("kanban.next", time = short_time(at, now)).into_owned(),
            None => rust_i18n::t!("kanban.no_next").into_owned(),
        };
        // 宿主到点和立即触发时看的都是没完成的里面最后开出的那张。
        let last_holds = data
            .todos
            .iter()
            .filter(|todo| todo.schedule.as_deref() == Some(schedule.id.as_str()) && todo.done_at.is_none())
            .max_by_key(|todo| todo.created)
            .is_some_and(|todo| holds_schedule(todo, todo.session.and_then(|id| data.sessions.get(&id)), now));
        let held = schedule.next_fire.is_some() && last_holds;
        let held_text = held.then(|| rust_i18n::t!("kanban.next_held").into_owned());
        let place = show_place.then(|| self.place_name(&schedule.spec.dir));
        let worktree = schedule.spec.worktree.then(|| rust_i18n::t!("kanban.worktree_on").into_owned());
        let details: Vec<String> =
            [place.clone(), Some(agent_text(&schedule.spec.agent)), worktree.clone()].into_iter().flatten().collect();
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
                    .px(px(6.))
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
        // 宿主这时会回绝立即触发：灰着，提示里说为什么。停用的照样能触发。
        let fire = {
            let label = rust_i18n::t!("kanban.fire_schedule").into_owned();
            let busy = if last_holds {
                Some(rust_i18n::t!("kanban.fire_schedule_held").into_owned())
            } else if schedule.running {
                Some(rust_i18n::t!("kanban.fire_schedule_running").into_owned())
            } else {
                None
            };
            let disabled = busy.is_some();
            let mut button = icon_button("todo-schedule-fire", ZAP_ICON, busy.clone().unwrap_or_else(|| label.clone()))
                .aria_label(action_for(&label, title))
                .disabled(disabled);
            // 读屏的人看不到提示，灰着的原因另报给辅助工具。
            if let Some(why) = busy {
                button = button.aria_description(why);
            }
            if disabled {
                button
            } else {
                let id = schedule.id.clone();
                let program = schedule.run.as_deref().map(program_name);
                button.on_press(cx, move |this, _, cx| {
                    if store::host_below(cx, 5) {
                        let text = rust_i18n::t!("kanban.dialog.host_too_old_for", what = label.as_str()).into_owned();
                        this.show_toast(ToastState::Failed(text), cx);
                        return;
                    }
                    this.fire_schedule_now(id.clone(), program.clone(), cx)
                })
            }
        };
        let remove = {
            let id = schedule.id.clone();
            let label = rust_i18n::t!("kanban.remove_schedule").into_owned();
            icon_button("todo-schedule-remove", TRASH_ICON, label.clone())
                .aria_label(action_for(&label, title))
                .on_press(cx, move |this, window, cx| this.remove_schedule(id.clone(), window, cx))
        };
        // 停用的字各调淡一档，一眼和还在排的分开；下面的记录链接照样能点，不跟着淡。
        let (title_alpha, details_alpha) = if schedule.paused { (0.6, 0.4) } else { (1., 0.55) };
        let run_line = run_line_in(schedule, now, &rust_i18n::locale());
        let failed = run_line.as_ref().is_some_and(|(_, failed)| *failed);
        let spoken_next = match &held_text {
            Some(held) => format!("{next} · {held}"),
            None => next.clone(),
        };
        let mut description = format!("{rule}。{spoken_next}。{}", details.join(" · "));
        if let Some((line, _)) = &run_line {
            description = format!("{description}。{line}");
        }
        let theme = cx.theme();
        let (destructive, warning, muted) = (theme.destructive, theme.warning_text, theme.muted_foreground);
        let small = |text: String, color| div().text_size(FontSize::Small).text_color(color).truncate().child(text);
        let run_line =
            run_line.map(|(line, failed)| small(line, if failed { destructive } else { fg.opacity(details_alpha) }));
        // 这一行是要用户留意的提醒，折行写全，不截断。
        let held_line = held_text.map(|text| div().text_size(FontSize::Small).text_color(warning).child(text));
        let idle = schedule.paused || schedule.next_fire.is_none();
        // 左边的图标说状态：停用、上次先跑的命令失败了、在排；不会再跑的和停用的一样调淡。
        let (status_icon, status_color) = if schedule.paused {
            (BAN_ICON, muted)
        } else if failed {
            (CLOCK_ICON, destructive)
        } else if idle {
            (CLOCK_ICON, muted)
        } else {
            (CLOCK_ICON, fg.opacity(0.7))
        };
        // 下一次什么时候是最要紧的一条，和标题放在一行、靠右；停用和不会再跑时调淡。
        let next_color = if idle { muted } else { fg };
        // 说明那一行：规则、项目、agent、工作树，agent 画成 logo。辅助工具读上面的 `description`。放不下时
        // 规则留全，后面几段各自截断。
        let meta_color = fg.opacity(details_alpha);
        let mut meta: Vec<AnyElement> = Vec::new();
        let shrinking = |text: String| div().min_w_0().truncate().child(text).into_any_element();
        let parts = [div().flex_none().child(rule).into_any_element()]
            .into_iter()
            .chain(place.map(shrinking))
            .chain([agent_badge(&schedule.spec.agent, meta_color)])
            .chain(worktree.map(shrinking));
        for part in parts {
            if !meta.is_empty() {
                meta.push(div().flex_none().child("·").into_any_element());
            }
            meta.push(part);
        }
        let meta = div()
            .flex()
            .items_center()
            .gap(px(4.))
            .min_w_0()
            .overflow_hidden()
            .text_size(FontSize::Small)
            .text_color(meta_color)
            .children(meta);
        div()
            .id(named("todo-schedule", &schedule.id))
            .role(Role::ListItem)
            .aria_label(schedule.spec.title.clone())
            .aria_description(description)
            .flex_none()
            .py(px(10.))
            .pl(px(12.))
            .pr(px(6.))
            .flex()
            .items_start()
            .gap(px(10.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme.border.opacity(0.6))
            .bg(theme.mix(0.03))
            .child(div().flex_none().pt(px(3.)).child(svg().path(status_icon).size(px(14.)).text_color(status_color)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(fg.opacity(title_alpha))
                                    .child(schedule.spec.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(FontSize::Small)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(next_color)
                                    .child(next),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.))
                                    .child(fire)
                                    .child(pause)
                                    .child(edit)
                                    .child(remove),
                            ),
                    )
                    .child(meta)
                    .children(held_line)
                    .children(run_line)
                    // 链接按钮自带的左边距往回收，文字和上面几行对齐。
                    .child(div().ml(px(-6.)).flex().flex_wrap().items_center().gap(px(4.)).children(run_rows)),
            )
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

    #[test]
    fn schedules_group_by_place() {
        let schedule = |id: &str, dir: &str| {
            let mut spec = todo(id, None, None).spec;
            spec.dir = PathBuf::from(dir);
            Schedule {
                id: id.into(),
                spec,
                when: runode_protocol::todo::When::Every { minutes: 60 },
                last_fired: 0,
                next_fire: None,
                paused: false,
                run: None,
                run_timeout: None,
                last_run: None,
                running: false,
            }
        };
        let schedules = [
            schedule("a", "/z"),
            schedule("b", "/runode"),
            schedule("c", "/seeker"),
            schedule("d", "/runode"),
            schedule("e", "/y"),
        ];
        let workspaces = [PathBuf::from("/seeker"), PathBuf::from("/runode")];
        let groups: Vec<(String, Vec<&str>)> = group_by_place(&schedules, &workspaces)
            .into_iter()
            .map(|(dir, group)| (dir.display().to_string(), group.iter().map(|s| s.id.as_str()).collect()))
            .collect();
        let expected: Vec<(String, Vec<&str>)> = vec![
            ("/seeker".into(), vec!["c"]),
            ("/runode".into(), vec!["b", "d"]),
            ("/y".into(), vec!["e"]),
            ("/z".into(), vec!["a"]),
        ];
        assert_eq!(groups, expected);
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
            run: None,
            run_timeout: None,
            last_run: None,
            running: false,
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

    #[test]
    fn program_names_drop_dirs_and_quotes() {
        assert_eq!(program_name("~/.runode/scripts/gha-watch.sh --repo x"), "gha-watch.sh");
        assert_eq!(program_name("'./check.sh' -v"), "check.sh");
        assert_eq!(program_name("gh run list"), "gh");
        assert_eq!(program_name("  "), "");
        assert_eq!(program_name("sh ~/.runode/schedule-scripts/new-issues.sh 'good first'"), "new-issues.sh");
        assert_eq!(program_name("bash -e check.sh"), "check.sh");
        assert_eq!(program_name("sh"), "sh");
        assert_eq!(program_name("sh -c 'sleep 8; echo found'"), "sh");
        assert_eq!(program_name("pwsh -NoProfile -Command Get-Date"), "pwsh");
        assert_eq!(program_name("bash -ec 'set -x; ./check'"), "bash");
        assert_eq!(program_name("node -e 'console.log(1)'"), "node");
        assert_eq!(program_name("node --check watch.js"), "watch.js");
        assert_eq!(
            program_name(r#"powershell -NoProfile -ExecutionPolicy Bypass -File "/my dir/watch-ci.ps1" "a b""#),
            "watch-ci.ps1"
        );
    }

    /// 卡片上先跑的命令那一行：没跑过只说跑哪个程序，失败时接上输出的最后一行。
    #[test]
    fn run_lines_tell_the_last_outcome() {
        use runode_protocol::todo::ScheduleRun;
        let now = 1_800_000_000;
        let schedule = |run: Option<&str>, last_run: Option<ScheduleRun>| Schedule {
            id: "s".into(),
            spec: todo("s", None, None).spec,
            when: runode_protocol::todo::When::Every { minutes: 10 },
            last_fired: 0,
            next_fire: None,
            paused: false,
            run: run.map(str::to_owned),
            run_timeout: None,
            last_run,
            running: false,
        };
        let last = |outcome, output: &str| Some(ScheduleRun { at: now - 60, outcome, output: output.into() });
        let time = short_time(now - 60, now);
        assert_eq!(run_line_in(&schedule(None, None), now, "en"), None);
        assert_eq!(
            run_line_in(&schedule(Some("~/bin/watch.sh"), None), now, "zh-Hans"),
            Some(("先跑 watch.sh".to_owned(), false))
        );
        let failed = schedule(Some("~/bin/watch.sh"), last(RunOutcome::Failed, "checking\ngh: auth required\n\n"));
        assert_eq!(
            run_line_in(&failed, now, "zh-Hans"),
            Some((format!("先跑 watch.sh · 上次 {time} 失败：gh: auth required"), true))
        );
        let idle = schedule(Some("watch.sh"), last(RunOutcome::Idle, ""));
        assert_eq!(
            run_line_in(&idle, now, "en"),
            Some((format!("runs watch.sh first · last {time}: nothing to do"), false))
        );
        // 正在跑时不再接上次的结果，上次失败了也不标红。
        let running = Schedule { running: true, ..failed };
        assert_eq!(run_line_in(&running, now, "zh-Hans"), Some(("先跑 watch.sh · 正在跑…".to_owned(), false)));
        assert_eq!(run_line_in(&Schedule { running: true, ..schedule(None, None) }, now, "en"), None);
    }
}
