//! 首次启动的引导页（`onboarding::Onboarding`）铺在窗口里，盖住标签和分屏。它发 `OnboardingEvent::Close`
//! 时收起；切到标签或 workspace、关分屏这些离开它的操作也收起它。经这两条路收起都算走完了，之后启动不再打开；
//! 被设置页这类别的整页直接顶掉时不算走完，下次启动还会打开。

use std::cell::Cell;

use gpui::{AnyElement, App, Context, Entity, Focusable as _, Subscription, Window, prelude::*};

use super::{WindowView, cached_page};
use crate::onboarding::{Onboarding, OnboardingEvent};

thread_local! {
    /// 正在办宿主或会话转来的事。
    static BACKGROUND: Cell<bool> = const { Cell::new(false) };
}

/// 把 `BACKGROUND` 换回进来前的值，`f` 半路 panic 时也换回去。
struct Restore(bool);

impl Drop for Restore {
    fn drop(&mut self) {
        BACKGROUND.set(self.0);
    }
}

/// `background` 时把 `f` 当作后台转来的事办（别的进程经宿主的请求、标签里的程序退出），不是用户在窗口里的
/// 操作。嵌套时外层是后台，里面也算后台。
pub(super) fn with_background<R>(background: bool, f: impl FnOnce() -> R) -> R {
    let was = BACKGROUND.get();
    let _restore = Restore(was);
    BACKGROUND.set(was || background);
    f()
}

/// 正在办后台转来的事。
pub(super) fn in_background() -> bool {
    BACKGROUND.get()
}

/// 开着的引导页。
pub(super) struct OpenOnboarding {
    pub(super) view: Entity<Onboarding>,
    _events: Subscription,
}

/// 在最前面的窗口里打开引导页；一个窗口都没有时先开一个。
///
/// 点菜单时 GPUI 正在那个窗口里派发动作，窗口被借走了，这时 `update` 它会失败；等这一轮派发结束再打开。
pub(crate) fn show_onboarding(cx: &mut App) {
    cx.defer(|cx| {
        if super::remote::windows(cx).is_empty() {
            super::open_window(cx, None);
        }
        let Some(handle) = super::remote::front_window(cx) else { return };
        if let Err(err) = handle.update(cx, |view, window, cx| {
            window.activate_window();
            view.open_onboarding(window, cx);
        }) {
            tracing::warn!("failed to open the onboarding page: {err:#}");
        }
    });
}

impl WindowView {
    /// 打开引导页；已经开着时把焦点给它。
    pub(super) fn open_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.leave_todo_page(window, cx) {
            return;
        }
        self.drop_settings(cx);
        self.leave_mobile(cx);
        self.drop_kanban();
        self.drop_review();
        self.leave_screen_page(window, cx);
        if self.onboarding_page().is_none() {
            let view = cx.new(Onboarding::new);
            let events = cx.subscribe_in(&view, window, |this, _, event: &OnboardingEvent, window, cx| match event {
                OnboardingEvent::Close => this.close_onboarding(window, cx),
            });
            self.page = Some(super::Page::Onboarding(OpenOnboarding { view, _events: events }));
        }
        if let Some(page) = self.onboarding_page() {
            window.focus(&page.view.focus_handle(cx), cx);
        }
        cx.notify();
    }

    pub(super) fn close_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.onboarding_page().is_some() {
            self.leave_onboarding(cx);
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// 收起引导页并记下走完了，不动焦点。
    pub(super) fn leave_onboarding(&mut self, cx: &App) {
        if self.page.take_if(|page| matches!(page, super::Page::Onboarding(_))).is_some() {
            crate::onboarding::mark_done(cx);
        }
    }

    /// 引导页开着时窗口的内容。
    pub(super) fn render_onboarding_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(page) = self.onboarding_page().map(|page| page.view.clone()) else { return Vec::new() };
        let page = cached_page(page, window);
        self.render_page_body(page, true, window, cx)
    }
}
