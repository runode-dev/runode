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
    theme::{ADDED, MODIFIED, REMOVED, RENAMED},
};
use gpui::{Action, Context, Focusable, Pixels, Point, SharedString, Window, actions};
use runode_git::{self as git, FileStatus};
use runode_types::color::Rgb;

use super::{DiffTarget, Preview, PreviewPanel, RealRoot, WorkspaceId};

actions!(
    runode,
    [
        /// 关掉当前以外的预览标签。
        CloseOtherPreviews,
        /// 关掉当前预览标签右边的所有标签。
        ClosePreviewsToRight,
        CloseAllPreviews,
        /// 把临时的预览标签固定下来，不再被下一个打开的文件换掉。
        KeepPreviewOpen
    ]
);

/// 标签名按文件的 git 状态上的颜色。
fn status_color(status: FileStatus) -> Rgb {
    match status {
        FileStatus::Modified => MODIFIED,
        FileStatus::Added | FileStatus::Untracked => ADDED,
        FileStatus::Deleted | FileStatus::Conflicted => REMOVED,
        FileStatus::Renamed => RENAMED,
    }
}

/// 一个 workspace 的预览标签：一个文件一个，没固定的临时标签最多一个。没有标签时预览栏不显示。不进存档。
#[derive(Default)]
pub(super) struct PreviewTabs {
    pub tabs: Vec<Preview>,
    /// 当前显示的标签。
    pub active: usize,
    /// 预览在最前，盖住终端区；切到终端标签时退到后面，标签都关掉时也是。
    pub front: bool,
    /// 这个 workspace 最近读到的 git 状态，见 `PreviewPanel::set_git`。
    pub git: Option<Arc<git::Repos>>,
    /// `git` 主仓库的根目录和它解析过符号链接的路径，`Preview::refresh_marks` 比字面上不在根下的文件用；
    /// 由 `PreviewPanel::set_git` 在单独的线程上解析，解析好之前（或解析不了时）两个都是字面的根目录。
    pub real_root: Option<RealRoot>,
}

impl PreviewTabs {
    pub fn active(&self) -> Option<&Preview> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Preview> {
        self.tabs.get_mut(self.active)
    }

    /// 打开 `path`（`diff` 不为空时是它的 diff）并切过去，`pin` 时固定下来。已经开着就切过去；
    /// 没开着时换掉临时标签，没有临时标签就插在当前标签右边。返回被换掉的临时标签。
    pub fn open(&mut self, path: &Path, diff: Option<DiffTarget>, pin: bool) -> Option<Preview> {
        if let Some(ix) = self.tabs.iter().position(|tab| tab.path == path && tab.diff == diff) {
            self.active = ix;
            self.tabs[ix].pinned |= pin;
            return None;
        }
        let tab = Preview::new(path.to_path_buf(), diff, pin);
        if let Some(ix) = self.tabs.iter().position(|tab| !tab.pinned) {
            self.active = ix;
            return Some(std::mem::replace(&mut self.tabs[ix], tab));
        }
        let ix = if self.tabs.is_empty() { 0 } else { self.active + 1 };
        self.tabs.insert(ix, tab);
        self.active = ix;
        None
    }

    /// 只留下 `keep` 为真的标签，返回关掉的。当前标签关掉时切到它右边的那个，右边没有了就切到
    /// 最后一个。
    pub fn retain(&mut self, mut keep: impl FnMut(usize, &Preview) -> bool) -> Vec<Preview> {
        let mut kept_before_active = 0;
        let (mut kept, mut closed) = (Vec::new(), Vec::new());
        for (ix, tab) in std::mem::take(&mut self.tabs).into_iter().enumerate() {
            if keep(ix, &tab) {
                kept_before_active += usize::from(ix < self.active);
                kept.push(tab);
            } else {
                closed.push(tab);
            }
        }
        self.tabs = kept;
        self.active = kept_before_active.min(self.tabs.len().saturating_sub(1));
        self.front &= !self.tabs.is_empty();
        closed
    }

    /// 把第 `from` 个标签挪到第 `to` 个位置并切过去。
    fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(from);
        let to = to.min(self.tabs.len());
        self.tabs.insert(to, tab);
        self.active = to;
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
    /// 在编辑，改过还没存。
    pub dirty: bool,
    /// 加了几行、删了几行，没有改动时为空。
    pub stat: Option<(usize, usize)>,
}

impl PreviewPanel {
    pub(super) fn close_preview_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_active_preview(window, cx);
    }

    /// 预览栏里没有分屏，关分屏的键（cmd+w）也只关当前的预览标签，不连带关掉终端。
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
        let previews = self.tabs_mut();
        if from >= previews.tabs.len() {
            return;
        }
        let before = previews.active().map(|tab| (tab.path.clone(), tab.diff.clone()));
        previews.move_tab(from, to);
        let new = previews.active;
        // 先指回原来的当前标签，`activate_preview` 才知道换下去的是哪个。
        previews.active =
            previews.tabs.iter().position(|tab| Some((tab.path.clone(), tab.diff.clone())) == before).unwrap_or(new);
        self.activate_preview(new, cx);
        self.bring_front(cx);
    }

    /// workspace `ws` 的文件标签，和在最前时当前的是第几个。
    pub fn file_tabs(&self, ws: WorkspaceId) -> (Vec<FileTab>, Option<usize>) {
        let Some(previews) = self.workspaces.get(&ws) else {
            return (Vec::new(), None);
        };
        let tabs = previews
            .tabs
            .iter()
            .map(|tab| FileTab {
                // 按路径（加 diff 的哪一边）取，不按下标：关掉、拖动标签后下标整体平移，辅助工具的焦点会跳。
                key: match &tab.diff {
                    Some(diff) => format!("{}#{:?}", tab.path.display(), diff.side),
                    None => tab.path.display().to_string(),
                }
                .into(),
                name: tab.name(),
                path: tab.path.clone(),
                diff: tab.diff.is_some(),
                color: tab.status.map(status_color),
                pinned: tab.pinned,
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

    /// workspace `ws` 的文件标签在最前时，当前的是第几个。
    pub fn front_file(&self, ws: WorkspaceId) -> Option<usize> {
        self.front(ws).then(|| self.workspaces[&ws].active)
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
        let (count, active) = (previews.tabs.len(), previews.active);
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
        previews.retain(|_, _| false);
        assert!(previews.tabs.is_empty());
        assert!(previews.active().is_none());
    }

    #[test]
    fn moves_tabs_and_follows_renames() {
        let mut previews = PreviewTabs::default();
        for name in ["a", "dir/b", "dir/c"] {
            previews.open(Path::new(name), None, true);
        }
        previews.move_tab(0, 2);
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
}
