//! 预览栏的标签：开着的标签（`PreviewTabs`）怎么换、关、挪，交给标题栏画的样子（`FileTab`）和标题栏上
//! 的操作，右键菜单，以及菜单里关标签、固定标签的动作。

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::ui::{
    actions::{ClosePane, CloseTab},
    follow_move,
    menu::{MenuHost as _, MenuItem},
};
use gpui::{Action, Context, Focusable, Pixels, Point, SharedString, Window, actions};
use runode_git as git;
use runode_types::{
    color::Rgb,
    pane::{Axis, Direction},
};

use crate::git_view::status_color;

use super::{DiffTarget, Preview, PreviewPanel, RealRoot, WorkspaceId};
use crate::project_host::ProjectHost;

actions!(
    runode,
    [
        /// 关掉当前以外的预览标签。
        CloseOtherPreviews,
        /// 关掉当前预览标签右边的所有标签。
        ClosePreviewsToRight,
        CloseAllPreviews,
        /// 把临时的预览标签固定下来，不再被下一个打开的文件换掉。
        KeepPreviewOpen,
        /// 当前预览标签分到右边一块，和另一个文件左右并排。
        SplitPreviewRight,
        /// 当前预览标签分到下面一块，和另一个文件上下叠放。
        SplitPreviewDown,
        /// 分成两块的预览合回一块，留下当前的标签。
        UnsplitPreview
    ]
);

/// 一个 workspace 的预览标签：一个文件一个，没固定的临时标签最多一个。没有标签时预览栏不显示。不进存档。
#[derive(Default)]
pub(super) struct PreviewTabs {
    pub tabs: Vec<Preview>,
    /// 当前显示的标签；分屏时是当前块里的。
    pub active: usize,
    /// 预览分成并排的两块；为空时只有一块。
    pub split: Option<Split>,
    /// 预览在最前：没停靠时盖住终端区，停靠着时是和终端并排的两边里当前的那边；切到终端、标签都关掉时为假。
    pub front: bool,
    /// 停靠在终端区的哪一侧、和终端并排；为空时盖住终端区。标签都关掉时清掉。
    pub dock: Option<Dock>,
    /// 这个 workspace 最近读到的 git 状态。
    pub git: Option<Arc<git::Repos>>,
    /// `git` 主仓库的根目录和它解析过符号链接的路径，`Preview::refresh_marks` 比字面上不在根下的文件用；
    /// 由 `PreviewPanel::set_git` 在单独的线程上解析，解析好之前（或解析不了时）两个都是字面的根目录。
    pub real_root: Option<RealRoot>,
    /// 这个 workspace 的文件在哪台机器上。
    pub project: ProjectHost,
}

impl PreviewTabs {
    pub fn active(&self) -> Option<&Preview> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Preview> {
        self.tabs.get_mut(self.active)
    }

    /// 分屏时另一块显示的标签。
    pub fn other(&self) -> Option<usize> {
        self.split.map(|split| split.other)
    }

    /// 显示着的标签：当前的，分屏时还有另一块里的。
    pub fn shown(&self) -> impl Iterator<Item = usize> + use<> {
        let active = (self.active < self.tabs.len()).then_some(self.active);
        active.into_iter().chain(self.other())
    }

    /// 切到第 `ix` 个标签：它显示在分屏的另一块里时当前块换到那边，否则在当前块里显示它。
    pub fn activate(&mut self, ix: usize) {
        if let Some(split) = &mut self.split
            && split.other == ix
        {
            split.other = self.active;
            split.active_first = !split.active_first;
        }
        self.active = ix;
    }

    /// 把第 `ix` 个标签分到 `side` 一侧那块、成为当前的；另一块显示原来当前的标签，原来当前的就是它时显示
    /// 另一块原来的、没分屏时显示它旁边的。只有一个标签时不分。返回因此不再显示的标签。
    pub fn split(&mut self, ix: usize, side: Direction) -> Option<usize> {
        if ix >= self.tabs.len() {
            return None;
        }
        let keep = if ix == self.active {
            self.other().or_else(|| neighbour(ix + 1, self.tabs.len(), Some(ix)))?
        } else {
            self.active
        };
        let hidden = self.shown().find(|&shown| shown != ix && shown != keep);
        let axis = match side {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        };
        let ratio = self.split.map_or(0.5, |split| split.ratio);
        let active_first = matches!(side, Direction::Left | Direction::Up);
        self.active = ix;
        self.split = Some(Split { axis, ratio, other: keep, active_first });
        hidden
    }
    /// 打开 `path`（`diff` 不为空时是它的 diff）并切过去，`pin` 时固定下来。已经开着就切过去；
    /// 没开着时换掉临时标签，没有临时标签就插在当前标签右边。返回被换掉的临时标签。
    pub fn open(&mut self, path: &Path, diff: Option<DiffTarget>, pin: bool) -> Option<Preview> {
        if let Some(ix) = self.tabs.iter().position(|tab| tab.path == path && tab.diff == diff) {
            self.activate(ix);
            self.tabs[ix].pinned |= pin;
            return None;
        }
        let tab = Preview::new(path.to_path_buf(), diff, pin);
        if let Some(ix) = self.tabs.iter().position(|tab| !tab.pinned) {
            // 分屏另一块里的临时标签留着给人对照，固定下来，新的另开一个。
            if Some(ix) == self.other() {
                self.tabs[ix].pinned = true;
            } else {
                self.active = ix;
                return Some(std::mem::replace(&mut self.tabs[ix], tab));
            }
        }
        let ix = if self.tabs.is_empty() { 0 } else { self.active + 1 };
        self.tabs.insert(ix, tab);
        if let Some(split) = &mut self.split
            && split.other >= ix
        {
            split.other += 1;
        }
        self.active = ix;
        None
    }

    /// 只留下 `keep` 为真的标签，返回关掉的。显示着的标签关掉时那一块切到它右边的那个，右边没有了就切到
    /// 它左边最近的，分屏时跳过另一块显示着的；分屏的一块没有能显示的了就合成一块。
    pub fn retain(&mut self, mut keep: impl FnMut(usize, &Preview) -> bool) -> Vec<Preview> {
        let other = self.other();
        let (mut kept_before_active, mut kept_before_other) = (0, 0);
        let (mut new_active, mut new_other) = (None, None);
        let (mut kept, mut closed) = (Vec::new(), Vec::new());
        for (ix, tab) in std::mem::take(&mut self.tabs).into_iter().enumerate() {
            if keep(ix, &tab) {
                if ix == self.active {
                    new_active = Some(kept.len());
                }
                if Some(ix) == other {
                    new_other = Some(kept.len());
                }
                kept_before_active += usize::from(ix < self.active);
                kept_before_other += usize::from(other.is_some_and(|other| ix < other));
                kept.push(tab);
            } else {
                closed.push(tab);
            }
        }
        self.tabs = kept;
        let len = self.tabs.len();
        let active = new_active.or_else(|| neighbour(kept_before_active, len, new_other));
        let other = other.and(new_other.or_else(|| neighbour(kept_before_other, len, active)));
        match (&mut self.split, active, other) {
            (Some(split), Some(active), Some(other)) if active != other => {
                self.active = active;
                split.other = other;
            }
            (_, active, other) => {
                self.active = active.or(other).unwrap_or(0);
                self.split = None;
            }
        }
        self.front &= !self.tabs.is_empty();
        if self.tabs.is_empty() {
            self.dock = None;
        }
        closed
    }

    /// 把第 `from` 个标签挪到第 `to` 个位置，当前的和分屏另一块的跟着它们的标签走；返回它挪到了第几个。
    fn move_tab(&mut self, from: usize, to: usize) -> Option<usize> {
        if from >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(from);
        let to = to.min(self.tabs.len());
        self.tabs.insert(to, tab);
        let follow = |ix: usize| {
            if ix == from {
                return to;
            }
            let ix = if ix > from { ix - 1 } else { ix };
            if ix >= to { ix + 1 } else { ix }
        };
        self.active = follow(self.active);
        if let Some(split) = &mut self.split {
            split.other = follow(split.other);
        }
        Some(to)
    }

    /// `from` 改了名或挪到了 `to`：它和它下面的文件的标签换成新路径，固定与否和编辑中的内容不变，记下
    /// 要重读。返回换下来的旧标签。diff 标签不跟，扫描到新的改动时它自己会重读。
    pub fn moved(&mut self, from: &Path, to: &Path) -> Vec<Preview> {
        let mut old = Vec::new();
        for tab in self.tabs.iter_mut().filter(|tab| tab.diff.is_none()) {
            if let Some(path) = follow_move(&tab.path, from, to) {
                let mut new = Preview::new(path, None, tab.pinned);
                new.reload = true;
                new.editor = tab.editor.take();
                old.push(std::mem::replace(tab, new));
            }
        }
        old
    }
}

/// 从第 `start` 个往右、再从它左边往左找第一个不是 `avoid` 的标签，一共 `len` 个。
fn neighbour(start: usize, len: usize, avoid: Option<usize>) -> Option<usize> {
    (start..len).chain((0..start.min(len)).rev()).find(|&ix| Some(ix) != avoid)
}

/// 预览分成的两块：当前块显示 `PreviewTabs::active`，另一块显示 `other`，两块不显示同一个标签。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Split {
    pub axis: Axis,
    /// 前一块（左边或上面）占的比例。
    pub ratio: f32,
    pub other: usize,
    /// 当前块是前一块。
    pub active_first: bool,
}

/// 预览停靠在终端区的哪一侧（`side`），占终端区这个方向上的多少（`ratio`，0 到 1）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dock {
    pub side: Direction,
    pub ratio: f32,
}

/// 标题栏里画一个文件标签要的东西。
#[derive(Clone, Debug, PartialEq)]
pub struct FileTab {
    /// 标签元素的 id 用：路径，diff 标签再带上和什么比。
    pub key: SharedString,
    pub name: SharedString,
    pub path: PathBuf,
    /// diff 标签，图标画成 diff 的样子。
    pub diff: bool,
    /// 名字按文件的 git 状态上的颜色；不在仓库里或没改动时为空。
    pub color: Option<Rgb>,
    /// 临时标签为假，名字斜体。
    pub pinned: bool,
    /// 显示在分屏的另一块里，预览在最前或停靠着时和当前标签一起看得见。
    pub beside: bool,
    /// 在编辑，改过还没存。
    pub dirty: bool,
    /// 加了几行、删了几行，没有改动时为空。
    pub stat: Option<(usize, usize)>,
}

impl PreviewPanel {
    pub(super) fn close_preview_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_active_preview(window, cx);
    }

    /// 关分屏的键（cmd+w）在预览栏里关的也是当前的预览标签，不关预览的一块，也不连带关掉终端。
    pub(super) fn close_preview_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        self.close_active_preview(window, cx);
    }

    fn close_active_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.tabs_mut().active;
        self.retain_previews(|ix, _| ix != active, window, cx);
    }

    pub(super) fn close_other_previews(&mut self, _: &CloseOtherPreviews, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.tabs_mut().active;
        self.retain_previews(|ix, _| ix == active, window, cx);
    }

    pub(super) fn close_previews_to_right(
        &mut self,
        _: &ClosePreviewsToRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.tabs_mut().active;
        self.retain_previews(|ix, _| ix <= active, window, cx);
    }

    pub(super) fn close_all_previews(&mut self, _: &CloseAllPreviews, window: &mut Window, cx: &mut Context<Self>) {
        self.retain_previews(|_, _| false, window, cx);
    }

    pub(super) fn keep_preview_open(&mut self, _: &KeepPreviewOpen, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(preview) = self.preview_mut() {
            preview.pinned = true;
            cx.notify();
        }
    }

    /// 把第 `from` 个标签挪到第 `to` 个位置并切过去。
    fn move_preview(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let Some(new) = self.tabs_mut().move_tab(from, to) else {
            return;
        };
        self.activate_preview(new, cx);
        self.bring_front(cx);
    }

    pub(super) fn split_preview_right(&mut self, _: &SplitPreviewRight, _: &mut Window, cx: &mut Context<Self>) {
        let active = self.tabs_mut().active;
        self.split_tab(active, Direction::Right, cx);
    }

    pub(super) fn split_preview_down(&mut self, _: &SplitPreviewDown, _: &mut Window, cx: &mut Context<Self>) {
        let active = self.tabs_mut().active;
        self.split_tab(active, Direction::Down, cx);
    }

    pub(super) fn unsplit_preview(&mut self, _: &UnsplitPreview, _: &mut Window, cx: &mut Context<Self>) {
        let previews = self.tabs_mut();
        if let Some(split) = previews.split.take()
            && let Some(tab) = previews.tabs.get(split.other)
        {
            tab.release_image(cx);
        }
        cx.notify();
    }

    /// 把当前 workspace 的第 `ix` 个文件标签分到预览的 `side` 一侧那块，切过去、带到最前；只有一个标签时只是
    /// 切过去。
    pub fn split_tab(&mut self, ix: usize, side: Direction, cx: &mut Context<Self>) {
        let previews = self.tabs_mut();
        if let Some(hidden) = previews.split(ix, side)
            && let Some(tab) = previews.tabs.get(hidden)
        {
            tab.release_image(cx);
        }
        self.activate_preview(ix, cx);
        self.bring_front(cx);
    }

    /// workspace `ws` 的文件标签，和在最前时当前的是第几个。
    pub fn file_tabs(&self, ws: WorkspaceId) -> (Vec<FileTab>, Option<usize>) {
        let Some(previews) = self.workspaces.get(&ws) else {
            return (Vec::new(), None);
        };
        let beside = previews.other().filter(|_| previews.front || previews.dock.is_some());
        let tabs = previews
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| FileTab {
                key: tab.key(),
                name: tab.name(),
                path: tab.path.clone(),
                diff: tab.diff.is_some(),
                color: tab.status.map(status_color),
                pinned: tab.pinned,
                beside: beside == Some(ix),
                dirty: tab.dirty(),
                stat: tab.line_stat(),
            })
            .collect();
        (tabs, self.front_file(ws))
    }

    /// workspace `ws` 有几个文件标签。
    pub fn file_tab_count(&self, ws: WorkspaceId) -> usize {
        self.workspaces.get(&ws).map_or(0, |previews| previews.tabs.len())
    }

    /// workspace `ws` 的预览停靠着时，显示着的是第几个文件标签，在不在最前都算。
    pub fn docked_file(&self, ws: WorkspaceId) -> Option<usize> {
        self.dock(ws).map(|_| self.workspaces[&ws].active)
    }

    /// workspace `ws` 的文件标签在最前时，当前的是第几个。
    pub fn front_file(&self, ws: WorkspaceId) -> Option<usize> {
        self.current(ws).then(|| self.workspaces[&ws].active)
    }

    /// 切到当前 workspace 的第 `ix` 个文件标签、带到最前，焦点交给预览栏。
    pub fn show_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_preview(ix, cx);
        self.bring_front(cx);
        window.focus(&self.focus_handle(cx), cx);
    }

    /// 关掉当前 workspace 的第 `ix` 个文件标签，改过没存的先问。
    pub fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.retain_previews(|i, _| i != ix, window, cx);
    }

    /// 把当前 workspace 的第 `ix` 个文件标签固定下来。
    pub fn pin_tab(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs_mut().tabs.get_mut(ix) {
            tab.pinned = true;
            cx.notify();
        }
    }

    /// 把当前 workspace 的第 `from` 个文件标签挪到第 `to` 个位置，切过去、带到最前，焦点交给预览栏。
    pub fn move_tab(&mut self, from: usize, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.move_preview(from, to, cx);
        window.focus(&self.focus_handle(cx), cx);
    }

    /// 在 `position` 弹出当前文件标签的右键菜单。
    pub fn open_tab_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.open_preview_menu(position, cx);
    }

    /// 在 `position` 弹出当前预览标签的右键菜单。
    fn open_preview_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(previews) = self.tabs() else {
            return;
        };
        let (count, active, split) = (previews.tabs.len(), previews.active, previews.split.is_some());
        let pinned = previews.active().is_some_and(|tab| tab.pinned);
        let item = |key: &str, action: Box<dyn Action>, enabled: bool| {
            Some(MenuItem::new(rust_i18n::t!(key), action, enabled, cx))
        };
        let mut items = vec![
            item("preview.close", Box::new(CloseTab), true),
            item("preview.close_others", Box::new(CloseOtherPreviews), count > 1),
            item("preview.close_right", Box::new(ClosePreviewsToRight), active + 1 < count),
            item("preview.close_all", Box::new(CloseAllPreviews), true),
        ];
        if !pinned {
            items.extend([None, item("preview.keep_open", Box::new(KeepPreviewOpen), true)]);
        }
        items.extend([
            None,
            item("preview.split_right", Box::new(SplitPreviewRight), count > 1),
            item("preview.split_down", Box::new(SplitPreviewDown), count > 1),
        ]);
        if split {
            items.push(item("preview.unsplit", Box::new(UnsplitPreview), true));
        }
        let target = self.focus.clone();
        self.open_context_menu(position, items, target, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// 各标签的文件名，临时标签后面带 `*`，当前标签前面带 `>`。
    fn tabs(previews: &PreviewTabs) -> Vec<String> {
        previews
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let mark = if ix == previews.active { ">" } else { "" };
                let temp = if tab.pinned { "" } else { "*" };
                format!("{mark}{}{temp}", tab.path.display())
            })
            .collect()
    }

    #[test]
    fn temporary_tab_is_replaced_and_pinned_tabs_open_beside_the_active_one() {
        let mut previews = PreviewTabs::default();
        assert!(previews.open(Path::new("a"), None, false).is_none());
        assert_eq!(previews.open(Path::new("b"), None, false).map(|old| old.path.clone()), Some(PathBuf::from("a")));
        assert_eq!(tabs(&previews), [">b*"]);
        // 再开已经开着的文件只是切过去，带 `pin` 时固定下来。
        previews.open(Path::new("b"), None, true);
        assert_eq!(tabs(&previews), [">b"]);
        previews.open(Path::new("c"), None, true);
        previews.open(Path::new("d"), None, false);
        assert_eq!(tabs(&previews), ["b", "c", ">d*"]);
        // 开固定标签也先占掉临时标签的位置。
        previews.open(Path::new("e"), None, true);
        assert_eq!(tabs(&previews), ["b", "c", ">e"]);
        // 没有临时标签时插在当前标签右边。
        previews.active = 0;
        previews.open(Path::new("f"), None, false);
        assert_eq!(tabs(&previews), ["b", ">f*", "c", "e"]);
        // 临时标签不论在哪都被换掉，位置不变。
        previews.active = 3;
        previews.open(Path::new("g"), None, false);
        assert_eq!(tabs(&previews), ["b", ">g*", "c", "e"]);
    }

    #[test]
    fn closing_the_active_tab_moves_to_its_right_neighbour() {
        let mut previews = PreviewTabs::default();
        for name in ["a", "b", "c", "d"] {
            previews.open(Path::new(name), None, true);
        }
        previews.active = 1;
        assert_eq!(previews.retain(|ix, _| ix != 1).len(), 1);
        assert_eq!(tabs(&previews), ["a", ">c", "d"]);
        // 关掉当前标签左边的，当前标签不变。
        previews.retain(|ix, _| ix != 0);
        assert_eq!(tabs(&previews), [">c", "d"]);
        // 右边没有了切到最后一个。
        previews.active = 1;
        previews.retain(|ix, _| ix != 1);
        assert_eq!(tabs(&previews), [">c"]);
        // 停靠着的预览关掉一部分标签时还停靠着，都关掉时下次打开改回盖住终端区。
        previews.dock = Some(Dock { side: Direction::Right, ratio: 0.4 });
        previews.retain(|_, _| true);
        assert!(previews.dock.is_some());
        previews.retain(|_, _| false);
        assert!(previews.tabs.is_empty());
        assert!(previews.active().is_none());
        assert!(previews.dock.is_none());
    }

    #[test]
    fn moves_tabs_and_follows_renames() {
        let mut previews = PreviewTabs::default();
        for name in ["a", "dir/b", "dir/c"] {
            previews.open(Path::new(name), None, true);
        }
        previews.active = 0;
        assert_eq!(previews.move_tab(0, 2), Some(2));
        assert_eq!(tabs(&previews), ["dir/b", "dir/c", ">a"]);
        previews.move_tab(2, 0);
        assert_eq!(tabs(&previews), [">a", "dir/b", "dir/c"]);
        previews.open(Path::new("dir/c"), None, false);
        previews.tabs[2].pinned = false;
        let old = previews.moved(Path::new("dir"), Path::new("new"));
        assert_eq!(old.len(), 2);
        // 新路径按本机的分隔符拼。
        let (b, c) = (Path::new("new").join("b"), Path::new("new").join("c"));
        assert_eq!(tabs(&previews), ["a".to_owned(), b.display().to_string(), format!(">{}*", c.display())]);
        // 改名的就是开着的文件：新路径不带结尾的 `/`。
        previews.moved(Path::new("a"), Path::new("z"));
        assert_eq!(previews.tabs[0].path.as_os_str(), "z");
    }

    fn split_tabs(names: &[&str], active: usize) -> PreviewTabs {
        let mut previews = PreviewTabs::default();
        for name in names {
            previews.open(Path::new(name), None, true);
        }
        previews.active = active;
        previews
    }

    /// 两块各显示的标签：当前块的和另一块的。
    fn blocks(previews: &PreviewTabs) -> (String, Option<String>) {
        let name = |ix: usize| previews.tabs[ix].path.display().to_string();
        (name(previews.active), previews.other().map(name))
    }

    #[test]
    fn splitting_shows_the_tab_beside_the_active_one() {
        // 只有一个标签时分不了。
        let mut previews = split_tabs(&["a"], 0);
        assert_eq!(previews.split(0, Direction::Right), None);
        assert!(previews.split.is_none());
        // 分出别的标签时原来当前的留在另一块。
        let mut previews = split_tabs(&["a", "b", "c"], 0);
        previews.split(2, Direction::Right);
        assert_eq!(blocks(&previews), ("c".into(), Some("a".into())));
        let split = previews.split.unwrap();
        assert_eq!((split.axis, split.active_first), (Axis::Horizontal, false));
        // 分出当前的标签时另一块显示原来另一块的，大小不变；朝下分就上下叠放。
        previews.split.as_mut().unwrap().ratio = 0.3;
        assert_eq!(previews.split(2, Direction::Up), None);
        assert_eq!(blocks(&previews), ("c".into(), Some("a".into())));
        let split = previews.split.unwrap();
        assert_eq!((split.axis, split.active_first, split.ratio), (Axis::Vertical, true, 0.3));
        // 分出第三个标签时，原来另一块的不再显示。
        assert_eq!(previews.split(1, Direction::Left), Some(0));
        assert_eq!(blocks(&previews), ("b".into(), Some("c".into())));
        // 没分屏时分出当前的标签，另一块显示它右边的，最右边的显示左边的。
        let mut previews = split_tabs(&["a", "b", "c"], 2);
        previews.split(2, Direction::Right);
        assert_eq!(blocks(&previews), ("c".into(), Some("b".into())));
    }

    #[test]
    fn activating_the_other_block_swaps_the_current_block() {
        let mut previews = split_tabs(&["a", "b", "c"], 0);
        previews.split(1, Direction::Right);
        previews.activate(0);
        assert_eq!(blocks(&previews), ("a".into(), Some("b".into())));
        assert!(previews.split.unwrap().active_first);
        // 切到没显示的标签时在当前块里显示，另一块不动。
        previews.activate(2);
        assert_eq!(blocks(&previews), ("c".into(), Some("b".into())));
        // 打开另一块里的文件也是换过去。
        previews.open(Path::new("b"), None, false);
        assert_eq!(blocks(&previews), ("b".into(), Some("c".into())));
    }

    #[test]
    fn opening_files_keeps_the_other_block() {
        let mut previews = split_tabs(&["a", "b"], 0);
        previews.tabs[0].pinned = false;
        previews.split(1, Direction::Right);
        // 另一块里的临时标签不被换掉，固定下来；新的插在当前标签右边。
        previews.open(Path::new("x"), None, false);
        assert_eq!(tabs(&previews), ["a", "b", ">x*"]);
        assert_eq!(blocks(&previews), ("x".into(), Some("a".into())));
        // 插在另一块的标签前面时另一块跟着它的标签走。
        previews.activate(0);
        previews.open(Path::new("y"), None, true);
        assert_eq!(tabs(&previews), ["a", ">y", "b", "x"]);
        assert_eq!(blocks(&previews), ("y".into(), Some("x".into())));
    }

    #[test]
    fn closing_a_shown_tab_fills_its_block_or_merges() {
        let mut previews = split_tabs(&["a", "b", "c", "d"], 1);
        previews.split(2, Direction::Right);
        assert_eq!(blocks(&previews), ("c".into(), Some("b".into())));
        // 当前块的关掉时换上右边的。
        previews.retain(|ix, _| ix != 2);
        assert_eq!(blocks(&previews), ("d".into(), Some("b".into())));
        // 另一块的关掉时换上右边的，跳过当前块显示着的，再往左找。
        previews.retain(|ix, _| ix != 1);
        assert_eq!(blocks(&previews), ("d".into(), Some("a".into())));
        // 关掉别的，分屏和两块不动，下标跟着变。
        let mut previews = split_tabs(&["a", "b", "c", "d"], 3);
        previews.split(1, Direction::Right);
        assert_eq!(blocks(&previews), ("b".into(), Some("d".into())));
        previews.retain(|ix, _| ix != 0);
        assert_eq!(blocks(&previews), ("b".into(), Some("d".into())));
        previews.retain(|ix, _| ix != 1);
        assert_eq!(blocks(&previews), ("b".into(), Some("d".into())));
        // 一块没有能显示的了就合成一块。
        previews.retain(|ix, _| ix != 1);
        assert_eq!(blocks(&previews), ("b".into(), None));
        assert!(previews.split.is_none());
    }

    #[test]
    fn moving_tabs_keeps_both_blocks() {
        let mut previews = split_tabs(&["a", "b", "c"], 0);
        previews.split(2, Direction::Right);
        assert_eq!(blocks(&previews), ("c".into(), Some("a".into())));
        previews.move_tab(0, 2);
        assert_eq!(tabs(&previews), ["b", ">c", "a"]);
        assert_eq!(blocks(&previews), ("c".into(), Some("a".into())));
    }
}
