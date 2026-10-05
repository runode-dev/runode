//! 标题栏：窗口的标题栏设置、标签和新建标签按钮、拖动中的标签，以及标题、agent 状态标记和快捷键提示。

use gpui::{
    Action, Animation, AnimationExt, AnyElement, App, BoxShadow, Context, Div, ElementId, Hsla, MouseButton,
    MouseDownEvent, Pixels, Render, SharedString, Stateful, TitlebarOptions, Window, div, point, prelude::*, px, svg,
};
use runode_shared_types::color::Rgb;

use super::{
    AGENT_MARK_WIDTH, NEW_TAB_BUTTON_WIDTH, NewTab, SelectLastTab, SelectTab, TAB_CLOSE_SIZE, TITLEBAR_HEIGHT,
    TRAFFIC_LIGHTS_ORIGIN, WindowView,
    agents::{Mark, Status},
    divider_color,
    model::TabId,
};
use crate::{
    terminal_view::{DEFAULT_TITLE, hsla},
    tooltip::{shortcut_text, tooltip},
};

/// 标签右侧槽位的宽度，放快捷键提示或响铃标记；和关闭按钮一边一个，标题才在标签里居中。
const TAB_SIDE_SLOT: f32 = 20.;
/// 比这窄的标签只留标题：快捷键提示收起，关闭按钮悬停时浮在标题左边。
const TAB_COMPACT_WIDTH: f32 = 120.;

/// 窗口的标题栏设置：透明，终端背景一直铺到窗口顶部，标签栏画在红绿灯或侧栏右边。
pub fn titlebar_options() -> TitlebarOptions {
    TitlebarOptions {
        title: Some(DEFAULT_TITLE.into()),
        appears_transparent: true,
        traffic_light_position: Some(point(
            px(TRAFFIC_LIGHTS_ORIGIN.0),
            px(TRAFFIC_LIGHTS_ORIGIN.1),
        )),
    }
}

/// 拖动中的标签：拖动时跟着鼠标画出来，放到另一个标签上时按 `id` 挪位置。
#[derive(Clone)]
struct DraggedTab {
    id: TabId,
    /// 开始拖动时所在的位置，用来决定落点提示画在目标标签的哪一边。
    ix: usize,
    title: SharedString,
    width: Pixels,
    fg: Hsla,
    bg: Hsla,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_chip(self.width, px(TITLEBAR_HEIGHT), self.title.clone(), self.fg, self.bg).justify_center()
    }
}

/// 拖动标签或 workspace 时跟着鼠标的那张卡片：带边框和阴影的圆角块，`label` 放不下时截断。
pub(super) fn drag_chip(width: Pixels, height: Pixels, label: SharedString, fg: Hsla, bg: Hsla) -> Div {
    div()
        .w(width)
        .h(height)
        .px(px(8.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .bg(bg)
        .border_1()
        .border_color(fg.opacity(0.15))
        .shadow(vec![BoxShadow {
            color: Hsla::black().opacity(0.3),
            offset: point(px(0.), px(2.)),
            blur_radius: px(8.),
            spread_radius: px(0.),
            inset: false,
        }])
        .text_size(px(12.))
        .text_color(fg)
        .child(div().min_w_0().truncate().child(label))
}

/// 标签和侧栏 workspace 行上的关闭按钮；什么时候显示、放在哪、点了做什么由调用方接着写。
pub(super) fn close_button(id: impl Into<ElementId>, fg: Hsla) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(TAB_CLOSE_SIZE))
        .rounded(px(3.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(14.))
        .text_color(fg.opacity(0.75))
        .hover(|close| close.bg(fg.opacity(0.18)).text_color(fg))
        .child("×")
}

/// 标题栏和面板上带图标的开关按钮，图标边长 `icon_size`；悬停时底色和图标变亮，`shown` 时
/// 底色一直亮着，图标也亮一些。位置、尺寸、提示和点击由调用方接着写。
pub(super) fn icon_toggle(
    id: &'static str,
    icon: &'static str,
    icon_size: f32,
    shown: bool,
    fg: Rgb,
    bg: Rgb,
) -> Stateful<Div> {
    let hover_bg = hsla(bg.mix(fg, 0.10));
    let active_bg = hsla(bg.mix(fg, 0.07));
    let fg = hsla(fg);
    div()
        .id(id)
        .group(id)
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .when(shown, |button| button.bg(active_bg))
        .hover(|button| button.bg(hover_bg))
        .child(
            svg()
                .path(icon)
                .size(px(icon_size))
                .text_color(fg.opacity(if shown { 0.9 } else { 0.55 }))
                .group_hover(id, |icon| icon.text_color(fg)),
        )
}

/// 居中的标题，有 agent 时前面加上它的状态标记。
pub(super) fn titled(title: SharedString, mark: Option<Mark>, id: impl Into<ElementId>, fg: Hsla) -> Div {
    div()
        .min_w_0()
        .flex()
        .justify_center()
        .items_center()
        .gap(px(5.))
        .children(mark.map(|mark| agent_mark(mark, id, fg)))
        .child(div().min_w_0().truncate().child(title))
}

/// agent 的状态标记：工作中播放该 agent 自己的工作动画，空闲时是一个空心圆点，等回答是琥珀色
/// 实心圆点，干完了没看是绿色的对勾。
pub(super) fn agent_mark(mark: Mark, id: impl Into<ElementId>, fg: Hsla) -> AnyElement {
    let slot = div()
        .flex_none()
        .w(px(AGENT_MARK_WIDTH))
        .flex()
        .justify_center()
        .items_center();
    match mark.status {
        // 所有转圈的标记共用同一个时钟，同一种 agent 的几个标签一起转时步调一致。
        Status::Working => {
            let (frames, frame_time) = mark.kind.spinner();
            let period = frame_time * frames.len() as u32;
            slot.when_some(mark.kind.spinner_color(), |slot, color| slot.text_color(gpui::rgb(color)))
                .with_animation(
                    id,
                    // 每格只重画一次：转圈每动一下都要重画整个窗口，并不便宜。
                    Animation::new(period)
                        .repeat_synced()
                        .with_max_fps(1. / frame_time.as_secs_f32()),
                    move |slot, delta| {
                        let frame = (delta * frames.len() as f32) as usize;
                        slot.child(frames[frame.min(frames.len() - 1)])
                    },
                )
                .into_any_element()
        }
        Status::Idle => slot
            .child(
                div()
                    .size(px(6.))
                    .rounded_full()
                    .border_1()
                    .border_color(fg.opacity(0.6)),
            )
            .into_any_element(),
        // 等用户回答：不动的实心琥珀色圆点，比空闲显眼。
        Status::Blocked => slot
            .child(div().size(px(7.)).rounded_full().bg(gpui::rgb(AGENT_BLOCKED_COLOR)))
            .into_any_element(),
        // 干完了还没看：绿色对勾，和等回答的圆点形状也不同，不靠颜色也分得开。
        Status::Done => slot
            .text_size(px(11.))
            .text_color(gpui::rgb(AGENT_DONE_COLOR))
            .child("✓")
            .into_any_element(),
    }
}

/// agent 等用户回答时标记的颜色。
const AGENT_BLOCKED_COLOR: u32 = 0xE5A50A;
/// agent 干完了、用户还没看时标记的颜色。
const AGENT_DONE_COLOR: u32 = 0x3FB950;

/// 快捷键提示，从键位表里查，快捷键改了也跟着变：先找 `select` 的绑定，`is_last` 时再找
/// `last` 的。后加的绑定优先，显示最后一个。
pub(super) fn shortcut_hint(select: &dyn Action, last: &dyn Action, is_last: bool, cx: &App) -> Option<SharedString> {
    shortcut_text(select, cx).or_else(|| if is_last { shortcut_text(last, cx) } else { None })
}

/// 第 `ix` 个标签的快捷键提示。默认是 ⌘1 到 ⌘8 对应前八个，⌘9 对应最后一个。
fn tab_shortcut(ix: usize, len: usize, cx: &App) -> Option<SharedString> {
    shortcut_hint(&SelectTab(ix), &SelectLastTab, ix + 1 == len, cx)
}

impl WindowView {
    pub(super) fn render_tab(
        &self,
        ix: usize,
        width: Pixels,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let workspace = self.workspace();
        let tab = &workspace.tabs[ix];
        let id = tab.id;
        let title = SharedString::from(tab.focused_view().read(cx).title().to_owned());
        let mark = tab.mark(cx);
        let active = ix == workspace.active;
        let active_bg = hsla(bg.mix(fg, 0.08));
        let hover_bg = hsla(bg.mix(fg, 0.04));
        let close_tooltip = tooltip(rust_i18n::t!("menu.close_tab"), None, fg, bg);
        let fg = hsla(fg);
        let group = SharedString::from(format!("tab-{ix}"));
        // 当前标签自己就是一块亮色，和它相邻的分隔线去掉，只是不画颜色，免得宽度跳动。
        let divider = if active || ix == workspace.active + 1 {
            fg.opacity(0.)
        } else {
            divider_color(fg)
        };
        let dragged = DraggedTab {
            id,
            ix,
            title: title.clone(),
            width,
            fg,
            bg: active_bg,
        };
        let compact = width < px(TAB_COMPACT_WIDTH);
        let bell_dot = || div().size(px(6.)).rounded_full().bg(fg.opacity(0.8));
        // 右侧槽位：响铃标记优先，其次快捷键提示；紧凑时只在响铃时占一个圆点的宽度。
        let side_slot = if compact {
            tab.bell.then(|| div().flex_none().child(bell_dot()))
        } else {
            let content = if tab.bell {
                bell_dot().into_any_element()
            } else {
                div()
                    .text_size(px(11.))
                    .text_color(fg.opacity(0.35))
                    .children(tab_shortcut(ix, workspace.tabs.len(), cx))
                    .into_any_element()
            };
            Some(
                div()
                    .flex_none()
                    .w(px(TAB_SIDE_SLOT))
                    .flex()
                    .justify_end()
                    .items_center()
                    .child(content),
            )
        };
        div()
            .id(("tab", ix))
            .group(group.clone())
            .flex_none()
            .w(width)
            .h_full()
            .overflow_hidden()
            .flex()
            .items_center()
            .px(px(if compact { 4. } else { 8. }))
            .gap(px(4.))
            .when(ix > 0, |tab| tab.border_l_1().border_color(divider))
            .map(|tab| {
                if active {
                    tab.bg(active_bg).text_color(fg)
                } else {
                    tab.text_color(fg.opacity(0.55))
                        .hover(|tab| tab.bg(hover_bg).text_color(fg.opacity(0.8)))
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    // 标签铺满了标题栏，双击标签和双击空白标题栏一样缩放窗口。
                    if event.click_count >= 2 {
                        window.titlebar_double_click();
                    } else {
                        this.activate(ix, window, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_tab_by_id(id, window, cx);
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            // 落点提示画在目标标签靠近原位置的另一侧：往右拖插到它右边，往左拖插到它左边。
            .drag_over::<DraggedTab>(move |style, dragged, _, _| {
                let marker = fg.opacity(0.6);
                if dragged.ix < ix {
                    style.border_r_2().border_color(marker)
                } else if dragged.ix > ix {
                    style.border_l_2().border_color(marker)
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                this.move_tab(dragged.id, ix, window, cx);
            }))
            .child(
                close_button(("tab-close", ix), fg)
                    .flex_none()
                    // 紧凑时平时宽度为零，悬停时才撑开，标题让出位置，两者不重叠。
                    // 不能用 display 切换：悬停状态在 prepaint 和 paint 之间可能变化，
                    // prepaint 时隐藏、paint 时显示会让 gpui 去画没 prepaint 过的子元素而 panic。
                    .map(|close| {
                        if compact {
                            close
                                .w_0()
                                .overflow_hidden()
                                .group_hover(group, |close| close.w(px(TAB_CLOSE_SIZE)))
                        } else {
                            close.invisible().group_hover(group, |close| close.visible())
                        }
                    })
                    .tooltip(close_tooltip)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_tab_by_id(id, window, cx);
                        }),
                    ),
            )
            .child(titled(title, mark, ("tab-agent", ix), fg).flex_1())
            .children(side_slot)
    }

    pub(super) fn render_new_tab_button(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let hover_bg = hsla(bg.mix(fg, 0.04));
        let tooltip = tooltip(rust_i18n::t!("menu.new_tab"), Some(&NewTab), fg, bg);
        let fg = hsla(fg);
        div()
            .id("new-tab")
            .flex_none()
            .w(px(NEW_TAB_BUTTON_WIDTH))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .border_l_1()
            .border_color(divider_color(fg))
            .text_size(px(16.))
            .text_color(fg.opacity(0.55))
            .hover(|button| button.bg(hover_bg).text_color(fg))
            .child("+")
            .tooltip(tooltip)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.new_tab(&NewTab, window, cx);
                }),
            )
    }
}
