//! 一键排列分屏：在当前分屏旁边一次开出几个新 shell（同一个目录），焦点留在原来的分屏。给 agent
//! 准备好几个终端，agent 再经命令行（`runode send right …` 这类）去用；这里只开分屏，不启动
//! agent，也不往终端里打字。
//!
//! 动作 `ArrangePanes` 弹出一个小浮层选方向（右边、下边、右侧叠放）和个数（1 到 4），左右键换
//! 方向、数字键或上下键改个数、回车打开、Esc 关掉，也可以用鼠标点。

use gpui::{
    Context, Div, FocusHandle, Focusable, Hsla, KeyDownEvent, MouseButton, SharedString, Stateful, Subscription,
    Window, actions, div, prelude::*, px, relative,
};
use runode_shared_types::{
    color::Rgb,
    pane::{Axis, Node, Rect, SplitId},
};

use super::{TITLEBAR_HEIGHT, WindowView, divider_color};
use crate::ui::hsla;

actions!(
    runode,
    [
        /// 在当前分屏旁边一次开出几个新终端，先弹出浮层选方向和个数。
        ArrangePanes
    ]
);

/// 新分屏摆在哪。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Arrangement {
    /// 在原分屏右边排成一行，连同原分屏一样宽。
    Right,
    /// 在原分屏下边排成一列，连同原分屏一样高。
    Down,
    /// 原分屏占左半边，新分屏在右半边从上到下叠放、一样高。
    RightStack,
}

impl Arrangement {
    const ALL: [Self; 3] = [Self::Right, Self::Down, Self::RightStack];

    fn label(self) -> SharedString {
        match self {
            Self::Right => rust_i18n::t!("layout.right"),
            Self::Down => rust_i18n::t!("layout.down"),
            Self::RightStack => rust_i18n::t!("layout.right_stack"),
        }
        .into_owned()
        .into()
    }
}

/// 一次最多开几个。
const MAX_COUNT: usize = 4;

/// 开一个新分屏的一步：把 `target`（`None` 是原分屏，`Some(i)` 是第 i 个新分屏）沿 `axis` 一分为
/// 二，新分屏在后（右或下），前一半占 `ratio`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Step {
    pub(super) target: Option<usize>,
    pub(super) axis: Axis,
    pub(super) ratio: f32,
}

/// 开 `count` 个新分屏的各步，第 i 步开出第 i 个新分屏。每次都从上一块里切下一份，比例让切完以后
/// 同一排的各块一样大。
pub(super) fn plan(arrangement: Arrangement, count: usize) -> Vec<Step> {
    // 剩下 `left` 块（含被切的这块）要一样大时，前一块占的比例。
    let share = |left: usize| 1. / left as f32;
    (0..count)
        .map(|i| {
            let previous = i.checked_sub(1);
            match arrangement {
                Arrangement::Right => Step { target: previous, axis: Axis::Horizontal, ratio: share(count + 1 - i) },
                Arrangement::Down => Step { target: previous, axis: Axis::Vertical, ratio: share(count + 1 - i) },
                Arrangement::RightStack if i == 0 => Step { target: None, axis: Axis::Horizontal, ratio: 0.5 },
                Arrangement::RightStack => Step { target: previous, axis: Axis::Vertical, ratio: share(count + 1 - i) },
            }
        })
        .collect()
}

/// 按 `steps` 在 `root` 里把 `original` 切开，放进 `new` 里的各个新分屏；`split_ids` 是各步新建的
/// 分屏节点的标识。三者一样长。
pub(super) fn apply<T: Copy + PartialEq>(
    root: &mut Node<T>,
    original: T,
    new: &[T],
    split_ids: &[SplitId],
    steps: &[Step],
) {
    for ((step, &leaf), &id) in steps.iter().zip(new).zip(split_ids) {
        let target = step.target.map_or(original, |i| new[i]);
        root.split(target, leaf, step.axis, id);
        root.set_ratio(id, step.ratio);
    }
}

/// 开着的排列浮层：选中的方向和个数。
pub(super) struct ArrangePicker {
    arrangement: Arrangement,
    count: usize,
    focus: FocusHandle,
    _blur: Subscription,
}

impl WindowView {
    /// 打开排列浮层；已经开着时关掉。
    pub(super) fn arrange_panes(&mut self, _: &ArrangePanes, window: &mut Window, cx: &mut Context<Self>) {
        if self.arrange_picker.is_some() {
            self.close_arrange_picker(window, cx);
            return;
        }
        let focus = cx.focus_handle();
        // 点到别处就关掉；切到别的应用时窗口失去焦点，回来接着用。
        let blur = cx.on_blur(&focus, window, |this, window, cx| {
            if window.is_window_active() {
                this.arrange_picker = None;
                cx.notify();
            }
        });
        window.focus(&focus, cx);
        self.arrange_picker = Some(ArrangePicker { arrangement: Arrangement::Right, count: 2, focus, _blur: blur });
        cx.notify();
    }

    /// 关掉排列浮层，焦点还给当前终端。
    fn close_arrange_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.arrange_picker.take().is_some() {
            window.focus(&self.tab().focused_view().focus_handle(cx), cx);
            cx.notify();
        }
    }

    fn arrange_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() {
            return;
        }
        let Some(picker) = &mut self.arrange_picker else {
            return;
        };
        let at = Arrangement::ALL.iter().position(|a| *a == picker.arrangement).unwrap_or(0);
        let len = Arrangement::ALL.len();
        match keystroke.key.as_str() {
            "left" => picker.arrangement = Arrangement::ALL[(at + len - 1) % len],
            "right" | "tab" => picker.arrangement = Arrangement::ALL[(at + 1) % len],
            "up" => picker.count = (picker.count + 1).min(MAX_COUNT),
            "down" => picker.count = picker.count.saturating_sub(1).max(1),
            "enter" => {
                self.confirm_arrange(window, cx);
                cx.stop_propagation();
                return;
            }
            "escape" => {
                self.close_arrange_picker(window, cx);
                cx.stop_propagation();
                return;
            }
            key => match key.parse::<usize>() {
                Ok(n) if (1..=MAX_COUNT).contains(&n) => picker.count = n,
                _ => return,
            },
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// 按浮层里选的开分屏，关掉浮层。
    fn confirm_arrange(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &self.arrange_picker else {
            return;
        };
        let (arrangement, count) = (picker.arrangement, picker.count);
        self.close_arrange_picker(window, cx);
        self.arrange(arrangement, count, window, cx);
    }

    /// 在当前分屏旁边按 `arrangement` 开 `count` 个新终端，都在当前终端的目录里；焦点不动。
    /// 有终端起不来时开出几个算几个。
    fn arrange(&mut self, arrangement: Arrangement, count: usize, window: &mut Window, cx: &mut Context<Self>) {
        let original = self.tab().focused;
        let mut entries = Vec::new();
        for _ in 0..count {
            let Some(view) = self.spawn_beside_focused(window, cx) else {
                break;
            };
            entries.push(self.pane_entry(view, window, cx));
        }
        if entries.is_empty() {
            return;
        }
        let steps = plan(arrangement, entries.len());
        let new: Vec<_> = entries.iter().map(|(id, _)| *id).collect();
        let split_ids: Vec<_> = steps.iter().map(|_| self.next_id()).collect();
        let tab = self.tab_mut();
        apply(&mut tab.root, original, &new, &split_ids, &steps);
        tab.panes.extend(entries);
        // 放大着时别的分屏都看不见，新开的也看不见。
        tab.zoomed = false;
        window.focus(&self.tab().focused_view().focus_handle(cx), cx);
        self.save(cx);
        cx.notify();
    }

    /// 浮在标题栏下方正中的排列浮层：方向、个数、排出来的样子和打开按钮。
    pub(super) fn render_arrange_picker(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Option<Div> {
        let picker = self.arrange_picker.as_ref()?;
        let panel_bg = hsla(bg.mix(fg, 0.05));
        let selected_bg = hsla(bg.mix(fg, 0.16));
        let hover_bg = hsla(bg.mix(fg, 0.09));
        let open_hover_bg = hsla(bg.mix(fg, 0.22));
        let fg = hsla(fg);
        let choice = |id: (&'static str, usize), label: SharedString, selected: bool| -> Stateful<Div> {
            div()
                .id(id)
                .px(px(10.))
                .py(px(4.))
                .rounded(px(5.))
                .border_1()
                .border_color(fg.opacity(if selected { 0.35 } else { 0.12 }))
                .map(|button| if selected { button.bg(selected_bg) } else { button.hover(|b| b.bg(hover_bg)) })
                .child(label)
        };
        let directions = Arrangement::ALL.iter().enumerate().map(|(ix, &arrangement)| {
            choice(("arrange-direction", ix), arrangement.label(), arrangement == picker.arrangement).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Some(picker) = &mut this.arrange_picker {
                        picker.arrangement = arrangement;
                    }
                    cx.notify();
                }),
            )
        });
        let counts = (1..=MAX_COUNT).map(|count| {
            choice(("arrange-count", count), count.to_string().into(), count == picker.count).on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Some(picker) = &mut this.arrange_picker {
                        picker.count = count;
                    }
                    cx.notify();
                }),
            )
        });
        let row = |label: SharedString| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(div().w(px(48.)).flex_none().text_color(fg.opacity(0.6)).child(label))
        };
        let open = div()
            .id("arrange-open")
            .px(px(14.))
            .py(px(5.))
            .rounded(px(5.))
            .bg(selected_bg)
            .hover(|button| button.bg(open_hover_bg))
            .child(rust_i18n::t!("layout.open").into_owned())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.confirm_arrange(window, cx);
                }),
            );
        let panel = div()
            .id("arrange-picker")
            .track_focus(&picker.focus)
            .on_key_down(cx.listener(Self::arrange_key))
            .w(px(380.))
            .max_w_full()
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .rounded(px(8.))
            .bg(panel_bg)
            .border_1()
            .border_color(fg.opacity(0.15))
            .shadow_md()
            .occlude()
            .text_size(px(12.))
            .text_color(fg)
            .child(div().text_color(fg).child(rust_i18n::t!("layout.title").into_owned()))
            .child(row(rust_i18n::t!("layout.direction").into_owned().into()).children(directions))
            .child(row(rust_i18n::t!("layout.count").into_owned().into()).children(counts))
            .child(
                div()
                    .flex()
                    .items_end()
                    .justify_between()
                    .gap(px(10.))
                    .child(preview(picker.arrangement, picker.count, fg))
                    .child(open),
            )
            .child(
                div()
                    .pt(px(8.))
                    .border_t_1()
                    .border_color(divider_color(fg))
                    .text_size(px(11.))
                    .text_color(fg.opacity(0.45))
                    .child(rust_i18n::t!("layout.hint").into_owned()),
            );
        // 外层铺满窗口宽度、只为把浮层摆到正中，没有鼠标处理，不挡下面的点击。
        Some(
            div()
                .absolute()
                .top(px(TITLEBAR_HEIGHT + 8.))
                .left_0()
                .right_0()
                .px(px(16.))
                .flex()
                .justify_center()
                .child(panel),
        )
    }
}

/// 排出来的样子：一个小方框里按比例画出原分屏（实心）和新分屏。
fn preview(arrangement: Arrangement, count: usize, fg: Hsla) -> Div {
    const WIDTH: f32 = 120.;
    const HEIGHT: f32 = 72.;
    let mut root = Node::Leaf(0);
    let new: Vec<usize> = (1..=count).collect();
    let split_ids: Vec<SplitId> = (1..=count as u64).collect();
    apply(&mut root, 0, &new, &split_ids, &plan(arrangement, count));
    let area = Rect { x: 0., y: 0., width: 1., height: 1. };
    div().relative().flex_none().w(px(WIDTH)).h(px(HEIGHT)).children(root.rects(area).into_iter().map(
        |(leaf, rect)| {
            div()
                .absolute()
                .left(relative(rect.x))
                .top(relative(rect.y))
                .w(relative(rect.width))
                .h(relative(rect.height))
                .p(px(1.5))
                .child(
                    div()
                        .size_full()
                        .rounded(px(3.))
                        .border_1()
                        .border_color(fg.opacity(0.4))
                        .when(leaf == 0, |pane| pane.bg(fg.opacity(0.25))),
                )
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 从单个原分屏 0 开出 `count` 个，各块在 0..120 见方里的位置，取整。
    fn arranged(arrangement: Arrangement, count: usize) -> Vec<(u32, [i32; 4])> {
        let mut root = Node::Leaf(0);
        let new: Vec<u32> = (1..=count as u32).collect();
        let ids: Vec<SplitId> = (1..=count as u64).collect();
        apply(&mut root, 0, &new, &ids, &plan(arrangement, count));
        let area = Rect { x: 0., y: 0., width: 120., height: 120. };
        root.rects(area)
            .into_iter()
            .map(|(leaf, r)| (leaf, [r.x, r.y, r.width, r.height].map(|v| v.round() as i32)))
            .collect()
    }

    #[test]
    fn right_and_down_make_equal_columns_and_rows() {
        assert_eq!(
            arranged(Arrangement::Right, 3),
            [(0, [0, 0, 30, 120]), (1, [30, 0, 30, 120]), (2, [60, 0, 30, 120]), (3, [90, 0, 30, 120])]
        );
        assert_eq!(
            arranged(Arrangement::Down, 2),
            [(0, [0, 0, 120, 40]), (1, [0, 40, 120, 40]), (2, [0, 80, 120, 40])]
        );
        assert_eq!(arranged(Arrangement::Right, 1), [(0, [0, 0, 60, 120]), (1, [60, 0, 60, 120])]);
    }

    #[test]
    fn right_stack_keeps_the_left_half_and_stacks_the_rest() {
        assert_eq!(
            arranged(Arrangement::RightStack, 3),
            [(0, [0, 0, 60, 120]), (1, [60, 0, 60, 40]), (2, [60, 40, 60, 40]), (3, [60, 80, 60, 40])]
        );
        assert_eq!(arranged(Arrangement::RightStack, 1), arranged(Arrangement::Right, 1));
        let four = arranged(Arrangement::RightStack, 4);
        assert_eq!(four.len(), 5);
        assert!(four[1..].iter().all(|(_, [x, _, w, h])| *x == 60 && *w == 60 && *h == 30));
    }

    #[test]
    fn only_the_original_pane_is_divided() {
        // 原分屏 0 本来在右半边，左边的 9 不动。
        let mut root = Node::Leaf(9);
        root.split(9, 0, Axis::Horizontal, 100);
        apply(&mut root, 0, &[1, 2], &[1, 2], &plan(Arrangement::Down, 2));
        let rects: Vec<_> = root
            .rects(Rect { x: 0., y: 0., width: 120., height: 120. })
            .into_iter()
            .map(|(leaf, r)| (leaf, [r.x, r.y, r.width, r.height].map(|v| v.round() as i32)))
            .collect();
        assert_eq!(rects, [(9, [0, 0, 60, 120]), (0, [60, 0, 60, 40]), (1, [60, 40, 60, 40]), (2, [60, 80, 60, 40])]);
    }
}
