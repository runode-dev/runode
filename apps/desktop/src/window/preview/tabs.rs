//! 预览栏的标签条：开着的标签（`PreviewTabs`）怎么换、关、挪，拖动标签换位置，标签的样子和
//! 右键菜单，以及菜单里关标签、固定标签的动作。

use std::path::Path;

use gpui::{
    Action, Context, Div, Hsla, MouseButton, MouseDownEvent, Pixels, Point, Render, ScrollHandle, SharedString,
    Stateful, Window, actions, div, img, prelude::*, px, svg,
};
use runode_shared_types::color::Rgb;

use super::{DiffTarget, Preview};
use crate::{
    assets::DIFF_ICON,
    ui::{file_icons::file_icon, hsla, tooltip::tooltip},
    window::{
        CloseTab, TITLEBAR_HEIGHT, WindowView, divider_color,
        files::menu_item,
        project::{RENAMED, status_color},
        titlebar::{close_button, drag_chip},
    },
};

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

/// 预览标签最宽这么宽，名字再长就截断。
const TAB_MAX_WIDTH: f32 = 180.;
/// 拖动预览标签时跟着鼠标的卡片宽度。
const DRAG_CHIP_WIDTH: f32 = 140.;
/// 当前标签顶上那条强调色细线的粗细。
const TAB_ACCENT_HEIGHT: f32 = 2.;

/// 标签条底下的分隔线，叠在不是当前标签的标签和标签后面的空白底部。
pub(super) fn tab_underline(fg: Rgb) -> Div {
    div().absolute().bottom_0().left_0().w_full().h(px(1.)).bg(divider_color(hsla(fg)))
}

/// 预览栏的标签：一个文件一个，没固定的临时标签最多一个。没有标签时预览栏不显示。不进存档。
#[derive(Default)]
pub(in crate::window) struct PreviewTabs {
    pub tabs: Vec<Preview>,
    /// 当前显示的标签。
    pub active: usize,
    /// 标签条的横向滚动位置。
    pub scroll: ScrollHandle,
}

impl PreviewTabs {
    pub fn active(&self) -> Option<&Preview> {
        self.tabs.get(self.active)
    }

    pub(super) fn active_mut(&mut self) -> Option<&mut Preview> {
        self.tabs.get_mut(self.active)
    }

    /// 打开 `path`（`diff` 不为空时是它的 diff）并切过去，`pin` 时固定下来。已经开着就切过去；
    /// 没开着时换掉临时标签，没有临时标签就插在当前标签右边。返回被换掉的临时标签。
    pub(super) fn open(&mut self, path: &Path, diff: Option<DiffTarget>, pin: bool) -> Option<Preview> {
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
    pub(super) fn retain(&mut self, mut keep: impl FnMut(usize, &Preview) -> bool) -> Vec<Preview> {
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

    /// `from` 改了名或挪到了 `to`：它和它下面的文件的标签换成新路径，固定与否不变。返回换下来的
    /// 旧标签。diff 标签不跟，扫描到新的改动时它自己会重读。
    pub(super) fn moved(&mut self, from: &Path, to: &Path) -> Vec<Preview> {
        let mut old = Vec::new();
        for tab in self.tabs.iter_mut().filter(|tab| tab.diff.is_none()) {
            if let Ok(rest) = tab.path.strip_prefix(from) {
                let new = Preview::new(to.join(rest), None, tab.pinned);
                old.push(std::mem::replace(tab, new));
            }
        }
        old
    }
}

/// 拖动中的预览标签：跟着鼠标画出来，放到另一个标签上时挪过去。
#[derive(Clone)]
struct DraggedPreviewTab {
    /// 开始拖动时所在的位置：挪的是这个标签，落点提示也按它画在目标标签的哪一边。
    ix: usize,
    name: SharedString,
    fg: Hsla,
    bg: Hsla,
}

impl Render for DraggedPreviewTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_chip(px(DRAG_CHIP_WIDTH), px(TITLEBAR_HEIGHT), self.name.clone(), self.fg, self.bg)
    }
}

impl WindowView {
    pub(super) fn close_preview_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.workspace().project.previews.active;
        self.retain_previews(|ix, _| ix != active, window, cx);
    }

    pub(super) fn close_other_previews(&mut self, _: &CloseOtherPreviews, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.workspace().project.previews.active;
        self.retain_previews(|ix, _| ix == active, window, cx);
    }

    pub(super) fn close_previews_to_right(
        &mut self,
        _: &ClosePreviewsToRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.workspace().project.previews.active;
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
        let previews = &mut self.workspace_mut().project.previews;
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
    }

    /// 第 `ix` 个预览标签：文件图标和名字，名字按 git 状态上色，临时标签用斜体；当前标签和
    /// 悬停着的标签显示关闭按钮。
    pub(super) fn render_preview_tab(&self, ix: usize, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let previews = &self.workspace().project.previews;
        let tab = &previews.tabs[ix];
        let active = ix == previews.active;
        let name = tab.name();
        let active_bg = hsla(bg.mix(fg, 0.08));
        let hover_bg = hsla(bg.mix(fg, 0.04));
        let underline = tab_underline(fg);
        let close_tooltip = tooltip(rust_i18n::t!("tooltip.close_preview"), None, fg, bg);
        let path_tooltip = tooltip(SharedString::from(tab.path.display().to_string()), None, fg, bg);
        let fg = hsla(fg);
        let color = tab.status.map_or(fg, |status| hsla(status_color(status)));
        let group = SharedString::from(format!("preview-tab-{ix}"));
        let dragged = DraggedPreviewTab { ix, name: name.clone(), fg, bg: active_bg };
        // diff 标签的图标是 diff 的样子，不是文件类型的。
        let icon = match &tab.diff {
            Some(_) => svg().path(DIFF_ICON).flex_none().size(px(14.)).text_color(fg.opacity(0.8)).into_any_element(),
            None => img(file_icon(&tab.file_name())).flex_none().size(px(14.)).into_any_element(),
        };
        div()
            .id(("preview-tab", ix))
            .group(group.clone())
            .flex_none()
            .max_w(px(TAB_MAX_WIDTH))
            .h_full()
            .pl(px(10.))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(6.))
            .relative()
            .border_r_1()
            .border_color(divider_color(fg))
            // 当前标签顶上一条强调色，底下不画分隔线，和正文连成一块；别的标签悬停时稍亮。
            .map(|tab| {
                if active {
                    tab.child(div().absolute().top_0().left_0().w_full().h(px(TAB_ACCENT_HEIGHT)).bg(hsla(RENAMED)))
                } else {
                    tab.hover(|tab| tab.bg(hover_bg)).child(underline)
                }
            })
            .child(div().flex_none().flex().when(!active, |icon| icon.opacity(0.6)).child(icon))
            .child(
                div()
                    .id(("preview-tab-name", ix))
                    .min_w_0()
                    .truncate()
                    .text_color(if active { color } else { color.opacity(0.6) })
                    .when(!tab.pinned, |name| name.italic())
                    .child(name)
                    .tooltip(path_tooltip),
            )
            .child(
                close_button(("preview-tab-close", ix), fg)
                    .flex_none()
                    .when(!active, |close| close.invisible().group_hover(group, |close| close.visible()))
                    .tooltip(close_tooltip)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.retain_previews(|i, _| i != ix, window, cx);
                        }),
                    ),
            )
            // 标题栏按下会拖动窗口，标签自己接住。双击把临时标签固定下来。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    window.focus(&this.preview_focus, cx);
                    if event.click_count >= 2
                        && let Some(tab) = this.workspace_mut().project.previews.tabs.get_mut(ix)
                    {
                        tab.pinned = true;
                    }
                    this.activate_preview(ix, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.retain_previews(|i, _| i != ix, window, cx);
                }),
            )
            // 右键先切到这个标签，菜单里的操作都对着当前标签。
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    window.focus(&this.preview_focus, cx);
                    this.activate_preview(ix, cx);
                    this.open_preview_menu(event.position, cx);
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            // 落点提示画在目标标签靠近原位置的另一侧：往右拖插到它右边，往左拖插到它左边。
            .drag_over::<DraggedPreviewTab>(move |style, dragged, _, _| {
                let marker = fg.opacity(0.6);
                if dragged.ix < ix {
                    style.border_r_2().border_color(marker)
                } else if dragged.ix > ix {
                    style.border_l_2().border_color(marker)
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedPreviewTab, _, cx| {
                this.move_preview(dragged.ix, ix, cx);
            }))
    }

    /// 在 `position` 弹出当前预览标签的右键菜单。
    fn open_preview_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let previews = &self.workspace().project.previews;
        let (count, active) = (previews.tabs.len(), previews.active);
        let pinned = previews.active().is_some_and(|tab| tab.pinned);
        let item = |key: &str, action: Box<dyn Action>, enabled: bool| Some(menu_item(key, action, enabled, cx));
        let mut items = vec![
            item("preview.close", Box::new(CloseTab), true),
            item("preview.close_others", Box::new(CloseOtherPreviews), count > 1),
            item("preview.close_right", Box::new(ClosePreviewsToRight), active + 1 < count),
            item("preview.close_all", Box::new(CloseAllPreviews), true),
        ];
        if !pinned {
            items.extend([None, item("preview.keep_open", Box::new(KeepPreviewOpen), true)]);
        }
        let target = self.preview_focus.clone();
        self.open_menu(position, items, target, cx);
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
        assert_eq!(tabs(&previews), ["a", "new/b", ">new/c*"]);
    }
}
