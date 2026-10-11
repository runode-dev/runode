//! 右侧面板的 GitHub Actions 页，照 VS Code 的 GitHub Actions 扩展分三段：当前分支最近的运行；
//! 仓库的工作流和各自最近的运行；设置里仓库、组织和各部署环境的 secret 和 variable。运行展开是
//! job，job 展开是 step，重跑过的运行另有之前的几次尝试。行尾的按钮在浏览器里打开、重跑、取消、
//! 复制、删除；看日志、盯着运行、触发工作流（有输入参数时 gh 一个个问）和填 secret、variable 的值
//! 在旁边分出的终端里交给 gh。工作流可以固定到状态栏上，显示它最近一次运行的状态。
//!
//! 经用户自己装的 gh 读写，见 `gh`；和模拟器页一样用终端里 shell 报告的 PATH 找它。窗口在前台时，
//! 页上显示着的和固定在状态栏上的数据隔 `POLL_IDLE` 重读一次，有没跑完的运行或 job 时隔 `POLL_RUNNING`。

mod gh;

use std::{
    borrow::Cow,
    cell::Cell,
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    AccessibleAction, AnyElement, App, Axis, Bounds, ClipboardItem, Context, CursorStyle, Div, Entity, EntityId,
    Focusable, MouseButton, MouseDownEvent, Pixels, Role, ScrollHandle, SharedString, Stateful, Subscription, Window,
    canvas, div, prelude::*, px, svg,
};
use runode_shared_types::color::Rgb;

use super::{
    DIVIDER_GRAB_WIDTH, Divider, WindowView, divider_color,
    git_panel::chevron,
    inline_edit::InlineEdit,
    model::ToolCache,
    persist::format::PinnedWorkflow,
    project::{ADDED, MODIFIED, REMOVED, panel_message, panel_shell, panel_title},
    row_buttons::{RowButton, button, row_buttons},
    simulator::text_button,
    status_bar,
    titlebar::icon_toggle,
};
use crate::{
    assets::{
        ACTIONS_CANCELLED_ICON, ACTIONS_FAILURE_ICON, ACTIONS_INPROGRESS_ICON, ACTIONS_PENDING_ICON,
        ACTIONS_QUEUED_ICON, ACTIONS_SKIPPED_ICON, ACTIONS_SUCCESS_ICON, ACTIONS_WAITING_ICON, ACTIONS_WORKFLOW_ICON,
        BAN_ICON, COPY_ICON, GLOBE_ICON, OPEN_FILE_ICON, PENCIL_ICON, PIN_ICON, PLAY_ICON, PLUS_ICON, REFRESH_ICON,
        SYNC_ICON, TERMINAL_ICON, TRASH_ICON,
    },
    terminal_view::{TerminalEvent, TerminalView},
    ui::{
        a11y::{A11yPress, PressDown},
        hsla,
        scrollbar::scrollbar,
        tooltip::tooltip,
    },
};
use gh::{Data, Gh, Job, Query, Run, Scope, State, Workflow};

/// 重跑、取消以后隔多久再读一次运行的状态。
const RUN_SETTLE: Duration = Duration::from_secs(3);
const ROW_HEIGHT: f32 = 22.;
/// 拖分隔线时每段至少留这么高（标题加一行），辅助工具每调一下改这么多。
const SECTION_MIN_HEIGHT: f32 = ROW_HEIGHT * 2.;
const SECTION_STEP: f32 = ROW_HEIGHT * 3.;
/// 三段标题的文字。
const SECTION_KEYS: [&str; 3] =
    ["github_actions.current_branch", "github_actions.workflows", "github_actions.settings"];
/// 树里每深一层往右缩进的宽度。
const INDENT: f32 = 12.;

/// 树里能展开的一项。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Node {
    /// 三段：当前分支、工作流、设置。
    Branch,
    Workflows,
    Settings,
    Run(u64),
    /// 一次运行之前的几次尝试。
    Attempts(u64),
    Attempt(u64, u32),
    Job(u64),
    Workflow(u64),
    Secrets(Scope),
    Variables(Scope),
    Environments,
    Environment(String),
}

impl Node {
    fn section(&self) -> bool {
        matches!(self, Self::Branch | Self::Workflows | Self::Settings)
    }
}

/// 正在给 `scope` 那一级加 secret（`secret` 为真）或 variable，就地输入名字。
struct Adding {
    secret: bool,
    scope: Scope,
    edit: InlineEdit,
}

pub(super) struct ActionsPage {
    /// 数据是在哪个仓库里读的；终端换了仓库时清掉重读。
    dir: Option<PathBuf>,
    data: HashMap<Query, Result<Data, String>>,
    fetched: HashMap<Query, Instant>,
    loading: HashSet<Query>,
    /// 上一帧页上用到的数据，定时重读只读这些。
    shown: HashSet<Query>,
    expanded: HashSet<Node>,
    hovered: Option<usize>,
    adding: Option<Adding>,
    /// 重跑、取消、删除和打开组织设置页出的错，显示在标题下面，下次操作或刷新时清掉。
    error: Option<String>,
    /// 上次看 PATH 里有没有 gh 时的 PATH 和结果，标签要不要显示靠它，PATH 没变就不再找。
    found: ToolCache,
    /// 三段各自的滚动位置。
    scrolls: [ScrollHandle; 3],
    /// 拖过分隔线的段的高度（连标题），没拖过的为空，按内容和剩下的地方定。
    pub(super) heights: [Option<f32>; 3],
    /// 上一帧三段在窗口里的位置，拖分隔线时按它算高度。
    bounds: Rc<Cell<[Option<Bounds<Pixels>>; 3]>>,
    a11y: bool,
    /// 在终端里填 secret、variable 值的那几个终端，按终端记着订阅，gh 跑完时重读对应的列表，见 `set_in_terminal`。
    watches: HashMap<EntityId, Subscription>,
}

impl Default for ActionsPage {
    fn default() -> Self {
        Self {
            dir: None,
            data: HashMap::new(),
            fetched: HashMap::new(),
            loading: HashSet::new(),
            shown: HashSet::new(),
            expanded: HashSet::from([Node::Branch, Node::Workflows]),
            hovered: None,
            adding: None,
            error: None,
            found: ToolCache::default(),
            scrolls: [ScrollHandle::new(), ScrollHandle::new(), ScrollHandle::new()],
            heights: [None; 3],
            bounds: Rc::default(),
            a11y: false,
            watches: HashMap::new(),
        }
    }
}

/// 一帧里排出来的行，以及用到的数据。
#[derive(Default)]
struct Walk {
    rows: Vec<AnyElement>,
    /// 前面几段已经拿走的行数；行号接着往下数，悬停记的行号在三段之间不重。
    base: usize,
    /// 排好的三段：标题行和展开后的各行。
    sections: Vec<(Vec<AnyElement>, Vec<AnyElement>)>,
    shown: HashSet<Query>,
}

impl Walk {
    /// 下一行的行号。
    fn ix(&self) -> usize {
        self.base + self.rows.len()
    }

    fn take(&mut self) -> Vec<AnyElement> {
        let rows = std::mem::take(&mut self.rows);
        self.base += rows.len();
        rows
    }

    /// 一段排完了：`header` 是它的标题行，排在后面的行是它展开的内容。
    fn end_section(&mut self, header: Vec<AnyElement>) {
        let body = self.take();
        self.sections.push((header, body));
    }
}

impl WindowView {
    pub(super) fn github_actions_shown(&self) -> bool {
        self.workspace().panel == Some(super::project::SidePanel::GitHubActions)
    }

    /// gh 在仓库根目录里跑；面板收着、还没读过 git 状态时在终端的目录里跑。
    fn actions_dir(&self, cx: &Context<Self>) -> PathBuf {
        match &self.workspace().project.git {
            Some(git) => git.main.root.clone(),
            None => self.project_dir(cx),
        }
    }

    fn actions_gh(&self, cx: &Context<Self>) -> Gh {
        Gh { dir: self.actions_dir(cx), path: self.shell_path(cx) }
    }

    /// 当前仓库所在的分支，分离头指针时为空。
    fn actions_branch(&self) -> Option<String> {
        self.workspace().project.git.as_ref()?.main.info.branch.clone()
    }

    /// PATH 里有 gh、当前目录在仓库里时才显示标签；正显示着时仍留着。
    pub(super) fn github_actions_tab_visible(&self, cx: &App) -> bool {
        if self.github_actions_shown() {
            return true;
        }
        if !self.git_button_visible() {
            return false;
        }
        self.tool_on_path(&self.github_actions.found, gh::PROGRAM, cx)
    }

    /// 终端换了仓库时清掉读过的数据，展开的只留三段。
    fn sync_actions_dir(&mut self, cx: &Context<Self>) {
        let dir = self.actions_dir(cx);
        let page = &mut self.github_actions;
        if page.dir.as_ref() == Some(&dir) {
            return;
        }
        page.dir = Some(dir);
        page.data.clear();
        page.fetched.clear();
        page.loading.clear();
        page.shown.clear();
        page.error = None;
        page.adding = None;
        page.expanded.retain(Node::section);
    }

    /// 在后台读一份数据；同一份正在读时不再读。读完时仓库已经换了就丢掉。
    fn fetch_actions(&mut self, query: Query, cx: &mut Context<Self>) {
        if !self.github_actions.loading.insert(query.clone()) {
            return;
        }
        let gh = self.actions_gh(cx);
        let dir = gh.dir.clone();
        let job = cx.background_spawn({
            let query = query.clone();
            async move { gh.fetch(&query) }
        });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                let page = &mut this.github_actions;
                if page.dir.as_ref() != Some(&dir) {
                    return;
                }
                page.loading.remove(&query);
                page.fetched.insert(query.clone(), Instant::now());
                if page.data.get(&query) != Some(&result) {
                    page.data.insert(query, result);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 窗口在前台时每秒看一次：页上显示着的、固定在状态栏上的数据，到时候了就重读。
    pub(super) fn poll_github_actions(&mut self, cx: &mut Context<Self>) {
        let shown = self.github_actions_shown();
        let pinned = status_bar::shown(cx) && !self.workspace().pinned_workflows.is_empty();
        if !shown && !pinned {
            return;
        }
        self.sync_actions_dir(cx);
        let mut wanted: Vec<Query> = if shown { self.actions_queries() } else { Vec::new() };
        if pinned {
            wanted.extend(self.workspace().pinned_workflows.iter().map(|pin| Query::WorkflowRuns(pin.id)));
        }
        let page = &self.github_actions;
        let now = Instant::now();
        let due: Vec<Query> = wanted
            .into_iter()
            .filter(|query| gh::due(page.data.get(query), page.fetched.get(query).copied(), now))
            .collect();
        for query in due {
            self.fetch_actions(query, cx);
        }
    }

    /// 页上要重读的数据：上一帧树里用到的，加上等着登录的。显示登录说明时树没排，`shown` 可能是空的，
    /// 等着登录的那几份（比如状态栏上固定的工作流读出来的）也要接着读，登好以后页才恢复得过来。
    fn actions_queries(&self) -> Vec<Query> {
        let page = &self.github_actions;
        let login = page.data.iter().filter(|(_, result)| matches!(result, Err(err) if err == gh::LOGIN));
        let mut queries: Vec<Query> = page.shown.iter().cloned().collect();
        queries.extend(login.map(|(query, _)| query.clone()).filter(|query| !page.shown.contains(query)));
        queries
    }

    /// 刷新按钮：清掉出的错，显示着的数据都重读。
    fn refresh_github_actions(&mut self, cx: &mut Context<Self>) {
        self.github_actions.error = None;
        for query in self.actions_queries() {
            self.fetch_actions(query, cx);
        }
        cx.notify();
    }

    /// 重跑、取消以后页上显示着的运行和 job 马上重读，过 `RUN_SETTLE` 再读一次：GitHub 要过一会儿才把
    /// 运行改成排队或取消，第一次多半还读到原来的状态；读到没跑完的以后才按快的间隔接着读。
    fn refetch_runs(&mut self, cx: &mut Context<Self>) {
        self.refetch_runs_now(cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RUN_SETTLE).await;
            this.update(cx, |this, cx| this.refetch_runs_now(cx)).ok();
        })
        .detach();
    }

    fn refetch_runs_now(&mut self, cx: &mut Context<Self>) {
        let page = &self.github_actions;
        let pinned = self.workspace().pinned_workflows.iter().map(|pin| Query::WorkflowRuns(pin.id));
        let runs: HashSet<Query> = page
            .shown
            .iter()
            .filter(|query| matches!(query, Query::BranchRuns(_) | Query::WorkflowRuns(_) | Query::Jobs(..)))
            .cloned()
            .chain(pinned)
            .collect();
        for query in runs {
            self.fetch_actions(query, cx);
        }
    }

    /// 删完 secret 或 variable 以后马上重读页上显示着的那几份。
    fn refetch_settings(&mut self, cx: &mut Context<Self>) {
        let settings: Vec<Query> = self
            .github_actions
            .shown
            .iter()
            .filter(|query| matches!(query, Query::Secrets(_) | Query::Variables(_)))
            .cloned()
            .collect();
        for query in settings {
            self.fetch_actions(query, cx);
        }
    }

    /// 在后台跑一条 gh 命令，出错时显示在标题下面；跑完不管成败都交给 `then`，成了时带上结果。
    fn run_gh<T: Send + 'static>(
        &mut self,
        work: impl FnOnce(&Gh) -> Result<T, String> + Send + 'static,
        then: impl FnOnce(&mut Self, Option<T>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.github_actions.error = None;
        let gh = self.actions_gh(cx);
        let job = cx.background_spawn(async move { work(&gh) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                let value = match result {
                    Ok(value) => Some(value),
                    Err(err) => {
                        this.github_actions.error = Some(gh_error(err));
                        None
                    }
                };
                then(this, value, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 在仓库目录里分出一个终端跑 `command`，放在哪按配置项 `task-placement`，和项目命令一样。
    fn run_gh_in_terminal(
        &mut self,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<TerminalView>> {
        let dir = self.actions_dir(cx);
        self.run_in_terminal(&dir, command, window, cx)
    }

    /// 在终端里交给 gh 设 `scope` 那一级叫 `name` 的 secret（`secret` 为真）或 variable 的值。值在终端里填，
    /// 页上不知道什么时候填完，所以盯着那个终端：gh 跑起来又回到 shell（或者终端关了）时重读那份列表。
    fn set_in_terminal(&mut self, secret: bool, scope: Scope, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let command = gh::set_command(secret, &scope, name);
        let Some(view) = self.run_gh_in_terminal(command, window, cx) else {
            return;
        };
        let query = if secret { Query::Secrets(scope) } else { Query::Variables(scope) };
        let id = view.entity_id();
        // 命令打进去之前前台就是 shell，看到过前台不是 shell 以后再回到 shell 才算跑完。
        let mut ran = false;
        let watch = cx.subscribe(&view, move |this, view, event: &TerminalEvent, cx| {
            let done = match event {
                TerminalEvent::Exited => true,
                TerminalEvent::MetaChanged => {
                    let shell = view.read(cx).meta().foreground_is_shell;
                    ran |= !shell;
                    ran && shell
                }
                _ => false,
            };
            if done {
                this.github_actions.watches.remove(&id);
                this.fetch_actions(query.clone(), cx);
            }
        });
        self.github_actions.watches.insert(id, watch);
    }

    fn toggle_actions_node(&mut self, node: Node, cx: &mut Context<Self>) {
        let expanded = &mut self.github_actions.expanded;
        if !expanded.remove(&node) {
            expanded.insert(node);
        }
        cx.notify();
    }

    fn toggle_pin(&mut self, workflow: &Workflow, cx: &mut Context<Self>) {
        let pins = &mut self.workspace_mut().pinned_workflows;
        match pins.iter().position(|pin| pin.id == workflow.id) {
            Some(ix) => {
                pins.remove(ix);
            }
            None => pins.push(PinnedWorkflow { id: workflow.id, name: workflow.name.clone() }),
        }
        self.save(cx);
        cx.notify();
    }

    fn start_adding(&mut self, secret: bool, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
        let node = if secret { Node::Secrets(scope.clone()) } else { Node::Variables(scope.clone()) };
        self.github_actions.expanded.insert(node);
        let edit = InlineEdit::new(String::new(), 0, Self::finish_adding, window, cx);
        self.github_actions.adding = Some(Adding { secret, scope, edit });
        cx.notify();
    }

    /// 切走或收起页时丢掉还没填完的名字。
    pub(super) fn cancel_github_actions_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(adding) = self.github_actions.adding.take() {
            adding.edit.release_focus(&self.focus_handle(cx), window, cx);
        }
    }

    /// 名字填好了：在终端里交给 gh 问值。
    fn finish_adding(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(adding) = self.github_actions.adding.take() else {
            return;
        };
        adding.edit.release_focus(&self.focus_handle(cx), window, cx);
        let name = adding.edit.text(cx).trim().to_owned();
        if commit && !name.is_empty() {
            self.set_in_terminal(adding.secret, adding.scope, &name, window, cx);
        }
        cx.notify();
    }

    fn delete_setting(
        &mut self,
        secret: bool,
        scope: Scope,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = if secret {
            rust_i18n::t!("github_actions.delete_secret_title", name = name)
        } else {
            rust_i18n::t!("github_actions.delete_variable_title", name = name)
        };
        let detail = rust_i18n::t!("github_actions.delete_detail");
        let confirm = rust_i18n::t!("github_actions.delete");
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &title,
            Some(&detail),
            &[&confirm, &*rust_i18n::t!("git.cancel")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            this.update(cx, |this, cx| {
                this.run_gh(move |gh| gh.delete(secret, &scope, &name), |this, _, cx| this.refetch_settings(cx), cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_github_actions_panel(
        &mut self,
        width: f32,
        fg: Rgb,
        bg: Rgb,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.sync_actions_dir(cx);
        self.github_actions.a11y = window.is_a11y_active();
        let view = cx.entity().downgrade();
        let refresh = icon_toggle("actions-refresh", REFRESH_ICON, 14., false, fg, bg)
            .aria_label(rust_i18n::t!("github_actions.refresh").into_owned())
            .flex_none()
            .size(px(22.))
            .tooltip(tooltip(rust_i18n::t!("github_actions.refresh"), None, fg, bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.refresh_github_actions(cx);
                }),
            )
            .on_a11y_press(view, |this, _, cx| this.refresh_github_actions(cx));
        let title = panel_title()
            .child(div().flex_1().min_w_0().truncate().text_color(hsla(fg)).child("GitHub Actions"))
            .child(refresh);
        let shell = panel_shell("github-actions-panel", width, fg, bg, cx)
            .bg(hsla(bg.mix(fg, 0.03)))
            .text_size(px(12.))
            .child(self.render_panel_tabs(fg, bg, cx))
            .child(title);
        if !self.git_button_visible() {
            return shell.child(panel_message(rust_i18n::t!("panel.not_repo").into_owned(), fg));
        }
        let missing = self.github_actions.data.values().any(|result| matches!(result, Err(err) if err == gh::MISSING));
        if missing {
            return shell.child(panel_message(rust_i18n::t!("github_actions.missing").into_owned(), fg));
        }
        // 没登录时每份数据都是同一个错，整页换成登录的说明，不在每段里各说一遍。
        let login = self.github_actions.data.values().any(|result| matches!(result, Err(err) if err == gh::LOGIN));
        if login {
            return shell.child(self.render_actions_login(fg, bg, cx));
        }
        let error = self.github_actions.error.clone().map(|error| {
            div()
                .id("actions-error")
                .role(Role::Alert)
                .aria_label(error.clone())
                .flex_none()
                .px(px(10.))
                .pb(px(6.))
                .text_color(hsla(REMOVED))
                .child(error)
        });
        let mut walk = Walk::default();
        self.walk_actions(&mut walk, fg, bg, cx);
        // 树里展开了、还没读过的现在读。
        let missing: Vec<Query> =
            walk.shown.iter().filter(|query| !self.github_actions.data.contains_key(*query)).cloned().collect();
        self.github_actions.shown = walk.shown;
        for query in missing {
            self.fetch_actions(query, cx);
        }
        // 和 VS Code 一样三段的标题一直看得见：展开的段各自滚动，内容短的只占自己那么高，
        // 剩下的地方由内容长的几段平分，当前分支的运行再多也挤不走下面两段。段和段之间的分隔线
        // 能拖：拖过的段按拖出的高度，最后一段展开的总是填满剩下的地方。
        let page = &self.github_actions;
        let expanded: Vec<bool> = walk.sections.iter().map(|(_, body)| !body.is_empty()).collect();
        let last = expanded.iter().rposition(|open| *open);
        let heights = page.heights;
        let dragged = heights.iter().any(Option::is_some);
        let all_bounds = page.bounds.clone();
        let sections =
            walk.sections.into_iter().zip(page.scrolls.clone()).enumerate().map(|(si, ((header, body), scroll))| {
                let measure = {
                    let all_bounds = all_bounds.clone();
                    canvas(
                        move |bounds, _, _| {
                            let mut all = all_bounds.get();
                            all[si] = Some(bounds);
                            all_bounds.set(all);
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full()
                };
                let section = div()
                    .relative()
                    .flex()
                    .flex_col()
                    .when(si > 0, |section| section.border_t_1().border_color(divider_color(hsla(fg))))
                    .child(measure)
                    .children(header)
                    // 上面一段展开着才能拖这一段上沿的分隔线。
                    .when(si > 0 && expanded[si - 1], |section| section.child(self.actions_divider(si, cx)));
                if body.is_empty() {
                    return section.flex_none();
                }
                let height = (body.len() + 1) as f32 * ROW_HEIGHT + 1.;
                let section = match heights[si].filter(|_| Some(si) != last) {
                    Some(set) => {
                        section.flex_initial().h(px(set.min(height))).min_h(px(SECTION_MIN_HEIGHT.min(height)))
                    }
                    // 拖过分隔线以后最后一段填满剩下的地方，分隔线停在松手的位置，不在底下空出一截。
                    None if Some(si) == last && dragged => section.flex_1().min_h_0(),
                    None => section.flex_1().min_h_0().max_h(px(height)),
                };
                let list = div()
                    .id(("actions-section", si))
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .children(body);
                section.child(div().flex_1().min_h_0().relative().child(list).child(scrollbar(
                    ("actions-scroll", si),
                    scroll,
                    Axis::Vertical,
                    hsla(fg),
                )))
            });
        let tree = div()
            .id("actions-tree")
            .role(Role::Tree)
            .aria_label("GitHub Actions")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .children(sections);
        shell.children(error).child(tree)
    }

    /// 第 `si` 段上沿的分隔线：拖动改上面那段的高度，双击三段都恢复按内容排。辅助工具报成分隔条，
    /// 加减一下改三行高。
    fn actions_divider(&self, si: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let above = rust_i18n::t!(SECTION_KEYS[si - 1]);
        let label = rust_i18n::t!("github_actions.resize", name = above).into_owned();
        let view = cx.entity().downgrade();
        let nudge = move |delta: f32| {
            let view = view.clone();
            move |_: Option<&gpui::accesskit::ActionData>, _: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| {
                    let above = this.github_actions.bounds.get()[si - 1];
                    if let Some(above) = above {
                        this.resize_actions_section(si, f32::from(above.bottom()) + delta);
                        cx.notify();
                    }
                })
                .ok();
            }
        };
        div()
            .id(("actions-divider", si))
            .role(Role::Splitter)
            .aria_label(label)
            .absolute()
            .left_0()
            .top(px(-DIVIDER_GRAB_WIDTH / 2.))
            .w_full()
            .h(px(DIVIDER_GRAB_WIDTH))
            .cursor(CursorStyle::ResizeUpDown)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        this.github_actions.heights = [None; 3];
                        this.save(cx);
                    } else {
                        this.dragging_divider = Some(Divider::GitHubActions(si));
                    }
                    cx.notify();
                }),
            )
            .on_a11y_action(AccessibleAction::Increment, nudge(SECTION_STEP))
            .on_a11y_action(AccessibleAction::Decrement, nudge(-SECTION_STEP))
    }

    /// 拖第 `si` 段上沿的分隔线到窗口里的纵坐标 `y`：上面那段的下沿跟到 `y`，下面那段拖过高度的
    /// 下沿不动；两段都至少留一个标题加一行。
    pub(super) fn resize_actions_section(&mut self, si: usize, y: f32) {
        let page = &mut self.github_actions;
        let bounds = page.bounds.get();
        let (Some(above), Some(below)) = (bounds[si - 1], bounds[si]) else {
            return;
        };
        let top = f32::from(above.origin.y);
        let bottom = f32::from(below.bottom());
        let below_min = f32::from(below.size.height).min(SECTION_MIN_HEIGHT);
        let y = y.min(bottom - below_min).max(top + SECTION_MIN_HEIGHT);
        page.heights[si - 1] = Some(y - top);
        if page.heights[si].is_some() {
            page.heights[si] = Some(bottom - y);
        }
    }

    /// gh 没登录或令牌失效：说明，和在旁边的终端里跑 `gh auth login` 的按钮。登好以后定时重读时自己恢复。
    fn render_actions_login(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Div {
        let text = rust_i18n::t!("github_actions.login_needed").into_owned();
        let login = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            this.run_gh_in_terminal(gh::LOGIN_COMMAND.to_owned(), window, cx);
        };
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.))
            .px(px(16.))
            .child(
                div()
                    .id("actions-login-message")
                    .role(Role::Label)
                    .aria_label(text.clone())
                    .w_full()
                    .text_center()
                    .text_color(hsla(fg).opacity(0.6))
                    .child(text),
            )
            .child(
                text_button("actions-login", rust_i18n::t!("github_actions.login").into_owned(), fg, bg)
                    .on_click(cx.listener(move |this, _, window, cx| login(this, window, cx)))
                    .on_a11y_press(cx.entity().downgrade(), login),
            )
    }

    /// 排出树里看得见的各行。
    fn walk_actions(&self, walk: &mut Walk, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) {
        let page = &self.github_actions;
        let branch = self.actions_branch();
        let detail = branch.clone().unwrap_or_else(|| rust_i18n::t!("github_actions.no_branch").into_owned());
        self.section_row(walk, Node::Branch, "github_actions.current_branch", Some(detail), fg, bg, cx);
        let header = walk.take();
        if page.expanded.contains(&Node::Branch) {
            match branch {
                Some(branch) => {
                    let query = Query::BranchRuns(branch);
                    if let Some(Data::Runs(runs)) = self.query_rows(walk, &query, 1, fg) {
                        for run in runs {
                            self.walk_run(walk, run, false, 1, fg, bg, cx);
                        }
                    }
                }
                None => self.note_row(walk, 1, rust_i18n::t!("github_actions.no_branch").into_owned(), fg),
            }
        }
        walk.end_section(header);
        self.section_row(walk, Node::Workflows, "github_actions.workflows", None, fg, bg, cx);
        let header = walk.take();
        if page.expanded.contains(&Node::Workflows)
            && let Some(Data::Workflows(workflows)) = self.query_rows(walk, &Query::Workflows, 1, fg)
        {
            for workflow in workflows {
                self.workflow_row(walk, workflow, fg, bg, cx);
                if page.expanded.contains(&Node::Workflow(workflow.id))
                    && let Some(Data::Runs(runs)) = self.query_rows(walk, &Query::WorkflowRuns(workflow.id), 2, fg)
                {
                    for run in runs {
                        self.walk_run(walk, run, true, 2, fg, bg, cx);
                    }
                }
            }
        }
        walk.end_section(header);
        self.section_row(walk, Node::Settings, "github_actions.settings", None, fg, bg, cx);
        let header = walk.take();
        if page.expanded.contains(&Node::Settings) {
            self.walk_settings(walk, Scope::Repo, 1, fg, bg, cx);
            self.walk_settings(walk, Scope::Org, 1, fg, bg, cx);
            self.group_row(walk, Node::Environments, 1, rust_i18n::t!("github_actions.environments"), None, fg, bg, cx);
            if page.expanded.contains(&Node::Environments)
                && let Some(Data::Names(envs)) = self.query_rows(walk, &Query::Environments, 2, fg)
            {
                for env in envs {
                    let node = Node::Environment(env.clone());
                    self.group_row(walk, node.clone(), 2, Cow::Owned(env.clone()), None, fg, bg, cx);
                    if page.expanded.contains(&node) {
                        self.walk_settings(walk, Scope::Env(env.clone()), 3, fg, bg, cx);
                    }
                }
            }
        }
        walk.end_section(header);
    }

    /// 一份数据的状态：在读、出错、是空的时排一行说明；读好了交回数据。
    fn query_rows<'a>(&'a self, walk: &mut Walk, query: &Query, depth: usize, fg: Rgb) -> Option<&'a Data> {
        walk.shown.insert(query.clone());
        let empty = match self.github_actions.data.get(query) {
            None => Some(rust_i18n::t!("github_actions.loading")),
            Some(Err(err)) => {
                self.note_row(walk, depth, err.clone(), fg);
                return None;
            }
            Some(Ok(data)) => match data {
                Data::Runs(runs) if runs.is_empty() => Some(rust_i18n::t!("github_actions.no_runs")),
                Data::Workflows(workflows) if workflows.is_empty() => {
                    Some(rust_i18n::t!("github_actions.no_workflows"))
                }
                Data::Jobs(jobs) if jobs.is_empty() => Some(rust_i18n::t!("github_actions.no_jobs")),
                Data::Names(names) if names.is_empty() => Some(rust_i18n::t!("github_actions.none")),
                Data::Variables(variables) if variables.is_empty() => Some(rust_i18n::t!("github_actions.none")),
                data => return Some(data),
            },
        };
        if let Some(text) = empty {
            self.note_row(walk, depth, text.into_owned(), fg);
        }
        None
    }

    /// 一次运行，展开后是它最近这次尝试的 job，重跑过的另有之前的尝试。`in_workflow` 为真时排在
    /// 工作流下面，旁边写分支；否则写工作流。
    #[allow(clippy::too_many_arguments)]
    fn walk_run(
        &self,
        walk: &mut Walk,
        run: &Run,
        in_workflow: bool,
        depth: usize,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) {
        let node = Node::Run(run.id);
        let expanded = self.github_actions.expanded.contains(&node);
        let state = run.state();
        let detail = if in_workflow { &run.head_branch } else { &run.workflow_name };
        let title = format!("#{} {}", run.number, run.display_title);
        let ix = walk.ix();
        let mut buttons = vec![{
            let url = run.url.clone();
            button(GLOBE_ICON, rust_i18n::t!("github_actions.open_in_browser"), move |_, _, cx| cx.open_url(&url))
        }];
        let id = run.id;
        if state.running() {
            buttons.push(button(TERMINAL_ICON, rust_i18n::t!("github_actions.watch"), move |this, window, cx| {
                this.run_gh_in_terminal(gh::watch_command(id), window, cx);
            }));
            buttons.push(button(BAN_ICON, rust_i18n::t!("github_actions.cancel"), move |this, _, cx| {
                this.run_gh(move |gh| gh.cancel(id), |this, _, cx| this.refetch_runs(cx), cx);
            }));
        } else {
            buttons.push(button(SYNC_ICON, rust_i18n::t!("github_actions.rerun"), move |this, _, cx| {
                this.run_gh(move |gh| gh.rerun(id), |this, _, cx| this.refetch_runs(cx), cx);
            }));
        }
        let row = self
            .actions_row(format!("run-{}", run.id), ix, depth, fg, bg, cx)
            .aria_expanded(expanded)
            .aria_label(format!("{title}, {detail}, {}", state_label(state)))
            .child(chevron(expanded, fg))
            .child(state_icon(state, fg))
            .child(div().flex_initial().min_w_0().truncate().child(title))
            .child(div().flex_1().min_w_0().truncate().text_color(hsla(fg).opacity(0.5)).child(detail.clone()))
            .children(self.actions_row_buttons(ix, buttons, fg, bg, cx))
            .on_press_down(cx, move |this, _, cx| this.toggle_actions_node(node.clone(), cx));
        walk.rows.push(row.into_any_element());
        if !expanded {
            return;
        }
        self.walk_jobs(walk, run.id, run.attempt, depth + 1, fg, bg, cx);
        if run.attempt <= 1 {
            return;
        }
        let attempts = Node::Attempts(run.id);
        self.group_row(
            walk,
            attempts.clone(),
            depth + 1,
            rust_i18n::t!("github_actions.previous_attempts"),
            None,
            fg,
            bg,
            cx,
        );
        if !self.github_actions.expanded.contains(&attempts) {
            return;
        }
        for attempt in (1..run.attempt).rev() {
            let node = Node::Attempt(run.id, attempt);
            let text = rust_i18n::t!("github_actions.attempt", n = attempt);
            let url = format!("{}/attempts/{attempt}", run.url);
            let open =
                button(GLOBE_ICON, rust_i18n::t!("github_actions.open_in_browser"), move |_, _, cx| cx.open_url(&url));
            self.group_row(walk, node.clone(), depth + 2, text, Some(open), fg, bg, cx);
            if self.github_actions.expanded.contains(&node) {
                self.walk_jobs(walk, run.id, attempt, depth + 3, fg, bg, cx);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_jobs(
        &self,
        walk: &mut Walk,
        run: u64,
        attempt: u32,
        depth: usize,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) {
        let Some(Data::Jobs(jobs)) = self.query_rows(walk, &Query::Jobs(run, attempt), depth, fg) else {
            return;
        };
        for job in jobs {
            self.walk_job(walk, job, depth, fg, bg, cx);
        }
    }

    /// 一个 job，展开后是它的 step；点 step 在浏览器里打开 job 页上那一步的日志。
    fn walk_job(&self, walk: &mut Walk, job: &Job, depth: usize, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) {
        let node = Node::Job(job.id);
        let expanded = self.github_actions.expanded.contains(&node);
        let state = job.state();
        let ix = walk.ix();
        let id = job.id;
        let mut buttons = Vec::new();
        // 日志要等 job 跑完才拿得到。
        if !state.running() {
            buttons.push(button(TERMINAL_ICON, rust_i18n::t!("github_actions.logs"), move |this, window, cx| {
                this.run_gh_in_terminal(gh::logs_command(id), window, cx);
            }));
        }
        let url = job.url.clone();
        buttons.push(button(GLOBE_ICON, rust_i18n::t!("github_actions.open_in_browser"), move |_, _, cx| {
            cx.open_url(&url)
        }));
        let row = self
            .actions_row(format!("job-{}", job.id), ix, depth, fg, bg, cx)
            .aria_expanded(expanded)
            .aria_label(format!("{}, {}", job.name, state_label(state)))
            .child(chevron(expanded, fg))
            .child(state_icon(state, fg))
            .child(div().flex_1().min_w_0().truncate().child(job.name.clone()))
            .children(self.actions_row_buttons(ix, buttons, fg, bg, cx))
            .on_press_down(cx, move |this, _, cx| this.toggle_actions_node(node.clone(), cx));
        walk.rows.push(row.into_any_element());
        if !expanded {
            return;
        }
        for (si, step) in job.steps.iter().enumerate() {
            let ix = walk.ix();
            let state = step.state();
            let url = gh::step_url(job, si);
            let open = {
                let url = url.clone();
                button(GLOBE_ICON, rust_i18n::t!("github_actions.step_logs"), move |_, _, cx| cx.open_url(&url))
            };
            let row = self
                .actions_row(format!("step-{}-{si}", job.id), ix, depth + 1, fg, bg, cx)
                .aria_label(format!("{}, {}", step.name, state_label(state)))
                .child(div().flex_none().w(px(12.)))
                .child(state_icon(state, fg))
                .child(div().flex_1().min_w_0().truncate().child(step.name.clone()))
                .children(self.actions_row_buttons(ix, vec![open], fg, bg, cx))
                .on_press_down(cx, move |_, _, cx| cx.open_url(&url));
            walk.rows.push(row.into_any_element());
        }
    }

    fn workflow_row(&self, walk: &mut Walk, workflow: &Workflow, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) {
        let node = Node::Workflow(workflow.id);
        let expanded = self.github_actions.expanded.contains(&node);
        let active = workflow.state == "active";
        let pinned = self.workspace().pinned_workflows.iter().any(|pin| pin.id == workflow.id);
        let ix = walk.ix();
        let mut buttons = Vec::new();
        let id = workflow.id;
        if active && workflow.dispatch {
            let branch = self.actions_branch();
            buttons.push(button(PLAY_ICON, rust_i18n::t!("github_actions.trigger"), move |this, window, cx| {
                this.run_gh_in_terminal(gh::trigger_command(id, branch.as_deref()), window, cx);
            }));
        }
        let file = self.actions_dir(cx).join(&workflow.path);
        buttons.push(button(OPEN_FILE_ICON, rust_i18n::t!("github_actions.open_workflow"), move |this, _, cx| {
            this.open_preview(&file, true, cx);
        }));
        let pin = {
            let workflow = workflow.clone();
            let key = if pinned { "github_actions.unpin" } else { "github_actions.pin" };
            let mut pin = button(PIN_ICON, rust_i18n::t!(key), move |this, _, cx| this.toggle_pin(&workflow, cx));
            pin.toggled = Some(pinned);
            pin
        };
        buttons.push(pin);
        let mut label = workflow.name.clone();
        if !active {
            label.push_str(&format!(" ({})", rust_i18n::t!("github_actions.disabled")));
        }
        let row = self
            .actions_row(format!("workflow-{}", workflow.id), ix, 1, fg, bg, cx)
            .aria_expanded(expanded)
            .aria_label(label.clone())
            .child(chevron(expanded, fg))
            .child(
                svg().flex_none().path(ACTIONS_WORKFLOW_ICON).size(px(13.)).text_color(hsla(fg).opacity(if active {
                    0.6
                } else {
                    0.3
                })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when(!active, |name| name.text_color(hsla(fg).opacity(0.5)))
                    .child(label),
            )
            .children(self.actions_row_buttons(ix, buttons, fg, bg, cx))
            .on_press_down(cx, move |this, _, cx| this.toggle_actions_node(node.clone(), cx));
        walk.rows.push(row.into_any_element());
    }

    /// `scope` 那一级的 secret 和 variable 两组；组织那一级只能看、复制，改要到浏览器里组织的设置页去。
    #[allow(clippy::too_many_arguments)]
    fn walk_settings(&self, walk: &mut Walk, scope: Scope, depth: usize, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) {
        for secret in [true, false] {
            let (node, query) = if secret {
                (Node::Secrets(scope.clone()), Query::Secrets(scope.clone()))
            } else {
                (Node::Variables(scope.clone()), Query::Variables(scope.clone()))
            };
            let label = match (&scope, secret) {
                (Scope::Org, true) => rust_i18n::t!("github_actions.org_secrets"),
                (Scope::Org, false) => rust_i18n::t!("github_actions.org_variables"),
                (_, true) => rust_i18n::t!("github_actions.secrets"),
                (_, false) => rust_i18n::t!("github_actions.variables"),
            };
            let add = if scope == Scope::Org {
                button(GLOBE_ICON, rust_i18n::t!("github_actions.manage_in_browser"), move |this, _, cx| {
                    this.run_gh(
                        move |gh| gh.org_settings_url(secret),
                        |_, url, cx| {
                            if let Some(url) = url {
                                cx.open_url(&url)
                            }
                        },
                        cx,
                    )
                })
            } else {
                let scope = scope.clone();
                let key = if secret { "github_actions.add_secret" } else { "github_actions.add_variable" };
                button(PLUS_ICON, rust_i18n::t!(key), move |this, window, cx| {
                    this.start_adding(secret, scope.clone(), window, cx)
                })
            };
            self.group_row(walk, node.clone(), depth, label, Some(add), fg, bg, cx);
            if !self.github_actions.expanded.contains(&node) {
                continue;
            }
            if let Some(adding) = &self.github_actions.adding
                && adding.secret == secret
                && adding.scope == scope
            {
                let label = if secret {
                    rust_i18n::t!("github_actions.add_secret")
                } else {
                    rust_i18n::t!("github_actions.add_variable")
                };
                let row = self
                    .actions_row("adding".to_owned(), walk.ix(), depth + 1, fg, bg, cx)
                    .aria_label(label.into_owned())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(div().flex_1().min_w_0().child(adding.edit.render(px(ROW_HEIGHT - 4.), fg, bg)));
                walk.rows.push(row.into_any_element());
            }
            match self.query_rows(walk, &query, depth + 1, fg) {
                Some(Data::Names(names)) => {
                    for name in names {
                        self.setting_row(walk, secret, &scope, name, None, depth + 1, fg, bg, cx);
                    }
                }
                Some(Data::Variables(variables)) => {
                    for variable in variables {
                        self.setting_row(
                            walk,
                            secret,
                            &scope,
                            &variable.name,
                            Some(&variable.value),
                            depth + 1,
                            fg,
                            bg,
                            cx,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    /// 一个 secret 或 variable（有 `value` 的）：复制名字和值，改值，删掉。
    #[allow(clippy::too_many_arguments)]
    fn setting_row(
        &self,
        walk: &mut Walk,
        secret: bool,
        scope: &Scope,
        name: &str,
        value: Option<&str>,
        depth: usize,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) {
        let ix = walk.ix();
        let copy = |key: &'static str, text: String| {
            button(COPY_ICON, rust_i18n::t!(key), move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
            })
        };
        let mut buttons = vec![copy("github_actions.copy_name", name.to_owned())];
        if let Some(value) = value {
            buttons.push(copy("github_actions.copy_value", value.to_owned()));
        }
        if *scope != Scope::Org {
            let (scope, name) = (scope.clone(), name.to_owned());
            buttons.push(button(PENCIL_ICON, rust_i18n::t!("github_actions.update"), {
                let (scope, name) = (scope.clone(), name.clone());
                move |this, window, cx| this.set_in_terminal(secret, scope.clone(), &name, window, cx)
            }));
            buttons.push(button(TRASH_ICON, rust_i18n::t!("github_actions.delete"), move |this, window, cx| {
                this.delete_setting(secret, scope.clone(), name.clone(), window, cx);
            }));
        }
        let label = match value {
            Some(value) => format!("{name} = {value}"),
            None => name.to_owned(),
        };
        let row = self
            .actions_row(format!("setting-{secret}-{scope:?}-{name}"), ix, depth, fg, bg, cx)
            .aria_label(label)
            .child(div().flex_none().w(px(12.)))
            .child(div().flex_initial().min_w_0().truncate().child(name.to_owned()))
            .children(value.map(|value| {
                div().flex_1().min_w_0().truncate().text_color(hsla(fg).opacity(0.5)).child(value.to_owned())
            }))
            .when(value.is_none(), |row| row.child(div().flex_1()))
            .children(self.actions_row_buttons(ix, buttons, fg, bg, cx));
        walk.rows.push(row.into_any_element());
    }

    /// 三段的标题行，`detail` 写在标题后面。
    #[allow(clippy::too_many_arguments)]
    fn section_row(
        &self,
        walk: &mut Walk,
        node: Node,
        key: &'static str,
        detail: Option<String>,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) {
        let expanded = self.github_actions.expanded.contains(&node);
        let label = rust_i18n::t!(key).into_owned();
        let aria = match &detail {
            Some(detail) => format!("{label} {detail}"),
            None => label.clone(),
        };
        let row = self
            .actions_row(format!("{node:?}"), walk.ix(), 0, fg, bg, cx)
            .aria_expanded(expanded)
            .aria_label(aria)
            .child(chevron(expanded, fg))
            .child(div().flex_none().text_size(px(11.)).font_weight(gpui::FontWeight::SEMIBOLD).child(label))
            .child(div().flex_1().min_w_0().truncate().text_color(hsla(fg).opacity(0.5)).children(detail))
            .on_press_down(cx, move |this, _, cx| this.toggle_actions_node(node.clone(), cx));
        walk.rows.push(row.into_any_element());
    }

    /// 能展开的一组：之前的尝试、某次尝试、secret 和 variable 的分组、部署环境；`action` 是行尾的按钮。
    #[allow(clippy::too_many_arguments)]
    fn group_row(
        &self,
        walk: &mut Walk,
        node: Node,
        depth: usize,
        label: Cow<'static, str>,
        action: Option<RowButton>,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) {
        let expanded = self.github_actions.expanded.contains(&node);
        let ix = walk.ix();
        let row = self
            .actions_row(format!("{node:?}"), ix, depth, fg, bg, cx)
            .aria_expanded(expanded)
            .aria_label(label.clone().into_owned())
            .child(chevron(expanded, fg))
            .child(div().flex_1().min_w_0().truncate().child(label.into_owned()))
            .children(self.actions_row_buttons(ix, action.into_iter().collect(), fg, bg, cx))
            .on_press_down(cx, move |this, _, cx| this.toggle_actions_node(node.clone(), cx));
        walk.rows.push(row.into_any_element());
    }

    /// 在读、出错、没有内容时的一行说明，也报给辅助工具。
    fn note_row(&self, walk: &mut Walk, depth: usize, text: String, fg: Rgb) {
        let row = div()
            .id(("actions-note", walk.ix()))
            .role(Role::Label)
            .aria_label(text.clone())
            .flex_none()
            .min_h(px(ROW_HEIGHT))
            .w_full()
            .pl(px(8. + (depth as f32 + 1.) * INDENT))
            .pr(px(8.))
            .flex()
            .items_center()
            .italic()
            .text_color(hsla(fg).opacity(0.45))
            // gh 的错可能很长，折行显示完整。
            .child(div().flex_1().min_w_0().child(text));
        walk.rows.push(row.into_any_element());
    }

    /// 一行的外框：定高、按深度缩进，鼠标移上去时底色变亮，行尾的按钮露出来。`key` 按这一行显示的东西取
    /// （运行、job 的 id，段名），不按行号：辅助工具按元素 id 认节点，读到数据、上面多出几行时行号会变，
    /// 用行号的话 VoiceOver 的焦点会跳、按下会按到别的行上。悬停只管这一帧，仍按行号 `ix` 记。
    #[allow(clippy::too_many_arguments)]
    fn actions_row(
        &self,
        key: String,
        ix: usize,
        depth: usize,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(SharedString::from(key))
            .role(Role::TreeItem)
            .aria_level(depth + 1)
            .flex_none()
            .h(px(ROW_HEIGHT))
            .w_full()
            .pl(px(8. + depth as f32 * INDENT))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .overflow_hidden()
            .text_color(hsla(fg))
            .hover(|row| row.bg(hsla(bg.mix(fg, 0.06))))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let page = &mut this.github_actions;
                if *hovered {
                    page.hovered = Some(ix);
                } else if page.hovered == Some(ix) {
                    page.hovered = None;
                }
                cx.notify();
            }))
    }

    /// 第 `ix` 行行尾的按钮，鼠标在这行或辅助工具在读时才画；固定着的工作流不悬停也露出图钉，
    /// 一眼看出哪些固定了。
    fn actions_row_buttons(
        &self,
        ix: usize,
        buttons: Vec<RowButton>,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let page = &self.github_actions;
        row_buttons(buttons, page.hovered == Some(ix) || page.a11y, true, fg, bg, cx)
    }

    /// 状态栏左边固定的工作流：最近一次运行的状态和工作流名，点了在浏览器里打开那次运行。
    pub(super) fn render_pinned_workflows(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let page = &self.github_actions;
        self.workspace()
            .pinned_workflows
            .iter()
            .map(|pin| {
                let latest = match page.data.get(&Query::WorkflowRuns(pin.id)) {
                    Some(Ok(Data::Runs(runs))) => runs.first(),
                    _ => None,
                };
                let state = latest.map(Run::state);
                let description = match latest {
                    Some(run) => format!("#{} {}, {}", run.number, run.display_title, state_label(run.state())),
                    None => rust_i18n::t!("github_actions.no_runs").into_owned(),
                };
                let url = latest.map(|run| run.url.clone());
                div()
                    .id(SharedString::from(format!("status-workflow-{}", pin.id)))
                    .role(Role::Button)
                    .aria_label(pin.name.clone())
                    .aria_description(description.clone())
                    .flex_none()
                    .h(px(status_bar::STATUS_BAR_HEIGHT - 6.))
                    .px(px(6.))
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .hover(|item| item.bg(hsla(bg.mix(fg, 0.10))))
                    .tooltip(tooltip(description, None, fg, bg))
                    .children(state.map(|state| state_icon(state, fg)))
                    .child(pin.name.clone())
                    .on_press_down(cx, move |_, _, cx| {
                        if let Some(url) = &url {
                            cx.open_url(url);
                        }
                    })
                    .into_any_element()
            })
            .collect()
    }
}

/// gh 报的错换成页上显示的话：没登录、仓库不属于组织这两种有专门的说明。
fn gh_error(err: String) -> String {
    match err.as_str() {
        gh::LOGIN => rust_i18n::t!("github_actions.login_needed").into_owned(),
        gh::NOT_ORG => rust_i18n::t!("github_actions.not_org").into_owned(),
        _ => err,
    }
}

/// 状态的图标，按状态上色：成功绿、失败红、在跑和排队黄，其余灰。
fn state_icon(state: State, fg: Rgb) -> gpui::Svg {
    let (icon, color) = match state {
        State::Success => (ACTIONS_SUCCESS_ICON, hsla(ADDED)),
        State::Failure => (ACTIONS_FAILURE_ICON, hsla(REMOVED)),
        State::Cancelled => (ACTIONS_CANCELLED_ICON, hsla(fg).opacity(0.5)),
        State::Skipped => (ACTIONS_SKIPPED_ICON, hsla(fg).opacity(0.5)),
        State::InProgress => (ACTIONS_INPROGRESS_ICON, hsla(MODIFIED)),
        State::Queued => (ACTIONS_QUEUED_ICON, hsla(MODIFIED)),
        State::Waiting | State::ActionRequired => (ACTIONS_WAITING_ICON, hsla(MODIFIED)),
        State::Pending => (ACTIONS_PENDING_ICON, hsla(fg).opacity(0.5)),
    };
    svg().flex_none().path(icon).size(px(13.)).text_color(color)
}

fn state_label(state: State) -> Cow<'static, str> {
    match state {
        State::Success => rust_i18n::t!("github_actions.state.success"),
        State::Failure => rust_i18n::t!("github_actions.state.failure"),
        State::Cancelled => rust_i18n::t!("github_actions.state.cancelled"),
        State::Skipped => rust_i18n::t!("github_actions.state.skipped"),
        State::InProgress => rust_i18n::t!("github_actions.state.in_progress"),
        State::Queued => rust_i18n::t!("github_actions.state.queued"),
        State::Waiting | State::ActionRequired => rust_i18n::t!("github_actions.state.waiting"),
        State::Pending => rust_i18n::t!("github_actions.state.pending"),
    }
}
