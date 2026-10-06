//! 标签里的终端区：分屏树、分隔线、拖动分隔线时的遮罩，以及别的终端里的程序（经命令行）正在
//! 操作某个分屏时右上角的驱动标记。

use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui::{
    AnyElement, App, Context, CursorStyle, Div, EntityId, ExternalPaths, MouseButton, MouseDownEvent, MouseMoveEvent,
    SharedString, StyleRefinement, Window, canvas, div, prelude::*, px, relative,
};
use runode_protocol::SessionId;
use runode_shared_types::{
    agent::AgentKind,
    color::Rgb,
    pane::{Axis, Node},
    session::{DriveAction, Driver},
};

use super::{DIVIDER_GRAB_WIDTH, Divider, WindowView, files::DraggedFile, model::Tab};
use crate::ui::hsla;

/// 没有焦点的分屏蒙上一层背景色，这是蒙层的不透明度。
const UNFOCUSED_DIM: f32 = 0.3;
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
    /// 当前标签的终端区：分屏树，或者放大着的那一个终端。
    pub(super) fn render_panes(&mut self, fg: Rgb, bg: Rgb, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let now = now_ms();
        let badges = self.driver_badges(now, window, cx);
        self.schedule_driver_redraw(now, cx);
        let tab = self.tab();
        if tab.zoomed || tab.root.is_leaf() {
            return self.render_leaf(tab, tab.focused, &badges, fg, bg, cx);
        }
        self.render_node(tab, &tab.root, &badges, fg, bg, cx)
    }

    /// 当前标签里各个正被操作的分屏上要显示的驱动标记。
    fn driver_badges(&self, now: u64, window: &Window, cx: &App) -> HashMap<EntityId, SharedString> {
        let locale = rust_i18n::locale();
        let mut badges = HashMap::new();
        for (pane, (view, _)) in &self.tab().panes {
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
                    if let Some(agent) = terminal.agent().filter(|agent| agent.kind != AgentKind::Other) {
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
            .when(dimmed, |leaf| leaf.child(div().absolute().size_full().bg(hsla(bg).opacity(UNFOCUSED_DIM))))
            .children(badge)
            // 从文件树或访达拖来的文件放到这个终端上：切到它，把路径打进去。
            .on_drop(cx.listener(move |this, dragged: &DraggedFile, window, cx| {
                this.drop_paths_on_pane(id, std::slice::from_ref(&dragged.path), window, cx);
            }))
            .on_drop(cx.listener(move |this, dropped: &ExternalPaths, window, cx| {
                this.drop_paths_on_pane(id, dropped.paths(), window, cx);
            }))
            .into_any_element()
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
                    .map(|divider| if horizontal { divider.w(px(1.)).h_full() } else { divider.h(px(1.)).w_full() }),
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
                    Divider::Preview | Divider::Git | Divider::Files => {
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
