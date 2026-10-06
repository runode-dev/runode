//! 从长列表里挑一项的浮层：主题、字体、语言、提示音、快捷键的动作都用它。上面一个输入框按名字
//! 筛，上下键挪、回车选、Esc 或点外面关掉。

use std::ops::Range;

use gpui::{
    AnyElement, Context, Entity, Focusable, KeyDownEvent, MouseButton, ScrollStrategy, SharedString, Subscription,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};

use super::{SettingsView, controls::Colors, keybinds};
use crate::ui::text_field::{TextField, TextFieldEvent};

const PICKER_WIDTH: f32 = 420.;
const ROW_HEIGHT: f32 = 28.;
const VISIBLE_ROWS: f32 = 12.;

/// 选了以后写到哪里。
#[derive(Clone)]
pub(super) enum PickTarget {
    /// 只有一个值的键；选了空值就是删掉，回到默认。
    Value(&'static str),
    /// 字体列表里的第几个，等于列表长度时是加一个。
    FontFamily(usize),
    Theme(ThemeSlot),
    Keybind(keybinds::Target),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ThemeSlot {
    Single,
    Light,
    Dark,
}

pub(super) struct PickItem {
    value: String,
    label: SharedString,
    /// 名字后面淡色的补充，比如动作的写法。
    detail: Option<SharedString>,
}

impl PickItem {
    pub fn new(value: impl Into<String>, label: impl Into<SharedString>) -> Self {
        Self { value: value.into(), label: label.into(), detail: None }
    }

    pub fn with_detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.label.to_lowercase().contains(&query)
            || self.value.to_lowercase().contains(&query)
            || self.detail.as_ref().is_some_and(|detail| detail.to_lowercase().contains(&query))
    }
}

pub(super) struct Picker {
    title: SharedString,
    items: Vec<PickItem>,
    /// 当前选着的值，列表里打个勾。
    current: Option<String>,
    target: PickTarget,
    field: Entity<TextField>,
    /// 高亮的那一行，在筛过的列表里数。
    highlighted: usize,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
}

impl Picker {
    /// 筛过之后留下的项在 `items` 里的位置。
    fn visible(&self, cx: &gpui::App) -> Vec<usize> {
        let query = self.field.read(cx).query().trim();
        (0..self.items.len()).filter(|&ix| query.is_empty() || self.items[ix].matches(query)).collect()
    }
}

impl SettingsView {
    pub(super) fn open_picker(
        &mut self,
        title: impl Into<SharedString>,
        items: Vec<PickItem>,
        current: Option<String>,
        target: PickTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = cx.new(|cx| TextField::new(String::new(), cx));
        let subscription = cx.subscribe_in(&field, window, |this, _, event: &TextFieldEvent, window, cx| match event {
            TextFieldEvent::Changed(_) => {
                if let Some(picker) = &mut this.picker {
                    picker.highlighted = 0;
                    picker.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                cx.notify();
            }
            TextFieldEvent::Next => this.confirm_picker(window, cx),
            TextFieldEvent::Dismiss => this.close_picker(window, cx),
            TextFieldEvent::Previous => {}
        });
        let highlighted = current.as_ref().and_then(|current| items.iter().position(|item| item.value == *current));
        let scroll = UniformListScrollHandle::new();
        scroll.scroll_to_item(highlighted.unwrap_or(0), ScrollStrategy::Center);
        window.focus(&field.focus_handle(cx), cx);
        self.picker = Some(Picker {
            title: title.into(),
            items,
            current,
            target,
            field,
            highlighted: highlighted.unwrap_or(0),
            scroll,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(super) fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn confirm_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &self.picker else {
            return;
        };
        let visible = picker.visible(cx);
        if let Some(&ix) = visible.get(picker.highlighted) {
            self.pick(ix, window, cx);
        }
    }

    /// 选了 `items` 里的第 `ix` 项。
    fn pick(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        window.focus(&self.focus_handle, cx);
        let Some(item) = picker.items.into_iter().nth(ix) else {
            return;
        };
        let value = item.value;
        match picker.target {
            PickTarget::Value(key) => {
                if key.ends_with("-sound") && value != "none" {
                    crate::ui::sound::play(&value);
                }
                let values = if value.is_empty() { Vec::new() } else { vec![value] };
                self.write_or_report(key, values, cx);
            }
            PickTarget::FontFamily(ix) => {
                let mut fonts = self.config.font_family.clone();
                match fonts.get_mut(ix) {
                    Some(font) => *font = value,
                    None => fonts.push(value),
                }
                fonts.dedup();
                self.write_or_report("font-family", fonts, cx);
            }
            PickTarget::Theme(slot) => self.set_theme(slot, value, cx),
            PickTarget::Keybind(target) => keybinds::picked(self, target, value, window, cx),
        }
        cx.notify();
    }

    fn picker_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        let count = picker.visible(cx).len();
        if count == 0 {
            return;
        }
        let step: isize = match event.keystroke.key.as_str() {
            "up" => -1,
            "down" => 1,
            "pageup" => -(VISIBLE_ROWS as isize),
            "pagedown" => VISIBLE_ROWS as isize,
            _ => return,
        };
        cx.stop_propagation();
        picker.highlighted = picker.highlighted.saturating_add_signed(step).min(count - 1);
        picker.scroll.scroll_to_item(picker.highlighted, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn render_picker_rows(&mut self, range: Range<usize>, colors: Colors, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(picker) = &self.picker else {
            return Vec::new();
        };
        let visible = picker.visible(cx);
        range
            .filter_map(|row| {
                let ix = *visible.get(row)?;
                let item = &picker.items[ix];
                let current = picker.current.as_deref() == Some(item.value.as_str());
                Some(
                    div()
                        .id(("pick", row))
                        .w_full()
                        .h(px(ROW_HEIGHT))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(5.))
                        .map(|el| {
                            if row == picker.highlighted {
                                el.bg(colors.selected)
                            } else {
                                el.hover(|el| el.bg(colors.hover))
                            }
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.pick(ix, window, cx);
                            }),
                        )
                        .child(
                            div().w(px(12.)).flex_none().text_color(colors.accent).children(
                                current.then(|| {
                                    gpui::svg().path("icons/check.svg").size(px(11.)).text_color(colors.accent)
                                }),
                            ),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(item.label.clone()))
                        .children(item.detail.clone().map(|detail| {
                            div().flex_none().text_size(px(11.)).text_color(colors.fg.opacity(0.45)).child(detail)
                        }))
                        .into_any_element(),
                )
            })
            .collect()
    }

    pub(super) fn render_picker(&mut self, colors: Colors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.picker.as_ref()?;
        let count = picker.visible(cx).len();
        let list = uniform_list(
            "picker-list",
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| this.render_picker_rows(range, colors, cx)),
        )
        .track_scroll(&picker.scroll)
        .h(px(ROW_HEIGHT * VISIBLE_ROWS.min(count.max(1) as f32) + 8.))
        .p(px(4.));
        let empty = (count == 0).then(|| {
            div()
                .px(px(12.))
                .py(px(10.))
                .text_color(colors.fg.opacity(0.5))
                .child(rust_i18n::t!("settings.no_matches").into_owned())
        });
        let panel = div()
            .id("picker")
            .w(px(PICKER_WIDTH))
            .flex()
            .flex_col()
            .rounded(px(8.))
            .bg(colors.panel)
            .border_1()
            .border_color(colors.border)
            .shadow_lg()
            .occlude()
            .text_size(px(12.5))
            .capture_key_down(cx.listener(Self::picker_key))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px(px(12.))
                    .pt(px(10.))
                    .pb(px(4.))
                    .text_size(px(11.))
                    .text_color(colors.fg.opacity(0.55))
                    .child(picker.title.clone()),
            )
            .child(
                div()
                    .h(px(30.))
                    .mx(px(8.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .bg(colors.control)
                    .border_1()
                    .border_color(colors.border)
                    .child(div().flex_1().min_w_0().h_full().child(picker.field.clone())),
            )
            .map(|panel| match empty {
                Some(empty) => panel.child(empty),
                None => panel.child(list),
            });
        // 铺满窗口的一层，点在浮层外面就关掉。
        Some(
            div()
                .id("picker-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                // 不让面板被拉成整个窗口那么高。
                .items_start()
                .pt(px(70.))
                .bg(gpui::black().opacity(0.15))
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_picker(window, cx)))
                .child(panel)
                .into_any_element(),
        )
    }
}
