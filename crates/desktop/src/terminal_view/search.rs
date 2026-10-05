//! 终端里的搜索：打开和关闭搜索栏、转发它的事件、切换匹配，以及画右上角的搜索栏。

use gpui::{AppContext as _, Context, CursorStyle, Entity, Focusable, Window, div, prelude::*, px};

use super::{TerminalView, hsla};
use crate::search_bar::{
    EndSearch, SearchField, SearchFieldEvent, SearchNext, SearchPrevious, SearchSelection, StartSearch,
};

impl TerminalView {
    pub(super) fn start_search(&mut self, _: &StartSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(None, window, cx);
    }

    pub(super) fn search_selection(&mut self, _: &SearchSelection, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.session.selection_text() {
            // 搜索只在一行里找，多行选区只取第一行。
            let line = text.lines().next().unwrap_or_default().to_owned();
            self.open_search(Some(line), window, cx);
        }
    }

    /// 打开搜索栏并把焦点移过去；给了 `query` 时用它替换搜索词并立即搜索。
    fn open_search(&mut self, query: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let field = match &self.search_field {
            Some((field, _)) => field.clone(),
            None => {
                let field = cx.new(|cx| SearchField::new(String::new(), cx));
                let events = cx.subscribe_in(&field, window, Self::handle_search_event);
                self.search_field = Some((field.clone(), events));
                field
            }
        };
        if let Some(query) = query {
            self.session.search(&query);
            field.update(cx, |field, cx| field.set_query(query, cx));
        }
        window.focus(&field.focus_handle(cx), cx);
        cx.notify();
    }

    fn handle_search_event(
        &mut self,
        _: &Entity<SearchField>,
        event: &SearchFieldEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SearchFieldEvent::Changed(query) => self.session.search(query),
            SearchFieldEvent::Next => self.session.search_step(false),
            SearchFieldEvent::Previous => self.session.search_step(true),
            SearchFieldEvent::Dismiss => self.close_search(window, cx),
        }
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_field = None;
        self.session.end_search();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// 搜索栏开着时切到下一个或上一个匹配；没开时这些键不做事。
    pub(super) fn search_next(&mut self, _: &SearchNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.is_some() {
            self.session.search_step(false);
            cx.notify();
        }
    }

    pub(super) fn search_previous(&mut self, _: &SearchPrevious, _: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.is_some() {
            self.session.search_step(true);
            cx.notify();
        }
    }

    pub(super) fn end_search(&mut self, _: &EndSearch, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.is_some() {
            self.close_search(window, cx);
        }
    }

    /// 右上角的搜索栏：输入框、匹配进度、上下切换和关闭按钮。
    pub(super) fn render_search_bar(&self, field: &Entity<SearchField>, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let frame = self.session.peek_colors();
        let fg = hsla(frame.0);
        let bar_bg = hsla(frame.1.mix(frame.0, 0.1));
        let status = match self.session.search_status() {
            Some((_, 0)) if !field.read(cx).query().is_empty() => rust_i18n::t!("search.no_results").into_owned(),
            Some((Some(selected), total)) => format!("{}/{total}", selected + 1),
            Some((None, total)) if total > 0 => format!("-/{total}"),
            _ => String::new(),
        };
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex_none()
                .size(px(20.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(fg.opacity(0.7))
                .hover(|button| button.bg(fg.opacity(0.15)).text_color(fg))
                .child(label)
        };
        div()
            .id("search-bar")
            .absolute()
            .top(px(8.))
            .right(px(16.))
            .w(px(320.))
            .h(px(32.))
            .pl(px(10.))
            .pr(px(4.))
            .flex()
            .items_center()
            .gap(px(4.))
            .rounded(px(6.))
            .bg(bar_bg)
            .border_1()
            .border_color(fg.opacity(0.15))
            .shadow_md()
            .occlude()
            .text_size(px(12.))
            .text_color(fg)
            .cursor(CursorStyle::Arrow)
            // 搜索词常从终端里复制而来，带着提示符图标之类的字符，界面字体没有这些字形，
            // 输入框用终端的字体。
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .font_family(self.font.family.clone())
                    .cursor(CursorStyle::IBeam)
                    .child(field.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .min_w(px(36.))
                    .text_right()
                    .text_color(fg.opacity(0.6))
                    .child(status),
            )
            .child(button("search-previous", "↑").on_click(cx.listener(|view, _, _, cx| {
                view.session.search_step(true);
                cx.notify();
            })))
            .child(button("search-next", "↓").on_click(cx.listener(|view, _, _, cx| {
                view.session.search_step(false);
                cx.notify();
            })))
            .child(button("search-close", "×").on_click(cx.listener(|view, _, window, cx| {
                view.close_search(window, cx);
            })))
    }
}
