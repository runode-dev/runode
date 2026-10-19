//! 就地输入框：回车确定，
//! Esc 取消，点到别处也算确定；切到别的应用时窗口失去焦点，回来接着改。

use gpui::{App, Context, Entity, FocusHandle, Focusable, Pixels, Subscription, Window, prelude::*, px};

use crate::input::{Input, InputEvent, InputState};

pub struct InlineEdit {
    pub field: Entity<InputState>,
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
        let field = cx.new(|cx| InputState::editing(text, select, cx).with_label(label));
        // 输入框原本是搜索框：回车是「下一个」，Esc 是「关闭搜索」，在这里分别是确定和取消。
        let events = cx.subscribe_in(&field, window, move |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Next => finish(this, true, window, cx),
            InputEvent::Dismiss => finish(this, false, window, cx),
            InputEvent::Changed(_) => cx.notify(),
            InputEvent::Previous => {}
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

    /// 带边框的输入框，高 `height`，比别处的 `Input` 留白和圆角小一些，好塞进一行里。
    pub fn render(&self, height: Pixels) -> Input {
        Input::new(&self.field).h(height).px(px(3.)).rounded(px(3.))
    }
}
