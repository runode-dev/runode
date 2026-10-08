//! 标签里的终端区：分屏树、分隔线、拖动分隔线时的遮罩，别的终端里的程序（经命令行）正在
//! 操作某个分屏时右上角的驱动标记，以及 agent 报了用量时分屏底下的一行。

use std::{
    borrow::Cow,
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui::{
    Action, AnyElement, App, Context, CursorStyle, Div, EntityId, ExternalPaths, Focusable, FontWeight, MouseButton,
    MouseDownEvent, MouseMoveEvent, SharedString, Stateful, StyleRefinement, Window, canvas, div, prelude::*, px,
    relative, svg,
};
use runode_protocol::SessionId;
use runode_shared_types::{
    agent::AgentUsage,
    color::Rgb,
    pane::{Axis, Node},
    session::{DriveAction, Driver},
};

use super::{
    AGENT_MARK_WIDTH, CARD_GAP, ClosePane, DIVIDER_GRAB_WIDTH, Divider, NewSplitDown, NewSplitRight, NewTab,
    PANE_HEADER_HEIGHT, TogglePaneZoom, WindowView,
    agents::logo::agent_logo,
    card, cards,
    files::DraggedFile,
    model::Tab,
    status_bar,
    titlebar::{icon_toggle, pane_label, styled_agent_mark},
};
use crate::{
    assets::{CLOSE_ICON, MAXIMIZE_ICON, MINIMIZE_ICON, SPLIT_DOWN_ICON, SPLIT_RIGHT_ICON, TERMINAL_ICON},
    ui::{
        hsla,
        tooltip::{shortcut_text, tooltip},
    },
};

/// 没有焦点的分屏蒙上一层背景色，这是蒙层的不透明度。
const UNFOCUSED_DIM: f32 = 0.3;
/// 卡片样式下分屏比这窄时标题条上不放按钮，标题留着位置；分屏、放大和关闭照样能用快捷键和菜单。
const PANE_BUTTONS_MIN_WIDTH: f32 = 240.;
/// 驱动标记从最近一次操作起显示这么久，之后自己消失。
const DRIVER_SHOWN_MS: u64 = 10_000;

/// 驱动标记还要显示多久；过了 `DRIVER_SHOWN_MS`、不该再显示时为 `None`。宿主的钟比这边快一点
/// 时算刚刚操作。
fn driver_shown_for(at_ms: u64, now_ms: u64) -> Option<Duration> {
    let ago = now_ms.saturating_sub(at_ms);
    (ago < DRIVER_SHOWN_MS).then(|| Duration::from_millis(DRIVER_SHOWN_MS - ago))
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

/// 驱动方怎么称呼：`name_of` 给出它所在会话的称呼（见 `WindowView::session_label`），找不到那个
/// 终端时是会话标识的前 8 位；不在 runode 的终端里跑的程序为 `None`。
fn driver_name(by: Option<&str>, name_of: impl Fn(SessionId) -> Option<String>) -> Option<String> {
    let by = by?;
    let title = by.parse::<SessionId>().ok().and_then(name_of);
    Some(title.unwrap_or_else(|| by.chars().take(8).collect()))
}

/// 驱动标记上的文字：「由 <驱动方> 操作 · <动作>」，按 `locale` 的语言。不写过了几秒：那样标记
/// 显示着的十秒里要每秒重画一次，窗口就一直在画帧；标记还在就说明是十秒内的操作。
fn driver_text(name: Option<&str>, action: DriveAction, locale: &str) -> String {
    let who = match name {
        Some(name) => name.to_owned(),
        None => rust_i18n::t!("driver.outside", locale = locale).into_owned(),
    };
    let action = match action {
        DriveAction::Input => rust_i18n::t!("driver.action.input", locale = locale),
        DriveAction::Keys => rust_i18n::t!("driver.action.keys", locale = locale),
        DriveAction::Paste => rust_i18n::t!("driver.action.paste", locale = locale),
        DriveAction::ClearScreen => rust_i18n::t!("driver.action.clear_screen", locale = locale),
        DriveAction::Kill => rust_i18n::t!("driver.action.kill", locale = locale),
        DriveAction::Unknown => rust_i18n::t!("driver.action.unknown", locale = locale),
    };
    rust_i18n::t!("driver.badge", locale = locale, who = who, action = action).into_owned()
}

impl Tab {
    /// 这个标签里有分屏正被别的终端里的程序操作着（驱动标记还没消失）。
    pub(super) fn driven(&self, now_ms: u64, cx: &App) -> bool {
        self.panes.values().any(|(view, _)| {
            view.read(cx).driver().is_some_and(|driver| driver_shown_for(driver.at_ms, now_ms).is_some())
        })
    }
}

impl WindowView {
    /// 当前标签的终端区：分屏树，或者放大着的那一个终端；workspace 里没有标签时是空的标签区。
    pub(super) fn render_panes(&mut self, fg: Rgb, bg: Rgb, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let now = now_ms();
        let badges = self.driver_badges(now, window, cx);
        self.schedule_driver_redraw(now, cx);
        let Some(tab) = self.tab() else {
            return self.render_empty_tab(fg, bg, cx);
        };
        if tab.zoomed || tab.root.is_leaf() {
            return self.render_leaf(tab, tab.focused, &badges, fg, bg, cx);
        }
        self.render_node(tab, &tab.root, &badges, fg, bg, cx)
    }

    /// 没有标签的 workspace 的终端区：一句说明和新建标签的按钮。接着窗口自己的焦点，快捷键照常派发。
    fn render_empty_tab(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> AnyElement {
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let fg = hsla(fg);
        let shortcut = shortcut_text(&NewTab, cx);
        let button = div()
            .id("empty-new-tab")
            .px(px(12.))
            .py(px(6.))
            .rounded(px(6.))
            .border_1()
            .border_color(fg.opacity(0.15))
            .flex()
            .gap(px(8.))
            .text_color(fg.opacity(0.8))
            .hover(|button| button.bg(hover_bg).text_color(fg))
            .child(rust_i18n::t!("menu.new_tab").into_owned())
            .children(shortcut.map(|shortcut| div().text_color(fg.opacity(0.4)).child(shortcut)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.new_tab(&NewTab, window, cx);
                }),
            );
        let empty = div()
            .id("empty-tab")
            .track_focus(&self.empty_focus)
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .text_size(px(13.))
            .text_color(fg.opacity(0.45))
            .child(rust_i18n::t!("workspace.empty").into_owned())
            .child(button);
        if cards(cx) {
            card(fg, hsla(bg)).size_full().child(empty).into_any_element()
        } else {
            empty.into_any_element()
        }
    }

    /// 当前标签里各个正被操作的分屏上要显示的驱动标记。
    fn driver_badges(&self, now: u64, window: &Window, cx: &App) -> HashMap<EntityId, SharedString> {
        let locale = rust_i18n::locale();
        let mut badges = HashMap::new();
        for (pane, (view, _)) in self.tab().into_iter().flat_map(|tab| &tab.panes) {
            let Some(driver) = view.read(cx).driver() else {
                continue;
            };
            if driver_shown_for(driver.at_ms, now).is_none() {
                continue;
            }
            let name = driver_name(driver.by.as_deref(), |id| self.session_label(id, window, cx));
            badges.insert(*pane, driver_text(name.as_deref(), driver.action, &locale).into());
        }
        badges
    }

    /// 会话 `id` 怎么称呼：前台是 agent 时用 agent 的名字，前台是别的程序时用程序名，前台是
    /// shell 时用终端标题（shell 的标题多半只是目录名，看不出是谁，所以排在最后）。在所有窗口里
    /// 找；这个窗口正在更新、从窗口表里读不到，直接用 `self`。
    fn session_label(&self, id: SessionId, window: &Window, cx: &App) -> Option<String> {
        let title_in = |view: &WindowView| {
            view.workspaces
                .iter()
                .flat_map(|workspace| &workspace.tabs)
                .flat_map(|tab| tab.panes.values())
                .map(|(terminal, _)| terminal.read(cx))
                .find(|terminal| terminal.session_id() == Some(id))
                .map(|terminal| {
                    if let Some(agent) = terminal.agent().filter(|agent| agent.kind.is_known()) {
                        return agent.kind.display_name().to_owned();
                    }
                    let meta = terminal.meta();
                    match &meta.foreground {
                        Some(program) if !meta.foreground_is_shell => program.clone(),
                        _ => terminal.title().to_owned(),
                    }
                })
        };
        if let Some(title) = title_in(self) {
            return Some(title);
        }
        let own = window.window_handle();
        cx.windows()
            .into_iter()
            .filter(|handle| *handle != own)
            .filter_map(|handle| handle.downcast::<WindowView>())
            .find_map(|handle| title_in(handle.read(cx).ok()?))
    }

    /// 当前 workspace 里还有驱动标记（分屏上的文字或者标签上的图标）显示着时，到最早的那个该消失
    /// 的时候重画一次，它就自己消失了；标记显示着的时候不重画。已经约好的重画不晚于这个时候就
    /// 留着，晚了（比如切到了另一个 workspace）就换成早的。
    fn schedule_driver_redraw(&mut self, now: u64, cx: &mut Context<Self>) {
        let next = self
            .workspace()
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.values())
            .filter_map(|(view, _)| view.read(cx).driver().map(|driver: &Driver| driver.at_ms))
            .filter_map(|at| driver_shown_for(at, now))
            .min();
        let Some(next) = next else {
            return;
        };
        let due = now + next.as_millis() as u64;
        if self.driver_redraw.as_ref().is_some_and(|(at, _)| *at <= due) {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(next).await;
            this.update(cx, |this, cx| {
                this.driver_redraw = None;
                cx.notify();
            })
            .ok();
        });
        self.driver_redraw = Some((due, task));
    }

    fn render_leaf(
        &self,
        tab: &Tab,
        id: EntityId,
        badges: &HashMap<EntityId, SharedString>,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = tab.panes[&id].0.clone();
        let dimmed = !tab.zoomed && !tab.root.is_leaf() && id != tab.focused;
        let layout = self.layout.clone();
        let badge = badges.get(&id).map(|text| driver_badge(text.clone(), fg, bg));
        let cards = cards(cx);
        let terminal = div()
            .relative()
            .when(!cards, |terminal| terminal.size_full())
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
            .when(dimmed, |leaf| leaf.child(div().absolute().size_full().bg(hsla(bg).opacity(UNFOCUSED_DIM))))
            .children(badge)
            // 从文件树或访达拖来的文件放到这个终端上：切到它，把路径打进去。
            .on_drop(cx.listener(move |this, dragged: &DraggedFile, window, cx| {
                this.drop_paths_on_pane(id, std::slice::from_ref(&dragged.path), window, cx);
            }))
            .on_drop(cx.listener(move |this, dropped: &ExternalPaths, window, cx| {
                this.drop_paths_on_pane(id, dropped.paths(), window, cx);
            }));
        let usage = {
            let terminal = tab.panes[&id].0.read(cx);
            let locale = rust_i18n::locale();
            terminal.agent().and(terminal.meta().agent_usage.as_ref()).map(|usage| usage_text(usage, &locale))
        };
        let usage = usage.filter(|text| !text.is_empty()).map(|text| usage_line(text, fg));
        if !cards {
            let Some(usage) = usage else {
                return terminal.into_any_element();
            };
            // 终端用四边定位撑满用量那一行上面的部分，道理同下面卡片里的终端。
            return div()
                .size_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(terminal.absolute().top_0().left_0().right_0().bottom_0()),
                )
                .child(usage.pt(px(4.)))
                .into_any_element();
        }
        // 卡片：上面是标题条，下面是终端。终端四周留一点，它的方角落在卡片的圆角里面。终端用四边
        // 定位撑满标题条下面的部分：百分比的高度在这里会按整张卡片算，比剩下的高出一个标题条。
        let group = SharedString::from(format!("pane-{}", id.as_u64()));
        let inset = px(4.);
        card(hsla(fg), hsla(bg))
            .group(group.clone())
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_pane_header(tab, id, group, fg, bg, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(terminal.absolute().top_0().left(inset).right(inset).bottom(inset)),
            )
            .children(usage)
            .into_any_element()
    }

    /// 卡片样式下分屏顶上的标题条：agent 的状态标记（前台不是 agent 时是终端图标）和终端标题，
    /// 右边是向右、向下分屏，放大或还原，关闭分屏的按钮，当前分屏一直显示，别的分屏悬停时显示；
    /// 分屏窄于 `PANE_BUTTONS_MIN_WIDTH` 时不放。
    /// 点标题条切到这个分屏，双击放大或还原。
    fn render_pane_header(
        &self,
        tab: &Tab,
        id: EntityId,
        group: SharedString,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let split = !tab.root.is_leaf();
        let current = id == tab.focused;
        // 宽度取上一帧画出来的；还没画过时当它够宽。
        let narrow =
            self.layout.borrow().panes.get(&id).is_some_and(|bounds| bounds.size.width < px(PANE_BUTTONS_MIN_WIDTH));
        let (name, dir) = pane_label(tab.panes[&id].0.read(cx));
        let fg_hsla = hsla(fg);
        // 有 logo 的 agent：前面放 logo，状态标记跟在标题后面；没有 logo 的 agent 状态标记放前面。
        let mark = tab.pane_mark(id, cx);
        let logo = mark.and_then(|mark| agent_logo(mark.kind, px(14.), fg_hsla.opacity(0.85)));
        let trailing_mark = logo.is_some().then_some(mark).flatten();
        let icon = match (logo, mark) {
            (Some(logo), _) => logo,
            (None, Some(mark)) => styled_agent_mark(mark, ("pane-agent", id), fg_hsla, true),
            (None, None) => div()
                .flex_none()
                .w(px(AGENT_MARK_WIDTH + 2.))
                .flex()
                .justify_center()
                .child(svg().path(TERMINAL_ICON).size(px(14.)).text_color(fg_hsla.opacity(0.6)))
                .into_any_element(),
        };
        type Handler = fn(&mut WindowView, EntityId, &mut Window, &mut Context<WindowView>);
        let button =
            |key: &'static str, icon: &'static str, text: Cow<'static, str>, action: &dyn Action, handler: Handler| {
                icon_toggle(key, icon, 13., false, fg, bg)
                    .flex_none()
                    .size(px(22.))
                    .tooltip(tooltip(text, Some(action), fg, bg))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            handler(this, id, window, cx);
                        }),
                    )
            };
        let (zoom_icon, zoom_text) = if tab.zoomed {
            (MINIMIZE_ICON, rust_i18n::t!("tooltip.restore_split"))
        } else {
            (MAXIMIZE_ICON, rust_i18n::t!("menu.zoom_split"))
        };
        let buttons = div()
            .flex_none()
            .flex()
            .gap(px(2.))
            .child(button(
                "pane-split-right",
                SPLIT_RIGHT_ICON,
                rust_i18n::t!("menu.split_right"),
                &NewSplitRight,
                |this, id, window, cx| {
                    this.focus_pane_from_header(id, window, cx);
                    this.new_split_right(&NewSplitRight, window, cx);
                },
            ))
            .child(button(
                "pane-split-down",
                SPLIT_DOWN_ICON,
                rust_i18n::t!("menu.split_down"),
                &NewSplitDown,
                |this, id, window, cx| {
                    this.focus_pane_from_header(id, window, cx);
                    this.new_split_down(&NewSplitDown, window, cx);
                },
            ))
            .when(split, |buttons| {
                buttons.child(button("pane-zoom", zoom_icon, zoom_text, &TogglePaneZoom, |this, id, window, cx| {
                    this.focus_pane_from_header(id, window, cx);
                    this.toggle_pane_zoom(&TogglePaneZoom, window, cx);
                }))
            })
            .child(button(
                "pane-close",
                CLOSE_ICON,
                rust_i18n::t!("tooltip.close_split"),
                &ClosePane,
                |this, id, window, cx| {
                    this.confirm_close_pane(id, window, cx);
                },
            ))
            // 不能用 display 切换，见 `render_tab` 里关闭按钮的说明。
            .when(!(current && split), |buttons| buttons.invisible().group_hover(group, |buttons| buttons.visible()));
        div()
            .id(("pane-header", id))
            .flex_none()
            .h(px(PANE_HEADER_HEIGHT))
            .pl(px(10.))
            .pr(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(fg_hsla.opacity(if current || !split { 0.9 } else { 0.55 }))
            .child(icon)
            .child(div().flex_shrink_0().max_w(relative(0.6)).truncate().child(name))
            .children(dir.map(|dir| div().min_w_0().truncate().text_color(fg_hsla.opacity(0.45)).child(dir)))
            .children(trailing_mark.map(|mark| styled_agent_mark(mark, ("pane-agent", id), fg_hsla, true)))
            .child(div().flex_1())
            .children((!narrow).then_some(buttons))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.focus_pane_from_header(id, window, cx);
                    if event.click_count >= 2 {
                        this.toggle_pane_zoom(&TogglePaneZoom, window, cx);
                    }
                }),
            )
    }

    /// 在分屏的标题条上点了一下：切到这个分屏。已经是当前分屏时不动，放大着也不还原。
    fn focus_pane_from_header(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab().is_some_and(|tab| tab.focused == id) {
            window.focus(&self.focus_handle(cx), cx);
        } else {
            self.focus_pane_in_active_tab(id, window, cx);
        }
    }

    fn render_node(
        &self,
        tab: &Tab,
        node: &Node<EntityId>,
        badges: &HashMap<EntityId, SharedString>,
        fg: Rgb,
        bg: Rgb,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let split = match node {
            Node::Leaf(id) => return self.render_leaf(tab, *id, badges, fg, bg, cx),
            Node::Split(split) => split,
        };
        let (id, axis, ratio) = (split.id, split.axis, split.ratio);
        let horizontal = axis == Axis::Horizontal;
        let first = self.render_node(tab, &split.first, badges, fg, bg, cx);
        let second = self.render_node(tab, &split.second, badges, fg, bg, cx);
        // 卡片样式下两张卡片之间空出 `CARD_GAP`，空隙本身就是分隔线；经典样式下是一像素的线。
        let cards = cards(cx);
        let line = if cards { hsla(fg).opacity(0.) } else { hsla(fg).opacity(0.15) };
        let gap = if cards { CARD_GAP } else { 1. };
        let grab = if cards { CARD_GAP } else { DIVIDER_GRAB_WIDTH };
        let layout = self.layout.clone();
        div()
            .relative()
            .size_full()
            .flex()
            .when(!horizontal, |split| split.flex_col())
            .child(
                div()
                    .flex_none()
                    .map(
                        |pane| {
                            if horizontal { pane.w(relative(ratio)).h_full() } else { pane.h(relative(ratio)).w_full() }
                        },
                    )
                    .min_w_0()
                    .min_h_0()
                    .child(first),
            )
            .child(
                div()
                    .flex_none()
                    .bg(line)
                    .map(|divider| if horizontal { divider.w(px(gap)).h_full() } else { divider.h(px(gap)).w_full() }),
            )
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
                                .w(px(grab))
                                .left(relative(ratio))
                                .ml(px((gap - grab) / 2.))
                                .cursor(CursorStyle::ResizeLeftRight)
                        } else {
                            handle
                                .left_0()
                                .w_full()
                                .h(px(grab))
                                .top(relative(ratio))
                                .mt(px((gap - grab) / 2.))
                                .cursor(CursorStyle::ResizeUpDown)
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            if event.click_count >= 2 {
                                // 双击分隔线让两边一样大。
                                if let Some(tab) = this.tab_mut() {
                                    tab.root.set_ratio(id, 0.5);
                                }
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
        let vertical = matches!(divider, Divider::Split(_, Axis::Vertical) | Divider::GitGraph);
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
                        // 卡片样式下分隔线落在侧栏和卡片之间的空隙中间。
                        let spacing = if cards(cx) { CARD_GAP / 2. } else { 0. };
                        this.resize_sidebar(f32::from(event.position.x) - spacing);
                        cx.notify();
                        return;
                    }
                    Divider::Preview | Divider::Panel => {
                        let viewport = f32::from(window.viewport_size().width);
                        // 卡片样式下分隔线和窗口右边之间还有卡片的间距，按经典样式算宽度前先扣掉。
                        let widths = this.right_panel_widths(viewport);
                        let spacing = if cards(cx) {
                            this.right_divider_offset(divider, widths, true)
                                - this.right_divider_offset(divider, widths, false)
                        } else {
                            0.
                        };
                        this.resize_right_panel(divider, f32::from(event.position.x), viewport - spacing);
                        cx.notify();
                        return;
                    }
                    Divider::GitGraph => {
                        // 卡片样式下面板的卡片底下离窗口底边还有一段间距。
                        let spacing = if cards(cx) { CARD_GAP } else { 0. };
                        let viewport = f32::from(window.viewport_size().height) - spacing - status_bar::height(cx);
                        this.resize_git_graph(f32::from(event.position.y), viewport);
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
                if let Some(tab) = this.tab_mut() {
                    tab.root.set_ratio(id, ratio);
                }
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

/// 分屏底下显示用量的那一行。
fn usage_line(text: String, fg: Rgb) -> Div {
    div()
        .flex_none()
        .px(px(10.))
        .pb(px(4.))
        .text_size(px(11.))
        .text_color(hsla(fg).opacity(0.55))
        .truncate()
        .child(text)
}

/// 分屏底下那一行：模型、上下文占用、缓存读写、花费和五小时限额，缺的项不写。
fn usage_text(usage: &AgentUsage, locale: &str) -> String {
    let mut parts = Vec::new();
    parts.extend(usage.model.clone());
    match (usage.context_tokens, usage.context_window) {
        (Some(used), Some(window)) if window > 0 => {
            parts.push(format!("{} / {} ({}%)", tokens(used), tokens(window), used * 100 / window));
        }
        (Some(used), _) => parts.push(tokens(used)),
        _ => {}
    }
    if let (Some(read), Some(write)) = (usage.cache_read_tokens, usage.cache_write_tokens) {
        let (read, write) = (tokens(read), tokens(write));
        parts.push(rust_i18n::t!("usage.cache", locale = locale, read = read, write = write).into_owned());
    }
    parts.extend(usage.cost_micro_usd.map(|micro| format!("${:.2}", micro as f64 / 1e6)));
    parts.extend(usage.five_hour_percent.map(|percent| format!("5h {percent}%")));
    parts.join("  ·  ")
}

/// token 数的简写：`850`、`48.6K`、`156K`、`1M`。
fn tokens(n: u64) -> String {
    let short = |value: f64, unit: &str| {
        let text = if value < 100. { format!("{value:.1}") } else { format!("{value:.0}") };
        format!("{}{unit}", text.trim_end_matches(".0"))
    };
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => short(n as f64 / 1e3, "K"),
        _ => short(n as f64 / 1e6, "M"),
    }
}

/// 分屏右上角的驱动标记。没有鼠标处理，点击照样落到下面的终端上。
fn driver_badge(text: SharedString, fg: Rgb, bg: Rgb) -> Div {
    let panel = hsla(bg.mix(fg, 0.12));
    let fg = hsla(fg);
    div()
        .absolute()
        .top(px(6.))
        .right(px(10.))
        .max_w(relative(0.8))
        .px(px(8.))
        .py(px(3.))
        .rounded(px(6.))
        .bg(panel.opacity(0.92))
        .border_1()
        .border_color(fg.opacity(0.15))
        .text_size(px(11.))
        .text_color(fg.opacity(0.8))
        .child(div().min_w_0().truncate().child(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_reads_short() {
        assert_eq!(tokens(850), "850");
        assert_eq!(tokens(48_600), "48.6K");
        assert_eq!(tokens(200_000), "200K");
        assert_eq!(tokens(1_000_000), "1M");
        let usage = AgentUsage {
            model: Some("Opus".into()),
            context_tokens: Some(48_600),
            cache_read_tokens: Some(27_200),
            cache_write_tokens: Some(21_400),
            context_window: Some(1_000_000),
            cost_micro_usd: Some(1_234_000),
            five_hour_percent: Some(23),
        };
        assert_eq!(
            usage_text(&usage, "en"),
            "Opus  ·  48.6K / 1M (4%)  ·  cache read 27.2K, write 21.4K  ·  $1.23  ·  5h 23%"
        );
        assert_eq!(usage_text(&AgentUsage::default(), "en"), "");
    }

    #[test]
    fn the_badge_goes_away_ten_seconds_after_the_drive() {
        assert_eq!(driver_shown_for(1_000, 1_000), Some(Duration::from_secs(10)));
        assert_eq!(driver_shown_for(1_000, 4_250), Some(Duration::from_millis(6_750)));
        assert_eq!(driver_shown_for(1_000, 10_999), Some(Duration::from_millis(1)));
        assert_eq!(driver_shown_for(1_000, 11_000), None);
        // 宿主的钟快一点时算刚刚。
        assert_eq!(driver_shown_for(5_000, 4_000), Some(Duration::from_secs(10)));
    }

    #[test]
    fn the_driver_is_named_by_its_session_or_its_id() {
        let by = "0123456789abcdef0123456789abcdef";
        let known: SessionId = by.parse().unwrap();
        let name_of = |id: SessionId| (id == known).then(|| "Claude Code".to_owned());
        assert_eq!(driver_name(Some(by), name_of).as_deref(), Some("Claude Code"));
        assert_eq!(driver_name(Some("fedcba9876543210fedcba9876543210"), name_of).as_deref(), Some("fedcba98"));
        assert_eq!(driver_name(Some("abc"), name_of).as_deref(), Some("abc"));
        assert_eq!(driver_name(None, name_of), None);
    }

    #[test]
    fn badge_text_names_the_driver_and_the_action() {
        assert_eq!(driver_text(Some("claude"), DriveAction::Input, "zh-Hans"), "由 claude 操作 · 打字");
        assert_eq!(driver_text(Some("claude"), DriveAction::Keys, "en"), "Driven by claude · keys");
        assert_eq!(driver_text(None, DriveAction::Paste, "en"), "Driven by a program outside Runode · paste");
        for action in [DriveAction::ClearScreen, DriveAction::Kill, DriveAction::Unknown] {
            for locale in ["en", "zh-Hans", "zh-Hant"] {
                let text = driver_text(Some("x"), action, locale);
                assert!(!text.contains("driver."), "{locale}: {text}");
            }
        }
    }
}
