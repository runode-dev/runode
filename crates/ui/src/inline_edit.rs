//! 就地输入框：回车确定，
//! Esc 取消，点到别处默认也算确定（点到不拿焦点的空白处也算），`cancel_on_leave` 的算取消；切到别的应用时
//! 窗口失去焦点，回来接着改。

use std::{cell::Cell, rc::Rc};

use gpui::{App, Context, Entity, FocusHandle, Focusable, Pixels, Subscription, Window, prelude::*, px};

use crate::input::{Input, InputEvent, InputState};

type Leave = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct InlineEdit {
    pub field: Entity<InputState>,
    /// 在输入框外按下鼠标时结束。点到不拿焦点的地方焦点不动，光靠失焦结束不了。
    leave: Leave,
    /// 失焦、点到框外时算确定；为假时算取消。
    commit_on_leave: Rc<Cell<bool>>,
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
        // 复用搜索框的事件：`Next`（回车）在这里是确定，`Dismiss`（Esc）是取消。
        let events = cx.subscribe_in(&field, window, move |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Next => finish(this, true, window, cx),
            InputEvent::Dismiss => finish(this, false, window, cx),
            InputEvent::Changed(_) => cx.notify(),
            InputEvent::Previous => {}
        });
        let commit_on_leave = Rc::new(Cell::new(true));
        let focus = field.focus_handle(cx);
        let blur = cx.on_blur(&focus, window, {
            let commit_on_leave = commit_on_leave.clone();
            move |this, window, cx| {
                if window.is_window_active() {
                    finish(this, commit_on_leave.get(), window, cx);
                }
            }
        });
        window.focus(&focus, cx);
        let view = cx.weak_entity();
        let leave = Rc::new({
            let commit_on_leave = commit_on_leave.clone();
            move |window: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| finish(this, commit_on_leave.get(), window, cx)).ok();
            }
        });
        Self { field, leave, commit_on_leave, _subscriptions: [events, blur] }
    }

    /// 失焦、点到框外时算取消，只有回车才确定：确定以后要建出东西的输入（比如新分支名），打到一半点开不能就建。
    pub fn cancel_on_leave(self) -> Self {
        self.commit_on_leave.set(false);
        self
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
        let leave = self.leave.clone();
        Input::new(&self.field)
            .h(height)
            .px(px(3.))
            .rounded(px(3.))
            .on_mouse_down_out(move |_, window, cx| leave(window, cx))
    }
}
