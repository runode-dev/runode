//! Git 面板底部的「图表」，单独一栏：标题一行可以收起，上沿的分隔线
//! 拖动改变高度，展开时每行一个提交，左边画分支的线和点，右边是引用标签和说明首行，行尾淡色写着
//! 多久以前。点提交展开它改的文件，点文件在预览栏里看它在这个提交里的 diff；右键可以复制提交号、
//! 切到这个提交或者从它新建分支。多个仓库时显示最近点过的那块的仓库（`GitPanel::graph_repo`），
//! 仓库名写在标题上。
//!
//! 历史在后台读，只在图表展开着时读显示的那个仓库的；仓库的 `RepoInfo` 变了（提交、切分支、拉取、
//! 储藏之后）或者点了刷新才重读，工作区里的文件变了不重读。lane 怎么排由
//! `runode_git::graph_layout` 算好，这里只照着画。

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use gpui::{
    AccessibleAction, Action, AnyElement, BorderStyle, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, Div,
    Hsla, MouseButton, MouseDownEvent, PathBuilder, Pixels, PromptLevel, Role, Window, canvas, div, fill, img, point,
    prelude::*, px, quad, svg, uniform_list,
};
use runode_git::{self as git, Commit, DiffSide, GraphRow, Half, RefKind};
use runode_shared_types::color::Rgb;

use super::{
    PressDown,
    list::{ROW_HEIGHT, a11y_open_diff, chevron, file_description, status_letter},
    repo_name,
    rows::{Busy, CommitChanges, CommitNote, GRAPH_PAGE, Graph, GraphNote},
};
use crate::{
    assets::{ARROW_DOWN_ICON, REFRESH_ICON},
    ui::{
        file_icons::{file_icon, folder_icon},
        hsla,
        tooltip::tooltip,
    },
    window::{
        DIVIDER_GRAB_WIDTH, Divider, TITLEBAR_HEIGHT, WindowView, divider_color,
        files::menu_item,
        model::base_name,
        preview::DiffTarget,
        project::{ADDED, MODIFIED, REMOVED, RENAMED, added_label, removed_label},
    },
};

/// lane 轮换的颜色：面板里改动用的那几种，再加紫、青、橙，深浅底色上都看得清。
const LANE_COLORS: [Rgb; 7] =
    [RENAMED, ADDED, MODIFIED, Rgb(0xB0, 0x83, 0xF0), Rgb(0x39, 0xB8, 0xC6), REMOVED, Rgb(0xE0, 0x8A, 0x3C)];
/// 一条 lane 的宽度。lane 多时压窄，整个图不超过 `MAX_GRAPH_WIDTH`，最窄 `MIN_LANE_WIDTH`；
/// 再多的 lane 画到图的右边界为止，截掉。
const LANE_WIDTH: f32 = 10.;
const MAX_GRAPH_WIDTH: f32 = 80.;
const MIN_LANE_WIDTH: f32 = 4.;
/// 一行最多显示几个引用标签，其余合成「+N」。
const MAX_REF_LABELS: usize = 2;
/// 引用标签最宽这么宽，再长的截断；估算宽度时按每个字 6px 加两边留白。
const MAX_LABEL_WIDTH: f32 = 90.;
/// 说明首行至少留这么宽：放不下时先不显示日期，再少显示标签。
const MIN_SUBJECT_WIDTH: f32 = 80.;
/// 行尾日期占的宽度，和「+N」占的宽度。
const DATE_WIDTH: f32 = 56.;
const MORE_LABEL_WIDTH: f32 = 22.;
/// 鼠标移到「加载更多」那一行时链接的底色变亮，用这个组名。
const GRAPH_MORE_GROUP: &str = "git-graph-more";
/// 文件比提交、块头比文件往右缩进的宽度。
const INDENT: f32 = 12.;
/// 图表展开着时没拖过的高度占面板标题以下的几成；拖动时最矮这么高，上面的改动列表至少留
/// `CHANGES_MIN_HEIGHT`。
const GRAPH_SHARE: f32 = 0.4;
const GRAPH_MIN_HEIGHT: f32 = 90.;
const CHANGES_MIN_HEIGHT: f32 = 120.;

/// 图表里一个提交的右键菜单项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitOp {
    CopyId,
    Checkout,
    CreateBranch,
}

/// 对根目录是 `repo` 的仓库里提交号是 `id` 的提交做 `op`。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct GitCommitAction {
    pub repo: PathBuf,
    pub id: String,
    pub op: CommitOp,
}

/// 图表展开着时多高：`saved` 是拖过的高度，`room` 是面板标题以下的高度。
fn graph_height(saved: Option<f32>, room: f32) -> f32 {
    saved.unwrap_or(room * GRAPH_SHARE).min(room - CHANGES_MIN_HEIGHT).max(GRAPH_MIN_HEIGHT)
}

/// 图有 `lanes` 条 lane 时一条 lane 画多宽，以及整个图多宽。
fn lane_geometry(lanes: usize) -> (f32, f32) {
    let lanes = lanes.max(1) as f32;
    let lane = (MAX_GRAPH_WIDTH / lanes).clamp(MIN_LANE_WIDTH, LANE_WIDTH);
    (lane, (lane * lanes).min(MAX_GRAPH_WIDTH))
}

impl WindowView {
    /// 图表展开着、显示的仓库还没读过或者变了时，在后台读一遍。
    pub(super) fn sync_graphs(&mut self, cx: &mut Context<Self>) {
        if self.git_graph_collapsed {
            return;
        }
        let project = &self.workspace().project;
        let Some(git) = project.git.as_ref() else {
            return;
        };
        let panel = &project.git_panel;
        let Some(repo) = git.get(panel.graph_repo(git)) else {
            return;
        };
        if panel.repos.get(&repo.root).is_none_or(|state| state.graph.needs_read(&repo.info)) {
            let (repo, info) = (repo.repo(), repo.info.clone());
            self.load_history(repo, info, cx);
        }
    }

    /// 面板底部的图表：标题一行，点了收起展开；展开时下面是提交的列表，高度由标题上沿的分隔线
    /// 拖动，双击分隔线恢复默认高度。`room` 是面板标题以下的高度。
    pub(super) fn render_graph_pane(
        &self,
        git: &git::Repos,
        room: f32,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Div {
        let panel = &self.workspace().project.git_panel;
        let open = !self.git_graph_collapsed;
        let fg_hsla = hsla(fg);
        let repo = git.get(panel.graph_repo(git));
        let name = repo.filter(|_| git.count() > 1).map(repo_name);
        let root = repo.map(|repo| repo.root.clone());
        let title = rust_i18n::t!("git.section.graph").into_owned();
        let header = div()
            .id("git-graph-header")
            .role(Role::Button)
            .aria_label(title.clone())
            .aria_expanded(open)
            .when_some(name.clone(), |header, name| header.aria_description(name))
            .flex_none()
            .h(px(ROW_HEIGHT + 4.))
            .w_full()
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .border_t_1()
            .border_color(divider_color(fg_hsla))
            .text_color(fg_hsla)
            .hover(|header| header.bg(hsla(bg.mix(fg, 0.06))))
            .child(chevron(open, fg))
            .child(div().flex_none().text_size(px(11.)).font_weight(gpui::FontWeight::SEMIBOLD).child(title.clone()))
            .child(
                div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(fg_hsla.opacity(0.5)).children(name),
            )
            .when_some(root.filter(|_| open), |header, root| {
                header.child(self.header_button(
                    "git-graph-refresh",
                    REFRESH_ICON,
                    Some(rust_i18n::t!("git.graph.refresh").into_owned()),
                    fg,
                    bg,
                    cx,
                    move |this, _, cx| this.refresh_graph(&root, cx),
                ))
            })
            .on_press_down(cx, |this, _, cx| {
                this.git_graph_collapsed = !this.git_graph_collapsed;
                this.save(cx);
                cx.notify();
            });
        let pane = div().relative().flex_none().w_full().flex().flex_col().child(header);
        if !open {
            return pane;
        }
        let list = div().id("git-graph").role(Role::Tree).aria_label(title).flex_1().min_h_0().flex().flex_col().child(
            uniform_list(
                "git-graph-rows",
                panel.graph_rows.len(),
                cx.processor(move |this, range: Range<usize>, _, cx| this.render_graph_rows(range, fg, bg, cx)),
            )
            .track_scroll(&panel.graph_scroll)
            .flex_1(),
        );
        let handle = div()
            .id("git-graph-divider")
            .absolute()
            .left_0()
            .top(px(-DIVIDER_GRAB_WIDTH / 2.))
            .w_full()
            .h(px(DIVIDER_GRAB_WIDTH))
            .cursor(CursorStyle::ResizeUpDown)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        this.git_graph_height = None;
                        this.save(cx);
                    } else {
                        this.dragging_divider = Some(Divider::GitGraph);
                    }
                    cx.notify();
                }),
            );
        pane.h(px(graph_height(self.git_graph_height, room))).child(list).child(handle)
    }

    /// 拖动图表上沿的分隔线，上沿跟到窗口里的纵坐标 `y`；面板从窗口顶上一直到底。
    pub(in crate::window) fn resize_git_graph(&mut self, y: f32, viewport: f32) {
        self.git_graph_height = Some(graph_height(Some(viewport - y), viewport - TITLEBAR_HEIGHT));
    }

    /// 图表的列表里看得见的那些行。
    fn render_graph_rows(&self, range: Range<usize>, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let project = &self.workspace().project;
        let Some(git) = &project.git else {
            return Vec::new();
        };
        let panel = &project.git_panel;
        range
            .filter_map(|ix| panel.graph_rows.get(ix).map(|row| (ix, *row)))
            .map(|(ix, row)| self.render_git_row(ix, row, git, fg, bg, cx))
            .collect()
    }

    /// 在后台读 `repo` 的历史，读完换上、重排行。
    fn load_history(&mut self, repo: git::Repo, info: git::RepoInfo, cx: &mut Context<Self>) {
        let id = self.workspace().id;
        let root = repo.root.clone();
        let graph = &mut self.workspace_mut().project.git_panel.repo_mut(&root).graph;
        graph.loading = true;
        graph.stale = false;
        let limit = graph.limit();
        // 「加载更多」那一行换成「正在读取」。
        let project = &mut self.workspace_mut().project;
        project.git_panel.rebuild(project.git.as_ref());
        let job = cx.background_spawn(async move { repo.history(limit) });
        cx.spawn(async move |this, cx| {
            let history = job.await;
            this.update(cx, |this, cx| {
                let Some(workspace) = this.workspaces.iter_mut().find(|workspace| workspace.id == id) else {
                    return;
                };
                let project = &mut workspace.project;
                let graph = &mut project.git_panel.repo_mut(&root).graph;
                graph.loading = false;
                graph.read_for = Some(info);
                graph.lanes =
                    history.as_ref().map_or(0, |history| history.rows.iter().map(|row| row.width).max().unwrap_or(0));
                graph.history = Some(history.map_err(|err| err.message));
                project.git_panel.rebuild(project.git.as_ref());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 监听到 `paths` 变了：落在哪个仓库的引用上（新建、删除、移动分支或 tag，HEAD 变了），那个
    /// 仓库读过的图表就要重读，展开着时下次显示就读。`RepoInfo` 看不出别的分支和 tag 的变化。
    pub(in crate::window) fn graph_refs_changed(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let project = &mut self.workspace_mut().project;
        let Some(git) = &project.git else {
            return;
        };
        let mut marked = false;
        for repo in git.iter() {
            if let Some(state) = project.git_panel.repos.get_mut(&repo.root)
                && state.graph.history.is_some()
                && paths.iter().any(|path| git::refs_changed(&repo.git_dir, path))
            {
                state.graph.stale = true;
                marked = true;
            }
        }
        if marked {
            cx.notify();
        }
    }

    /// 根目录是 `root` 的仓库的图表下次显示时重读。
    pub(super) fn refresh_graph(&mut self, root: &Path, cx: &mut Context<Self>) {
        self.workspace_mut().project.git_panel.repo_mut(root).graph.stale = true;
        cx.notify();
    }

    /// 刷新时所有仓库的图表都重读。
    pub(super) fn refresh_graphs(&mut self) {
        for repo in self.workspace_mut().project.git_panel.repos.values_mut() {
            repo.graph.stale = true;
        }
    }

    /// 「加载更多」：多读一页。
    /// 读的期间再点不算，免得一下子叠上好几页。
    fn load_more_commits(&mut self, root: &Path, cx: &mut Context<Self>) {
        let project = &mut self.workspace_mut().project;
        let graph = &mut project.git_panel.repo_mut(root).graph;
        if graph.loading {
            return;
        }
        graph.limit = graph.limit() + GRAPH_PAGE;
        graph.stale = true;
        self.sync_graphs(cx);
        cx.notify();
    }

    /// 展开或收起第 `ci` 个提交；展开时在后台读它改了什么。
    fn toggle_commit(&mut self, root: &Path, ci: usize, cx: &mut Context<Self>) {
        let id = self.workspace().id;
        let project = &mut self.workspace_mut().project;
        let Some(commit) = project.git_panel.repos.get(root).and_then(|repo| repo.graph.commit(ci)).cloned() else {
            return;
        };
        let Some(repo) =
            project.git.as_ref().and_then(|git| git.iter().find(|repo| repo.root == root)).map(git::Snapshot::repo)
        else {
            return;
        };
        let load = project.git_panel.toggle_commit(root, &commit.id, project.git.as_ref());
        cx.notify();
        if !load {
            return;
        }
        let root = root.to_path_buf();
        let job = cx.background_spawn(async move {
            let changes = repo.commit_changes(&commit);
            (commit.id, changes)
        });
        cx.spawn(async move |this, cx| {
            let (commit, changes) = job.await;
            this.update(cx, |this, cx| {
                let Some(workspace) = this.workspaces.iter_mut().find(|workspace| workspace.id == id) else {
                    return;
                };
                let project = &mut workspace.project;
                let graph = &mut project.git_panel.repo_mut(&root).graph;
                // 读的时候又收起来了就不要了。
                if !graph.expanded.contains(&commit) {
                    return;
                }
                let changes = match changes {
                    Ok(files) => CommitChanges::Ready(files),
                    Err(_) => CommitChanges::Failed,
                };
                graph.changes.insert(commit, changes);
                project.git_panel.rebuild(project.git.as_ref());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 提交的右键菜单：复制提交号、切到这个提交、从这里新建分支。
    pub(super) fn git_commit_action(&mut self, action: &GitCommitAction, window: &mut Window, cx: &mut Context<Self>) {
        let root = action.repo.clone();
        let id = action.id.clone();
        match action.op {
            CommitOp::CopyId => cx.write_to_clipboard(ClipboardItem::new_string(id)),
            CommitOp::Checkout => {
                let short: String = id.chars().take(7).collect();
                let answer = window.prompt(
                    PromptLevel::Info,
                    &rust_i18n::t!("git.graph.checkout_title", id = short),
                    Some(&rust_i18n::t!("git.graph.checkout_detail")),
                    &[&*rust_i18n::t!("git.graph.checkout_confirm"), &*rust_i18n::t!("git.cancel")],
                    cx,
                );
                cx.spawn_in(window, async move |this, cx| {
                    if answer.await.ok() == Some(0) {
                        this.update_in(cx, |this, window, cx| {
                            this.run_git(
                                &root,
                                Busy::Checkout,
                                window,
                                cx,
                                move |repo| repo.checkout_detached(&id),
                                |_, (), _, _| {},
                            );
                        })
                        .ok();
                    }
                })
                .detach();
            }
            CommitOp::CreateBranch => self.open_branch_picker(&root, true, Some(id), window, cx),
        }
    }

    fn open_commit_menu_at(
        &mut self,
        position: gpui::Point<Pixels>,
        root: PathBuf,
        id: String,
        cx: &mut Context<Self>,
    ) {
        let enabled = self.workspace().project.git_panel.busy(&root).is_none();
        let item = |key: &str, op, enabled| {
            let action = GitCommitAction { repo: root.clone(), id: id.clone(), op };
            Some(menu_item(key, Box::new(action), enabled, cx))
        };
        let items = vec![
            item("git.graph.copy_id", CommitOp::CopyId, true),
            None,
            item("git.graph.checkout", CommitOp::Checkout, enabled),
            item("git.graph.create_branch", CommitOp::CreateBranch, enabled),
        ];
        let target = self.git_focus.clone();
        self.open_menu(position, items, target, cx);
    }

    /// 根目录是 `root` 的仓库的图表状态。
    fn graph(&self, root: &Path) -> Option<&Graph> {
        self.workspace().project.git_panel.repos.get(root).map(|repo| &repo.graph)
    }

    /// 展开的提交下面那几行左边的线：第 `ci` 个提交往下走的 lane 接着画实线穿过去，下面的提交才
    /// 连得上。叠在行的左边，不占地方，行里的内容照旧缩进。
    fn lanes_below(&self, root: &Path, ci: usize) -> Option<impl IntoElement> {
        let graph = self.graph(root)?;
        let row = graph.history.as_ref()?.as_ref().ok()?.rows.get(ci)?;
        let (lane, width) = lane_geometry(graph.lanes);
        Some(div().absolute().left(px(8.)).top_0().h_full().child(lanes_tail(row, lane, width, false)))
    }

    /// 图表里的一个提交：左边的线和点，引用标签，说明首行，行尾多久以前。
    pub(super) fn render_commit(
        &self,
        ix: usize,
        repo: &git::Snapshot,
        ci: usize,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(graph) = self.graph(&repo.root) else {
            return div().into_any_element();
        };
        let history = graph.history.as_ref().and_then(|history| history.as_ref().ok());
        let (Some(commit), Some(row)) = (graph.commit(ci), history.and_then(|history| history.rows.get(ci))) else {
            return div().into_any_element();
        };
        let (lane, width) = lane_geometry(graph.lanes);
        let dim = hsla(fg).opacity(0.5);
        let head = commit.refs.iter().any(|r| matches!(r.kind, RefKind::Head | RefKind::CurrentBranch));
        let mut detail = format!("{} · {} · {}", commit.author, commit.date, commit.short_id());
        if !commit.refs.is_empty() {
            let names: Vec<_> = commit.refs.iter().map(|r| r.name.as_str()).collect();
            detail.push_str(&format!("\n{}", names.join(", ")));
        }
        let tip = format!("{}\n{detail}", commit.subject);
        let expanded = graph.expanded.contains(&commit.id);
        let root = repo.root.clone();
        let id = commit.id.clone();
        let fit = fit_commit_row(self.workspace().project.git_panel.width, width, commit);
        let labels = ref_labels(commit, fit.labels, fg, bg);
        self.git_row(("git-commit", ix), 0., fg, bg)
            .role(Role::TreeItem)
            .aria_level(1)
            .aria_expanded(expanded)
            .aria_label(commit.subject.clone())
            .aria_description(detail)
            .tooltip(tooltip(tip, None, fg, bg))
            .child(lanes_canvas(row.clone(), lane, width, commit.parents.len() > 1, head, bg))
            .children(labels)
            .child(
                div()
                    .flex_1()
                    .min_w(px(40.))
                    .truncate()
                    .when(head, |subject| subject.font_weight(gpui::FontWeight::SEMIBOLD))
                    .child(commit.subject.clone()),
            )
            .when(fit.date, |row| {
                row.child(
                    div()
                        .flex_none()
                        .max_w(px(DATE_WIDTH))
                        .truncate()
                        .text_size(px(11.))
                        .text_color(dim)
                        .child(short_date(&commit.date)),
                )
            })
            .on_press_down(cx, {
                let root = root.clone();
                move |this, _, cx| this.toggle_commit(&root, ci, cx)
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_commit_menu_at(event.position, root.clone(), id.clone(), cx);
                }),
            )
            .into_any_element()
    }

    /// 展开的提交下面它改的一个文件：状态字母、文件名、所在目录和加减了多少行。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_commit_file(
        &self,
        ix: usize,
        repo: &git::Snapshot,
        ci: usize,
        fi: usize,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(graph) = self.graph(&repo.root) else {
            return div().into_any_element();
        };
        let (Some(commit), Some(file)) = (graph.commit(ci), graph.file(ci, fi)) else {
            return div().into_any_element();
        };
        let (_, width) = lane_geometry(graph.lanes);
        let panel = &self.workspace().project.git_panel;
        let depth = panel.graph_depth.get(ix).copied().unwrap_or(0) as f32;
        let tree = panel.tree;
        let name = base_name(&file.path);
        // 以树形式查看时所在目录已经在上面的行里了，不再写。
        let dir = if tree {
            String::new()
        } else {
            file.path.parent().map(|dir| dir.display().to_string()).unwrap_or_default()
        };
        let dim = hsla(fg).opacity(0.5);
        let side = DiffSide::Commit { id: commit.id.clone(), parent: commit.parents.first().cloned() };
        let target =
            DiffTarget { root: repo.root.clone(), rel: file.path.clone(), old_rel: file.old_path.clone(), side };
        let open = target.clone();
        self.git_row(("git-commit-file", ix), 0., fg, bg)
            .role(Role::TreeItem)
            .aria_level(depth as usize + 2)
            .aria_label(name.clone())
            .aria_description(file_description(file, &dir))
            .relative()
            .children(self.lanes_below(&repo.root, ci))
            .pl(px(8. + width + INDENT * (depth + 1.)))
            .when(tree, |row| row.child(div().flex_none().w(px(12.))))
            .child(img(file_icon(&name)).flex_none().size(px(14.)))
            .child(div().flex_initial().min_w_0().truncate().child(name))
            .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(dim).child(dir))
            .when(file.added > 0, |row| row.child(added_label(file.added)))
            .when(file.removed > 0, |row| row.child(removed_label(file.removed)))
            .child(status_letter(file.status))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.click_diff(target.clone(), event.click_count, cx);
                }),
            )
            .on_a11y_action(AccessibleAction::Click, a11y_open_diff(open, cx))
            .into_any_element()
    }

    /// 以树形式查看时展开的提交下面的一个目录，点了展开收起。
    pub(super) fn render_commit_dir(
        &self,
        ix: usize,
        di: usize,
        ci: usize,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = &self.workspace().project.git_panel;
        let Some(dir) = panel.dirs.get(di) else {
            return div().into_any_element();
        };
        let depth = panel.graph_depth.get(ix).copied().unwrap_or(0) as f32;
        let width = self.graph(&dir.root).map_or(0., |graph| lane_geometry(graph.lanes).1);
        let last = base_name(&dir.path);
        self.git_row(("git-commit-dir", ix), 0., fg, bg)
            .role(Role::TreeItem)
            .aria_level(depth as usize + 2)
            .aria_expanded(dir.expanded)
            .aria_label(dir.name.clone())
            .relative()
            .children(self.lanes_below(&dir.root, ci))
            .pl(px(8. + width + INDENT * (depth + 1.)))
            .child(chevron(dir.expanded, fg))
            .child(img(folder_icon(&last, dir.expanded)).flex_none().size(px(14.)))
            .child(div().flex_1().min_w_0().truncate().child(dir.name.clone()))
            .on_press_down(cx, move |this, _, cx| {
                let project = &mut this.workspace_mut().project;
                project.git_panel.toggle_dir(di, project.git.as_ref());
                cx.notify();
            })
            .into_any_element()
    }

    /// 图表里不是提交的那一行。读到了提交以后的「加载更多」和「正在读取」接在最后一个提交下面：
    /// 还往下走的 lane 画成虚线，文字和说明首行对齐；「加载更多」是带向下箭头的链接，点了多读
    /// 一页。还没有提交时的说明缩进写在开头。
    pub(super) fn render_graph_note(
        &self,
        ix: usize,
        repo: &git::Snapshot,
        note: GraphNote,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = match note {
            GraphNote::Loading => rust_i18n::t!("git.graph.loading"),
            GraphNote::Failed => rust_i18n::t!("git.graph.failed"),
            GraphNote::Empty => rust_i18n::t!("git.no_commits"),
            GraphNote::More => rust_i18n::t!("git.graph.more"),
        };
        let graph = self.graph(&repo.root);
        let last = graph
            .and_then(|graph| graph.history.as_ref()?.as_ref().ok()?.rows.last())
            .map(|row| (row, lane_geometry(graph.map_or(0, |graph| graph.lanes))));
        // 「加载更多」报成按钮，其余是一句说明。
        let row = self
            .git_row(("git-graph-note", ix), 0., fg, bg)
            .role(if note == GraphNote::More { Role::Button } else { Role::Label })
            .aria_label(text.clone().into_owned());
        let Some((last, (lane, width))) = last else {
            return row
                .pl(px(8. + INDENT))
                .italic()
                .text_color(hsla(fg).opacity(0.45))
                .child(text.into_owned())
                .into_any_element();
        };
        let row = row.child(lanes_tail(last, lane, width, true));
        if note != GraphNote::More {
            return row.italic().text_color(hsla(fg).opacity(0.45)).child(text.into_owned()).into_any_element();
        }
        let accent = hsla(RENAMED);
        let root = repo.root.clone();
        row.child(
            div()
                .flex_none()
                .h(px(18.))
                .px(px(6.))
                .ml(px(-6.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .gap(px(4.))
                .text_color(accent)
                .group_hover(GRAPH_MORE_GROUP, |link| link.bg(accent.opacity(0.15)))
                .child(svg().flex_none().path(ARROW_DOWN_ICON).size(px(12.)).text_color(accent))
                .child(text.into_owned()),
        )
        .group(GRAPH_MORE_GROUP)
        .cursor_pointer()
        .on_press_down(cx, move |this, _, cx| this.load_more_commits(&root, cx))
        .into_any_element()
    }

    /// 展开的提交下面不列文件时的说明。
    pub(super) fn render_commit_note(
        &self,
        ix: usize,
        repo: &git::Snapshot,
        ci: usize,
        note: CommitNote,
        fg: Rgb,
        bg: Rgb,
    ) -> AnyElement {
        let text = match note {
            CommitNote::Loading => rust_i18n::t!("git.graph.loading_changes"),
            CommitNote::Failed => rust_i18n::t!("git.graph.changes_failed"),
            CommitNote::Empty => rust_i18n::t!("git.graph.no_files"),
        };
        let width = self.graph(&repo.root).map_or(0., |graph| lane_geometry(graph.lanes).1);
        self.git_row(("git-commit-note", ix), 0., fg, bg)
            .role(Role::Label)
            .aria_label(text.clone().into_owned())
            .relative()
            .children(self.lanes_below(&repo.root, ci))
            .pl(px(8. + width + INDENT))
            .italic()
            .text_color(hsla(fg).opacity(0.45))
            .child(text.into_owned())
            .into_any_element()
    }
}

/// 一行提交里放得下什么：显示几个引用标签，显不显示日期。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fit {
    labels: usize,
    date: bool,
}

/// 估一个引用标签多宽。
fn label_width(name: &str) -> f32 {
    (name.chars().count() as f32 * 6. + 12.).min(MAX_LABEL_WIDTH)
}

/// 面板 `panel` 宽、图 `graph` 宽时这一行放得下什么：说明首行至少留 `MIN_SUBJECT_WIDTH`，不够时
/// 先不显示日期，再从后往前少显示标签（少了的合进「+N」）。面板宽度还不知道时都显示。
fn fit_commit_row(panel: f32, graph: f32, commit: &Commit) -> Fit {
    let shown = commit.refs.len().min(MAX_REF_LABELS);
    if panel <= 0. {
        return Fit { labels: shown, date: true };
    }
    // 两边留白 16，各部分之间的间隔 6。
    let room = panel - 16. - graph - 6.;
    let labels = |count: usize| {
        let more = if commit.refs.len() > count { MORE_LABEL_WIDTH + 6. } else { 0. };
        commit.refs.iter().take(count).map(|r| label_width(&r.name) + 6.).sum::<f32>() + more
    };
    if room - labels(shown) - DATE_WIDTH - 6. >= MIN_SUBJECT_WIDTH {
        return Fit { labels: shown, date: true };
    }
    let count = (0..=shown).rev().find(|&count| room - labels(count) >= MIN_SUBJECT_WIDTH).unwrap_or(0);
    Fit { labels: count, date: false }
}

/// `2 days ago` 这样的相对时间缩成 `2 days`，行尾放得下；tooltip 里是完整的。
fn short_date(date: &str) -> String {
    date.strip_suffix(" ago").unwrap_or(date).to_owned()
}

/// 指向提交的引用的前 `shown` 个小标签：HEAD 所在的分支（或分离的 HEAD）实心突出，本地分支描边，
/// 远端分支淡一些，tag 带黄色；太长的截断。其余的合成「+N」。
fn ref_labels(commit: &Commit, shown: usize, fg: Rgb, bg: Rgb) -> Vec<gpui::Div> {
    let fg_hsla = hsla(fg);
    let mut labels: Vec<_> = commit
        .refs
        .iter()
        .take(shown)
        .map(|r| {
            let label = div()
                .flex_none()
                .max_w(px(MAX_LABEL_WIDTH))
                .h(px(16.))
                .px(px(5.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .text_size(px(10.))
                .border_1()
                .truncate();
            let label = match r.kind {
                RefKind::Head | RefKind::CurrentBranch => label
                    .bg(hsla(RENAMED))
                    .border_color(hsla(RENAMED))
                    .text_color(gpui::white())
                    .font_weight(gpui::FontWeight::SEMIBOLD),
                RefKind::Branch => label.border_color(fg_hsla.opacity(0.35)).text_color(fg_hsla),
                RefKind::Remote => label.border_color(fg_hsla.opacity(0.2)).text_color(fg_hsla.opacity(0.6)),
                RefKind::Tag => {
                    label.bg(hsla(bg.mix(MODIFIED, 0.18))).border_color(hsla(MODIFIED).opacity(0.5)).text_color(fg_hsla)
                }
            };
            label.child(r.name.clone())
        })
        .collect();
    if commit.refs.len() > shown {
        labels.push(
            div()
                .flex_none()
                .text_size(px(10.))
                .text_color(fg_hsla.opacity(0.5))
                .child(format!("+{}", commit.refs.len() - shown)),
        );
    }
    labels
}

/// 提交 `last` 下面不是提交的那一行的线：`last` 底下还往下走的 lane 竖着穿过去。`dashed` 时画成
/// 淡色的虚线，接在最后一个提交下面表示后面还有；否则是实线，接到下面的提交。
fn lanes_tail(last: &GraphRow, lane: f32, width: f32, dashed: bool) -> impl IntoElement {
    let mut lanes: Vec<_> = last
        .lines
        .iter()
        .filter(|line| line.half == Half::Bottom)
        .map(|line| (line.to, LANE_COLORS[line.color % LANE_COLORS.len()]))
        .collect();
    lanes.sort_by_key(|&(column, _)| column);
    lanes.dedup_by_key(|&mut (column, _)| column);
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let left = f32::from(bounds.origin.x);
            let top = f32::from(bounds.origin.y);
            let height = f32::from(bounds.size.height);
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                for &(column, color) in &lanes {
                    let x = left + lane * (column as f32 + 0.5) - 0.75;
                    if !dashed {
                        let line = Bounds::new(point(px(x), px(top)), gpui::size(px(1.5), px(height)));
                        window.paint_quad(fill(line, hsla(color)));
                        continue;
                    }
                    let mut y = top;
                    while y < top + height {
                        let dash = Bounds::new(point(px(x), px(y)), gpui::size(px(1.5), px(3.)));
                        window.paint_quad(fill(dash, hsla(color).opacity(0.5)));
                        y += 5.;
                    }
                }
            });
        },
    )
    .flex_none()
    .w(px(width))
    .h_full()
}

/// 一行的线和点。线：上半段从行顶连到点的高度，下半段从点的高度连到行底，换列的用曲线连。点：
/// 合并提交是空心的，HEAD 大一圈。超出图宽的 lane 截掉，点贴在右边界上。
fn lanes_canvas(row: GraphRow, lane: f32, width: f32, merge: bool, head: bool, bg: Rgb) -> impl IntoElement {
    let color = |index: usize| hsla(LANE_COLORS[index % LANE_COLORS.len()]);
    let bg = hsla(bg);
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let left = f32::from(bounds.origin.x);
            let top = f32::from(bounds.origin.y);
            let height = f32::from(bounds.size.height);
            let x = |column: usize| left + lane * (column as f32 + 0.5);
            let (mid, bottom) = (top + height / 2., top + height);
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                for line in &row.lines {
                    let (y0, y1) = match line.half {
                        Half::Top => (top, mid),
                        Half::Bottom => (mid, bottom),
                    };
                    let (x0, x1) = (x(line.from), x(line.to));
                    let mut path = PathBuilder::stroke(px(1.5));
                    path.move_to(point(px(x0), px(y0)));
                    if line.from == line.to {
                        path.line_to(point(px(x1), px(y1)));
                    } else {
                        let ym = (y0 + y1) / 2.;
                        path.cubic_bezier_to(point(px(x1), px(y1)), point(px(x0), px(ym)), point(px(x1), px(ym)));
                    }
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color(line.color));
                    }
                }
                let radius = if head { 4.5 } else { 3.5 };
                let cx = x(row.column).min(left + width - radius);
                let dot =
                    Bounds::new(point(px(cx - radius), px(mid - radius)), gpui::size(px(radius * 2.), px(radius * 2.)));
                let dot_color: Hsla = color(row.color);
                let fill = if merge && !head { bg } else { dot_color };
                window.paint_quad(quad(dot, px(radius), fill, px(1.5), dot_color, BorderStyle::default()));
            });
        },
    )
    .flex_none()
    .w(px(width))
    .h_full()
}

#[cfg(test)]
mod tests {
    use super::*;
    use runode_git::CommitRef;

    fn commit(refs: &[&str]) -> Commit {
        Commit {
            id: "a".repeat(40),
            parents: Vec::new(),
            subject: "subject".into(),
            author: String::new(),
            date: "2 days ago".into(),
            refs: refs.iter().map(|&name| CommitRef { name: name.into(), kind: RefKind::Branch }).collect(),
        }
    }

    #[test]
    fn narrow_rows_drop_the_date_then_labels() {
        let two = commit(&["main", "origin/main"]);
        assert_eq!(fit_commit_row(320., 20., &two), Fit { labels: 2, date: true });
        // 窄了先不要日期，再少显示标签，说明首行总留着最小宽度。
        assert_eq!(fit_commit_row(250., 20., &two), Fit { labels: 2, date: false });
        assert_eq!(fit_commit_row(200., 20., &two), Fit { labels: 1, date: false });
        assert_eq!(fit_commit_row(140., 20., &two), Fit { labels: 0, date: false });
        // 很长的分支名按最大宽度算，会被截断。
        let long = commit(&[&"x".repeat(60)]);
        assert_eq!(label_width(&long.refs[0].name), MAX_LABEL_WIDTH);
        assert_eq!(fit_commit_row(0., 20., &long), Fit { labels: 1, date: true });
        assert_eq!(short_date("2 days ago"), "2 days");
    }

    #[test]
    fn keeps_room_for_the_changes_above() {
        // 没拖过时占四成；拖得太高给上面的改动留够地方，太矮不矮于下限。
        assert_eq!(graph_height(None, 600.), 240.);
        assert_eq!(graph_height(Some(550.), 600.), 480.);
        assert_eq!(graph_height(Some(10.), 600.), GRAPH_MIN_HEIGHT);
        // 窗口矮得两头都顾不上时保住图表的下限。
        assert_eq!(graph_height(None, 150.), GRAPH_MIN_HEIGHT);
    }

    #[test]
    fn squeezes_many_lanes() {
        assert_eq!(lane_geometry(3), (LANE_WIDTH, 30.));
        assert_eq!(lane_geometry(16), (5., 80.));
        assert_eq!(lane_geometry(40), (MIN_LANE_WIDTH, MAX_GRAPH_WIDTH));
    }
}
