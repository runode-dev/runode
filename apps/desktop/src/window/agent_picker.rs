//! agent 列表：盖在窗口上面的浮层，列出所有窗口里的 agent 分屏，输入文字过滤，上下键选、回车
//! 跳过去、Esc 关掉。排序和过滤的规则见 `agents`。

use crate::ui::picker::{matches_query, nav_delta, picker_panel};
use crate::ui::popover::Popover;
use crate::ui::theme::ActiveTheme as _;
use std::collections::HashMap;

use gpui::{
    Context, Div, Entity, EntityId, Focusable, KeyDownEvent, Role, ScrollHandle, SharedString, Subscription, Window,
    div, prelude::*, px,
};

use super::{
    GotoAgent, PressDown, WindowView,
    agents::{AgentEntry, reveal},
    cards,
    titlebar::styled_agent_mark,
};
use crate::ui::a11y::StaticText;
use crate::ui::{
    font_size::{FontSize, scaled},
    input::{InputEvent, InputState},
};

/// 浮层的宽度；窗口窄时随窗口收窄。
const PICKER_WIDTH: f32 = 560.;
/// 每行两行字：agent 和所在的标签，下面是目录。
const ROW_HEIGHT: f32 = 40.;
/// 最多显示这么多行，多了滚动。
const VISIBLE_ROWS: f32 = 8.;

/// 开着的 agent 列表。
pub(super) struct AgentPicker {
    field: Entity<InputState>,
    /// 选中的那一行的分屏；状态变化让行重新排序时选中项跟着走。还没选过或者它不在列表里了时
    /// 选第一行。
    selected: Option<EntityId>,
    scroll: ScrollHandle,
    /// 各分屏的目录，打开列表后第一次列出它时读一次，免得每次重画都去问 shell 在哪。
    dirs: HashMap<EntityId, SharedString>,
    _subscriptions: [Subscription; 2],
}

impl WindowView {
    /// 打开 agent 列表；已经开着时关掉。
    pub(super) fn goto_agent(&mut self, _: &GotoAgent, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent_picker.is_some() {
            self.close_agent_picker(window, cx);
            return;
        }
        let field = cx.new(|cx| {
            InputState::new(String::new(), cx)
                .with_label(rust_i18n::t!("agent.picker.filter"))
                .with_placeholder(rust_i18n::t!("search.placeholder"))
        });
        // 输入框原本是搜索框：回车是「下一个」，Esc 是「关闭搜索」，在这里分别是跳过去和关掉。
        let events = cx.subscribe_in(&field, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Changed(_) => {
                if let Some(picker) = &mut this.agent_picker {
                    picker.selected = None;
                    picker.scroll.scroll_to_item(0);
                }
                cx.notify();
            }
            InputEvent::Next => this.confirm_agent_picker(window, cx),
            InputEvent::Dismiss => this.close_agent_picker(window, cx),
            InputEvent::Previous => {}
        });
        let focus = field.focus_handle(cx);
        // 点到别处就关掉；切到别的应用时窗口失去焦点，回来接着用。
        let blur = cx.on_blur(&focus, window, |this, window, cx| {
            if window.is_window_active() {
                this.agent_picker = None;
                cx.notify();
            }
        });
        window.focus(&focus, cx);
        self.agent_picker = Some(AgentPicker {
            field,
            selected: None,
            scroll: ScrollHandle::new(),
            dirs: HashMap::new(),
            _subscriptions: [events, blur],
        });
        cx.notify();
    }

    /// 关掉 agent 列表；焦点还在输入框里时还给当前终端。
    pub(super) fn close_agent_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.agent_picker.take() else {
            return;
        };
        let focused = picker.field.focus_handle(cx).is_focused(window);
        drop(picker);
        if focused {
            window.focus(&self.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// 过滤后的各行和它们的目录（家目录写成 `~`）；列表没开着时为空。
    fn picker_rows(&mut self, window: &Window, cx: &mut Context<Self>) -> Vec<(AgentEntry, SharedString)> {
        if self.agent_picker.is_none() {
            return Vec::new();
        }
        let entries = self.agent_entries(window, cx);
        let Some(picker) = self.agent_picker.as_mut() else {
            return Vec::new();
        };
        let query = picker.field.read(cx).query().to_owned();
        entries
            .into_iter()
            .filter_map(|entry| {
                let dir = picker
                    .dirs
                    .entry(entry.pane)
                    .or_insert_with(|| entry.view.read(cx).display_cwd().unwrap_or_default().into())
                    .clone();
                let mut fields = entry.haystack().to_vec();
                fields.push(dir.clone());
                matches_query(&fields, &query).then_some((entry, dir))
            })
            .collect()
    }

    fn picker_selected(&self, rows: &[(AgentEntry, SharedString)]) -> usize {
        let selected = self.agent_picker.as_ref().and_then(|picker| picker.selected);
        selected.and_then(|pane| rows.iter().position(|(entry, _)| entry.pane == pane)).unwrap_or(0)
    }

    /// 上下键：选中上一行（负数）或下一行，到头了绕回另一头。输入框自己的上下键（光标到行首、
    /// 行尾）在这之前拦下。
    fn agent_picker_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(delta) = nav_delta(event) else {
            return;
        };
        cx.stop_propagation();
        let rows = self.picker_rows(window, cx);
        if rows.is_empty() {
            return;
        }
        let next = (self.picker_selected(&rows) as isize + delta).rem_euclid(rows.len() as isize) as usize;
        if let Some(picker) = &mut self.agent_picker {
            picker.selected = Some(rows[next].0.pane);
            picker.scroll.scroll_to_item(next);
        }
        cx.notify();
    }

    /// 回车：跳到选中的那一行。
    fn confirm_agent_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.picker_rows(window, cx);
        let Some((entry, _)) = rows.get(self.picker_selected(&rows)) else {
            return;
        };
        let (target, pane) = (entry.window, entry.pane);
        self.close_agent_picker(window, cx);
        reveal(target, pane, cx);
    }

    /// 浮在标题栏下方正中的列表：上面是输入框，下面是各行。
    pub(super) fn render_agent_picker(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let rows = self.picker_rows(window, cx);
        let selected = self.picker_selected(&rows);
        let picker = self.agent_picker.as_ref()?;
        let selected_bg = cx.theme().popover_selected;
        let hover_bg = cx.theme().popover_hover;
        let fg = cx.theme().foreground;
        let empty = rows.is_empty().then(|| {
            let text = if picker.field.read(cx).query().is_empty() {
                rust_i18n::t!("agent.picker.none")
            } else {
                rust_i18n::t!("agent.picker.no_matches")
            };
            div()
                .id("agent-empty")
                .aria_static_text(text.clone())
                .px(px(10.))
                .py(px(10.))
                .text_color(fg.opacity(0.5))
                .child(text.into_owned())
        });
        let rows: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(ix, (entry, dir))| {
                let (target, pane) = (entry.window, entry.pane);
                let place = SharedString::from(format!("{} · {}", entry.workspace_name, entry.tab_title));
                div()
                    .id(("agent-row", pane))
                    .role(Role::ListBoxOption)
                    .aria_label(format!("{} · {place}", entry.kind_name()))
                    .aria_description(format!("{} · {dir}", entry.mark.status.label()))
                    .aria_selected(ix == selected)
                    .flex_none()
                    .h(scaled(ROW_HEIGHT))
                    .px(px(8.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .map(|row| if ix == selected { row.bg(selected_bg) } else { row.hover(|row| row.bg(hover_bg)) })
                    .on_press_down(cx, move |this, window, cx| {
                        this.close_agent_picker(window, cx);
                        reveal(target, pane, cx);
                    })
                    .child(styled_agent_mark(entry.mark, ("picker-agent", ix), fg, cards(cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .gap(px(6.))
                                    .child(div().flex_none().text_color(fg).child(entry.kind_name()))
                                    .child(div().min_w_0().truncate().text_color(fg.opacity(0.6)).child(place)),
                            )
                            // 路径长时留下结尾：最后几级目录最能区分。
                            .child(
                                div()
                                    .text_size(FontSize::Small)
                                    .text_color(fg.opacity(0.45))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis_start()
                                    .child(dir),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(FontSize::Small)
                            .text_color(fg.opacity(0.45))
                            .child(entry.mark.status.label()),
                    )
            })
            .collect();
        let panel = Popover::new("agent-picker")
            .role(Role::Dialog)
            .aria_label(rust_i18n::t!("agent.picker.title"))
            .w(px(PICKER_WIDTH))
            .capture_key_down(cx.listener(Self::agent_picker_key));
        let list = div()
            .id("agent-list")
            .role(Role::ListBox)
            .aria_label(rust_i18n::t!("agent.picker.title"))
            .max_h(scaled(ROW_HEIGHT * VISIBLE_ROWS + 8.))
            .track_scroll(&picker.scroll)
            .children(rows)
            .children(empty);
        Some(picker_panel(panel, picker.field.clone(), list, cx))
    }
}
