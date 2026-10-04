//! 一个窗口里的一组终端标签：顶部标签栏，以及新建、切换和关闭标签。

use gpui::{
    Action, App, Context, Div, Entity, EntityId, FocusHandle, Focusable, MouseButton, Render,
    SharedString, Stateful, Subscription, TitlebarOptions, Window, actions, div, point,
    prelude::*, px,
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
    _events: Subscription,
}

pub struct Workspace {
    /// 至少有一个；最后一个关掉时窗口跟着关。
    tabs: Vec<Tab>,
    active: usize,
}

impl Workspace {
    pub fn new(first: Entity<TerminalView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
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
            TerminalEvent::Bell => window.play_system_bell(),
            TerminalEvent::Exited => self.close(view.entity_id(), window, cx),
        }
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix;
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

    fn render_tab(&self, ix: usize, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let view = &self.tabs[ix].view;
        let id = view.entity_id();
        let title = SharedString::from(view.read(cx).title().to_owned());
        let active = ix == self.active;
        let active_bg = hsla(bg.mix(fg, 0.08));
        let fg = hsla(fg);
        let group = SharedString::from(format!("tab-{ix}"));
        div()
            .id(("tab", ix))
            .group(group.clone())
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .px(px(8.))
            .gap(px(4.))
            .when(ix > 0, |tab| {
                tab.border_l_1().border_color(fg.opacity(0.12))
            })
            .when(active, |tab| tab.bg(active_bg))
            .text_color(if active { fg } else { fg.opacity(0.55) })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.activate(ix, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close(id, window, cx);
                }),
            )
            .child(
                div()
                    .id(("tab-close", ix))
                    .flex_none()
                    .size(px(TAB_CLOSE_SIZE))
                    .rounded(px(3.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .invisible()
                    .group_hover(group, |close| close.visible())
                    .hover(|close| close.bg(fg.opacity(0.15)))
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
            // 和左边的关闭按钮等宽，标题才在标签里居中。
            .child(div().flex_none().w(px(TAB_CLOSE_SIZE)))
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
        let tabs: Vec<_> = if show_tabs {
            (0..self.tabs.len())
                .map(|ix| self.render_tab(ix, fg, bg, cx))
                .collect()
        } else {
            Vec::new()
        };
        let titlebar = (!fullscreen || show_tabs).then(|| {
            div()
                .id("titlebar")
                .h(px(TITLEBAR_HEIGHT))
                .flex_none()
                .flex()
                .when(!fullscreen, |bar| bar.pl(px(TRAFFIC_LIGHTS_WIDTH)))
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
