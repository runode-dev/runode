//! 标题栏：窗口的标题栏设置、终端标签、排在它们后面的文件标签（预览栏的标签）和新建标签按钮、拖动中的
//! 标签，以及标题、agent 状态标记和快捷键提示。

use std::borrow::Cow;

use crate::ui::theme::ActiveTheme as _;

use gpui::{
    Action, AnyElement, App, Axis, BoxShadow, Context, Div, ElementId, Hsla, MouseButton, MouseDownEvent, Pixels,
    Render, Role, SharedString, Stateful, TitlebarOptions, Window, WindowControlArea, div, img, linear_color_stop,
    linear_gradient, point, prelude::*, px, svg,
};
use runode_types::agent::AgentKind;

use super::{
    AGENT_MARK_WIDTH, NEW_TAB_BUTTON_WIDTH, NewTab, PressDown, SelectLastTab, SelectTab, TAB_CLOSE_SIZE,
    TAB_TRACK_HEIGHT, TITLEBAR_HEIGHT, TRAFFIC_LIGHTS_ORIGIN, WindowView,
    agents::{
        Mark, Status,
        logo::{accent, agent_logo, brand_color},
    },
    model::{Tab, TabId},
    pane_move::DraggedPane,
    panes::now_ms,
};
use crate::ui::a11y::A11yPress;
// agent 状态标记的颜色和转圈在 `runode_ui`，看板也用。
use crate::ui::{
    spinner::spinner,
    theme::{AGENT_BLOCKED_COLOR, AGENT_DONE_COLOR},
};
// 标签共用的拖动卡片和落点提示在 `runode_ui::tabs`，侧栏从这里引。
pub(super) use crate::ui::tabs::{drag_chip, drop_marker};
use crate::{
    assets::{PIXEL_QUESTION_ICON, PLUS_ICON, PROMPT_ICON},
    preview_panel::FileTab,
    terminal_view::{DEFAULT_TITLE, TerminalView},
    ui::{
        base_name,
        button::{Button, ButtonSize},
        display_dir,
        file_icons::file_icon,
        font_size::FontSize,
        hsla,
        icons::DIFF_ICON,
        panel::{added_label, removed_label},
        tooltip::{shortcut_text, tooltip},
    },
};

/// 标签右侧槽位的宽度，放快捷键提示或响铃标记；和关闭按钮一边一个，标题才在标签里居中。
const TAB_SIDE_SLOT: f32 = 20.;
/// 比这窄的标签只留标题：快捷键提示收起，关闭按钮悬停时浮在标题左边。
const TAB_COMPACT_WIDTH: f32 = 120.;

/// Windows 上标题栏右上角一个窗口按钮的宽度，和系统画的一样。
const WINDOW_CONTROL_WIDTH: f32 = 46.;
/// 标题栏右上角给窗口按钮（`render_window_controls`）让出的宽度，只有 Windows 上不为零。
pub(super) const WINDOW_CONTROLS_WIDTH: f32 = if cfg!(windows) { 3. * WINDOW_CONTROL_WIDTH } else { 0. };
/// 窗口按钮的图标字体，Windows 10 起自带，系统自己的标题栏也用它。
const WINDOW_CONTROL_FONT: &str = "Segoe MDL2 Assets";
/// 鼠标停在关闭按钮上时的底色，和系统的一样。
const WINDOW_CLOSE_HOVER: u32 = 0xC42B1C;

/// 窗口的标题栏设置：macOS 和 Windows 上透明，终端背景一直铺到窗口顶部，macOS 上标签栏画在红绿灯
/// 或侧栏右边，Windows 上右上角自己画窗口按钮；别的系统上用系统自己的标题栏和边框，标签栏在它下面。
pub fn titlebar_options() -> TitlebarOptions {
    let macos = cfg!(target_os = "macos");
    TitlebarOptions {
        title: Some(DEFAULT_TITLE.into()),
        appears_transparent: macos || cfg!(windows),
        traffic_light_position: macos.then(|| point(px(TRAFFIC_LIGHTS_ORIGIN.0), px(TRAFFIC_LIGHTS_ORIGIN.1))),
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

/// 当前标签的底色和悬停时的底色：卡片样式下当前标签是终端背景色的胶囊，从标签条上浮起来。
fn tab_backgrounds(cards: bool, cx: &App) -> (Hsla, Hsla) {
    if cards {
        let (track, pill) = crate::ui::theme::tab_track_colors(cx.theme().fg_rgb(), cx.theme().bg_rgb());
        (hsla(pill), hsla(track.mix(pill, 0.5)))
    } else {
        (cx.theme().mix(0.08), cx.theme().mix(0.04))
    }
}

/// 卡片样式下当前标签底下的一点阴影。
fn pill_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: Hsla::black().opacity(0.08),
        offset: point(px(0.), px(1.)),
        blur_radius: px(2.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// 居中的标题，有 agent 时前面加上它的状态标记。
pub(super) fn titled(title: SharedString, mark: Option<Mark>, id: impl Into<ElementId>, fg: Hsla) -> Div {
    div()
        .min_w_0()
        .flex()
        .justify_center()
        .items_center()
        .gap(px(5.))
        .children(mark.map(|mark| styled_agent_mark(mark, id, fg, false)))
        .child(div().min_w_0().truncate().child(title))
}

/// agent 的状态标记：工作中播放该 agent 自己的工作动画，空闲时是一个空心圆点，等回答是琥珀色
/// 实心圆点，干完了没看是绿色的对勾。`brand` 时（卡片样式）工作中的转圈放大加粗，等回答画成
/// 像素画的问号，都用这个 agent 自己的颜色（`brand_color`），没有的转圈沿用文字颜色、问号还是琥珀色。
pub(super) fn styled_agent_mark(mark: Mark, id: impl Into<ElementId>, fg: Hsla, brand: bool) -> AnyElement {
    let slot = div().flex_none().w(px(AGENT_MARK_WIDTH)).flex().justify_center().items_center();
    match mark.status {
        // 所有转圈的标记共用同一个时钟，同一种 agent 的几个标签一起转时步调一致。
        Status::Working => {
            let (frames, frame_time) = mark.kind.spinner();
            // 卡片样式下转圈放大一号、加粗，用这个 agent 自己的颜色，几个点、圈的字符也看得出在动。
            let color = mark.kind.spinner_color().or_else(|| brand.then(|| brand_color(mark.kind)).flatten());
            slot.when_some(color, |slot, color| slot.text_color(gpui::rgb(color)))
                .when(brand, |slot| slot.text_size(px(15.)).font_weight(gpui::FontWeight::BOLD))
                .child(spinner(id, frames, frame_time))
                .into_any_element()
        }
        Status::Idle => {
            slot.child(div().size(px(6.)).rounded_full().border_1().border_color(fg.opacity(0.6))).into_any_element()
        }
        // 等用户回答：不动的实心琥珀色圆点，比空闲显眼。
        Status::Blocked => {
            if !brand {
                return slot
                    .child(div().size(px(7.)).rounded_full().bg(gpui::rgb(AGENT_BLOCKED_COLOR)))
                    .into_any_element();
            }
            // 卡片样式下是像素画的问号，一眼看出是在问你。6×9 格，每格 1.5 点，Retina 屏上正好 3 个像素。
            let color = brand_color(mark.kind).unwrap_or(AGENT_BLOCKED_COLOR);
            slot.child(svg().path(PIXEL_QUESTION_ICON).flex_none().w(px(9.)).h(px(13.5)).text_color(gpui::rgb(color)))
                .into_any_element()
        }
        // 干完了还没看：绿色对勾，和等回答的圆点形状也不同，不靠颜色也分得开。
        Status::Done => slot.text_size(px(11.)).text_color(gpui::rgb(AGENT_DONE_COLOR)).child("✓").into_any_element(),
    }
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
/// （Claude Code 写的是在做的事），标题只是目录名或没设时是 agent 的名字（`Codex`、`Claude Code`），
/// 是谁已经由前面的 logo 标出。shell 在前台时名字是 shell 自己（`zsh`、`fish`），别的程序是它设的
/// 标题，没设时是程序名；目录和名字一样时不再写。
pub(super) fn pane_label(view: &TerminalView) -> (SharedString, Option<SharedString>) {
    let meta = view.meta();
    let name = if let Some(agent) = meta.agent.filter(|agent| agent.kind.is_known()) {
        let dir_name = view.cwd().and_then(|dir| dir.file_name().map(|name| name.to_string_lossy().into_owned()));
        let title = meta.title.clone().filter(|title| dir_name.as_ref() != Some(title));
        title.or_else(|| Some(agent.kind.display_name().to_owned()))
    } else if meta.foreground_is_shell {
        meta.foreground.clone()
    } else {
        meta.title.clone().or_else(|| meta.foreground.clone())
    }
    .unwrap_or_else(|| view.title().to_owned());
    let dir = view.cwd().map(|dir| display_dir(&dir)).filter(|dir| *dir != name);
    (name.into(), dir.map(Into::into))
}

/// 标签图标叠里一块的边长、圆角，以及后面那几块往右错开多少。
const TILE_SIZE: f32 = 18.;
const TILE_RADIUS: f32 = 5.;
const TILE_OFFSET: f32 = 4.;
/// shell 和普通程序那块的底色和提示符的颜色，深浅主题下都一样，像个小终端；压在后面时用浅一些的
/// 灰，和前面那块分得开。
const PROMPT_TILE: u32 = 0x232326;
const PROMPT_TILE_BACK: u32 = 0x76767C;
const PROMPT_COLOR: u32 = 0xEDEDED;

/// 卡片样式下标签前面的图标叠：当前分屏的图标在最前，其余分屏（最多再两个）压在后面，往右错开
/// 露出一条边。有 logo 的 agent 是浅底上的 logo，压在后面时铺它的代表色；shell 和别的程序是深底
/// 上的提示符。
fn tab_icons(tab: &Tab, cx: &App) -> Div {
    let others = tab.root.leaves().into_iter().filter(|id| *id != tab.focused);
    let kinds: Vec<Option<AgentKind>> = std::iter::once(tab.focused)
        .chain(others)
        .take(3)
        .map(|id| {
            let agent = tab.panes.get(&id).and_then(|(view, _)| view.read(cx).agent());
            agent.map(|agent| agent.kind).filter(|kind| accent(*kind).is_some())
        })
        .collect();
    let logo_bg = if cx.theme().is_light { gpui::white() } else { cx.theme().mix(0.14) };
    let fg = cx.theme().foreground;
    let tile = |color: Hsla| {
        div()
            .absolute()
            .top_0()
            .size(px(TILE_SIZE))
            .rounded(px(TILE_RADIUS))
            .border_1()
            .border_color(fg.opacity(0.15))
            .bg(color)
            .flex()
            .items_center()
            .justify_center()
    };
    let back = kinds.iter().enumerate().skip(1).rev().map(|(k, kind)| {
        let color = kind.and_then(accent).unwrap_or(PROMPT_TILE_BACK);
        tile(gpui::rgb(color).into()).left(px(TILE_OFFSET * k as f32))
    });
    let front = match kinds.first().copied().flatten() {
        Some(kind) => tile(logo_bg).children(agent_logo(kind, px(12.), fg.opacity(0.85))),
        None => tile(gpui::rgb(PROMPT_TILE).into())
            .child(svg().path(PROMPT_ICON).size(px(11.)).text_color(gpui::rgb(PROMPT_COLOR))),
    };
    div()
        .relative()
        .flex_none()
        .w(px(TILE_SIZE + TILE_OFFSET * (kinds.len().max(1) - 1) as f32))
        .h(px(TILE_SIZE))
        .children(back)
        .child(front.left_0())
}

/// 快捷键提示，从键位表里查，快捷键改了也跟着变：先找 `select` 的绑定，`is_last` 时再找
/// `last` 的。后加的绑定优先，显示最后一个。
pub(super) fn shortcut_hint(select: &dyn Action, last: &dyn Action, is_last: bool, cx: &App) -> Option<SharedString> {
    shortcut_text(select, cx).or_else(|| if is_last { shortcut_text(last, cx) } else { None })
}

impl WindowView {
    /// Windows 上标题栏右上角的最小化、最大化（最大化着时是还原）和关闭按钮，盖在窗口最上层，全屏时不画。
    /// 鼠标按下由系统照命中测试的结果（`WindowControlArea`）办，这里只画样子；辅助工具的按下另外登记。
    pub(super) fn render_window_controls(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if !cfg!(windows) || window.is_fullscreen() {
            return None;
        }
        let fg = cx.theme().foreground;
        let hover = cx.theme().mix(0.08);
        let view = cx.entity().downgrade();
        let button = |id: &'static str, area: WindowControlArea, glyph: &'static str, label: Cow<'_, str>| {
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
                .font_family(WINDOW_CONTROL_FONT)
                .text_size(px(10.))
                .text_color(fg)
                .child(glyph)
        };
        let (max_glyph, max_label) = if window.is_maximized() {
            ("\u{E923}", rust_i18n::t!("window_controls.restore"))
        } else {
            ("\u{E922}", rust_i18n::t!("window_controls.maximize"))
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
                        "\u{E921}",
                        rust_i18n::t!("window_controls.minimize"),
                    )
                    .hover(|el| el.bg(hover))
                    .on_a11y_press(view.clone(), |_, window, _| window.minimize_window()),
                )
                .child(
                    button("window-maximize", WindowControlArea::Max, max_glyph, max_label)
                        .hover(|el| el.bg(hover))
                        .on_a11y_press(view.clone(), |_, window, _| window.zoom_window()),
                )
                .child(
                    button(
                        "window-close",
                        WindowControlArea::Close,
                        "\u{E8BB}",
                        rust_i18n::t!("window_controls.close"),
                    )
                    .hover(|el| el.bg(gpui::rgb(WINDOW_CLOSE_HOVER)).text_color(gpui::white()))
                    .on_a11y_press(view, |_, window, cx| {
                        window.dispatch_action(Box::new(crate::menus::CloseWindow), cx);
                    }),
                ),
        )
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
        let icons = cards.then(|| tab_icons(tab, cx));
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
        // 右侧槽位的内容：响铃标记优先，其次快捷键提示。
        let side_content = |shortcut: Option<SharedString>| {
            if tab.bell {
                bell_dot().into_any_element()
            } else {
                div().text_size(FontSize::Small).text_color(fg.opacity(0.35)).children(shortcut).into_any_element()
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
                    tab.relative().flex_none().w(width).h_full().rounded(px(TAB_TRACK_HEIGHT / 2. - 6.))
                } else {
                    tab.flex_none().w(width).h_full().when(ix > 0, |tab| tab.border_l_1().border_color(divider))
                }
            })
            .overflow_hidden()
            .flex()
            .items_center()
            .px(px(if compact { 4. } else { 8. }))
            .gap(px(4.))
            .map(|tab| {
                if active {
                    tab.bg(active_bg).text_color(fg).when(cards, |tab| tab.shadow(pill_shadow()))
                } else {
                    tab.text_color(fg.opacity(0.55)).hover(|tab| tab.bg(hover_bg).text_color(fg.opacity(0.8)))
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
            .map(|el| match (icons, label) {
                (Some(icons), Some((name, dir))) => {
                    // 卡片样式：图标叠、靠左的名字和目录（放不下时末尾渐隐）、状态标记，悬停时右边出关闭
                    // 按钮。两个不是当前标签的标签之间画一条短竖线。
                    let tab_bg = if active { active_bg } else { cx.theme().tab_track };
                    let separator = ix > 0 && !active && !after_front;
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
                        .gap(px(5.))
                        .child(div().flex_none().whitespace_nowrap().child(name))
                        .children(dir.map(|dir| {
                            div()
                                .flex_none()
                                .whitespace_nowrap()
                                .text_color(fg.opacity(if active { 0.45 } else { 0.35 }))
                                .child(dir)
                        }))
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
                    el.when(separator, |el| {
                        el.child(div().absolute().left_0().top(px(7.)).bottom(px(7.)).w(px(1.)).bg(cx.theme().border))
                    })
                    .child(icons)
                    .child(label)
                    .children(driven_tooltip.map(|driven| driven_icon(("tab-driven", id), fg).tooltip(driven)))
                    .children(mark.map(|mark| styled_agent_mark(mark, ("tab-agent", id), fg, true)))
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
                                close.w_0().overflow_hidden().group_hover(group, |close| close.w(px(TAB_CLOSE_SIZE))),
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
                                            .child(close.invisible().group_hover(group, |close| close.visible())),
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
                                    close.invisible().group_hover(group, |close| close.visible())
                                }
                            })
                            .tooltip(close_tooltip)
                            .on_press_down(cx, move |this, window, cx| this.close_tab_by_id(id, window, cx)),
                    )
                    .child(titled(title, mark, ("tab-agent", id), fg).flex_1())
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
        let separator = cards && pos > 0 && !active && !after_front;
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
                    el.relative().flex_none().w(width).h_full().rounded(px(TAB_TRACK_HEIGHT / 2. - 6.))
                } else {
                    el.flex_none().w(width).h_full().border_l_1().border_color(divider)
                }
            })
            .overflow_hidden()
            .flex()
            .items_center()
            .px(px(8.))
            .gap(px(5.))
            .map(|el| {
                if active {
                    el.bg(active_bg).when(cards, |el| el.shadow(pill_shadow()))
                } else {
                    el.hover(|el| el.bg(hover_bg))
                }
            })
            .when(separator, |el| {
                el.child(div().absolute().left_0().top(px(7.)).bottom(px(7.)).w(px(1.)).bg(cx.theme().border))
            })
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
                                .when(!active, |close| close.invisible().group_hover(group, |close| close.visible()))
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

    /// 卡片样式下标签条右边的新建标签按钮。
    pub(super) fn render_new_tab_button_card(&self, cx: &mut Context<Self>) -> Button {
        let tooltip = tooltip(rust_i18n::t!("menu.new_tab"), Some(&NewTab));
        Button::new("new-tab", rust_i18n::t!("menu.new_tab"))
            .icon(PLUS_ICON)
            .icon_size(16.)
            .with_size(ButtonSize::Icon)
            .ml(px(4.))
            .mr(px(6.))
            .w(px(NEW_TAB_BUTTON_WIDTH - 8.))
            .h(px(TAB_TRACK_HEIGHT - 4.))
            .tooltip(tooltip)
            .on_press_down(cx, |this, window, cx| this.new_tab(&NewTab, window, cx))
    }

    pub(super) fn render_new_tab_button(&self, cx: &mut Context<Self>) -> Button {
        let tooltip = tooltip(rust_i18n::t!("menu.new_tab"), Some(&NewTab));
        Button::new("new-tab", rust_i18n::t!("menu.new_tab"))
            .with_size(ButtonSize::IconSm)
            .w(px(NEW_TAB_BUTTON_WIDTH))
            .h_full()
            .rounded(px(0.))
            .border_l_1()
            .border_color(cx.theme().border)
            .text_size(px(16.))
            .child("+")
            .tooltip(tooltip)
            .on_press_down(cx, |this, window, cx| this.new_tab(&NewTab, window, cx))
    }
}
