//! 标签里的终端区：分屏树、分隔线和拖动分隔线时的遮罩。

use gpui::{
    AnyElement, Context, CursorStyle, Div, EntityId, MouseButton, MouseDownEvent, MouseMoveEvent, StyleRefinement,
    canvas, div, prelude::*, px, relative,
};
use runode_shared_types::{
    color::Rgb,
    pane::{Axis, Node},
};

use super::{DIVIDER_GRAB_WIDTH, Divider, WindowView, model::Tab};
use crate::terminal_view::hsla;

/// 没有焦点的分屏蒙上一层背景色，这是蒙层的不透明度。
const UNFOCUSED_DIM: f32 = 0.3;

impl WindowView {
    /// 当前标签的终端区：分屏树，或者放大着的那一个终端。
    pub(super) fn render_panes(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.tab();
        if tab.zoomed || tab.root.is_leaf() {
            return self.render_leaf(tab, tab.focused, bg, cx);
        }
        self.render_node(tab, &tab.root, fg, bg, cx)
    }

    fn render_leaf(&self, tab: &Tab, id: EntityId, bg: Rgb, _: &mut Context<Self>) -> AnyElement {
        let view = tab.panes[&id].0.clone();
        let dimmed = !tab.zoomed && !tab.root.is_leaf() && id != tab.focused;
        let layout = self.layout.clone();
        div()
            .relative()
            .size_full()
            // 终端自己没变时复用上一帧画好的内容：标题栏和侧栏的转圈每一下都会重画整个窗口，
            // 不缓存的话每一下都要把所有终端格子重新排一遍。
            .child(view.cached(StyleRefinement::default().size_full()))
            .child(
                canvas(
                    move |bounds, _, _| {
                        layout.borrow_mut().panes.insert(id, bounds);
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            // 蒙层没有鼠标处理，点击照样落到下面的终端上。
            .when(dimmed, |leaf| {
                leaf.child(div().absolute().size_full().bg(hsla(bg).opacity(UNFOCUSED_DIM)))
            })
            .into_any_element()
    }

    fn render_node(&self, tab: &Tab, node: &Node<EntityId>, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> AnyElement {
        let split = match node {
            Node::Leaf(id) => return self.render_leaf(tab, *id, bg, cx),
            Node::Split(split) => split,
        };
        let (id, axis, ratio) = (split.id, split.axis, split.ratio);
        let horizontal = axis == Axis::Horizontal;
        let first = self.render_node(tab, &split.first, fg, bg, cx);
        let second = self.render_node(tab, &split.second, fg, bg, cx);
        let line = hsla(fg).opacity(0.15);
        let layout = self.layout.clone();
        div()
            .relative()
            .size_full()
            .flex()
            .when(!horizontal, |split| split.flex_col())
            .child(
                div()
                    .flex_none()
                    .map(|pane| if horizontal { pane.w(relative(ratio)).h_full() } else { pane.h(relative(ratio)).w_full() })
                    .min_w_0()
                    .min_h_0()
                    .child(first),
            )
            .child(div().flex_none().bg(line).map(|divider| {
                if horizontal { divider.w(px(1.)).h_full() } else { divider.h(px(1.)).w_full() }
            }))
            .child(div().flex_1().min_w_0().min_h_0().child(second))
            .child(
                canvas(
                    move |bounds, _, _| {
                        layout.borrow_mut().splits.insert(id, bounds);
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            // 分隔线只有一像素宽，在它两侧放一条透明的把手供拖动。
            .child(
                div()
                    .id(("divider", id))
                    .absolute()
                    .map(|handle| {
                        if horizontal {
                            handle
                                .top_0()
                                .h_full()
                                .w(px(DIVIDER_GRAB_WIDTH))
                                .left(relative(ratio))
                                .ml(px(-DIVIDER_GRAB_WIDTH / 2.))
                                .cursor(CursorStyle::ResizeLeftRight)
                        } else {
                            handle
                                .left_0()
                                .w_full()
                                .h(px(DIVIDER_GRAB_WIDTH))
                                .top(relative(ratio))
                                .mt(px(-DIVIDER_GRAB_WIDTH / 2.))
                                .cursor(CursorStyle::ResizeUpDown)
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            if event.click_count >= 2 {
                                // 双击分隔线让两边一样大。
                                this.tab_mut().root.set_ratio(id, 0.5);
                                this.save(cx);
                            } else {
                                this.dragging_divider = Some(Divider::Split(id, axis));
                            }
                            cx.notify();
                        }),
                    ),
            )
            .into_any_element()
    }

    /// 拖动分隔线期间盖在整个窗口上的一层：接住所有鼠标移动和松开，免得落进终端或侧栏。
    pub(super) fn render_divider_drag(&self, divider: Divider, cx: &mut Context<Self>) -> Div {
        let vertical = matches!(divider, Divider::Split(_, Axis::Vertical));
        div()
            .absolute()
            .size_full()
            .occlude()
            .cursor(if vertical { CursorStyle::ResizeUpDown } else { CursorStyle::ResizeLeftRight })
            // 终端在窗口上监听移动和松开，这里拦下，拖动期间它们收不到。
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                cx.stop_propagation();
                let Some(divider) = this.dragging_divider else {
                    return;
                };
                // 在窗口外松开时收不到松开事件，回来时按键已经没按着了。
                if event.pressed_button != Some(MouseButton::Left) {
                    this.dragging_divider = None;
                    this.save(cx);
                    cx.notify();
                    return;
                }
                let (id, axis) = match divider {
                    Divider::Split(id, axis) => (id, axis),
                    Divider::Sidebar => {
                        this.resize_sidebar(f32::from(event.position.x));
                        cx.notify();
                        return;
                    }
                    Divider::Changes | Divider::Files => {
                        let viewport = f32::from(window.viewport_size().width);
                        this.resize_right_panel(divider, f32::from(event.position.x), viewport);
                        cx.notify();
                        return;
                    }
                };
                let Some(bounds) = this.layout.borrow().splits.get(&id).copied() else {
                    return;
                };
                let ratio = if axis == Axis::Horizontal {
                    (event.position.x - bounds.origin.x) / bounds.size.width
                } else {
                    (event.position.y - bounds.origin.y) / bounds.size.height
                };
                this.tab_mut().root.set_ratio(id, ratio);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.dragging_divider = None;
                    this.save(cx);
                    cx.notify();
                }),
            )
    }
}
