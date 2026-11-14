//! 新建、编辑任务和定时的对话框：标题、提示词、在哪个工作区或目录里做、agent 和它的模型、强度（见 `runode_agent::models`），
//! 要用的 skill、以哪个自定义 agent 的身份跑（`skills`）、附上的文件，免不免确认，要不要独立的工作树，做完要不要自动标完成，
//! 开始后多久没做完就算没做成（时限）；新建的和还没开始的任务另挑什么时候做（`StartMode`）：马上开始、放进待办等我来
//! 开始、排队（可以指定先等另一个任务做完），排队的存好以后另发 `ClientMsg::QueueTodo`；新建时还能挑定时，定时多一组
//! 什么时候跑（一次、每天某时可选星期几、每隔几分钟、cron 表达式），下面列出接下来几次到点的时刻。时间都按本机的本地
//! 时间填、显示（`runode_todo::LocalTime`）。底栏用一句话（`summary_in`）说照现在填的会怎么做。填得不对时在对话框里说，
//! 不发请求；宿主那边还会再查一遍。时限、排队和 cron 要宿主认得相应的看板级别（`store::host_below`），不认得时也在
//! 对话框里说，不发。
//!
//! 对话框单独是一个视图（`TodoDialog`），由看板开、存在看板身上，收起、存好了经 `DialogEvent` 告诉看板。它盖住整个
//! 窗口，由 app 画在窗口的最上层（`Kanban::dialog`）。新建时标题栏的「模板」菜单列出模板（`templates`），挑一个就照它
//! 填好，带 `{{input}}` 的另要填一项（issue、PR 号），保存时换进去。

use crate::ui::dialog::{backdrop, is_dismiss_key};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, LazyLock, Mutex},
};

use crate::kanban::{
    skills::{self, Found},
    templates::{self, Template},
    when,
};
use crate::ui::{
    a11y::{Disable as _, Press, PressDown as _, StaticText},
    base_name,
    button::{Button, ButtonVariant},
    chip::Chip,
    dialog::Dialog,
    display_dir,
    font_size::{FontSize, scaled},
    icons::{CHECK_ICON, CLOSE_ICON, FOLDER_ICON, PLUS_ICON, SPARKLE_ICON},
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    menu::{Menu, MenuHost, MenuItem},
    panel::TITLEBAR_HEIGHT,
    scrollbar::scrollbar,
    segmented::Segmented,
    select::SelectTrigger,
    textarea::{Textarea, TextareaEvent, TextareaState},
    theme::ActiveTheme as _,
};
use gpui::{
    Action, App, Axis, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyDownEvent,
    MouseButton, PathPromptOptions, Render, Role, ScrollHandle, SharedString, Subscription, Window, canvas, div,
    prelude::*, px, svg,
};
use runode_protocol::{
    ClientMsg, HostMsg,
    todo::{DEFAULT_AGENT, Outcome, Schedule, Todo, TodoSpec, When},
};

use crate::kanban::store;
use runode_agent::models::{Catalog, LAUNCHERS, Launcher, Pick, Unanswered};
use runode_todo::LocalTime;

/// 各栏右边给滚动条留的宽度，伸进面板的内边距里，比面板的内边距窄。
const SCROLLBAR_GUTTER: f32 = 14.;

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

/// 「排在别的后面」菜单的一项：`queued` 为假是不排队；排队时 `after` 是要先等做完的任务，为空是不等哪个。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(crate) struct PickTodoQueue {
    queued: bool,
    after: Option<String>,
}

/// 「模板」菜单的一项：第 `index` 个模板，为空时是空白的。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(crate) struct PickTodoTemplate {
    index: Option<usize>,
}

/// 「模板」菜单的最后一项：打开模板目录。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(crate) struct OpenTodoTemplates;

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
    After,
    Template,
}

/// 定时跑的方式，对话框里四选一。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Once,
    Daily,
    Every,
    Cron,
}

/// 新建的和还没开始的任务什么时候做，对话框里分段挑一段；由 `TodoDialog` 的 `schedule`、`start`、`queued` 定
/// （`start_mode_of`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartMode {
    /// 存好就开始。
    Now,
    /// 放进待办，等用户点开始。
    Manual,
    /// 排队：要等的任务（`TodoDialog::after`，可以不等哪个）做完、在跑的少于上限时开始。
    Queue,
    /// 存成定时，只有新建时能挑。
    Schedule,
}

/// 时限现成的几档，分钟数；别的由用户自己填。
const LIMIT_PRESETS: [u32; 3] = [15, 30, 60];

/// 对话框面板的宽度。
const DIALOG_WIDTH: f32 = 660.;

/// 左栏标签的宽度，按基准字号的点数。
const LABEL_WIDTH: f32 = 110.;

/// 下拉框、小签和左栏标签共用的一行高，按基准字号的点数。
const CONTROL_HEIGHT: f32 = 30.;

/// 新建、编辑任务和定时的对话框，见模块说明。
pub struct TodoDialog {
    /// 是定时（否则是任务）；新建时随「什么时候做」选没选定时变。
    pub(crate) schedule: bool,
    editing: Option<Editing>,
    /// 新建时「模板」菜单里列的模板，后台读好前为空；挑着的是第几个，空白的为空。
    templates: Vec<Template>,
    template: Option<usize>,
    /// 模板目录，没有家目录时为空。
    templates_dir: Option<PathBuf>,
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
    /// 本机终端里 shell 报告的 PATH，没有这样的终端时为空。
    shell_path: Option<OsString>,
    /// 各 agent（`LAUNCHERS` 里第几个）的程序在 PATH 上找不找得到；后台找好之前为空，菜单里全列。
    installed: Option<Vec<bool>>,
    /// 正在问模型清单的 agent（`LAUNCHERS` 里第几个），和没问到的、为什么没问到。
    loading: HashSet<usize>,
    failed: HashMap<usize, Unanswered>,
    /// 标题和按钮之间那些栏的滚动位置，滚动条读它。
    fields_scroll: ScrollHandle,
    dir: PathBuf,
    /// `dir` 所在仓库当前的分支，写在项目框里；后台读好前、不在仓库里时为空。
    branch: Option<String>,
    worktree: bool,
    /// 挑上的 skill、自定义 agent 和附上的文件，见 `TodoSpec`；`found` 是在 `dir` 能找到的 skill 和自定义 agent。
    skills: Vec<String>,
    subagent: Option<String>,
    attachments: Vec<PathBuf>,
    found: Found,
    auto: bool,
    auto_done: bool,
    /// 时限，分钟数，空着是不限（`TodoSpec::time_limit`）。
    time_limit: Entity<InputState>,
    /// 时限选了「自己定」：露出时限的输入框，填的正好是 `LIMIT_PRESETS` 里的一档也不跳回那一档。
    custom_limit: bool,
    /// 还没开始的任务选了马上开始：存好就交给看板开始（`DialogEvent::Saved` 的 `start`），排队的不算。
    start: bool,
    /// 还没开始的任务要不要排队、要先等哪个任务做完（`Todo::queued`、`Todo::after`），存好以后经
    /// `ClientMsg::QueueTodo` 交给宿主。
    queued: bool,
    after: Option<String>,
    /// 任务开始过了：目录和要不要工作树不能再改。
    locked: bool,
    /// 读不懂的定时规则（`When::Unknown`）在用户选一种之前为空，存的时候规则照原样留着。
    kind: Option<Kind>,
    /// 一次：`YYYY-MM-DD HH:MM`；每天：`HH:MM`；每隔：分钟数；cron：五段的表达式。四个框各记各的，切来切去不丢。
    at: Entity<InputState>,
    clock: Entity<InputState>,
    minutes: Entity<InputState>,
    cron: Entity<InputState>,
    /// 每天跑时只在这几天（第 0 位是星期日），全不选是每天。
    days: u8,
    error: Option<String>,
    /// 填得不对的那一栏，下一次画时把焦点交给它。
    focus_wrong: Option<FocusHandle>,
    saving: bool,
    /// 菜单里点了一项后焦点交给它，动作从这里派发到对话框。
    focus: FocusHandle,
    menu: Option<Menu>,
    /// 开着的菜单是哪个框的，菜单挂在它下面。
    menu_for: Option<Combo>,
    /// 各个框上一帧的下沿离窗口顶多远（按 `Combo` 的次序），菜单最高到窗口底边。
    combo_bottoms: Rc<[Cell<f32>; 8]>,
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

/// 一次性定时的时间栏 `text` 是哪一刻。和编辑前的 `original` 显示得一样（没改）时照原样，不重新解析：
/// `full_time` 不带秒，夏令时回拨那个钟点也可能换成另一刻。
fn once_at(text: &str, original: Option<u64>) -> Option<u64> {
    original.filter(|at| full_time(*at) == text.trim()).or_else(|| LocalTime::parse(text)?.unix())
}

/// 至少一分钟的分钟数。
fn parse_minutes(text: &str) -> Option<u32> {
    text.trim().parse().ok().filter(|minutes| *minutes >= 1)
}

/// 时限栏：空着是不限，否则是至少一分钟的分钟数；写得不对时为 `Err`。
fn parse_time_limit(text: &str) -> Result<Option<u32>, ()> {
    if text.trim().is_empty() { Ok(None) } else { parse_minutes(text).map(Some).ok_or(()) }
}

/// 时限栏写的 `text` 落在哪一档小签上：`Some(None)` 是不限，`Some(Some(n))` 是 `LIMIT_PRESETS` 里的 n 分钟，
/// 为空是自己定（`custom` 选着、或者写的不是现成的一档）。
fn limit_preset(text: &str, custom: bool) -> Option<Option<u32>> {
    match parse_time_limit(text) {
        _ if custom => None,
        Ok(None) => Some(None),
        Ok(Some(minutes)) if LIMIT_PRESETS.contains(&minutes) => Some(Some(minutes)),
        _ => None,
    }
}

/// `when` 从 `now` 起接下来最多三次到点的时刻。
fn next_fires(when: &When, now: u64) -> Vec<u64> {
    std::iter::successors(runode_todo::next_fire(when, now), |at| runode_todo::next_fire(when, *at)).take(3).collect()
}

/// 底栏那句话开头说的什么时候做。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lead<'a> {
    /// 开始过的任务：不说什么时候。
    Plain,
    Now,
    Manual,
    Queued,
    /// 等第几号任务做完；它已经不在看板上时为空。
    After(Option<u64>),
    /// 一次性的定时，到点的时刻（`when::fire_text`）。
    Once(&'a str),
    /// 重复的定时，规则的说法（`when::describe_when`）。
    Repeat(&'a str),
    /// 定时的规则还没填对或者读不懂，说不出什么时候。
    Due,
}

/// 底栏那句话要说的：什么时候做、交给谁（模型没挑时为空）、在哪个项目里、开不开工作树、时限几分钟。
struct Summary<'a> {
    lead: Lead<'a>,
    agent: &'a str,
    model: Option<&'a str>,
    project: &'a str,
    worktree: bool,
    limit: Option<u32>,
}

/// 按 `locale` 把 `summary` 拼成一整句：各种组合都是 locale 里的整句，代码里不拼标点。
fn summary_in(summary: &Summary, locale: &str) -> String {
    let agent = match summary.model {
        Some(model) => {
            rust_i18n::t!("kanban.dialog.summary.agent_model", locale = locale, agent = summary.agent, model = model)
                .into_owned()
        }
        None => summary.agent.to_owned(),
    };
    let body_key = match (summary.worktree, summary.limit.is_some()) {
        (false, false) => "kanban.dialog.summary.here",
        (false, true) => "kanban.dialog.summary.here_limit",
        (true, false) => "kanban.dialog.summary.worktree",
        (true, true) => "kanban.dialog.summary.worktree_limit",
    };
    let limit = summary.limit.map(|minutes| when::limit_text_in(minutes, locale)).unwrap_or_default();
    let body =
        rust_i18n::t!(body_key, locale = locale, agent = agent, project = summary.project, limit = limit).into_owned();
    let lead = |key: &str| rust_i18n::t!(key, locale = locale, body = body).into_owned();
    match summary.lead {
        Lead::Plain => body.clone(),
        Lead::Now => lead("kanban.dialog.summary.now"),
        Lead::Manual => lead("kanban.dialog.summary.manual"),
        Lead::Queued => lead("kanban.dialog.summary.queued"),
        Lead::After(Some(number)) => {
            rust_i18n::t!("kanban.dialog.summary.after", locale = locale, number = number, body = body).into_owned()
        }
        Lead::After(None) => lead("kanban.dialog.summary.after_gone"),
        Lead::Once(time) => {
            rust_i18n::t!("kanban.dialog.summary.once", locale = locale, time = time, body = body).into_owned()
        }
        Lead::Repeat(when) => {
            rust_i18n::t!("kanban.dialog.summary.repeat", locale = locale, when = when, body = body).into_owned()
        }
        Lead::Due => lead("kanban.dialog.summary.due"),
    }
}

/// 任务 `from` 顺着 `Todo::after` 一个个往前等，会等到任务 `me`（`from` 就是 `me` 也算）。
fn waits_for(todos: &[Todo], from: &str, me: &str) -> bool {
    let mut seen = HashSet::new();
    let mut at = Some(from);
    while let Some(id) = at {
        if id == me {
            return true;
        }
        if !seen.insert(id) {
            return false;
        }
        at = todos.iter().find(|todo| todo.id == id).and_then(|todo| todo.after.as_deref());
    }
    false
}

/// 任务 `me`（新建时为空）能排在哪些任务后面等：没做完、也没报没做成的（等它的会跟着记成没做成），不是它自己，
/// 也不会顺着等回它自己；开始了的照样能等。
fn after_candidates<'a>(todos: &'a [Todo], me: Option<&str>) -> Vec<&'a Todo> {
    todos
        .iter()
        .filter(|todo| {
            todo.done_at.is_none()
                && todo.outcome != Some(Outcome::Failed)
                && me.is_none_or(|me| !waits_for(todos, &todo.id, me))
        })
        .collect()
}

/// 存好以后要不要发 `ClientMsg::QueueTodo`、发什么：新建的（`original` 为空）只在排队时发，编辑的只在和原来的
/// `(queued, after)` 不同时发；不排队时不等谁。
fn queue_change(
    original: Option<(bool, Option<&str>)>,
    queued: bool,
    after: Option<&str>,
) -> Option<(bool, Option<String>)> {
    let after = after.filter(|_| queued);
    let changed = match original {
        None => queued,
        Some((was_queued, was_after)) => (was_queued, was_after.filter(|_| was_queued)) != (queued, after),
    };
    changed.then(|| (queued, after.map(str::to_owned)))
}

/// 对话框的 `schedule`、`start`、`queued` 落在「什么时候做」的哪一段：定时先于排队，排队先于马上开始。
fn start_mode_of(schedule: bool, start: bool, queued: bool) -> StartMode {
    match (schedule, queued, start) {
        (true, _, _) => StartMode::Schedule,
        (false, true, _) => StartMode::Queue,
        (false, false, true) => StartMode::Now,
        (false, false, false) => StartMode::Manual,
    }
}

/// 「排在别的后面」里任务 `todo` 的写法：编号和标题。
fn after_label(todo: &Todo) -> String {
    rust_i18n::t!("kanban.dialog.after_task", number = todo.number, title = todo.spec.title.as_str()).into_owned()
}

/// 提示词的第一行，标题空着时用它。
fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default()
}

/// 对话框要看板办的事。
pub(crate) enum DialogEvent {
    /// 按了 Esc、× 或点了外面：收起，焦点交回看板；新建的写了字时（`TodoDialog::worth_keeping`）留着下次接着写。
    Dismissed,
    /// 按了取消：关掉，写了的不留，焦点交回看板。
    Discarded,
    /// 宿主办成了：关掉，焦点同样交回看板；按的是「保存并开始」时 `start` 是要接着开始的任务。`failed` 是存好了、
    /// 后面跟的请求没办成的原因。
    Saved { start: Option<String>, failed: Option<String> },
}

impl EventEmitter<DialogEvent> for TodoDialog {}

impl TodoDialog {
    /// 新建（`editing` 为空）或编辑任务、定时的对话框，焦点给标题；新建时目录先填 `dir`（当前 workspace 的）。
    /// `workspaces` 是各工作区的目录和名字，`shell_path` 是本机终端里 shell 报告的 PATH。
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
        let creating = spec.is_none();
        let new_todo = creating && !schedule;
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
            area.set_placeholder(
                rust_i18n::t!("kanban.dialog.prompt_placeholder", key = crate::keybinds::secondary_text("enter"))
                    .into_owned()
                    .into(),
                cx,
            );
            area.set_text(prompt_text, cx);
            area
        });
        let when = match &editing {
            Some(Editing::Schedule(schedule)) => Some(schedule.when.clone()),
            _ => None,
        };
        let (clock, minutes, cron) = ("09:00".to_owned(), "60".to_owned(), "0 9 * * 1-5".to_owned());
        let (kind, at_text, clock_text, minutes_text, cron_text, days) = match when {
            Some(When::Once { at }) => (Some(Kind::Once), full_time(at), clock, minutes, cron, 0),
            Some(When::Daily { hour, minute, days }) => {
                (Some(Kind::Daily), String::new(), format!("{hour:02}:{minute:02}"), minutes, cron, days)
            }
            Some(When::Every { minutes }) => (Some(Kind::Every), String::new(), clock, minutes.to_string(), cron, 0),
            Some(When::Cron { expr }) => (Some(Kind::Cron), String::new(), clock, minutes, expr, 0),
            // 读不懂的规则照原样留着。
            Some(When::Unknown) => (None, String::new(), clock, minutes, cron, 0),
            None => (Some(Kind::Daily), String::new(), clock, minutes, cron, 0),
        };
        let at_text = if at_text.is_empty() { next_hour() } else { at_text };
        let at = field(at_text, "kanban.dialog.once", "kanban.dialog.once_placeholder", cx);
        let clock = field(clock_text, "kanban.dialog.daily", "kanban.dialog.daily_placeholder", cx);
        let minutes = field(minutes_text, "kanban.dialog.every", "kanban.dialog.every_placeholder", cx);
        let cron = field(cron_text, "kanban.dialog.cron", "kanban.dialog.cron_placeholder", cx);
        let limit_text =
            spec.as_ref().and_then(|spec| spec.time_limit).map(|limit| limit.to_string()).unwrap_or_default();
        let custom_limit = limit_preset(&limit_text, false).is_none();
        let time_limit = field(limit_text, "kanban.dialog.time_limit", "kanban.dialog.time_limit_placeholder", cx);
        let (queued, after) = match &editing {
            Some(Editing::Todo(todo)) => (todo.queued, todo.after.clone()),
            _ => (false, None),
        };
        let input = cx.new(|cx| InputState::new(String::new(), cx));
        // 输入框里回车、提示词里 ⌘回车都按底栏的主按钮办，Esc 关掉对话框。
        let mut events = Vec::new();
        for input in [&title, &command, &at, &clock, &minutes, &cron, &time_limit] {
            events.push(Self::subscribe_input(input, window, cx));
        }
        events.push(cx.subscribe_in(&prompt, window, |this, _, event: &TextareaEvent, _, cx| match event {
            TextareaEvent::Submit => this.save(this.start, cx),
            TextareaEvent::Changed => this.clear_error(cx),
        }));
        window.focus(&title.focus_handle(cx), cx);
        let spec = spec.unwrap_or_default();
        let mut dialog = Self {
            schedule,
            editing,
            templates: Vec::new(),
            template: None,
            templates_dir: runode_paths::Dirs::from_env().todo_templates_dir(),
            input_events: None,
            input,
            title,
            prompt,
            agent,
            command,
            workspaces,
            shell_path,
            installed: None,
            loading: HashSet::new(),
            fields_scroll: ScrollHandle::new(),
            failed: HashMap::new(),
            dir,
            branch: None,
            worktree,
            skills: spec.skills,
            subagent: spec.subagent,
            attachments: spec.attachments,
            found: Found::default(),
            auto: spec.auto,
            auto_done: spec.auto_done,
            time_limit,
            custom_limit,
            start: false,
            queued,
            after,
            locked,
            kind,
            at,
            clock,
            minutes,
            cron,
            days,
            error: None,
            focus_wrong: None,
            saving: false,
            focus: cx.focus_handle(),
            menu: None,
            menu_for: None,
            combo_bottoms: Rc::default(),
            _events: events,
        };
        if new_todo {
            dialog.check_repo(cx);
        }
        if creating {
            dialog.load_templates(cx);
        }
        dialog.find_skills(cx);
        dialog.find_branch(cx);
        dialog.find_installed(new_todo, cx);
        if let Some(pick) = &dialog.agent {
            dialog.fetch_models(pick.agent, cx);
        }
        dialog
    }

    /// 在后台按 PATH 找各 agent 的程序，记进 `installed`。`new_todo` 时挑着的那家没装、也没挑模型和强度，
    /// 而别家有装着的，换成菜单里排第一的那家。
    fn find_installed(&mut self, new_todo: bool, cx: &mut Context<Self>) {
        let path = self.shell_path.clone();
        let job = cx.background_executor().spawn(async move {
            let path = crate::process_env::user_path(path);
            let programs: Vec<String> = LAUNCHERS.iter().map(|launcher| launcher.program().to_owned()).collect();
            let found = runode_terminal::shells::find_commands(path.as_deref(), &programs);
            found.iter().map(Option::is_some).collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let installed = job.await;
            this.update(cx, |dialog, cx| {
                dialog.installed = Some(installed);
                let untouched = dialog.agent.as_ref().filter(|pick| pick.model.is_none() && pick.effort.is_none());
                if new_todo
                    && let Some(current) = untouched.map(|pick| dialog.family_of(pick.agent))
                    && !dialog.family_installed(current)
                    && let Some(other) = (0..LAUNCHERS.len())
                        .find(|agent| runode_agent::models::listed(*agent) && dialog.family_installed(*agent))
                {
                    dialog.agent = Some(Pick { agent: other, model: None, effort: None });
                    dialog.fetch_models(other, cx);
                }
                dialog.refresh_menu(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 第 `agent` 个 agent 那一家在菜单里列的那个（`runode_agent::models::listed`）。
    fn family_of(&self, agent: usize) -> usize {
        (0..=agent).rev().find(|index| runode_agent::models::listed(*index)).unwrap_or(agent)
    }

    /// 菜单里列的第 `listed` 家有哪个版本装着；还没找好时当作装着。
    fn family_installed(&self, listed: usize) -> bool {
        let Some(installed) = &self.installed else { return true };
        std::iter::successors(Some(listed), |agent| runode_agent::models::same_kind(*agent))
            .any(|agent| installed.get(agent).copied().unwrap_or(false))
    }

    /// 输入框里回车按底栏的主按钮办，Esc 关掉对话框，改了字就收起上次的报错。
    fn subscribe_input(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::Next => this.save(this.start, cx),
            InputEvent::Dismiss => cx.emit(DialogEvent::Dismissed),
            InputEvent::Changed(_) => this.clear_error(cx),
            InputEvent::Previous => {}
        })
    }

    /// 动手改了：上次保存时的报错已经不一定对，收起来。
    fn clear_error(&mut self, cx: &mut Context<Self>) {
        if self.error.take().is_some() {
            cx.notify();
        }
    }

    /// 在后台读模板（`templates::load`），读好了列出来。
    fn load_templates(&mut self, cx: &mut Context<Self>) {
        let dir = self.templates_dir.clone();
        let load = cx.background_executor().spawn(async move { templates::load(dir.as_deref()) });
        cx.spawn(async move |this, cx| {
            let templates = load.await;
            this.update(cx, |dialog, cx| {
                dialog.templates = templates;
                dialog.refresh_menu(cx);
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
    fn pick_template(&mut self, action: &PickTodoTemplate, window: &mut Window, cx: &mut Context<Self>) {
        let index = action.index;
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

    /// 打开模板目录，没有就先建：它在 home 下的点目录里，访达和资源管理器默认都看不到。
    fn open_templates(&mut self, _: &OpenTodoTemplates, _: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = &self.templates_dir else { return };
        if let Err(err) = std::fs::create_dir_all(dir) {
            tracing::warn!("cannot create {}: {err}", dir.display());
        }
        cx.open_with_system(dir);
    }

    /// 「模板」菜单：空白和各个模板，挑着的打勾，最后是打开模板目录；还没有模板时只有最后一项，没有家目录时
    /// 连它也没有。
    fn template_items(&self) -> Vec<Option<MenuItem>> {
        let mut items: Vec<Option<MenuItem>> = Vec::new();
        if !self.templates.is_empty() {
            let blank = rust_i18n::t!("kanban.dialog.template_blank").into_owned();
            let named = self.templates.iter().enumerate().map(|(index, template)| (Some(index), template.name.clone()));
            items.extend(std::iter::once((None, blank)).chain(named).map(|(index, label)| {
                let action = Box::new(PickTodoTemplate { index });
                Some(MenuItem::checkbox(label, None, self.template == index, action))
            }));
        }
        if let Some(dir) = &self.templates_dir {
            if !items.is_empty() {
                items.push(None);
            }
            let open = rust_i18n::t!("kanban.dialog.templates_open").into_owned();
            let item = MenuItem::label(open, None, Some(Box::new(OpenTodoTemplates))).with_tooltip(display_dir(dir));
            items.push(Some(item));
        }
        items
    }

    /// 系统选择框的结果：选好的路径；取消了为空，打不开时还在对话框里写出原因。
    fn picked(
        &mut self,
        result: Result<anyhow::Result<Option<Vec<PathBuf>>>, futures::channel::oneshot::Canceled>,
        cx: &mut Context<Self>,
    ) -> Option<Vec<PathBuf>> {
        match result {
            Ok(Ok(paths)) => paths,
            Ok(Err(err)) => {
                tracing::warn!("could not open the system file picker: {err:#}");
                let key = if cfg!(target_os = "linux") {
                    "kanban.dialog.picker_failed_linux"
                } else {
                    "kanban.dialog.picker_failed"
                };
                self.error = Some(rust_i18n::t!(key).into_owned());
                cx.notify();
                None
            }
            Err(_) => None,
        }
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
            let picked = paths.await;
            let Some(paths) = this.update(cx, |dialog, cx| dialog.picked(picked, cx)).ok().flatten() else {
                return;
            };
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

    fn pick_queue(&mut self, action: &PickTodoQueue, _: &mut Window, cx: &mut Context<Self>) {
        self.queued = action.queued;
        self.after = action.after.clone().filter(|_| action.queued);
        cx.notify();
    }

    /// 现在选着的什么时候做。
    fn start_mode(&self) -> StartMode {
        start_mode_of(self.schedule, self.start, self.queued)
    }

    /// 换什么时候做：换到定时时存成定时、不开始也不排队；离开排队时不再等谁。
    fn pick_start(&mut self, mode: StartMode, cx: &mut Context<Self>) {
        self.schedule = mode == StartMode::Schedule;
        self.start = mode == StartMode::Now;
        self.queued = mode == StartMode::Queue;
        if mode != StartMode::Queue {
            self.after = None;
        }
        cx.notify();
    }

    /// 在编辑的任务；新建、编辑定时时为空。
    fn editing_todo(&self) -> Option<&Todo> {
        match &self.editing {
            Some(Editing::Todo(todo)) => Some(todo),
            _ => None,
        }
    }

    /// 能排在后面等的任务的 id 和写法（`after_candidates`），等着的那个不在里面时也列上。
    fn after_choices(&self, cx: &App) -> Vec<(String, String)> {
        let todos = &store::data(cx).todos;
        let mut afters: Vec<(String, String)> =
            after_candidates(todos, self.editing_todo().map(|todo| todo.id.as_str()))
                .into_iter()
                .map(|todo| (todo.id.clone(), after_label(todo)))
                .collect();
        if let Some(after) = &self.after
            && !afters.iter().any(|(id, _)| id == after)
        {
            afters.push((after.clone(), self.after_text(after, cx)));
        }
        afters
    }

    /// 「排在别的后面」的菜单：先是不等哪个，再是能等的各个任务，挑着的打勾。
    fn after_items(&self, cx: &App) -> Vec<Option<MenuItem>> {
        let any = rust_i18n::t!("kanban.dialog.queue_any").into_owned();
        let any =
            MenuItem::checkbox(any, None, self.after.is_none(), Box::new(PickTodoQueue { queued: true, after: None }));
        let choices = self.after_choices(cx);
        let mut items = vec![Some(any)];
        if !choices.is_empty() {
            items.push(None);
        }
        items.extend(choices.into_iter().map(|(id, label)| {
            let checked = self.after.as_ref() == Some(&id);
            let action = Box::new(PickTodoQueue { queued: true, after: Some(id) });
            Some(MenuItem::checkbox(label, None, checked, action))
        }));
        items
    }

    /// 任务 `after` 的写法；它已经不在看板上时不写是哪个。
    fn after_text(&self, after: &str, cx: &App) -> String {
        store::data(cx)
            .todos
            .iter()
            .find(|todo| todo.id == after)
            .map_or_else(|| rust_i18n::t!("kanban.dialog.after_gone").into_owned(), after_label)
    }

    /// 存好以后要发的 `ClientMsg::QueueTodo` 的 `(queued, after)`；不是还能开始的任务、或者不用发时为空。
    fn queue_to_send(&self) -> Option<(bool, Option<String>)> {
        if !self.startable() {
            return None;
        }
        let original = self.editing_todo().map(|todo| (todo.queued, todo.after.as_deref()));
        queue_change(original, self.queued, self.after.as_deref())
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

    /// 在后台读 `dir` 所在仓库当前的分支（`runode_git::current_branch`），读完了还是这个目录时换上。
    fn find_branch(&mut self, cx: &mut Context<Self>) {
        let dir = self.dir.clone();
        let branch = cx.background_executor().spawn({
            let dir = dir.clone();
            async move { runode_git::current_branch(&dir) }
        });
        cx.spawn(async move |this, cx| {
            let branch = branch.await;
            this.update(cx, |dialog, cx| {
                if dialog.dir == dir {
                    dialog.branch = branch;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 换了目录：重新看要不要工作树、在哪个分支上，再问一遍挑着的 agent 的模型清单（有的 agent 读项目里的配置）。
    fn dir_changed(&mut self, cx: &mut Context<Self>) {
        self.check_repo(cx);
        self.branch = None;
        self.find_branch(cx);
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
            let picked = paths.await;
            let Some(paths) = this.update(cx, |dialog, cx| dialog.picked(picked, cx)).ok().flatten() else {
                return;
            };
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
            let catalog = runode_agent::models::fetch(launcher, &dir, crate::process_env::user_path(path))?;
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
        let items = self.menu_items(combo, cx);
        self.menu_for = Some(combo);
        let target = self.focus.clone();
        self.open_dropdown(items, target, None, cx);
        // 对话框能收起来留很久，其间装上、卸掉的 agent 每次打开菜单时重新找，找好了换上。
        if combo == Combo::Agent {
            self.find_installed(false, cx);
        }
    }

    /// 开着的菜单照现在的清单重建：问到了模型清单时用。
    fn refresh_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(combo) = self.menu_for.filter(|_| self.dropdown_open()) {
            let items = self.menu_items(combo, cx);
            self.replace_menu_items(items, cx);
        }
    }

    /// 框 `combo` 的菜单项。
    fn menu_items(&self, combo: Combo, cx: &App) -> Vec<Option<MenuItem>> {
        match combo {
            Combo::Workspace => self.workspace_items(),
            Combo::Agent => self.agent_items(),
            Combo::Model => self.model_items(),
            Combo::Effort => self.effort_items(),
            Combo::Skills => self.skill_items(),
            Combo::Subagent => self.subagent_items(),
            Combo::After => self.after_items(cx),
            Combo::Template => self.template_items(),
        }
    }

    /// agent 菜单：`LAUNCHERS` 里装着的每家一项（`listed`），最后是自己写的命令。
    fn agent_items(&self) -> Vec<Option<MenuItem>> {
        let current = self.agent.as_ref().map(|pick| pick.launcher().label);
        // 没装的不列，挑着的那家照样列，免得存着的任务打开后菜单里没有它。
        let mut items: Vec<_> = LAUNCHERS
            .iter()
            .enumerate()
            .filter(|(index, spec)| {
                runode_agent::models::listed(*index) && (self.family_installed(*index) || current == Some(spec.label))
            })
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
    fn opens_menu<E: StatefulInteractiveElement>(combo: Combo, element: E, cx: &mut Context<Self>) -> E {
        element
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                if this.menu_for == Some(combo) && this.dropdown_open() {
                    cx.stop_propagation();
                    this.close_menu(window, cx);
                }
            }))
            .on_press_down(cx, move |this, window, cx| this.toggle_menu(combo, window, cx))
    }

    /// 挂在框 `combo` 下面的菜单，开着的不是它的菜单时为空。菜单比框宽：靠右一栏的框右边对齐着往左展开，
    /// 别的左边对齐着往右展开，都不越出对话框。
    fn combo_menu(&self, combo: Combo, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let bottom = self.combo_bottoms[combo as usize].get();
        self.menu_for.filter(|open| *open == combo).and_then(|_| match combo {
            Combo::Effort | Combo::Subagent | Combo::Template => self.render_dropdown_menu(bottom, window, cx),
            _ => self.render_dropdown_menu_rightward(bottom, window, cx),
        })
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

    /// 挑着的 agent 问过了、找不到它的命令。
    fn agent_missing(&self) -> bool {
        self.agent.as_ref().is_some_and(|pick| {
            !self.loading.contains(&pick.agent) && self.failed.get(&pick.agent) == Some(&Unanswered::Missing)
        })
    }

    /// 能「保存并开始」：是任务，还没开始过也没做完。
    fn startable(&self) -> bool {
        match &self.editing {
            None => !self.schedule,
            Some(Editing::Todo(todo)) => todo.started.is_none() && todo.done_at.is_none(),
            Some(Editing::Schedule(_)) => false,
        }
    }

    /// 是新建的对话框，收起时能留着下次接着写；正在存的不算。
    pub(crate) fn draft(&self) -> bool {
        self.editing.is_none() && !self.saving
    }

    /// 按 Esc、× 或点外面收起时值得留着：是 `draft`，标题或提示词写了字。
    pub(crate) fn worth_keeping(&self, cx: &App) -> bool {
        let blank = self.title.read(cx).query().trim().is_empty() && self.prompt.read(cx).text().trim().is_empty();
        self.draft() && !blank
    }

    /// 标题输入框的焦点，收起来的对话框重新打开时交给它。
    pub(crate) fn title_focus(&self, cx: &App) -> FocusHandle {
        self.title.focus_handle(cx)
    }

    /// 按对话框里填的发请求；填得不对时在对话框里说。办成了再发要改的开始方式（`queue_to_send`）。都办成了就关掉
    /// 对话框，`start` 时交给看板接着开始（排队的不开始）；没办成时说原因。新建的任务已经建好、只是开始方式没改成时
    /// 照样关掉，原因交给看板说，免得再存一遍建出第二个。
    fn save(&mut self, start: bool, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let message = match self.request(cx) {
            Ok(message) => message,
            Err((error, field)) => {
                self.error = Some(error);
                self.focus_wrong = Some(field);
                cx.notify();
                return;
            }
        };
        let queue = self.queue_to_send();
        let start = start && self.startable() && !self.queued;
        let edited = self.editing_todo().map(|todo| todo.id.clone());
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
            let answer = match answer {
                Ok(answer) => {
                    let id = match answer {
                        HostMsg::TodoAdded { id, .. } => Some(id),
                        _ => edited.clone(),
                    };
                    let queue_failed = match (queue, id.clone()) {
                        (Some((queued, after)), Some(id)) => {
                            let step: store::Step =
                                Box::new(move |req| ClientMsg::QueueTodo { req, id, queued, after });
                            let Ok(sent) = this.update(cx, |_, cx| store::ask(cx, step)) else {
                                return;
                            };
                            sent.await.err().map(|err| format!("{err:#}"))
                        }
                        _ => None,
                    };
                    Ok((id, queue_failed))
                }
                Err(err) => Err(err),
            };
            this.update(cx, |dialog, cx| {
                match answer {
                    Ok((id, None)) => cx.emit(DialogEvent::Saved { start: id.filter(|_| start), failed: None }),
                    Ok((_, Some(reason))) if edited.is_none() => {
                        cx.emit(DialogEvent::Saved { start: None, failed: Some(reason) });
                    }
                    Ok((_, Some(reason))) => {
                        dialog.saving = false;
                        dialog.error = Some(reason);
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

/// 输入框随内容长高，至少 28 点。
fn grow_input(state: &Entity<InputState>) -> Input {
    Input::new(state).h_auto().min_h(scaled(28.)).py(px(4.))
}

/// 表单的一行：左边一栏定宽的标签，和右边 `child` 的第一行对齐。
fn row(label: String, child: impl IntoElement, cx: &App) -> Div {
    let subtle = cx.theme().subtle_foreground;
    div()
        .flex()
        .items_start()
        .gap(px(12.))
        .child(
            div()
                .flex_none()
                .w(scaled(LABEL_WIDTH))
                .min_h(scaled(CONTROL_HEIGHT))
                .flex()
                .items_center()
                .text_size(FontSize::Body)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(subtle)
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(child))
}

/// 控件下面或旁边的一句说明，报给辅助工具。
fn hint(id: impl Into<gpui::ElementId>, text: String, cx: &App) -> gpui::Stateful<Div> {
    let muted = cx.theme().muted_foreground;
    div().id(id).aria_static_text(text.clone()).text_size(FontSize::Small).text_color(muted).child(text)
}

/// 定时、开始方式下面的小卡：底色比对话框深一档，和外框分开。
fn card(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(14.))
        .py(px(12.))
        .rounded(px(10.))
        .bg(theme.field)
        .border_1()
        .border_color(theme.border)
}

/// 一排里的小签：胶囊形，和下拉框一样高。
fn pill(chip: Chip) -> Chip {
    chip.h(scaled(28.)).px(px(11.)).rounded_full()
}

/// 复选框：一个小方块打勾，后面一行字，点字也算点方块。不可用时灰掉、报给辅助工具，调用方别挂点击。
fn checkbox(id: &'static str, label: String, checked: bool, enabled: bool, cx: &App) -> gpui::Stateful<Div> {
    let theme = cx.theme();
    let (primary, on_primary, border) = (theme.primary, theme.primary_foreground, theme.control_border);
    let mark = div().flex_none().size(px(16.)).rounded(px(5.)).flex().items_center().justify_center().map(|mark| {
        if checked {
            mark.bg(primary).child(svg().path(CHECK_ICON).size(px(11.)).text_color(on_primary))
        } else {
            mark.border_1().border_color(border)
        }
    });
    div()
        .id(id)
        .role(Role::CheckBox)
        .aria_label(label.clone())
        .aria_toggled(checked.into())
        .min_h(scaled(CONTROL_HEIGHT))
        .flex()
        .items_center()
        .gap(px(8.))
        .when(enabled, |checkbox| checkbox.cursor_pointer())
        .when(!enabled, |checkbox| checkbox.opacity(0.6))
        .aria_disabled(!enabled)
        .child(mark)
        .child(label)
}

impl TodoDialog {
    /// 点了弹出菜单的框：左边是挑着的值，右边一个下拉箭头，菜单（`menu`）挂在它下面。
    #[allow(clippy::too_many_arguments)]
    fn combo(
        &self,
        id: &'static str,
        kind: Combo,
        label: String,
        value: String,
        enabled: bool,
        menu: Option<Div>,
        cx: &App,
    ) -> SelectTrigger {
        let theme = cx.theme();
        let hover_bg = theme.mix(0.14);
        let (bg, border) = (theme.secondary, theme.control_border);
        let open = self.menu_for == Some(kind) && self.dropdown_open();
        let bottoms = self.combo_bottoms.clone();
        SelectTrigger::new(id, label, value)
            .aria_expanded(open)
            .relative()
            .flex_none()
            .max_w(scaled(300.))
            .h(scaled(CONTROL_HEIGHT))
            .pl(px(10.))
            .pr(px(8.))
            .gap(px(7.))
            .rounded(px(8.))
            .bg(bg)
            .border_1()
            .border_color(border)
            .when(enabled, |combo| combo.hover(move |combo| combo.bg(hover_bg)))
            .when(!enabled, |combo| combo.cursor_default().opacity(0.6))
            .aria_disabled(!enabled)
            // 记下下沿，菜单的最大高度按它算。
            .child(
                canvas(move |bounds, _, _| bottoms[kind as usize].set(f32::from(bounds.bottom())), |_, (), _, _| {})
                    .absolute()
                    .size_full(),
            )
            .children(menu)
    }

    /// 对话框里能打字的框，按显示的先后：模板要填的那一项、标题、提示词、自定义 agent 的命令、定时的时间、
    /// 自己定的时限。
    fn text_fields(&self, cx: &App) -> Vec<FocusHandle> {
        let template_input = self.template.and_then(|index| self.templates[index].input.as_ref()).map(|_| &self.input);
        let when = match self.kind {
            Some(Kind::Once) if self.schedule => Some(&self.at),
            Some(Kind::Daily) if self.schedule => Some(&self.clock),
            Some(Kind::Every) if self.schedule => Some(&self.minutes),
            Some(Kind::Cron) if self.schedule => Some(&self.cron),
            _ => None,
        };
        let command = self.agent.is_none().then_some(&self.command);
        let inputs = template_input.into_iter().chain([&self.title]);
        let mut fields: Vec<FocusHandle> = inputs.map(|input| input.focus_handle(cx)).collect();
        fields.push(self.prompt.focus_handle(cx));
        let limit = self.limit_choice(cx).is_none().then_some(&self.time_limit);
        fields.extend(command.into_iter().chain(when).chain(limit).map(|input| input.focus_handle(cx)));
        fields
    }

    /// Tab 和 Shift+Tab 在能打字的框之间往后、往前挪，到头了绕回另一头；焦点不在这些框里时到第一个。
    fn tab_between_fields(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        if event.keystroke.key != "tab" || modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }
        let fields = self.text_fields(cx);
        let at = fields.iter().position(|field| field.is_focused(window));
        let next = match at {
            Some(at) if modifiers.shift => (at + fields.len() - 1) % fields.len(),
            Some(at) => (at + 1) % fields.len(),
            None => 0,
        };
        cx.stop_propagation();
        window.focus(&fields[next], cx);
    }

    /// 对话框的标题：新建的一律是新建任务（定时也在里面挑），编辑的分任务和定时。
    fn heading(&self) -> String {
        match (&self.editing, self.schedule) {
            (Some(_), false) => rust_i18n::t!("kanban.edit"),
            (Some(_), true) => rust_i18n::t!("kanban.edit_schedule"),
            (None, _) => rust_i18n::t!("kanban.new_todo"),
        }
        .into_owned()
    }

    /// 标题栏的「模板」框：框里是挑着的模板名，没挑时写「模板」；只有新建时有，菜单一项都没有时也不画。
    fn render_template_picker(&self, window: &Window, cx: &mut Context<Self>) -> Option<SelectTrigger> {
        if self.editing.is_some() || (self.templates.is_empty() && self.templates_dir.is_none()) {
            return None;
        }
        let subtle = cx.theme().subtle_foreground;
        let name = rust_i18n::t!("kanban.dialog.templates").into_owned();
        let picked = self.template.map(|index| self.templates[index].name.clone());
        let value = picked.clone().unwrap_or_else(|| name.clone());
        // 没挑时框里写的是框的名字，报给辅助工具的值写空白。
        let state = picked.unwrap_or_else(|| rust_i18n::t!("kanban.dialog.template_blank").into_owned());
        let menu = self.combo_menu(Combo::Template, window, cx);
        let picker = self
            .combo("todo-dialog-template", Combo::Template, name, value, true, menu, cx)
            .aria_value(state)
            .max_w(scaled(200.))
            .h(scaled(28.))
            .bg(gpui::transparent_black())
            .border_color(gpui::transparent_black())
            .text_color(subtle);
        Some(Self::opens_menu(Combo::Template, picker, cx))
    }

    /// 顶上一条：标题，右边是「模板」框（`render_template_picker`）、关掉的按钮和它的键。
    fn render_header(&self, title: String, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let templates = self.render_template_picker(window, cx);
        let subtle = cx.theme().subtle_foreground;
        let close = Button::new("todo-dialog-close", rust_i18n::t!("kanban.dialog.close").into_owned())
            .variant(ButtonVariant::Ghost)
            .icon(CLOSE_ICON)
            .icon_size(13.)
            .h(scaled(28.))
            .pl(px(9.))
            .pr(px(6.))
            .gap(px(7.))
            .rounded(px(7.))
            .text_color(subtle)
            .cursor_pointer()
            .child(Kbd::new("esc"))
            .on_press(cx, |_, _, cx| cx.emit(DialogEvent::Dismissed));
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .pl(px(20.))
            .pr(px(14.))
            .pt(px(14.))
            .pb(px(12.))
            .child(
                div()
                    .id("todo-dialog-title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(title.clone())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(FontSize::Title)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .children(templates)
            .child(close)
    }

    /// 标题和按钮之间的各栏；窗口矮时只有这些滚动，标题和保存按钮总在。默认大小的窗口里附件和下面的开关就在
    /// 底下藏着，挂上滚动条，鼠标进来时看得出还能往下滚。滚动条画在面板右边的内边距里，不压住各栏的边框。
    fn render_fields(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fields = div()
            .id("todo-dialog-fields")
            .flex_1()
            .min_h_0()
            .pr(px(SCROLLBAR_GUTTER))
            .track_scroll(&self.fields_scroll)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(self.render_composer(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .children(self.render_template_input(cx))
                    .child(self.render_workspace(window, cx))
                    .child(self.render_agent_row(window, cx))
                    .child(self.render_skills_row(window, cx))
                    .children(self.render_start(window, cx))
                    .children(self.render_when(cx))
                    .child(self.render_time_limit(cx))
                    .child(self.render_attachments(cx))
                    .child(self.render_switches(cx)),
            );
        // 面板的高度跟着内容，压得住各栏的只有伸缩，百分比的高度压不住。
        div().flex_1().min_h_0().mr(px(-SCROLLBAR_GUTTER)).relative().flex().flex_col().child(fields).child(scrollbar(
            "todo-dialog-scrollbar",
            self.fields_scroll.clone(),
            Axis::Vertical,
        ))
    }

    /// 标题和提示词：标题一行，下面是提示词的框，框底下一行说明。
    fn render_composer(&self, cx: &App) -> impl IntoElement {
        let prompt_hint = rust_i18n::t!("kanban.dialog.prompt_hint").into_owned();
        let title = Input::new(&self.title).h(scaled(34.)).px(px(14.)).rounded(px(10.)).text_size(FontSize::Large);
        let prompt = Textarea::new(&self.prompt)
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(14.))
            .pt(px(12.))
            .pb(px(8.))
            .rounded(px(10.))
            .text_size(FontSize::Large)
            .child(hint("todo-dialog-prompt-hint", prompt_hint, cx));
        div().flex().flex_col().gap(px(8.)).child(title).child(prompt)
    }

    /// 挑着的模板要另外填的那一项，放在各行的最上面。
    fn render_template_input(&self, cx: &App) -> Option<Div> {
        self.template
            .and_then(|index| self.templates[index].input.clone())
            .map(|label| row(label, grow_input(&self.input), cx))
    }

    /// 什么时候做：分段挑马上开始、放进待办、排在别的后面，新建时还能挑定时，下面一张小卡说明会怎样。排在别的
    /// 后面时在卡里挑先等哪个（也可以不等哪个），定时时卡里填什么时候跑（`when_card`）。只有新建的和还能开始的
    /// 任务有。
    fn render_start(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let creating = self.editing.is_none();
        if !creating && !self.startable() {
            return None;
        }
        let mode = self.start_mode();
        let label = rust_i18n::t!("kanban.dialog.when").into_owned();
        let option = |group: Segmented, id: &'static str, choice: StartMode, key: &str, cx: &mut Context<Self>| {
            group.option(id, rust_i18n::t!(key).into_owned(), mode == choice, cx, move |dialog, _, cx| {
                dialog.pick_start(choice, cx);
            })
        };
        let group = Segmented::new("todo-dialog-start", label.clone()).fill().h(scaled(CONTROL_HEIGHT)).w_full();
        let group = option(group, "todo-dialog-start-now", StartMode::Now, "kanban.dialog.start_now", cx);
        let group = option(group, "todo-dialog-start-manual", StartMode::Manual, "kanban.dialog.start_manual", cx);
        let group = option(group, "todo-dialog-start-queue", StartMode::Queue, "kanban.dialog.start_after", cx);
        let group = if creating {
            option(group, "todo-dialog-start-schedule", StartMode::Schedule, "kanban.dialog.start_schedule", cx)
        } else {
            group
        };
        let note = |key: &str, cx: &App| hint("todo-dialog-start-hint", rust_i18n::t!(key).into_owned(), cx);
        let card = match mode {
            StartMode::Now => card(cx).child(note("kanban.dialog.now_hint", cx)),
            StartMode::Manual => card(cx).child(note("kanban.dialog.manual_hint", cx)),
            StartMode::Queue => {
                let subtle = cx.theme().subtle_foreground;
                let value = match self.after.as_deref() {
                    Some(after) => self.after_text(after, cx),
                    None => rust_i18n::t!("kanban.dialog.queue_any").into_owned(),
                };
                let name = rust_i18n::t!("kanban.dialog.start_after").into_owned();
                let menu = self.combo_menu(Combo::After, window, cx);
                let after = self.combo("todo-dialog-after", Combo::After, name, value, true, menu, cx);
                let after = Self::opens_menu(Combo::After, after, cx);
                let lead = rust_i18n::t!("kanban.dialog.queue_lead").into_owned();
                let tail = rust_i18n::t!("kanban.dialog.queue_tail").into_owned();
                let pick = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(div().text_color(subtle).child(lead))
                    .child(after)
                    .child(div().text_color(subtle).child(tail));
                let hint = if self.after.is_some() { "kanban.dialog.after_hint" } else { "kanban.dialog.queued_hint" };
                card(cx).child(pick).child(note(hint, cx))
            }
            StartMode::Schedule => self.when_card(cx),
        };
        Some(row(label, div().flex().flex_col().gap(px(8.)).child(group).child(card), cx))
    }

    /// 编辑定时时什么时候跑那一行（`when_card`），没有分段；新建的不画。
    fn render_when(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.schedule || self.editing.is_none() {
            return None;
        }
        let label = rust_i18n::t!("kanban.dialog.when").into_owned();
        Some(row(label, self.when_card(cx), cx))
    }

    /// 定时什么时候跑的小卡：一排小签挑一次、每天（可以只挑几天）、每隔、cron，后面接着填时间，下面列出接下来
    /// 几次到点的时刻。
    fn when_card(&self, cx: &mut Context<Self>) -> Div {
        let subtle = cx.theme().subtle_foreground;
        let kinds = [
            ("todo-dialog-kind-once", Kind::Once, "kanban.dialog.kind_once"),
            ("todo-dialog-kind-daily", Kind::Daily, "kanban.dialog.kind_daily"),
            ("todo-dialog-kind-every", Kind::Every, "kanban.dialog.kind_every"),
            ("todo-dialog-kind-cron", Kind::Cron, "kanban.dialog.kind_cron"),
        ]
        .map(|(id, kind, key)| {
            pill(Chip::new(id, rust_i18n::t!(key).into_owned(), self.kind == Some(kind))).on_press(
                cx,
                move |dialog, window, cx| {
                    dialog.kind = Some(kind);
                    let input = match kind {
                        Kind::Once => dialog.at.clone(),
                        Kind::Daily => dialog.clock.clone(),
                        Kind::Every => dialog.minutes.clone(),
                        Kind::Cron => dialog.cron.clone(),
                    };
                    window.focus(&input.focus_handle(cx), cx);
                    cx.notify();
                },
            )
        });
        let at_word =
            || div().ml(px(6.)).text_color(subtle).child(rust_i18n::t!("kanban.dialog.clock_at").into_owned());
        let field = |input: &Entity<InputState>, width: f32| Input::new(input).w(scaled(width)).h(scaled(28.));
        // 框后面跟的：一次、每天前面写「在」，每隔的框里只有数字、单位写在后面，cron 用等宽字。
        let input: Vec<gpui::AnyElement> = match self.kind {
            Some(Kind::Once) => vec![at_word().into_any_element(), field(&self.at, 150.).into_any_element()],
            Some(Kind::Daily) => vec![at_word().into_any_element(), field(&self.clock, 72.).into_any_element()],
            Some(Kind::Every) => vec![
                field(&self.minutes, 64.).ml(px(6.)).into_any_element(),
                div().text_color(subtle).child(rust_i18n::t!("kanban.dialog.every").into_owned()).into_any_element(),
            ],
            Some(Kind::Cron) => {
                let mono = crate::ui::font::mono(cx);
                let cron = Input::new(&self.cron)
                    .flex_1()
                    .min_w(scaled(140.))
                    .h(scaled(28.))
                    .ml(px(6.))
                    .when_some(mono, |input, family| input.font_family(family));
                vec![cron.into_any_element()]
            }
            None => Vec::new(),
        };
        let first = div().flex().flex_wrap().items_center().gap(px(6.)).children(kinds).children(input);
        let days = (self.kind == Some(Kind::Daily)).then(|| {
            let pills: Vec<_> = week_order()
                .map(|day| {
                    let on = self.days & (1 << day) != 0;
                    let label = weekday(day);
                    Chip::new(("todo-weekday", usize::from(day)), label, on).on_press(cx, move |dialog, _, cx| {
                        dialog.days ^= 1 << day;
                        cx.notify();
                    })
                })
                .collect();
            div().flex().flex_wrap().items_center().gap(px(4.)).children(pills).child(
                hint("todo-dialog-days-hint", rust_i18n::t!("kanban.dialog.days_hint").into_owned(), cx).ml(px(4.)),
            )
        });
        let cron_hint = (self.kind == Some(Kind::Cron))
            .then(|| hint("todo-dialog-cron-hint", rust_i18n::t!("kanban.dialog.cron_hint").into_owned(), cx));
        let fires = match self.kind {
            None => Some(hint("todo-dialog-unknown", rust_i18n::t!("kanban.dialog.unknown_hint").into_owned(), cx)),
            // 填得不对时不列；为什么不对等保存时在底栏说。
            Some(_) => self.typed_when(cx).ok().and_then(|when| {
                let now = when::unix_now();
                let times: Vec<String> =
                    next_fires(&when, now).into_iter().map(|at| when::fire_text(at, now)).collect();
                let text = match times.as_slice() {
                    [] => return None,
                    [first, second, third] => {
                        rust_i18n::t!("kanban.dialog.next_fires", first = first, second = second, third = third)
                    }
                    [first, ..] => rust_i18n::t!("kanban.dialog.next_fire", first = first),
                };
                Some(hint("todo-dialog-next-fires", text.into_owned(), cx).text_color(subtle))
            }),
        };
        card(cx).child(first).children(days).children(cron_hint).children(fires)
    }

    /// 项目那一行：在哪个工作区或目录里做（框里带着当前的分支），要不要独立的工作树；任务开始过了就都不能再改。
    fn render_workspace(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let workspace_label = rust_i18n::t!("kanban.dialog.workspace").into_owned();
        let workspace_text = self
            .workspaces
            .iter()
            .find(|(dir, _)| *dir == self.dir)
            .map_or_else(|| display_dir(&self.dir), |(_, name)| name.to_string());
        let menu = self.combo_menu(Combo::Workspace, window, cx);
        let mut workspace = self
            .combo(
                "todo-dialog-workspace",
                Combo::Workspace,
                workspace_label.clone(),
                workspace_text,
                !self.locked,
                menu,
                cx,
            )
            .leading(svg().path(FOLDER_ICON).flex_none().size(px(13.)).text_color(muted));
        if let Some(branch) = self.branch.clone() {
            let mono = crate::ui::font::mono(cx);
            let text = div()
                .flex_none()
                .max_w(scaled(120.))
                .truncate()
                .text_size(FontSize::Small)
                .text_color(muted)
                .when_some(mono, |text, family| text.font_family(family))
                .child(branch.clone());
            workspace = workspace.trailing(text).aria_description(branch);
        }
        let workspace = if self.locked { workspace } else { Self::opens_menu(Combo::Workspace, workspace, cx) };
        let worktree_label = rust_i18n::t!("kanban.dialog.worktree").into_owned();
        let worktree = checkbox("todo-dialog-worktree", worktree_label, self.worktree, !self.locked, cx).when(
            !self.locked,
            |checkbox| {
                checkbox.on_press(cx, |dialog, _, cx| {
                    dialog.worktree = !dialog.worktree;
                    cx.notify();
                })
            },
        );
        let controls =
            div().flex().flex_wrap().items_center().gap_x(px(14.)).gap_y(px(8.)).child(workspace).child(worktree);
        row(workspace_label, controls, cx)
    }

    /// 交给谁那一行：agent，挑了 agent 时接着挑模型和强度；自己写命令时下面是命令框。
    fn render_agent_row(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let default = rust_i18n::t!("kanban.dialog.default").into_owned();
        let agent_label = rust_i18n::t!("kanban.dialog.agent").into_owned();
        let agent_text = self.agent.as_ref().map_or_else(
            || rust_i18n::t!("kanban.dialog.custom").into_owned(),
            |pick| pick.launcher().label.to_owned(),
        );
        let menu = self.combo_menu(Combo::Agent, window, cx);
        let agent = Self::opens_menu(
            Combo::Agent,
            self.combo("todo-dialog-agent", Combo::Agent, agent_label.clone(), agent_text, true, menu, cx)
                .leading(svg().path(SPARKLE_ICON).flex_none().size(px(13.)).text_color(muted)),
            cx,
        );
        // 框里值前面淡淡写上它是什么。
        let named = |label: String| div().flex_none().text_color(muted).child(label);
        let controls = div().flex().flex_wrap().items_center().gap(px(6.)).child(agent);
        let column = div().flex().flex_col().gap(px(6.));
        let column = match &self.agent {
            Some(pick) => {
                let launcher = pick.launcher();
                let catalog = cached(launcher);
                // 没有模型或强度参数的 agent 不画那个框；强度接在模型名后面的，挑了模型才能挑强度。
                let model = launcher.model_arg.map(|_| {
                    let text = pick.model.as_deref().map_or_else(
                        || default.clone(),
                        |model| catalog.as_deref().map_or(model, |catalog: &Catalog| catalog.label(model)).to_owned(),
                    );
                    let label = rust_i18n::t!("kanban.dialog.model").into_owned();
                    let menu = self.combo_menu(Combo::Model, window, cx);
                    Self::opens_menu(
                        Combo::Model,
                        self.combo("todo-dialog-model", Combo::Model, label.clone(), text, true, menu, cx)
                            .leading(named(label)),
                        cx,
                    )
                });
                let effort = launcher.effort_arg.map(|_| {
                    let enabled = pick.model.is_some() || !launcher.effort_needs_model;
                    let text = pick.effort.clone().filter(|_| enabled).unwrap_or_else(|| default.clone());
                    let label = rust_i18n::t!("kanban.dialog.effort").into_owned();
                    let menu = self.combo_menu(Combo::Effort, window, cx);
                    let effort = self
                        .combo("todo-dialog-effort", Combo::Effort, label.clone(), text, enabled, menu, cx)
                        .leading(named(label));
                    if enabled { Self::opens_menu(Combo::Effort, effort, cx) } else { effort }
                });
                // 找不到命令时开始了也只会在终端里报错，挑的时候就说出来。
                let missing = self.agent_missing().then(|| {
                    let note = rust_i18n::t!("kanban.dialog.agent_missing", agent = launcher.label).into_owned();
                    hint("todo-dialog-agent-missing", note, cx).text_color(cx.theme().destructive)
                });
                column.child(controls.children(model).children(effort)).children(missing)
            }
            None => column.child(controls).child(grow_input(&self.command)),
        };
        row(agent_label, column, cx)
    }

    /// skill 那一行，挑着的 agent 能用自定义 agent 时接着挑它。
    fn render_skills_row(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let none = rust_i18n::t!("kanban.dialog.none").into_owned();
        let skills_label = rust_i18n::t!("kanban.dialog.skills").into_owned();
        let skills_text = if self.skills.is_empty() { none.clone() } else { self.skills.join(", ") };
        let menu = self.combo_menu(Combo::Skills, window, cx);
        let skills = Self::opens_menu(
            Combo::Skills,
            self.combo("todo-dialog-skills", Combo::Skills, skills_label.clone(), skills_text, true, menu, cx),
            cx,
        );
        // 挑着的 agent 有自定义 agent 的参数、或者已经挑了（自己写的命令也可能用得上，存着的不能悄悄丢掉）时，
        // 和 skill 并排挑。
        let subagent = (self.takes_subagent() || self.subagent.is_some()).then(|| {
            let label = rust_i18n::t!("kanban.dialog.subagent").into_owned();
            let text = self.subagent.clone().unwrap_or_else(|| none.clone());
            let menu = self.combo_menu(Combo::Subagent, window, cx);
            let subagent = self
                .combo("todo-dialog-subagent", Combo::Subagent, label.clone(), text, true, menu, cx)
                .leading(div().flex_none().text_color(muted).child(label));
            Self::opens_menu(Combo::Subagent, subagent, cx)
        });
        let controls = div().flex().flex_wrap().items_center().gap(px(6.)).child(skills).children(subagent);
        row(skills_label, controls, cx)
    }

    /// 时限栏写的落在哪一档小签上（`limit_preset`）。
    fn limit_choice(&self, cx: &App) -> Option<Option<u32>> {
        limit_preset(self.time_limit.read(cx).query(), self.custom_limit)
    }

    /// 时限换成现成的一档（为空是不限），收起自己定的框。
    fn set_limit(&mut self, minutes: Option<u32>, cx: &mut Context<Self>) {
        self.custom_limit = false;
        let text = minutes.map(|minutes| minutes.to_string()).unwrap_or_default();
        self.time_limit.update(cx, |input, cx| input.set_query(text, cx));
        cx.notify();
    }

    /// 时限那一行：不限、现成的几档和自己定的小签，自己定时后面是分钟数的框；下面一句说明。开始过的任务照样能改。
    fn render_time_limit(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let subtle = cx.theme().subtle_foreground;
        let choice = self.limit_choice(cx);
        let none = pill(Chip::new(
            "todo-limit-none",
            rust_i18n::t!("kanban.dialog.time_limit_placeholder").into_owned(),
            choice == Some(None),
        ))
        .on_press(cx, |dialog, _, cx| dialog.set_limit(None, cx));
        let presets = LIMIT_PRESETS.map(|minutes| {
            pill(Chip::new(("todo-limit", minutes as usize), when::limit_text(minutes), choice == Some(Some(minutes))))
                .on_press(cx, move |dialog, _, cx| dialog.set_limit(Some(minutes), cx))
        });
        let custom = pill(Chip::new(
            "todo-limit-custom",
            rust_i18n::t!("kanban.dialog.limit_custom").into_owned(),
            choice.is_none(),
        ))
        .on_press(cx, |dialog, window, cx| {
            dialog.custom_limit = true;
            window.focus(&dialog.time_limit.focus_handle(cx), cx);
            cx.notify();
        });
        // 框里只有数字，单位写在后面。
        let input = choice.is_none().then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(Input::new(&self.time_limit).w(scaled(64.)).h(scaled(28.)))
                .child(div().text_color(subtle).child(rust_i18n::t!("kanban.dialog.time_limit_unit").into_owned()))
        });
        let chips = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .child(none)
            .children(presets)
            .child(custom)
            .children(input);
        let note = hint("todo-dialog-time-limit-hint", rust_i18n::t!("kanban.dialog.time_limit_hint").into_owned(), cx);
        let label = rust_i18n::t!("kanban.dialog.time_limit").into_owned();
        row(label, div().flex().flex_col().gap(px(6.)).child(chips).child(note), cx)
    }

    /// 附件一个一块，写文件名，报给辅助工具的是整个路径；后面跟着添加的按钮。
    fn render_attachments(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let primary_bg = cx.theme().mix(0.16);
        let chips: Vec<_> = self
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
        let controls = div().flex().flex_wrap().items_center().gap(px(6.)).children(chips).child(add_attachment);
        row(rust_i18n::t!("kanban.dialog.attachments").into_owned(), controls, cx)
    }

    /// 免确认一路跑完、做完自动标完成的两个复选框，排在一行，左栏不写标签。
    fn render_switches(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let auto = checkbox("todo-dialog-auto", rust_i18n::t!("kanban.dialog.auto").into_owned(), self.auto, true, cx)
            .on_press(cx, |dialog, _, cx| {
                dialog.auto = !dialog.auto;
                cx.notify();
            });
        let auto_done_label = rust_i18n::t!("kanban.dialog.auto_done").into_owned();
        let auto_done = checkbox("todo-dialog-auto-done", auto_done_label, self.auto_done, true, cx).on_press(
            cx,
            |dialog, _, cx| {
                dialog.auto_done = !dialog.auto_done;
                cx.notify();
            },
        );
        row(String::new(), div().flex().flex_wrap().gap_x(px(16.)).child(auto).child(auto_done), cx)
    }

    /// 照现在填的会怎么做的那句话（`summary_in`）。
    fn summary(&self, cx: &App) -> String {
        let agent = match &self.agent {
            Some(pick) => pick.launcher().label.to_owned(),
            // 自己写的命令说程序名，空着时说是自己写的命令。
            None => self
                .command
                .read(cx)
                .query()
                .split_whitespace()
                .next()
                .map_or_else(|| rust_i18n::t!("kanban.dialog.custom").into_owned(), str::to_owned),
        };
        let model = self.agent.as_ref().and_then(|pick| {
            let model = pick.model.as_deref()?;
            Some(cached(pick.launcher()).map_or_else(|| model.to_owned(), |catalog| catalog.label(model).to_owned()))
        });
        let project = self
            .workspaces
            .iter()
            .find(|(dir, _)| *dir == self.dir)
            .map_or_else(|| base_name(&self.dir), |(_, name)| name.to_string());
        let limit = parse_time_limit(self.time_limit.read(cx).query()).ok().flatten();
        let now = when::unix_now();
        let schedule_when = self.schedule.then(|| self.typed_when(cx).ok()).flatten();
        let when_text = match &schedule_when {
            Some(When::Once { at }) => when::fire_text(*at, now),
            Some(When::Unknown) | None => String::new(),
            Some(rule) => when::describe_when(rule),
        };
        let lead = match self.start_mode() {
            StartMode::Schedule => match schedule_when {
                Some(When::Once { .. }) => Lead::Once(&when_text),
                Some(When::Unknown) | None => Lead::Due,
                Some(_) => Lead::Repeat(&when_text),
            },
            _ if !self.startable() => Lead::Plain,
            StartMode::Now => Lead::Now,
            StartMode::Manual => Lead::Manual,
            StartMode::Queue => match self.after.as_deref() {
                None => Lead::Queued,
                Some(after) => {
                    Lead::After(store::data(cx).todos.iter().find(|todo| todo.id == after).map(|todo| todo.number))
                }
            },
        };
        let summary =
            Summary { lead, agent: &agent, model: model.as_deref(), project: &project, worktree: self.worktree, limit };
        summary_in(&summary, &rust_i18n::locale())
    }

    /// 底栏主按钮上的字：新建时说按下会怎样，编辑时是保存，还没开始的任务选了马上开始时是保存并开始。
    fn primary_label(&self) -> String {
        let new = self.editing.is_none();
        let key = match (self.start_mode(), self.startable()) {
            (StartMode::Schedule, _) if new => "kanban.dialog.create_schedule",
            (StartMode::Now, true) if new => "kanban.dialog.create_and_start",
            (StartMode::Now, true) => "kanban.save_and_start",
            (StartMode::Manual, true) if new => "kanban.dialog.create_todo",
            (StartMode::Queue, true) if new => "kanban.dialog.create_queued",
            _ => "kanban.save",
        };
        rust_i18n::t!(key).into_owned()
    }

    /// 底栏：左边是存的时候没办成的原因，没有时是照现在填的会怎么做；右边是取消和主按钮。
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, subtle, destructive) = (theme.border, theme.subtle_foreground, theme.destructive);
        let (primary_fg, mono_fg) = (theme.primary_foreground, theme.primary_foreground.opacity(0.7));
        let note = match self.error.clone() {
            Some(error) => {
                div().id("todo-dialog-error").aria_static_text(error.clone()).text_color(destructive).child(error)
            }
            None => {
                let summary = self.summary(cx);
                div()
                    .id("todo-dialog-summary")
                    .aria_static_text(summary.clone())
                    .line_clamp(2)
                    .text_size(FontSize::Small)
                    .text_color(subtle)
                    .child(summary)
            }
        };
        let cancel = Button::new("todo-dialog-cancel", rust_i18n::t!("kanban.cancel").into_owned())
            .variant(ButtonVariant::Outline)
            .h(scaled(32.))
            .px(px(14.))
            .rounded(px(8.))
            .cursor_pointer()
            .on_press(cx, |_, _, cx| cx.emit(DialogEvent::Discarded));
        // 「找不到命令」只按 PATH 找，shell 里用别名装的 agent 也会被说成找不到，所以照样让开始。键是提示词框
        // 提交的键（`TextareaEvent::Submit`）。
        let label = self.primary_label();
        let submit = Button::new("todo-dialog-save", label.clone())
            .variant(ButtonVariant::Default)
            .h(scaled(32.))
            .pl(px(14.))
            .pr(px(8.))
            .gap(px(8.))
            .rounded(px(8.))
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .when(self.saving, |button| button.opacity(0.5))
            .child(label)
            .child(Kbd::new(crate::keybinds::secondary_text("enter")).bg(primary_fg.opacity(0.1)).text_color(mono_fg))
            .on_press(cx, |this, _, cx| this.save(this.start, cx));
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(px(14.))
            .gap_y(px(10.))
            .pl(px(20.))
            .pr(px(14.))
            .pt(px(12.))
            .pb(px(14.))
            .border_t_1()
            .border_color(border)
            .child(div().flex_1().min_w(scaled(200.)).child(note))
            .child(div().flex_none().flex().gap(px(8.)).child(cancel).child(submit))
    }
}

impl Render for TodoDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focus_menu(window, cx);
        if let Some(field) = self.focus_wrong.take() {
            window.focus(&field, cx);
        }
        let title = self.heading();
        let surface = cx.theme().surface;
        let panel = Dialog::new("todo-dialog", title.clone())
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::pick_agent))
            .on_action(cx.listener(Self::pick_model))
            .on_action(cx.listener(Self::pick_effort))
            .on_action(cx.listener(Self::pick_workspace))
            .on_action(cx.listener(Self::pick_skill))
            .on_action(cx.listener(Self::pick_subagent))
            .on_action(cx.listener(Self::pick_queue))
            .on_action(cx.listener(Self::pick_template))
            .on_action(cx.listener(Self::open_templates))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if is_dismiss_key(&event.keystroke) {
                    cx.stop_propagation();
                    cx.emit(DialogEvent::Dismissed);
                } else {
                    this.tab_between_fields(event, window, cx);
                }
            }))
            .w(px(DIALOG_WIDTH))
            .max_w_full()
            .max_h_full()
            .rounded(px(14.))
            .bg(surface)
            .flex()
            .flex_col()
            .child(self.render_header(title, window, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(20.))
                    .pb(px(18.))
                    .flex()
                    .flex_col()
                    .child(self.render_fields(window, cx)),
            )
            .child(self.render_footer(cx));
        backdrop("todo-dialog-backdrop")
            .pt(px(TITLEBAR_HEIGHT + 32.))
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Dismissed)))
            .child(panel)
    }
}

impl TodoDialog {
    /// 定时那几栏现在填的规则；填得不对时是说明的键和该去改的那一栏。只有编辑读不懂的定时、还没选一种时是
    /// `When::Unknown`：保存时就不发 `when`，规则照原样留着。
    fn typed_when(&self, cx: &App) -> Result<When, (&'static str, &Entity<InputState>)> {
        Ok(match self.kind {
            None => When::Unknown,
            Some(Kind::Once) => {
                let original = match &self.editing {
                    Some(Editing::Schedule(Schedule { when: When::Once { at }, .. })) => Some(*at),
                    _ => None,
                };
                When::Once {
                    at: once_at(self.at.read(cx).query(), original).ok_or(("kanban.dialog.bad_once", &self.at))?,
                }
            }
            Some(Kind::Daily) => {
                let (hour, minute) = runode_todo::parse_clock(self.clock.read(cx).query())
                    .ok_or(("kanban.dialog.bad_daily", &self.clock))?;
                When::Daily { hour, minute, days: self.days }
            }
            Some(Kind::Every) => When::Every {
                minutes: parse_minutes(self.minutes.read(cx).query())
                    .ok_or(("kanban.dialog.bad_every", &self.minutes))?,
            },
            Some(Kind::Cron) => {
                let expr = self.cron.read(cx).query().trim().to_owned();
                if !runode_todo::valid_cron(&expr) {
                    return Err(("kanban.dialog.bad_cron", &self.cron));
                }
                let when = When::Cron { expr };
                if runode_todo::next_fire(&when, when::unix_now()).is_none() {
                    return Err(("kanban.dialog.cron_never", &self.cron));
                }
                when
            }
        })
    }

    /// 按填的内容造出要发的请求；填得不对时是给人看的原因和该去改的那一栏。
    fn request(&self, cx: &App) -> Result<store::Step, (String, FocusHandle)> {
        let wrong = |key: &str, field: FocusHandle| (rust_i18n::t!(key).into_owned(), field);
        let mut prompt = self.prompt.read(cx).text().trim().to_owned();
        let mut typed = self.title.read(cx).query().trim().to_owned();
        if let Some(label) = self.template.and_then(|index| self.templates[index].input.as_ref()) {
            let input = self.input.read(cx).query().trim();
            if input.is_empty() {
                let error = rust_i18n::t!("kanban.dialog.need_input", label = label).into_owned();
                return Err((error, self.input.focus_handle(cx)));
            }
            prompt = templates::fill(&prompt, input);
            typed = templates::fill(&typed, input);
        }
        let title = if typed.is_empty() { first_line(&prompt).to_owned() } else { typed };
        if title.is_empty() {
            return Err(wrong("kanban.dialog.need_title", self.prompt.focus_handle(cx)));
        }
        let agent = match &self.agent {
            Some(pick) => pick.command(),
            None => self.command.read(cx).query().trim().to_owned(),
        };
        if agent.is_empty() {
            return Err(wrong("kanban.dialog.need_agent", self.command.focus_handle(cx)));
        }
        let time_limit = parse_time_limit(self.time_limit.read(cx).query())
            .map_err(|()| wrong("kanban.dialog.bad_time_limit", self.time_limit.focus_handle(cx)))?;
        // 填的都查过了再看宿主认不认得：宿主旧了要重开 Runode，先把能就地改的说完。
        let too_old = |what: &str, field: FocusHandle| {
            let what = rust_i18n::t!(what);
            (rust_i18n::t!("kanban.dialog.host_too_old_for", what = what).into_owned(), field)
        };
        let check_time_limit = || {
            if time_limit.is_some() && store::host_below(cx, 2) {
                return Err(too_old("kanban.dialog.time_limit", self.time_limit.focus_handle(cx)));
            }
            Ok(())
        };
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
            auto_done: self.auto_done,
            time_limit,
        };
        if !self.schedule {
            check_time_limit()?;
            if self.queue_to_send().is_some() && store::host_below(cx, 3) {
                return Err(too_old("kanban.dialog.queue", self.focus.clone()));
            }
            return Ok(match &self.editing {
                Some(Editing::Todo(todo)) => {
                    let id = todo.id.clone();
                    Box::new(move |req| ClientMsg::UpdateTodo { req, id, spec: Some(spec), done: None })
                }
                _ => Box::new(move |req| ClientMsg::AddTodo { req, spec }),
            });
        }
        let when = self.typed_when(cx).map_err(|(key, input)| wrong(key, input.focus_handle(cx)))?;
        check_time_limit()?;
        let original = match &self.editing {
            Some(Editing::Schedule(schedule)) => Some(&schedule.when),
            _ => None,
        };
        if matches!(when, When::Cron { .. }) && original != Some(&when) && store::host_below(cx, 3) {
            return Err(too_old("kanban.dialog.kind_cron", self.cron.focus_handle(cx)));
        }
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

/// 一周七天按界面语言的习惯排好的顺序（`kanban.first_weekday` 是一周从哪天起），每个值从星期日的 0 数到星期六的 6。
pub(crate) fn week_order() -> impl Iterator<Item = u8> {
    let first = rust_i18n::t!("kanban.first_weekday").parse::<u8>().map_or(0, |day| day % 7);
    (0..7u8).map(move |offset| (first + offset) % 7)
}

/// 下一个整点，`YYYY-MM-DD HH:MM`：新建一次性的定时时先填上它。
fn next_hour() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    full_time(next_local_hour(now))
}

/// `now` 之后本地时间的下一个整点。按本地的分钟数补齐，不按 UTC 的整点：UTC+5:30 这类时区的 UTC 整点在本地
/// 是半点。
fn next_local_hour(now: u64) -> u64 {
    let minute = LocalTime::from_unix(now).map_or(0, |at| at.minute);
    now - now % 60 + u64::from(60 - minute) * 60
}

/// `YYYY-MM-DD HH:MM`，按本地时间。
pub(crate) fn full_time(at: u64) -> String {
    LocalTime::from_unix(at).map_or_else(String::new, |at| at.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clocks_minutes_and_dates() {
        assert_eq!(parse_minutes("15"), Some(15));
        assert_eq!(parse_minutes("0"), None);
        assert_eq!(parse_minutes("x"), None);
        assert_eq!(first_line("\n  fix it \nmore"), "fix it");
    }

    #[test]
    fn parses_time_limits() {
        assert_eq!(parse_time_limit("  "), Ok(None));
        assert_eq!(parse_time_limit("0"), Err(()));
        assert_eq!(parse_time_limit("x"), Err(()));
        assert_eq!(parse_time_limit(" 15 "), Ok(Some(15)));
    }

    /// 时限栏写的落在哪一档：空着是不限，现成的几档各是各的，别的和选了自己定的都是自己定。
    #[test]
    fn limit_presets_follow_the_text() {
        assert_eq!(limit_preset(" ", false), Some(None));
        assert_eq!(limit_preset("30", false), Some(Some(30)));
        assert_eq!(limit_preset(" 60 ", false), Some(Some(60)));
        assert_eq!(limit_preset("45", false), None);
        assert_eq!(limit_preset("x", false), None);
        assert_eq!(limit_preset("30", true), None);
        assert_eq!(limit_preset("", true), None);
    }

    /// 接下来几次：重复的列三次，一次性的只有一次，过去了的一次都没有。
    #[test]
    fn next_fires_list_up_to_three() {
        let now = 1_800_000_000;
        assert_eq!(next_fires(&When::Every { minutes: 15 }, now), [now + 900, now + 1800, now + 2700]);
        assert_eq!(next_fires(&When::Once { at: now + 60 }, now), [now + 60]);
        assert!(next_fires(&When::Once { at: now - 60 }, now).is_empty());
        assert!(next_fires(&When::Unknown, now).is_empty());
        let daily = next_fires(&When::Daily { hour: 9, minute: 0, days: 0 }, now);
        assert_eq!(daily.len(), 3);
        assert!(daily.windows(2).all(|pair| pair[1] > pair[0]));
    }

    /// 底栏那句话：开头的什么时候做、有没有模型、开不开工作树、有没有时限各种搭配，三种语言都是整句。
    #[test]
    fn summaries_read_as_sentences() {
        let summary = |lead, model, worktree, limit| Summary {
            lead,
            agent: "Claude Code",
            model,
            project: "demo",
            worktree,
            limit,
        };
        let cases = [
            (
                summary(Lead::Repeat("每天 09:00"), Some("opus"), true, Some(30)),
                "zh-Hans",
                "每天 09:00 开一个新任务：交给 Claude Code（opus），在 demo 的新工作树里做，最多跑 30 分钟。",
            ),
            (summary(Lead::Now, None, false, None), "zh-Hans", "存好就开始：交给 Claude Code，在 demo 里做。"),
            (
                summary(Lead::After(Some(3)), Some("opus"), false, Some(60)),
                "zh-Hans",
                "等 #3 做完自动开始：交给 Claude Code（opus），在 demo 里做，最多跑 1 小时。",
            ),
            (summary(Lead::Plain, None, true, None), "zh-Hans", "交给 Claude Code，在 demo 的新工作树里做。"),
            (
                summary(Lead::Queued, None, false, None),
                "zh-Hans",
                "放进待办排队，有空位就开始：交给 Claude Code，在 demo 里做。",
            ),
            (
                summary(Lead::Manual, Some("opus"), true, None),
                "zh-Hant",
                "先放進待辦，等你點開始：交給 Claude Code（opus），在 demo 的新工作樹裡做。",
            ),
            (
                summary(Lead::Queued, None, false, Some(15)),
                "zh-Hant",
                "放進待辦排隊，有空位就開始：交給 Claude Code，在 demo 裡做，最多跑 15 分鐘。",
            ),
            (
                summary(Lead::Now, Some("opus"), true, Some(30)),
                "en",
                "Starts once saved. Claude Code (opus) works in a new worktree of demo for up to 30 min.",
            ),
            (
                summary(Lead::Once("tomorrow at 09:00"), None, false, None),
                "en",
                "Opens a new task tomorrow at 09:00. Claude Code works in demo.",
            ),
            (
                summary(Lead::After(None), None, true, None),
                "en",
                "Starts after the task it waits for is done. Claude Code works in a new worktree of demo.",
            ),
            (
                summary(Lead::Due, None, false, Some(60)),
                "en",
                "Opens a new task when it is due. Claude Code works in demo for up to 1 h.",
            ),
        ];
        for (summary, locale, want) in cases {
            assert_eq!(summary_in(&summary, locale), want, "{locale}");
        }
    }

    #[test]
    fn queue_change_sends_only_what_changed() {
        // 新建：不排队不发，排队才发。
        assert_eq!(queue_change(None, false, None), None);
        assert_eq!(queue_change(None, true, None), Some((true, None)));
        assert_eq!(queue_change(None, true, Some("b")), Some((true, Some("b".into()))));
        // 编辑：没变不发，改了等谁发，不再排队时不等谁。
        assert_eq!(queue_change(Some((true, Some("b"))), true, Some("b")), None);
        assert_eq!(queue_change(Some((false, None)), false, None), None);
        assert_eq!(queue_change(Some((true, Some("b"))), true, Some("c")), Some((true, Some("c".into()))));
        assert_eq!(queue_change(Some((true, Some("b"))), false, Some("b")), Some((false, None)));
    }

    /// 分段跟着字段走：定时盖过排队和马上开始，排队盖过马上开始，都没有时是放进待办。
    #[test]
    fn start_modes_follow_the_fields() {
        let cases = [
            ((true, true, true), StartMode::Schedule),
            ((true, false, false), StartMode::Schedule),
            ((false, true, true), StartMode::Queue),
            ((false, false, true), StartMode::Queue),
            ((false, true, false), StartMode::Now),
            ((false, false, false), StartMode::Manual),
        ];
        for ((schedule, start, queued), want) in cases {
            assert_eq!(start_mode_of(schedule, start, queued), want, "{schedule} {start} {queued}");
        }
    }

    fn todo(id: &str, after: Option<&str>, started: Option<u64>, done_at: Option<u64>) -> Todo {
        Todo {
            id: id.into(),
            number: 1,
            spec: TodoSpec { title: id.into(), dir: "/tmp".into(), agent: "claude".into(), ..Default::default() },
            created: 0,
            started,
            session: None,
            worktree_dir: None,
            branch: None,
            done_at,
            schedule: None,
            rank: 0,
            outcome: None,
            result: None,
            queued: after.is_some(),
            after: after.map(str::to_owned),
        }
    }

    #[test]
    fn after_candidates_skip_done_self_and_loops() {
        let ids = |todos: Vec<&Todo>| todos.into_iter().map(|todo| todo.id.clone()).collect::<Vec<_>>();
        // b 等 me，a 等 b：a 等下去会绕回 me。
        let todos = [
            todo("me", None, None, None),
            todo("a", Some("b"), None, None),
            todo("b", Some("me"), None, None),
            todo("done", None, Some(1), Some(2)),
            todo("running", None, Some(1), None),
        ];
        assert_eq!(ids(after_candidates(&todos, Some("me"))), ["running"]);
        assert_eq!(ids(after_candidates(&todos, None)), ["me", "a", "b", "running"]);
        assert!(waits_for(&todos, "a", "me"));
        assert!(!waits_for(&todos, "running", "me"));
        // 和自己无关的环不会一直绕下去。
        let looped = [todo("x", Some("y"), None, None), todo("y", Some("x"), None, None), todo("me", None, None, None)];
        assert!(!waits_for(&looped, "x", "me"));
        assert_eq!(ids(after_candidates(&looped, Some("me"))), ["x", "y"]);
        // 报了没做成的不能等：等它的会跟着记成没做成。
        let mut failed = todo("failed", None, Some(1), None);
        failed.outcome = Some(Outcome::Failed);
        assert!(after_candidates(&[failed], None).is_empty());
    }

    // 不改 `TZ`：本地时间来回一趟原样回来，和时区无关。
    #[test]
    fn local_times_round_trip() {
        let at = LocalTime::parse("2026-10-13 14:30").and_then(LocalTime::unix).unwrap();
        assert_eq!(full_time(at), "2026-10-13 14:30");
        assert!(next_hour().ends_with(":00"));
        // 时间栏没改时带秒的原值照旧，改了才按填的算。
        assert_eq!(once_at("2026-10-13 14:30", Some(at + 17)), Some(at + 17));
        assert_eq!(once_at("2026-10-13 14:31", Some(at + 17)), Some(at + 60));
        assert_eq!(once_at("2026-10-13 14:30", None), Some(at));
        for now in [at, at + 59, at + 30 * 60, at + 89 * 60 + 59] {
            let next = next_local_hour(now);
            assert!(next > now && next - now <= 3600, "{now} -> {next}");
            assert_eq!(LocalTime::from_unix(next).unwrap().minute, 0, "{now} -> {next}");
        }
    }
}
