//! 就地输入框：侧栏改 workspace 的名字、文件树新建和改名共用。回车确定，Esc 取消，点到别处
//! 也算确定；切到别的应用时窗口失去焦点，回来接着改。

use gpui::{App, Context, Div, Entity, FocusHandle, Focusable, Pixels, Subscription, Window, div, prelude::*, px};
use runode_shared_types::color::Rgb;

use super::WindowView;
use crate::{
    search_bar::{SearchField, SearchFieldEvent},
    terminal_view::hsla,
};

/// 输入框结束时调的函数，`commit` 为真是确定，为假是取消。
type Finish = fn(&mut WindowView, bool, &mut Window, &mut Context<WindowView>);

pub(super) struct InlineEdit {
    pub field: Entity<SearchField>,
    _subscriptions: [Subscription; 2],
}

impl InlineEdit {
    /// 打开输入框并把焦点给它，`text` 的前 `select` 个字节选中，直接打字就替换掉。结束时调
    /// `finish`；文字变了时重画窗口，旁边跟着文字变的东西（比如文件图标）也跟着变。
    pub fn new(text: String, select: usize, finish: Finish, window: &mut Window, cx: &mut Context<WindowView>) -> Self {
        let field = cx.new(|cx| SearchField::editing(text, select, cx));
        // 输入框原本是搜索框：回车是「下一个」，Esc 是「关闭搜索」，在这里分别是确定和取消。
        let events = cx.subscribe_in(&field, window, move |this, _, event: &SearchFieldEvent, window, cx| match event {
            SearchFieldEvent::Next => finish(this, true, window, cx),
            SearchFieldEvent::Dismiss => finish(this, false, window, cx),
            SearchFieldEvent::Changed(_) => cx.notify(),
            SearchFieldEvent::Previous => {}
        });
        let focus = field.focus_handle(cx);
        let blur = cx.on_blur(&focus, window, move |this, window, cx| {
            if window.is_window_active() {
                finish(this, true, window, cx);
            }
        });
        window.focus(&focus, cx);
        Self { field, _subscriptions: [events, blur] }
    }

    /// 输入的文字，去掉首尾空白。
    pub fn text(&self, cx: &App) -> String {
        self.field.read(cx).query().trim().to_owned()
    }

    /// 结束时焦点还在输入框里（按了回车或 Esc），交给 `to`；点别处结束时焦点已经去了别处。
    pub fn release_focus(&self, to: &FocusHandle, window: &mut Window, cx: &mut App) {
        if self.field.focus_handle(cx).is_focused(window) {
            window.focus(to, cx);
        }
    }

    /// 带边框的输入框，高 `height`。
    pub fn render(&self, height: Pixels, fg: Rgb, bg: Rgb) -> Div {
        div()
            .h(height)
            .px(px(3.))
            .rounded(px(3.))
            .bg(hsla(bg))
            .border_1()
            .border_color(hsla(fg).opacity(0.3))
            .text_color(hsla(fg))
            .child(self.field.clone())
    }
}
