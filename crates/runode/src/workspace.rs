//! 一个窗口里的一组终端标签：顶部标签栏，以及新建、切换和关闭标签。

use gpui::{
    Action, App, BoxShadow, Context, Div, Entity, EntityId, FocusHandle, Focusable, Hsla,
    MouseButton, MouseDownEvent, Pixels, Render, ScrollHandle, SharedString, Stateful, Subscription, TitlebarOptions, Window,
    actions, div, point, prelude::*, px,
};

use crate::{
    session::Rgb,
    terminal_view::{DEFAULT_TITLE, TerminalEvent, TerminalView, hsla},
};

actions!(
    runode,
    [NewTab, CloseTab, NextTab, PreviousTab, SelectLastTab]
);

/// 切换到第几个标签（从 0 数）。
#[derive(Clone, PartialEq, Action)]
#[action(namespace = runode, no_json)]
pub struct SelectTab(pub usize);

/// 透明标题栏的高度：终端内容从它下面开始，这一条用来拖动窗口，多个标签时也画在这里。
const TITLEBAR_HEIGHT: f32 = 28.;
/// 红绿灯按钮的位置，以及标题栏左侧给它们留出的宽度。
const TRAFFIC_LIGHTS_ORIGIN: (f32, f32) = (12., 10.);
const TRAFFIC_LIGHTS_WIDTH: f32 = 78.;
/// 标签上关闭按钮的边长。
const TAB_CLOSE_SIZE: f32 = 16.;
/// 标签右侧槽位的宽度，放快捷键提示或响铃标记；和关闭按钮一边一个，标题才在标签里居中。
const TAB_SIDE_SLOT: f32 = 20.;
/// 标签栏最右边新建标签按钮的宽度。
const NEW_TAB_BUTTON_WIDTH: f32 = 28.;
/// 标签最窄的宽度；标签多到挤不下时标签条改为横向滚动。
const TAB_MIN_WIDTH: f32 = 64.;
/// 比这窄的标签只留标题：快捷键提示收起，关闭按钮悬停时浮在标题左边。
const TAB_COMPACT_WIDTH: f32 = 120.;

/// 窗口的标题栏设置：透明，终端背景一直铺到窗口顶部，标签栏画在红绿灯右边。
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

struct Tab {
    view: Entity<TerminalView>,
    /// 不在前台时响过铃，切过去后清掉。
    bell: bool,
    _events: Subscription,
}

/// 拖动中的标签：拖动时跟着鼠标画出来，放到另一个标签上时按 `id` 挪位置。
#[derive(Clone)]
struct DraggedTab {
    id: EntityId,
    /// 开始拖动时所在的位置，用来决定落点提示画在目标标签的哪一边。
    ix: usize,
    title: SharedString,
    width: Pixels,
    fg: Hsla,
    bg: Hsla,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(self.width)
            .h(px(TITLEBAR_HEIGHT))
            .px(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .bg(self.bg)
            .border_1()
            .border_color(self.fg.opacity(0.15))
            .shadow(vec![BoxShadow {
                color: Hsla::black().opacity(0.3),
                offset: point(px(0.), px(2.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
                inset: false,
            }])
            .text_size(px(12.))
            .text_color(self.fg)
            .child(div().min_w_0().truncate().child(self.title.clone()))
    }
}

/// 第 `ix` 个标签的快捷键提示：⌘1 到 ⌘8 对应前八个，⌘9 对应最后一个。
fn tab_shortcut(ix: usize, len: usize) -> Option<SharedString> {
    if ix < 8 {
        Some(format!("⌘{}", ix + 1).into())
    } else if ix == len - 1 {
        Some("⌘9".into())
    } else {
        None
    }
}

pub struct Workspace {
    /// 至少有一个；最后一个关掉时窗口跟着关。
    tabs: Vec<Tab>,
    active: usize,
    /// 标签条的滚动位置，切换标签时把当前标签滚进视野。
    tab_scroll: ScrollHandle,
}

impl Workspace {
    pub fn new(first: Entity<TerminalView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            tab_scroll: ScrollHandle::new(),
        };
        this.insert_tab(0, first, window, cx);
        this
    }

    fn insert_tab(
        &mut self,
        ix: usize,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let events = cx.subscribe_in(&view, window, Self::handle_terminal_event);
        self.tabs.insert(
            ix,
            Tab {
                view,
                bell: false,
                _events: events,
            },
        );
        self.activate(ix, window, cx);
    }

    fn handle_terminal_event(
        &mut self,
        view: &Entity<TerminalView>,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalEvent::TitleChanged => {
                if self.tabs[self.active].view == *view {
                    self.sync_window_title(window, cx);
                }
                cx.notify();
            }
            TerminalEvent::Bell => {
                window.play_system_bell();
                if let Some(ix) = self.tabs.iter().position(|tab| tab.view == *view)
                    && ix != self.active
                {
                    self.tabs[ix].bell = true;
                    cx.notify();
                }
            }
            TerminalEvent::Exited => self.close(view.entity_id(), window, cx),
        }
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix;
        self.tabs[ix].bell = false;
        self.tab_scroll.scroll_to_item(ix);
        window.focus(&self.tabs[ix].view.focus_handle(cx), cx);
        self.sync_window_title(window, cx);
        cx.notify();
    }

    fn sync_window_title(&self, window: &mut Window, cx: &App) {
        window.set_window_title(self.tabs[self.active].view.read(cx).title());
    }

    /// 关掉一个标签；关掉的是当前标签时切到它右边那个（没有就左边）。
    fn close(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|tab| tab.view.entity_id() == id) else {
            return;
        };
        if self.tabs.len() == 1 {
            window.remove_window();
            return;
        }
        self.tabs.remove(ix);
        let active = if ix < self.active || self.active == self.tabs.len() {
            self.active - 1
        } else {
            self.active
        };
        self.activate(active, window, cx);
    }

    /// 把拖动的标签挪到第 `to` 个位置，并切到它。
    fn move_tab(&mut self, id: EntityId, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|tab| tab.view.entity_id() == id) else {
            return;
        };
        let tab = self.tabs.remove(from);
        let to = to.min(self.tabs.len());
        self.tabs.insert(to, tab);
        self.activate(to, window, cx);
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        match TerminalView::spawn(window, cx) {
            Ok(view) => self.insert_tab(self.active + 1, view, window, cx),
            Err(err) => tracing::error!("failed to start terminal session: {err:#}"),
        }
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close(self.tabs[self.active].view.entity_id(), window, cx);
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate((self.active + 1) % self.tabs.len(), window, cx);
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.tabs.len();
        self.activate((self.active + len - 1) % len, window, cx);
    }

    fn select_tab(&mut self, action: &SelectTab, window: &mut Window, cx: &mut Context<Self>) {
        if action.0 < self.tabs.len() {
            self.activate(action.0, window, cx);
        }
    }

    fn select_last_tab(&mut self, _: &SelectLastTab, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(self.tabs.len() - 1, window, cx);
    }

    fn render_tab(
        &self,
        ix: usize,
        width: Pixels,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let tab = &self.tabs[ix];
        let id = tab.view.entity_id();
        let title = SharedString::from(tab.view.read(cx).title().to_owned());
        let active = ix == self.active;
        let active_bg = hsla(bg.mix(fg, 0.08));
        let hover_bg = hsla(bg.mix(fg, 0.04));
        let fg = hsla(fg);
        let group = SharedString::from(format!("tab-{ix}"));
        // 当前标签自己就是一块亮色，和它相邻的分隔线去掉，只是不画颜色，免得宽度跳动。
        let divider = if active || ix == self.active + 1 {
            fg.opacity(0.)
        } else {
            fg.opacity(0.12)
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
                    .children(tab_shortcut(ix, self.tabs.len()))
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
                    this.close(id, window, cx);
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
                div()
                    .id(("tab-close", ix))
                    .flex_none()
                    .size(px(TAB_CLOSE_SIZE))
                    .rounded(px(3.))
                    .flex()
                    .items_center()
                    .justify_center()
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
                    .text_size(px(14.))
                    .text_color(fg.opacity(0.75))
                    .hover(|close| close.bg(fg.opacity(0.18)).text_color(fg))
                    .child("×")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close(id, window, cx);
                        }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_center()
                    .child(title),
            )
            .children(side_slot)
    }

    fn render_new_tab_button(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let hover_bg = hsla(bg.mix(fg, 0.04));
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
            .border_color(fg.opacity(0.12))
            .text_size(px(16.))
            .text_color(fg.opacity(0.55))
            .hover(|button| button.bg(hover_bg).text_color(fg))
            .child("+")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.new_tab(&NewTab, window, cx);
                }),
            )
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.tabs[self.active].view.focus_handle(cx)
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.tabs[self.active].view.clone();
        let (fg, bg) = view.update(cx, |view, _| view.colors());
        let fullscreen = window.is_fullscreen();
        let show_tabs = self.tabs.len() > 1;
        // 标题栏透明后内容铺到红绿灯下面，顶部这条要能拖动窗口、双击缩放。
        // 全屏时没有红绿灯，只有一个标签时不留这一条。
        let left_inset = if fullscreen { 0. } else { TRAFFIC_LIGHTS_WIDTH };
        let tabs: Vec<_> = if show_tabs {
            // 标签平分标题栏除去两头的宽度，但不窄于 `TAB_MIN_WIDTH`，挤不下就让标签条滚动；
            // 拖动时的预览也照这个宽度画。
            let tab_width = ((window.viewport_size().width
                - px(left_inset + NEW_TAB_BUTTON_WIDTH))
                / self.tabs.len() as f32)
                .max(px(TAB_MIN_WIDTH));
            let strip = div()
                .id("tabs")
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .overflow_x_scroll()
                .track_scroll(&self.tab_scroll)
                .children(
                    (0..self.tabs.len())
                        .map(|ix| self.render_tab(ix, tab_width, fg, bg, cx))
                        .collect::<Vec<_>>(),
                );
            vec![strip, self.render_new_tab_button(fg, bg, cx)]
        } else {
            Vec::new()
        };
        let titlebar = (!fullscreen || show_tabs).then(|| {
            div()
                .id("titlebar")
                .h(px(TITLEBAR_HEIGHT))
                .flex_none()
                .flex()
                .pl(px(left_inset))
                .text_size(px(12.))
                .on_mouse_down(MouseButton::Left, |event, window, _| {
                    if event.click_count >= 2 {
                        window.titlebar_double_click();
                    } else {
                        window.start_window_move();
                    }
                })
                .children(tabs)
        });
        div()
            .id("workspace")
            .key_context("Workspace")
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::select_tab))
            .on_action(cx.listener(Self::select_last_tab))
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(bg))
            .children(titlebar)
            .child(div().flex_1().min_h_0().child(view))
    }
}
