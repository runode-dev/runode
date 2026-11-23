//! 把预览栏接到窗口上。预览栏本身在 `preview_panel`：一个窗口一个 `PreviewPanel`，各 workspace
//! 的文件标签都在它里面。文件标签和终端标签一起排在标题栏里，
//! 点中文件标签时预览盖满终端区，切到终端标签时退到后面；把文件标签拖到终端区靠边处，预览停靠在那一侧、和终端
//! 并排，拖到终端区正中或拖回标题栏又盖满终端区；拖到停靠着的预览靠边处，预览分成两块。这里每次对它做什么之前告诉它当前是哪个
//! workspace、交给它那个 workspace 最近读到的 git 状态，画的时候交给它字体、宽度和配置里的预览字号（颜色
//! 它自己从主题读）；它要窗口办的事（文件树定位、出现收起和到最前时重排窗口、开关变了要存档、把路径打进
//! 终端、按块暂存）经 `PreviewEvent` 交回来。自动换行和 Markdown 显示源码的开关在它身上，随窗口存档。

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use crate::preview_panel::{Dock, Input, PreviewEvent, PreviewPanel};
use crate::ui::{
    panel::{DIVIDER_GRAB_WIDTH, split_handle},
    theme::ActiveTheme as _,
};
use gpui::{
    AnyElement, App, Context, DragMoveEvent, Empty, Entity, EntityId, Focusable as _, Pixels, Point, Stateful, Window,
    canvas, div, prelude::*, px, relative,
};
use runode_config::PreviewClick;
use runode_types::pane::{Axis, Direction};

pub(in crate::window) use crate::preview_panel::DiffTarget;

use super::{
    Divider, RATIO_STEP, WindowView, cached_page, cards, covered_panes,
    pane_move::{DropZone, zone_at, zone_overlay},
    status_bar,
    titlebar::DraggedFileTab,
};
use crate::WorkspaceId;
use crate::{config::AppConfig, project_host::ProjectHost};

impl WindowView {
    /// 对当前 workspace 的预览栏做 `f`：先告诉它是哪个 workspace，交给它这个 workspace 最近读到的 git 状态。
    pub(super) fn with_preview<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut PreviewPanel, &mut Context<PreviewPanel>) -> R,
    ) -> R {
        let workspace = self.workspace();
        let (id, git, project) = (workspace.id, workspace.project.git.clone(), ProjectHost::of(&workspace.host));
        self.preview.update(cx, |panel, cx| {
            panel.set_workspace(id, project, cx);
            panel.set_git(id, git, cx);
            f(panel, cx)
        })
    }

    /// 当前 workspace 有开着的文件标签。
    pub(super) fn preview_shown(&self, cx: &App) -> bool {
        self.preview.read(cx).shown(self.workspace().id)
    }

    /// 当前 workspace 的文件标签在最前，预览盖在终端区上；看板、配对设备页这些铺满时让给它们。
    pub(super) fn preview_front(&self, cx: &App) -> bool {
        !self.page_covers_terminals() && self.preview.read(cx).front(self.workspace().id)
    }

    /// 切到当前 workspace 的第 `ix` 个文件标签，预览盖到终端区上。开着的看板、配对设备页先收起；
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

    /// 把当前 workspace 的第 `from` 个文件标签挪到第 `to` 个位置并切过去；挪了位置时停靠着的预览改回盖住终端区。
    pub(super) fn move_file_tab(&mut self, from: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.leave_pages(window, cx) {
            return;
        }
        self.with_preview(cx, |panel, cx| {
            // 放回自己身上多半是点标签时手抖，不算拖回来。
            if from != to {
                panel.set_dock(None, cx);
            }
            panel.move_tab(from, to, window, cx);
        });
        self.reveal_front_tab(cx);
        cx.notify();
    }

    /// 停靠着的预览改回盖住终端区，切到第 `ix` 个文件标签。
    pub(super) fn undock_preview(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview_dock(cx).is_some() {
            self.with_preview(cx, |panel, cx| panel.set_dock(None, cx));
            self.show_file_tab(ix, window, cx);
        }
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
    /// 时重读一次。文件树跟着定位到它。
    pub(super) fn open_preview(&mut self, path: &Path, pin: bool, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.open(path, pin, cx));
        self.reveal_front_tab(cx);
    }

    /// 在预览栏里打开 `path` 并切到它的标签，再选中第 `line` 行（从 0 数）、滚到中间；`hit` 是这一行里要选中的
    /// 字节范围，给了时只选中那段。
    pub(super) fn open_preview_at(
        &mut self,
        path: &Path,
        line: usize,
        hit: Option<Range<usize>>,
        pin: bool,
        cx: &mut Context<Self>,
    ) {
        self.with_preview(cx, |panel, cx| panel.open_at(path, line, hit, pin, cx));
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

    /// 关掉各 workspace 里 `path` 和它下面的文件的预览标签：主机 `host` 上的这个文件删掉了。
    pub(super) fn close_previews_under(
        &mut self,
        path: &Path,
        host: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.with_preview(cx, |panel, cx| panel.close_under(path, host, window, cx));
    }

    /// 主机 `host` 上的 `from` 改了名或挪到了 `to`：各 workspace 里那台主机上它和它下面的文件的预览标签跟过去，
    /// 当前标签换了就重读。
    pub(super) fn move_previews(&mut self, from: &Path, to: &Path, host: Option<&str>, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.moved(from, to, host, cx));
    }

    /// 窗口切回前台、切回这个 workspace 时，当前预览的文件变过就重读；在后台时监听到的改动只等到这时。
    pub(super) fn refresh_preview_if_changed(&mut self, cx: &mut Context<Self>) {
        self.with_preview(cx, |panel, cx| panel.refresh_if_changed(cx));
    }

    /// 「交给 agent」贴给谁：当前分屏没在跑 agent、这个标签里别的分屏在跑时是那个分屏（按布局里的先后取第一个，
    /// 不取开在单独窗口里的），否则为空，贴进当前分屏。贴进一个光是 shell 的分屏，多行的块会被当成命令。
    fn quote_target(&self, cx: &App) -> Option<EntityId> {
        let tab = self.tab()?;
        if tab.focused_view().read(cx).agent().is_some() {
            return None;
        }
        tab.root.leaves().into_iter().find(|id| {
            !tab.popped.contains(id) && tab.panes.get(id).is_some_and(|(view, _)| view.read(cx).agent().is_some())
        })
    }

    /// 「交给 agent」：把 `text` 贴进 `quote_target` 选的分屏，焦点交给它。
    pub(super) fn quote_to_agent(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.quote_target(cx) {
            self.focus_pane_in_active_tab(pane, window, cx);
        }
        if let Some(view) = self.focused_view().cloned() {
            // 预览盖着终端，退到后面才看得到贴进去的字。
            self.send_preview_back(cx);
            cx.notify();
            window.focus(&view.focus_handle(cx), cx);
            view.update(cx, |view, cx| view.paste_text(text, window, cx));
        }
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

    /// 焦点交给当前的终端，在最前的文件先退到后面。
    pub(super) fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview_current(cx) {
            self.send_preview_back(cx);
            cx.notify();
        }
        window.focus(&self.focus_handle(cx), cx);
    }

    /// workspace `id` 关掉了，预览栏丢掉它的标签。
    pub(super) fn forget_previews(&mut self, id: WorkspaceId, cx: &mut Context<Self>) {
        self.preview.update(cx, |panel, cx| panel.forget_workspace(id, cx));
    }

    /// 当前 workspace 的预览在最前：盖着终端区，或者停靠着、是当前的那边；看板、配对设备页这些铺满时为假。
    pub(super) fn preview_current(&self, cx: &App) -> bool {
        !self.page_covers_terminals() && self.preview.read(cx).current(self.workspace().id)
    }

    /// 当前 workspace 的预览停靠在哪；看板、配对设备页这些铺满时为空。
    fn preview_dock(&self, cx: &App) -> Option<Dock> {
        self.preview.read(cx).dock(self.workspace().id).filter(|_| !self.page_covers_terminals())
    }

    /// 终端区：终端（`panes`），预览停靠着时和它并排，宽 `width`；在最前的预览或开着的审阅页盖住整块。停靠的
    /// 预览和终端之间空出 `gap`。拖来的文件标签放在靠边处时预览停靠到那一侧，放在正中时盖住终端区。
    pub(super) fn render_terminal_area(
        &mut self,
        panes: AnyElement,
        width: f32,
        gap: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        let front = self.preview_front(cx).then(|| self.render_preview(width, window, cx));
        let covering = front.or_else(|| self.render_review(window, cx));
        let panes = covered_panes(panes, covering.is_some());
        let body = match self.preview_dock(cx) {
            Some(dock) => self.render_docked_preview(panes, dock, width, gap, window, cx),
            None => panes.into_any_element(),
        };
        let layout = self.layout.clone();
        div()
            .id("terminal-area")
            .relative()
            .size_full()
            .child(body)
            .children(covering)
            .child(
                canvas(move |bounds, _, _| layout.borrow_mut().terminals = Some(bounds), |_, (), _, _| {})
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .when(cx.has_active_drag(), |area| area.child(self.render_file_tab_drop(cx)))
    }

    /// 拖动期间铺在终端区最上面的一层，接拖来的文件标签：盖着的预览挡住了下面的元素，放下的事件到不了它们。
    /// 落在停靠着的预览靠边处时预览分成两块、拖来的标签在那一侧，落在它正中时在当前块里显示；落在别处按终端区
    /// 算，停靠或盖住终端区。
    fn render_file_tab_drop(&self, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let layout = self.layout.borrow();
        let docked = self.preview_dock(cx).and(layout.docked_preview);
        let overlay = self.file_tab_drop.map(|(zone, in_preview)| match (in_preview, docked, layout.terminals) {
            (true, Some(preview), Some(area)) => div()
                .absolute()
                .left(preview.origin.x - area.origin.x)
                .top(preview.origin.y - area.origin.y)
                .w(preview.size.width)
                .h(preview.size.height)
                .child(zone_overlay(zone, cx)),
            _ => div().absolute().inset_0().child(zone_overlay(zone, cx)),
        });
        div()
            .id("file-tab-drop")
            .absolute()
            .inset_0()
            .children(overlay)
            .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<DraggedFileTab>, _, cx| {
                let position = event.event.position;
                let zone = match docked.filter(|preview| preview.contains(&position)) {
                    Some(preview) => zone_at(preview, position).map(|zone| (zone, true)),
                    None => zone_at(event.bounds, position).map(|zone| (zone, false)),
                };
                if zone != this.file_tab_drop {
                    this.file_tab_drop = zone;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|this, dragged: &DraggedFileTab, window, cx| {
                let Some((zone, in_preview)) = this.file_tab_drop.take() else {
                    return;
                };
                match (zone, in_preview) {
                    (DropZone::Edge(side), true) => {
                        this.with_preview(cx, |panel, cx| panel.split_tab(dragged.ix, side, cx));
                    }
                    (DropZone::Center, true) => {}
                    (DropZone::Edge(side), false) => this.with_preview(cx, |panel, cx| panel.set_dock(Some(side), cx)),
                    (DropZone::Center, false) => this.with_preview(cx, |panel, cx| panel.set_dock(None, cx)),
                }
                this.show_file_tab(dragged.ix, window, cx);
            }))
    }

    /// 预览停靠在终端（`panes`）的 `dock` 一侧，中间一条能拖动改大小的分隔线。
    fn render_docked_preview(
        &mut self,
        panes: Stateful<gpui::Div>,
        dock: Dock,
        width: f32,
        gap: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let horizontal = matches!(dock.side, Direction::Left | Direction::Right);
        let preview_first = matches!(dock.side, Direction::Left | Direction::Up);
        // 预览在后一块时前面还隔着空隙。
        let preview_width = match dock.side {
            Direction::Left => width * dock.ratio,
            Direction::Right => width * dock.ratio - gap,
            Direction::Up | Direction::Down => width,
        };
        let layout = self.layout.clone();
        let preview = div().relative().size_full().child(self.render_preview(preview_width, window, cx)).child(
            canvas(move |bounds, _, _| layout.borrow_mut().docked_preview = Some(bounds), |_, (), _, _| {})
                .absolute()
                .size_full(),
        );
        let (first, second) = if preview_first {
            (preview.into_any_element(), panes.into_any_element())
        } else {
            (panes.into_any_element(), preview.into_any_element())
        };
        // 分隔线停在前一块的这个比例处。
        let split = if preview_first { dock.ratio } else { 1. - dock.ratio };
        // 卡片样式下空隙本身就是分隔线；经典样式下是一像素的线。
        let line = if cards(cx) { cx.theme().foreground.opacity(0.) } else { cx.theme().foreground.opacity(0.15) };
        let grab = gap.max(DIVIDER_GRAB_WIDTH);
        let axis = if horizontal { Axis::Horizontal } else { Axis::Vertical };
        let handle = split_handle(
            "preview-divider",
            !horizontal,
            rust_i18n::t!("divider.preview").into_owned(),
            cx,
            move |this, double, cx| {
                if double {
                    this.with_preview(cx, |panel, _| panel.set_dock_ratio(0.5));
                } else {
                    this.dragging_divider = Some(Divider::Preview(axis));
                }
            },
            move |this, delta, _, cx| {
                // 加是分隔线往右、往下挪。
                let delta = if preview_first { delta } else { -delta };
                this.with_preview(cx, |panel, _| panel.set_dock_ratio(dock.ratio + delta * RATIO_STEP));
            },
        )
        .absolute()
        .map(|handle| {
            if horizontal {
                handle.top_0().h_full().w(px(grab)).left(relative(split)).ml(px((gap - grab) / 2.))
            } else {
                handle.left_0().w_full().h(px(grab)).top(relative(split)).mt(px((gap - grab) / 2.))
            }
        });
        div()
            .relative()
            .size_full()
            .flex()
            .when(!horizontal, |el| el.flex_col())
            .child(
                div()
                    .flex_none()
                    .map(|el| if horizontal { el.w(relative(split)).h_full() } else { el.h(relative(split)).w_full() })
                    .min_w_0()
                    .min_h_0()
                    .child(first),
            )
            .child(
                div()
                    .flex_none()
                    .bg(line)
                    .map(|el| if horizontal { el.w(px(gap)).h_full() } else { el.h(px(gap)).w_full() }),
            )
            .child(div().flex_1().min_w_0().min_h_0().child(second))
            .child(handle)
            .into_any_element()
    }

    /// 拖着停靠的预览和终端之间的分隔线，鼠标到了 `position`。
    pub(super) fn drag_preview_divider(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(bounds), Some(dock)) = (self.layout.borrow().terminals, self.preview_dock(cx)) else {
            return;
        };
        let along = match dock.side {
            Direction::Left | Direction::Right => (position.x - bounds.origin.x) / bounds.size.width,
            Direction::Up | Direction::Down => (position.y - bounds.origin.y) / bounds.size.height,
        };
        let ratio = if matches!(dock.side, Direction::Left | Direction::Up) { along } else { 1. - along };
        self.with_preview(cx, |panel, _| panel.set_dock_ratio(ratio));
    }

    /// 当前 workspace 的预览，宽 `width`，盖满所在的区域。没有标签时什么也不画。
    ///
    /// 缓存：它盖着的终端一输出窗口就重画，预览只在自己的标签、编辑、高亮这些变了时重画。辅助工具开着时
    /// 不缓存：照搬上一帧时跳过 prepaint，报给辅助工具的节点就丢了。
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
        let preview = cached_page(self.preview.clone(), window);
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
            PreviewEvent::Quote(text) => self.quote_to_agent(text.clone(), window, cx),
            // 经 Git 面板跑，和面板上的操作一起按仓库排队；做完它发 `GitPanelEvent::HunkApplied`，再重读 diff。
            PreviewEvent::ApplyHunk { root, file, hunk, action } => {
                self.with_git_panel(cx, |panel, cx| panel.apply_hunk(root, file.clone(), *hunk, *action, window, cx));
            }
        }
    }
}
