//! 把「看改动并提交」那一页接到窗口上。页本身在 `review`：打开时建一个 `Review`，收起时丢掉。
//! 它盖在终端区上，标题栏和右侧面板照旧，打开时右侧面板切到改动页；切到任何一个标签或 workspace、按 Esc
//! 或「回到终端」就收起。每次画之前交给它当前项目的 git 状态、在忙的仓库和刚提交好的那几次；它要办的 git
//! 操作交给 Git 面板跑，和面板上的操作一起按仓库排队，本机和远端的项目都走这一条路。

use gpui::{AnyElement, Context, Entity, Focusable as _, StyleRefinement, Subscription, Window, div, prelude::*};

use super::{Page, WindowView, cards, project::SidePanel};
use crate::{
    review::{Input, Review, ReviewEvent},
    ui::{card::Card, theme::ActiveTheme as _},
};

/// 开着的 Review 页。
pub(super) struct OpenReview {
    pub(super) view: Entity<Review>,
    _events: Subscription,
}

impl WindowView {
    /// 在当前项目上打开 Review 页，右侧面板切到改动页，焦点给它；已经开着时只给焦点。看板不让走时什么都不做。
    pub(super) fn show_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review_page().is_none() {
            if !self.leave_pages(window, cx) {
                return;
            }
            self.drop_settings(cx);
            let view = cx.new(Review::new);
            let events = cx.subscribe_in(&view, window, Self::on_review_event);
            self.page = Some(Page::Review(OpenReview { view, _events: events }));
        }
        self.set_panel(Some(SidePanel::Git), window, cx);
        if let Some(page) = self.review_page() {
            window.focus(&page.view.focus_handle(cx), cx);
        }
        cx.notify();
    }

    pub(super) fn close_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drop_review() {
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// 开着的是 Review 页时收起它，不动焦点；之前开着时返回真。
    pub(super) fn drop_review(&mut self) -> bool {
        self.page.take_if(|page| matches!(page, Page::Review(_))).is_some()
    }

    fn on_review_event(
        &mut self,
        _: &Entity<Review>,
        event: &ReviewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ReviewEvent::Close => self.close_review(window, cx),
            // 预览和这一页都盖在终端区上，先收起这一页才看得到。
            ReviewEvent::OpenFile(path) => {
                self.close_review(window, cx);
                self.open_preview(path, true, false, cx);
            }
            ReviewEvent::OpenDiff(target) => {
                self.close_review(window, cx);
                self.click_diff(target.clone(), 2, cx);
            }
            ReviewEvent::File(action) => self.with_git_panel(cx, |panel, cx| panel.git_file_action(action, window, cx)),
            ReviewEvent::ApplyHunk { root, file, hunk, action } => {
                self.with_git_panel(cx, |panel, cx| panel.apply_hunk(root, file.clone(), *hunk, *action, window, cx));
            }
            // 贴进去的字在终端里，收起这一页才看得到。
            ReviewEvent::Quote(text) => {
                self.close_review(window, cx);
                self.quote_to_agent(text.clone(), window, cx);
            }
        }
    }

    /// 交给 Review 页的东西：当前项目的 git 状态，以及 Git 面板记着的在忙的仓库和刚提交好的那几次。
    fn review_input(&self, cx: &Context<Self>) -> Input {
        let workspace = self.workspace();
        let git = workspace.project.git.clone();
        let panel = self.git_panel.read(cx);
        let busy = (git.iter())
            .flat_map(|git| std::iter::once(&git.main).chain(&git.subs))
            .filter(|repo| panel.is_busy(&repo.root))
            .map(|repo| repo.root.clone())
            .collect();
        Input { git, busy, committed: panel.committed(workspace.id) }
    }

    /// Review 页开着时盖满终端区的内容；没开着时为空。卡片样式下画成一张卡片，否则垫上背景色。
    ///
    /// 缓存：它盖着的终端一输出窗口就重画，这一页只在自己的数据、滚动变了时重画。辅助工具开着时不缓存：照搬上一帧
    /// 的视图跳过 prepaint，报给辅助工具的节点就丢了。
    pub(super) fn render_review(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let view = self.review_page()?.view.clone();
        let input = self.review_input(cx);
        if view.update(cx, |page, _| page.set_input(input)) {
            // 画的时候发的通知不会再要一帧，等这一帧画完再通知。
            let view = view.clone();
            cx.defer(move |cx| view.update(cx, |_, cx| cx.notify()));
        }
        let page = if window.is_a11y_active() {
            view.into_any_element()
        } else {
            view.cached(StyleRefinement::default().size_full()).into_any_element()
        };
        let page = if cards(cx) {
            Card::new().size_full().overflow_hidden().child(page).into_any_element()
        } else {
            div().size_full().bg(cx.theme().background).child(page).into_any_element()
        };
        Some(div().absolute().top_0().left_0().size_full().child(page).into_any_element())
    }
}
