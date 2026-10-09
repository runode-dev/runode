//! 终端里的搜索：打开和关闭搜索栏、转发它的事件、切换匹配，以及画右上角的搜索栏。

use gpui::{
    AppContext as _, Context, CursorStyle, Entity, Focusable, Role, SharedString, Window, actions, div, prelude::*, px,
};
use runode_terminal::session::Session;

use super::TerminalView;
use crate::ui::{
    a11y::Press,
    hsla,
    text_field::{EndSearch, SearchNext, SearchPrevious, TextField, TextFieldEvent},
    tooltip::tooltip,
};

actions!(
    runode,
    [
        /// 打开搜索栏；已经打开时把焦点移过去。
        StartSearch,
        /// 用当前选区的文字搜索。
        SearchSelection
    ]
);

impl TerminalView {
    pub(super) fn start_search(&mut self, _: &StartSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(None, window, cx);
    }

    pub(super) fn search_selection(&mut self, _: &SearchSelection, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.screen.shown().and_then(Session::selection_text) {
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
                let field =
                    cx.new(|cx| TextField::new(String::new(), cx).with_label(rust_i18n::t!("search.placeholder")));
                let events = cx.subscribe_in(&field, window, Self::handle_search_event);
                self.search_field = Some((field.clone(), events));
                field
            }
        };
        if let Some(query) = query {
            if let Some(session) = self.screen.shown_mut() {
                session.search(&query);
            }
            field.update(cx, |field, cx| field.set_query(query, cx));
        }
        window.focus(&field.focus_handle(cx), cx);
        cx.notify();
    }

    fn handle_search_event(
        &mut self,
        _: &Entity<TextField>,
        event: &TextFieldEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (event, self.screen.shown_mut()) {
            (TextFieldEvent::Dismiss, _) => self.close_search(window, cx),
            (TextFieldEvent::Changed(query), Some(session)) => session.search(query),
            (TextFieldEvent::Next, Some(session)) => session.search_step(false),
            (TextFieldEvent::Previous, Some(session)) => session.search_step(true),
            // 没有界面这份 VT 时没有可搜的，回到显示时按搜索栏里的词重新搜，见 `vt_replaced`。
            (_, None) => {}
        }
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_field = None;
        if let Some(session) = self.screen.shown_mut() {
            session.end_search();
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// 搜索栏开着时切到下一个或上一个匹配；没开时这些键不做事。
    pub(super) fn search_next(&mut self, _: &SearchNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.is_some()
            && let Some(session) = self.screen.shown_mut()
        {
            session.search_step(false);
            cx.notify();
        }
    }

    pub(super) fn search_previous(&mut self, _: &SearchPrevious, _: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.is_some()
            && let Some(session) = self.screen.shown_mut()
        {
            session.search_step(true);
            cx.notify();
        }
    }

    pub(super) fn end_search(&mut self, _: &EndSearch, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_field.is_some() {
            self.close_search(window, cx);
        }
    }

    /// 右上角的搜索栏：输入框、匹配进度、上下切换和关闭按钮。
    pub(super) fn render_search_bar(
        &self,
        field: &Entity<TextField>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let frame = self.colors;
        let fg = hsla(frame.0);
        let bar_bg = hsla(frame.1.mix(frame.0, 0.1));
        let status = match self.screen.shown().and_then(Session::search_status) {
            Some((_, 0)) if !field.read(cx).query().is_empty() => rust_i18n::t!("search.no_results").into_owned(),
            Some((Some(selected), total)) => format!("{}/{total}", selected + 1),
            Some((None, total)) if total > 0 => format!("-/{total}"),
            _ => String::new(),
        };
        // `label` 是画出来的符号，`name` 是报给辅助工具的名字，和悬停提示一样。
        let button = |id: &'static str, label: &'static str, name: &SharedString| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(name.clone())
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
        let name = |key: &str| SharedString::from(rust_i18n::t!(key).into_owned());
        let (previous, next, close) =
            (name("menu.find_previous"), name("menu.find_next"), name("tooltip.close_search"));
        div()
            .id("search-bar")
            .role(Role::Search)
            .aria_label(rust_i18n::t!("search.placeholder").into_owned())
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
            // 匹配进度报成状态，辅助工具读得到「3/12」「无结果」。
            .child(
                div()
                    .id("search-status")
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .flex_none()
                    .min_w(px(36.))
                    .text_right()
                    .text_color(fg.opacity(0.6))
                    .child(status),
            )
            .child(
                button("search-previous", "↑", &previous)
                    .tooltip(tooltip(previous, Some(&SearchPrevious), frame.0, frame.1))
                    .on_press(cx, |view, _, cx| {
                        if let Some(session) = view.screen.shown_mut() {
                            session.search_step(true);
                        }
                        cx.notify();
                    }),
            )
            .child(
                button("search-next", "↓", &next).tooltip(tooltip(next, Some(&SearchNext), frame.0, frame.1)).on_press(
                    cx,
                    |view, _, cx| {
                        if let Some(session) = view.screen.shown_mut() {
                            session.search_step(false);
                        }
                        cx.notify();
                    },
                ),
            )
            .child(
                button("search-close", "×", &close)
                    .tooltip(tooltip(close, Some(&EndSearch), frame.0, frame.1))
                    .on_press(cx, |view, window, cx| view.close_search(window, cx)),
            )
    }
}
