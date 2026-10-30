//! 把预览栏接到窗口上。预览栏本身在 `preview_panel`：一个窗口一个 `PreviewPanel`，各 workspace
//! 的文件标签都在它里面。文件标签和终端标签一起排在标题栏里（`titlebar` 照 `PreviewPanel::file_tabs` 画），
//! 点中文件标签时预览盖满终端区，切到终端标签时退到后面。这里每次对它做什么之前告诉它当前是哪个
//! workspace、交给它那个 workspace 最近读到的 git 状态，画的时候交给它字体、宽度和配置里的预览字号（颜色
//! 它自己从主题读）；它要窗口办的事（文件树定位、出现收起和到最前时重排窗口、开关变了要存档、把路径打进
//! 终端、按块暂存）经 `PreviewEvent` 交回来。自动换行和 Markdown 显示源码的开关在它身上，随窗口存档。

use std::path::{Path, PathBuf};

use crate::preview_panel::{Input, PreviewEvent, PreviewPanel};
use gpui::{
    AnyElement, App, Context, Empty, Entity, Focusable as _, IntoElement as _, ParentElement as _, Pixels, Point,
    StyleRefinement, Styled as _, Window, div,
};
use runode_config::PreviewClick;

pub(in crate::window) use crate::preview_panel::DiffTarget;

use super::{WindowView, cards, model::WorkspaceId, status_bar};
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
            panel.set_workspace(id, cx);
            panel.set_git(id, git, cx);
            f(panel, cx)
        })
    }

    /// 当前 workspace 有开着的文件标签。
    pub(super) fn preview_shown(&self, cx: &App) -> bool {
        self.preview.read(cx).shown(self.workspace().id)
    }

    /// 当前 workspace 的文件标签在最前，预览盖在终端区上；看板、手机端引导页这些铺满时让给它们。
    pub(super) fn preview_front(&self, cx: &App) -> bool {
        !self.page_covers_terminals() && self.preview.read(cx).front(self.workspace().id)
    }

    /// 切到当前 workspace 的第 `ix` 个文件标签，预览盖到终端区上。开着的看板、手机端引导页先收起；
    /// 看板不让走时不切，返回假。
    pub(super) fn show_file_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.leave_pages(window, cx) {
            return false;
        }
        self.with_preview(cx, |panel, cx| panel.show_tab(ix, window, cx));
        self.reveal_front_tab(cx);
        cx.notify();
        true
    }

    /// 收起盖着终端区的看板、手机端引导页，好让预览露出来；看板不让走时返回假。
    fn leave_pages(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.leave_todo_page(window, cx) {
            return false;
        }
        self.leave_mobile(cx);
        self.kanban = None;
        true
    }

    /// 标签条滚到露出当前的标签。
    fn reveal_front_tab(&self, cx: &App) {
        if let Some(front) = self.front_tab(cx) {
            super::model::reveal_tab(&self.workspace().tab_scroll, front, self.titlebar_tab_count(cx));
        }
    }

    /// 关掉当前 workspace 的第 `ix` 个文件标签。
    pub(super) fn close_file_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.close_tab(ix, window, cx));
    }

    /// 把当前 workspace 的第 `ix` 个文件标签固定下来。
    pub(super) fn pin_file_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.pin_tab(ix, cx));
    }

    /// 把当前 workspace 的第 `from` 个文件标签挪到第 `to` 个位置并切过去。
    pub(super) fn move_file_tab(&mut self, from: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.leave_pages(window, cx) {
            return;
        }
        self.with_preview(cx, |panel, cx| panel.move_tab(from, to, window, cx));
        self.reveal_front_tab(cx);
        cx.notify();
    }

    /// 切到当前 workspace 的第 `ix` 个文件标签，在 `position` 弹出它的右键菜单。
    pub(super) fn open_file_tab_menu(
        &mut self,
        ix: usize,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.show_file_tab(ix, window, cx) {
            self.with_preview(cx, |panel, cx| panel.open_tab_menu(position, cx));
        }
    }

    /// 在预览栏里打开 `path` 并切到它的标签：`pin` 时开成固定标签，否则开成临时标签；已经开着
    /// 时重读一次。文件树跟着定位到它。`edit` 时读完是能编辑的文本就进编辑模式。
    pub(super) fn open_preview(&mut self, path: &Path, pin: bool, edit: bool, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.open(path, pin, edit, cx));
        self.reveal_front_tab(cx);
    }

    /// 和 `open_preview` 一样打开 `path`，再选中第 `line` 行（从 0 数）、滚到中间。
    pub(super) fn open_preview_at(&mut self, path: &Path, line: usize, pin: bool, edit: bool, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.open_at(path, line, pin, edit, cx));
        self.reveal_front_tab(cx);
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
        self.reveal_front_tab(cx);
        cx.notify();
    }

    /// 关掉各 workspace 里 `path` 和它下面的文件的预览标签：文件删掉了。
    pub(super) fn close_previews_under(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.close_under(path, window, cx));
    }

    /// `from` 改了名或挪到了 `to`：各 workspace 里它和它下面的文件的预览标签跟过去，当前标签换了就重读。
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
        self.preview.update(cx, |panel, cx| panel.set_git(id, git, cx));
    }

    /// 当前 workspace 的预览退到终端后面：切到了终端标签，或焦点要交给终端了。
    pub(super) fn send_preview_back(&mut self, cx: &mut Context<Self>) {
        let id = self.workspace().id;
        self.preview.update(cx, |panel, _| panel.send_back(id));
    }

    /// 焦点交给当前的终端，盖着终端区的文件先退到后面。
    pub(super) fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview_front(cx) {
            self.send_preview_back(cx);
            cx.notify();
        }
        window.focus(&self.focus_handle(cx), cx);
    }

    /// workspace `id` 关掉了，预览栏丢掉它的标签。
    pub(super) fn forget_previews(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        self.preview.update(cx, |panel, cx| panel.remove_workspace(id, cx));
    }

    /// 当前 workspace 的预览，宽 `width`，盖满终端区。没有标签时什么也不画。
    ///
    /// 缓存：它盖着的终端一输出窗口就重画，预览只在自己的标签、编辑、高亮这些变了时重画。辅助工具开着时
    /// 不缓存，理由同 `cached_slot`。
    pub(super) fn render_preview(&mut self, width: f32, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.preview_shown(cx) {
            return Empty.into_any_element();
        }
        let config = &cx.global::<AppConfig>().0;
        let input = Input {
            width,
            font: self.font_family(cx),
            font_size: config.preview_font_size,
            cards: cards(cx),
            status_bar_height: status_bar::height(cx),
        };
        if self.with_preview(cx, |panel, _| panel.set_input(input)) {
            // 画的时候发的通知不会再要一帧，等这一帧画完再通知。
            let preview = self.preview.clone();
            cx.defer(move |cx| preview.update(cx, |_, cx| cx.notify()));
        }
        let preview = if window.is_a11y_active() {
            self.preview.clone().into_any_element()
        } else {
            self.preview.clone().cached(StyleRefinement::default().size_full()).into_any_element()
        };
        div().absolute().top_0().left_0().size_full().child(preview).into_any_element()
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
            PreviewEvent::Fronted => cx.notify(),
            PreviewEvent::Reveal(path) => self.with_file_tree(cx, |tree, cx| tree.reveal_file(path, cx)),
            PreviewEvent::OptionsChanged => self.save(cx),
            PreviewEvent::InsertPath { path, lines } => {
                self.send_preview_back(cx);
                cx.notify();
                self.insert_paths(vec![path.clone()], *lines, window, cx);
            }
            PreviewEvent::Quote(text) => {
                if let Some(view) = self.focused_view().cloned() {
                    // 预览盖着终端，退到后面才看得到贴进去的字。
                    self.send_preview_back(cx);
                    cx.notify();
                    window.focus(&view.focus_handle(cx), cx);
                    view.update(cx, |view, cx| view.paste_text(text.clone(), window, cx));
                }
            }
            // 经 Git 面板跑，和面板上的操作一起按仓库排队；做完它发 `GitPanelEvent::HunkApplied`，再重读 diff。
            PreviewEvent::ApplyHunk { root, file, hunk, action } => {
                self.with_git_panel(cx, |panel, cx| panel.apply_hunk(root, file.clone(), *hunk, *action, window, cx));
            }
        }
    }
}
