//! 侧栏最上面的一行：找 agent 的搜索框和新建任务的按钮；最下面的导航：任务（看板）、手机（配对设备页）
//! 和设置，各行右边是它的近况。

use std::rc::Rc;

use gpui::{AnyElement, Context, Div, MouseButton, Role, Stateful, Window, div, prelude::*, px, svg};

use super::{ROW_ICON_SIZE, ROW_TEXT_OPACITY, SIDEBAR_INSET};
use crate::{
    kanban::Tally,
    menus::OpenSettings,
    ui::{
        a11y::{A11yPress as _, PressDown as _},
        button::{Button, ButtonSize, ButtonVariant},
        font_size::{FontSize, scaled},
        icons::{KANBAN_ICON, PHONE_ICON, PLUS_ICON, SEARCH_ICON, SLIDERS_ICON},
        status_mark::{MarkKind, status_mark},
        theme::ActiveTheme as _,
        tooltip::{shortcut_text, tooltip},
    },
    window::{GotoAgent, NewTodo, ToggleKanban, WindowView},
};

/// 导航一行的高度。
const NAV_ROW_HEIGHT: f32 = 30.;
/// 搜索框的高度，新建按钮是同样边长的方块。
const SEARCH_HEIGHT: f32 = 32.;

/// 任务一行右边写的近况。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TasksSummary {
    /// 有任务需要你，只写它，盖过待办和在跑的数。
    NeedsYou(usize),
    Both {
        todo: usize,
        running: usize,
    },
    Todo(usize),
    Running(usize),
}

impl TasksSummary {
    /// 没有需要你、待办和在跑的任务时为空，右边什么都不写。
    fn of(tally: Tally) -> Option<Self> {
        match tally {
            Tally { needs_you: 1.., .. } => Some(Self::NeedsYou(tally.needs_you)),
            Tally { todo: 0, running: 0, .. } => None,
            Tally { todo, running: 0, .. } => Some(Self::Todo(todo)),
            Tally { todo: 0, running, .. } => Some(Self::Running(running)),
            Tally { todo, running, .. } => Some(Self::Both { todo, running }),
        }
    }

    fn text(self) -> String {
        match self {
            Self::NeedsYou(count) => rust_i18n::t!("sidebar.tasks_needs_you", count = count),
            Self::Both { todo, running } => rust_i18n::t!("sidebar.tasks_both", todo = todo, running = running),
            Self::Todo(todo) => rust_i18n::t!("sidebar.tasks_todo", todo = todo),
            Self::Running(running) => rust_i18n::t!("sidebar.tasks_running", running = running),
        }
        .into_owned()
    }
}

impl WindowView {
    /// 侧栏最上面的一行：按下搜索框打开列出所有 agent 的浮层（`GotoAgent`），右边的按钮新建任务。
    pub(super) fn render_sidebar_search(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (field, border, fg, muted) = (theme.field, theme.border, theme.foreground, theme.muted_foreground);
        let label = rust_i18n::t!("sidebar.search").into_owned();
        let new_label = rust_i18n::t!("kanban.new_todo").into_owned();
        let search = div()
            .id("sidebar-search")
            .role(Role::Button)
            .aria_label(label.clone())
            .flex_1()
            .min_w_0()
            .h(scaled(SEARCH_HEIGHT))
            .px(px(10.))
            .rounded(px(8.))
            .bg(field)
            .border_1()
            .border_color(border)
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(FontSize::Medium)
            .text_color(muted)
            .hover(|search| search.text_color(fg))
            .child(svg().path(SEARCH_ICON).flex_none().size(px(13.)).text_color(muted))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .children(
                shortcut_text(&GotoAgent, cx).map(|text| div().flex_none().text_size(FontSize::Small).child(text)),
            )
            .on_press_down(cx, |this, window, cx| this.goto_agent(&GotoAgent, window, cx));
        let new_todo = Button::new("sidebar-new-todo", new_label.clone())
            .variant(ButtonVariant::Default)
            .with_size(ButtonSize::Icon)
            .icon(PLUS_ICON)
            .icon_size(14.)
            .size(scaled(SEARCH_HEIGHT))
            .rounded(px(8.))
            .tooltip(tooltip(new_label, Some(&NewTodo)))
            .on_press_down(cx, |this, window, cx| this.new_todo(&NewTodo, window, cx));
        div()
            .flex_none()
            .pt(px(6.))
            .px(px(SIDEBAR_INSET))
            .flex()
            .items_center()
            .gap(px(6.))
            .child(search)
            .child(new_todo)
    }

    /// 侧栏最下面的导航：任务、手机和设置，开着哪一页哪一行高亮。
    pub(super) fn render_sidebar_nav(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let tasks = self.render_nav_tasks(cx);
        let phone = self.render_nav_phone(cx);
        let settings_label = rust_i18n::t!("workspace.settings").into_owned();
        let settings_shortcut = shortcut_text(&OpenSettings, cx);
        let settings = page_entry(
            "nav-settings",
            SLIDERS_ICON,
            settings_label,
            self.settings_page().is_some(),
            settings_shortcut.map(|text| div().child(text).into_any_element()),
            cx,
            |this, window, cx| this.open_settings(window, cx),
        );
        div()
            .id("sidebar-nav")
            .role(Role::Group)
            .aria_label(rust_i18n::t!("sidebar.nav").into_owned())
            .flex_none()
            .border_t_1()
            .border_color(cx.theme().border)
            .pt(px(8.))
            .px(px(6.))
            .pb(px(6.))
            .flex()
            .flex_col()
            .gap(px(1.))
            .child(tasks)
            .child(phone)
            .child(settings)
    }

    /// 任务一行：开着看板（任务页、定时页）时高亮，右边是需要你的数，没有时是待办和在跑的数。
    fn render_nav_tasks(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let label = rust_i18n::t!("sidebar.tasks").into_owned();
        let summary = TasksSummary::of(crate::kanban::tally(cx));
        let trailing = summary.map(|summary| match summary {
            TasksSummary::NeedsYou(_) => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_color(cx.theme().warning_text)
                .child(status_mark("nav-tasks-waiting", MarkKind::Waiting { blink: false }, 11., cx))
                .child(div().min_w_0().truncate().child(summary.text()))
                .into_any_element(),
            _ => div().min_w_0().truncate().child(summary.text()).into_any_element(),
        });
        page_entry(
            "nav-tasks",
            KANBAN_ICON,
            label.clone(),
            self.kanban_page().is_some(),
            trailing,
            cx,
            |this, window, cx| {
                this.close_todo_page(window, cx);
                this.show_kanban(false, window, cx);
            },
        )
        .when_some(summary, |row, summary| row.aria_description(summary.text()))
        .tooltip(tooltip(label, Some(&ToggleKanban)))
    }

    /// 手机一行：开着配对设备页时高亮，有设备连着时右边写第一台连着的。远程访问关着时设备表不再读，
    /// 留着的「连着」不算数。
    fn render_nav_phone(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let label = rust_i18n::t!("sidebar.phone").into_owned();
        let listening = cx.global::<crate::config::AppConfig>().0.remote_access;
        let live = crate::mobile_page::devices::devices(cx)
            .iter()
            .find(|device| listening && device.live)
            .map(|device| rust_i18n::t!("sidebar.phone_live", name = device.name.as_ref()).into_owned());
        let trailing = live.clone().map(|text| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(div().flex_none().size(px(6.)).rounded_full().bg(cx.theme().success))
                .child(div().min_w_0().truncate().child(text))
                .into_any_element()
        });
        page_entry("nav-phone", PHONE_ICON, label, self.mobile_page().is_some(), trailing, cx, |this, window, cx| {
            this.show_mobile(window, cx);
        })
        .when_some(live, |row, live| row.aria_description(live))
    }
}

/// 导航里打开一页的一行，那一页开着时高亮，`trailing` 是靠右的近况。按下鼠标就开，辅助工具的按下另外登记。
/// 开着的状态报成按下着（`aria_toggled`）而不是选中：AccessKit 不给带选中状态的按钮 Invoke，Windows 上
/// 辅助工具就按不动它。近况由调用方写进 `aria_description`。
fn page_entry(
    id: &'static str,
    icon: &'static str,
    label: String,
    active: bool,
    trailing: Option<AnyElement>,
    cx: &mut Context<WindowView>,
    open: impl Fn(&mut WindowView, &mut Window, &mut Context<WindowView>) + 'static,
) -> Stateful<Div> {
    let theme = cx.theme();
    let (active_bg, hover_bg, fg, muted) =
        (theme.accent_selected, theme.accent, theme.foreground, theme.muted_foreground);
    let open = Rc::new(open);
    let press = open.clone();
    let view = cx.entity().downgrade();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .aria_toggled(active.into())
        .flex_none()
        .h(scaled(NAV_ROW_HEIGHT))
        .px(px(8.))
        .rounded(px(7.))
        .flex()
        .items_center()
        .gap(px(8.))
        .text_size(FontSize::Medium)
        .map(|row| {
            if active {
                row.bg(active_bg).text_color(fg)
            } else {
                row.text_color(fg.opacity(ROW_TEXT_OPACITY)).hover(|row| row.bg(hover_bg).text_color(fg))
            }
        })
        .child(
            div()
                .flex_none()
                .w(px(ROW_ICON_SIZE))
                .flex()
                .justify_center()
                .child(svg().path(icon).size(px(15.)).text_color(fg.opacity(if active { 0.9 } else { 0.6 }))),
        )
        .child(div().flex_none().child(label))
        .children(trailing.map(|trailing| {
            div().flex_1().min_w_0().flex().justify_end().text_size(FontSize::Small).text_color(muted).child(trailing)
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                open(this, window, cx);
            }),
        )
        .on_a11y_press(view, move |this, window, cx| press(this, window, cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tally(needs_you: usize, todo: usize, running: usize) -> Tally {
        Tally { needs_you, todo, running }
    }

    #[test]
    fn tasks_summary_picks_the_text() {
        assert_eq!(TasksSummary::of(tally(0, 0, 0)), None);
        assert_eq!(TasksSummary::of(tally(0, 3, 0)), Some(TasksSummary::Todo(3)));
        assert_eq!(TasksSummary::of(tally(0, 0, 2)), Some(TasksSummary::Running(2)));
        assert_eq!(TasksSummary::of(tally(0, 3, 1)), Some(TasksSummary::Both { todo: 3, running: 1 }));
        // 有需要你的就只写它。
        assert_eq!(TasksSummary::of(tally(2, 3, 1)), Some(TasksSummary::NeedsYou(2)));
        assert_eq!(TasksSummary::of(tally(1, 0, 0)), Some(TasksSummary::NeedsYou(1)));
    }
}
