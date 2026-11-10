//! 新建工作树的对话框：挑项目（侧栏里知道主工作树的仓库，`RepoProject`）和它所在的主机，填工作区的名字，挑新工作区
//! 第一个终端里起哪家 agent；「高级」里改新分支的名字、挑从哪个分支拉出。工作树建在主工作树下
//! `runode_git::WORKTREES_DIR` 里，建好开成工作区，归在这个项目下。名字、agent 和高级里的都可以不填：分支名按
//! 工作区的名字取，名字也没填时取一个没被占的；不挑 agent 是空白终端。打开「创建更多」时建好一个后对话框留着，
//! 清空名字接着建下一个。

use std::{cell::Cell, collections::HashSet, path::PathBuf};

use gpui::{
    Action, AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, FontWeight, Hsla, KeyDownEvent, MouseButton,
    Role, Stateful, Subscription, Window, div, img, prelude::*, px, svg,
};
use runode_agent::models::LAUNCHERS;
use runode_git::Branch;

use super::{
    NewWorkspace, TITLEBAR_HEIGHT, WindowView,
    agents::logo::{agent_logo, logo_path},
    new_tab::Installed,
    sidebar::RepoProject,
};
use crate::{
    host_client::hosts::HostId,
    project_host::ProjectHost,
    remote_hosts,
    ui::{
        a11y::{Press as _, PressDown as _, StaticText as _},
        button::{Button, ButtonSize, ButtonVariant},
        dialog::{Dialog, backdrop, is_dismiss_key},
        font_size::{FontSize, scaled},
        icons::{
            CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, CLOSE_ICON, FOLDER_ICON, FOLDER_PLUS_ICON, MONITOR_ICON,
            SERVER_ICON, TERMINAL_ICON,
        },
        input::{Input, InputEvent, InputState},
        menu::{MenuAnchors, MenuHost as _, MenuItem},
        select::SelectTrigger,
        switch::Switch,
        theme::ActiveTheme as _,
        tooltip::tooltip,
    },
};

/// 名字也没填时取的分支名的前缀，后面接序号。
const AUTO_BRANCH: &str = "worktree";
/// 下拉框的高度。
const SELECT_HEIGHT: f32 = 32.;

thread_local! {
    /// 上次建工作树时挑的 agent（`LAUNCHERS` 里第几个），下次打开对话框先挑上它；空白终端为空。
    static LAST_AGENT: Cell<Option<usize>> = const { Cell::new(None) };
}

/// 项目菜单的一项。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct PickWorktreeProject {
    host: HostId,
    dir: PathBuf,
}

/// 主机菜单的一项：换到这台主机上的第一个项目。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct PickWorktreeHost {
    host: HostId,
}

/// agent 菜单的一项：`LAUNCHERS` 里第几个，为空时是空白终端。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct PickWorktreeAgent {
    agent: Option<usize>,
}

/// 起点菜单的一项：分支名，为空时是当前 HEAD。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub(super) struct PickWorktreeBase {
    branch: Option<String>,
}

/// 点了弹出菜单的几个框。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Combo {
    Project,
    Host,
    Agent,
    Base,
}

impl Combo {
    /// 框的元素 id，也是 `MenuAnchors` 里记它位置的名字。
    fn id(self) -> &'static str {
        match self {
            Self::Project => "new-worktree-project",
            Self::Host => "new-worktree-host",
            Self::Agent => "new-worktree-agent",
            Self::Base => "new-worktree-base",
        }
    }
}

pub(super) struct NewWorktreeDialog {
    focus: FocusHandle,
    /// 挑着的项目：所在的主机和 `Workspace::project_dir`。
    host: HostId,
    project: PathBuf,
    name: Entity<InputState>,
    branch: Entity<InputState>,
    /// 挑着的 agent，`LAUNCHERS` 里第几个；为空时是空白终端。
    agent: Option<usize>,
    /// 那台主机上装着的 agent，问到了才有。
    agents: Option<(HostId, Vec<Installed>)>,
    /// 那个项目的分支，后台读到了才有。
    branches: Option<(HostId, PathBuf, Vec<Branch>)>,
    /// 新分支从哪个分支拉出；为空时是当前 HEAD。
    base: Option<String>,
    advanced: bool,
    more: bool,
    /// 正在后台建，建完之前不再建。
    creating: bool,
    error: Option<String>,
    anchors: MenuAnchors,
    /// 开着的菜单是哪个框的。
    menu_for: Option<Combo>,
    _events: [Subscription; 2],
}

impl NewWorktreeDialog {
    fn name_text(&self, cx: &App) -> String {
        self.name.read(cx).query().trim().to_owned()
    }

    /// 挑着的项目的分支；还没读到时为空。
    fn branches(&self) -> Option<&[Branch]> {
        match &self.branches {
            Some((host, project, branches)) if *host == self.host && *project == self.project => Some(branches),
            _ => None,
        }
    }

    /// 挑着的项目所在主机上装着的 agent；还没问到时为空。
    fn installed(&self) -> Option<&[Installed]> {
        match &self.agents {
            Some((host, agents)) if *host == self.host => Some(agents),
            _ => None,
        }
    }

    /// 不填分支名时用的分支名：按工作区的名字取，被占了时后面加序号；名字也没填或取不出时是没被占的
    /// `AUTO_BRANCH-序号`。`taken` 是已经有了的分支和工作树目录。
    fn default_branch(&self, taken: &HashSet<String>, cx: &App) -> String {
        match branch_from_name(&self.name_text(cx)) {
            Some(base) => unused_branch(&base, taken),
            None => free_branch(taken),
        }
    }

    /// 要建的分支：填了就是填的，否则是 `default_branch`。
    fn branch_name(&self, taken: &HashSet<String>, cx: &App) -> String {
        let typed = self.branch.read(cx).query().trim().to_owned();
        if typed.is_empty() { self.default_branch(taken, cx) } else { typed }
    }
}

/// 新工作树的目录名：分支名里的 `/` 换成 `-`，一个分支只占 `runode_git::WORKTREES_DIR` 下的一级。
pub(super) fn worktree_dir_name(branch: &str) -> String {
    branch.replace('/', "-")
}

/// 工作区名字 `name` 对应的分支名：空白换成 `-` 后能当分支名就用它，不然取其中的 ASCII 字母和数字转成小写，其余的
/// 连成一个 `-`；什么都不剩时为空。
fn branch_from_name(name: &str) -> Option<String> {
    let dashed = name.split_whitespace().collect::<Vec<_>>().join("-");
    if dashed.is_empty() {
        return None;
    }
    if runode_git::valid_branch_name(&dashed) {
        return Some(dashed);
    }
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    (!slug.is_empty()).then(|| slug.to_owned())
}

/// 分支 `branch` 没被占：它和它的工作树目录名都不在 `taken` 里。
fn branch_free(branch: &str, taken: &HashSet<String>) -> bool {
    !taken.contains(branch) && !taken.contains(&worktree_dir_name(branch))
}

/// `base` 没被占时就是它，被占了时是 `base-2`、`base-3`……里第一个没被占的。
fn unused_branch(base: &str, taken: &HashSet<String>) -> String {
    std::iter::once(base.to_owned())
        .chain((2..).map(|n| format!("{base}-{n}")))
        .find(|name| branch_free(name, taken))
        .unwrap_or_default()
}

/// `AUTO_BRANCH-1`、`AUTO_BRANCH-2`……里第一个没被占的。
fn free_branch(taken: &HashSet<String>) -> String {
    (1..).map(|n| format!("{AUTO_BRANCH}-{n}")).find(|name| branch_free(name, taken)).unwrap_or_default()
}

impl WindowView {
    /// 打开新建工作树的对话框，挑上 `project`（主机和 `Workspace::project_dir`）；没给时挑当前工作区的项目，它不是
    /// 仓库时挑第一个能建的。一个能建的项目都没有时不开。已经开着时换成这个项目，焦点给名字框。
    pub(super) fn open_new_worktree(
        &mut self,
        project: Option<(HostId, PathBuf)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.leave_todo_page(window, cx) {
            return;
        }
        let projects = self.repo_projects();
        let current = (self.workspace().host.clone(), self.workspace().project_dir().to_path_buf());
        let picked = [project, Some(current)]
            .into_iter()
            .flatten()
            .find(|(host, dir)| projects.iter().any(|p| p.host == *host && p.dir == *dir))
            .or_else(|| projects.first().map(|p| (p.host.clone(), p.dir.clone())));
        let Some((host, dir)) = picked else {
            return;
        };
        if self.new_worktree.is_none() {
            let field = |label: &str, cx: &mut Context<Self>| {
                let label = rust_i18n::t!(label).into_owned();
                cx.new(|cx| InputState::new(String::new(), cx).with_label(label.clone()).with_placeholder(label))
            };
            let name = field("worktree.name", cx);
            let branch = field("worktree.branch", cx);
            let on_field =
                |this: &mut Self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>| match event {
                    InputEvent::Next => this.submit_new_worktree(window, cx),
                    InputEvent::Dismiss => this.close_new_worktree(window, cx),
                    InputEvent::Changed(_) => {
                        if let Some(dialog) = &mut this.new_worktree {
                            dialog.error = None;
                        }
                        this.refresh_branch_placeholder(cx);
                        cx.notify();
                    }
                    InputEvent::Previous => {}
                };
            let on_name =
                cx.subscribe_in(&name, window, move |this, _, event, window, cx| on_field(this, event, window, cx));
            let on_branch =
                cx.subscribe_in(&branch, window, move |this, _, event, window, cx| on_field(this, event, window, cx));
            self.new_worktree = Some(NewWorktreeDialog {
                focus: cx.focus_handle(),
                host,
                project: dir,
                name,
                branch,
                agent: LAST_AGENT.get(),
                agents: None,
                branches: None,
                base: None,
                advanced: false,
                more: false,
                creating: false,
                error: None,
                anchors: MenuAnchors::default(),
                menu_for: None,
                _events: [on_name, on_branch],
            });
            self.load_worktree_agents(cx);
            self.load_worktree_branches(cx);
        } else {
            self.set_worktree_project(host, dir, cx);
        }
        if let Some(dialog) = &self.new_worktree {
            window.focus(&dialog.name.focus_handle(cx), cx);
        }
        self.refresh_branch_placeholder(cx);
        cx.notify();
    }

    pub(super) fn close_new_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_worktree.take().is_some() {
            self.close_menu(window, cx);
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// 换项目：起点回到当前 HEAD，读它的分支；换了主机时再问那台主机装着哪些 agent。
    fn set_worktree_project(&mut self, host: HostId, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(dialog) = &mut self.new_worktree else {
            return;
        };
        if dialog.host == host && dialog.project == dir {
            return;
        }
        let host_changed = dialog.host != host;
        dialog.host = host;
        dialog.project = dir;
        dialog.base = None;
        dialog.error = None;
        if host_changed {
            self.load_worktree_agents(cx);
        }
        self.load_worktree_branches(cx);
        self.refresh_branch_placeholder(cx);
        cx.notify();
    }

    /// 挑着的项目不在侧栏里了（比如它最后一个工作区关了）时换成同一台主机上的另一个项目，没有就换别的主机上的；
    /// 一个能建的项目都没有了时关掉对话框。
    pub(super) fn keep_new_worktree_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.new_worktree else {
            return;
        };
        let projects = self.repo_projects();
        if projects.iter().any(|p| p.host == dialog.host && p.dir == dialog.project) {
            return;
        }
        match projects.iter().find(|p| p.host == dialog.host).or(projects.first()) {
            Some(project) => self.set_worktree_project(project.host.clone(), project.dir.clone(), cx),
            None => self.close_new_worktree(window, cx),
        }
    }

    /// 挑着的项目已经有了的分支和工作树目录名，取默认分支名时避开它们。
    fn worktree_taken(&self) -> HashSet<String> {
        let Some(dialog) = &self.new_worktree else {
            return HashSet::new();
        };
        let branches = dialog.branches().unwrap_or_default().iter().filter(|b| !b.remote).map(|b| b.name.clone());
        let dirs = self
            .workspaces
            .iter()
            .filter(|w| w.host == dialog.host && w.project_dir() == dialog.project)
            .filter_map(|w| w.dir.file_name().map(|name| name.to_string_lossy().into_owned()));
        branches.chain(dirs).collect()
    }

    /// 分支名框空着时显示要用的默认分支名。
    fn refresh_branch_placeholder(&mut self, cx: &mut Context<Self>) {
        let taken = self.worktree_taken();
        let Some(dialog) = &self.new_worktree else {
            return;
        };
        let default = dialog.default_branch(&taken, cx);
        dialog.branch.update(cx, |branch, _| branch.set_placeholder(default));
    }

    /// 在后台问挑着的项目所在的主机装着哪些 agent，问到时对话框还在那台主机上才记下；挑着的 agent 那台主机上
    /// 没装时换成空白终端。
    fn load_worktree_agents(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = &self.new_worktree else {
            return;
        };
        let (host, job) = self.ask_installed_agents(dialog.host.clone(), cx);
        cx.spawn(async move |this, cx| {
            let agents = job.await;
            this.update(cx, |this, cx| {
                if let Some(dialog) = this.new_worktree.as_mut().filter(|dialog| dialog.host == host) {
                    if dialog.agent.is_some_and(|agent| !agents.iter().any(|a| a.agent == agent)) {
                        dialog.agent = None;
                    }
                    dialog.agents = Some((host, agents));
                    this.refresh_worktree_menu(cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 在后台读挑着的项目的分支，读到时对话框还挑着它才记下。
    fn load_worktree_branches(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = &self.new_worktree else {
            return;
        };
        let (host, project) = (dialog.host.clone(), dialog.project.clone());
        let Some(main_worktree) =
            self.repo_projects().into_iter().find(|p| p.host == host && p.dir == project).map(|p| p.main_worktree)
        else {
            return;
        };
        let repo = ProjectHost::of(&host).repo(runode_git::Repo::new(main_worktree));
        let job = cx.background_spawn(async move { repo.branches() });
        cx.spawn(async move |this, cx| {
            let branches = job.await;
            this.update(cx, |this, cx| {
                if let Some(dialog) =
                    this.new_worktree.as_mut().filter(|dialog| dialog.host == host && dialog.project == project)
                {
                    dialog.branches = Some((host, project, branches));
                    this.refresh_branch_placeholder(cx);
                    this.refresh_worktree_menu(cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn pick_worktree_project(&mut self, action: &PickWorktreeProject, _: &mut Window, cx: &mut Context<Self>) {
        self.set_worktree_project(action.host.clone(), action.dir.clone(), cx);
    }

    fn pick_worktree_host(&mut self, action: &PickWorktreeHost, _: &mut Window, cx: &mut Context<Self>) {
        if self.new_worktree.as_ref().is_some_and(|dialog| dialog.host == action.host) {
            return;
        }
        if let Some(project) = self.repo_projects().into_iter().find(|p| p.host == action.host) {
            self.set_worktree_project(project.host, project.dir, cx);
        }
    }

    fn pick_worktree_agent(&mut self, action: &PickWorktreeAgent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.new_worktree {
            dialog.agent = action.agent;
            cx.notify();
        }
    }

    fn pick_worktree_base(&mut self, action: &PickWorktreeBase, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.new_worktree {
            dialog.base.clone_from(&action.branch);
            dialog.error = None;
            cx.notify();
        }
    }

    /// 框 `combo` 的菜单开着。
    fn worktree_menu_open(&self, combo: Combo) -> bool {
        self.menu.is_some() && self.new_worktree.as_ref().is_some_and(|dialog| dialog.menu_for == Some(combo))
    }

    /// 在框 `combo` 下面弹出它的菜单，点了的那项派发到对话框上；已经开着时关掉。
    fn toggle_worktree_menu(&mut self, combo: Combo, window: &mut Window, cx: &mut Context<Self>) {
        if self.worktree_menu_open(combo) {
            self.close_menu(window, cx);
            return;
        }
        let items = self.worktree_menu_items(combo, cx);
        let Some(dialog) = &mut self.new_worktree else {
            return;
        };
        dialog.menu_for = Some(combo);
        let (position, target) = (dialog.anchors.below(combo.id(), window), dialog.focus.clone());
        self.open_context_menu(position, items, target, cx);
    }

    /// 开着的菜单照现在的清单重建：问到了 agent、读到了分支时用。
    fn refresh_worktree_menu(&mut self, cx: &mut Context<Self>) {
        let combo = self.new_worktree.as_ref().and_then(|dialog| dialog.menu_for);
        if let Some(combo) = combo.filter(|combo| self.worktree_menu_open(*combo)) {
            let items = self.worktree_menu_items(combo, cx);
            self.replace_menu_items(items, cx);
        }
    }

    fn worktree_menu_items(&self, combo: Combo, cx: &App) -> Vec<Option<MenuItem>> {
        let Some(dialog) = &self.new_worktree else {
            return Vec::new();
        };
        let loading = || vec![Some(MenuItem::label(rust_i18n::t!("worktree.loading").into_owned(), None, None))];
        match combo {
            Combo::Project => self
                .repo_projects()
                .into_iter()
                .filter(|project| project.host == dialog.host)
                .map(|project| {
                    let checked = project.dir == dialog.project;
                    let path = project.host.display_dir(&project.main_worktree);
                    let action = Box::new(PickWorktreeProject { host: project.host, dir: project.dir });
                    Some(MenuItem::checkbox(project.name, None, checked, action).with_tooltip(path))
                })
                .collect(),
            Combo::Host => {
                let mut hosts: Vec<HostId> = Vec::new();
                for project in self.repo_projects() {
                    if !hosts.contains(&project.host) {
                        hosts.push(project.host);
                    }
                }
                hosts
                    .into_iter()
                    .map(|host| {
                        let (label, icon) = (remote_hosts::display_name(&host, cx), host_icon(&host));
                        let checked = host == dialog.host;
                        Some(MenuItem::checkbox(label, Some(icon), checked, Box::new(PickWorktreeHost { host })))
                    })
                    .collect()
            }
            Combo::Agent => {
                let blank = rust_i18n::t!("worktree.blank_terminal").into_owned();
                let action = Box::new(PickWorktreeAgent { agent: None });
                let mut items =
                    vec![Some(MenuItem::checkbox(blank, Some(TERMINAL_ICON), dialog.agent.is_none(), action))];
                match dialog.installed() {
                    None => items.extend([None].into_iter().chain(loading())),
                    Some([]) => {}
                    Some(agents) => {
                        items.push(None);
                        items.extend(agents.iter().map(|installed| {
                            let label = LAUNCHERS[installed.agent].label.to_owned();
                            let icon = installed.kind().and_then(logo_path);
                            let checked = dialog.agent == Some(installed.agent);
                            let action = Box::new(PickWorktreeAgent { agent: Some(installed.agent) });
                            Some(MenuItem::checkbox(label, icon, checked, action))
                        }));
                    }
                }
                items
            }
            Combo::Base => {
                let Some(branches) = dialog.branches() else {
                    return loading();
                };
                let head = base_head_label(branches);
                let action = Box::new(PickWorktreeBase { branch: None });
                let mut items = vec![Some(MenuItem::checkbox(head, None, dialog.base.is_none(), action))];
                for remote in [false, true] {
                    let group: Vec<_> = branches
                        .iter()
                        .filter(|branch| branch.remote == remote && !branch.current)
                        .map(|branch| {
                            let checked = dialog.base.as_deref() == Some(branch.name.as_str());
                            let action = Box::new(PickWorktreeBase { branch: Some(branch.name.clone()) });
                            Some(MenuItem::checkbox(branch.name.clone(), None, checked, action))
                        })
                        .collect();
                    if !group.is_empty() {
                        items.push(None);
                        items.extend(group);
                    }
                }
                items
            }
        }
    }

    /// Esc 关掉；⌘回车（别的系统上 Ctrl+回车）建；Tab 在名字框和展开着的分支名框之间挪。
    fn new_worktree_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if is_dismiss_key(keystroke) {
            cx.stop_propagation();
            self.close_new_worktree(window, cx);
            return;
        }
        let m = &keystroke.modifiers;
        if keystroke.key == "enter" && m.secondary(cx) && !m.shift && !m.alt {
            cx.stop_propagation();
            self.submit_new_worktree(window, cx);
            return;
        }
        if keystroke.key == "tab" && !m.control && !m.alt && !m.platform {
            let Some(dialog) = &self.new_worktree else { return };
            let mut fields = vec![dialog.name.focus_handle(cx)];
            if dialog.advanced {
                fields.push(dialog.branch.focus_handle(cx));
            }
            let at = fields.iter().position(|field| field.is_focused(window));
            let next = match at {
                Some(at) if m.shift => (at + fields.len() - 1) % fields.len(),
                Some(at) => (at + 1) % fields.len(),
                None => 0,
            };
            cx.stop_propagation();
            window.focus(&fields[next], cx);
        }
    }

    /// 按对话框里填的建工作树：分支名不对时在对话框里说；建成了开成工作区，「创建更多」开着时清空名字留着对话框，
    /// 否则关掉；git 建不成时在对话框里说原因。
    fn submit_new_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keep_new_worktree_project(window, cx);
        let taken = self.worktree_taken();
        let projects = self.repo_projects();
        let Some(dialog) = &mut self.new_worktree else {
            return;
        };
        if dialog.creating {
            return;
        }
        let Some(project) = projects.into_iter().find(|p| p.host == dialog.host && p.dir == dialog.project) else {
            return;
        };
        let branch = dialog.branch_name(&taken, cx);
        if !runode_git::valid_branch_name(&branch) {
            dialog.error = Some(rust_i18n::t!("workspace.worktree_bad_name", name = branch.as_str()).into_owned());
            cx.notify();
            return;
        }
        let name = Some(dialog.name_text(cx)).filter(|name| !name.is_empty()).unwrap_or_else(|| branch.clone());
        let command = dialog.agent.map(|agent| {
            let installed = dialog.installed().and_then(|agents| agents.iter().find(|a| a.agent == agent));
            installed.map_or_else(|| super::new_tab::program(&LAUNCHERS[agent]).to_owned(), |a| a.command.clone())
        });
        LAST_AGENT.set(dialog.agent);
        dialog.creating = true;
        dialog.error = None;
        cx.notify();
        let RepoProject { host, dir: project, main_worktree, .. } = project;
        let worktree = main_worktree.join(runode_git::WORKTREES_DIR).join(worktree_dir_name(&branch));
        let repo = ProjectHost::of(&host).repo(runode_git::Repo::new(main_worktree));
        let base = dialog.base.clone();
        // 建的时候对话框可能关了又开了一个新的，建完只回到发起的这一个上。
        let token = dialog.name.entity_id();
        let job = cx.background_spawn({
            let (worktree, branch) = (worktree.clone(), branch.clone());
            async move { repo.add_worktree(&worktree, &branch, base.as_deref()) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let added = job.await;
            this.update_in(cx, |this, window, cx| {
                if let Err(err) = added {
                    if let Some(dialog) = this.new_worktree.as_mut().filter(|d| d.name.entity_id() == token) {
                        dialog.creating = false;
                        dialog.error = Some(crate::git_view::git_error_text(&err));
                        cx.notify();
                    }
                    return;
                }
                this.open_worktree(&host, project, worktree, name, command, window, cx);
                let Some(dialog) = this.new_worktree.as_mut().filter(|d| d.name.entity_id() == token) else {
                    return;
                };
                if !dialog.more {
                    this.close_new_worktree(window, cx);
                    return;
                }
                dialog.creating = false;
                for field in [&dialog.name, &dialog.branch] {
                    field.update(cx, |field, cx| field.set_query(String::new(), cx));
                }
                // 新分支记进已有的分支，下一个默认分支名避开它。
                if let Some((_, _, branches)) = &mut dialog.branches {
                    branches.push(Branch {
                        name: branch,
                        remote: false,
                        current: false,
                        upstream: None,
                        subject: String::new(),
                        date: String::new(),
                        time: None,
                    });
                }
                window.focus(&dialog.name.focus_handle(cx), cx);
                this.refresh_branch_placeholder(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 在新建的工作树 `worktree` 上开工作区，名字是 `name`，归在项目 `project` 下、排在它的最后，并切过去；
    /// `command` 给了就在第一个终端里跑它（起 agent）。
    #[allow(clippy::too_many_arguments)]
    fn open_worktree(
        &mut self,
        host: &HostId,
        project: PathBuf,
        worktree: PathBuf,
        name: String,
        command: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.spawn_terminal_on(host, Some(&worktree), None, window, cx) else {
            return;
        };
        if let Some(command) = command {
            view.update(cx, |view, cx| view.run_command(command, cx));
        }
        let ix = self
            .workspaces
            .iter()
            .rposition(|workspace| workspace.host == *host && workspace.project_dir() == project)
            .map_or(self.workspaces.len(), |last| last + 1);
        self.insert_workspace(ix, worktree, Some(name), view, window, cx);
        let workspace = &mut self.workspaces[self.active];
        workspace.common_git_dir = Some(project);
        let id = workspace.id;
        self.regroup_workspace(id);
        self.save(cx);
        cx.notify();
    }

    /// 点了弹出菜单的框：框里是挑着的值，菜单从框下面开出来；菜单开着时再按一下是关掉。
    fn worktree_select(
        &self,
        combo: Combo,
        label: String,
        value: String,
        leading: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> SelectTrigger {
        let theme = cx.theme();
        let hover_bg = theme.mix(0.06);
        let open = self.worktree_menu_open(combo);
        let anchors = self.new_worktree.as_ref().map(|dialog| dialog.anchors.clone()).unwrap_or_default();
        SelectTrigger::new(combo.id(), label, value)
            .when_some(leading, SelectTrigger::leading)
            .aria_expanded(open)
            .relative()
            .h(scaled(SELECT_HEIGHT))
            .px(px(10.))
            .gap(px(8.))
            .rounded(px(8.))
            .bg(theme.background)
            .border_1()
            .border_color(theme.input)
            .when(open, |select| select.bg(theme.mix(0.08)))
            .hover(move |select| select.bg(hover_bg))
            .child(anchors.track(combo.id()))
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                if this.worktree_menu_open(combo) {
                    cx.stop_propagation();
                    this.close_menu(window, cx);
                }
            }))
            .on_press_down(cx, move |this, window, cx| this.toggle_worktree_menu(combo, window, cx))
    }

    pub(super) fn render_new_worktree(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let dialog = self.new_worktree.as_ref()?;
        let projects = self.repo_projects();
        let project = projects.iter().find(|p| p.host == dialog.host && p.dir == dialog.project);
        let fg = cx.theme().foreground;
        let title = rust_i18n::t!("worktree.title").into_owned();
        let optional = rust_i18n::t!("worktree.optional").into_owned();
        // 一栏上面的说明：`optional` 的在后面淡淡地注明可不填，右边可以挂一个按钮。
        let caption = |key: &str, optional_mark: bool, extra: Option<AnyElement>| {
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .min_h(scaled(20.))
                .text_color(fg.opacity(0.7))
                .child(rust_i18n::t!(key).into_owned())
                .when(optional_mark, |row| row.child(div().text_color(fg.opacity(0.4)).child(optional.clone())))
                .child(div().flex_1())
                .children(extra)
        };
        let column = |caption: Div, field: AnyElement| {
            div().flex_1().min_w_0().flex().flex_col().gap(px(6.)).child(caption).child(field)
        };

        let close = Button::new("new-worktree-close", rust_i18n::t!("worktree.close"))
            .icon(CLOSE_ICON)
            .icon_size(16.)
            .with_size(ButtonSize::Icon)
            .rounded(px(6.))
            .cursor_pointer()
            .on_press(cx, |this, window, cx| this.close_new_worktree(window, cx));
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .id("new-worktree-title")
                    .role(Role::Heading)
                    .aria_level(1)
                    .aria_label(title.clone())
                    .text_size(FontSize::Heading)
                    .font_weight(FontWeight::BOLD)
                    .child(title.clone()),
            )
            .child(close);

        let add_project_label = rust_i18n::t!("workspace.add_project").into_owned();
        let add_project = Button::new("new-worktree-add-project", add_project_label.clone())
            .with_size(ButtonSize::IconXs)
            .icon(FOLDER_PLUS_ICON)
            .icon_size(13.)
            .tooltip(tooltip(add_project_label, Some(&NewWorkspace)))
            .on_press(cx, |this, window, cx| {
                this.close_new_worktree(window, cx);
                this.new_workspace(&NewWorkspace, window, cx);
            });
        let project_icon = div().flex_none().size(px(16.)).flex().items_center().justify_center().map(|slot| {
            match project.and_then(|project| project.avatar.clone()) {
                Some(avatar) => slot.child(img(avatar).size(px(16.)).rounded(px(4.))),
                None => slot.child(svg().path(FOLDER_ICON).size(px(14.)).text_color(fg.opacity(0.6))),
            }
        });
        let project_name = project.map(|project| project.name.clone()).unwrap_or_default();
        let project_select = self.worktree_select(
            Combo::Project,
            rust_i18n::t!("worktree.project").into_owned(),
            project_name,
            Some(project_icon.into_any_element()),
            cx,
        );
        let host_icon = svg().flex_none().path(host_icon(&dialog.host)).size(px(14.)).text_color(fg.opacity(0.7));
        let host_select = self.worktree_select(
            Combo::Host,
            rust_i18n::t!("worktree.host").into_owned(),
            remote_hosts::display_name(&dialog.host, cx),
            Some(host_icon.into_any_element()),
            cx,
        );
        let where_row = div()
            .flex()
            .gap(px(12.))
            .child(column(
                caption("worktree.project", false, Some(add_project.into_any_element())),
                project_select.into_any_element(),
            ))
            .child(column(caption("worktree.host", false, None), host_select.into_any_element()));

        let name_field = column(
            caption("worktree.name", true, None),
            Input::new(&dialog.name).h(scaled(SELECT_HEIGHT)).px(px(10.)).rounded(px(8.)).into_any_element(),
        );

        let (agent_logo_el, agent_name) = match dialog.agent {
            Some(agent) => {
                let kind = super::new_tab::launcher_kind(agent);
                (kind.and_then(|kind| agent_logo(kind, px(14.), fg)), LAUNCHERS[agent].label.to_owned())
            }
            None => (
                Some(
                    svg().path(TERMINAL_ICON).flex_none().size(px(14.)).text_color(fg.opacity(0.7)).into_any_element(),
                ),
                rust_i18n::t!("worktree.blank_terminal").into_owned(),
            ),
        };
        let agent_select = self.worktree_select(
            Combo::Agent,
            rust_i18n::t!("worktree.agent").into_owned(),
            agent_name,
            agent_logo_el,
            cx,
        );
        let agent_field = column(caption("worktree.agent", true, None), agent_select.into_any_element());

        let advanced_label = rust_i18n::t!("worktree.advanced").into_owned();
        let advanced_toggle = div()
            .id("new-worktree-advanced")
            .role(Role::Button)
            .aria_label(advanced_label.clone())
            .aria_expanded(dialog.advanced)
            .flex()
            .items_center()
            .gap(px(6.))
            .self_start()
            .py(px(2.))
            .cursor_pointer()
            .font_weight(FontWeight::MEDIUM)
            .child(advanced_label)
            .child(
                svg()
                    .path(if dialog.advanced { CHEVRON_DOWN_ICON } else { CHEVRON_RIGHT_ICON })
                    .size(px(12.))
                    .text_color(fg.opacity(0.7)),
            )
            .on_press(cx, |this, window, cx| {
                if let Some(dialog) = &mut this.new_worktree {
                    dialog.advanced = !dialog.advanced;
                    let focus = if dialog.advanced { &dialog.branch } else { &dialog.name }.focus_handle(cx);
                    window.focus(&focus, cx);
                    cx.notify();
                }
            });
        let advanced = dialog.advanced.then(|| {
            let base = match (&dialog.base, dialog.branches()) {
                (Some(base), _) => base.clone(),
                (None, Some(branches)) => base_head_label(branches),
                (None, None) => rust_i18n::t!("worktree.base_head_plain").into_owned(),
            };
            let base_select =
                self.worktree_select(Combo::Base, rust_i18n::t!("worktree.base").into_owned(), base, None, cx);
            div()
                .flex()
                .gap(px(12.))
                .child(column(
                    caption("worktree.branch", true, None),
                    Input::new(&dialog.branch).h(scaled(SELECT_HEIGHT)).px(px(10.)).rounded(px(8.)).into_any_element(),
                ))
                .child(column(caption("worktree.base", false, None), base_select.into_any_element()))
        });

        let error = dialog.error.clone().map(|error| {
            div()
                .id("new-worktree-error")
                .aria_static_text(error.clone())
                .text_color(cx.theme().destructive)
                .child(error)
        });

        let more_label = rust_i18n::t!("worktree.more").into_owned();
        let more = Switch::new("new-worktree-more", more_label.clone(), dialog.more)
            .label(more_label)
            .text_color(fg.opacity(0.7))
            .cursor_pointer()
            .on_press(cx, |this, _, cx| {
                if let Some(dialog) = &mut this.new_worktree {
                    dialog.more = !dialog.more;
                    cx.notify();
                }
            });
        let create_label =
            rust_i18n::t!(if dialog.creating { "worktree.creating" } else { "worktree.create" }).into_owned();
        let create = Button::new("new-worktree-create", create_label.clone())
            .variant(ButtonVariant::Default)
            .h_auto()
            .px(px(14.))
            .py(px(7.))
            .gap(px(10.))
            .rounded(px(8.))
            .disabled(dialog.creating || project.is_none())
            .child(create_label)
            .child(
                div()
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(cx.theme().primary_foreground.opacity(0.3))
                    .text_size(FontSize::Small)
                    .text_color(cx.theme().primary_foreground.opacity(0.7))
                    .child(crate::keybinds::secondary_text("enter")),
            )
            .when(!dialog.creating && project.is_some(), |button| {
                button.cursor_pointer().on_press(cx, |this, window, cx| this.submit_new_worktree(window, cx))
            });
        let footer =
            div().pt(px(4.)).flex().items_center().gap(px(12.)).child(more).child(div().flex_1()).child(create);

        let panel = Dialog::new("new-worktree-dialog", title)
            .track_focus(&dialog.focus)
            .on_key_down(cx.listener(Self::new_worktree_key))
            // 各个菜单里点的项派发到对话框上。
            .on_action(cx.listener(Self::pick_worktree_project))
            .on_action(cx.listener(Self::pick_worktree_host))
            .on_action(cx.listener(Self::pick_worktree_agent))
            .on_action(cx.listener(Self::pick_worktree_base))
            .w(px(520.))
            .max_w_full()
            .max_h_full()
            .p(px(24.))
            .flex()
            .flex_col()
            .gap(px(18.))
            .child(header)
            .child(where_row)
            .child(name_field)
            .child(agent_field)
            .child(advanced_toggle)
            .children(advanced)
            .children(error)
            .child(footer);
        // 铺满窗口的底子挡住下面的点击，点到对话框外面就取消。
        Some(
            backdrop("new-worktree-backdrop")
                .pt(px(TITLEBAR_HEIGHT + 32.))
                .bg(Hsla::black().opacity(0.25))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_new_worktree(window, cx)),
                )
                .child(panel),
        )
    }
}

/// 主机菜单和框里主机前面的图标：本机是显示器，远端是服务器。
fn host_icon(host: &HostId) -> &'static str {
    if host.is_local() { MONITOR_ICON } else { SERVER_ICON }
}

/// 起点是当前 HEAD 时显示的字：知道当前分支时带上它。
fn base_head_label(branches: &[Branch]) -> String {
    match branches.iter().find(|branch| branch.current) {
        Some(current) => rust_i18n::t!("worktree.base_head", branch = current.name.as_str()).into_owned(),
        None => rust_i18n::t!("worktree.base_head_plain").into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktrees_take_one_level_per_branch() {
        assert_eq!(worktree_dir_name("feat/login/form"), "feat-login-form");
        assert_eq!(worktree_dir_name("fix"), "fix");
    }

    #[test]
    fn branches_follow_the_workspace_name() {
        assert_eq!(branch_from_name("feat/login").as_deref(), Some("feat/login"));
        assert_eq!(branch_from_name(" 修复 登录 ").as_deref(), Some("修复-登录"));
        assert_eq!(branch_from_name("Fix: login?").as_deref(), Some("fix-login"));
        assert_eq!(branch_from_name("   "), None);
        assert_eq!(branch_from_name("：？"), Some("：？".to_owned()));
        assert_eq!(branch_from_name("~^:"), None);
    }

    #[test]
    fn branches_from_names_skip_taken_names() {
        let taken: HashSet<String> = ["fix-login", "fix-login-2", "feat-x"].into_iter().map(String::from).collect();
        assert_eq!(unused_branch("fix-login", &taken), "fix-login-3");
        assert_eq!(unused_branch("fix", &taken), "fix");
        // 工作树目录名被占了也算：`feat/x` 的目录是 `feat-x`。
        assert_eq!(unused_branch("feat/x", &taken), "feat/x-2");
    }

    #[test]
    fn automatic_branches_skip_taken_names() {
        let taken: HashSet<String> = ["worktree-1", "worktree-3"].into_iter().map(String::from).collect();
        assert_eq!(free_branch(&taken), "worktree-2");
        assert_eq!(free_branch(&HashSet::new()), "worktree-1");
    }
}
