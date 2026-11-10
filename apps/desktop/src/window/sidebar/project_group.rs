//! 侧栏里的项目：同一台主机上同一个仓库的各个工作树（`Workspace::project_dir`）归在一个项目下，每个项目一行
//! 组头，能收起、拖动排序，能在仓库里新建工作树开成工作区；列表顶上一条写「项目」，带一起收起展开和添加项目
//! 的按钮。

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use gpui::{
    AccessibleAction, AnyElement, App, Axis, ClickEvent, Context, Div, FontWeight, Modifiers, PromptButton,
    PromptLevel, Render, Role, SharedString, Stateful, Window, div, img, prelude::*, px, svg,
};

use super::{RENAME_FIELD_HEIGHT, ROW_HEIGHT, ROW_ICON_SIZE, drag_chip, drop_marker};
use crate::{
    host_client::hosts::HostId,
    project_host::ProjectHost,
    ui::{
        button::{Button, ButtonSize},
        font_size::{self, FontSize, scaled},
        icons::{
            BRANCH_ICON, CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, COLLAPSE_ALL_ICON, EXPAND_ALL_ICON, FOLDER_ICON,
            FOLDER_PLUS_ICON, PLUS_ICON,
        },
        theme::{AGENT_BLOCKED_COLOR, ActiveTheme as _},
        tooltip::tooltip,
    },
    window::{NewWorkspace, PressDown as _, RenameTarget, Renaming, WindowView, inline_edit::InlineEdit},
};

/// 项目的组头。
const PROJECT_HEADER_HEIGHT: f32 = 28.;
/// 列表顶上写「项目」的那一条。
const PROJECTS_BAR_HEIGHT: f32 = 26.;

/// 拖动中的项目：拖动时跟着鼠标画出来，放到同一台主机的另一个项目上时整组挪过去。
#[derive(Clone)]
struct DraggedProject {
    host: HostId,
    dir: PathBuf,
    /// 开始拖动时项目第一个 workspace 在列表里的位置，用来决定落点提示画在目标上边还是下边。
    ix: usize,
    name: SharedString,
    /// 侧栏当前的宽度，预览照这个宽度画。
    width: f32,
}

impl Render for DraggedProject {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = cx.theme().mix(0.10);
        drag_chip(px(self.width - 12.), px(PROJECT_HEADER_HEIGHT * font_size::scale(cx)), self.name.clone(), bg, cx)
    }
}

/// 项目组头上的名字：家目录写 `~`，别的取目录名，裸仓库去掉 `.git` 后缀。还不知道主工作树、`dir` 是共用的
/// git 目录 `.git` 时取它的上一级。
fn project_name(host: &HostId, dir: &Path) -> String {
    let dir = dir.parent().filter(|_| dir.file_name().is_some_and(|name| name == ".git")).unwrap_or(dir);
    if host.home().as_deref() == Some(dir) {
        return "~".into();
    }
    let Some(name) = dir.file_name() else { return dir.display().to_string() };
    let name = name.to_string_lossy();
    name.strip_suffix(".git").filter(|stem| !stem.is_empty()).unwrap_or(&name).to_owned()
}

/// 项目在侧栏里的元素 id：按主机和目录取，不按位置。
fn project_key(host: &HostId, dir: &Path) -> String {
    format!("project-{}-{}", host.key().unwrap_or_default(), dir.display())
}

/// 新工作树的目录名：分支名里的 `/` 换成 `-`，一个分支只占 `runode_git::WORKTREES_DIR` 下的一级。
fn worktree_dir_name(branch: &str) -> String {
    branch.replace('/', "-")
}

impl WindowView {
    fn project_collapsed(&self, host: &HostId, dir: &Path) -> bool {
        self.collapsed_projects.iter().any(|(h, d)| h == host && d == dir)
    }

    /// 侧栏里的各个项目，按列表的先后，不重复。
    fn projects(&self) -> Vec<(HostId, PathBuf)> {
        let mut projects: Vec<(HostId, PathBuf)> = Vec::new();
        for workspace in &self.workspaces {
            if !projects.iter().any(|(h, d)| *h == workspace.host && d == workspace.project_dir()) {
                projects.push((workspace.host.clone(), workspace.project_dir().to_path_buf()));
            }
        }
        projects
    }

    fn all_projects_collapsed(&self) -> bool {
        let projects = self.projects();
        !projects.is_empty() && projects.iter().all(|(host, dir)| self.project_collapsed(host, dir))
    }

    /// `expand` 为空时切换项目收没收起；给了时只在不是那个状态时切换，辅助工具的展开、收起用。
    fn toggle_project(&mut self, host: &HostId, dir: &Path, expand: Option<bool>, cx: &mut Context<Self>) {
        let at = self.collapsed_projects.iter().position(|(h, d)| h == host && d == dir);
        if expand.is_some_and(|expand| expand != at.is_some()) {
            return;
        }
        match at {
            Some(at) => drop(self.collapsed_projects.remove(at)),
            None => self.collapsed_projects.push((host.clone(), dir.to_path_buf())),
        }
        self.save(cx);
        cx.notify();
    }

    /// 都收着时全展开，否则全收起。
    fn toggle_all_projects(&mut self, cx: &mut Context<Self>) {
        self.collapsed_projects = if self.all_projects_collapsed() { Vec::new() } else { self.projects() };
        self.save(cx);
        cx.notify();
    }

    /// 列表里第 `ix` 个 workspace 有要用户处理的：响过铃，或者 agent 在等回答、干完了没看。
    pub(super) fn workspace_needs_attention(&self, ix: usize, cx: &App) -> bool {
        let workspace = &self.workspaces[ix];
        workspace.bell() || workspace.mark(cx).is_some_and(|mark| mark.status.needs_attention())
    }

    /// 列表顶上的一条：「项目」，右边是一起收起或展开所有项目、添加项目的按钮。
    pub(super) fn render_projects_bar(&self, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        let (icon, toggle_label) = if self.all_projects_collapsed() {
            (EXPAND_ALL_ICON, rust_i18n::t!("workspace.expand_all"))
        } else {
            (COLLAPSE_ALL_ICON, rust_i18n::t!("workspace.collapse_all"))
        };
        let add_label = rust_i18n::t!("workspace.add_project");
        div()
            .flex_none()
            .h(scaled(PROJECTS_BAR_HEIGHT))
            .pl(px(14.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap(px(2.))
            .text_size(FontSize::Small)
            .text_color(fg.opacity(0.5))
            .child(div().flex_1().min_w_0().truncate().child(rust_i18n::t!("workspace.projects")))
            .child(
                Button::new("projects-toggle-all", toggle_label.clone())
                    .with_size(ButtonSize::IconXs)
                    .icon(icon)
                    .icon_size(13.)
                    .tooltip(tooltip(toggle_label, None))
                    .on_press_down(cx, |this, _, cx| this.toggle_all_projects(cx)),
            )
            .child(
                Button::new("projects-add", add_label.clone())
                    .with_size(ButtonSize::IconXs)
                    .icon(FOLDER_PLUS_ICON)
                    .icon_size(13.)
                    .tooltip(tooltip(add_label, Some(&NewWorkspace)))
                    .on_press_down(cx, |this, window, cx| this.new_workspace(&NewWorkspace, window, cx)),
            )
    }

    /// 一台主机上 `range` 里的 workspace，按项目分段，每段包成一块：前面是项目的组头，正在新建工作树时组头下面
    /// 是输入框，收起的项目只留当前 workspace 那一行。拖来的项目放在整块上，落点提示画在整块的上边或下边。
    pub(super) fn render_project_groups(
        &self,
        range: Range<usize>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut groups = Vec::new();
        let mut start = range.start;
        for group in self.workspaces[range.clone()].chunk_by(|a, b| a.project_dir() == b.project_dir()) {
            let group_range = start..start + group.len();
            start = group_range.end;
            let (host, dir) = (&group[0].host, group[0].project_dir());
            let collapsed = self.project_collapsed(host, dir);
            let shown = |ix: &usize| !collapsed || *ix == self.active;
            let attention =
                group_range.clone().filter(|ix| !shown(ix)).any(|ix| self.workspace_needs_attention(ix, cx));
            let first = group_range.start == range.start;
            let mut rows = vec![
                self.render_project_header(host, dir, group_range.clone(), collapsed, attention, cx).into_any_element(),
            ];
            if let Some(edit) = self.new_worktree_edit(host, dir) {
                rows.push(self.render_new_worktree_row(edit, cx).into_any_element());
            }
            for ix in group_range.clone().filter(shown) {
                rows.push(self.render_row(ix, modifiers, cx).into_any_element());
            }
            let (over_host, over_dir) = (host.clone(), dir.to_path_buf());
            let (drop_host, drop_dir) = (host.clone(), dir.to_path_buf());
            let ix = group_range.start;
            groups.push(
                div()
                    .id(SharedString::from(format!("{}-block", project_key(host, dir))))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    // 项目之间空开一些，组头和自己的工作区挨近，一眼看出谁归谁。
                    .mt(px(if first { 2. } else { 8. }))
                    .drag_over::<DraggedProject>(move |style, dragged, _, cx| {
                        if dragged.host == over_host && dragged.dir != over_dir {
                            drop_marker(style, dragged.ix, ix, Axis::Vertical, cx)
                        } else {
                            style
                        }
                    })
                    .on_drop(cx.listener(move |this, dragged: &DraggedProject, _, cx| {
                        if dragged.host == drop_host {
                            this.move_project(&drop_host, &dragged.dir, &drop_dir, cx);
                        }
                    }))
                    .children(rows)
                    .into_any_element(),
            );
        }
        groups
    }

    fn new_worktree_edit(&self, host: &HostId, dir: &Path) -> Option<&InlineEdit> {
        self.renaming.as_ref().and_then(|renaming| match &renaming.target {
            RenameTarget::NewWorktree(h, d) if h == host && d == dir => Some(&renaming.edit),
            _ => None,
        })
    }

    /// 项目的组头：仓库 owner 的 GitHub 头像（没有时画文件夹）和项目名，悬停时露出新建工作树（项目是仓库时）
    /// 和收起的按钮；点一下收起、展开，按住能拖动排序。项目里的 workspace 是 `range`。
    fn render_project_header(
        &self,
        host: &HostId,
        dir: &Path,
        range: Range<usize>,
        collapsed: bool,
        attention: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let fg = cx.theme().foreground;
        let workspaces = &self.workspaces[range.clone()];
        let avatar = workspaces.iter().find_map(|workspace| workspace.repo.avatar.clone());
        // 只有知道主工作树的仓库才能新建工作树；名字和提示也按它写，不写 git 目录。
        let main_worktree = workspaces.iter().find_map(|workspace| workspace.repo.main_worktree.clone());
        let shown_dir = main_worktree.as_deref().unwrap_or(dir);
        let name = project_name(host, shown_dir);
        let path = host.display_dir(shown_dir);
        let dragged = DraggedProject {
            host: host.clone(),
            dir: dir.to_path_buf(),
            ix: range.start,
            name: name.clone().into(),
            width: self.sidebar_width(),
        };
        let key = SharedString::from(project_key(host, dir));
        let group = SharedString::from(format!("{key}-group"));
        let a11y_description =
            [Some(path.clone()), attention.then(|| rust_i18n::t!("workspace.host_attention").into_owned())]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
        let (toggle_host, toggle_dir) = (host.clone(), dir.to_path_buf());
        let a11y = |expand: Option<bool>, cx: &mut Context<Self>| {
            let (host, dir) = (toggle_host.clone(), toggle_dir.clone());
            let view = cx.entity().downgrade();
            move |_: Option<&gpui::accesskit::ActionData>, _: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| this.toggle_project(&host, &dir, expand, cx)).ok();
            }
        };
        let (expand, collapse, flip) = (a11y(Some(true), cx), a11y(Some(false), cx), a11y(None, cx));
        let (press_host, press_dir) = (host.clone(), dir.to_path_buf());
        let icon =
            div().flex_none().size(px(ROW_ICON_SIZE)).flex().items_center().justify_center().map(|slot| match avatar {
                Some(avatar) => slot.child(img(avatar).size(px(ROW_ICON_SIZE)).rounded(px(4.))),
                None => slot.child(svg().path(FOLDER_ICON).size(px(14.)).text_color(fg.opacity(0.6))),
            });
        let new_worktree = main_worktree.is_some().then(|| {
            let label = rust_i18n::t!("workspace.new_worktree", project = name.as_str()).into_owned();
            let (host, dir) = (host.clone(), dir.to_path_buf());
            Button::new(SharedString::from(format!("{key}-new")), label.clone())
                .with_size(ButtonSize::IconXs)
                .icon(PLUS_ICON)
                .icon_size(12.)
                .tooltip(tooltip(label, None))
                // 悬停在组头上才显出来，几个组头不一排排都是按钮；用透明度藏，辅助工具照样按得到。
                .opacity(0.)
                .group_hover(group.clone(), |button| button.opacity(1.))
                .on_press_down(cx, move |this, window, cx| {
                    this.start_new_worktree(host.clone(), dir.clone(), window, cx)
                })
        });
        div()
            .id(key)
            .role(Role::Button)
            .aria_label(name.clone())
            .aria_expanded(!collapsed)
            .aria_description(a11y_description)
            .on_a11y_action(AccessibleAction::Expand, expand)
            .on_a11y_action(AccessibleAction::Collapse, collapse)
            .on_a11y_action(AccessibleAction::Click, flip)
            .flex_none()
            .h(scaled(PROJECT_HEADER_HEIGHT))
            .group(group.clone())
            .pl(px(8.))
            .pr(px(4.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_color(fg.opacity(0.9))
            .hover(|header| header.bg(cx.theme().accent))
            .tooltip(tooltip(path, None))
            // 松开时才收起、展开：按下就办的话，拖动排序一按住组就先收起了。拖动过后不算点击。
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_project(&press_host, &press_dir, None, cx);
            }))
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .child(icon)
            .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(name))
            .when(attention, |header| {
                header.child(div().flex_none().size(px(6.)).rounded_full().bg(gpui::rgb(AGENT_BLOCKED_COLOR)))
            })
            .children(new_worktree)
            // 收起时箭头一直显示，提醒这个项目里还有藏着的行。
            .child(
                div()
                    .flex_none()
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(!collapsed, |chevron| chevron.opacity(0.).group_hover(group, |chevron| chevron.opacity(1.)))
                    .child(
                        svg()
                            .path(if collapsed { CHEVRON_RIGHT_ICON } else { CHEVRON_DOWN_ICON })
                            .size(px(12.))
                            .text_color(fg.opacity(0.4)),
                    ),
            )
    }

    /// 项目组头下面输入新分支名的一行。
    fn render_new_worktree_row(&self, edit: &InlineEdit, cx: &mut Context<Self>) -> Div {
        let fg = cx.theme().foreground;
        div()
            .flex_none()
            .h(scaled(ROW_HEIGHT))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .flex_none()
                    .w(px(ROW_ICON_SIZE))
                    .flex()
                    .justify_center()
                    .child(svg().path(BRANCH_ICON).size(px(12.)).text_color(fg.opacity(0.5))),
            )
            .child(div().flex_1().min_w_0().child(edit.render(px(RENAME_FIELD_HEIGHT * font_size::scale(cx)))))
    }

    /// 在主机 `host` 上项目是 `dir` 的仓库里新建工作树：组头下面出一个输入框，输入新分支的名字，按回车才建，
    /// 按 Esc 或点开都不建。
    fn start_new_worktree(&mut self, host: HostId, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_pending_edit(window, cx);
        self.collapsed_projects.retain(|(h, d)| !(*h == host && *d == dir));
        let label = rust_i18n::t!("workspace.worktree_branch").into_owned();
        let edit = InlineEdit::new(String::new(), 0, label.clone(), Self::finish_rename, window, cx).cancel_on_leave();
        edit.field.update(cx, |field, _| field.set_placeholder(label));
        self.renaming = Some(Renaming { target: RenameTarget::NewWorktree(host, dir), edit });
        cx.notify();
    }

    /// 在主机 `host` 上项目是 `project`（`Workspace::project_dir`）的仓库里，从当前 HEAD 拉出新分支 `branch`，
    /// 检出在主工作树下 `runode_git::WORKTREES_DIR` 里的新工作树，建好后开成工作区，归在这个项目下。不知道主
    /// 工作树时不建；建不出来时弹框说原因。
    pub(super) fn create_worktree(
        &mut self,
        host: HostId,
        project: PathBuf,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(main_worktree) = self
            .workspaces
            .iter()
            .filter(|workspace| workspace.host == host && workspace.project_dir() == project)
            .find_map(|workspace| workspace.repo.main_worktree.clone())
        else {
            return;
        };
        if !runode_git::valid_branch_name(&branch) {
            let detail = rust_i18n::t!("workspace.worktree_bad_name", name = branch.as_str()).into_owned();
            report_worktree_failed(&detail, window, cx);
            return;
        }
        let worktree = main_worktree.join(runode_git::WORKTREES_DIR).join(worktree_dir_name(&branch));
        let repo = ProjectHost::of(&host).repo(runode_git::Repo::new(main_worktree));
        let job = cx.background_spawn({
            let (worktree, branch) = (worktree.clone(), branch.clone());
            async move { repo.add_worktree(&worktree, &branch) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let added = job.await;
            this.update_in(cx, |this, window, cx| match added {
                Ok(()) => this.open_worktree(&host, project, worktree, branch, window, cx),
                Err(err) => report_worktree_failed(&crate::git_view::git_error_text(&err), window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// 在新建的工作树 `worktree` 上开工作区，名字是分支名，归在项目 `project` 下、排在它的最后，并切过去。
    fn open_worktree(
        &mut self,
        host: &HostId,
        project: PathBuf,
        worktree: PathBuf,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.spawn_terminal_on(host, Some(&worktree), None, window, cx) else {
            return;
        };
        let ix = self
            .workspaces
            .iter()
            .rposition(|workspace| workspace.host == *host && workspace.project_dir() == project)
            .map_or(self.workspaces.len(), |last| last + 1);
        self.insert_workspace(ix, worktree, Some(branch), view, window, cx);
        let workspace = &mut self.workspaces[self.active];
        workspace.common_git_dir = Some(project);
        let id = workspace.id;
        self.regroup_workspace(id);
        self.save(cx);
        cx.notify();
    }
}

fn report_worktree_failed(detail: &str, window: &mut Window, cx: &mut App) {
    let answer = crate::prompt::show(
        window,
        PromptLevel::Warning,
        &rust_i18n::t!("workspace.worktree_failed"),
        Some(detail),
        &[PromptButton::cancel(rust_i18n::t!("ssh.ok"))],
        cx,
    );
    // 只说一声，但收的那头要留到框关掉。
    cx.foreground_executor().spawn(answer).detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_are_named_after_their_directory() {
        assert_eq!(project_name(&HostId::Local, Path::new("/src/runode")), "runode");
        assert_eq!(project_name(&HostId::Local, Path::new("/srv/app.git")), "app");
        assert_eq!(project_name(&HostId::Local, Path::new("/src/runode/.git")), "runode");
    }

    #[test]
    fn worktrees_take_one_level_per_branch() {
        assert_eq!(worktree_dir_name("feat/login/form"), "feat-login-form");
        assert_eq!(worktree_dir_name("fix"), "fix");
    }
}
