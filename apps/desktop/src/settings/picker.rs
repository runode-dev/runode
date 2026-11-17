//! 从长列表里挑一项的浮层：主题、字体、语言、提示音、快捷键的动作都用它。上面一个输入框按名字
//! 筛，上下键挪、回车选、Esc 或点外面关掉。

use crate::ui::icons::CHECK_ICON;
use crate::ui::picker::{match_rank, matches_query, nav_delta};
use crate::ui::theme::Theme;
use runode_config::Config;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, Focusable, KeyDownEvent, MouseButton, Role, ScrollStrategy, SharedString,
    Subscription, Task, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};

use super::SettingsView;
use crate::ui::popover::Popover;
use crate::ui::theme::ActiveTheme as _;
use crate::ui::{
    a11y::{Press as _, StaticText as _},
    font_size::{FontSize, scaled},
    input::{Input, InputEvent, InputState},
};

const PICKER_WIDTH: f32 = 420.;
const ROW_HEIGHT: f32 = 28.;
const VISIBLE_ROWS: f32 = 12.;
/// 键盘挪到一个主题后停这么久才换上它。
const PREVIEW_DELAY: Duration = Duration::from_millis(120);

/// 选了以后写到哪里。
#[derive(Clone)]
pub(super) enum PickTarget {
    /// 只有一个值的键；选了空值就是删掉，回到默认。
    Value(&'static str),
    /// 字体列表里的第几个，等于列表长度时是加一个。
    FontFamily(usize),
    Theme(ThemeSlot),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemeSlot {
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
        let detail = self.detail.as_deref().unwrap_or_default();
        matches_query(&[self.label.as_ref(), self.value.as_str(), detail], query)
    }
}

pub(super) struct Picker {
    title: SharedString,
    items: Vec<PickItem>,
    /// 当前选着的值，列表里打个勾。
    current: Option<String>,
    target: PickTarget,
    field: Entity<InputState>,
    /// 高亮的那一行，在筛过的列表里数。
    highlighted: usize,
    /// 挑主题时键盘挪过高亮、先换上看过效果，没选就关掉时要换回文件里的配置。
    previewed: bool,
    /// 等着换上键盘挪到的主题；跟着浮层一起丢掉就不换了。
    preview: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
}

impl Picker {
    /// 筛过之后留下的项在 `items` 里的位置，按 `match_rank` 排，同一档里照原来的先后。
    fn visible(&self, cx: &gpui::App) -> Vec<usize> {
        let query = self.field.read(cx).query();
        let mut visible: Vec<usize> = (0..self.items.len()).filter(|&ix| self.items[ix].matches(query)).collect();
        visible.sort_by_key(|&ix| match_rank(&self.items[ix].label, query));
        visible
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
        let title = title.into();
        let field = cx.new(|cx| {
            InputState::new(String::new(), cx)
                .with_label(title.clone())
                .with_placeholder(rust_i18n::t!("search.placeholder"))
        });
        let subscription = cx.subscribe_in(&field, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Changed(_) => {
                if let Some(picker) = &mut this.picker {
                    picker.highlighted = 0;
                    picker.scroll.scroll_to_item(0, ScrollStrategy::Top);
                }
                cx.notify();
            }
            InputEvent::Next => this.confirm_picker(window, cx),
            InputEvent::Dismiss => this.close_picker(window, cx),
            InputEvent::Previous => {}
        });
        let highlighted = current.as_ref().and_then(|current| items.iter().position(|item| item.value == *current));
        let scroll = UniformListScrollHandle::new();
        scroll.scroll_to_item(highlighted.unwrap_or(0), ScrollStrategy::Center);
        window.focus(&field.focus_handle(cx), cx);
        self.picker = Some(Picker {
            title,
            items,
            current,
            target,
            field,
            highlighted: highlighted.unwrap_or(0),
            previewed: false,
            preview: None,
            scroll,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(super) fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_picker(cx);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// 没选就丢掉浮层；先换上看过效果的，换回文件里的配置。
    pub(crate) fn drop_picker(&mut self, cx: &mut gpui::App) {
        if self.picker.take().is_some_and(|picker| picker.previewed) {
            crate::config::reload(cx);
        }
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
                    crate::sound::play(&value);
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
                // 选了列表里别处已有的字体时只留前面那个。
                let mut seen = std::collections::HashSet::new();
                fonts.retain(|font| seen.insert(font.clone()));
                self.write_or_report("font-family", fonts, cx);
            }
            PickTarget::Theme(slot) => {
                // 预览时换上的值可能放在另一边，`set_theme` 要按文件里的配置拼；写成了会重载，没写成要自己换回去。
                if picker.previewed {
                    self.config = Arc::new(Config::load(self.config.dark));
                }
                self.set_theme(slot, value, cx);
                if picker.previewed && self.errors.contains_key("theme") {
                    crate::config::reload(cx);
                }
            }
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
        let step = match event.keystroke.key.as_str() {
            "pageup" => -(VISIBLE_ROWS as isize),
            "pagedown" => VISIBLE_ROWS as isize,
            _ => match nav_delta(event) {
                Some(step) => step,
                None => return,
            },
        };
        cx.stop_propagation();
        picker.highlighted = picker.highlighted.saturating_add_signed(step).min(count - 1);
        picker.scroll.scroll_to_item(picker.highlighted, ScrollStrategy::Nearest);
        if let PickTarget::Theme(slot) = picker.target
            && let Some(&ix) = picker.visible(cx).get(picker.highlighted)
        {
            let name = picker.items[ix].value.clone();
            // 按住方向键连着挪时只换上停下来的那个：每换一次要重读配置、通知各宿主。
            picker.preview = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(PREVIEW_DELAY).await;
                this.update(cx, |this, cx| {
                    if let Some(picker) = &mut this.picker {
                        picker.previewed = true;
                    }
                    let value = this.theme_preview(slot, name);
                    crate::config::preview("theme", &value, cx);
                })
                .ok();
            }));
        }
        cx.notify();
    }

    fn render_picker_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let Some(picker) = &self.picker else {
            return Vec::new();
        };
        let visible = picker.visible(cx);
        let row_hover = cx.theme().popover_hover;
        range
            .filter_map(|row| {
                let ix = *visible.get(row)?;
                let item = &picker.items[ix];
                let current = picker.current.as_deref() == Some(item.value.as_str());
                Some(
                    div()
                        .id(format!("pick-{}", item.value))
                        .role(Role::ListBoxOption)
                        .aria_label(item.label.clone())
                        .aria_selected(row == picker.highlighted)
                        .w_full()
                        .h(scaled(ROW_HEIGHT))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(5.))
                        .map(|el| {
                            if row == picker.highlighted {
                                el.bg(theme.popover_selected)
                            } else {
                                el.hover(|el| el.bg(row_hover))
                            }
                        })
                        .on_press(cx, move |this, window, cx| this.pick(ix, window, cx))
                        .child(div().w(px(12.)).flex_none().text_color(theme.ring).children(
                            current.then(|| gpui::svg().path(CHECK_ICON).size(px(11.)).text_color(theme.ring)),
                        ))
                        .child(div().flex_1().min_w_0().truncate().child(item.label.clone()))
                        .children(item.detail.clone().map(|detail| {
                            div()
                                .flex_none()
                                .text_size(FontSize::Small)
                                .text_color(theme.foreground.opacity(0.45))
                                .child(detail)
                        }))
                        .into_any_element(),
                )
            })
            .collect()
    }

    pub(super) fn render_picker(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.picker.as_ref()?;
        let count = picker.visible(cx).len();
        let list = uniform_list(
            "picker-list",
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| this.render_picker_rows(range, cx)),
        )
        .track_scroll(&picker.scroll)
        .h(scaled(ROW_HEIGHT * VISIBLE_ROWS.min(count.max(1) as f32) + 8.))
        .p(px(4.));
        let empty = (count == 0).then(|| {
            let text = rust_i18n::t!("settings.no_matches").into_owned();
            div()
                .id("picker-empty")
                .aria_static_text(text.clone())
                .px(px(12.))
                .py(px(10.))
                .text_color(theme.foreground.opacity(0.5))
                .child(text)
        });
        let panel = Popover::new("picker")
            .role(Role::Dialog)
            .aria_label(picker.title.clone())
            .w(px(PICKER_WIDTH))
            .flex()
            .flex_col()
            .shadow_lg()
            .capture_key_down(cx.listener(Self::picker_key))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px(px(12.))
                    .pt(px(10.))
                    .pb(px(4.))
                    .text_size(FontSize::Small)
                    .text_color(theme.foreground.opacity(0.55))
                    .child(picker.title.clone()),
            )
            .child(Input::new(&picker.field).h(scaled(30.)).mx(px(8.)).bg(theme.mix(0.10)))
            .map(|panel| match empty {
                Some(empty) => panel.child(empty),
                // `UniformList` 报不了角色，列表框的角色挂在包它的一层上。
                None => panel
                    .child(div().id("picker-options").role(Role::ListBox).aria_label(picker.title.clone()).child(list)),
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
