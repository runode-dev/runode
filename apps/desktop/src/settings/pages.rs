//! 设置窗口的各页：每页有哪些项、各用什么控件，以及主题、字体、调色板这些不止一个控件的项。

use gpui::{AnyElement, Context, Div, ElementId, SharedString, Window, div, prelude::*, px, svg};
use runode_shared_types::agent::AgentKind;

use super::{
    Commit, SettingsView,
    controls::{
        Colors, button, dropdown, icon_button, input_box, on_click, reset_button, row, section, segmented, swatch,
        switch,
    },
    picker::{PickItem, PickTarget, ThemeSlot},
};
use crate::{i18n::tr, ui::hsla};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    General,
    Appearance,
    Colors,
    Terminal,
    Files,
    Agents,
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
    AgentExclude,
    ConfigFiles,
    ConfigFileActions,
    Pairing,
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
    pub const ALL: [Self; 8] = [
        Self::General,
        Self::Appearance,
        Self::Colors,
        Self::Terminal,
        Self::Files,
        Self::Agents,
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
            Self::Remote => "remote",
            Self::Keybinds => "keybinds",
        }
    }

    pub fn title(self) -> String {
        tr(&format!("settings.page.{}", self.id()))
    }

    fn items(self) -> &'static [Item] {
        use Control::*;
        use Item::*;
        match self {
            Self::General => &[
                Row("language", Language),
                Row("terminal-host", Switch),
                Row("auto-update", Switch),
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
            ],
            Self::Colors => &[
                Section("colors"),
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
                Section("palette"),
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
            Self::Remote => &[
                Row("remote-access", Switch),
                Row("remote-access-port", Text(80.)),
                Row("remote-access-name", Text(200.)),
                Section("pairing"),
                Pairing,
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
                Item::AgentExclude => Some("agent-notifications-exclude"),
                Item::ConfigFiles => Some("config-file"),
                Item::Section(_) | Item::ConfigFileActions | Item::Pairing => None,
            })
            .collect()
    }
}

/// 翻译里键名用下划线。
fn tr_key(key: &str) -> String {
    key.replace('-', "_")
}

fn key_title(key: &str) -> String {
    tr(&format!("settings.key.{}", tr_key(key)))
}

fn key_hint(key: &str) -> SharedString {
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

/// `theme` 的值拆开：不用主题、一个主题，或者浅色、深色外观各一个。
#[derive(Debug, PartialEq)]
pub(super) enum ThemeChoice {
    None,
    Single(String),
    Pair { light: String, dark: String },
}

impl ThemeChoice {
    pub fn parse(value: Option<&str>) -> Self {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Self::None;
        };
        let side = |want: &str| {
            value.split(',').map(str::trim).find_map(|part| part.strip_prefix(want)).map(|name| name.trim().to_owned())
        };
        match (side("light:"), side("dark:")) {
            (None, None) => Self::Single(value.to_owned()),
            (light, dark) => {
                let light = light.or_else(|| dark.clone()).unwrap_or_default();
                let dark = dark.unwrap_or_else(|| light.clone());
                Self::Pair { light, dark }
            }
        }
    }

    pub fn value(&self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Single(name) => Some(name.clone()),
            Self::Pair { light, dark } => Some(format!("light:{light},dark:{dark}")),
        }
    }

    /// 选了 `slot` 用 `name` 之后的样子。
    pub fn with(&self, slot: ThemeSlot, name: String) -> Self {
        let (light, dark) = match self {
            Self::None => (name.clone(), name.clone()),
            Self::Single(current) => (current.clone(), current.clone()),
            Self::Pair { light, dark } => (light.clone(), dark.clone()),
        };
        match slot {
            ThemeSlot::Single => Self::Single(name),
            ThemeSlot::Light => Self::Pair { light: name, dark },
            ThemeSlot::Dark => Self::Pair { light, dark: name },
        }
    }
}

impl SettingsView {
    pub(super) fn render_page(&mut self, colors: Colors, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let title = div()
            .pt(px(4.))
            .pb(px(8.))
            .text_size(px(20.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(self.page.title());
        let mut page = div().flex().flex_col().child(title);
        if self.page == Page::Keybinds {
            return page.child(self.render_keybinds(colors, window, cx));
        }
        for item in self.page.items() {
            let element = match *item {
                Item::Section(name) => section(tr(&format!("settings.section.{name}")), colors).into_any_element(),
                Item::Row(key, control) => self.render_row(key, control, colors, window, cx).into_any_element(),
                Item::Theme => self.render_theme(colors, cx).into_any_element(),
                Item::FontFamily => self.render_font_family(colors, cx).into_any_element(),
                Item::Palette => self.render_palette(colors, window, cx).into_any_element(),
                Item::AgentExclude => self.render_agent_exclude(colors, cx).into_any_element(),
                Item::ConfigFiles => self.render_config_files(colors, window, cx).into_any_element(),
                Item::ConfigFileActions => self.render_config_file_actions(colors, cx).into_any_element(),
                Item::Pairing => self.render_pairing(colors, cx).into_any_element(),
            };
            page = page.child(element);
        }
        page
    }

    /// runode 的配置文件里写了 `key` 时的恢复按钮。
    fn reset(&self, key: &'static str, colors: Colors, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.file.has(key).then(|| {
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
        let error = self.errors.get(key).cloned();
        let control = match control {
            Control::Switch => {
                let on = current.first().is_some_and(|value| value == "true");
                switch(id("switch", key), on, colors)
                    .on_click(on_click(cx, move |this, _, cx| this.write_or_report(key, vec![(!on).to_string()], cx)))
                    .into_any_element()
            }
            Control::Choice(values) => {
                let options = values.iter().map(|value| (value.to_string(), choice_label(key, value))).collect();
                let selected = current.first().map_or("", String::as_str);
                segmented(key, options, Some(selected), colors, cx, move |this, value, _, cx| {
                    let values = if value.is_empty() { Vec::new() } else { vec![value.to_owned()] };
                    this.write_or_report(key, values, cx);
                })
                .into_any_element()
            }
            Control::Text(width) => {
                let input = self.field(key, Commit::Value(key), Some(key_placeholder(key)), window, cx);
                input_box(input, width, self.errors.contains_key(key), colors).into_any_element()
            }
            Control::Color => {
                let color = current.first().and_then(|value| runode_config::color::parse(value)).map(hsla);
                let input = self.field(key, Commit::Value(key), Some(key_placeholder(key)), window, cx);
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(swatch(color, 20., colors))
                    .child(input_box(input, 130., self.errors.contains_key(key), colors))
                    .into_any_element()
            }
            Control::Language => {
                let label = match &self.config.language {
                    Some(locale) => language_name(locale),
                    None => rust_i18n::t!("settings.follow_system").into_owned(),
                };
                dropdown(id("dropdown", key), label, 180., colors)
                    .on_click(on_click(cx, move |this, window, cx| this.open_language_picker(window, cx)))
                    .into_any_element()
            }
            Control::Sound => {
                let label = match current.first().map(String::as_str) {
                    None | Some("none") => rust_i18n::t!("settings.no_sound").into_owned(),
                    Some(name) => name.to_owned(),
                };
                dropdown(id("dropdown", key), label, 180., colors)
                    .on_click(on_click(cx, move |this, window, cx| this.open_sound_picker(key, window, cx)))
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

    fn open_theme_picker(
        &mut self,
        slot: ThemeSlot,
        current: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = runode_config::theme_names().into_iter().map(|name| PickItem::new(name.clone(), name)).collect();
        let title = rust_i18n::t!(match slot {
            ThemeSlot::Single => "settings.key.theme",
            ThemeSlot::Light => "settings.theme.light",
            ThemeSlot::Dark => "settings.theme.dark",
        })
        .into_owned();
        self.open_picker(title, items, current, PickTarget::Theme(slot), window, cx);
    }

    /// 选好了主题。
    pub(super) fn set_theme(&mut self, slot: ThemeSlot, name: String, cx: &mut Context<Self>) {
        let theme = ThemeChoice::parse(self.config.theme.as_deref()).with(slot, name);
        self.write_or_report("theme", theme.value().into_iter().collect(), cx);
    }

    fn render_theme(&mut self, colors: Colors, cx: &mut Context<Self>) -> Div {
        let theme = ThemeChoice::parse(self.config.theme.as_deref());
        let mode = match theme {
            ThemeChoice::None => "none",
            ThemeChoice::Single(_) => "single",
            ThemeChoice::Pair { .. } => "pair",
        };
        let options = ["none", "single", "pair"]
            .into_iter()
            .map(|value| (value.to_owned(), tr(&format!("settings.theme.{value}")).into()))
            .collect();
        let modes = segmented("theme-mode", options, Some(mode), colors, cx, |this, value, window, cx| {
            let theme = ThemeChoice::parse(this.config.theme.as_deref());
            match (value, theme) {
                ("none", _) => this.write_or_report("theme", Vec::new(), cx),
                ("single", ThemeChoice::Pair { dark, .. }) => this.write_or_report("theme", vec![dark], cx),
                ("single", ThemeChoice::None) => this.open_theme_picker(ThemeSlot::Single, None, window, cx),
                ("pair", ThemeChoice::Single(name)) => {
                    this.write_or_report("theme", vec![format!("light:{name},dark:{name}")], cx)
                }
                ("pair", ThemeChoice::None) => this.open_theme_picker(ThemeSlot::Dark, None, window, cx),
                _ => {}
            }
        });
        let reset = self.reset("theme", colors, cx);
        let error = self.errors.get("theme").cloned();
        let mut out = div().flex().flex_col().child(row(
            key_title("theme"),
            Some(key_hint("theme")),
            modes,
            reset,
            error,
            colors,
        ));
        let slots = match theme {
            ThemeChoice::None => Vec::new(),
            ThemeChoice::Single(name) => vec![(ThemeSlot::Single, name)],
            ThemeChoice::Pair { light, dark } => vec![(ThemeSlot::Light, light), (ThemeSlot::Dark, dark)],
        };
        for (slot, name) in slots {
            let (title, element) = match slot {
                ThemeSlot::Single => ("settings.theme.name", "theme-single"),
                ThemeSlot::Light => ("settings.theme.light", "theme-light"),
                ThemeSlot::Dark => ("settings.theme.dark", "theme-dark"),
            };
            let current = name.clone();
            let picker = dropdown(element, name, 240., colors).on_click(on_click(cx, move |this, window, cx| {
                this.open_theme_picker(slot, Some(current.clone()), window, cx)
            }));
            out = out.child(row(rust_i18n::t!(title).into_owned(), None, picker, None, None, colors));
        }
        out
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
            let picker = dropdown(("font", ix), name, 240., colors).on_click(on_click(cx, move |this, window, cx| {
                this.open_font_picker(ix, Some(current.clone()), window, cx)
            }));
            let remove = (count > 1).then(|| {
                icon_button(("remove-font", ix), "icons/minus.svg", colors).on_click(on_click(
                    cx,
                    move |this, _, cx| {
                        let mut fonts = this.config.font_family.clone();
                        fonts.remove(ix);
                        this.write_or_report("font-family", fonts, cx);
                    },
                ))
            });
            let control =
                div().flex().items_center().gap(px(4.)).child(picker).child(div().w(px(20.)).children(remove));
            out = out.child(row(title, hint, control, reset, error, colors));
        }
        let add = button("add-font", rust_i18n::t!("settings.add_fallback_font").into_owned(), colors)
            .on_click(on_click(cx, move |this, window, cx| this.open_font_picker(count, None, window, cx)));
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
                            .child(input_box(input, 92., error.is_some(), colors)),
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
                div()
                    .id(("exclude", ix))
                    .h(px(24.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if on { colors.accent } else { colors.border })
                    .text_size(px(12.))
                    .when(on, |chip| chip.bg(colors.accent.opacity(0.15)))
                    .hover(|chip| chip.bg(colors.hover))
                    .children(on.then(|| svg().path("icons/check.svg").size(px(11.)).text_color(colors.accent)))
                    .child(name)
                    .on_click(on_click(cx, move |this, _, cx| {
                        let mut values = this.config.values(KEY);
                        if on {
                            values.retain(|name| name != label);
                        } else {
                            values.push(label.to_owned());
                        }
                        this.write_or_report(KEY, values, cx);
                    }))
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
        for ix in 0..=files.len() {
            let id = format!("{KEY}:{ix}");
            let placeholder = rust_i18n::t!("settings.config_file_placeholder").into_owned();
            let input = self.field(&id, Commit::Item(KEY, ix), Some(placeholder.into()), window, cx);
            let error = self.errors.get(&id).cloned();
            let remove = (ix < files.len()).then(|| {
                icon_button(("remove-config-file", ix), "icons/minus.svg", colors).on_click(on_click(
                    cx,
                    move |this, _, cx| {
                        let mut files = this.file.values(KEY);
                        files.remove(ix);
                        this.write_or_report(KEY, files, cx);
                    },
                ))
            });
            out = out.child(
                div()
                    .py(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(input_box(input, 420., error.is_some(), colors))
                            .child(div().w(px(20.)).children(remove)),
                    )
                    .children(error.map(|err| div().text_size(px(11.5)).text_color(colors.error).child(err))),
            );
        }
        out
    }

    fn render_config_file_actions(&mut self, colors: Colors, cx: &mut Context<Self>) -> Div {
        let path = runode_config::config_path().map(|path| {
            match runode_paths::Dirs::from_env()
                .home
                .and_then(|home| path.strip_prefix(home).ok().map(|rest| rest.to_owned()))
            {
                Some(rest) => format!("~/{}", rest.display()),
                None => path.display().to_string(),
            }
        });
        let open = button("open-config", rust_i18n::t!("settings.open_config").into_owned(), colors)
            .on_click(on_click(cx, |_, _, cx| crate::config::open(cx)));
        let reload = button("reload-config", rust_i18n::t!("settings.reload_config").into_owned(), colors)
            .on_click(on_click(cx, |_, _, cx| crate::config::reload(cx)));
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

    #[test]
    fn theme_value_splits_and_joins() {
        assert_eq!(ThemeChoice::parse(None), ThemeChoice::None);
        assert_eq!(ThemeChoice::parse(Some("Dracula")), ThemeChoice::Single("Dracula".into()));
        let pair = ThemeChoice::parse(Some("light:Day, dark:Night"));
        assert_eq!(pair, ThemeChoice::Pair { light: "Day".into(), dark: "Night".into() });
        assert_eq!(pair.value().as_deref(), Some("light:Day,dark:Night"));
        assert_eq!(pair.with(ThemeSlot::Light, "Dawn".into()).value().as_deref(), Some("light:Dawn,dark:Night"));
        assert_eq!(
            ThemeChoice::Single("A".into()).with(ThemeSlot::Dark, "B".into()).value().as_deref(),
            Some("light:A,dark:B")
        );
        assert_eq!(ThemeChoice::None.with(ThemeSlot::Single, "C".into()).value().as_deref(), Some("C"));
        // 只写了一边的，另一边用同一个。
        assert_eq!(ThemeChoice::parse(Some("dark:X")), ThemeChoice::Pair { light: "X".into(), dark: "X".into() });
    }
}
