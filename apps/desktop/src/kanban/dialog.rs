//! 新建、编辑任务和定时的对话框：标题、提示词、在哪个工作区或目录里做、agent 和它的模型、强度（见 `runode_agent::models`），
//! 要用的 skill、以哪个自定义 agent 的身份跑（`skills`）、附上的文件，免不免确认，要不要独立的工作树；定时多一组什么时候跑
//! （一次、每天某时可选星期几、每隔几分钟）。时间都按本机的本地时间填、显示，换算用 libc 的
//! `localtime_r` 和 `mktime`。填得不对时在对话框里说，不发请求；宿主那边还会再查一遍。
//!
//! 对话框单独是一个视图（`TodoDialog`），由看板开、存在看板身上，关掉、存好了经 `DialogEvent` 告诉看板。定时的
//! 盖住整个窗口，由 app 画在窗口的最上层（`Kanban::dialog`）；任务的是看板里的一页（`TodoDialog::page`），由看板
//! 画，新建时左边列出模板（`templates`），点一个就照它填好，带 `{{input}}` 的另要填一项（issue、PR 号），保存时换进去。

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, LazyLock, Mutex},
};

use crate::assets::CHEVRON_DOWN_ICON;
use crate::kanban::{
    skills::{self, Found},
    templates::{self, Template},
};
use crate::ui::{
    a11y::{Disable as _, Press, PressDown as _},
    base_name,
    button::{Button, ButtonVariant},
    dialog::Dialog,
    display_dir,
    font_size::{FontSize, scaled},
    icons::{CLOSE_ICON, PLUS_ICON},
    input::{Input, InputEvent, InputState},
    menu::{Menu, MenuHost, MenuItem},
    panel::TITLEBAR_HEIGHT,
    switch::Switch,
    textarea::{Textarea, TextareaEvent, TextareaState},
    theme::ActiveTheme as _,
};
use gpui::{
    Action, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent,
    MouseButton, PathPromptOptions, Render, Role, SharedString, Stateful, Subscription, Window, canvas, div,
    prelude::*, px, svg,
};
use runode_protocol::{
    ClientMsg, HostMsg,
    todo::{DEFAULT_AGENT, Schedule, Todo, TodoSpec, When},
};

use crate::kanban::store;
use runode_agent::models::{Catalog, LAUNCHERS, Launcher, Pick, Unanswered};

/// 问到的各家 agent 的模型清单：下次打开对话框先用着，同时再问一遍。
static CATALOGS: LazyLock<Mutex<HashMap<&'static str, Arc<Catalog>>>> = LazyLock::new(Mutex::default);

/// 上次问到的 `launcher` 的清单。
fn cached(launcher: &Launcher) -> Option<Arc<Catalog>> {
    CATALOGS.lock().ok()?.get(launcher.command).cloned()
}

/// agent 菜单的一项：`LAUNCHERS` 里第 `index` 个，为空时是自己写的命令。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(crate) struct PickTodoAgent {
    index: Option<usize>,
}

/// 模型菜单的一项：写进命令的模型名，为空时是默认。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(crate) struct PickTodoModel {
    value: Option<String>,
}

/// 强度菜单的一项，为空时是默认。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(crate) struct PickTodoEffort {
    value: Option<String>,
}

/// 工作区菜单的一项：第 `index` 个工作区的目录，为空时弹出目录选择框。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(crate) struct PickTodoWorkspace {
    index: Option<usize>,
}

/// skill 菜单的一项：挑上或去掉这个 skill。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(crate) struct PickTodoSkill {
    name: String,
}

/// 自定义 agent 菜单的一项，为空时是不指定。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(crate) struct PickTodoSubagent {
    name: Option<String>,
}

/// 对话框改的是哪一个；新建时为空。
#[derive(Clone)]
pub(crate) enum Editing {
    Todo(Todo),
    Schedule(Schedule),
}

/// 点了弹出菜单的几个框。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Combo {
    Workspace,
    Agent,
    Model,
    Effort,
    Skills,
    Subagent,
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
    /// 是看板里的一页（任务的），不是盖住窗口的对话框（定时的）。
    pub(crate) page: bool,
    editing: Option<Editing>,
    /// 新建任务时左边列的模板，后台读好前为空；挑着的是第几个，空白的为空。
    templates: Vec<Template>,
    template: Option<usize>,
    /// 挑着的模板要另外填的一项（`Template::input`），换模板时跟着换说明。
    input: Entity<InputState>,
    input_events: Option<Subscription>,
    title: Entity<InputState>,
    prompt: Entity<TextareaState>,
    /// 挑的 agent、模型和强度；为空时跑 `command` 里自己写的命令。
    agent: Option<Pick>,
    command: Entity<InputState>,
    /// 各工作区的目录和名字，工作区菜单里列。
    workspaces: Vec<(PathBuf, SharedString)>,
    /// 找 agent 用的 PATH，问模型清单时用。
    shell_path: Option<OsString>,
    /// 正在问模型清单的 agent（`LAUNCHERS` 里第几个），和没问到的、为什么没问到。
    loading: HashSet<usize>,
    failed: HashMap<usize, Unanswered>,
    dir: PathBuf,
    worktree: bool,
    /// 挑上的 skill、自定义 agent 和附上的文件，见 `TodoSpec`；`found` 是在 `dir` 能找到的 skill 和自定义 agent。
    skills: Vec<String>,
    subagent: Option<String>,
    attachments: Vec<PathBuf>,
    found: Found,
    auto: bool,
    /// 任务开始过了：目录和要不要工作树不能再改。
    locked: bool,
    /// 读不懂的定时规则（`When::Unknown`）在用户选一种之前为空，存的时候规则照原样留着。
    kind: Option<Kind>,
    /// 一次：`YYYY-MM-DD HH:MM`；每天：`HH:MM`；每隔：分钟数。三个框各记各的，切来切去不丢。
    at: Entity<InputState>,
    clock: Entity<InputState>,
    minutes: Entity<InputState>,
    /// 每天跑时只在这几天（第 0 位是星期日），全不选是每天。
    days: u8,
    error: Option<String>,
    saving: bool,
    /// 菜单里点了一项后焦点交给它，动作从这里派发到对话框。
    focus: FocusHandle,
    menu: Option<Menu>,
    /// 开着的菜单是哪个框的，菜单挂在它下面。
    menu_for: Option<Combo>,
    /// 各个框上一帧的下沿离窗口顶多远（按 `Combo` 的次序），菜单最高到窗口底边。
    combo_bottoms: Rc<[Cell<f32>; 6]>,
    _events: Vec<Subscription>,
}

impl MenuHost for TodoDialog {
    fn menu_slot(&mut self) -> &mut Option<Menu> {
        &mut self.menu
    }

    fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }
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

/// 一次性定时的时间栏 `text` 是哪一刻。和编辑前的 `original` 显示得一样（没改）时照原样，不重新解析：
/// `full_time` 不带秒，夏令时回拨那个钟点也可能换成另一刻。
fn once_at(text: &str, original: Option<u64>) -> Option<u64> {
    original.filter(|at| full_time(*at) == text.trim()).or_else(|| parse_date_time(text))
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
    /// 宿主办成了：关掉，焦点同样交回看板；按的是「保存并开始」时 `start` 是要接着开始的任务。
    Saved { start: Option<String> },
}

impl EventEmitter<DialogEvent> for TodoDialog {}

impl TodoDialog {
    /// 新建（`editing` 为空）或编辑任务、定时的对话框，焦点给标题；新建时目录先填 `dir`（当前 workspace 的）。
    /// `workspaces` 是各工作区的目录和名字，`shell_path` 是找 agent 用的 PATH。
    pub(crate) fn new(
        schedule: bool,
        editing: Option<Editing>,
        dir: PathBuf,
        workspaces: Vec<(PathBuf, SharedString)>,
        shell_path: Option<OsString>,
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
        // 新建任务时目录在 git 仓库里就默认开工作树（`check_repo` 在后台看完再打开）；定时默认不开，每次到点都
        // 开一个会越堆越多。
        let worktree = spec.as_ref().is_some_and(|spec| spec.worktree);
        let new_todo = spec.is_none() && !schedule;
        let field = |text: String, label: &str, placeholder: &str, cx: &mut Context<Self>| {
            let (label, placeholder) = (rust_i18n::t!(label).into_owned(), rust_i18n::t!(placeholder).into_owned());
            let select = text.len();
            cx.new(|cx| InputState::editing(text, select, cx).with_label(label).with_placeholder(placeholder))
        };
        let title = field(
            spec.as_ref().map(|spec| spec.title.clone()).unwrap_or_default(),
            "kanban.dialog.title_label",
            "kanban.dialog.title_placeholder",
            cx,
        );
        let saved_agent = spec.as_ref().map_or_else(|| DEFAULT_AGENT.to_owned(), |spec| spec.agent.clone());
        let agent = Pick::parse(&saved_agent);
        let command = field(saved_agent, "kanban.dialog.command", "kanban.dialog.command_placeholder", cx);
        let prompt_text = spec.as_ref().map(|spec| spec.prompt.clone()).unwrap_or_default();
        let prompt = cx.new(|cx| {
            let mut area = TextareaState::new(cx);
            area.set_line_limits(3, 10, cx);
            area.set_label(rust_i18n::t!("kanban.dialog.prompt"), cx);
            area.set_placeholder(rust_i18n::t!("kanban.dialog.prompt_placeholder").into_owned().into(), cx);
            area.set_text(prompt_text, cx);
            area
        });
        let when = match &editing {
            Some(Editing::Schedule(schedule)) => Some(schedule.when.clone()),
            _ => None,
        };
        let (kind, at_text, clock_text, minutes_text, days) = match when {
            Some(When::Once { at }) => (Some(Kind::Once), full_time(at), "09:00".to_owned(), "60".to_owned(), 0),
            Some(When::Daily { hour, minute, days }) => {
                (Some(Kind::Daily), String::new(), format!("{hour:02}:{minute:02}"), "60".to_owned(), days)
            }
            Some(When::Every { minutes }) => {
                (Some(Kind::Every), String::new(), "09:00".to_owned(), minutes.to_string(), 0)
            }
            Some(When::Unknown) => (None, String::new(), "09:00".to_owned(), "60".to_owned(), 0),
            None => (Some(Kind::Daily), String::new(), "09:00".to_owned(), "60".to_owned(), 0),
        };
        let at_text = if at_text.is_empty() { next_hour() } else { at_text };
        let at = field(at_text, "kanban.dialog.once", "kanban.dialog.once_placeholder", cx);
        let clock = field(clock_text, "kanban.dialog.daily", "kanban.dialog.daily_placeholder", cx);
        let minutes = field(minutes_text, "kanban.dialog.every", "kanban.dialog.every_placeholder", cx);
        let input = cx.new(|cx| InputState::new(String::new(), cx));
        // 输入框里回车保存，Esc 关掉对话框；提示词里 ⌘回车保存。
        let mut events = Vec::new();
        for input in [&title, &command, &at, &clock, &minutes] {
            events.push(Self::subscribe_input(input, window, cx));
        }
        events.push(cx.subscribe_in(&prompt, window, |this, _, event: &TextareaEvent, _, cx| match event {
            TextareaEvent::Submit => this.save(false, cx),
            TextareaEvent::Changed => {}
        }));
        window.focus(&title.focus_handle(cx), cx);
        let spec = spec.unwrap_or_default();
        let mut dialog = Self {
            schedule,
            page: !schedule,
            editing,
            templates: Vec::new(),
            template: None,
            input_events: None,
            input,
            title,
            prompt,
            agent,
            command,
            workspaces,
            shell_path,
            loading: HashSet::new(),
            failed: HashMap::new(),
            dir,
            worktree,
            skills: spec.skills,
            subagent: spec.subagent,
            attachments: spec.attachments,
            found: Found::default(),
            auto: spec.auto,
            locked,
            kind,
            at,
            clock,
            minutes,
            days,
            error: None,
            saving: false,
            focus: cx.focus_handle(),
            menu: None,
            menu_for: None,
            combo_bottoms: Rc::default(),
            _events: events,
        };
        if new_todo {
            dialog.check_repo(cx);
            dialog.load_templates(cx);
        }
        dialog.find_skills(cx);
        if let Some(pick) = &dialog.agent {
            dialog.fetch_models(pick.agent, cx);
        }
        dialog
    }

    /// 输入框里回车保存，Esc 关掉对话框。
    fn subscribe_input(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::Next => this.save(false, cx),
            InputEvent::Dismiss => cx.emit(DialogEvent::Dismissed),
            InputEvent::Changed(_) | InputEvent::Previous => {}
        })
    }

    /// 在后台读模板（`templates::load`），读好了列出来。
    fn load_templates(&mut self, cx: &mut Context<Self>) {
        let dir = runode_paths::Dirs::from_env().todo_templates_dir();
        let load = cx.background_executor().spawn(async move { templates::load(dir.as_deref()) });
        cx.spawn(async move |this, cx| {
            let templates = load.await;
            this.update(cx, |dialog, cx| {
                dialog.templates = templates;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 在后台找 `dir` 能用的 skill 和自定义 agent（`skills::find`），找完了还是这个目录时换上。
    fn find_skills(&mut self, cx: &mut Context<Self>) {
        let dir = self.dir.clone();
        let home = runode_paths::Dirs::from_env().home;
        let find = cx.background_executor().spawn({
            let dir = dir.clone();
            async move { skills::find(home.as_deref(), &dir) }
        });
        cx.spawn(async move |this, cx| {
            let found = find.await;
            this.update(cx, |dialog, cx| {
                if dialog.dir == dir {
                    dialog.found = found;
                    dialog.refresh_menu(cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 照第 `index` 个模板填好（为空时是空白的）：免不免确认和工作树换成它的，skill 加上它的；标题、提示词
    /// 是空的或者还是上一个模板的原样时换成它的，用户改过的不动。要另外填一项的，那一栏换上它的说明，焦点给它。
    fn pick_template(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.template.map(|index| self.templates[index].clone()).unwrap_or_default();
        let untouched = |text: &str, original: &str| text.trim().is_empty() || text.trim() == original.trim();
        self.template = index.filter(|index| *index < self.templates.len());
        let template = self.template.map(|index| self.templates[index].clone()).unwrap_or_default();
        if untouched(self.title.read(cx).query(), &before.title) {
            self.title.update(cx, |input, cx| input.set_query(template.title.clone(), cx));
        }
        if untouched(self.prompt.read(cx).text(), &before.prompt) {
            self.prompt.update(cx, |area, cx| area.set_text(template.prompt.clone(), cx));
        }
        self.auto = template.auto;
        for skill in &template.skills {
            if !self.skills.contains(skill) {
                self.skills.push(skill.clone());
            }
        }
        self.worktree = template.worktree;
        self.check_repo(cx);
        let focus = match &template.input {
            Some(label) => {
                let (label, placeholder) = (label.clone(), label.clone());
                self.input =
                    cx.new(|cx| InputState::new(String::new(), cx).with_label(label).with_placeholder(placeholder));
                self.input_events = Some(Self::subscribe_input(&self.input, window, cx));
                self.input.focus_handle(cx)
            }
            None => self.title.focus_handle(cx),
        };
        window.focus(&focus, cx);
        self.error = None;
        cx.notify();
    }

    /// 弹出系统的文件选择框，选好的文件附上。
    fn browse_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(rust_i18n::t!("kanban.dialog.add_attachment").into_owned().into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            this.update(cx, |dialog, cx| {
                for path in paths {
                    if !dialog.attachments.contains(&path) {
                        dialog.attachments.push(path);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn pick_skill(&mut self, action: &PickTodoSkill, _: &mut Window, cx: &mut Context<Self>) {
        match self.skills.iter().position(|skill| *skill == action.name) {
            Some(index) => {
                self.skills.remove(index);
            }
            None => self.skills.push(action.name.clone()),
        }
        cx.notify();
    }

    fn pick_subagent(&mut self, action: &PickTodoSubagent, _: &mut Window, cx: &mut Context<Self>) {
        self.subagent.clone_from(&action.name);
        cx.notify();
    }

    /// skill 菜单：找到的和挑上的（可能是模板带来、或者在别的目录里找到的），挑上的打勾；一个都没有时说一句。
    fn skill_items(&self) -> Vec<Option<MenuItem>> {
        let mut names: Vec<&String> = self.found.skills.iter().chain(&self.skills).collect();
        names.sort();
        names.dedup();
        if names.is_empty() {
            return vec![Some(MenuItem::label(rust_i18n::t!("kanban.dialog.no_skills").into_owned(), None, None))];
        }
        names
            .into_iter()
            .map(|name| {
                let action = Box::new(PickTodoSkill { name: name.clone() });
                Some(MenuItem::checkbox(name.clone(), None, self.skills.contains(name), action))
            })
            .collect()
    }

    /// 自定义 agent 菜单：先是不指定，再是找到的，挑着的不在里面时也列上。
    fn subagent_items(&self) -> Vec<Option<MenuItem>> {
        let none = rust_i18n::t!("kanban.dialog.none").into_owned();
        let mut names: Vec<&String> = self.found.subagents.iter().chain(&self.subagent).collect();
        names.sort();
        names.dedup();
        std::iter::once((None, none))
            .chain(names.into_iter().map(|name| (Some(name.clone()), name.clone())))
            .map(|(name, label)| {
                let checked = name == self.subagent;
                Some(MenuItem::checkbox(label, None, checked, Box::new(PickTodoSubagent { name })))
            })
            .collect()
    }

    /// 在后台看目录在不在 git 仓库里（`repo_root` 要起 git，慢的时候别冻住界面），看完了定要不要工作树：不在
    /// 仓库里开不了；新建任务在仓库里默认打开。这期间换了目录或拨过开关就不动。
    fn check_repo(&mut self, cx: &mut Context<Self>) {
        let (dir, worktree) = (self.dir.clone(), self.worktree);
        let repo = cx.background_executor().spawn({
            let dir = dir.clone();
            async move { runode_git::repo_root(&dir).is_some() }
        });
        cx.spawn(async move |this, cx| {
            let repo = repo.await;
            this.update(cx, |dialog, cx| {
                if dialog.dir != dir || dialog.worktree != worktree {
                    return;
                }
                // 挑了模板时照模板的。
                let new_todo = !dialog.schedule && dialog.editing.is_none() && dialog.template.is_none();
                dialog.worktree = repo && (dialog.worktree || new_todo);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 换了目录：重新看要不要工作树，再问一遍挑着的 agent 的模型清单（有的 agent 读项目里的配置）。
    fn dir_changed(&mut self, cx: &mut Context<Self>) {
        self.check_repo(cx);
        self.find_skills(cx);
        // shortcut: 上一次还在问着时不再问，清单按旧目录的算；目录换得比 agent 答得快时再按目录分开记。
        if let Some(pick) = &self.agent {
            self.fetch_models(pick.agent, cx);
        }
    }

    /// 弹出系统的目录选择框，选好的目录换上。
    fn browse_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(rust_i18n::t!("kanban.dialog.choose_dir").into_owned().into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            this.update(cx, |dialog, cx| {
                dialog.dir = dir;
                dialog.dir_changed(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 在后台问第 `agent` 个 agent 的模型清单（`runode_agent::models::fetch`），记进 `CATALOGS`；问着的时候不再问；
    /// 问到之前菜单里用上次问到的，问到的是空的也不盖掉上次的。找不到命令而同一家还有别的版本（`same_kind`）时，
    /// 还没挑模型和强度的换成那个版本再问。
    fn fetch_models(&mut self, agent: usize, cx: &mut Context<Self>) {
        if !self.loading.insert(agent) {
            return;
        }
        let (dir, path) = (self.dir.clone(), self.shell_path.clone());
        let job = cx.background_executor().spawn(async move {
            let launcher = &LAUNCHERS[agent];
            let catalog = runode_agent::models::fetch(launcher, &dir, path)?;
            if let Ok(mut catalogs) = CATALOGS.lock()
                && (!catalog.models.is_empty() || !catalogs.contains_key(launcher.command))
            {
                catalogs.insert(launcher.command, Arc::new(catalog));
            }
            Ok(())
        });
        cx.spawn(async move |this, cx| {
            let catalog = job.await;
            this.update(cx, |dialog, cx| {
                dialog.loading.remove(&agent);
                match catalog {
                    Ok(()) => dialog.failed.remove(&agent),
                    Err(why) => dialog.failed.insert(agent, why),
                };
                // 挑了模型或强度的不换，存着的任务里挑了的也一样。什么都没挑的换：存着的 `opencode2 mini --prompt`
                // 在没有 opencode2 的机器上本来就跑不起来，换成 1.x 的写法，保存时命令跟着变。
                if catalog == Err(Unanswered::Missing)
                    && let Some(other) = runode_agent::models::same_kind(agent)
                    && let Some(pick) = dialog
                        .agent
                        .as_mut()
                        .filter(|pick| pick.agent == agent && pick.model.is_none() && pick.effort.is_none())
                {
                    *pick = Pick { agent: other, model: None, effort: None };
                    dialog.fetch_models(other, cx);
                }
                // 模型、强度的菜单开着时换上问到的清单。
                dialog.refresh_menu(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 在框 `combo` 下面弹出它的菜单，点了的那项派发到对话框；已经开着时关掉（辅助工具的按下也走这里）。
    fn toggle_menu(&mut self, combo: Combo, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu_for == Some(combo) && self.dropdown_open() {
            self.close_menu(window, cx);
            return;
        }
        let items = self.menu_items(combo);
        self.menu_for = Some(combo);
        let target = self.focus.clone();
        self.open_dropdown(items, target, None, cx);
    }

    /// 开着的菜单照现在的清单重建：问到了模型清单时用。
    fn refresh_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(combo) = self.menu_for.filter(|_| self.dropdown_open()) {
            let items = self.menu_items(combo);
            self.replace_menu_items(items, cx);
        }
    }

    /// 框 `combo` 的菜单项。
    fn menu_items(&self, combo: Combo) -> Vec<Option<MenuItem>> {
        match combo {
            Combo::Workspace => self.workspace_items(),
            Combo::Agent => self.agent_items(),
            Combo::Model => self.model_items(),
            Combo::Effort => self.effort_items(),
            Combo::Skills => self.skill_items(),
            Combo::Subagent => self.subagent_items(),
        }
    }

    /// agent 菜单：`LAUNCHERS` 里每家一项（`listed`），最后是自己写的命令。
    fn agent_items(&self) -> Vec<Option<MenuItem>> {
        let current = self.agent.as_ref().map(|pick| pick.launcher().label);
        let mut items: Vec<_> = LAUNCHERS
            .iter()
            .enumerate()
            .filter(|(index, _)| runode_agent::models::listed(*index))
            .map(|(index, spec)| {
                let action = Box::new(PickTodoAgent { index: Some(index) });
                Some(MenuItem::checkbox(spec.label.to_owned(), None, current == Some(spec.label), action))
            })
            .collect();
        let custom = rust_i18n::t!("kanban.dialog.custom").into_owned();
        let action = Box::new(PickTodoAgent { index: None });
        items.extend([None, Some(MenuItem::checkbox(custom, None, current.is_none(), action))]);
        items
    }

    /// 模型或强度的菜单：先是默认，再是 `options`（值和显示的名字），`current` 打勾；还没问到清单时最后说一句。
    fn options_items(
        &self,
        options: Vec<(String, String)>,
        current: Option<&str>,
        action: fn(Option<String>) -> Box<dyn Action>,
    ) -> Vec<Option<MenuItem>> {
        let default = rust_i18n::t!("kanban.dialog.default").into_owned();
        let note = self.agent.as_ref().and_then(|pick| self.catalog_note(pick.agent));
        let mut items: Vec<_> = std::iter::once((None, default))
            .chain(options.into_iter().map(|(value, label)| (Some(value), label)))
            .map(|(value, label)| {
                let checked = value.as_deref() == current;
                Some(MenuItem::checkbox(label, None, checked, action(value)))
            })
            .collect();
        if let Some(note) = note {
            items.extend([None, Some(MenuItem::label(note, None, None))]);
        }
        items
    }

    /// 第 `agent` 个 agent 的模型清单还没问到、或者问到的是空的时说的话：正在问、找不到这个命令、问不到，或者它
    /// 没给出模型（多半是没登录、没配密钥）。
    fn catalog_note(&self, agent: usize) -> Option<String> {
        let empty = match cached(&LAUNCHERS[agent]) {
            Some(catalog) if !catalog.models.is_empty() => return None,
            known => known.is_some(),
        };
        let key = match (self.loading.contains(&agent), self.failed.get(&agent)) {
            (true, _) => "kanban.dialog.loading_models",
            (false, Some(Unanswered::Missing)) => "kanban.dialog.agent_missing",
            (false, _) if empty => "kanban.dialog.empty_models",
            (false, _) => "kanban.dialog.no_models",
        };
        Some(rust_i18n::t!(key, agent = LAUNCHERS[agent].label).into_owned())
    }

    /// 挑着的 agent 的模型菜单。
    fn model_items(&self) -> Vec<Option<MenuItem>> {
        let Some(pick) = &self.agent else { return Vec::new() };
        let options = cached(pick.launcher()).map_or_else(Vec::new, |catalog| {
            catalog.models.iter().map(|model| (model.id.clone(), model.label.clone())).collect()
        });
        let action = |value| -> Box<dyn Action> { Box::new(PickTodoModel { value }) };
        self.options_items(options, pick.model.as_deref(), action)
    }

    /// 挑着的模型的强度菜单。
    fn effort_items(&self) -> Vec<Option<MenuItem>> {
        let Some(pick) = &self.agent else { return Vec::new() };
        let options = cached(pick.launcher()).map_or_else(Vec::new, |catalog| {
            catalog.efforts(pick.model.as_deref()).iter().map(|effort| (effort.clone(), effort.clone())).collect()
        });
        let action = |value| -> Box<dyn Action> { Box::new(PickTodoEffort { value }) };
        self.options_items(options, pick.effort.as_deref(), action)
    }

    /// 工作区菜单：各工作区，最后是另选目录。
    fn workspace_items(&self) -> Vec<Option<MenuItem>> {
        let mut items: Vec<_> = self
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, (dir, name))| {
                let action = Box::new(PickTodoWorkspace { index: Some(index) });
                Some(
                    MenuItem::checkbox(name.to_string(), None, *dir == self.dir, action).with_tooltip(display_dir(dir)),
                )
            })
            .collect();
        let browse = rust_i18n::t!("kanban.dialog.choose_dir").into_owned();
        items.extend([None, Some(MenuItem::label(browse, None, Some(Box::new(PickTodoWorkspace { index: None }))))]);
        items
    }

    /// 框 `combo` 按下就开关它的菜单。菜单开着时在捕获阶段就关掉、不再往下传：不然菜单自己的「点到外面就关」
    /// 先关了，按下又把它打开。
    fn opens_menu(combo: Combo, element: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        element
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                if this.menu_for == Some(combo) && this.dropdown_open() {
                    cx.stop_propagation();
                    this.close_menu(window, cx);
                }
            }))
            .on_press_down(cx, move |this, window, cx| this.toggle_menu(combo, window, cx))
    }

    /// 挂在框 `combo` 下面的菜单，开着的不是它的菜单时为空。
    fn combo_menu(&self, combo: Combo, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let bottom = self.combo_bottoms[combo as usize].get();
        self.menu_for.filter(|open| *open == combo).and_then(|_| self.render_dropdown_menu(bottom, window, cx))
    }

    /// 换 agent：模型和强度回到默认；换成自己写的命令时先填上刚才挑的那条，焦点给它。
    fn pick_agent(&mut self, action: &PickTodoAgent, window: &mut Window, cx: &mut Context<Self>) {
        // 挑的还是同一家（`fetch_models` 可能已经换成了它的另一个版本）就不换，只再问一遍清单：问不到时登录了、
        // 装好了回来再点一下就行。
        let picked = action.index.and_then(|index| LAUNCHERS.get(index)).map(|spec| spec.label);
        if let Some(pick) = &self.agent
            && picked == Some(pick.launcher().label)
        {
            self.fetch_models(pick.agent, cx);
            return;
        }
        match action.index.filter(|index| *index < LAUNCHERS.len()) {
            Some(agent) => {
                self.agent = Some(Pick { agent, model: None, effort: None });
                self.fetch_models(agent, cx);
            }
            None => {
                if let Some(pick) = self.agent.take() {
                    let text = pick.command();
                    self.command.update(cx, |input, cx| input.set_query(text, cx));
                }
                window.focus(&self.command.focus_handle(cx), cx);
            }
        }
        cx.notify();
    }

    /// 换模型：挑着的强度新模型用不了、或者换回默认模型而强度要跟着模型写时，强度回到默认。
    fn pick_model(&mut self, action: &PickTodoModel, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(pick) = &mut self.agent {
            pick.model.clone_from(&action.value);
            let usable = |effort: &String| match cached(pick.launcher()) {
                Some(catalog) => catalog.efforts(pick.model.as_deref()).contains(effort),
                None => true,
            };
            if pick.effort.as_ref().is_some_and(|effort| !usable(effort))
                || (pick.model.is_none() && pick.launcher().effort_needs_model)
            {
                pick.effort = None;
            }
            cx.notify();
        }
    }

    fn pick_effort(&mut self, action: &PickTodoEffort, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(pick) = &mut self.agent {
            pick.effort.clone_from(&action.value);
            cx.notify();
        }
    }

    fn pick_workspace(&mut self, action: &PickTodoWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        match action.index.and_then(|index| self.workspaces.get(index)) {
            Some((dir, _)) => {
                if *dir != self.dir {
                    self.dir = dir.clone();
                    self.dir_changed(cx);
                }
                cx.notify();
            }
            None => self.browse_dir(window, cx),
        }
    }

    /// 挑着的 agent 能以自定义 agent 的身份跑（`Launcher::subagent_arg`）。
    fn takes_subagent(&self) -> bool {
        self.agent.as_ref().is_some_and(|pick| pick.launcher().subagent_arg.is_some())
    }

    /// 能「保存并开始」：是任务，还没开始过也没做完。
    fn startable(&self) -> bool {
        match &self.editing {
            None => !self.schedule,
            Some(Editing::Todo(todo)) => todo.started.is_none() && todo.done_at.is_none(),
            Some(Editing::Schedule(_)) => false,
        }
    }

    /// 按对话框里填的发请求；填得不对时在对话框里说。宿主办成了就关掉对话框，`start` 时交给看板接着开始；
    /// 没办成时说原因。
    /// 是写到一半的新建任务那一页，离开时能收起来留着；正在存的不算。
    pub(crate) fn draft(&self) -> bool {
        self.page && self.editing.is_none() && !self.saving
    }

    /// 标题输入框的焦点，收起来的那一页重新摆出来时交给它。
    pub(crate) fn title_focus(&self, cx: &App) -> FocusHandle {
        self.title.focus_handle(cx)
    }

    fn save(&mut self, start: bool, cx: &mut Context<Self>) {
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
        // 目录在不在放到单独的线程上查（`fs_probe::off_thread`），查过了再发；起不了线程时交给宿主查。开始过的
        // 任务目录改不了，宿主也允许原目录没了（agent 还在工作树里跑）时只改文字，不查。
        let dir = (!self.locked).then(|| self.dir.clone());
        let dir_ok = crate::fs_probe::off_thread(move || dir.is_none_or(|dir| dir.is_dir()), true);
        cx.spawn(async move |this, cx| {
            let answer = if dir_ok.await {
                let Ok(answer) = this.update(cx, |_, cx| store::ask(cx, message)) else {
                    return;
                };
                answer.await
            } else {
                Err(anyhow::anyhow!(rust_i18n::t!("kanban.dialog.bad_dir").into_owned()))
            };
            this.update(cx, |dialog, cx| {
                match answer {
                    Ok(answer) => {
                        let start = match (answer, &dialog.editing) {
                            (HostMsg::TodoAdded { id, .. }, _) => Some(id),
                            (_, Some(Editing::Todo(todo))) => Some(todo.id.clone()),
                            _ => None,
                        }
                        .filter(|_| start);
                        cx.emit(DialogEvent::Saved { start });
                    }
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focus_menu(window, cx);
        let dialog = &*self;
        let theme = cx.theme();
        let hover_bg = theme.mix(0.10);
        let primary_bg = theme.mix(0.16);
        let (fg_h, bg_h, input_border, destructive) =
            (theme.foreground, theme.background, theme.input, theme.destructive);
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
        // 点了弹出菜单的框：左边是挑着的值，右边一个下拉箭头，菜单（`menu`）挂在它下面。
        let open = |kind: Combo| dialog.menu_for == Some(kind) && dialog.dropdown_open();
        let combo = |id: &'static str, kind: Combo, label: String, value: String, enabled: bool, menu: Option<Div>| {
            let bottoms = dialog.combo_bottoms.clone();
            div()
                .id(id)
                .role(Role::ComboBox)
                .aria_label(label)
                .aria_value(value.clone())
                .aria_expanded(open(kind))
                .relative()
                .min_w_0()
                .min_h(scaled(28.))
                .px(px(8.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(6.))
                .bg(bg_h)
                .border_1()
                // 和上下的输入框（`Input`）一样的边框。
                .border_color(input_border)
                .child(div().flex_1().min_w_0().truncate().child(value))
                .child(svg().path(CHEVRON_DOWN_ICON).flex_none().size(px(12.)).text_color(fg_h.opacity(0.6)))
                .when(enabled, |combo| combo.cursor_pointer().hover(move |combo| combo.bg(hover_bg)))
                .when(!enabled, |combo| combo.opacity(0.6))
                .aria_disabled(!enabled)
                // 记下下沿，菜单的最大高度按它算。
                .child(
                    canvas(
                        move |bounds, _, _| bottoms[kind as usize].set(f32::from(bounds.bottom())),
                        |_, (), _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .children(menu)
        };
        let workspace_label = rust_i18n::t!("kanban.dialog.workspace").into_owned();
        let workspace_text = dialog
            .workspaces
            .iter()
            .find(|(dir, _)| *dir == dialog.dir)
            .map_or_else(|| display_dir(&dialog.dir), |(_, name)| name.to_string());
        let menu = dialog.combo_menu(Combo::Workspace, window, cx);
        let workspace = combo(
            "todo-dialog-workspace",
            Combo::Workspace,
            workspace_label.clone(),
            workspace_text,
            !dialog.locked,
            menu,
        );
        let workspace = if dialog.locked { workspace } else { Self::opens_menu(Combo::Workspace, workspace, cx) };
        let default = rust_i18n::t!("kanban.dialog.default").into_owned();
        let agent_label = rust_i18n::t!("kanban.dialog.agent").into_owned();
        let agent_text = dialog.agent.as_ref().map_or_else(
            || rust_i18n::t!("kanban.dialog.custom").into_owned(),
            |pick| pick.launcher().label.to_owned(),
        );
        let menu = dialog.combo_menu(Combo::Agent, window, cx);
        let agent = Self::opens_menu(
            Combo::Agent,
            combo("todo-dialog-agent", Combo::Agent, agent_label.clone(), agent_text, true, menu),
            cx,
        );
        // 挑了 agent 时并排挑模型和强度；自己写命令时下面是命令框。
        let agent_row = match &dialog.agent {
            Some(pick) => {
                let launcher = pick.launcher();
                let catalog = cached(launcher);
                // 没有模型或强度参数的 agent 不画那一栏；强度接在模型名后面的，挑了模型才能挑强度。
                let model = launcher.model_arg.map(|_| {
                    let text = pick.model.as_deref().map_or_else(
                        || default.clone(),
                        |model| catalog.as_deref().map_or(model, |catalog: &Catalog| catalog.label(model)).to_owned(),
                    );
                    let label = rust_i18n::t!("kanban.dialog.model").into_owned();
                    let menu = dialog.combo_menu(Combo::Model, window, cx);
                    Self::opens_menu(
                        Combo::Model,
                        combo("todo-dialog-model", Combo::Model, label, text, true, menu),
                        cx,
                    )
                });
                let effort = launcher.effort_arg.map(|_| {
                    let enabled = pick.model.is_some() || !launcher.effort_needs_model;
                    let text = pick.effort.clone().filter(|_| enabled).unwrap_or_else(|| default.clone());
                    let label = rust_i18n::t!("kanban.dialog.effort").into_owned();
                    let menu = dialog.combo_menu(Combo::Effort, window, cx);
                    let effort = combo("todo-dialog-effort", Combo::Effort, label, text, enabled, menu);
                    if enabled { Self::opens_menu(Combo::Effort, effort, cx) } else { effort }
                });
                let column = |key: &str, child: gpui::Stateful<gpui::Div>| {
                    div().flex_1().min_w_0().child(field(rust_i18n::t!(key).into_owned(), child.into_any_element()))
                };
                div()
                    .flex()
                    .gap(px(8.))
                    .child(column("kanban.dialog.agent", agent))
                    .children(model.map(|model| column("kanban.dialog.model", model)))
                    .children(effort.map(|effort| column("kanban.dialog.effort", effort)))
                    .into_any_element()
            }
            None => div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(field(agent_label, agent.into_any_element()))
                .child(input(&dialog.command))
                .into_any_element(),
        };
        let none = rust_i18n::t!("kanban.dialog.none").into_owned();
        let skills_label = rust_i18n::t!("kanban.dialog.skills").into_owned();
        let skills_text = if dialog.skills.is_empty() { none.clone() } else { dialog.skills.join(", ") };
        let menu = dialog.combo_menu(Combo::Skills, window, cx);
        let skills = Self::opens_menu(
            Combo::Skills,
            combo("todo-dialog-skills", Combo::Skills, skills_label.clone(), skills_text, true, menu),
            cx,
        );
        // 挑着的 agent 有自定义 agent 的参数、或者已经挑了（自己写的命令也可能用得上，存着的不能悄悄丢掉）时，
        // 和 skill 并排挑。
        let subagent = (dialog.takes_subagent() || dialog.subagent.is_some()).then(|| {
            let label = rust_i18n::t!("kanban.dialog.subagent").into_owned();
            let text = dialog.subagent.clone().unwrap_or_else(|| none.clone());
            let menu = dialog.combo_menu(Combo::Subagent, window, cx);
            let subagent = combo("todo-dialog-subagent", Combo::Subagent, label.clone(), text, true, menu);
            let subagent = Self::opens_menu(Combo::Subagent, subagent, cx);
            div().flex_1().min_w_0().child(field(label, subagent.into_any_element()))
        });
        let skills_row = div()
            .flex()
            .gap(px(8.))
            .child(div().flex_1().min_w_0().child(field(skills_label, skills.into_any_element())))
            .children(subagent);
        // 附件一个一块，写文件名，报给辅助工具的是整个路径；后面跟着添加的按钮。
        let chips: Vec<_> = dialog
            .attachments
            .iter()
            .map(|path| {
                let name = base_name(path);
                let remove = rust_i18n::t!("kanban.dialog.remove_attachment", name = name.as_str()).into_owned();
                let id = format!("todo-attachment-{}", path.display());
                let gone = path.clone();
                div()
                    .id(gpui::ElementId::Name(id.clone().into()))
                    .role(Role::Group)
                    .aria_label(display_dir(path))
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .pl(px(8.))
                    .rounded(px(4.))
                    .bg(primary_bg)
                    .child(div().max_w(scaled(220.)).truncate().child(name))
                    .child(
                        super::icon_button(gpui::ElementId::Name(format!("{id}-remove").into()), CLOSE_ICON, remove)
                            .on_press(cx, move |dialog, _, cx| {
                                dialog.attachments.retain(|path| *path != gone);
                                cx.notify();
                            }),
                    )
            })
            .collect();
        let add_attachment =
            Button::new("todo-dialog-attach", rust_i18n::t!("kanban.dialog.add_attachment").into_owned())
                .variant(ButtonVariant::Ghost)
                .icon(PLUS_ICON)
                .icon_size(12.)
                .h_auto()
                .px(px(8.))
                .py(px(3.))
                .cursor_pointer()
                .on_press(cx, |dialog, window, cx| dialog.browse_attachments(window, cx));
        let attachments = field(
            rust_i18n::t!("kanban.dialog.attachments").into_owned(),
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .children(chips)
                .child(add_attachment)
                .into_any_element(),
        );
        let auto_label = rust_i18n::t!("kanban.dialog.auto").into_owned();
        let auto = Switch::new("todo-dialog-auto", auto_label.clone(), dialog.auto)
            .label(auto_label)
            .cursor_pointer()
            .on_press(cx, |dialog, _, cx| {
                dialog.auto = !dialog.auto;
                cx.notify();
            });
        // 挑着的模板要另外填的那一项，放在最上面。
        let template_input = dialog
            .template
            .and_then(|index| dialog.templates[index].input.clone())
            .map(|label| field(label, input(&dialog.input).into_any_element()));
        let worktree_label = rust_i18n::t!("kanban.dialog.worktree").into_owned();
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
                pill(id.into(), rust_i18n::t!(label).into_owned(), dialog.kind == Some(kind), Role::RadioButton)
                    .on_press(cx, move |dialog, window, cx| {
                        dialog.kind = Some(kind);
                        let input = match kind {
                            Kind::Once => dialog.at.clone(),
                            Kind::Daily => dialog.clock.clone(),
                            Kind::Every => dialog.minutes.clone(),
                        };
                        window.focus(&input.focus_handle(cx), cx);
                        cx.notify();
                    })
            };
            let when_label = rust_i18n::t!("kanban.dialog.when").into_owned();
            let kinds = div()
                .id("todo-dialog-when")
                .role(Role::RadioGroup)
                .aria_label(when_label.clone())
                .flex()
                .p(px(2.))
                .gap(px(2.))
                .rounded(px(6.))
                .bg(bg_h)
                .child(kind("todo-dialog-once", Kind::Once, "kanban.dialog.kind_once", cx))
                .child(kind("todo-dialog-daily", Kind::Daily, "kanban.dialog.kind_daily", cx))
                .child(kind("todo-dialog-every", Kind::Every, "kanban.dialog.kind_every", cx));
            let when_input = match dialog.kind {
                Some(Kind::Once) => input(&dialog.at).into_any_element(),
                Some(Kind::Daily) => input(&dialog.clock).into_any_element(),
                Some(Kind::Every) => input(&dialog.minutes).into_any_element(),
                None => {
                    let hint = rust_i18n::t!("kanban.dialog.unknown_hint").into_owned();
                    div()
                        .id("todo-dialog-unknown")
                        .role(Role::Label)
                        .aria_label(hint.clone())
                        .text_size(FontSize::Small)
                        .text_color(fg_h.opacity(0.55))
                        .child(hint)
                        .into_any_element()
                }
            };
            let days = (dialog.kind == Some(Kind::Daily)).then(|| {
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
                        .child(rust_i18n::t!("kanban.dialog.days_hint").into_owned()),
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
                .child(when_input)
                .children(days)
        });
        let title = match (&dialog.editing, dialog.schedule) {
            (Some(_), false) => rust_i18n::t!("kanban.edit"),
            (Some(_), true) => rust_i18n::t!("kanban.edit_schedule"),
            (None, false) => rust_i18n::t!("kanban.new_todo"),
            (None, true) => rust_i18n::t!("kanban.new_schedule"),
        }
        .into_owned();
        let cancel = button("todo-dialog-cancel", rust_i18n::t!("kanban.cancel").into_owned(), ButtonVariant::Ghost)
            .on_press(cx, |_, _, cx| cx.emit(DialogEvent::Dismissed));
        let save = button("todo-dialog-save", rust_i18n::t!("kanban.save").into_owned(), ButtonVariant::Secondary)
            .when(dialog.saving, |button| button.opacity(0.5))
            .on_press(cx, |this, _, cx| this.save(false, cx));
        let save_and_start = dialog.startable().then(|| {
            button(
                "todo-dialog-save-start",
                rust_i18n::t!("kanban.save_and_start").into_owned(),
                ButtonVariant::Default,
            )
            .when(dialog.saving, |button| button.opacity(0.5))
            .on_press(cx, |this, _, cx| this.save(true, cx))
        });
        let page = dialog.page;
        // 新建任务那一页左边的模板，空白的在最前面。
        let templates = (page && dialog.editing.is_none()).then(|| {
            let label = rust_i18n::t!("kanban.dialog.templates").into_owned();
            let blank = rust_i18n::t!("kanban.dialog.template_blank").into_owned();
            let rows: Vec<_> = std::iter::once((None, "blank".to_owned(), blank))
                .chain(
                    dialog.templates.iter().enumerate().map(|(index, t)| (Some(index), t.id.clone(), t.name.clone())),
                )
                .map(|(index, id, name)| {
                    let id = gpui::ElementId::Name(format!("todo-template-{id}").into());
                    pill(id, name, dialog.template == index, Role::RadioButton)
                        .truncate()
                        .on_press(cx, move |dialog, window, cx| dialog.pick_template(index, window, cx))
                })
                .collect();
            let hint = runode_paths::Dirs::from_env().todo_templates_dir().map(|dir| {
                let hint = rust_i18n::t!("kanban.dialog.templates_hint", dir = display_dir(&dir)).into_owned();
                div()
                    .id("todo-templates-hint")
                    .role(Role::Label)
                    .aria_label(hint.clone())
                    .pt(px(8.))
                    .text_size(FontSize::Small)
                    .text_color(fg_h.opacity(0.55))
                    .child(hint)
            });
            div()
                .id("todo-templates")
                .role(Role::RadioGroup)
                .aria_label(label.clone())
                .flex_none()
                .w(scaled(200.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .overflow_y_scroll()
                .child(div().pb(px(6.)).text_color(fg_h.opacity(0.7)).child(label))
                .children(rows)
                .children(hint)
        });
        let panel = Dialog::new("todo-dialog", title.clone())
            .when(page, |panel| panel.role(Role::Group))
            .track_focus(&dialog.focus)
            .on_action(cx.listener(Self::pick_agent))
            .on_action(cx.listener(Self::pick_model))
            .on_action(cx.listener(Self::pick_effort))
            .on_action(cx.listener(Self::pick_workspace))
            .on_action(cx.listener(Self::pick_skill))
            .on_action(cx.listener(Self::pick_subagent))
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    cx.emit(DialogEvent::Dismissed);
                }
            }))
            .when(page, |panel| panel.flex_1().min_w_0().max_w(px(720.)))
            .when(!page, |panel| panel.w(px(480.)).max_w_full())
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
                    .children(template_input)
                    .child(field(
                        rust_i18n::t!("kanban.dialog.title_label").into_owned(),
                        input(&dialog.title).into_any_element(),
                    ))
                    .child(field(
                        rust_i18n::t!("kanban.dialog.prompt").into_owned(),
                        Textarea::new(&dialog.prompt)
                            .min_h(scaled(28.))
                            .py(px(4.))
                            .flex()
                            .items_center()
                            .into_any_element(),
                    ))
                    .children(when)
                    .child(field(workspace_label, workspace.into_any_element()))
                    .child(agent_row)
                    .child(skills_row)
                    .child(attachments)
                    .child(div().flex().flex_wrap().gap(px(16.)).child(worktree).child(auto)),
            )
            .children(dialog.error.clone().map(|error| {
                div()
                    .id("todo-dialog-error")
                    .role(Role::Label)
                    .aria_label(error.clone())
                    .text_color(destructive)
                    .child(error)
            }))
            .child(
                div().pt(px(4.)).flex().justify_end().gap(px(8.)).child(cancel).child(save).children(save_and_start),
            );
        if page {
            return div()
                .id("todo-page")
                .size_full()
                .flex()
                .gap(px(16.))
                .children(templates)
                .child(panel)
                .into_any_element();
        }
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
            .into_any_element()
    }
}

impl TodoDialog {
    /// 按填的内容造出要发的请求；填得不对时是给人看的原因。
    fn request(&self, cx: &App) -> Result<store::Step, String> {
        let mut prompt = self.prompt.read(cx).text().trim().to_owned();
        let mut typed = self.title.read(cx).query().trim().to_owned();
        if let Some(label) = self.template.and_then(|index| self.templates[index].input.as_ref()) {
            let input = self.input.read(cx).query().trim();
            if input.is_empty() {
                return Err(rust_i18n::t!("kanban.dialog.need_input", label = label).into_owned());
            }
            prompt = templates::fill(&prompt, input);
            typed = templates::fill(&typed, input);
        }
        let title = if typed.is_empty() { first_line(&prompt).to_owned() } else { typed };
        if title.is_empty() {
            return Err(rust_i18n::t!("kanban.dialog.need_title").into_owned());
        }
        let agent = match &self.agent {
            Some(pick) => pick.command(),
            None => self.command.read(cx).query().trim().to_owned(),
        };
        if agent.is_empty() {
            return Err(rust_i18n::t!("kanban.dialog.need_agent").into_owned());
        }
        let spec = TodoSpec {
            title,
            prompt,
            dir: self.dir.clone(),
            agent,
            worktree: self.worktree,
            skills: self.skills.clone(),
            subagent: self.subagent.clone(),
            attachments: self.attachments.clone(),
            auto: self.auto,
        };
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
            // 只有编辑读不懂的定时、还没选一种时为空：下面就不发 `when`，规则照原样留着。
            None => When::Unknown,
            Some(Kind::Once) => {
                let original = match &self.editing {
                    Some(Editing::Schedule(Schedule { when: When::Once { at }, .. })) => Some(*at),
                    _ => None,
                };
                When::Once {
                    at: once_at(self.at.read(cx).query(), original)
                        .ok_or_else(|| rust_i18n::t!("kanban.dialog.bad_once").into_owned())?,
                }
            }
            Some(Kind::Daily) => {
                let (hour, minute) = parse_clock(self.clock.read(cx).query())
                    .ok_or_else(|| rust_i18n::t!("kanban.dialog.bad_daily").into_owned())?;
                When::Daily { hour, minute, days: self.days }
            }
            Some(Kind::Every) => When::Every {
                minutes: parse_minutes(self.minutes.read(cx).query())
                    .ok_or_else(|| rust_i18n::t!("kanban.dialog.bad_every").into_owned())?,
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
    let key = format!("kanban.weekday.{day}");
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
        // 时间栏没改时带秒的原值照旧，改了才按填的算。
        assert_eq!(once_at("2026-10-13 14:30", Some(at + 17)), Some(at + 17));
        assert_eq!(once_at("2026-10-13 14:31", Some(at + 17)), Some(at + 60));
        assert_eq!(once_at("2026-10-13 14:30", None), Some(at));
        for now in [at, at + 59, at + 30 * 60, at + 89 * 60 + 59] {
            let next = next_local_hour(now);
            assert!(next > now && next - now <= 3600, "{now} -> {next}");
            assert_eq!(super::local(next).unwrap().minute, 0, "{now} -> {next}");
        }
    }
}
