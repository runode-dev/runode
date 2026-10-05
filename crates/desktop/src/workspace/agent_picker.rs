//! agent 列表：盖在窗口上面的浮层，列出所有窗口里的 agent 分屏，输入文字过滤，上下键选、回车
//! 跳过去、Esc 关掉。排序和过滤的规则见 `agents`。

use std::collections::HashMap;

use gpui::{
    Context, Div, Entity, EntityId, Focusable, KeyDownEvent, MouseButton, ScrollHandle, SharedString, Subscription,
    Window, div, prelude::*, px,
};
use runode_shared_types::color::Rgb;

use super::{
    GotoAgent, TITLEBAR_HEIGHT, WindowView,
    agents::{AgentEntry, matches_query, reveal},
    divider_color,
    model::display_dir,
    titlebar::agent_mark,
};
use crate::{
    search_bar::{SearchField, SearchFieldEvent},
    terminal_view::hsla,
};

/// 浮层的宽度；窗口窄时随窗口收窄。
const PICKER_WIDTH: f32 = 560.;
/// 每行两行字：agent 和所在的标签，下面是目录。
const ROW_HEIGHT: f32 = 40.;
/// 最多显示这么多行，多了滚动。
const VISIBLE_ROWS: f32 = 8.;
const INPUT_HEIGHT: f32 = 34.;

/// 开着的 agent 列表。
pub(super) struct AgentPicker {
    field: Entity<SearchField>,
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
        let field = cx.new(|cx| SearchField::new(String::new(), cx));
        // 输入框原本是搜索框：回车是「下一个」，Esc 是「关闭搜索」，在这里分别是跳过去和关掉。
        let events = cx.subscribe_in(&field, window, |this, _, event: &SearchFieldEvent, window, cx| match event {
            SearchFieldEvent::Changed(_) => {
                if let Some(picker) = &mut this.agent_picker {
                    picker.selected = None;
                    picker.scroll.scroll_to_item(0);
                }
                cx.notify();
            }
            SearchFieldEvent::Next => this.confirm_agent_picker(window, cx),
            SearchFieldEvent::Dismiss => this.close_agent_picker(window, cx),
            SearchFieldEvent::Previous => {}
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
            window.focus(&self.tab().focused_view().focus_handle(cx), cx);
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
                    .or_insert_with(|| entry.view.read(cx).cwd().map(|dir| display_dir(&dir)).unwrap_or_default().into())
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
        let keystroke = &event.keystroke;
        let m = &keystroke.modifiers;
        let ctrl = m.control && !m.alt && !m.shift && !m.platform;
        let delta = match keystroke.key.as_str() {
            "up" if !m.modified() => -1,
            "down" if !m.modified() => 1,
            "p" if ctrl => -1,
            "n" if ctrl => 1,
            _ => return,
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
    pub(super) fn render_agent_picker(&mut self, fg: Rgb, bg: Rgb, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let rows = self.picker_rows(window, cx);
        let selected = self.picker_selected(&rows);
        let picker = self.agent_picker.as_ref()?;
        let panel_bg = hsla(bg.mix(fg, 0.05));
        let selected_bg = hsla(bg.mix(fg, 0.14));
        let hover_bg = hsla(bg.mix(fg, 0.09));
        let fg = hsla(fg);
        let empty = rows.is_empty().then(|| {
            let text = if picker.field.read(cx).query().is_empty() {
                rust_i18n::t!("agent.picker.none")
            } else {
                rust_i18n::t!("agent.picker.no_matches")
            };
            div().px(px(10.)).py(px(10.)).text_color(fg.opacity(0.5)).child(text.into_owned())
        });
        let rows: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(ix, (entry, dir))| {
                let (target, pane) = (entry.window, entry.pane);
                let place = SharedString::from(format!("{} · {}", entry.workspace_name, entry.tab_title));
                div()
                    .id(("agent-row", ix))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(8.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .map(|row| if ix == selected { row.bg(selected_bg) } else { row.hover(|row| row.bg(hover_bg)) })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_agent_picker(window, cx);
                            reveal(target, pane, cx);
                        }),
                    )
                    .child(agent_mark(entry.mark, ("picker-agent", ix), fg))
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
                                    .text_size(px(11.))
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
                            .text_size(px(11.))
                            .text_color(fg.opacity(0.45))
                            .child(entry.mark.status.label()),
                    )
            })
            .collect();
        let panel = div()
            .id("agent-picker")
            .w(px(PICKER_WIDTH))
            .max_w_full()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .bg(panel_bg)
            .border_1()
            .border_color(fg.opacity(0.15))
            .shadow_md()
            .occlude()
            .text_size(px(12.))
            .text_color(fg)
            .capture_key_down(cx.listener(Self::agent_picker_key))
            .child(
                div()
                    .flex_none()
                    .h(px(INPUT_HEIGHT))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(divider_color(fg))
                    .child(div().flex_1().min_w_0().h_full().child(picker.field.clone())),
            )
            .child(
                div()
                    .id("agent-list")
                    .max_h(px(ROW_HEIGHT * VISIBLE_ROWS + 8.))
                    .overflow_y_scroll()
                    .track_scroll(&picker.scroll)
                    .p(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .children(rows)
                    .children(empty),
            );
        // 外层铺满窗口宽度、只为把列表摆到正中，没有鼠标处理，不挡下面的点击。
        Some(
            div()
                .absolute()
                .top(px(TITLEBAR_HEIGHT + 8.))
                .left_0()
                .right_0()
                .px(px(16.))
                .flex()
                .justify_center()
                .child(panel),
        )
    }
}
