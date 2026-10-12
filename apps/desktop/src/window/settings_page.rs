//! 设置页（`settings::SettingsView`）铺在窗口里：盖住侧栏、终端和状态栏，整个窗口都是设置。它发
//! `settings::Close`（返回、Esc、关分屏的键）时收起；按快捷键或点菜单办窗口上的动作（切标签、新开
//! 终端等，见 `act`）、打开手机端引导页、拖文件夹进来开 workspace、跳到某个分屏（点通知、跳 agent）
//! 时也先收起，结果看得见；设置页开着时菜单里的「关闭」只收起设置页。收起前把还没写回的输入框写回。
//! 后台的终端退出、别处开了新终端时不收起，焦点也留在设置页（`WindowView` 的 `focus_handle` 开着
//! 设置页时给的是它）。

use gpui::{Action, App, AppContext as _, Context, Entity, Focusable as _, Subscription, Window};

use super::WindowView;
use crate::settings::{Close, SettingsView};

pub(super) struct SettingsPage {
    pub(super) view: Entity<SettingsView>,
    _close: Subscription,
}

/// 菜单里的「设置…」：在最前面的窗口里打开设置页；一个窗口都没有时先开一个。
///
/// 按 ⌘, 或点菜单时，GPUI 正在那个窗口里派发这个动作，窗口被借走了，这时 `update` 它会失败；
/// 等这一轮派发结束再打开。
pub(crate) fn show_settings(cx: &mut App) {
    cx.defer(|cx| {
        if super::remote::windows(cx).is_empty() {
            super::open_window(cx, None);
        }
        let Some(handle) = super::remote::front_window(cx) else { return };
        if let Err(err) = handle.update(cx, |view, window, cx| {
            window.activate_window();
            view.open_settings(window, cx);
        }) {
            tracing::warn!("failed to open the settings page: {err:#}");
        }
    });
}

impl WindowView {
    /// 打开设置页；已经开着时把焦点给它。
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mobile = None;
        self.new_workspace = None;
        self.add_task = None;
        self.commit_message_dialog = None;
        let page = self.settings.get_or_insert_with(|| {
            let view = cx.new(|cx| SettingsView::new(window, cx));
            let close =
                cx.subscribe_in(&view, window, |this, _, _: &Close, window, cx| this.close_settings(window, cx));
            SettingsPage { view, _close: close }
        });
        window.focus(&page.view.focus_handle(cx), cx);
        cx.notify();
    }

    /// 窗口上动作的处理函数 `f`，设置页开着时先收起它。
    pub(super) fn act<A: Action>(
        cx: &mut Context<Self>,
        f: fn(&mut Self, &A, &mut Window, &mut Context<Self>),
    ) -> impl Fn(&A, &mut Window, &mut App) + 'static {
        cx.listener(move |this, action, window, cx| {
            this.close_settings(window, cx);
            f(this, action, window, cx);
        })
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drop_settings(cx) {
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// 收起设置页，不动焦点；之前开着时返回真。
    pub(super) fn drop_settings(&mut self, cx: &mut App) -> bool {
        let Some(page) = self.settings.take() else { return false };
        page.view.update(cx, |view, cx| view.commit_all(cx));
        true
    }
}
