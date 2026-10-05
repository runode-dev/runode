//! 窗口左侧的 workspace 列表：切换、拖动排序、改名、关闭和新建。

use gpui::{
    AnyElement, BoxShadow, Context, CursorStyle, Div, Focusable, Hsla, MouseButton, MouseDownEvent, Render,
    SharedString, Stateful, Window, div, point, prelude::*, px, svg,
};
use runode_model::color::Rgb;

use super::{
    AGENT_MARK_WIDTH, DIVIDER_GRAB_WIDTH, Divider, NewWorkspace, RenameWorkspace, Renaming, SelectLastWorkspace,
    SelectWorkspace, TAB_CLOSE_SIZE, TITLEBAR_HEIGHT, TRAFFIC_LIGHTS_WIDTH, ToggleSidebar, WindowView,
    model::{WorkspaceId, display_dir},
    titlebar::{agent_mark, shortcut_hint},
};
use crate::{
    assets::SIDEBAR_ICON,
    search_bar::{SearchField, SearchFieldEvent},
    terminal_view::hsla,
};

/// 侧栏的默认宽度，比红绿灯宽得多，红绿灯落在侧栏顶上。
const SIDEBAR_WIDTH: f32 = 200.;
/// 拖动侧栏宽度的范围；最窄也要放得下红绿灯。
const SIDEBAR_MIN_WIDTH: f32 = 140.;
const SIDEBAR_MAX_WIDTH: f32 = 480.;
/// 红绿灯右边收起、展开侧栏的按钮。
const SIDEBAR_TOGGLE_WIDTH: f32 = 24.;
const SIDEBAR_TOGGLE_HEIGHT: f32 = 20.;
/// 侧栏收着时标题栏左边让出的宽度：红绿灯和开关按钮，再空一点才到标签，图标离两边差不多远。
pub(super) const SIDEBAR_TOGGLE_INSET: f32 = TRAFFIC_LIGHTS_WIDTH + SIDEBAR_TOGGLE_WIDTH + 6.;
/// 每个 workspace 一行：名字和目录各占一行。
const ROW_HEIGHT: f32 = 40.;
/// 改名输入框的高度。
const RENAME_FIELD_HEIGHT: f32 = 18.;

/// 拖动中的 workspace：拖动时跟着鼠标画出来，放到另一行上时按 `id` 挪位置。
#[derive(Clone)]
struct DraggedWorkspace {
    id: WorkspaceId,
    /// 开始拖动时所在的位置，用来决定落点提示画在目标行的上边还是下边。
    ix: usize,
    name: SharedString,
    /// 侧栏当前的宽度，预览照这个宽度画。
    width: f32,
    fg: Hsla,
    bg: Hsla,
}

impl Render for DraggedWorkspace {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(self.width - 12.))
            .h(px(ROW_HEIGHT))
            .px(px(8.))
            .flex()
            .items_center()
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
            .child(div().min_w_0().truncate().child(self.name.clone()))
    }
}

impl WindowView {
    /// 用户手动收起或展开过就按那个来，否则多于一个 workspace 时显示；改名时总要显示。
    pub(super) fn sidebar_visible(&self) -> bool {
        self.renaming.is_some() || self.sidebar_shown.unwrap_or(self.workspaces.len() > 1)
    }

    pub(super) fn sidebar_width(&self) -> f32 {
        self.sidebar_width.unwrap_or(SIDEBAR_WIDTH)
    }

    /// 拖动分隔线时侧栏的右边跟到窗口里的横坐标 `x`。
    pub(super) fn resize_sidebar(&mut self, x: f32) {
        self.sidebar_width = Some(x.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH));
    }

    /// 侧栏右边的分隔线只有一像素宽，在它两侧放一条透明的把手供拖动。盖在窗口的最上层，
    /// 伸进终端区的一半才能先于终端接到鼠标。
    pub(super) fn render_sidebar_handle(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        div()
            .id("sidebar-divider")
            .absolute()
            .top_0()
            .h_full()
            .left(px(self.sidebar_width() - DIVIDER_GRAB_WIDTH / 2.))
            .w(px(DIVIDER_GRAB_WIDTH))
            .cursor(CursorStyle::ResizeLeftRight)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        // 双击恢复默认宽度。
                        this.sidebar_width = None;
                        this.save(cx);
                    } else {
                        this.dragging_divider = Some(Divider::Sidebar);
                    }
                    cx.notify();
                }),
            )
    }

    /// 红绿灯右边收起、展开侧栏的按钮，侧栏收着时也在原处，不随侧栏跳动。
    pub(super) fn render_sidebar_toggle(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let hover_bg = hsla(bg.mix(fg, 0.10));
        let fg = hsla(fg);
        let group = "sidebar-toggle";
        div()
            .id(group)
            .group(group)
            .absolute()
            .left(px(TRAFFIC_LIGHTS_WIDTH))
            .top(px((TITLEBAR_HEIGHT - SIDEBAR_TOGGLE_HEIGHT) / 2.))
            .w(px(SIDEBAR_TOGGLE_WIDTH))
            .h(px(SIDEBAR_TOGGLE_HEIGHT))
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .hover(|button| button.bg(hover_bg))
            .child(
                svg()
                    .path(SIDEBAR_ICON)
                    .size(px(16.))
                    .text_color(fg.opacity(0.55))
                    .group_hover(group, |icon| icon.text_color(fg)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.toggle_sidebar(&ToggleSidebar, window, cx);
                }),
            )
    }

    pub(super) fn render_sidebar(&self, fg: Rgb, bg: Rgb, fullscreen: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let rows: Vec<_> = (0..self.workspaces.len()).map(|ix| self.render_row(ix, fg, bg, cx)).collect();
        div()
            .id("sidebar")
            .flex_none()
            .w(px(self.sidebar_width()))
            .h_full()
            .flex()
            .flex_col()
            .bg(hsla(bg.mix(fg, 0.03)))
            .border_r_1()
            .border_color(hsla(fg).opacity(0.12))
            .text_size(px(12.))
            // 顶上这条放红绿灯，和标题栏一样能拖动窗口、双击缩放，比标题栏多留一点，第一行
            // 不贴着红绿灯；全屏时没有红绿灯。
            .child(
                div()
                    .flex_none()
                    .h(px(if fullscreen { 6. } else { TITLEBAR_HEIGHT + 6. }))
                    .on_mouse_down(MouseButton::Left, |event, window, _| {
                        if event.click_count >= 2 {
                            window.titlebar_double_click();
                        } else {
                            window.start_window_move();
                        }
                    }),
            )
            .child(
                div()
                    .id("workspace-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .px(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(rows),
            )
            .child(self.render_new_workspace_button(fg, bg, cx))
    }

    fn render_row(&self, ix: usize, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let workspace = &self.workspaces[ix];
        let id = workspace.id;
        let active = ix == self.active;
        let active_bg = hsla(bg.mix(fg, 0.10));
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let field_bg = hsla(bg);
        let fg = hsla(fg);
        let group = SharedString::from(format!("workspace-{ix}"));
        let renaming = self.renaming.as_ref().filter(|renaming| renaming.id == id).map(|r| r.field.clone());
        let mark = match workspace.agent(cx) {
            Some(agent) => agent_mark(agent, ("workspace-agent", ix), fg),
            None => div().flex_none().w(px(AGENT_MARK_WIDTH)).into_any_element(),
        };
        let name: AnyElement = match renaming.clone() {
            Some(field) => div()
                .h(px(RENAME_FIELD_HEIGHT))
                .px(px(3.))
                .rounded(px(3.))
                .bg(field_bg)
                .border_1()
                .border_color(fg.opacity(0.3))
                .text_color(fg)
                .child(field)
                .into_any_element(),
            None => div().truncate().child(workspace.name.clone()).into_any_element(),
        };
        // 右侧：响铃标记优先，其次快捷键提示；悬停时换成关闭按钮。
        let hint = if workspace.bell() {
            div().size(px(6.)).rounded_full().bg(fg.opacity(0.8)).into_any_element()
        } else {
            div()
                .text_size(px(11.))
                .text_color(fg.opacity(0.35))
                .children(shortcut_hint(
                    &SelectWorkspace(ix),
                    &SelectLastWorkspace,
                    ix + 1 == self.workspaces.len(),
                    cx,
                ))
                .into_any_element()
        };
        // 目录和名字一样（比如家目录的 `~`）时不再写一遍。
        let dir = Some(display_dir(&workspace.dir)).filter(|dir| *dir != *workspace.name);
        let dragged =
            DraggedWorkspace { id, ix, name: workspace.name.clone(), width: self.sidebar_width(), fg, bg: active_bg };
        div()
            .id(("workspace", ix))
            .group(group.clone())
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(8.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .map(|row| {
                if active {
                    row.bg(active_bg).text_color(fg)
                } else {
                    row.text_color(fg.opacity(0.7)).hover(|row| row.bg(hover_bg).text_color(fg))
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // 点在改名输入框里时交给输入框挪光标、选字。
                    if this.renaming.as_ref().is_some_and(|renaming| renaming.id == id) {
                        return;
                    }
                    cx.stop_propagation();
                    if event.click_count >= 2 {
                        this.start_rename(ix, window, cx);
                    } else {
                        this.activate_workspace(ix, window, cx);
                    }
                }),
            )
            // 改名时在输入框里拖选文字，不能把整行拖走。
            .when(renaming.is_none(), |row| {
                row.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            })
            // 落点提示画在目标行靠近原位置的另一侧：往下拖插到它下面，往上拖插到它上面。
            .drag_over::<DraggedWorkspace>(move |style, dragged, _, _| {
                let marker = fg.opacity(0.6);
                if dragged.ix < ix {
                    style.border_b_2().border_color(marker)
                } else if dragged.ix > ix {
                    style.border_t_2().border_color(marker)
                } else {
                    style
                }
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedWorkspace, window, cx| {
                this.move_workspace(dragged.id, ix, window, cx);
            }))
            .child(mark)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(name)
                    // 路径长时留下结尾：最后几级目录最能区分。
                    .children(dir.map(|dir| {
                        div()
                            .text_size(px(11.))
                            .text_color(fg.opacity(0.45))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_start()
                            .child(dir)
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .relative()
                    .h_full()
                    .min_w(px(TAB_CLOSE_SIZE))
                    .flex()
                    .items_center()
                    .justify_end()
                    .child(div().group_hover(group.clone(), |hint| hint.invisible()).child(hint))
                    .child(
                        div()
                            .id(("workspace-close", ix))
                            .absolute()
                            .right_0()
                            .top(px((ROW_HEIGHT - TAB_CLOSE_SIZE) / 2.))
                            .size(px(TAB_CLOSE_SIZE))
                            .rounded(px(3.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .invisible()
                            .group_hover(group, |close| close.visible())
                            .text_size(px(14.))
                            .text_color(fg.opacity(0.75))
                            .hover(|close| close.bg(fg.opacity(0.18)).text_color(fg))
                            .child("×")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    if let Some(ix) = this.workspaces.iter().position(|w| w.id == id) {
                                        this.confirm_close_workspace(ix, window, cx);
                                    }
                                }),
                            ),
                    ),
            )
    }

    fn render_new_workspace_button(&self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) -> Stateful<Div> {
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let fg = hsla(fg);
        div()
            .id("new-workspace")
            .flex_none()
            .h(px(32.))
            .m(px(6.))
            .px(px(8.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_color(fg.opacity(0.55))
            .hover(|button| button.bg(hover_bg).text_color(fg))
            .child(
                div()
                    .flex_none()
                    .w(px(AGENT_MARK_WIDTH))
                    .flex()
                    .justify_center()
                    .text_size(px(14.))
                    .child("+"),
            )
            .child(div().min_w_0().truncate().child(rust_i18n::t!("workspace.new").into_owned()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.new_workspace(&NewWorkspace, window, cx);
                }),
            )
    }

    pub(super) fn rename_workspace(&mut self, _: &RenameWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        self.start_rename(self.active, window, cx);
    }

    /// 在侧栏里把第 `ix` 个 workspace 的名字换成输入框，原名全选，直接打字就替换掉。
    fn start_rename(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // 正在改别的 workspace 时先把那个改完。
        self.finish_rename(true, window, cx);
        let workspace = &self.workspaces[ix];
        let id = workspace.id;
        let name = workspace.name.to_string();
        let field = cx.new(|cx| {
            let mut field = SearchField::new(name, cx);
            field.select_all_text(cx);
            field
        });
        // 输入框原本是搜索框：回车是「下一个」，Esc 是「关闭搜索」，在这里分别是确定和取消。
        let events = cx.subscribe_in(&field, window, |this, _, event: &SearchFieldEvent, window, cx| match event {
            SearchFieldEvent::Next => this.finish_rename(true, window, cx),
            SearchFieldEvent::Dismiss => this.finish_rename(false, window, cx),
            SearchFieldEvent::Changed(_) | SearchFieldEvent::Previous => {}
        });
        let focus = field.focus_handle(cx);
        // 点到别处算确定；切到别的应用时窗口失去焦点，回来接着改。
        let blur = cx.on_blur(&focus, window, |this, window, cx| {
            if window.is_window_active() {
                this.finish_rename(true, window, cx);
            }
        });
        window.focus(&focus, cx);
        self.renaming = Some(Renaming { id, field, _subscriptions: [events, blur] });
        self.sidebar_scroll.scroll_to_item(ix);
        cx.notify();
    }

    /// 结束改名；`commit` 时用输入框里的名字，空的不算。
    fn finish_rename(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        let name = renaming.field.read(cx).query().trim().to_owned();
        if commit
            && !name.is_empty()
            && let Some(workspace) = self.workspaces.iter_mut().find(|w| w.id == renaming.id)
        {
            workspace.name = name.into();
            self.save(cx);
        }
        // 按回车或 Esc 结束时焦点还在输入框里，交回终端；点别处结束时焦点已经去了别处。
        if renaming.field.focus_handle(cx).is_focused(window) {
            window.focus(&self.tab().focused_view().focus_handle(cx), cx);
        }
        cx.notify();
    }
}
