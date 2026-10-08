//! 窗口左侧的 workspace 列表：切换、拖动排序、改名、关闭和新建；顶上是手机端入口（`mobile`），下面是
//! 后台会话（`background`）。每行名字前是 GitHub 头像或 git 图标，下面是当前分支（`repo`）。

mod repo;

use gpui::{
    Action, AnyElement, App, Axis, Context, CursorStyle, Div, ExternalPaths, Focusable, Hsla, Modifiers, MouseButton,
    MouseDownEvent, Render, SharedString, Stateful, TextAlign, Window, canvas, div, img, prelude::*, px, relative, svg,
};
use runode_shared_types::color::Rgb;

use super::{
    CARD_GAP, DIVIDER_GRAB_WIDTH, Divider, NewWorkspace, RenameWorkspace, Renaming, SelectLastWorkspace,
    SelectWorkspace, TAB_CLOSE_SIZE, TITLEBAR_HEIGHT, TRAFFIC_LIGHTS_WIDTH, ToggleSidebar, WindowView, background,
    cards, divider_color, drag_window,
    inline_edit::InlineEdit,
    model::{WorkspaceId, display_dir},
    titlebar::{close_button, drag_chip, drop_marker, icon_toggle, shortcut_hint, styled_agent_mark},
};
use crate::{
    assets::{BRANCH_ICON, GIT_ICON, SIDEBAR_ICON},
    ui::{hsla, tooltip::tooltip},
};
pub(super) use repo::RepoBadge;

/// 侧栏的默认宽度，比红绿灯宽得多，红绿灯落在侧栏顶上。
const SIDEBAR_WIDTH: f32 = 200.;
/// 拖动侧栏宽度的范围；最窄也要放得下红绿灯。
const SIDEBAR_MIN_WIDTH: f32 = 140.;
const SIDEBAR_MAX_WIDTH: f32 = 480.;
/// 红绿灯右边收起、展开侧栏的按钮。
const SIDEBAR_TOGGLE_WIDTH: f32 = 28.;
const SIDEBAR_TOGGLE_HEIGHT: f32 = 24.;
/// 侧栏收着时标题栏左边让出的宽度：红绿灯和开关按钮，再空一点才到标签，图标离两边差不多远。
pub(super) const SIDEBAR_TOGGLE_INSET: f32 = TRAFFIC_LIGHTS_WIDTH + SIDEBAR_TOGGLE_WIDTH + 8.;
/// 每个 workspace 一行：名字和目录各占一行。
const ROW_HEIGHT: f32 = 40.;
/// 侧栏每行名字前的图标那一格：workspace 的 GitHub 头像或 git 图标，手机端入口和新建按钮的图标也占这么宽，
/// 几处的文字才对齐。
pub(super) const ROW_ICON_SIZE: f32 = 16.;
/// 改名输入框的高度。
const RENAME_FIELD_HEIGHT: f32 = 18.;

/// 快捷键要按着的修饰键，查法和 `shortcut_hint` 一样：先找 `select` 的绑定，`is_last` 时再找 `last` 的，
/// 都取最后一个绑定的第一键。
fn shortcut_modifiers(select: &dyn Action, last: &dyn Action, is_last: bool, cx: &App) -> Option<Modifiers> {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let modifiers = |action: &dyn Action| {
        keymap.bindings_for_action(action).next_back()?.keystrokes().first().map(|k| *k.modifiers())
    };
    modifiers(select).or_else(|| if is_last { modifiers(last) } else { None })
}

/// 目录那一行的行高。
const PATH_LINE_HEIGHT: f32 = 16.;

/// 一行路径，按排好版后的宽度用 `fit_path` 截短再画；字体、字号、颜色跟着外面的文字样式。GPUI 只会按字
/// 截断，量文字要等知道宽度，所以画在 canvas 上。
fn path_line(path: String) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            let style = window.text_style();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let shape = |text: &str| {
                let text = SharedString::from(text.to_owned());
                let run = style.to_run(text.len());
                window.text_system().shape_line(text, font_size, &[run], None)
            };
            shape(&fit_path(&path, |text| shape(text).width <= bounds.size.width))
        },
        |bounds, line, window, cx| {
            line.paint(bounds.origin, bounds.size.height, TextAlign::Left, None, window, cx).ok();
        },
    )
    .w_full()
    .h(px(PATH_LINE_HEIGHT))
}

/// 路径放不下时整级整级地从开头删，前面写 `…`：`…/barey.cn/runode`；最后一级也放不下时再从它的开头按字删。
fn fit_path(path: &str, fits: impl Fn(&str) -> bool) -> String {
    if fits(path) {
        return path.to_owned();
    }
    let mut tail = path;
    for (i, _) in path.match_indices('/').filter(|&(i, _)| i > 0) {
        tail = &path[i..];
        let candidate = format!("…{tail}");
        if fits(&candidate) {
            return candidate;
        }
    }
    tail.char_indices()
        .skip(1)
        .map(|(i, _)| format!("…{}", &tail[i..]))
        .find(|candidate| fits(candidate))
        .unwrap_or_else(|| "…".to_owned())
}

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
        drag_chip(px(self.width - 12.), px(ROW_HEIGHT), self.name.clone(), self.fg, self.bg)
    }
}

impl WindowView {
    /// 用户手动收起或展开过就按那个来，否则多于一个 workspace 或者有后台会话时显示；改名时总要
    /// 显示。
    pub(super) fn sidebar_visible(&self) -> bool {
        self.renaming.is_some() || self.sidebar_shown.unwrap_or(self.workspaces.len() > 1 || background::any())
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
        // 卡片样式下分隔线落在侧栏和卡片之间的空隙中间。
        let spacing = if cards(cx) { CARD_GAP / 2. } else { 0. };
        div()
            .id("sidebar-divider")
            .absolute()
            .top_0()
            .h_full()
            .left(px(self.sidebar_width() + spacing - DIVIDER_GRAB_WIDTH / 2.))
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
        let text = if self.sidebar_visible() {
            rust_i18n::t!("tooltip.hide_sidebar")
        } else {
            rust_i18n::t!("tooltip.show_sidebar")
        };
        let tooltip = tooltip(text, Some(&ToggleSidebar), fg, bg);
        icon_toggle("sidebar-toggle", SIDEBAR_ICON, 16., false, fg, bg)
            .absolute()
            .left(px(TRAFFIC_LIGHTS_WIDTH))
            .top(px((TITLEBAR_HEIGHT - SIDEBAR_TOGGLE_HEIGHT) / 2.))
            .w(px(SIDEBAR_TOGGLE_WIDTH))
            .h(px(SIDEBAR_TOGGLE_HEIGHT))
            .tooltip(tooltip)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.toggle_sidebar(&ToggleSidebar, window, cx);
                }),
            )
    }

    pub(super) fn render_sidebar(&self, fg: Rgb, bg: Rgb, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let fullscreen = window.is_fullscreen();
        let modifiers = window.modifiers();
        let rows: Vec<_> = (0..self.workspaces.len()).map(|ix| self.render_row(ix, fg, bg, modifiers, cx)).collect();
        let drop_bg = hsla(bg.mix(fg, 0.08));
        div()
            .id("sidebar")
            .flex_none()
            .w(px(self.sidebar_width()))
            .h_full()
            .flex()
            .flex_col()
            // 卡片样式下侧栏直接画在外框上。
            .when(!cards(cx), |sidebar| {
                sidebar.bg(hsla(bg.mix(fg, 0.03))).border_r_1().border_color(divider_color(hsla(fg)))
            })
            // 从访达拖来的东西悬在侧栏上时提示放下能开 workspace；放下由窗口根上的 `open_dropped_dirs` 办。
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.bg(drop_bg))
            .text_size(px(12.))
            // 顶上这条放红绿灯，和标题栏一样能拖动窗口、双击缩放，比标题栏多留一点，第一行
            // 不贴着红绿灯；全屏时没有红绿灯。
            .child(
                div()
                    .flex_none()
                    .h(px(if fullscreen { 6. } else { TITLEBAR_HEIGHT + 6. }))
                    .on_mouse_down(MouseButton::Left, drag_window),
            )
            .child(self.render_mobile_entry(fg, bg, cx))
            // 手机端入口不是 workspace，和下面的列表用一条线隔开。
            .child(div().flex_none().mx(px(14.)).mb(px(6.)).h(px(1.)).bg(divider_color(hsla(fg))))
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
            .children(self.render_background(fg, bg, cx))
            .child(self.render_new_workspace_button(fg, bg, cx))
    }

    fn render_row(&self, ix: usize, fg: Rgb, bg: Rgb, modifiers: Modifiers, cx: &mut Context<Self>) -> Stateful<Div> {
        let workspace = &self.workspaces[ix];
        let id = workspace.id;
        // 开着手机端引导页时高亮的是入口。
        let active = ix == self.active && self.mobile.is_none();
        let active_bg = hsla(bg.mix(fg, 0.10));
        let hover_bg = hsla(bg.mix(fg, 0.06));
        let close_tooltip = tooltip(rust_i18n::t!("menu.close_workspace"), None, fg, bg);
        let rgb_fg = fg;
        let fg = hsla(fg);
        let group = SharedString::from(format!("workspace-{ix}"));
        let renaming = self.renaming.as_ref().filter(|renaming| renaming.id == id);
        // 名字前是 GitHub 头像，不在 GitHub 上的仓库画 git 图标；agent 的标记跟在名字后面。
        let repo = &workspace.repo;
        // 靠上对着名字那一行，不在两行中间。
        let icon =
            div().flex_none().w(px(ROW_ICON_SIZE)).mt(px(3.)).map(|slot| match (&repo.avatar, repo.branch.is_some()) {
                (Some(avatar), _) => slot.child(img(avatar.clone()).size(px(ROW_ICON_SIZE)).rounded(px(3.))),
                (None, true) => slot.child(svg().path(GIT_ICON).size(px(ROW_ICON_SIZE)).text_color(fg.opacity(0.6))),
                (None, false) => slot,
            });
        let mark = workspace.mark(cx).map(|mark| styled_agent_mark(mark, ("workspace-agent", ix), fg, cards(cx)));
        let name: AnyElement = match renaming {
            Some(renaming) => renaming.edit.render(px(RENAME_FIELD_HEIGHT), rgb_fg, bg).into_any_element(),
            None => div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(div().min_w_0().truncate().child(workspace.name.clone()))
                .children(mark)
                .into_any_element(),
        };
        // 右侧：响铃标记优先，其次快捷键提示，提示只在按着它的修饰键或者单按 ⌘ 时显示，平时不和名字抢眼；
        // 悬停时换成关闭按钮。按 ⌘ 是找快捷键时最先按的键，也让它看得到。
        let is_last = ix + 1 == self.workspaces.len();
        let hint = if workspace.bell() {
            div().size(px(6.)).rounded_full().bg(fg.opacity(0.8)).into_any_element()
        } else {
            let held = modifiers == Modifiers::command()
                || shortcut_modifiers(&SelectWorkspace(ix), &SelectLastWorkspace, is_last, cx) == Some(modifiers);
            div()
                .text_size(px(11.))
                .text_color(fg.opacity(0.5))
                // 不按时只是看不见、位置照留，按下时名字一栏不变窄，截断的地方不跳。
                .when(!held, |hint| hint.invisible())
                .children(shortcut_hint(&SelectWorkspace(ix), &SelectLastWorkspace, is_last, cx))
                .into_any_element()
        };
        // 目录和名字一样（比如家目录的 `~`）时不再写一遍。
        let dir = Some(display_dir(&workspace.dir)).filter(|dir| *dir != *workspace.name);
        // 第二行先写分支再写目录，分支名长时最多占一半多，目录还留着结尾。
        let branch = repo.branch.clone().map(|branch| {
            div()
                .flex_none()
                .max_w(relative(0.6))
                .flex()
                .items_center()
                .gap(px(3.))
                .text_color(fg.opacity(0.6))
                .child(svg().flex_none().path(BRANCH_ICON).size(px(10.)).text_color(fg.opacity(0.6)))
                .child(div().min_w_0().truncate().child(branch))
        });
        let details = (branch.is_some() || dir.is_some()).then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(11.))
                .text_color(fg.opacity(0.45))
                .children(branch)
                // 路径长时留下结尾：最后几级目录最能区分。
                .children(dir.map(|dir| div().flex_1().min_w_0().child(path_line(dir))))
        });
        let dragged =
            DraggedWorkspace { id, ix, name: workspace.name.clone(), width: self.sidebar_width(), fg, bg: active_bg };
        div()
            // 改名时换一个 id：双击那次按下被 GPUI 记作待拖动，改名期间不挂 `on_drag`，松开时也就没人清它，
            // 换了 id 旧的元素状态连同这次按下一起丢掉，改完名不会一动鼠标就拖起整行。
            .id(if renaming.is_some() { ("workspace-renaming", ix) } else { ("workspace", ix) })
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
            .when(renaming.is_none(), |row| row.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone())))
            .drag_over::<DraggedWorkspace>(move |style, dragged, _, _| {
                drop_marker(style, dragged.ix, ix, Axis::Vertical, fg)
            })
            .on_drop(cx.listener(move |this, dragged: &DraggedWorkspace, window, cx| {
                this.move_workspace(dragged.id, ix, window, cx);
            }))
            // 名字一行和目录一行同高，图标按固定的上边距靠上排，不随字体的行高跳。
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_start()
                    .gap(px(6.))
                    .line_height(px(PATH_LINE_HEIGHT))
                    .child(icon)
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(1.)).child(name).children(details)),
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
                        close_button(("workspace-close", ix), fg)
                            .absolute()
                            .right_0()
                            .top(px((ROW_HEIGHT - TAB_CLOSE_SIZE) / 2.))
                            .invisible()
                            .group_hover(group, |close| close.visible())
                            .tooltip(close_tooltip)
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
        let tooltip = tooltip(rust_i18n::t!("workspace.new"), Some(&NewWorkspace), fg, bg);
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
            .child(div().flex_none().w(px(ROW_ICON_SIZE)).flex().justify_center().text_size(px(14.)).child("+"))
            .child(div().min_w_0().truncate().child(rust_i18n::t!("workspace.new").into_owned()))
            .tooltip(tooltip)
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
        let select = name.len();
        let edit = InlineEdit::new(name, select, Self::finish_rename, window, cx);
        self.renaming = Some(Renaming { id, edit });
        self.sidebar_scroll.scroll_to_item(ix);
        cx.notify();
    }

    /// 结束改名；`commit` 时用输入框里的名字，空的不算。
    fn finish_rename(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        let name = renaming.edit.text(cx);
        if commit
            && !name.is_empty()
            && let Some(ix) = self.workspaces.iter().position(|w| w.id == renaming.id)
        {
            self.set_workspace_name(ix, name.into(), window, cx);
        }
        renaming.edit.release_focus(&self.focus_handle(cx), window, cx);
        cx.notify();
    }

    /// 把第 `ix` 个 workspace 改名为 `name`；没有终端时窗口标题用的是它的名字，跟着改。
    pub(super) fn set_workspace_name(
        &mut self,
        ix: usize,
        name: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.workspaces[ix].name = name;
        if ix == self.active && self.tab().is_none() {
            window.set_window_title(&self.workspace().name);
        }
        self.save(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::fit_path;

    #[test]
    fn fit_path_drops_whole_directories_first() {
        let fit = |path, width| fit_path(path, |text| text.chars().count() <= width);
        let path = "/Volumes/dev/barey.cn/runode";
        assert_eq!(fit(path, 40), path);
        // 放得下几级就留几级，不截在一级目录的中间。
        assert_eq!(fit(path, 26), "…/dev/barey.cn/runode");
        assert_eq!(fit(path, 20), "…/barey.cn/runode");
        assert_eq!(fit(path, 10), "…/runode");
        assert_eq!(fit("~/src/app", 7), "…/app");
        // 最后一级也放不下时才按字截。
        assert_eq!(fit(path, 5), "…node");
        assert_eq!(fit(path, 0), "…");
    }
}
