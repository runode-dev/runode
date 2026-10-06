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
    color::Rgb,
    pane::{Axis, Node},
    session::{DriveAction, Driver},
};

use super::{DIVIDER_GRAB_WIDTH, Divider, WindowView, files::DraggedFile, model::Tab};
use crate::terminal_view::hsla;

/// 没有焦点的分屏蒙上一层背景色，这是蒙层的不透明度。
const UNFOCUSED_DIM: f32 = 0.3;
/// 驱动标记从最近一次操作起显示这么久，之后自己消失。
const DRIVER_SHOWN_MS: u64 = 10_000;

/// 操作过去了几秒；过了 `DRIVER_SHOWN_MS`、不该再显示时为 `None`。宿主的钟比这边快一点时算
/// 刚刚操作。
fn driven_secs_ago(at_ms: u64, now_ms: u64) -> Option<u64> {
    let ago = now_ms.saturating_sub(at_ms);
    (ago < DRIVER_SHOWN_MS).then_some(ago / 1000)
}

/// 到驱动标记上的文字下一次要变（秒数加一或者消失）还要多久。
fn until_next_change(at_ms: u64, now_ms: u64) -> Duration {
    let ago = now_ms.saturating_sub(at_ms);
    Duration::from_millis(1000 - ago % 1000)
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

/// 驱动方怎么称呼：它所在会话的终端标题，找不到那个终端时是会话标识的前 8 位；不在 runode 的
/// 终端里跑的程序为 `None`。
fn driver_name(by: Option<&str>, title_of: impl Fn(SessionId) -> Option<String>) -> Option<String> {
    let by = by?;
    let title = by.parse::<SessionId>().ok().and_then(title_of);
    Some(title.unwrap_or_else(|| by.chars().take(8).collect()))
}

/// 驱动标记上的文字：「由 <驱动方> 操作 · <动作> <N>s 前」，按 `locale` 的语言。
fn driver_text(name: Option<&str>, action: DriveAction, secs: u64, locale: &str) -> String {
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
    rust_i18n::t!("driver.badge", locale = locale, who = who, action = action, secs = secs).into_owned()
}

impl Tab {
    /// 这个标签里有分屏正被别的终端里的程序操作着（驱动标记还没消失）。
    pub(super) fn driven(&self, now_ms: u64, cx: &App) -> bool {
        self.panes.values().any(|(view, _)| {
            view.read(cx).driver().is_some_and(|driver| driven_secs_ago(driver.at_ms, now_ms).is_some())
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
            let Some(secs) = driven_secs_ago(driver.at_ms, now) else {
                continue;
            };
            let name = driver_name(driver.by.as_deref(), |id| self.session_title(id, window, cx));
            badges.insert(*pane, driver_text(name.as_deref(), driver.action, secs, &locale).into());
        }
        badges
    }

    /// 显示会话 `id` 的终端的标题，在所有窗口里找。这个窗口正在更新、从窗口表里读不到，直接用
    /// `self`。
    fn session_title(&self, id: SessionId, window: &Window, cx: &App) -> Option<String> {
        let title_in = |view: &WindowView| {
            view.workspaces
                .iter()
                .flat_map(|workspace| &workspace.tabs)
                .flat_map(|tab| tab.panes.values())
                .map(|(terminal, _)| terminal.read(cx))
                .find(|terminal| terminal.session_id() == id)
                .map(|terminal| terminal.title().to_owned())
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

    /// 当前 workspace 里还有驱动标记（分屏上的文字或者标签上的图标）显示着时，到它下一次要变的
    /// 时候重画一次，过期的标记就自己消失了。
    fn schedule_driver_redraw(&mut self, now: u64, cx: &mut Context<Self>) {
        if self.driver_redraw.is_some() {
            return;
        }
        let next = self
            .workspace()
            .tabs
            .iter()
            .flat_map(|tab| tab.panes.values())
            .filter_map(|(view, _)| view.read(cx).driver().map(|driver: &Driver| driver.at_ms))
            .filter(|at| driven_secs_ago(*at, now).is_some())
            .map(|at| until_next_change(at, now))
            .min();
        let Some(next) = next else {
            return;
        };
        self.driver_redraw = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(next).await;
            this.update(cx, |this, cx| {
                this.driver_redraw = None;
                cx.notify();
            })
            .ok();
        }));
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
    fn the_badge_counts_seconds_and_goes_away_after_ten() {
        assert_eq!(driven_secs_ago(1_000, 1_000), Some(0));
        assert_eq!(driven_secs_ago(1_000, 4_999), Some(3));
        assert_eq!(driven_secs_ago(1_000, 10_999), Some(9));
        assert_eq!(driven_secs_ago(1_000, 11_000), None);
        // 宿主的钟快一点时算刚刚。
        assert_eq!(driven_secs_ago(5_000, 4_000), Some(0));
        assert_eq!(until_next_change(1_000, 4_250), Duration::from_millis(750));
        assert_eq!(until_next_change(1_000, 1_000), Duration::from_secs(1));
    }

    #[test]
    fn the_driver_is_named_by_its_terminal_title_or_its_id() {
        let by = "0123456789abcdef0123456789abcdef";
        let known: SessionId = by.parse().unwrap();
        let title_of = |id: SessionId| (id == known).then(|| "claude · runode".to_owned());
        assert_eq!(driver_name(Some(by), title_of).as_deref(), Some("claude · runode"));
        assert_eq!(driver_name(Some("fedcba9876543210fedcba9876543210"), title_of).as_deref(), Some("fedcba98"));
        assert_eq!(driver_name(Some("abc"), title_of).as_deref(), Some("abc"));
        assert_eq!(driver_name(None, title_of), None);
    }

    #[test]
    fn badge_text_names_the_driver_the_action_and_how_long_ago() {
        assert_eq!(driver_text(Some("claude"), DriveAction::Input, 3, "zh-Hans"), "由 claude 操作 · 打字 3s 前");
        assert_eq!(driver_text(Some("claude"), DriveAction::Keys, 0, "en"), "Driven by claude · keys 0s ago");
        assert_eq!(driver_text(None, DriveAction::Paste, 9, "en"), "Driven by a program outside Runode · paste 9s ago");
        for action in [DriveAction::ClearScreen, DriveAction::Kill, DriveAction::Unknown] {
            for locale in ["en", "zh-Hans", "zh-Hant"] {
                let text = driver_text(Some("x"), action, 1, locale);
                assert!(!text.contains("driver."), "{locale}: {text}");
            }
        }
    }
}
