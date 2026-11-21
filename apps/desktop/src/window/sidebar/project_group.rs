//! 侧栏里的项目：同一台主机上同一个仓库的各个工作树（`Workspace::project_dir`）归在一个项目下，每个项目一行
//! 组头，能收起、拖动排序，能打开新建工作树的对话框；只有一个 workspace、名字又和项目一样的项目并成一行，不画
//! 组头（`solo_project`）。列表顶上一条写「项目」，带一起收起展开和添加项目的按钮。

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use gpui::{
    AccessibleAction, AnyElement, App, Axis, ClickEvent, Context, Div, FontWeight, Modifiers, MouseButton,
    MouseDownEvent, Render, Role, SharedString, Stateful, Window, div, prelude::*, px, relative, svg,
};

use super::{
    HEADER_INSET, HEADER_TEXT_OPACITY, TREE_LINE_INSET, TREE_ROW_INSET, drag_chip, drop_marker, mono_font,
    sidebar_inset,
};
use crate::window::RenameTarget;
use crate::{
    host_client::hosts::HostId,
    ui::{
        button::{Button, ButtonSize},
        font_size::{self, FontSize, scaled},
        icons::{CHEVRON_DOWN_ICON, CHEVRON_RIGHT_ICON, COLLAPSE_ALL_ICON, EXPAND_ALL_ICON, PLUS_ICON},
        status_mark::{MarkKind, status_mark},
        theme::ActiveTheme as _,
        tooltip::tooltip,
    },
    window::{NewWorkspace, PressDown as _, WindowView},
};

/// 项目的组头。
const PROJECT_HEADER_HEIGHT: f32 = 30.;
/// 组头上新建工作树按钮（`ButtonSize::IconXs`）的边长，悬停时它盖在分支上。
const NEW_WORKTREE_SIZE: f32 = 16.;
/// 列表顶上写「项目」的那一条。
const PROJECTS_BAR_HEIGHT: f32 = 26.;
/// 项目栏里「项目」字形上方留出的空白，按基准字号写。
pub(super) const PROJECTS_BAR_TEXT_TOP: f32 = 8.;

/// 能新建工作树的项目：知道主工作树的仓库。
pub(in crate::window) struct RepoProject {
    pub(in crate::window) host: HostId,
    /// `Workspace::project_dir`。
    pub(in crate::window) dir: PathBuf,
    pub(in crate::window) main_worktree: PathBuf,
    pub(in crate::window) name: String,
    pub(in crate::window) avatar: Option<PathBuf>,
}

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
pub(in crate::window) fn project_name(host: &HostId, dir: &Path) -> String {
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

    /// 侧栏里能新建工作树的项目，按列表的先后。
    pub(in crate::window) fn repo_projects(&self) -> Vec<RepoProject> {
        self.projects()
            .into_iter()
            .filter_map(|(host, dir)| {
                let workspaces = || self.workspaces.iter().filter(|w| w.host == host && w.project_dir() == dir);
                let main_worktree = workspaces().find_map(|workspace| workspace.repo.main_worktree.clone())?;
                let avatar = workspaces().find_map(|workspace| workspace.repo.avatar.clone());
                let name = project_name(&host, &main_worktree);
                Some(RepoProject { host, dir, main_worktree, name, avatar })
            })
            .collect()
    }

    /// `range` 里的 workspace 是一个项目的全部：只有一个、名字又和项目一样时并成一行，不画组头。
    pub(super) fn solo_project(&self, range: &Range<usize>) -> bool {
        let [workspace] = &self.workspaces[range.clone()] else { return false };
        let dir = workspace.repo.main_worktree.as_deref().unwrap_or(workspace.project_dir());
        *workspace.name == *project_name(&workspace.host, dir)
    }

    /// 第 `ix` 个 workspace 所在项目的全部 workspace；同一个项目的在列表里挨着。
    pub(super) fn project_range(&self, ix: usize) -> Range<usize> {
        let same = |w: &super::super::model::Workspace| {
            w.host == self.workspaces[ix].host && w.project_dir() == self.workspaces[ix].project_dir()
        };
        let start = self.workspaces[..ix].iter().rposition(|w| !same(w)).map_or(0, |at| at + 1);
        let end = self.workspaces[ix..].iter().position(|w| !same(w)).map_or(self.workspaces.len(), |at| ix + at);
        start..end
    }

    /// 画着组头、能收起的项目，按列表的先后。
    fn collapsible_projects(&self) -> Vec<(HostId, PathBuf)> {
        let mut projects = Vec::new();
        let mut start = 0;
        for group in self.workspaces.chunk_by(|a, b| a.host == b.host && a.project_dir() == b.project_dir()) {
            let range = start..start + group.len();
            start = range.end;
            if !self.solo_project(&range) {
                projects.push((group[0].host.clone(), group[0].project_dir().to_path_buf()));
            }
        }
        projects
    }

    fn all_projects_collapsed(&self) -> bool {
        let projects = self.collapsible_projects();
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
        self.collapsed_projects = if self.all_projects_collapsed() { Vec::new() } else { self.collapsible_projects() };
        self.save(cx);
        cx.notify();
    }

    /// 列表里第 `ix` 个 workspace 有要用户处理的：响过铃，或者 agent 在等回答、干完了没看。
    pub(super) fn workspace_needs_attention(&self, ix: usize, cx: &App) -> bool {
        let workspace = &self.workspaces[ix];
        workspace.bell() || workspace.mark(cx).is_some_and(|mark| mark.status.needs_attention())
    }

    /// 列表顶上的一条：「项目」，右边是添加项目的按钮，有能收起的项目时悬停另露出一起收起或展开它们的按钮。
    pub(super) fn render_projects_bar(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let (icon, toggle_label) = if self.all_projects_collapsed() {
            (EXPAND_ALL_ICON, rust_i18n::t!("workspace.expand_all"))
        } else {
            (COLLAPSE_ALL_ICON, rust_i18n::t!("workspace.collapse_all"))
        };
        let add_label = rust_i18n::t!("workspace.add_project");
        let group = SharedString::from("projects-bar");
        let collapsible = !self.collapsible_projects().is_empty();
        // 和列表一样的留白再让出 `HEADER_INSET`，「项目」和组头的箭头、行首的图标对齐。
        div()
            .flex_none()
            .h(scaled(PROJECTS_BAR_HEIGHT))
            .group(group.clone())
            .map(|bar| sidebar_inset(bar, HEADER_INSET, 2., cx))
            .flex()
            .items_center()
            .gap(px(2.))
            .text_size(FontSize::Body)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(muted)
            .child(div().flex_1().min_w_0().truncate().child(rust_i18n::t!("workspace.projects")))
            .when(collapsible, |bar| {
                bar.child(
                    Button::new("projects-toggle-all", toggle_label.clone())
                        .with_size(ButtonSize::Icon)
                        .icon(icon)
                        .icon_size(13.)
                        .tooltip(tooltip(toggle_label, None))
                        // 用透明度藏，辅助工具照样按得到。
                        .opacity(0.)
                        .group_hover(group, |button| button.opacity(1.))
                        .on_press_down(cx, |this, _, cx| this.toggle_all_projects(cx)),
                )
            })
            .child(
                Button::new("projects-add", add_label.clone())
                    .with_size(ButtonSize::Icon)
                    .icon(PLUS_ICON)
                    .icon_size(13.)
                    .tooltip(tooltip(add_label, Some(&NewWorkspace)))
                    .on_press_down(cx, |this, window, cx| this.new_workspace(&NewWorkspace, window, cx)),
            )
    }

    /// 一台主机上 `range` 里的 workspace，按项目分段，每段包成一块：前面是项目的组头，收起的项目只留当前
    /// workspace 那一行；并成一行的项目整块就是那一行，按住它拖的是项目。拖来的项目放在整块上，落点提示画在
    /// 整块的上边或下边。
    pub(super) fn render_project_groups(
        &self,
        range: Range<usize>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut groups = Vec::new();
        let mut start = range.start;
        let mut after_solo = false;
        for group in self.workspaces[range.clone()].chunk_by(|a, b| a.project_dir() == b.project_dir()) {
            let group_range = start..start + group.len();
            start = group_range.end;
            let (host, dir) = (&group[0].host, group[0].project_dir());
            let solo = self.solo_project(&group_range);
            let first = group_range.start == range.start;
            // 项目之间空开一些，组头和自己的工作区挨近，一眼看出谁归谁；并成一行的项目挨着时排成紧凑的列表。
            let margin = if first {
                2.
            } else if solo && after_solo {
                0.
            } else {
                8.
            };
            after_solo = solo;
            let body: Vec<AnyElement> = if solo {
                vec![self.render_solo_row(group_range.start, modifiers, cx).into_any_element()]
            } else {
                let collapsed = self.project_collapsed(host, dir);
                let shown = |ix: &usize| !collapsed || *ix == self.active;
                let attention =
                    group_range.clone().filter(|ix| !shown(ix)).any(|ix| self.workspace_needs_attention(ix, cx));
                let header = self.render_project_header(host, dir, group_range.clone(), collapsed, attention, cx);
                let rows: Vec<_> =
                    group_range.clone().filter(shown).map(|ix| self.render_row(ix, false, modifiers, cx)).collect();
                // 组头下面的行缩进，左边一条竖线把它们和组头连起来。
                let tree = (!rows.is_empty()).then(|| {
                    div()
                        .ml(px(TREE_LINE_INSET))
                        .pl(px(TREE_ROW_INSET))
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .children(rows)
                        .into_any_element()
                });
                std::iter::once(header.into_any_element()).chain(tree).collect()
            };
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
                    .mt(px(margin))
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
                    .children(body)
                    .into_any_element(),
            );
        }
        groups
    }

    /// 并成一行的项目：第 `ix` 个 workspace 的那一行，按住拖动的是整个项目；改名时在输入框里拖选文字，不拖。
    fn render_solo_row(&self, ix: usize, modifiers: Modifiers, cx: &mut Context<Self>) -> Stateful<Div> {
        let workspace = &self.workspaces[ix];
        let dragged = DraggedProject {
            host: workspace.host.clone(),
            dir: workspace.project_dir().to_path_buf(),
            ix,
            name: workspace.name.clone(),
            width: self.sidebar_width(),
        };
        let renaming =
            self.renaming.as_ref().is_some_and(|renaming| renaming.target == RenameTarget::Workspace(workspace.id));
        self.render_row(ix, true, modifiers, cx)
            .when(!renaming, |row| row.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone())))
    }

    /// 项目上写的分支：主工作树的分支，有没推的提交时后面写 ↑ 和个数；没开主工作树、
    /// 还没问到或者不在仓库里时为空。
    pub(super) fn project_branch_label(&self, host: &HostId, dir: &Path) -> Option<String> {
        let (ix, branch) = self.main_worktree_branch(host, dir)?;
        // 当前 workspace 读着的 git 状态比定时问的新，先用它。
        let git = self.workspace().project.git.as_ref().filter(|_| ix == self.active);
        Some(match git.map_or(self.workspaces[ix].repo.ahead, |git| git.main.info.ahead) {
            0 => branch.to_string(),
            ahead => format!("{branch} ↑{ahead}"),
        })
    }

    /// 给项目新建工作树的按钮，只有知道主工作树的仓库才有；摆在哪、何时露出由调用方定。
    pub(super) fn new_worktree_button(&self, host: &HostId, dir: &Path, cx: &mut Context<Self>) -> Option<Button> {
        let main_worktree = self
            .workspaces
            .iter()
            .filter(|workspace| workspace.host == *host && workspace.project_dir() == dir)
            .find_map(|workspace| workspace.repo.main_worktree.as_deref())?;
        let label = rust_i18n::t!("workspace.new_worktree", project = project_name(host, main_worktree)).into_owned();
        let (host, dir) = (host.clone(), dir.to_path_buf());
        Some(
            Button::new(SharedString::from(format!("{}-new", project_key(&host, &dir))), label.clone())
                .with_size(ButtonSize::IconXs)
                .icon(PLUS_ICON)
                .icon_size(12.)
                .tooltip(tooltip(label, None))
                .on_press_down(cx, move |this, window, cx| {
                    this.open_new_worktree(Some((host.clone(), dir.clone())), window, cx)
                }),
        )
    }

    /// 项目组头上写的分支：项目里开着主工作树的那个 workspace 的位置和当前分支；没开主工作树、还没问到或者不在
    /// 仓库里时为空。
    pub(super) fn main_worktree_branch(&self, host: &HostId, dir: &Path) -> Option<(usize, &SharedString)> {
        self.workspaces.iter().enumerate().find_map(|(ix, workspace)| {
            let main = workspace.repo.main_worktree.as_deref()?;
            let is_main = main == workspace.dir || main == workspace.real_dir();
            (workspace.host == *host && workspace.project_dir() == dir && is_main)
                .then(|| workspace.repo.branch.as_ref().map(|branch| (ix, branch)))?
        })
    }

    /// 项目的组头：收起、展开的箭头、项目图标（`project_icon`）和项目名，右边是分支（`project_branch_label`）；悬停时分支让给新建工作树
    /// 的按钮（项目是仓库时）。点一下收起、展开，按住能拖动排序。项目里的 workspace 是 `range`。
    fn render_project_header(
        &self,
        host: &HostId,
        dir: &Path,
        range: Range<usize>,
        collapsed: bool,
        attention: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let icon = self.project_icon(host, dir, cx);
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let workspaces = &self.workspaces[range.clone()];
        let first = workspaces[0].id;
        let branch = self.project_branch_label(host, dir);
        // 名字和提示按主工作树写，不写 git 目录。
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
        let a11y_description = [
            Some(path.clone()),
            branch.clone(),
            attention.then(|| rust_i18n::t!("workspace.host_attention").into_owned()),
        ]
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
        let new_worktree = self.new_worktree_button(host, dir, cx).map(|button| {
            button
                .absolute()
                .right_0()
                .top(relative(0.5))
                .mt(px(-NEW_WORKTREE_SIZE / 2.))
                // 悬停在组头上才显出来，几个组头不一排排都是按钮；用透明度藏，辅助工具照样按得到。
                .opacity(0.)
                .group_hover(group.clone(), |button| button.opacity(1.))
        });
        let has_button = new_worktree.is_some();
        // 收起时藏起来的行里有要处理的，名字后面画一个不闪的等回答标记。
        let attention_mark = attention.then(|| {
            status_mark(SharedString::from(format!("{key}-attention")), MarkKind::Waiting { blink: false }, 10., cx)
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
            .pl(px(HEADER_INSET))
            .pr(px(8.))
            .rounded(px(7.))
            .flex()
            .items_center()
            .gap(px(7.))
            .text_color(fg.opacity(HEADER_TEXT_OPACITY))
            .hover(|header| header.bg(super::sidebar_hover(cx)))
            .tooltip(tooltip(path, None))
            // 松开时才收起、展开：按下就办的话，拖动排序一按住组就先收起了。拖动过后不算点击。
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_project(&press_host, &press_dir, None, cx);
            }))
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_project_menu(first, event.position, cx);
                }),
            )
            .child(
                svg()
                    .path(if collapsed { CHEVRON_RIGHT_ICON } else { CHEVRON_DOWN_ICON })
                    .flex_none()
                    .size(px(12.))
                    .text_color(muted),
            )
            .children(icon.map(super::project_icon_image))
            .child(div().min_w_0().truncate().text_size(FontSize::Medium).font_weight(FontWeight::SEMIBOLD).child(name))
            .children(attention_mark)
            .child(
                div()
                    .ml_auto()
                    .flex_none()
                    .relative()
                    .h_full()
                    .min_w(px(NEW_WORKTREE_SIZE))
                    .max_w(relative(0.5))
                    .flex()
                    .items_center()
                    .justify_end()
                    .children(branch.map(|branch| {
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(mono_font(cx))
                            .text_size(FontSize::Small)
                            .text_color(muted)
                            .when(has_button, |text| text.group_hover(group.clone(), |text| text.invisible()))
                            .child(branch)
                    }))
                    .children(new_worktree),
            )
    }
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
}
