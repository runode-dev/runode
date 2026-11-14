//! 侧栏导航里的「手机」：窗口主区域换成配对设备页（`mobile_page`）。切到任何一个标签或 workspace
//! （`activate`）、按 Esc 就收起。

use gpui::{
    AnyElement, App, Context, Entity, Focusable as _, MouseButton, StyleRefinement, Subscription, Window,
    WindowControlArea, div, prelude::*, px,
};

use super::{CARD_GAP, TITLEBAR_HEIGHT, WindowView, cards, drag_window};
use crate::{
    config::AppConfig,
    mobile_page::{MobileEvent, MobilePage, Step},
    ui::card::Card,
};

/// 开着的配对设备页。
pub(super) struct OpenMobile {
    pub(super) view: Entity<MobilePage>,
    _events: Subscription,
}

/// 设置里的「配对…」：在最前面的窗口里打开引导页、直接到配对这一步；一个窗口都没有时先开一个。
pub(crate) fn show_pairing(cx: &mut App) {
    if super::remote::windows(cx).is_empty() {
        super::open_window(cx, None);
    }
    let Some(handle) = super::remote::front_window(cx) else { return };
    let _ = handle.update(cx, |view, window, cx| {
        window.activate_window();
        view.show_mobile(window, cx);
        if let Some(page) = view.mobile_page().map(|page| page.view.clone()) {
            page.update(cx, |page, cx| page.go(Step::Pair, cx));
        }
    });
}

/// 在后台跟着配对过的设备，只在有窗口开着配对设备页时读；远程访问开着时，露着侧栏（导航上写着连着的
/// 设备）的窗口也算。
pub fn watch_devices(cx: &mut App) {
    crate::mobile_page::devices::watch(cx, |cx| {
        let listening = cx.global::<AppConfig>().0.remote_access;
        super::remote::windows(cx).iter().any(|handle| {
            handle.read(cx).is_ok_and(|view| view.mobile_page().is_some() || (listening && view.sidebar_visible()))
        })
    });
}

impl WindowView {
    /// 打开配对设备页；已经开着时把焦点给它。
    pub(super) fn show_mobile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.leave_todo_page(window, cx) {
            return;
        }
        self.drop_settings(cx);
        self.drop_kanban();
        self.leave_screen_page(window, cx);
        if self.mobile_page().is_none() {
            let view = cx.new(|cx| MobilePage::new(window, cx));
            let events = cx.subscribe_in(&view, window, |this, _, event: &MobileEvent, window, cx| match event {
                MobileEvent::Close => this.close_mobile(window, cx),
            });
            self.page = Some(super::Page::Mobile(OpenMobile { view, _events: events }));
        }
        if let Some(page) = self.mobile_page() {
            window.focus(&page.view.focus_handle(cx), cx);
        }
        cx.notify();
    }

    pub(super) fn close_mobile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mobile_page().is_some() {
            self.leave_mobile(cx);
            window.focus(&self.focus_handle(cx), cx);
            cx.notify();
        }
    }

    /// 收起配对设备页，输入框里改了的电脑名先写回（`MobilePage::save_machine_name`）。
    pub(super) fn leave_mobile(&mut self, cx: &mut Context<Self>) {
        if let Some(page) = self.mobile_page().map(|page| page.view.clone()) {
            page.update(cx, |page, cx| page.save_machine_name(cx));
            self.page = None;
        }
    }

    /// 引导页开着时窗口的内容。缓存：窗口跟着终端输出几乎每帧都重画，这一页只在自己的状态变了时重画。辅助工具
    /// 开着时不缓存，GPUI 才会逐帧重建报给辅助工具的节点。这一页里转着的盲文格紧跟在它后面画
    /// （`MobilePage::spinner_overlay`）。
    pub(super) fn render_mobile_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(page) = self.mobile_page().map(|page| page.view.clone()) else { return Vec::new() };
        let overlay = page.read(cx).spinner_overlay();
        let page = if window.is_a11y_active() {
            page.into_any_element()
        } else {
            page.cached(StyleRefinement::default().size_full()).into_any_element()
        };
        let page = div().size_full().child(page).child(overlay).into_any_element();
        self.render_page_body(page, true, window, cx)
    }
}

impl WindowView {
    /// 铺满窗口的一页（配对设备页、看板、远程桌面）开着时窗口的内容：侧栏（收着时没有），右边整块是 `page`，
    /// 顶上留一条拖动窗口、放红绿灯。`rounded_bottom` 为假时卡片样式下那块的底下两个角是直的。
    pub(super) fn render_page_body(
        &mut self,
        page: AnyElement,
        rounded_bottom: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let fullscreen = window.is_fullscreen();
        let cards = cards(cx);
        let sidebar = self.sidebar_visible().then(|| self.render_sidebar(window, cx));
        let handle = sidebar.is_some().then(|| self.render_sidebar_handle(cx));
        let toggle = self.render_sidebar_toggle(fullscreen, cx);
        let main = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(TITLEBAR_HEIGHT))
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(MouseButton::Left, drag_window),
            )
            .child(if cards {
                Card::new()
                    .when(!rounded_bottom, |card| card.rounded_b(px(0.)))
                    .flex_1()
                    .min_h_0()
                    .mr(px(CARD_GAP))
                    .mb(px(CARD_GAP))
                    .overflow_hidden()
                    .child(page)
                    .into_any_element()
            } else {
                div().flex_1().min_h_0().child(page).into_any_element()
            });
        let mut body: Vec<AnyElement> = Vec::new();
        body.extend(sidebar.map(IntoElement::into_any_element));
        if cards {
            body.push(div().flex_none().w(px(CARD_GAP)).into_any_element());
        }
        body.push(main.into_any_element());
        body.extend(handle.map(IntoElement::into_any_element));
        body.push(toggle.into_any_element());
        body
    }
}
