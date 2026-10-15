//! 把右侧的预览栏接到窗口上。预览栏本身在 `preview_panel`：一个窗口一个 `PreviewPanel`，各 workspace
//! 的标签都在它里面。这里每次对它做什么之前告诉它当前是哪个 workspace、交给它那个 workspace 最近读到的 git
//! 状态，画的时候交给它字体、栏宽和配置里的预览字号（颜色它自己从主题读）；它要窗口办的事（文件树定位、
//! 出现收起和放大还原时重排窗口、开关变了要存档、把路径打进终端、按块暂存）经 `PreviewEvent` 交回来。自动
//! 换行和 Markdown 显示源码的开关在它身上，随窗口存档。

use std::path::{Path, PathBuf};

use crate::preview_panel::{Input, PreviewEvent, PreviewPanel};
use gpui::{AnyElement, App, Context, Empty, Entity, Focusable as _, IntoElement as _, Window};
use runode_config::PreviewClick;

pub(in crate::window) use crate::preview_panel::DiffTarget;

use super::{WindowView, cards, model::WorkspaceId, project::PANEL_TOGGLES_INSET, status_bar};
use crate::config::AppConfig;

impl WindowView {
    /// 对当前 workspace 的预览栏做 `f`：先告诉它是哪个 workspace，交给它这个 workspace 最近读到的 git 状态。
    pub(super) fn with_preview<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut PreviewPanel, &mut Context<PreviewPanel>) -> R,
    ) -> R {
        let workspace = self.workspace();
        let (id, git) = (workspace.id, workspace.project.git.clone());
        self.preview.update(cx, |panel, cx| {
            panel.set_workspace(id);
            panel.set_git(id, git);
            f(panel, cx)
        })
    }

    /// 当前 workspace 有开着的预览标签。
    pub(super) fn preview_shown(&self, cx: &App) -> bool {
        self.preview.read(cx).shown(self.workspace().id)
    }

    /// 预览栏放大着，盖在终端区上。
    pub(super) fn preview_maximized(&self, cx: &App) -> bool {
        self.preview.read(cx).maximized(self.workspace().id)
    }

    /// 预览栏在终端区右边占一栏：开着又没放大。
    pub(super) fn preview_in_column(&self, cx: &App) -> bool {
        self.preview_shown(cx) && !self.preview_maximized(cx)
    }

    /// 在预览栏里打开 `path` 并切到它的标签：`pin` 时开成固定标签，否则开成临时标签；已经开着
    /// 时重读一次。文件树跟着定位到它。
    pub(super) fn open_preview(&mut self, path: &Path, pin: bool, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.open(path, pin, cx));
    }

    /// 和 `open_preview` 一样打开 `path`，再选中第 `line` 行（从 0 数）、滚到中间。
    pub(super) fn open_preview_at(&mut self, path: &Path, line: usize, pin: bool, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.open_at(path, line, pin, cx));
    }

    /// Git 面板里点了改动的文件：按配置的 `PreviewClick` 在预览栏里打开它的 diff，单击打开时单击开成
    /// 临时标签、双击固定；双击打开时双击开成固定标签。
    pub(in crate::window) fn click_diff(&mut self, target: DiffTarget, clicks: usize, cx: &mut Context<Self>) {
        let single = cx.global::<AppConfig>().0.file_tree_preview_click == PreviewClick::Single;
        if clicks >= 2 {
            self.with_preview(cx, |panel, cx| panel.open_diff(target, true, cx));
        } else if single {
            self.with_preview(cx, |panel, cx| panel.open_diff(target, false, cx));
        }
        cx.notify();
    }

    /// 关掉 `path` 和它下面的文件的预览标签：文件删掉了。
    pub(super) fn close_previews_under(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.close_under(path, window, cx));
    }

    /// `from` 改了名或挪到了 `to`：它和它下面的文件的预览标签跟过去，当前标签换了就重读。
    pub(super) fn move_previews(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.moved(from, to, cx));
    }

    /// 窗口切回前台、切回这个 workspace 时，当前预览的文件变过就重读；在后台时监听到的改动只等到这时。
    pub(super) fn refresh_preview_if_changed(&mut self, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.refresh_if_changed(cx));
    }

    /// 监听到 `paths` 变了：当前预览的文件在里面就重读。
    pub(super) fn preview_files_changed(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.files_changed(paths, cx));
    }

    /// 监听说不准哪里变了：当前预览跟着文件变的不比修改时间直接重读，工作区 diff 标签记下要重读。
    pub(super) fn preview_all_files_changed(&mut self, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.all_files_changed(cx));
    }

    /// 扫描结果换上以后，当前显示的 diff 标签对应的 git 状态变了就重读。
    pub(in crate::window) fn reload_stale_diff(&mut self, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.reload_stale_diff(cx));
    }

    /// workspace `id` 换上了新读到的 git 状态，交给预览栏重算改动标记。
    pub(super) fn sync_preview_git(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspaces.iter().find(|workspace| workspace.id == id) else {
            return;
        };
        let git = workspace.project.git.clone();
        self.preview.update(cx, |panel, _| panel.set_git(id, git));
    }

    /// 还原当前 workspace 放大着的预览栏：焦点要交给终端了。
    pub(super) fn restore_preview_zoom(&mut self, cx: &mut Context<Self>) {
        let id = self.workspace().id;
        self.preview.update(cx, |panel, _| panel.restore_zoom(id));
    }

    /// workspace `id` 关掉了，预览栏丢掉它的标签。
    pub(super) fn forget_previews(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        self.preview.update(cx, |panel, cx| panel.remove_workspace(id, cx));
    }

    /// 当前 workspace 的预览栏，宽 `width`；`rightmost` 时它贴着窗口右边，标签条给右上角的面板开关让位。
    /// 没有标签时什么也不画。放大着时它自己盖满终端区。预览栏的颜色从主题读，`_fg`、`_bg` 用不着了，留着
    /// 只为不在这一步改窗口里的几处调用。
    pub(super) fn render_preview(&mut self, width: f32, rightmost: bool, cx: &mut Context<Self>) -> AnyElement {
        if !self.preview_shown(cx) {
            return Empty.into_any_element();
        }
        let config = &cx.global::<AppConfig>().0;
        let input = Input {
            width,
            right_inset: if rightmost { PANEL_TOGGLES_INSET } else { 6. },
            font: self.font_family(cx),
            font_size: config.preview_font_size,
            cards: cards(cx),
            status_bar_height: status_bar::height(cx),
        };
        self.with_preview(cx, |panel, _| panel.set_input(input));
        self.preview.clone().into_any_element()
    }

    /// 预览栏要窗口办的事。
    pub(super) fn on_preview_event(
        &mut self,
        _: &Entity<PreviewPanel>,
        event: &PreviewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PreviewEvent::Opened => {
                self.sync_project_watch(cx);
                self.refresh_project(cx);
                cx.notify();
            }
            PreviewEvent::Closed { focused } => {
                self.sync_project_watch(cx);
                // 焦点在预览栏上的交回终端。
                if *focused {
                    window.focus(&self.focus_handle(cx), cx);
                }
                cx.notify();
            }
            PreviewEvent::ZoomChanged => cx.notify(),
            PreviewEvent::Reveal(path) => self.with_file_tree(cx, |tree, cx| tree.reveal_file(path, cx)),
            PreviewEvent::OptionsChanged => self.save(cx),
            PreviewEvent::InsertPath { path, line } => self.insert_path(path, Some(*line), window, cx),
            // 经 Git 面板跑，和面板上的操作一起按仓库排队；做完它发 `GitPanelEvent::HunkApplied`，再重读 diff。
            PreviewEvent::ApplyHunk { root, file, hunk, action } => {
                self.with_git_panel(cx, |panel, cx| panel.apply_hunk(root, file.clone(), *hunk, *action, window, cx));
            }
        }
    }
}
