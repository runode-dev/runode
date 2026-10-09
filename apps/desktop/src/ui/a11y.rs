//! 让辅助工具（VoiceOver、CUA 这类读 AX 树的自动化工具）认得界面：GPUI 只把同时有 `id` 和 `role`
//! 的元素报出去，名字用 `aria_label`，状态用 `aria_toggled`、`aria_selected`、`aria_value` 这些。
//! 这里放几处界面共用的写法。

use std::rc::Rc;

use gpui::{
    AccessibleAction, ClickEvent, Context, MouseButton, MouseDownEvent, Role, StatefulInteractiveElement, Window,
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
        self.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| f(this, window, cx))).on_a11y_action(
            AccessibleAction::Click,
            move |_, window, cx| {
                view.update(cx, |this, cx| press(this, window, cx)).ok();
            },
        )
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
        .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
            view.update(cx, |this, cx| press(this, window, cx)).ok();
        })
    }
}

impl<E: StatefulInteractiveElement> PressDown for E {}

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
