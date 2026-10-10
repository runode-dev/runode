//! 窗口里不跟着终端输出变的几块（侧栏、标题栏、预览栏、右侧面板、状态栏），各包成一个缓存的视图。
//!
//! 终端一有输出就通知它的视图，GPUI 把它和它上面的 `WindowView` 标成要重画。这几块原先直接画在
//! `WindowView` 里，于是每来一次输出都要整个窗口重新排版、重画一遍，比画终端本身贵得多。包成缓存的
//! 视图后它们照搬上一帧，只有 `WindowView` 自己通知（它的状态变了）、它们里面的视图通知、尺寸变了
//! 或者窗口整个刷新时才重画。画的时候仍然借 `WindowView` 的方法和状态，事件也照旧交给 `WindowView`。
//!
//! 缓存的这一块是按根元素排版的，父元素的 flex 拉伸撑不到它：根元素要自己写满宽高（`w_full`、
//! `h_full` 或具体尺寸），不然会缩成内容的大小。

use gpui::{
    AnyElement, AppContext as _, Context, Empty, Entity, IntoElement, Render, Subscription, WeakEntity, Window,
};

use super::WindowView;

/// 画这一块：在 `WindowView` 上算出要的尺寸、颜色，返回这一块的元素。
pub(super) type RenderSlot = fn(&mut WindowView, &mut Window, &mut Context<WindowView>) -> AnyElement;

pub(super) struct Slot {
    view: WeakEntity<WindowView>,
    render: RenderSlot,
    _watch: Subscription,
}

impl Slot {
    pub(super) fn new(render: RenderSlot, cx: &mut Context<WindowView>) -> Entity<Self> {
        let view = cx.entity();
        cx.new(|cx| Self {
            view: view.downgrade(),
            render,
            // 这一块画的都是 `WindowView` 的状态，它一通知就跟着重画。
            _watch: cx.observe(&view, |_, _, cx| cx.notify()),
        })
    }
}

impl Render for Slot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render = self.render;
        self.view.update(cx, |view, cx| render(view, window, cx)).unwrap_or_else(|_| Empty.into_any_element())
    }
}
