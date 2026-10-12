//! 就地输入框：侧栏改 workspace 的名字、文件树新建和改名、GitHub Actions 页加 secret 共用。回车确定，
//! Esc 取消，点到别处也算确定；切到别的应用时窗口失去焦点，回来接着改。

use gpui::{App, Context, Div, Entity, FocusHandle, Focusable, Pixels, Subscription, Window, div, prelude::*, px};
use runode_shared_types::color::Rgb;

use crate::{
    hsla,
    text_field::{TextField, TextFieldEvent},
};

pub struct InlineEdit {
    pub field: Entity<TextField>,
    _subscriptions: [Subscription; 2],
}

impl InlineEdit {
    /// 在视图 `V` 里打开输入框并把焦点给它，`text` 的前 `select` 个字节选中，直接打字就替换掉；
    /// 报给辅助工具的名字是 `label`。结束时调 `finish`，`commit` 为真是确定、为假是取消；文字变了时
    /// 重画 `V`，旁边跟着文字变的东西（比如文件图标）也跟着变。
    pub fn new<V: 'static>(
        text: String,
        select: usize,
        label: String,
        finish: fn(&mut V, bool, &mut Window, &mut Context<V>),
        window: &mut Window,
        cx: &mut Context<V>,
    ) -> Self {
        let field = cx.new(|cx| TextField::editing(text, select, cx).with_label(label));
        // 输入框原本是搜索框：回车是「下一个」，Esc 是「关闭搜索」，在这里分别是确定和取消。
        let events = cx.subscribe_in(&field, window, move |this, _, event: &TextFieldEvent, window, cx| match event {
            TextFieldEvent::Next => finish(this, true, window, cx),
            TextFieldEvent::Dismiss => finish(this, false, window, cx),
            TextFieldEvent::Changed(_) => cx.notify(),
            TextFieldEvent::Previous => {}
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
