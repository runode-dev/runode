//! 让辅助工具（VoiceOver、CUA 这类读 AX 树的自动化工具）认得界面：GPUI 只把同时有 `id` 和 `role`
//! 的元素报出去，名字用 `aria_label`（`Role::Label` 除外，用 `StaticText`），状态用 `aria_toggled`、
//! `aria_selected`、`aria_value` 这些。
//! 这里放几处界面共用的写法。

use std::rc::Rc;

use gpui::{
    AccessibleAction, ClickEvent, Context, MouseButton, MouseDownEvent, Role, SharedString, StatefulInteractiveElement,
    WeakEntity, Window,
};

/// 点击或辅助工具按下时调 `f`。
///
/// GPUI 默认把辅助工具的按下换成在元素中心合成一次鼠标点击，元素滚出了可见区域、或者被别的东西
/// 盖住时就点不到，什么也不报；这里另外登记按下动作，直接调同一个 `f`。
pub trait Press: StatefulInteractiveElement + Sized {
    fn on_press<T: 'static>(
        self,
        cx: &mut Context<T>,
        f: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
    ) -> Self {
        let f = Rc::new(f);
        let press = f.clone();
        let view = cx.entity().downgrade();
        self.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| f(this, window, cx)))
            .on_a11y_press(view, move |this, window, cx| press(this, window, cx))
    }
}

impl<E: StatefulInteractiveElement> Press for E {}

/// 按下鼠标左键或辅助工具按下时调 `f`，按下的事件不再往外传。
///
/// 给按下鼠标就办、不等松开的按钮用，比如标题栏上要拦住外面按下就拖窗口的按钮；只挂 `on_mouse_down`
/// 的元素辅助工具按不到，这里同样另外登记按下动作。要弹菜单的按钮在 `f` 里取 `Window::mouse_position`：
/// 鼠标按下时它就是按下的位置，辅助工具按下时是鼠标当前所在的位置。
pub trait PressDown: StatefulInteractiveElement + Sized {
    fn on_press_down<T: 'static>(
        self,
        cx: &mut Context<T>,
        f: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
    ) -> Self {
        let f = Rc::new(f);
        let press = f.clone();
        let view = cx.entity().downgrade();
        self.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                f(this, window, cx);
            }),
        )
        .on_a11y_press(view, move |this, window, cx| press(this, window, cx))
    }
}

impl<E: StatefulInteractiveElement> PressDown for E {}

/// 辅助工具按下时调 `f`，`view` 已经没了时什么也不做。
///
/// 给 `Press`、`PressDown` 套不上的元素另外登记按下动作：鼠标的处理要看点了几下、分在捕获和冒泡两段，
/// 或者 `cx` 正被借着。
///
/// 报了展开状态（`aria_expanded`）的按钮，Windows 的 UI 自动化只给「展开、收起」，不给按下，所以这两个
/// 动作也调 `f`。自己另外登记展开、收起的元素（目录树的行这类）不能用它，不然一次展开会办两回。
pub trait A11yPress: StatefulInteractiveElement + Sized {
    fn on_a11y_press<T: 'static>(
        self,
        view: WeakEntity<T>,
        f: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
    ) -> Self {
        let f = Rc::new(f);
        [AccessibleAction::Click, AccessibleAction::Expand, AccessibleAction::Collapse].into_iter().fold(
            self,
            |element, action| {
                let (view, f) = (view.clone(), f.clone());
                element.on_a11y_action(action, move |_, window, cx| {
                    view.update(cx, |this, cx| f(this, window, cx)).ok();
                })
            },
        )
    }
}

impl<E: StatefulInteractiveElement> A11yPress for E {}

/// 报给辅助工具「不可用」：灰着、按了不办的按钮和菜单项。
///
/// GPUI 没有直接设它的写法，借合成子节点的回调改这个元素自己的节点；所以同一个元素不能再另用
/// `a11y_synthetic_children`。
pub trait Disable: StatefulInteractiveElement + Sized {
    fn aria_disabled(self, disabled: bool) -> Self {
        if disabled { self.a11y_synthetic_children(|builder| builder.parent_node().set_disabled()) } else { self }
    }
}

impl<E: StatefulInteractiveElement> Disable for E {}

/// 连同里面的东西一起不报给辅助工具：模态对话框开着时盖住的那部分界面，不然辅助工具照样读得到、按得到
/// 对话框后面的按钮。
///
/// 只在藏起来时才成为一个节点，平时不多出一层。和 `Disable` 一样借合成子节点的回调改节点。
pub trait Hide: StatefulInteractiveElement + Sized {
    fn aria_hidden(self, hidden: bool) -> Self {
        if hidden {
            self.role(Role::Group).a11y_synthetic_children(|builder| builder.parent_node().set_hidden())
        } else {
            self
        }
    }
}

impl<E: StatefulInteractiveElement> Hide for E {}

/// 报成一段静态文字（`Role::Label`），文字是 `text`：出错、空状态、加载中这些只起提示作用的字，以及
/// 只读的一行行内容。
///
/// AccessKit 的 `Role::Label` 从 value 取文字，Windows 和 Linux 的辅助工具把它当名字读，只给
/// `aria_label` 时名字是空的；macOS 的 VoiceOver 读静态文字的 `AXValue`，label 和 value 都给可能读两遍。
/// 所以同一个元素不要再给 `aria_label`；GPUI 也不收子元素里的文字，没给 `text` 就什么也读不到。
pub trait StaticText: StatefulInteractiveElement + Sized {
    fn aria_static_text(self, text: impl Into<SharedString>) -> Self {
        self.role(Role::Label).aria_value(text)
    }
}

impl<E: StatefulInteractiveElement> StaticText for E {}
