//! 新建、编辑任务和定时的对话框：标题、提示词、目录、agent、要不要独立的工作树；定时多一组什么时候跑
//! （一次、每天某时可选星期几、每隔几分钟）。时间都按本机的本地时间填、显示，换算用 libc 的
//! `localtime_r` 和 `mktime`。填得不对时在对话框里说，不发请求；宿主那边还会再查一遍。
//!
//! 对话框单独是一个视图（`TodoDialog`），由看板开、存在看板身上，盖住整个窗口，所以由 app 画在窗口的最上层
//! （`TodoBoard::dialog`）。关掉、存好了经 `DialogEvent` 告诉看板。

use std::path::PathBuf;

use gpui::{
    App, Context, Entity, EventEmitter, Focusable, FontWeight, Hsla, KeyDownEvent, MouseButton, PathPromptOptions,
    Render, Role, Subscription, Window, div, prelude::*, px,
};
use runode_protocol::{
    ClientMsg,
    todo::{DEFAULT_AGENT, Schedule, Todo, TodoSpec, When},
};
use runode_ui::{
    a11y::{Disable as _, Press},
    button::{Button, ButtonVariant},
    dialog::Dialog,
    display_dir,
    font_size::{FontSize, scaled},
    input::{Input, InputEvent, InputState},
    panel::TITLEBAR_HEIGHT,
    switch::Switch,
    textarea::{Textarea, TextareaEvent, TextareaState},
    theme::ActiveTheme as _,
};

use crate::store;

/// 对话框改的是哪一个；新建时为空。
#[derive(Clone)]
pub(crate) enum Editing {
    Todo(Todo),
    Schedule(Schedule),
}

/// 定时跑的方式，对话框里三选一。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Once,
    Daily,
    Every,
}

/// 新建、编辑任务和定时的对话框，见模块说明。
pub struct TodoDialog {
    /// 是定时（否则是任务）。
    schedule: bool,
    editing: Option<Editing>,
    title: Entity<InputState>,
    prompt: Entity<TextareaState>,
    agent: Entity<InputState>,
    dir: PathBuf,
    worktree: bool,
    /// 任务开始过了：目录和要不要工作树不能再改。
    locked: bool,
    kind: Kind,
    /// 一次：`YYYY-MM-DD HH:MM`；每天：`HH:MM`；每隔：分钟数。三个框各记各的，切来切去不丢。
    at: Entity<InputState>,
    clock: Entity<InputState>,
    minutes: Entity<InputState>,
    /// 每天跑时只在这几天（第 0 位是星期日），全不选是每天。
    days: u8,
    error: Option<String>,
    saving: bool,
    _events: Vec<Subscription>,
}

/// 本地时间的年月日时分。
pub(crate) struct Local {
    pub(crate) year: i32,
    pub(crate) month: u32,
    pub(crate) day: u32,
    pub(crate) hour: u32,
    pub(crate) minute: u32,
}

/// `at`（Unix 秒）的本地时间。
pub(crate) fn local(at: u64) -> Option<Local> {
    let at = libc::time_t::try_from(at).ok()?;
    // SAFETY: `tm` 是纯数据，全零是合法的值；`localtime_r` 只写它，两个指针都指向这里的局部变量。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: 同上。
    if unsafe { libc::localtime_r(&raw const at, &raw mut tm) }.is_null() {
        return None;
    }
    Some(Local {
        year: tm.tm_year + 1900,
        month: u32::try_from(tm.tm_mon + 1).ok()?,
        day: u32::try_from(tm.tm_mday).ok()?,
        hour: u32::try_from(tm.tm_hour).ok()?,
        minute: u32::try_from(tm.tm_min).ok()?,
    })
}

/// 本地时间的这一刻（Unix 秒）；日子不存在（2 月 30 日）时为空。
fn from_local(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Option<u64> {
    // SAFETY: 同 `local`。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = i32::try_from(month).ok()? - 1;
    tm.tm_mday = i32::try_from(day).ok()?;
    tm.tm_hour = i32::try_from(hour).ok()?;
    tm.tm_min = i32::try_from(minute).ok()?;
    tm.tm_isdst = -1;
    // SAFETY: 指针指向局部变量，`mktime` 规范化它。
    let at = unsafe { libc::mktime(&raw mut tm) };
    // 规范化挪了日子（2 月 30 日成了 3 月 2 日）就是填错了；春天跳过的钟点只挪时，照收。
    (tm.tm_mday == i32::try_from(day).ok()? && tm.tm_mon == i32::try_from(month).ok()? - 1)
        .then(|| u64::try_from(at).ok())
        .flatten()
}

/// `9:05`、`09:05` 这样的钟点。
fn parse_clock(text: &str) -> Option<(u8, u8)> {
    let (hour, minute) = text.trim().split_once(':')?;
    let (hour, minute): (u8, u8) = (hour.trim().parse().ok()?, minute.trim().parse().ok()?);
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// `YYYY-MM-DD HH:MM`，本地时间。
fn parse_date_time(text: &str) -> Option<u64> {
    let (date, clock) = text.trim().split_once(char::is_whitespace)?;
    let mut parts = date.split('-').map(|part| part.parse::<u32>().ok());
    let (year, month, day) = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    let (hour, minute) = parse_clock(clock)?;
    from_local(i32::try_from(year).ok()?, month, day, hour.into(), minute.into())
}

/// 至少一分钟的分钟数。
fn parse_minutes(text: &str) -> Option<u32> {
    text.trim().parse().ok().filter(|minutes| *minutes >= 1)
}

/// 提示词的第一行，标题空着时用它。
fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default()
}

/// 对话框要看板办的事。
pub(crate) enum DialogEvent {
    /// 按了取消、Esc 或点了外面：关掉，焦点交回看板。
    Dismissed,
    /// 宿主办成了：关掉，焦点同样交回看板。
    Saved,
}

impl EventEmitter<DialogEvent> for TodoDialog {}

impl TodoDialog {
    /// 新建（`editing` 为空）或编辑任务、定时的对话框，焦点给标题；新建时目录先填 `dir`（当前 workspace 的）。
    pub(crate) fn new(
        schedule: bool,
        editing: Option<Editing>,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let spec = match &editing {
            Some(Editing::Todo(todo)) => Some(todo.spec.clone()),
            Some(Editing::Schedule(schedule)) => Some(schedule.spec.clone()),
            None => None,
        };
        let locked = matches!(&editing, Some(Editing::Todo(todo)) if todo.started.is_some());
        let dir = spec.as_ref().map_or(dir, |spec| spec.dir.clone());
        // 新建任务时目录在 git 仓库里就默认开工作树；定时默认不开，每次到点都开一个会越堆越多。
        let worktree =
            spec.as_ref().map_or_else(|| !schedule && runode_git::repo_root(&dir).is_some(), |spec| spec.worktree);
        let field = |text: String, label: &str, placeholder: &str, cx: &mut Context<Self>| {
            let (label, placeholder) = (rust_i18n::t!(label).into_owned(), rust_i18n::t!(placeholder).into_owned());
            let select = text.len();
            cx.new(|cx| InputState::editing(text, select, cx).with_label(label).with_placeholder(placeholder))
        };
        let title = field(
            spec.as_ref().map(|spec| spec.title.clone()).unwrap_or_default(),
            "todo_board.dialog.title_label",
            "todo_board.dialog.title_placeholder",
            cx,
        );
        let agent = field(
            spec.as_ref().map_or_else(|| DEFAULT_AGENT.to_owned(), |spec| spec.agent.clone()),
            "todo_board.dialog.agent",
            "todo_board.dialog.agent_placeholder",
            cx,
        );
        let prompt_text = spec.as_ref().map(|spec| spec.prompt.clone()).unwrap_or_default();
        let prompt = cx.new(|cx| {
            let mut area = TextareaState::new(cx);
            area.set_line_limits(3, 10, cx);
            area.set_label(rust_i18n::t!("todo_board.dialog.prompt"), cx);
            area.set_placeholder(rust_i18n::t!("todo_board.dialog.prompt_placeholder").into_owned().into(), cx);
            area.set_text(prompt_text, cx);
            area
        });
        let when = match &editing {
            Some(Editing::Schedule(schedule)) => Some(schedule.when.clone()),
            _ => None,
        };
        let (kind, at_text, clock_text, minutes_text, days) = match when {
            Some(When::Once { at }) => (Kind::Once, full_time(at), "09:00".to_owned(), "60".to_owned(), 0),
            Some(When::Daily { hour, minute, days }) => {
                (Kind::Daily, String::new(), format!("{hour:02}:{minute:02}"), "60".to_owned(), days)
            }
            Some(When::Every { minutes }) => (Kind::Every, String::new(), "09:00".to_owned(), minutes.to_string(), 0),
            _ => (Kind::Daily, String::new(), "09:00".to_owned(), "60".to_owned(), 0),
        };
        let at_text = if at_text.is_empty() { next_hour() } else { at_text };
        let at = field(at_text, "todo_board.dialog.once", "todo_board.dialog.once_placeholder", cx);
        let clock = field(clock_text, "todo_board.dialog.daily", "todo_board.dialog.daily_placeholder", cx);
        let minutes = field(minutes_text, "todo_board.dialog.every", "todo_board.dialog.every_placeholder", cx);
        // 输入框里回车保存，Esc 关掉对话框；提示词里 ⌘回车保存。
        let mut events = Vec::new();
        for input in [&title, &agent, &at, &clock, &minutes] {
            events.push(cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Next => this.save(cx),
                InputEvent::Dismiss => cx.emit(DialogEvent::Dismissed),
                InputEvent::Changed(_) | InputEvent::Previous => {}
            }));
        }
        events.push(cx.subscribe_in(&prompt, window, |this, _, event: &TextareaEvent, _, cx| match event {
            TextareaEvent::Submit => this.save(cx),
            TextareaEvent::Changed => {}
        }));
        window.focus(&title.focus_handle(cx), cx);
        Self {
            schedule,
            editing,
            title,
            prompt,
            agent,
            dir,
            worktree,
            locked,
            kind,
            at,
            clock,
            minutes,
            days,
            error: None,
            saving: false,
            _events: events,
        }
    }

    /// 弹出系统的目录选择框，选好的目录换上。
    fn browse_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(rust_i18n::t!("todo_board.dialog.choose_dir").into_owned().into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            this.update(cx, |dialog, cx| {
                // 不在仓库里开不了工作树；新建任务换到仓库里时照打开对话框时的规矩默认打开。
                let new_todo = !dialog.schedule && dialog.editing.is_none();
                dialog.worktree = runode_git::repo_root(&dir).is_some() && (dialog.worktree || new_todo);
                dialog.dir = dir;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 按对话框里填的发请求；填得不对时在对话框里说。宿主办成了就关掉对话框，没办成时说原因。
    fn save(&mut self, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let message = match self.request(cx) {
            Ok(message) => message,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.saving = true;
        self.error = None;
        cx.notify();
        let answer = store::ask(cx, message);
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |dialog, cx| {
                match answer {
                    Ok(_) => cx.emit(DialogEvent::Saved),
                    Err(err) => {
                        dialog.saving = false;
                        dialog.error = Some(format!("{err:#}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for TodoDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog = &*self;
        let theme = cx.theme();
        let hover_bg = theme.mix(0.10);
        let primary_bg = theme.mix(0.16);
        let (fg_h, bg_h, input_border) = (theme.foreground, theme.background, theme.input);
        // 输入框随内容长高，至少 28 点。
        let input = |state: &Entity<InputState>| Input::new(state).h_auto().min_h(scaled(28.)).py(px(4.));
        let field = |label: String, child: gpui::AnyElement| {
            div().flex().flex_col().gap(px(6.)).child(div().text_color(fg_h.opacity(0.7)).child(label)).child(child)
        };
        // 对话框底下的按钮：比 `ButtonSize::Default` 高一些，高度由上下留白撑开。
        let button = |id: &'static str, label: String, variant: ButtonVariant| {
            Button::new(id, label).variant(variant).h_auto().py(px(5.)).cursor_pointer()
        };
        let pill = |id: gpui::ElementId, label: String, on: bool, role: Role| {
            div()
                .id(id)
                .role(role)
                .aria_label(label.clone())
                .aria_toggled(on.into())
                .px(px(10.))
                .py(px(3.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(on, |pill| pill.bg(primary_bg))
                .when(!on, |pill| pill.hover(move |pill| pill.bg(hover_bg)))
                .child(label)
        };
        let dir_label = rust_i18n::t!("todo_board.dialog.dir").into_owned();
        let dir_text = display_dir(&dialog.dir);
        let dir = div()
            .id("todo-dialog-dir")
            .role(Role::Button)
            .aria_label(dir_label.clone())
            .aria_value(dir_text.clone())
            .min_h(scaled(28.))
            .px(px(8.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .bg(bg_h)
            .border_1()
            // 和上下的输入框（`Input`）一样的边框。
            .border_color(input_border)
            .truncate()
            .child(dir_text)
            .when(!dialog.locked, |dir| {
                dir.cursor_pointer().on_press(cx, |this, window, cx| this.browse_dir(window, cx))
            })
            .when(dialog.locked, |dir| dir.opacity(0.6))
            .aria_disabled(dialog.locked);
        let worktree_label = rust_i18n::t!("todo_board.dialog.worktree").into_owned();
        // 一行字配一个开关，点字也算点开关。
        let worktree = Switch::new("todo-dialog-worktree", worktree_label.clone(), dialog.worktree)
            .label(worktree_label)
            .cursor_pointer()
            .when(!dialog.locked, |pill| {
                pill.on_press(cx, |dialog, _, cx| {
                    dialog.worktree = !dialog.worktree;
                    cx.notify();
                })
            })
            .when(dialog.locked, |pill| pill.opacity(0.6))
            .aria_disabled(dialog.locked);
        let when = dialog.schedule.then(|| {
            let kind = |id: &'static str, kind: Kind, label: &str, cx: &mut Context<Self>| {
                pill(id.into(), rust_i18n::t!(label).into_owned(), dialog.kind == kind, Role::RadioButton).on_press(
                    cx,
                    move |dialog, window, cx| {
                        dialog.kind = kind;
                        let input = match kind {
                            Kind::Once => dialog.at.clone(),
                            Kind::Daily => dialog.clock.clone(),
                            Kind::Every => dialog.minutes.clone(),
                        };
                        window.focus(&input.focus_handle(cx), cx);
                        cx.notify();
                    },
                )
            };
            let when_label = rust_i18n::t!("todo_board.dialog.when").into_owned();
            let kinds = div()
                .id("todo-dialog-when")
                .role(Role::RadioGroup)
                .aria_label(when_label.clone())
                .flex()
                .p(px(2.))
                .gap(px(2.))
                .rounded(px(6.))
                .bg(bg_h)
                .child(kind("todo-dialog-once", Kind::Once, "todo_board.dialog.kind_once", cx))
                .child(kind("todo-dialog-daily", Kind::Daily, "todo_board.dialog.kind_daily", cx))
                .child(kind("todo-dialog-every", Kind::Every, "todo_board.dialog.kind_every", cx));
            let when_input = match dialog.kind {
                Kind::Once => &dialog.at,
                Kind::Daily => &dialog.clock,
                Kind::Every => &dialog.minutes,
            };
            let days = (dialog.kind == Kind::Daily).then(|| {
                let pills: Vec<_> = (0..7u8)
                    .map(|day| {
                        let on = dialog.days & (1 << day) != 0;
                        let label = weekday(day);
                        pill(("todo-weekday", usize::from(day)).into(), label, on, Role::CheckBox).on_press(
                            cx,
                            move |dialog, _, cx| {
                                dialog.days ^= 1 << day;
                                cx.notify();
                            },
                        )
                    })
                    .collect();
                div().flex().flex_col().gap(px(4.)).child(div().flex().flex_wrap().gap(px(2.)).children(pills)).child(
                    div()
                        .text_size(FontSize::Small)
                        .text_color(fg_h.opacity(0.55))
                        .child(rust_i18n::t!("todo_board.dialog.days_hint").into_owned()),
                )
            });
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().text_color(fg_h.opacity(0.7)).child(when_label))
                        .child(kinds),
                )
                .child(input(when_input))
                .children(days)
        });
        let title = match (&dialog.editing, dialog.schedule) {
            (Some(_), false) => rust_i18n::t!("todo_board.edit"),
            (Some(_), true) => rust_i18n::t!("todo_board.edit_schedule"),
            (None, false) => rust_i18n::t!("todo_board.new_todo"),
            (None, true) => rust_i18n::t!("todo_board.new_schedule"),
        }
        .into_owned();
        let cancel =
            button("todo-dialog-cancel", rust_i18n::t!("todo_board.cancel").into_owned(), ButtonVariant::Ghost)
                .on_press(cx, |_, _, cx| cx.emit(DialogEvent::Dismissed));
        let save = button("todo-dialog-save", rust_i18n::t!("todo_board.save").into_owned(), ButtonVariant::Secondary)
            .when(dialog.saving, |button| button.opacity(0.5))
            .on_press(cx, |this, _, cx| this.save(cx));
        let panel = Dialog::new("todo-dialog", title.clone())
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    cx.emit(DialogEvent::Dismissed);
                }
            }))
            .w(px(480.))
            .max_w_full()
            .max_h_full()
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .id("todo-dialog-title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(title.clone())
                    .text_size(FontSize::Title)
                    .font_weight(FontWeight::BOLD)
                    .child(title),
            )
            // 窗口矮时只有中间这些滚动，标题和保存按钮总在。
            .child(
                div()
                    .id("todo-dialog-fields")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(field(
                        rust_i18n::t!("todo_board.dialog.title_label").into_owned(),
                        input(&dialog.title).into_any_element(),
                    ))
                    .child(field(
                        rust_i18n::t!("todo_board.dialog.prompt").into_owned(),
                        Textarea::new(&dialog.prompt)
                            .min_h(scaled(28.))
                            .py(px(4.))
                            .flex()
                            .items_center()
                            .into_any_element(),
                    ))
                    .children(when)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(div().text_color(fg_h.opacity(0.7)).child(dir_label))
                            .child(dir),
                    )
                    .child(field(
                        rust_i18n::t!("todo_board.dialog.agent").into_owned(),
                        input(&dialog.agent).into_any_element(),
                    ))
                    .child(div().flex().child(worktree)),
            )
            .children(dialog.error.clone().map(|error| {
                div()
                    .id("todo-dialog-error")
                    .role(Role::Label)
                    .aria_label(error.clone())
                    .text_color(gpui::red())
                    .child(error)
            }))
            .child(div().pt(px(4.)).flex().justify_end().gap(px(8.)).child(cancel).child(save));
        div()
            .id("todo-dialog-backdrop")
            .absolute()
            .size_full()
            .pt(px(TITLEBAR_HEIGHT + 32.))
            .pb(px(16.))
            .px(px(16.))
            .flex()
            .justify_center()
            .items_start()
            .bg(Hsla::black().opacity(0.15))
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Dismissed)))
            .child(panel)
    }
}

impl TodoDialog {
    /// 按填的内容造出要发的请求；填得不对时是给人看的原因。
    fn request(&self, cx: &App) -> Result<store::Step, String> {
        let prompt = self.prompt.read(cx).text().trim().to_owned();
        let typed = self.title.read(cx).query().trim().to_owned();
        let title = if typed.is_empty() { first_line(&prompt).to_owned() } else { typed };
        if title.is_empty() {
            return Err(rust_i18n::t!("todo_board.dialog.need_title").into_owned());
        }
        let agent = self.agent.read(cx).query().trim().to_owned();
        if agent.is_empty() {
            return Err(rust_i18n::t!("todo_board.dialog.need_agent").into_owned());
        }
        if !self.dir.is_dir() {
            return Err(rust_i18n::t!("todo_board.dialog.bad_dir").into_owned());
        }
        let spec = TodoSpec { title, prompt, dir: self.dir.clone(), agent, worktree: self.worktree };
        if !self.schedule {
            return Ok(match &self.editing {
                Some(Editing::Todo(todo)) => {
                    let id = todo.id.clone();
                    Box::new(move |req| ClientMsg::UpdateTodo { req, id, spec: Some(spec), done: None })
                }
                _ => Box::new(move |req| ClientMsg::AddTodo { req, spec }),
            });
        }
        let when = match self.kind {
            Kind::Once => When::Once {
                at: parse_date_time(self.at.read(cx).query())
                    .ok_or_else(|| rust_i18n::t!("todo_board.dialog.bad_once").into_owned())?,
            },
            Kind::Daily => {
                let (hour, minute) = parse_clock(self.clock.read(cx).query())
                    .ok_or_else(|| rust_i18n::t!("todo_board.dialog.bad_daily").into_owned())?;
                When::Daily { hour, minute, days: self.days }
            }
            Kind::Every => When::Every {
                minutes: parse_minutes(self.minutes.read(cx).query())
                    .ok_or_else(|| rust_i18n::t!("todo_board.dialog.bad_every").into_owned())?,
            },
        };
        Ok(match &self.editing {
            Some(Editing::Schedule(schedule)) => {
                let id = schedule.id.clone();
                // 没改什么时候跑时不发 `when`，宿主就不会从现在重新算起。
                let when = (when != schedule.when).then_some(when);
                Box::new(move |req| ClientMsg::UpdateSchedule { req, id, spec: Some(spec), when })
            }
            _ => Box::new(move |req| ClientMsg::AddSchedule { req, spec, when }),
        })
    }
}

/// 星期几的简称，`day` 从 0（星期日）数。
pub(crate) fn weekday(day: u8) -> String {
    let key = format!("todo_board.weekday.{day}");
    rust_i18n::t!(&key).into_owned()
}

/// 下一个整点，`YYYY-MM-DD HH:MM`：新建一次性的定时时先填上它。
fn next_hour() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    full_time(next_local_hour(now))
}

/// `now` 之后本地时间的下一个整点。按本地的分钟数补齐，不按 UTC 的整点：UTC+5:30 这类时区的 UTC 整点在本地
/// 是半点。
fn next_local_hour(now: u64) -> u64 {
    let minute = local(now).map_or(0, |at| at.minute);
    now - now % 60 + u64::from(60 - minute) * 60
}

/// `YYYY-MM-DD HH:MM`，按本地时间。
pub(crate) fn full_time(at: u64) -> String {
    local(at).map_or_else(String::new, |at| {
        format!("{:04}-{:02}-{:02} {:02}:{:02}", at.year, at.month, at.day, at.hour, at.minute)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clocks_minutes_and_dates() {
        assert_eq!(parse_clock("9:05"), Some((9, 5)));
        assert_eq!(parse_clock(" 23:59 "), Some((23, 59)));
        assert_eq!(parse_clock("24:00"), None);
        assert_eq!(parse_clock("9:5x"), None);
        assert_eq!(parse_clock("905"), None);
        assert_eq!(parse_minutes("15"), Some(15));
        assert_eq!(parse_minutes("0"), None);
        assert_eq!(parse_minutes("x"), None);
        assert_eq!(parse_date_time("2026-02-30 09:00"), None);
        assert_eq!(parse_date_time("2026-13-01 09:00"), None);
        assert_eq!(parse_date_time("2026-10-13"), None);
        assert_eq!(first_line("\n  fix it \nmore"), "fix it");
    }

    // 不改 `TZ`：本地时间来回一趟原样回来，和时区无关。
    #[test]
    fn local_times_round_trip() {
        let at = parse_date_time("2026-10-13 14:30").unwrap();
        let local = local(at).unwrap();
        assert_eq!((local.year, local.month, local.day, local.hour, local.minute), (2026, 10, 13, 14, 30));
        assert_eq!(full_time(at), "2026-10-13 14:30");
        assert!(next_hour().ends_with(":00"));
        for now in [at, at + 59, at + 30 * 60, at + 89 * 60 + 59] {
            let next = next_local_hour(now);
            assert!(next > now && next - now <= 3600, "{now} -> {next}");
            assert_eq!(super::local(next).unwrap().minute, 0, "{now} -> {next}");
        }
    }
}
