//! 设置页的各页：每页有哪些项、各用什么控件，以及主题、字体、调色板这些不止一个控件的项。

use gpui::{AnyElement, Context, Div, ElementId, Role, SharedString, Window, div, prelude::*, px};
use runode_config::StatusItem;
use runode_shared_types::agent::AgentKind;

use super::{
    Commit, SettingsView,
    controls::{
        Cards, Colors, Press, button, chip, dropdown, icon_button, input_box, reset_button, row, segmented, swatch,
        switch,
    },
    picker::{PickItem, PickTarget},
};
use crate::{
    i18n::tr,
    ui::{display_dir, hsla},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    General,
    Appearance,
    Colors,
    Terminal,
    Files,
    Agents,
    Decision,
    Remote,
    Keybinds,
}

/// 页面里的一项。
#[derive(Clone, Copy)]
enum Item {
    /// 小标题，文字是翻译里的 `settings.section.<名字>`。
    Section(&'static str),
    /// 一个键一行。
    Row(&'static str, Control),
    Theme,
    FontFamily,
    Palette,
    StatusBarHidden,
    AgentExclude,
    ConfigFiles,
    ConfigFileActions,
    Pairing,
    /// 登录时自启的两项（宿主、app），装没装看服务文件，不是配置里的键。
    Autostart,
    /// 模型那一页的全部内容，大多经 runode-infer 读写；只有两个默认模型（`chat-model`、
    /// `decision-model`）是配置里的键。
    Decision,
}

#[derive(Clone, Copy)]
enum Control {
    Switch,
    /// 分段选项；值为空的那一项表示不写，用默认。
    Choice(&'static [&'static str]),
    /// 输入框，宽度。
    Text(f32),
    Color,
    Language,
    Sound,
}

impl Page {
    pub const ALL: [Self; 9] = [
        Self::General,
        Self::Appearance,
        Self::Colors,
        Self::Terminal,
        Self::Files,
        Self::Agents,
        Self::Decision,
        Self::Remote,
        Self::Keybinds,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Colors => "colors",
            Self::Terminal => "terminal",
            Self::Files => "files",
            Self::Agents => "agents",
            Self::Decision => "decision",
            Self::Remote => "remote",
            Self::Keybinds => "keybinds",
        }
    }

    pub fn title(self) -> String {
        tr(&format!("settings.page.{}", self.id()))
    }

    pub fn icon(self) -> &'static str {
        use crate::assets::{
            FILES_ICON, KEYBOARD_ICON, MEMORY_ICON, PALETTE_ICON, PHONE_ICON, SETTINGS_ICON, SLIDERS_ICON,
            SPARKLE_ICON, TERMINAL_ICON,
        };
        match self {
            Self::General => SETTINGS_ICON,
            Self::Appearance => SLIDERS_ICON,
            Self::Colors => PALETTE_ICON,
            Self::Terminal => TERMINAL_ICON,
            Self::Files => FILES_ICON,
            Self::Agents => SPARKLE_ICON,
            Self::Decision => MEMORY_ICON,
            Self::Remote => PHONE_ICON,
            Self::Keybinds => KEYBOARD_ICON,
        }
    }

    fn items(self) -> &'static [Item] {
        use Control::*;
        use Item::*;
        match self {
            Self::General => &[
                Row("language", Language),
                Row("terminal-host", Switch),
                Row("auto-update", Switch),
                Section("login"),
                Autostart,
                Section("config_file"),
                ConfigFileActions,
                ConfigFiles,
            ],
            Self::Appearance => &[
                Theme,
                Section("font"),
                FontFamily,
                Row("font-size", Text(80.)),
                Row("adjust-cell-height", Text(80.)),
                Section("window"),
                Row("window-style", Choice(&["cards", "classic"])),
                Row("window-padding-x", Text(80.)),
                Row("window-padding-y", Text(80.)),
                Row("status-bar", Switch),
                StatusBarHidden,
            ],
            Self::Colors => &[
                Row("background", Color),
                Row("foreground", Color),
                Row("cursor-color", Color),
                Row("cursor-text", Color),
                Row("selection-background", Color),
                Row("selection-foreground", Color),
                Row("search-background", Color),
                Row("search-foreground", Color),
                Row("search-selected-background", Color),
                Row("search-selected-foreground", Color),
                Palette,
            ],
            Self::Terminal => &[
                Section("cursor"),
                Row("cursor-style", Choice(&["block", "bar", "underline", "block_hollow"])),
                Row("cursor-style-blink", Choice(&["", "true", "false"])),
                Row("cursor-style-blink-timeout", Text(80.)),
                Section("keyboard"),
                Row("macos-option-as-alt", Choice(&["false", "true", "left", "right"])),
                Section("shell"),
                Row("shell-integration", Choice(&["detect", "none", "zsh", "bash", "fish"])),
                Row("shell-integration-features", Choice(&["cursor", "no-cursor"])),
                Row("command-suggestions", Switch),
                Row("command-completions", Switch),
                Row("command-highlighting", Switch),
                Section("scrollback"),
                Row("scrollback-limit", Text(120.)),
                Section("clipboard"),
                Row("clipboard-write", Choice(&["allow", "deny"])),
                Row("clipboard-read", Choice(&["ask", "allow", "deny"])),
                Section("tasks"),
                Row("task-placement", Choice(&["right", "down", "tab"])),
            ],
            Self::Files => &[
                Row("file-tree-font-size", Text(80.)),
                Row("file-tree-preview-click", Choice(&["single", "double"])),
                Row("preview-font-size", Text(80.)),
            ],
            Self::Agents => &[
                Row("agent-notifications", Switch),
                Row("agent-done-sound", Sound),
                Row("agent-blocked-sound", Sound),
                AgentExclude,
            ],
            Self::Decision => &[Decision],
            Self::Remote => &[
                Row("remote-access", Switch),
                Row("remote-access-port", Text(80.)),
                Row("remote-access-name", Text(200.)),
                Pairing,
                Section("push"),
                Row("remote-access-push", Switch),
                Row("remote-access-push-text", Switch),
                Row("remote-access-push-delay", Text(80.)),
                Section("push_key"),
                Row("apns-key-file", Text(300.)),
                Row("apns-key-id", Text(120.)),
                Row("apns-team-id", Text(120.)),
                Row("apns-bundle-id", Text(200.)),
                Row("push-relay-url", Text(300.)),
            ],
            Self::Keybinds => &[],
        }
    }

    /// 这一页管的键。
    #[cfg(test)]
    fn keys(self) -> Vec<&'static str> {
        if self == Self::Keybinds {
            return vec!["keybind"];
        }
        self.items()
            .iter()
            .filter_map(|item| match item {
                Item::Row(key, _) => Some(*key),
                Item::Theme => Some("theme"),
                Item::FontFamily => Some("font-family"),
                Item::Palette => Some("palette"),
                Item::StatusBarHidden => Some("status-bar-hidden"),
                Item::AgentExclude => Some("agent-notifications-exclude"),
                Item::ConfigFiles => Some("config-file"),
                Item::Section(_) | Item::ConfigFileActions | Item::Pairing | Item::Autostart | Item::Decision => None,
            })
            // 模型页的两个默认模型不是 `Item::Row`，画在 `render_decision` 里。
            .chain(if self == Self::Decision { &["chat-model", "decision-model"][..] } else { &[] }.iter().copied())
            .collect()
    }
}

/// 翻译里键名用下划线。
fn tr_key(key: &str) -> String {
    key.replace('-', "_")
}

pub(super) fn key_title(key: &str) -> String {
    tr(&format!("settings.key.{}", tr_key(key)))
}

pub(super) fn key_hint(key: &str) -> SharedString {
    tr(&format!("settings.hint.{}", tr_key(key))).into()
}

fn choice_label(key: &str, value: &str) -> SharedString {
    if value.is_empty() {
        return rust_i18n::t!("settings.default").into_owned().into();
    }
    tr(&format!("settings.choice.{}.{value}", tr_key(key))).into()
}

fn id(prefix: &str, key: &str) -> ElementId {
    ElementId::Name(format!("{prefix}-{key}").into())
}

/// 语言的名字，用它自己的文字写。
pub(super) fn language_name(locale: &str) -> String {
    rust_i18n::t!("settings.language_name", locale = locale).into_owned()
}

impl SettingsView {
    pub(super) fn render_page(&mut self, colors: Colors, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let title = div()
            .id("settings-title")
            .role(Role::Heading)
            .aria_level(1)
            .aria_label(self.page.title())
            .pt(px(8.))
            .pb(px(12.))
            .text_size(px(22.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(self.page.title());
        let page = div().flex().flex_col().child(title);
        if self.page == Page::Keybinds {
            return page.child(self.render_keybinds(colors, window, cx));
        }
        // 小标题把一页分成几组，每组收进一张卡片。
        let mut cards = Cards::new(page, colors);
        for item in self.page.items() {
            let element = match *item {
                Item::Section(name) => {
                    cards.section(tr(&format!("settings.section.{name}")));
                    continue;
                }
                Item::Row(key, control) => self.render_row(key, control, colors, window, cx).into_any_element(),
                // 预览卡不进卡片，选主题的下拉自成一组。
                Item::Theme => {
                    cards.section(tr("settings.section.color_scheme"));
                    cards.raw(self.render_theme_modes(colors, cx).into_any_element());
                    for row in self.render_theme_rows(colors, cx) {
                        cards.push(row);
                    }
                    continue;
                }
                Item::FontFamily => self.render_font_family(colors, cx).into_any_element(),
                Item::Palette => self.render_palette(colors, window, cx).into_any_element(),
                Item::StatusBarHidden => self.render_status_bar_hidden(colors, cx).into_any_element(),
                Item::AgentExclude => self.render_agent_exclude(colors, cx).into_any_element(),
                Item::ConfigFiles => self.render_config_files(colors, window, cx).into_any_element(),
                Item::ConfigFileActions => self.render_config_file_actions(colors, cx).into_any_element(),
                Item::Pairing => self.render_pairing(colors, cx).into_any_element(),
                Item::Autostart => self.render_autostart(colors, cx).into_any_element(),
                // 自己分组、自己画卡片：没装 runode-infer 时只有一张。
                Item::Decision => {
                    cards.raw(self.render_decision(colors, window, cx).into_any_element());
                    continue;
                }
            };
            cards.push(element);
        }
        cards.finish()
    }

    /// runode 的配置文件里写了 `key`、写的又不是默认值时的恢复按钮：写的正是默认值时删掉它也没有变化，
    /// 比如开关拨回默认的那一边。
    pub(super) fn reset(&self, key: &'static str, colors: Colors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let changed = self.file.has(key) && self.file.values(key) != runode_config::Config::default().values(key);
        changed.then(|| {
            reset_button(id("reset", key), colors, cx, move |this, _, cx| this.write_or_report(key, Vec::new(), cx))
        })
    }

    fn render_row(
        &mut self,
        key: &'static str,
        control: Control,
        colors: Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let current = self.config.values(key);
        let mut error = self.errors.get(key).cloned();
        let control = match control {
            Control::Switch => {
                let on = current.first().is_some_and(|value| value == "true");
                let switch = switch(id("switch", key), key_title(key), on, colors)
                    .on_press(cx, move |this, _, cx| this.write_or_report(key, vec![(!on).to_string()], cx));
                let notify = key == "agent-notifications" && on;
                // 开着通知、系统设置里却拒绝了：通知发不出来，提示去系统设置里打开。
                let denied = notify && crate::window::notifications_denied(cx);
                if denied {
                    error.get_or_insert_with(|| rust_i18n::t!("settings.notifications_denied").into_owned());
                }
                let open = denied.then(|| {
                    button("open-notification-settings", rust_i18n::t!("settings.open_notification_settings"), colors)
                        .on_click(|_, _, cx| {
                            cx.open_url("x-apple.systempreferences:com.apple.Notifications-Settings.extension")
                        })
                });
                let test = notify.then(|| {
                    button("test-notification", rust_i18n::t!("settings.test_notification"), colors)
                        .on_click(|_, _, cx| crate::window::test_notification(cx))
                });
                div().flex().items_center().gap(px(8.)).children(open).children(test).child(switch).into_any_element()
            }
            Control::Choice(values) => {
                let options = values.iter().map(|value| (value.to_string(), choice_label(key, value))).collect();
                let selected = current.first().map_or("", String::as_str);
                segmented(key, key_title(key), options, Some(selected), colors, cx, move |this, value, _, cx| {
                    let values = if value.is_empty() { Vec::new() } else { vec![value.to_owned()] };
                    this.write_or_report(key, values, cx);
                })
                .into_any_element()
            }
            Control::Text(width) => {
                let input = self.field(key, Commit::Value(key), Some(key_placeholder(key)), window, cx);
                input_box(input, Some(width), self.errors.contains_key(key), colors, window, cx).into_any_element()
            }
            Control::Color => {
                let color = current.first().and_then(|value| runode_config::color::parse(value)).map(hsla);
                let input = self.field(key, Commit::Value(key), Some(key_placeholder(key)), window, cx);
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(swatch(color, 20., colors))
                    .child(input_box(input, Some(130.), self.errors.contains_key(key), colors, window, cx))
                    .into_any_element()
            }
            Control::Language => {
                let label = match &self.config.language {
                    Some(locale) => language_name(locale),
                    None => rust_i18n::t!("settings.follow_system").into_owned(),
                };
                dropdown(id("dropdown", key), key_title(key), None, label, 180., colors)
                    .on_press(cx, move |this, window, cx| this.open_language_picker(window, cx))
                    .into_any_element()
            }
            Control::Sound => {
                let label = match current.first().map(String::as_str) {
                    None | Some("none") => rust_i18n::t!("settings.no_sound").into_owned(),
                    Some(name) => name.to_owned(),
                };
                dropdown(id("dropdown", key), key_title(key), None, label, 180., colors)
                    .on_press(cx, move |this, window, cx| this.open_sound_picker(key, window, cx))
                    .into_any_element()
            }
        };
        let reset = self.reset(key, colors, cx);
        row(key_title(key), Some(key_hint(key)), control, reset, error, colors)
    }

    fn open_language_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = std::iter::once(PickItem::new("", rust_i18n::t!("settings.follow_system").into_owned()))
            .chain(runode_config::i18n::available().into_iter().map(|locale| {
                PickItem::new(locale.to_string(), language_name(&locale)).with_detail(locale.to_string())
            }))
            .collect();
        let current = self.config.language.clone().unwrap_or_default();
        self.open_picker(key_title("language"), items, Some(current), PickTarget::Value("language"), window, cx);
    }

    fn open_sound_picker(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let items = std::iter::once(PickItem::new("none", rust_i18n::t!("settings.no_sound").into_owned()))
            .chain(crate::ui::sound::names().into_iter().map(|name| PickItem::new(name.clone(), name)))
            .collect();
        let current = self.config.values(key).into_iter().next();
        self.open_picker(key_title(key), items, current, PickTarget::Value(key), window, cx);
    }

    fn render_font_family(&mut self, colors: Colors, cx: &mut Context<Self>) -> Div {
        let fonts = self.config.font_family.clone();
        let count = fonts.len();
        let mut out = div().flex().flex_col();
        for (ix, name) in fonts.into_iter().enumerate() {
            let (title, hint, reset, error) = if ix == 0 {
                (
                    key_title("font-family"),
                    Some(key_hint("font-family")),
                    self.reset("font-family", colors, cx),
                    self.errors.get("font-family").cloned(),
                )
            } else {
                (rust_i18n::t!("settings.fallback_font", n = ix).into_owned(), None, None, None)
            };
            let current = name.clone();
            let picker = dropdown(("font", ix), title.clone(), None, name, 240., colors)
                .on_press(cx, move |this, window, cx| this.open_font_picker(ix, Some(current.clone()), window, cx));
            let remove =
                (count > 1).then(|| {
                    icon_button(("remove-font", ix), "icons/minus.svg", rust_i18n::t!("settings.remove"), colors)
                        .on_press(cx, move |this, _, cx| {
                            let mut fonts = this.config.font_family.clone();
                            fonts.remove(ix);
                            this.write_or_report("font-family", fonts, cx);
                        })
                });
            let control = div().flex().items_center().gap(px(4.)).children(remove).child(picker);
            out = out.child(row(title, hint, control, reset, error, colors));
        }
        let add = button("add-font", rust_i18n::t!("settings.add_fallback_font").into_owned(), colors)
            .on_press(cx, move |this, window, cx| this.open_font_picker(count, None, window, cx));
        out.child(div().py(px(10.)).flex().child(add))
    }

    fn open_font_picker(&mut self, ix: usize, current: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let mut names = cx.text_system().all_font_names();
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup();
        let items = names.into_iter().map(|name| PickItem::new(name.clone(), name)).collect();
        self.open_picker(key_title("font-family"), items, current, PickTarget::FontFamily(ix), window, cx);
    }

    fn render_palette(&mut self, colors: Colors, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let reset = self.reset("palette", colors, cx);
        let error = self.errors.get("palette").cloned();
        let header = row(key_title("palette"), Some(key_hint("palette")), div(), reset, error, colors);
        let cells: Vec<_> = (0..16u8)
            .map(|ix| {
                let id = format!("palette:{ix}");
                let input = self.field(&id, Commit::Palette(ix), None, window, cx);
                let color = self.config.palette.iter().rfind(|(i, _)| *i == ix).map(|(_, c)| hsla(*c));
                let error = self.errors.get(&id).cloned();
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .w(px(18.))
                                    .text_size(px(11.))
                                    .text_color(colors.fg.opacity(0.55))
                                    .child(ix.to_string()),
                            )
                            .child(swatch(color, 18., colors))
                            .child(input_box(input, Some(92.), error.is_some(), colors, window, cx)),
                    )
                    .children(error.map(|err| div().text_size(px(11.)).text_color(colors.error).child(err)))
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .child(header)
            .child(div().py(px(10.)).grid().grid_cols(4).gap_x(px(12.)).gap_y(px(8.)).children(cells))
    }

    /// 状态栏上不显示的几块，一块一个标签；写回时和状态栏的右键菜单一样写成一行，逗号隔开。
    fn render_status_bar_hidden(&mut self, colors: Colors, cx: &mut Context<Self>) -> Div {
        const KEY: &str = "status-bar-hidden";
        let hidden = self.config.status_bar_hidden.clone();
        let chips: Vec<_> = StatusItem::ALL
            .into_iter()
            .enumerate()
            .map(|(ix, item)| {
                let on = hidden.contains(&item);
                let (icon, title) = crate::window::status_item_icon_and_title(item);
                chip(("status-hidden", ix), Some(icon), title, on, colors).on_press(cx, move |this, _, cx| {
                    let mut hidden = this.config.status_bar_hidden.clone();
                    if on {
                        hidden.retain(|other| *other != item);
                    } else {
                        hidden.push(item);
                    }
                    let names: Vec<_> = hidden.iter().map(|item| item.name()).collect();
                    let values = if names.is_empty() { Vec::new() } else { vec![names.join(", ")] };
                    this.write_or_report(KEY, values, cx);
                })
            })
            .collect();
        let reset = self.reset(KEY, colors, cx);
        let error = self.errors.get(KEY).cloned();
        let control = div().flex().gap(px(6.)).children(chips);
        row(key_title(KEY), Some(key_hint(KEY)), control, reset, error, colors)
    }

    fn render_agent_exclude(&mut self, colors: Colors, cx: &mut Context<Self>) -> Div {
        const KEY: &str = "agent-notifications-exclude";
        let excluded = self.config.values(KEY);
        let chips: Vec<_> = AgentKind::ALL
            .into_iter()
            .chain([AgentKind::Other])
            .enumerate()
            .map(|(ix, kind)| {
                let label = kind.label();
                let on = excluded.iter().any(|name| name == label);
                let name = match kind {
                    AgentKind::Other => rust_i18n::t!("settings.other_agents").into_owned(),
                    kind => kind.display_name().to_owned(),
                };
                chip(("exclude", ix), None, name, on, colors).on_press(cx, move |this, _, cx| {
                    let mut values = this.config.values(KEY);
                    if on {
                        values.retain(|name| name != label);
                    } else {
                        values.push(label.to_owned());
                    }
                    this.write_or_report(KEY, values, cx);
                })
            })
            .collect();
        let reset = self.reset(KEY, colors, cx);
        let error = self.errors.get(KEY).cloned();
        div()
            .flex()
            .flex_col()
            .child(row(key_title(KEY), Some(key_hint(KEY)), div(), reset, error, colors))
            .child(div().py(px(10.)).flex().flex_wrap().gap(px(6.)).children(chips))
    }

    fn render_config_files(&mut self, colors: Colors, window: &mut Window, cx: &mut Context<Self>) -> Div {
        const KEY: &str = "config-file";
        let files = self.file.values(KEY);
        let reset = self.reset(KEY, colors, cx);
        let mut out =
            div().flex().flex_col().child(row(key_title(KEY), Some(key_hint(KEY)), div(), reset, None, colors));
        // 末尾总留一个空的，在里面打字就是加一行。
        let last = files.len();
        for ix in 0..=last {
            let id = format!("{KEY}:{ix}");
            let placeholder = rust_i18n::t!("settings.config_file_placeholder").into_owned();
            let input = self.field(&id, Commit::Item(KEY, ix), Some(placeholder.into()), window, cx);
            let error = self.errors.get(&id).cloned();
            let remove = (ix < files.len()).then(|| {
                icon_button(("remove-config-file", ix), "icons/minus.svg", rust_i18n::t!("settings.remove"), colors)
                    .on_press(cx, move |this, _, cx| {
                        let mut files = this.file.values(KEY);
                        files.remove(ix);
                        this.write_or_report(KEY, files, cx);
                    })
            });
            out = out.child(
                div()
                    .pt(px(4.))
                    .pb(px(if ix == last { 12. } else { 4. }))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(input_box(input, None, error.is_some(), colors, window, cx))
                            .child(div().w(px(20.)).children(remove)),
                    )
                    .children(error.map(|err| div().text_size(px(11.5)).text_color(colors.error).child(err))),
            );
        }
        out
    }

    fn render_config_file_actions(&mut self, colors: Colors, cx: &mut Context<Self>) -> Div {
        let path = runode_config::config_path().map(|path| display_dir(&path));
        let open = button("open-config", rust_i18n::t!("settings.open_config").into_owned(), colors)
            .on_press(cx, |_, _, cx| crate::config::open(cx));
        let reload = button("reload-config", rust_i18n::t!("settings.reload_config").into_owned(), colors)
            .on_press(cx, |_, _, cx| crate::config::reload(cx));
        let hint = rust_i18n::t!("settings.config_file_hint", path = path.unwrap_or_default()).into_owned();
        row(
            rust_i18n::t!("settings.config_file").into_owned(),
            Some(hint.into()),
            div().flex().gap(px(6.)).child(open).child(reload),
            None,
            None,
            colors,
        )
    }
}

/// 输入框空着时的提示：默认值，没有默认值的写「默认」。
fn key_placeholder(key: &str) -> SharedString {
    match runode_config::Config::default().values(key).into_iter().next() {
        Some(value) => value.into(),
        None => rust_i18n::t!("settings.default").into_owned().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 配置的每个键都在某一页上，而且只在一页上。
    #[test]
    fn every_key_has_a_place() {
        let mut keys: Vec<&str> = Page::ALL.into_iter().flat_map(Page::keys).collect();
        keys.sort();
        let mut all: Vec<&str> = runode_config::KEYS.iter().flat_map(|group| group.iter().copied()).collect();
        all.sort();
        assert_eq!(keys, all);
    }

    /// 每个键、每页、每个选项都有翻译。
    #[test]
    fn every_label_is_translated() {
        for locale in runode_config::i18n::available() {
            let t = |key: &str| rust_i18n::t!(key, locale = &locale).into_owned();
            let missing = |key: &str| {
                let text = t(key);
                assert!(!text.contains("settings."), "{locale} misses {key}");
            };
            for page in Page::ALL {
                missing(&format!("settings.page.{}", page.id()));
                for item in page.items() {
                    match item {
                        Item::Row(key, control) => {
                            missing(&format!("settings.key.{}", tr_key(key)));
                            missing(&format!("settings.hint.{}", tr_key(key)));
                            if let Control::Choice(values) = control {
                                for value in values.iter().filter(|value| !value.is_empty()) {
                                    missing(&format!("settings.choice.{}.{value}", tr_key(key)));
                                }
                            }
                        }
                        Item::Section(name) => missing(&format!("settings.section.{name}")),
                        _ => {}
                    }
                }
                for key in page.keys() {
                    missing(&format!("settings.key.{}", tr_key(key)));
                    missing(&format!("settings.hint.{}", tr_key(key)));
                }
            }
        }
    }
}
