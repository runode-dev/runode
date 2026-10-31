//! 把远程桌面页（`screen_viewer::ScreenViewer`）接到窗口上：盖住标签和分屏、留着侧栏，一个窗口
//! 一次看一台主机。从菜单的「远程桌面」打开当前 workspace 所在的 SSH 主机，或从设置页的 SSH 主机列表打开
//! 那一台（`show_remote_desktop`）；切到任何一个标签或 workspace、打开别的整页、点页面上的关闭时收起，
//! 收起就断开。

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, StyleRefinement, Subscription, Window,
    prelude::*,
};

use super::{ToggleRemoteDesktop, WindowView};
use crate::screen_viewer::{ScreenViewer, ScreenViewerEvent};

pub(super) struct OpenScreenViewer {
    pub(super) view: Entity<ScreenViewer>,
    _events: Subscription,
}

/// 在最前面的窗口里打开主机 `host` 的远程桌面；一个窗口都没有时先开一个。等这一轮派发结束再开：点按钮时
/// GPUI 正借着那个窗口派发事件，当场 `update` 它会失败。
pub(crate) fn show_remote_desktop(host: String, cx: &mut App) {
    cx.defer(move |cx| {
        if super::remote::windows(cx).is_empty() {
            super::open_window(cx, None);
        }
        let Some(handle) = super::remote::front_window(cx) else { return };
        if let Err(err) = handle.update(cx, |view, window, cx| {
            window.activate_window();
            view.close_settings(window, cx);
            view.show_screen_viewer(host, window, cx);
        }) {
            tracing::warn!("failed to open the remote desktop: {err:#}");
        }
    });
}

impl WindowView {
    /// 打开或收起当前 workspace 所在主机的远程桌面；workspace 在本机时不理。
    pub(super) fn toggle_remote_desktop(
        &mut self,
        _: &ToggleRemoteDesktop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.screen_viewer.is_some() {
            self.close_screen_viewer(window, cx);
        } else if let Some(host) = self.workspace().host.label() {
            self.show_screen_viewer(host.to_owned(), window, cx);
        }
    }

    /// 打开主机 `host` 的远程桌面，焦点给它；开着的是别的主机时换成这一台。
    pub(super) fn show_screen_viewer(&mut self, host: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.leave_todo_page(window, cx) {
            return;
        }
        self.leave_mobile(cx);
        self.kanban = None;
        if self.screen_viewer.as_ref().is_none_or(|open| open.view.read(cx).host() != host) {
            let view = cx.new(|cx| ScreenViewer::new(host, window, cx));
            let events = cx.subscribe_in(&view, window, |this, _, event: &ScreenViewerEvent, window, cx| match event {
                ScreenViewerEvent::Close => this.close_screen_viewer(window, cx),
            });
            self.screen_viewer = Some(OpenScreenViewer { view, _events: events });
        }
        if let Some(open) = &self.screen_viewer {
            window.focus(&open.view.focus_handle(cx), cx);
        }
        cx.notify();
    }

    pub(super) fn close_screen_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen_viewer.take().is_some() {
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// 远程桌面开着时窗口的内容：侧栏加右边整块的远程桌面页（`render_page_body`）。
    pub(super) fn render_screen_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(view) = self.screen_viewer.as_ref().map(|open| open.view.clone()) else { return Vec::new() };
        // 缓存：来一帧画面只通知这一页，窗口跟着终端输出重画时照搬上一帧。辅助工具开着时不缓存：照搬上一帧时
        // 跳过 prepaint，报给辅助工具的节点就丢了。
        let page = if window.is_a11y_active() {
            view.into_any_element()
        } else {
            view.cached(StyleRefinement::default().size_full()).into_any_element()
        };
        self.render_page_body(page, window, cx)
    }
}
