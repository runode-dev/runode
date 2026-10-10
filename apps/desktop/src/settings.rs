//! 设置页：配置文件里的每一项做成开关、选项、输入框或列表，改了就写回 runode 自己的配置文件
//! （`runode_config::ConfigFile`，文件里别的行原样留着），再重载配置让各个窗口跟着变。铺在终端窗口
//! 里（`window::show_settings`），盖住侧栏和终端；点左上的返回或按 Esc 发 `Close`，回到终端。
//!
//! 显示的是合起来生效的值：Ghostty 的配置、主题和 runode 的配置文件叠在一起之后的结果。runode 的
//! 配置文件里写了的项旁边有恢复按钮，点了删掉那几行，回到 Ghostty、主题或内置的值。输入框停手
//! 一会儿、按回车或者失去焦点时写回，写之前按读配置时的规矩检查，不对就不写、在那一项下面说原因。
//!
//! 哪一页有哪些项在 `pages`，开关、选项这些控件在 `controls`，从长列表里挑一项的浮层在 `picker`，
//! 快捷键那一页在 `keybinds`，远程访问那一页的配对手机在 `pairing`，外观页的配色在 `theme`，
//! 模型那一页（大模型和决策模型，经用户自己装的 runode-infer）在 `decision`。

mod autostart;
mod controls;
mod decision;
pub(crate) use decision::DecisionPulls;
mod keybinds;
pub(crate) use keybinds::caps as keybind_caps;
mod pages;
mod pairing;
mod picker;
mod theme;

use std::{collections::HashMap, ffi::OsString, sync::Arc, time::Duration};

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    MouseDownEvent, Render, Role, ScrollHandle, SharedString, Subscription, Task, Window, div, point, prelude::*, px,
    svg,
};
use runode_config::{Config, ConfigFile};

use crate::{
    assets::ARROW_LEFT_ICON,
    config::AppConfig,
    ui::{
        a11y::Hide,
        text_field::{TextField, TextFieldEvent},
    },
};
use controls::Colors;
use pages::Page;
use picker::Picker;

/// 左边页面列表的宽度。
const NAV_WIDTH: f32 = 200.;
/// 输入框停手这么久之后写回。
const COMMIT_DELAY: Duration = Duration::from_millis(700);

/// 点了返回、按了 Esc 或关分屏的键：窗口收起设置页。
pub struct Close;

// 关分屏的键在设置页里收起设置页，见 `keybinds::bind`。
gpui::actions!(runode, [CloseSettings]);

/// 输入框里的文字改的是什么。
#[derive(Clone, Debug, PartialEq)]
enum Commit {
    /// 只有一个值的键；空着就删掉，回到默认。
    Value(&'static str),
    /// 可以写多行的键里的第几行；空着就删掉这一行。
    Item(&'static str, usize),
    /// 调色板里的一个序号。
    Palette(u8),
}

struct Field {
    input: Entity<TextField>,
    commit: Commit,
    /// 停手一会儿再写回的计时。
    pending: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

pub struct SettingsView {
    focus_handle: FocusHandle,
    page: Page,
    config: Arc<Config>,
    /// runode 自己的配置文件，写回时在它上面改。
    file: ConfigFile,
    /// 各个输入框，按 `field_id`；换页时清空。
    fields: HashMap<String, Field>,
    /// 写回失败的原因，按输入框或键名。
    errors: HashMap<String, String>,
    picker: Option<Picker>,
    keybinds: keybinds::State,
    /// 读过的主题的颜色，按主题名；找不到的主题记为 `None`，不再去找。
    theme_looks: HashMap<String, Option<theme::Look>>,
    scroll: ScrollHandle,
    /// 终端里 shell 报告的 PATH，找 runode-infer 用。
    shell_path: Option<OsString>,
    decision: decision::State,
    _observe: Subscription,
    _pulls: Subscription,
}

impl EventEmitter<Close> for SettingsView {}

impl SettingsView {
    /// `shell_path` 是打开设置页时那个终端里 shell 报告的 PATH。
    pub fn new(shell_path: Option<OsString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe_global_in::<AppConfig>(window, |this, window, cx| this.config_changed(window, cx));
        let pulls = cx.observe_global::<DecisionPulls>(|this, cx| this.pulls_changed(cx));
        Self {
            focus_handle: cx.focus_handle(),
            page: Page::General,
            config: cx.global::<AppConfig>().0.clone(),
            file: read_file(),
            fields: HashMap::new(),
            errors: HashMap::new(),
            picker: None,
            keybinds: keybinds::State::default(),
            theme_looks: HashMap::new(),
            scroll: ScrollHandle::new(),
            shell_path,
            decision: decision::State::default(),
            _observe: observe,
            _pulls: pulls,
        }
    }

    /// 配置重载了：换上新值，没在编辑的输入框跟着更新。
    fn config_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.config = cx.global::<AppConfig>().0.clone();
        self.file = read_file();
        for field in self.fields.values() {
            if field.input.focus_handle(cx).is_focused(window) {
                continue;
            }
            let text = self.field_text(&field.commit);
            field.input.update(cx, |input, cx| {
                if input.query() != text {
                    input.set_query(text, cx);
                }
            });
        }
        cx.notify();
    }

    fn select_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if self.page == page {
            return;
        }
        self.commit_all(cx);
        self.page = page;
        self.fields.clear();
        self.errors.clear();
        self.picker = None;
        self.keybinds.stop_recording();
        self.scroll.set_offset(point(px(0.), px(0.)));
        if page == Page::Decision {
            self.refresh_decision(cx);
        }
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// 输入框里该显示的文字：当前生效的值。
    fn field_text(&self, commit: &Commit) -> String {
        match commit {
            Commit::Value(key) => self.config.values(key).into_iter().next().unwrap_or_default(),
            Commit::Item(key, ix) => self.list_values(key).get(*ix).cloned().unwrap_or_default(),
            Commit::Palette(ix) => self
                .config
                .palette
                .iter()
                .rfind(|(i, _)| i == ix)
                .map(|(_, c)| runode_config::hex(*c))
                .unwrap_or_default(),
        }
    }

    /// 可以写多行的键当前的各行：生效的值；`config-file` 读进来不留原文，看配置文件本身。
    fn list_values(&self, key: &str) -> Vec<String> {
        match key {
            "config-file" => self.file.values(key),
            _ => self.config.values(key),
        }
    }

    /// `id` 这个输入框，没有就建一个，显示 `commit` 对应的值。
    fn field(
        &mut self,
        id: &str,
        commit: Commit,
        placeholder: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextField> {
        if let Some(field) = self.fields.get(id) {
            return field.input.clone();
        }
        let text = self.field_text(&commit);
        let label = match &commit {
            Commit::Value(key) | Commit::Item(key, _) => pages::key_title(key),
            Commit::Palette(ix) => format!("{} {ix}", pages::key_title("palette")),
        };
        let input = cx.new(|cx| {
            let input = TextField::editing(text, 0, cx).with_label(label);
            match placeholder {
                Some(placeholder) => input.with_placeholder(placeholder),
                None => input,
            }
        });
        let key = id.to_owned();
        let changed = cx.subscribe_in(&input, window, move |this, _, event: &TextFieldEvent, window, cx| match event {
            TextFieldEvent::Changed(_) => this.schedule_commit(&key, cx),
            TextFieldEvent::Next | TextFieldEvent::Previous => this.commit(&key, cx),
            TextFieldEvent::Dismiss => {
                this.revert(&key, cx);
                window.focus(&this.focus_handle, cx);
            }
        });
        let key = id.to_owned();
        let blurred = cx.on_focus_out(&input.focus_handle(cx), window, move |this, _, _, cx| this.commit(&key, cx));
        self.fields.insert(
            id.to_owned(),
            Field { input: input.clone(), commit, pending: None, _subscriptions: [changed, blurred] },
        );
        input
    }

    fn schedule_commit(&mut self, id: &str, cx: &mut Context<Self>) {
        let key = id.to_owned();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COMMIT_DELAY).await;
            this.update(cx, |this, cx| this.commit(&key, cx)).ok();
        });
        if let Some(field) = self.fields.get_mut(id) {
            field.pending = Some(task);
        }
    }

    /// 换页、收起前把还没写回的输入框写回。
    pub fn commit_all(&mut self, cx: &mut Context<Self>) {
        let pending: Vec<String> =
            self.fields.iter().filter(|(_, field)| field.pending.is_some()).map(|(id, _)| id.clone()).collect();
        for id in pending {
            self.commit(&id, cx);
        }
    }

    /// 把输入框 `id` 里的文字写回配置文件；和当前的值一样时不写。
    fn commit(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(field) = self.fields.get_mut(id) else {
            return;
        };
        field.pending = None;
        let text = field.input.read(cx).query().trim().to_owned();
        let commit = field.commit.clone();
        if text == self.field_text(&commit) {
            self.errors.remove(id);
            cx.notify();
            return;
        }
        let written = match commit {
            Commit::Value(key) => self.write(key, if text.is_empty() { Vec::new() } else { vec![text] }, cx),
            Commit::Item(key, ix) => {
                let mut values = self.list_values(key);
                match (ix < values.len(), text.is_empty()) {
                    (true, true) => drop(values.remove(ix)),
                    (true, false) => values[ix] = text,
                    (false, true) => {}
                    (false, false) => values.push(text),
                }
                self.write(key, values, cx)
            }
            Commit::Palette(ix) => {
                let mut values = self.file.values("palette");
                values.retain(|value| palette_index(value) != Some(ix));
                if !text.is_empty() {
                    values.push(format!("{ix}={text}"));
                }
                self.write("palette", values, cx)
            }
        };
        match written {
            Ok(()) => drop(self.errors.remove(id)),
            Err(err) => drop(self.errors.insert(id.to_owned(), err)),
        }
        cx.notify();
    }

    /// Esc：放弃输入框里改的，换回当前的值。
    fn revert(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(field) = self.fields.get_mut(id) else {
            return;
        };
        field.pending = None;
        let (input, commit) = (field.input.clone(), field.commit.clone());
        let text = self.field_text(&commit);
        input.update(cx, |input, cx| input.set_query(text, cx));
        self.errors.remove(id);
        cx.notify();
    }

    /// 把 `key` 在 runode 配置文件里的值换成 `values` 并重载配置；`values` 为空就是删掉，回到
    /// Ghostty、主题或内置的值。新写的值先按读配置的规矩检查，文件里本来就有的不再查（手写错了的
    /// 那几行留着，读配置时照样跳过）。
    fn write(&mut self, key: &str, values: Vec<String>, cx: &mut Context<Self>) -> Result<(), String> {
        let old = self.file.values(key);
        for value in values.iter().filter(|value| !old.contains(value)) {
            runode_config::check_value(key, value)
                .map_err(|err| rust_i18n::t!("settings.invalid", err = err).into_owned())?;
        }
        self.write_unchecked(key, &values, cx)
    }

    fn write_unchecked(&mut self, key: &str, values: &[String], cx: &mut Context<Self>) -> Result<(), String> {
        let path = runode_config::config_path().ok_or_else(|| rust_i18n::t!("settings.no_home").into_owned())?;
        self.file = crate::config::write_values(&path, key, values, cx)
            .map_err(|err| rust_i18n::t!("settings.write_failed", err = err.to_string()).into_owned())?;
        self.errors.remove(key);
        Ok(())
    }

    /// 写回，失败时把原因记在 `key` 这一项下面。
    fn write_or_report(&mut self, key: &str, values: Vec<String>, cx: &mut Context<Self>) {
        if let Err(err) = self.write(key, values, cx) {
            self.errors.insert(key.to_owned(), err);
        }
        cx.notify();
    }

    fn render_nav(&self, colors: Colors, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let item = |id: &'static str, icon: &'static str, label: String, selected: bool| {
            div()
                .id(id)
                .aria_label(label.clone())
                .h(px(34.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(8.))
                .text_size(px(14.))
                .map(|item| {
                    if selected {
                        item.bg(colors.selected).text_color(colors.fg)
                    } else {
                        item.text_color(colors.fg.opacity(0.75)).hover(|item| item.bg(colors.hover))
                    }
                })
                .child(svg().flex_none().path(icon).size(px(16.)).text_color(colors.fg.opacity(0.7)))
                .child(label)
        };
        let back = item("settings-back", ARROW_LEFT_ICON, rust_i18n::t!("settings.back").into_owned(), false)
            .role(Role::Button)
            .mb(px(8.))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(Close)));
        div()
            .flex_none()
            .w(px(NAV_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .bg(colors.panel)
            .border_r_1()
            .border_color(colors.border)
            .id("settings-nav")
            .child(titlebar_strip())
            .child(div().flex().flex_col().gap(px(2.)).px(px(10.)).child(back).child(
                div().id("settings-pages").role(Role::TabList).flex().flex_col().gap(px(2.)).children(
                    Page::ALL.into_iter().map(|page| {
                        item(page.id(), page.icon(), page.title(), page == self.page)
                            .role(Role::Tab)
                            .aria_selected(page == self.page)
                            .on_click(cx.listener(move |this, _, window, cx| this.select_page(page, window, cx)))
                    }),
                ),
            ))
    }

    /// Esc：焦点在页面本身（不在输入框、挑选浮层里，也没在录快捷键）时收起。
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape"
            && !event.keystroke.modifiers.modified()
            && self.focus_handle.is_focused(window)
            && self.picker.is_none()
        {
            cx.stop_propagation();
            cx.emit(Close);
        }
    }
}

/// 顶上那一条：放红绿灯，按住拖动窗口，双击缩放。
fn titlebar_strip() -> gpui::Div {
    div().flex_none().h(px(crate::window::TITLEBAR_HEIGHT)).w_full().on_mouse_down(
        MouseButton::Left,
        |event: &MouseDownEvent, window: &mut Window, _: &mut App| {
            if event.click_count >= 2 {
                window.titlebar_double_click();
            } else {
                window.start_window_move();
            }
        },
    )
}

/// `palette` 一行的序号。
fn palette_index(value: &str) -> Option<u8> {
    value.split_once('=')?.0.trim().parse().ok()
}

/// runode 的配置文件；读不出来时当作空的，写回时会再读一遍并报错。
fn read_file() -> ConfigFile {
    let Some(path) = runode_config::config_path() else {
        return ConfigFile::default();
    };
    ConfigFile::read(&path).unwrap_or_else(|err| {
        tracing::warn!("failed to read {}: {err}", path.display());
        ConfigFile::default()
    })
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Colors::new(&self.config);
        let nav = self.render_nav(colors, cx);
        let content = self.render_page(colors, window, cx);
        let picker = self.render_picker(colors, cx);
        // 挑选浮层挡住了后面的页面，开着时后面的不报给辅助工具。
        let modal = picker.is_some();
        div()
            .key_context("Settings")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|_, _: &CloseSettings, _, cx| cx.emit(Close)))
            .size_full()
            .relative()
            .flex()
            .bg(colors.bg)
            .text_color(colors.fg)
            .text_size(px(13.))
            .child(nav.aria_hidden(modal))
            .child(
                div().flex_1().min_w_0().h_full().flex().flex_col().child(titlebar_strip()).child(
                    div()
                        .id("settings-page")
                        .aria_hidden(modal)
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(div().mx_auto().w_full().max_w(px(760.)).px(px(32.)).pb(px(32.)).child(content)),
                ),
            )
            .children(picker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_lines_have_an_index() {
        assert_eq!(palette_index("3=#ffffff"), Some(3));
        assert_eq!(palette_index(" 12 = red"), Some(12));
        assert_eq!(palette_index("x=#fff"), None);
    }
}
