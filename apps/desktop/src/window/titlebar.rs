//! 标题栏：窗口的标题栏设置、终端标签、排在它们后面的文件标签（预览栏的标签）和新建标签按钮、拖动中的
//! 标签，以及标题、agent 状态标记和快捷键提示。

use crate::ui::a11y::RevealOnHover;
use std::borrow::Cow;

use crate::ui::theme::ActiveTheme as _;

use gpui::{
    Action, AnyElement, App, Axis, Context, CursorStyle, Decorations, Div, ElementId, Focusable as _, FontWeight, Hsla,
    MouseButton, MouseDownEvent, Pixels, Point, Render, ResizeEdge, Role, SharedString, Stateful, TitlebarOptions,
    Window, WindowControlArea, WindowDecorations, div, img, linear_color_stop, linear_gradient, point, prelude::*, px,
    svg,
};

use super::{
    AGENT_MARK_SIZE, CloseOtherTabs, CloseTab, CloseTabsToRight, NEW_TAB_BUTTON_WIDTH, NEW_TAB_MENU_BUTTON_WIDTH,
    NewTab, PressDown, SelectLastTab, SelectTab, TAB_CLOSE_SIZE, TAB_TRACK_HEIGHT, TITLEBAR_HEIGHT,
    TRAFFIC_LIGHTS_ORIGIN, WindowView,
    agents::{Mark, Status},
    model::TabId,
    pane_move::DraggedPane,
    panes::now_ms,
};
use crate::ui::a11y::A11yPress;
use crate::ui::menu::{MenuHost as _, MenuItem};
use crate::ui::status_mark::{MarkKind, status_mark};
// 标签共用的拖动卡片和落点提示在 `runode_ui::tabs`，侧栏从这里引。
pub(super) use crate::ui::tabs::{drag_chip, drop_marker};
use crate::{
    preview_panel::FileTab,
    terminal_view::{DEFAULT_TITLE, TerminalView},
    ui::icons::PLUS_ICON,
    ui::{
        base_name,
        button::{Button, ButtonSize},
        file_icons::file_icon,
        font_size::FontSize,
        hsla,
        icons::{CHEVRON_DOWN_ICON, CLOSE_ICON, DIFF_ICON, MAXIMIZE_ICON, MINIMIZE_ICON, MINUS_ICON},
        panel::{added_label, removed_label},
        tooltip::{shortcut_text, tooltip},
    },
};

/// 标签右侧槽位的宽度，放快捷键提示或响铃标记；和关闭按钮一边一个，标题才在标签里居中。
const TAB_SIDE_SLOT: f32 = 20.;
/// 比这窄的标签只留标题：快捷键提示收起，关闭按钮悬停时浮在标题左边。
const TAB_COMPACT_WIDTH: f32 = 120.;
/// 卡片样式下标签胶囊的圆角。
const TAB_RADIUS: f32 = 8.;

/// 标题栏右上角一个窗口按钮的宽度，和 Windows 系统画的一样。
const WINDOW_CONTROL_WIDTH: f32 = 46.;
/// 合成器不画边框的窗口四边能拖着改尺寸的宽度。
const RESIZE_EDGE: f32 = 5.;
/// 窗口按钮的图标字体，Windows 10 起自带，系统自己的标题栏也用它。
const WINDOW_CONTROL_FONT: &str = "Segoe MDL2 Assets";
/// 鼠标停在关闭按钮上时的底色，和系统的一样。
const WINDOW_CLOSE_HOVER: u32 = 0xC42B1C;

/// 窗口按钮（最小化、最大化、关闭）由 app 自己画：Windows 上总是；Linux 上窗口是客户端装饰
/// （`window_decorations`）时。
pub(super) fn draws_window_controls(window: &Window) -> bool {
    cfg!(windows)
        || (cfg!(not(target_os = "macos")) && matches!(window.window_decorations(), Decorations::Client { .. }))
}

/// 标题栏右上角给窗口按钮（`window_controls`）让出的宽度，app 不画窗口按钮时为零。
pub(super) fn window_controls_width(window: &Window) -> f32 {
    if draws_window_controls(window) { 3. * WINDOW_CONTROL_WIDTH } else { 0. }
}

/// 窗口的标题栏设置：macOS 和 Windows 上透明，终端背景一直铺到窗口顶部，macOS 上标签栏画在红绿灯
/// 或侧栏右边，Windows 上右上角自己画窗口按钮。Linux 上不要系统的标题栏和边框（`window_decorations`）。
pub fn titlebar_options() -> TitlebarOptions {
    let macos = cfg!(target_os = "macos");
    TitlebarOptions {
        title: Some(DEFAULT_TITLE.into()),
        appears_transparent: macos || cfg!(windows),
        traffic_light_position: macos.then(|| point(px(TRAFFIC_LIGHTS_ORIGIN.0), px(TRAFFIC_LIGHTS_ORIGIN.1))),
    }
}

/// Linux 上窗口要客户端装饰：标签栏顶到窗口最上面，窗口按钮和四边改尺寸的一圈由 app 自己画，和 Windows
/// 上一样，不用合成器画的标题栏。合成器不肯时 GPUI 照旧给服务端装饰，实际的看 `Window::window_decorations`。
pub fn window_decorations() -> Option<WindowDecorations> {
    cfg!(target_os = "linux").then_some(WindowDecorations::Client)
}

/// 拖动中的标签：拖动时跟着鼠标画出来，放到另一个标签上时按 `id` 挪位置。
#[derive(Clone)]
struct DraggedTab {
    id: TabId,
    /// 开始拖动时所在的位置，用来决定落点提示画在目标标签的哪一边。
    ix: usize,
    title: SharedString,
    width: Pixels,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 底色和当前标签一样，见 `render_tab`。
        let bg = if super::cards(cx) { cx.theme().tab_active } else { cx.theme().mix(0.08) };
        drag_chip(self.width, px(TITLEBAR_HEIGHT), self.title.clone(), bg, cx).justify_center()
    }
}

/// 拖动中的文件标签：只能放到别的文件标签上，按 `ix` 挪位置。
#[derive(Clone)]
struct DraggedFileTab {
    /// 开始拖动时是第几个文件标签：挪的是它，落点提示也按它画在目标标签的哪一边。
    ix: usize,
    name: SharedString,
    width: Pixels,
}

impl Render for DraggedFileTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = if super::cards(cx) { cx.theme().tab_active } else { cx.theme().mix(0.08) };
        drag_chip(self.width, px(TITLEBAR_HEIGHT), self.name.clone(), bg, cx).justify_center()
    }
}

/// 当前标签的底色和悬停时的底色：卡片样式下当前标签是一颗胶囊，别的标签平时没有底色。
fn tab_backgrounds(cards: bool, cx: &App) -> (Hsla, Hsla) {
    if cards { (cx.theme().tab_active, cx.theme().accent) } else { (cx.theme().mix(0.08), cx.theme().mix(0.04)) }
}

/// 居中的标题，有 agent 时前面加上它的状态标记；`blink` 时等回答的标记闪烁。
pub(super) fn titled(title: SharedString, mark: Option<Mark>, id: impl Into<ElementId>, blink: bool, cx: &App) -> Div {
    div()
        .min_w_0()
        .flex()
        .justify_center()
        .items_center()
        .gap(px(5.))
        .children(mark.map(|mark| agent_mark(Some(mark), id, AGENT_MARK_SIZE, blink, cx)))
        .child(div().min_w_0().truncate().child(title))
}

/// 分屏或一组分屏的状态标记，`size` 见方，不分是哪家 agent：等回答是琥珀色的光标（`blink` 时闪烁），
/// 在干活是盲文格，干完了没看是对勾，空闲和不是 agent（`mark` 为空）是提示符。
pub(super) fn agent_mark(mark: Option<Mark>, id: impl Into<ElementId>, size: f32, blink: bool, cx: &App) -> AnyElement {
    let kind = match mark.map(|mark| mark.status) {
        Some(Status::Blocked) => MarkKind::Waiting { blink },
        Some(Status::Working) => MarkKind::Working { spin: true },
        Some(Status::Done) => MarkKind::Done,
        Some(Status::Idle) | None => MarkKind::Idle,
    };
    status_mark(id, kind, size, cx)
}

/// 标签上的小图标：这个标签里有分屏正被别的终端里的程序操作着（分屏右上角有驱动标记）。
fn driven_icon(id: impl Into<ElementId>, fg: Hsla) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Image)
        .aria_label(rust_i18n::t!("driver.tab"))
        .flex_none()
        .text_size(px(15.))
        .line_height(px(15.))
        .text_color(fg.opacity(0.8))
        .child("⌨")
}

/// 卡片样式下标签和分屏标题条上写的：名字和所在目录。前台是认得的 agent 时名字是它设的标题
/// （Claude Code 写的是在做的事），标题只是目录名或没设时是 agent 的名字（`Codex`、`Claude Code`）。
/// 开来跑命令的终端（`TerminalView::task`）名字是那条命令，命令在跑和跑完了
/// 都一样；不是的话 shell 在前台时是 shell 自己（`zsh`、`fish`），别的程序是它设的
/// 标题，没设时是程序名；目录和名字一样时不再写。
pub(super) fn pane_label(view: &TerminalView) -> (SharedString, Option<SharedString>) {
    let meta = view.meta();
    let name = if let Some(agent) = meta.agent.filter(|agent| agent.kind.is_known()) {
        let dir_name = view.cwd().and_then(|dir| dir.file_name().map(|name| name.to_string_lossy().into_owned()));
        let title = meta.title.clone().filter(|title| dir_name.as_ref() != Some(title));
        title.or_else(|| Some(agent.kind.display_name().to_owned()))
    } else if let Some(task) = view.task() {
        Some(task.to_string())
    } else {
        let program = meta.foreground.as_deref().map(|name| runode_terminal::pty::program_label(name).to_owned());
        if meta.foreground_is_shell { program } else { meta.title.clone().or(program) }
    }
    .unwrap_or_else(|| view.title().to_owned());
    let dir = view.display_cwd().filter(|dir| *dir != name);
    (name.into(), dir.map(Into::into))
}

/// 快捷键提示，从键位表里查，快捷键改了也跟着变：先找 `select` 的绑定，`is_last` 时再找
/// `last` 的。后加的绑定优先，显示最后一个。
pub(super) fn shortcut_hint(select: &dyn Action, last: &dyn Action, is_last: bool, cx: &App) -> Option<SharedString> {
    shortcut_text(select, cx).or_else(|| if is_last { shortcut_text(last, cx) } else { None })
}

impl WindowView {
    /// 右键点了终端标签 `id`：先切过去，再在 `position` 弹出新建、关闭这几样的菜单，都对着它办。
    fn open_tab_menu(&mut self, id: TabId, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        // 快捷键按切过去之前的焦点找：刚切过去的终端上一帧还没画，键位表里找不到它。
        let lookup = self.focus_handle(cx);
        self.activate_tab_id(id, window, cx);
        let tabs = &self.workspace().tabs;
        let Some(ix) = tabs.iter().position(|tab| tab.id == id) else { return };
        let count = tabs.len();
        let focus = self.focus_handle(cx);
        let item = |key: &str, action: Box<dyn Action>, enabled: bool| {
            let shortcut = crate::ui::tooltip::shortcut_text_in(action.as_ref(), &lookup, window);
            Some(MenuItem::new(rust_i18n::t!(key), action, enabled, cx).with_shortcut(shortcut))
        };
        let items = vec![
            item("menu.new_tab", Box::new(NewTab), true),
            None,
            item("menu.close_tab", Box::new(CloseTab), true),
            item("menu.close_other_tabs", Box::new(CloseOtherTabs), count > 1),
            item("menu.close_tabs_right", Box::new(CloseTabsToRight), ix + 1 < count),
        ];
        self.open_context_menu(position, items, focus, cx);
    }

    /// 切到当前 workspace 里的标签 `id`。点标签时按 id 找：画出标签条以后标签可能已经增删、挪了位置，
    /// 也可能换了 workspace，照画的时候的序号会切到别的标签上，甚至越界。
    fn activate_tab_id(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.workspace().tabs.iter().position(|tab| tab.id == id)
            && self.leave_todo_page(window, cx)
        {
            self.activate(ix, window, cx);
        }
    }

    /// 标签条里当前的是第几个：终端标签在前，文件标签接在后面；文件标签在最前时是它。没有终端标签、文件
    /// 又不在最前时为空。
    pub(super) fn front_tab(&self, cx: &App) -> Option<usize> {
        let workspace = self.workspace();
        match self.preview.read(cx).front_file(workspace.id).filter(|_| self.preview_front(cx)) {
            Some(file) => Some(workspace.tabs.len() + file),
            None => (workspace.active < workspace.tabs.len()).then_some(workspace.active),
        }
    }

    /// 标题栏里排的标签数：终端标签加文件标签。
    pub(super) fn titlebar_tab_count(&self, cx: &App) -> usize {
        self.workspace().tabs.len() + self.preview.read(cx).file_tab_count(self.workspace().id)
    }

    /// 标签条里的标签：终端标签，后面接着文件标签，一样宽。
    pub(super) fn render_tabs(&self, width: Pixels, cards: bool, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let terminals = (0..self.workspace().tabs.len()).map(|ix| self.render_tab(ix, width, cards, cx));
        let mut tabs: Vec<_> = terminals.collect();
        let (files, _) = self.preview.read(cx).file_tabs(self.workspace().id);
        tabs.extend(files.iter().enumerate().map(|(ix, tab)| self.render_file_tab(ix, tab, width, cards, cx)));
        tabs
    }

    pub(super) fn render_tab(&self, ix: usize, width: Pixels, cards: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let workspace = self.workspace();
        let tab = &workspace.tabs[ix];
        let id = tab.id;
        let title = SharedString::from(tab.focused_view().read(cx).title().to_owned());
        let mark = tab.mark(cx);
        let front = self.front_tab(cx);
        let active = front == Some(ix);
        let after_front = front.is_some_and(|front| ix == front + 1);
        let (active_bg, hover_bg) = tab_backgrounds(cards, cx);
        let label = cards.then(|| pane_label(tab.focused_view().read(cx)));
        // 报给辅助工具的名字是标签上写的标题，agent 的状态和响铃这些只画成图形的写进说明。
        let a11y_label = label.as_ref().map_or_else(|| title.clone(), |(name, _)| name.clone());
        let a11y_description = [
            label.as_ref().and_then(|(_, dir)| dir.as_ref().map(ToString::to_string)),
            mark.map(Mark::describe),
            tab.bell.then(|| rust_i18n::t!("workspace.bell").into_owned()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let close_label = rust_i18n::t!("menu.close_tab");
        let close_tooltip = tooltip(close_label.clone(), None);
        let driven_tooltip = tab.driven(now_ms(), cx).then(|| tooltip(rust_i18n::t!("driver.tab"), None));
        let fg = cx.theme().foreground;
        let group = SharedString::from(format!("tab-{ix}"));
        // 当前标签自己就是一块亮色，和它相邻的分隔线去掉，只是不画颜色，免得宽度跳动。
        let divider = if active || after_front { fg.opacity(0.) } else { cx.theme().border };
        let dragged = DraggedTab { id, ix, title: title.clone(), width };
        let compact = width < px(TAB_COMPACT_WIDTH);
        let bell_dot = || div().size(px(6.)).rounded_full().bg(fg.opacity(0.8));
        let hint_color = if cards { cx.theme().muted_foreground } else { fg.opacity(0.35) };
        // 右侧槽位的内容：响铃标记优先，其次快捷键提示。
        let side_content = |shortcut: Option<SharedString>| {
            if tab.bell {
                bell_dot().into_any_element()
            } else {
                div().text_size(FontSize::Small).text_color(hint_color).children(shortcut).into_any_element()
            }
        };
        // 默认是 ⌘1 到 ⌘8 对应前八个标签，⌘9 对应最后一个。
        let shortcut = shortcut_hint(&SelectTab(ix), &SelectLastTab, ix + 1 == self.titlebar_tab_count(cx), cx);
        // 紧凑时只在响铃时占一个圆点的宽度。
        let side_slot = if compact {
            tab.bell.then(|| div().flex_none().child(bell_dot()))
        } else {
            Some(
                div()
                    .flex_none()
                    .w(px(TAB_SIDE_SLOT))
                    .flex()
                    .justify_end()
                    .items_center()
                    .child(side_content(shortcut.clone())),
            )
        };
        let view = cx.entity().downgrade();
        div()
            .id(("tab", id))
            .role(Role::Tab)
            .aria_label(a11y_label)
            .aria_selected(active)
            .when(!a11y_description.is_empty(), |tab| tab.aria_description(a11y_description))
            .group(group.clone())
            .map(|tab| {
                if cards {
                    // 紧凑时留白和间距收窄，给名字多留几个字的地方。
                    tab.relative()
                        .flex_none()
                        .w(width)
                        .h_full()
                        .rounded(px(TAB_RADIUS))
                        .pl(px(if compact { 6. } else { 10. }))
                        .pr(px(if compact { 4. } else { 8. }))
                        .gap(px(if compact { 4. } else { 8. }))
                } else {
                    tab.flex_none()
                        .w(width)
                        .h_full()
                        .when(ix > 0, |tab| tab.border_l_1().border_color(divider))
                        .px(px(if compact { 4. } else { 8. }))
                        .gap(px(4.))
                }
            })
            .overflow_hidden()
            .flex()
            .items_center()
            .map(|tab| match (cards, active) {
                (true, true) => tab.bg(active_bg).text_color(fg).font_weight(FontWeight::MEDIUM),
                (true, false) => tab.text_color(cx.theme().subtle_foreground).hover(|tab| tab.bg(hover_bg)),
                (false, true) => tab.bg(active_bg).text_color(fg),
                (false, false) => {
                    tab.text_color(fg.opacity(0.55)).hover(|tab| tab.bg(hover_bg).text_color(fg.opacity(0.8)))
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    // 标签铺满了标题栏，双击标签和双击空白标题栏一样缩放窗口。
                    if event.click_count >= 2 {
                        crate::ui::titlebar_double_click(window);
                    } else {
                        this.activate_tab_id(id, window, cx);
                    }
                }),
            )
            // 按下要看双击，用不了 `PressDown`；辅助工具按下时就是单击切过去。
            .on_a11y_press(view, move |this, window, cx| this.activate_tab_id(id, window, cx))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_tab_by_id(id, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_tab_menu(id, event.position, window, cx);
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedTab>(move |style, dragged, _, cx| {
                drop_marker(style, dragged.ix, ix, Axis::Horizontal, cx)
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                this.move_tab(dragged.id, ix, window, cx);
            }))
            // 拖来的分屏：挪进这个标签。
            .drag_over::<DraggedPane>(move |style, _, _, cx| style.bg(cx.theme().ring.opacity(0.18)))
            .on_drop(cx.listener(move |this, dragged: &DraggedPane, window, cx| {
                this.drop_pane_on_tab(dragged.pane, id, window, cx);
            }))
            .map(|el| match label {
                Some((name, _)) => {
                    // 卡片样式：状态标记（不是 agent 时是提示符）、靠左的名字（放不下时末尾渐隐），悬停时
                    // 右边出关闭按钮。目录不画，只报给辅助工具。
                    let tab_bg = if active { active_bg } else { cx.theme().frame };
                    let fade = |color: Hsla| {
                        linear_gradient(90., linear_color_stop(color.opacity(0.), 0.), linear_color_stop(color, 1.))
                    };
                    let label = div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .child(div().flex_none().whitespace_nowrap().text_size(FontSize::Medium).child(name))
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .h_full()
                                .w(px(20.))
                                .bg(fade(tab_bg))
                                .when(!active, |fader| {
                                    fader.group_hover(group.clone(), |fader| fader.bg(fade(hover_bg)))
                                }),
                        );
                    el.child(agent_mark(mark, ("tab-agent", id), AGENT_MARK_SIZE, !active, cx))
                        .child(label)
                        .children(driven_tooltip.map(|driven| driven_icon(("tab-driven", id), fg).tooltip(driven)))
                        .map(|el| {
                            let close = Button::new(("tab-close", id), close_label.clone())
                                .with_size(ButtonSize::IconXs)
                                .text_size(px(14.))
                                .child("×")
                                .tooltip(close_tooltip)
                                .on_press_down(cx, move |this, window, cx| this.close_tab_by_id(id, window, cx));
                            if compact {
                                // 平时宽度为零，悬停时才撑开；不能用 display 切换，见下面经典样式的说明。
                                el.children(tab.bell.then(|| div().flex_none().child(bell_dot()))).child(
                                    close
                                        .w_0()
                                        .overflow_hidden()
                                        .group_hover(group, |close| close.w(px(TAB_CLOSE_SIZE))),
                                )
                            } else {
                                // 右侧槽位平时放响铃标记或快捷键提示，悬停时换成关闭按钮，宽度不变。
                                el.child(
                                    div()
                                        .flex_none()
                                        .relative()
                                        .h_full()
                                        .min_w(px(TAB_CLOSE_SIZE))
                                        .flex()
                                        .items_center()
                                        .justify_end()
                                        .child(
                                            div()
                                                .group_hover(group.clone(), |hint| hint.invisible())
                                                .child(side_content(shortcut)),
                                        )
                                        .child(
                                            div()
                                                .absolute()
                                                .inset_0()
                                                .flex()
                                                .items_center()
                                                .justify_end()
                                                .child(close.reveal_on_hover(group)),
                                        ),
                                )
                            }
                        })
                }
                _ => el
                    .child(
                        Button::new(("tab-close", id), close_label)
                            .with_size(ButtonSize::IconXs)
                            .text_size(px(14.))
                            .child("×")
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
                                    close.reveal_on_hover(group)
                                }
                            })
                            .tooltip(close_tooltip)
                            .on_press_down(cx, move |this, window, cx| this.close_tab_by_id(id, window, cx)),
                    )
                    .child(titled(title, mark, ("tab-agent", id), !active, cx).flex_1())
                    .children(driven_tooltip.map(|driven| driven_icon(("tab-driven", id), fg).tooltip(driven)))
                    .children(side_slot),
            })
    }

    /// 第 `ix` 个文件标签，排在终端标签后面，样子跟着终端标签：文件图标（diff 标签是 diff 的图标）、按 git
    /// 状态上色的名字（临时标签斜体）、增删的行数、改过没存的圆点；右边平时是快捷键提示，当前标签和悬停时
    /// 换成关闭按钮。单击切过去，双击固定，中键关掉，右键弹菜单，拖动换位置。
    pub(super) fn render_file_tab(
        &self,
        ix: usize,
        tab: &FileTab,
        width: Pixels,
        cards: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let pos = self.workspace().tabs.len() + ix;
        let front = self.front_tab(cx);
        let active = front == Some(pos);
        let after_front = front.is_some_and(|front| pos == front + 1);
        let (active_bg, hover_bg) = tab_backgrounds(cards, cx);
        let fg = cx.theme().foreground;
        let color = tab.color.map_or(fg, hsla);
        let group = SharedString::from(format!("file-tab-{}", tab.key));
        let close_text = rust_i18n::t!("tooltip.close_preview");
        let close_tooltip = tooltip(close_text.clone(), None);
        // 报给辅助工具的名字带上增删的行数和改过没存，界面上这些画成带色的数和圆点。
        let label = match tab.stat {
            Some((added, removed)) => format!("{} +{added} −{removed}", tab.name),
            None => tab.name.to_string(),
        };
        let label = if tab.dirty { rust_i18n::t!("preview.modified_tab", name = label).into_owned() } else { label };
        let icon = if tab.diff {
            svg().path(DIFF_ICON).flex_none().size(px(14.)).text_color(fg.opacity(0.8)).into_any_element()
        } else {
            img(file_icon(&base_name(&tab.path))).flex_none().size(px(14.)).into_any_element()
        };
        let divider = if active || after_front { fg.opacity(0.) } else { cx.theme().border };
        let is_last = pos + 1 == self.titlebar_tab_count(cx);
        let shortcut = (width >= px(TAB_COMPACT_WIDTH))
            .then(|| shortcut_hint(&SelectTab(pos), &SelectLastTab, is_last, cx))
            .flatten();
        let dragged = DraggedFileTab { ix, name: tab.name.clone(), width };
        let view = cx.entity().downgrade();
        div()
            .id(SharedString::from(format!("file-tab:{}", tab.key)))
            .role(Role::Tab)
            .aria_label(label)
            .aria_description(tab.path.display().to_string())
            .aria_selected(active)
            .on_a11y_press(view, move |this, window, cx| {
                this.show_file_tab(ix, window, cx);
            })
            .group(group.clone())
            .map(|el| {
                if cards {
                    el.relative().flex_none().w(width).h_full().rounded(px(TAB_RADIUS))
                } else {
                    el.flex_none().w(width).h_full().border_l_1().border_color(divider)
                }
            })
            .overflow_hidden()
            .flex()
            .items_center()
            .px(px(8.))
            .gap(px(5.))
            .map(|el| if active { el.bg(active_bg) } else { el.hover(|el| el.bg(hover_bg)) })
            .child(div().flex_none().flex().when(!active, |icon| icon.opacity(0.6)).child(icon))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(if active { color } else { color.opacity(0.6) })
                    .when(!tab.pinned, |name| name.italic())
                    .child(tab.name.clone()),
            )
            .when_some(tab.stat, |el, (added, removed)| {
                el.child(
                    div()
                        .flex_none()
                        .flex()
                        .gap(px(4.))
                        .when(!active, |stat| stat.opacity(0.6))
                        .when(added > 0, |stat| stat.child(added_label(added)))
                        .when(removed > 0, |stat| stat.child(removed_label(removed))),
                )
            })
            .when(tab.dirty, |el| el.child(div().flex_none().size(px(6.)).rounded_full().bg(color)))
            // 右边的槽位靠右，名字短时也贴着右边：快捷键提示和关闭按钮叠在一起，宽度不跳。
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .relative()
                    .h_full()
                    .min_w(px(TAB_CLOSE_SIZE))
                    .flex()
                    .items_center()
                    .justify_end()
                    .children(shortcut.filter(|_| !active).map(|shortcut| {
                        div()
                            .text_size(FontSize::Small)
                            .text_color(fg.opacity(0.35))
                            .group_hover(group.clone(), |hint| hint.invisible())
                            .child(shortcut)
                    }))
                    .child(
                        div().absolute().inset_0().flex().items_center().justify_end().child(
                            Button::new(SharedString::from(format!("file-tab-close:{}", tab.key)), close_text)
                                .with_size(ButtonSize::IconXs)
                                .text_size(px(14.))
                                .child("×")
                                .when(!active, |close| close.reveal_on_hover(group))
                                .tooltip(close_tooltip)
                                .on_press_down(cx, move |this, window, cx| this.close_file_tab(ix, window, cx)),
                        ),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        this.pin_file_tab(ix, cx);
                    }
                    this.show_file_tab(ix, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_file_tab(ix, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_file_tab_menu(ix, event.position, window, cx);
                }),
            )
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedFileTab>(move |style, dragged, _, cx| {
                drop_marker(style, dragged.ix, ix, Axis::Horizontal, cx)
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedFileTab, window, cx| {
                this.move_file_tab(dragged.ix, ix, window, cx);
            }))
    }

    /// 卡片样式下标签条右边的新建标签按钮：「+」用默认 shell 开新标签，旁边的箭头弹出新建标签菜单。
    pub(super) fn render_new_tab_button_card(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let height = px(TAB_TRACK_HEIGHT - 4.);
        // 两个按钮挨着、底下垫一块和标签条同色的圆角底，只圆外侧的角，看着是一个按钮分成两半。
        let new_tab = new_tab_button(ButtonSize::Icon, cx)
            .w(px(NEW_TAB_BUTTON_WIDTH - NEW_TAB_MENU_BUTTON_WIDTH - 8.))
            .h(height)
            .rounded(px(0.))
            .rounded_l(px(4.));
        let options = new_tab_options_button(ButtonSize::Icon, self.new_tab_menu_open())
            .w(px(NEW_TAB_MENU_BUTTON_WIDTH))
            .h(height)
            .rounded(px(0.))
            .rounded_r(px(4.));
        div()
            .flex_none()
            .flex()
            .items_center()
            .ml(px(4.))
            .mr(px(4.))
            .rounded(px(4.))
            .bg(cx.theme().tab_track)
            .child(new_tab)
            .child(self.with_new_tab_menu(options, window, cx))
    }

    /// 经典样式下标签条右边的新建标签按钮，和卡片样式的一样分成「+」和弹出菜单的箭头。
    pub(super) fn render_new_tab_button(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let new_tab = new_tab_button(ButtonSize::IconSm, cx)
            .w(px(NEW_TAB_BUTTON_WIDTH - NEW_TAB_MENU_BUTTON_WIDTH))
            .h_full()
            .rounded(px(0.));
        let options = new_tab_options_button(ButtonSize::IconSm, self.new_tab_menu_open())
            .w(px(NEW_TAB_MENU_BUTTON_WIDTH))
            .h_full()
            .rounded(px(0.));
        div()
            .flex_none()
            .flex()
            .h_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(new_tab)
            .child(self.with_new_tab_menu(options, window, cx))
    }
}

/// 「+」：按下就用默认 shell 开新标签。图标平时也不调暗，和别的只有图标的按钮不同。
fn new_tab_button(size: ButtonSize, cx: &mut Context<WindowView>) -> Button {
    Button::new("new-tab", rust_i18n::t!("menu.new_tab"))
        .icon(PLUS_ICON)
        .icon_size(16.)
        .with_size(size)
        .text_color(cx.theme().foreground)
        .tooltip(tooltip(rust_i18n::t!("menu.new_tab"), Some(&NewTab)))
        .on_press_down(cx, |this, window, cx| this.new_tab(&NewTab, window, cx))
}

/// 「+」旁边弹出新建标签菜单的箭头。
/// 弹出新建标签菜单的箭头；菜单 `open` 着时不挂提示，免得到点盖在菜单上。
fn new_tab_options_button(size: ButtonSize, open: bool) -> Button {
    let label = rust_i18n::t!("menu.new_tab_options");
    Button::new("new-tab-menu", label.clone())
        .icon(CHEVRON_DOWN_ICON)
        .icon_size(12.)
        .with_size(size)
        .when(!open, |button| button.tooltip(tooltip(label, None)))
}

/// 标题栏右上角的最小化、最大化（最大化着时是还原）和关闭按钮（`draws_window_controls`），盖在窗口最上层，
/// 全屏时不画。Windows 上鼠标按下由系统照命中测试的结果（`WindowControlArea`）办，这里只画样子；Linux 上
/// 没有这套命中测试，点击自己办。辅助工具的按下另外登记。
pub(super) fn window_controls<T: 'static>(window: &Window, cx: &mut Context<T>) -> Option<Div> {
    if !draws_window_controls(window) || window.is_fullscreen() {
        return None;
    }
    let fg = cx.theme().foreground;
    let hover = cx.theme().mix(0.08);
    let view = cx.entity().downgrade();
    // Windows 上用系统标题栏的图标字体，Linux 上没有这个字体，用图标。
    let button = |id: &'static str,
                  area: WindowControlArea,
                  (glyph, icon): (&'static str, &'static str),
                  label: Cow<'_, str>,
                  press: fn(&mut Window, &mut App)| {
        let face = if cfg!(windows) {
            div().font_family(WINDOW_CONTROL_FONT).text_size(px(10.)).child(glyph).into_any_element()
        } else {
            svg().path(icon).size(px(12.)).text_color(fg).into_any_element()
        };
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(SharedString::from(label.into_owned()))
            .window_control_area(area)
            // 挡住下面的拖动区：GPUI 按登记的先后认窗口控制区，先画的拖动区会抢在前面。
            .occlude()
            .w(px(WINDOW_CONTROL_WIDTH))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(fg)
            .child(face)
            .when(!cfg!(windows), |button| button.on_click(move |_, window, cx| press(window, cx)))
            .on_a11y_press(view.clone(), move |_, window, cx| press(window, cx))
    };
    let (max_face, max_label) = if window.is_maximized() {
        (("\u{E923}", MINIMIZE_ICON), rust_i18n::t!("window_controls.restore"))
    } else {
        (("\u{E922}", MAXIMIZE_ICON), rust_i18n::t!("window_controls.maximize"))
    };
    Some(
        div()
            .absolute()
            .top_0()
            .right_0()
            .h(px(TITLEBAR_HEIGHT))
            .flex()
            .child(
                button(
                    "window-minimize",
                    WindowControlArea::Min,
                    ("\u{E921}", MINUS_ICON),
                    rust_i18n::t!("window_controls.minimize"),
                    |window, _| window.minimize_window(),
                )
                .hover(|el| el.bg(hover)),
            )
            .child(
                button("window-maximize", WindowControlArea::Max, max_face, max_label, |window, _| {
                    crate::ui::toggle_maximize(window)
                })
                .hover(|el| el.bg(hover)),
            )
            .child(
                button(
                    "window-close",
                    WindowControlArea::Close,
                    ("\u{E8BB}", CLOSE_ICON),
                    rust_i18n::t!("window_controls.close"),
                    |window, cx| window.dispatch_action(Box::new(crate::menus::CloseWindow), cx),
                )
                .hover(|el| el.bg(gpui::rgb(WINDOW_CLOSE_HOVER)).text_color(gpui::white())),
            ),
    )
}

/// 合成器不画边框（`Decorations::Client`）时窗口四边和四角能拖着改尺寸的一圈；铺满屏幕、最大化、贴着
/// 屏幕边的那几边不画。
pub(super) fn resize_edges(window: &Window) -> Option<Div> {
    let Decorations::Client { tiling } = window.window_decorations() else { return None };
    if cfg!(target_os = "macos") || window.is_fullscreen() || window.is_maximized() {
        return None;
    }
    let edge = px(RESIZE_EDGE);
    let handle = |id: &'static str, edge: ResizeEdge, cursor: CursorStyle| {
        div().id(id).absolute().occlude().cursor(cursor).on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.start_window_resize(edge);
        })
    };
    let (top, bottom, left, right) = (!tiling.top, !tiling.bottom, !tiling.left, !tiling.right);
    Some(
        div()
            .absolute()
            .size_full()
            .when(top, |el| {
                el.child(
                    handle("resize-top", ResizeEdge::Top, CursorStyle::ResizeUpDown).top_0().left_0().w_full().h(edge),
                )
            })
            .when(bottom, |el| {
                el.child(
                    handle("resize-bottom", ResizeEdge::Bottom, CursorStyle::ResizeUpDown)
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(edge),
                )
            })
            .when(left, |el| {
                el.child(
                    handle("resize-left", ResizeEdge::Left, CursorStyle::ResizeLeftRight)
                        .top_0()
                        .left_0()
                        .h_full()
                        .w(edge),
                )
            })
            .when(right, |el| {
                el.child(
                    handle("resize-right", ResizeEdge::Right, CursorStyle::ResizeLeftRight)
                        .top_0()
                        .right_0()
                        .h_full()
                        .w(edge),
                )
            })
            .when(top && left, |el| {
                el.child(
                    handle("resize-top-left", ResizeEdge::TopLeft, CursorStyle::ResizeUpLeftDownRight)
                        .top_0()
                        .left_0()
                        .size(edge * 2.),
                )
            })
            .when(top && right, |el| {
                el.child(
                    handle("resize-top-right", ResizeEdge::TopRight, CursorStyle::ResizeUpRightDownLeft)
                        .top_0()
                        .right_0()
                        .size(edge * 2.),
                )
            })
            .when(bottom && left, |el| {
                el.child(
                    handle("resize-bottom-left", ResizeEdge::BottomLeft, CursorStyle::ResizeUpRightDownLeft)
                        .bottom_0()
                        .left_0()
                        .size(edge * 2.),
                )
            })
            .when(bottom && right, |el| {
                el.child(
                    handle("resize-bottom-right", ResizeEdge::BottomRight, CursorStyle::ResizeUpLeftDownRight)
                        .bottom_0()
                        .right_0()
                        .size(edge * 2.),
                )
            }),
    )
}
