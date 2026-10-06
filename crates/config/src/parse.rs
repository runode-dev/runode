//! 读配置文件：先读 Ghostty 的配置，再读 runode 自己的，逐行解析 `key = value` 并按层套用。

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use runode_shared_types::{
    agent::AgentKind,
    color::{Rgb, TerminalColor},
    settings::{CursorStyle, MIN_SCROLLBACK_LIMIT, OptionAsAlt},
    shell::{IntegrationMode, Shell},
};

use crate::{
    CellHeight, Config, PreviewClick, color,
    theme::{Theme, find_theme, pick_theme},
};

/// runode 认的全部键，按配置模板里的顺序分组，组与组之间在模板里空一行。`apply` 只处理
/// 这里列出的键，模板也按这张表逐个写出，所以新增配置项只要在这里加上键名、在 `apply` 里
/// 解析、在 `template_values` 里给出默认值、在翻译里写好说明。`theme` 和 `config-file`
/// 在应用各层之前就已处理，列在这里是为了写进模板。
pub(crate) const KEYS: &[&[&str]] = &[
    &["language"],
    &["font-family", "font-size", "adjust-cell-height", "window-padding-x", "window-padding-y"],
    &["file-tree-font-size", "file-tree-preview-click", "preview-font-size"],
    &[
        "theme",
        "background",
        "foreground",
        "cursor-color",
        "cursor-text",
        "selection-background",
        "selection-foreground",
        "search-background",
        "search-foreground",
        "search-selected-background",
        "search-selected-foreground",
        "palette",
    ],
    &[
        "cursor-style",
        "cursor-style-blink",
        "macos-option-as-alt",
        "scrollback-limit",
        "shell-integration",
        "command-suggestions",
        "command-completions",
        "command-highlighting",
    ],
    &["terminal-host"],
    &["remote-access", "remote-access-port"],
    &["agent-notifications", "agent-notifications-exclude", "agent-done-sound", "agent-blocked-sound"],
    &["config-file"],
    &["keybind"],
];

/// 一条 `key = value`，带出处以便报错。
pub(crate) struct Entry {
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) origin: String,
}

impl Config {
    /// 读取 Ghostty 与 runode 的配置。`dark` 用于 `theme = light:X,dark:Y`。
    pub fn load(dark: bool) -> Self {
        let mut sources = Vec::new();
        // Ghostty 配置里的 keybind 针对的是另一套默认键位和动作，照搬过来含义会变，不读。
        let ghostty: Vec<Entry> = ghostty_config_paths()
            .iter()
            .flat_map(|path| read_entries(path, &mut sources))
            .filter(|e| e.key != "keybind")
            .collect();
        let runode = config_path().map(|path| read_entries(&path, &mut sources)).unwrap_or_default();
        Self::from_layers(&[ghostty, runode], dark, &mut sources)
    }

    /// 这份配置拿到系统外观为 `dark` 时还能不能用，也就是按 `dark` 重新加载会不会得到同样的结果
    /// （`dark` 这一项除外）。
    pub fn fits_appearance(&self, dark: bool) -> bool {
        self.dark == dark || !self.theme_follows_appearance
    }

    /// 所有可能的配置文件，供监视它们是否变化：Ghostty 和 runode 的配置文件（还不存在的也算在内，
    /// 新建配置文件同样要重载），以及上次加载时读到的主题和引入的文件。
    pub fn watch_paths(&self) -> Vec<PathBuf> {
        let mut paths = ghostty_config_paths();
        paths.extend(config_path());
        paths.extend(self.sources.iter().cloned());
        paths.dedup();
        paths
    }

    fn from_layers(layers: &[Vec<Entry>], dark: bool, sources: &mut Vec<PathBuf>) -> Self {
        let mut config = Self::default();
        // 先套主题，再让显式写出的键覆盖它。后一层的主题覆盖前一层的。
        let theme = layers.iter().flatten().rfind(|e| e.key == "theme").map(|e| e.value.clone());
        if let Some(theme) = theme.filter(|t| !t.is_empty()) {
            config.theme_follows_appearance = pick_theme(&theme, true) != pick_theme(&theme, false);
            let name = pick_theme(&theme, dark);
            match find_theme(&name) {
                Some(Theme::File(path)) => config.apply_layer(&read_entries(&path, sources)),
                Some(Theme::Bundled(text)) => config.apply_layer(&parse_entries(text, &name)),
                None => tracing::warn!("theme not found: {name}"),
            }
        }
        for layer in layers {
            config.apply_layer(layer);
        }
        config.sources = std::mem::take(sources);
        config.dark = dark;
        config
    }

    /// 应用一层配置。可重复的 `font-family` 在每层第一次出现时清空，
    /// 所以 runode 设了字体就整列替换 Ghostty 的。
    pub(crate) fn apply_layer(&mut self, entries: &[Entry]) {
        let mut seen = HashSet::new();
        for entry in entries {
            let first = seen.insert(entry.key.as_str());
            if let Err(err) = self.apply(&entry.key, &entry.value, first) {
                tracing::warn!("{}: {} = {}: {err}", entry.origin, entry.key, entry.value);
            }
        }
    }

    pub(crate) fn apply(&mut self, key: &str, value: &str, first_in_layer: bool) -> Result<(), String> {
        // 不在 `KEYS` 里的是 runode 用不上的 Ghostty 键。
        if !KEYS.iter().any(|group| group.contains(&key)) {
            return Ok(());
        }
        let defaults = Self::default();
        // 值为空表示恢复默认。
        let empty = value.is_empty();
        match key {
            "font-family" => {
                if first_in_layer || empty {
                    self.font_family.clear();
                }
                if empty {
                    self.font_family = defaults.font_family;
                } else {
                    self.font_family.push(value.to_owned());
                }
            }
            "font-size" => {
                self.font_size = if empty { defaults.font_size } else { parse_f32(value)? };
            }
            "adjust-cell-height" => {
                self.adjust_cell_height = if empty {
                    None
                } else if let Some(percent) = value.strip_suffix('%') {
                    Some(CellHeight::Percent(parse_f32(percent)?))
                } else {
                    Some(CellHeight::Pixels(parse_f32(value)?))
                };
            }
            "window-padding-x" => {
                self.window_padding_x = if empty { defaults.window_padding_x } else { parse_pair(value)? };
            }
            "window-padding-y" => {
                self.window_padding_y = if empty { defaults.window_padding_y } else { parse_pair(value)? };
            }
            "file-tree-font-size" => {
                self.file_tree_font_size = if empty { defaults.file_tree_font_size } else { parse_positive(value)? };
            }
            "preview-font-size" => {
                self.preview_font_size = if empty { defaults.preview_font_size } else { parse_positive(value)? };
            }
            "file-tree-preview-click" => {
                self.file_tree_preview_click = match value {
                    "" | "single" => PreviewClick::Single,
                    "double" => PreviewClick::Double,
                    _ => return Err("expected single or double".into()),
                };
            }
            "cursor-style" => {
                self.cursor_style = match value {
                    "" | "block" => CursorStyle::Block,
                    "bar" => CursorStyle::Bar,
                    "underline" => CursorStyle::Underline,
                    "block_hollow" => CursorStyle::BlockHollow,
                    _ => return Err("expected block, bar, underline or block_hollow".into()),
                };
            }
            "cursor-style-blink" => {
                self.cursor_style_blink = if empty { None } else { Some(parse_bool(value)?) };
            }
            "background" => {
                self.background = if empty { defaults.background } else { parse_color(value)? };
            }
            "foreground" => {
                self.foreground = if empty { defaults.foreground } else { parse_color(value)? };
            }
            "cursor-color" => {
                self.cursor_color = if empty { defaults.cursor_color } else { Some(parse_terminal_color(value)?) };
            }
            "cursor-text" => {
                self.cursor_text = if empty { None } else { Some(parse_terminal_color(value)?) };
            }
            "selection-background" => {
                self.selection_background = if empty { None } else { Some(parse_terminal_color(value)?) };
            }
            "selection-foreground" => {
                self.selection_foreground = if empty { None } else { Some(parse_terminal_color(value)?) };
            }
            "search-background" => {
                self.search_background = if empty { defaults.search_background } else { parse_terminal_color(value)? };
            }
            "search-foreground" => {
                self.search_foreground = if empty { defaults.search_foreground } else { parse_terminal_color(value)? };
            }
            "search-selected-background" => {
                self.search_selected_background =
                    if empty { defaults.search_selected_background } else { parse_terminal_color(value)? };
            }
            "search-selected-foreground" => {
                self.search_selected_foreground =
                    if empty { defaults.search_selected_foreground } else { parse_terminal_color(value)? };
            }
            "palette" => {
                let (index, color) = value.split_once('=').ok_or("expected N=COLOR")?;
                let index: u8 = index.trim().parse().map_err(|_| "palette index must be 0-255")?;
                let color = parse_color(color)?;
                self.palette.retain(|(i, _)| *i != index);
                self.palette.push((index, color));
            }
            "scrollback-limit" => {
                self.scrollback_limit = if empty {
                    defaults.scrollback_limit
                } else {
                    value.parse::<usize>().map_err(|_| "expected a number of bytes")?.max(MIN_SCROLLBACK_LIMIT)
                };
            }
            "shell-integration" => {
                self.shell_integration = match value {
                    "" | "detect" => IntegrationMode::Detect,
                    // elvish 和 nushell 还没有集成脚本，和 none 一样不启用。
                    "none" | "elvish" | "nushell" => IntegrationMode::Off,
                    name => IntegrationMode::Force(
                        Shell::from_name(name).ok_or("expected none, detect, bash, zsh or fish")?,
                    ),
                };
            }
            "command-suggestions" => {
                self.command_suggestions = if empty { defaults.command_suggestions } else { parse_bool(value)? };
            }
            "command-completions" => {
                self.command_completions = if empty { defaults.command_completions } else { parse_bool(value)? };
            }
            "command-highlighting" => {
                self.command_highlighting = if empty { defaults.command_highlighting } else { parse_bool(value)? };
            }
            "terminal-host" => {
                self.terminal_host = if empty { defaults.terminal_host } else { parse_bool(value)? };
            }
            "remote-access" => {
                self.remote_access = if empty { defaults.remote_access } else { parse_bool(value)? };
            }
            "remote-access-port" => {
                self.remote_access_port = if empty {
                    defaults.remote_access_port
                } else {
                    value.trim().parse().ok().filter(|&port| port > 0).ok_or("expected a port from 1 to 65535")?
                };
            }
            "agent-notifications" => {
                self.agent_notifications = if empty { defaults.agent_notifications } else { parse_bool(value)? };
            }
            "agent-notifications-exclude" => {
                // 值为空时清空；可以写多行，也可以一行用逗号隔开几个。
                if empty {
                    self.agent_notifications_exclude.clear();
                } else {
                    for kind in parse_agents(value)? {
                        if !self.agent_notifications_exclude.contains(&kind) {
                            self.agent_notifications_exclude.push(kind);
                        }
                    }
                }
            }
            "agent-done-sound" => {
                self.agent_done_sound = if empty { defaults.agent_done_sound } else { parse_sound(value) };
            }
            "agent-blocked-sound" => {
                self.agent_blocked_sound = if empty { defaults.agent_blocked_sound } else { parse_sound(value) };
            }
            "macos-option-as-alt" => {
                self.macos_option_as_alt = match value {
                    "" | "false" => OptionAsAlt::False,
                    "true" => OptionAsAlt::True,
                    "left" => OptionAsAlt::Left,
                    "right" => OptionAsAlt::Right,
                    _ => return Err("expected true, false, left or right".into()),
                };
            }
            "language" => match value {
                "" | "system" => self.language = None,
                tag => match crate::i18n::resolve(tag) {
                    Some(locale) => self.language = Some(locale),
                    // 没有翻译的语言用英文，同时报出来，免得写错了不知道。
                    None => {
                        self.language = Some(crate::i18n::FALLBACK.to_owned());
                        return Err(format!("no translation, using {}", crate::i18n::FALLBACK));
                    }
                },
            },
            "keybind" => {
                // 值为空时去掉此前写的 keybind，回到默认快捷键。
                if empty {
                    self.keybinds.clear();
                } else {
                    self.keybinds.push(crate::keybind::parse(value)?);
                }
            }
            // 主题和 `config-file` 在应用各层之前已处理。
            "theme" | "config-file" => {}
            // 列进了 `KEYS` 却没在这里解析；测试 `apply_handles_every_key` 会发现。
            _ => return Err(format!("{key} is listed in KEYS but not handled")),
        }
        Ok(())
    }
}

fn parse_f32(value: &str) -> Result<f32, String> {
    value.trim().parse().map_err(|_| "expected a number".into())
}

/// 大于 0 的数。
fn parse_positive(value: &str) -> Result<f32, String> {
    Some(parse_f32(value)?).filter(|n| *n > 0.).ok_or_else(|| "expected a positive number".into())
}

/// `N` 或 `A,B`，前者两边相同。
fn parse_pair(value: &str) -> Result<(f32, f32), String> {
    match value.split_once(',') {
        Some((a, b)) => Ok((parse_f32(a)?, parse_f32(b)?)),
        None => parse_f32(value).map(|v| (v, v)),
    }
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("expected true or false".into()),
    }
}

/// 逗号隔开的 agent 短名（`AgentKind::label`），`other` 是其他报告进度的程序。整行有一个
/// 认不出就整行不算，免得写错了不知道。
fn parse_agents(value: &str) -> Result<Vec<AgentKind>, String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            AgentKind::ALL
                .into_iter()
                .chain([AgentKind::Other])
                .find(|kind| kind.label() == name)
                .ok_or_else(|| format!("unknown agent: {name}"))
        })
        .collect()
}

/// 系统声音名；`none` 表示不出声。声音名在播放时才查得到有没有，这里不校验。
fn parse_sound(value: &str) -> Option<String> {
    (value != "none").then(|| value.to_owned())
}

fn parse_color(value: &str) -> Result<Rgb, String> {
    color::parse(value).ok_or_else(|| "not a color".into())
}

fn parse_terminal_color(value: &str) -> Result<TerminalColor, String> {
    match value {
        "cell-foreground" => Ok(TerminalColor::CellForeground),
        "cell-background" => Ok(TerminalColor::CellBackground),
        _ => parse_color(value)
            .map(TerminalColor::Rgb)
            .map_err(|_| "expected a color, cell-foreground or cell-background".into()),
    }
}

/// 读一个配置文件及其 `config-file` 引入的文件，按出现顺序返回条目。
/// 读到的文件记入 `sources`，同一文件只读一次以防循环引入。
fn read_entries(path: &Path, sources: &mut Vec<PathBuf>) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    if sources.iter().any(|p| p == path) {
        return Vec::new();
    }
    sources.push(path.to_owned());

    let mut entries = Vec::new();
    let mut includes = Vec::new();
    for entry in parse_entries(&text, &path.display().to_string()) {
        if entry.key == "config-file" {
            // `?` 前缀表示文件可以不存在；引入的文件在本文件之后处理。
            let (optional, file) = match entry.value.strip_prefix('?') {
                Some(file) => (true, file),
                None => (false, entry.value.as_str()),
            };
            let file = runode_paths::Dirs::from_env().expand_home(file);
            let file = path.parent().map_or(file.clone(), |dir| dir.join(&file));
            if !optional && !file.exists() {
                tracing::warn!("{}: config-file not found: {}", entry.origin, file.display());
            }
            includes.push(file);
            continue;
        }
        entries.push(entry);
    }
    for include in includes {
        entries.extend(read_entries(&include, sources));
    }
    entries
}

/// 逐行解析 `key = value`，跳过空行和注释。`name` 是出处，报错时带上行号。
pub(crate) fn parse_entries(text: &str, name: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        let value = value.trim();
        let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
        entries.push(Entry {
            key: key.trim().to_owned(),
            value: value.to_owned(),
            origin: format!("{name}:{}", n + 1),
        });
    }
    entries
}

/// runode 自己的配置文件；没有家目录时为 `None`。
pub fn config_path() -> Option<PathBuf> {
    runode_paths::Dirs::from_env().config_file()
}

/// Ghostty 配置文件的位置，按 Ghostty 的加载顺序。
fn ghostty_config_paths() -> Vec<PathBuf> {
    runode_paths::Dirs::from_env().ghostty_config_files()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn entries(text: &str) -> Vec<Entry> {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("runode-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = dir.join(n.to_string());
        std::fs::write(&path, text).unwrap();
        read_entries(&path, &mut Vec::new())
    }

    pub(crate) fn load(layers: &[&str]) -> Config {
        let layers: Vec<_> = layers.iter().map(|t| entries(t)).collect();
        Config::from_layers(&layers, true, &mut Vec::new())
    }

    #[test]
    fn apply_handles_every_key() {
        for key in KEYS.iter().flat_map(|group| group.iter()) {
            let result = Config::default().apply(key, "", true);
            assert!(!result.is_err_and(|err| err.contains("not handled")), "apply does not handle {key}");
        }
    }

    #[test]
    fn parses_ghostty_syntax() {
        let config = load(&[r#"
# 注释
font-family = "Hack Nerd Font Mono"
font-family = Menlo
font-size = 16
adjust-cell-height = 10%
window-padding-x = 4,8
window-padding-y = 3
cursor-style = bar
cursor-style-blink = false
background = #102030
foreground = white
palette = 1=#ff0000
macos-option-as-alt = left
scrollback-limit = 10000000
unknown-key = whatever
"#]);
        assert_eq!(config.font_family, ["Hack Nerd Font Mono", "Menlo"]);
        assert_eq!(config.font_size, 16.);
        assert_eq!(config.adjust_cell_height, Some(CellHeight::Percent(10.)));
        assert_eq!(config.window_padding_x, (4., 8.));
        assert_eq!(config.window_padding_y, (3., 3.));
        assert_eq!(config.cursor_style, CursorStyle::Bar);
        assert_eq!(config.cursor_style_blink, Some(false));
        assert_eq!(config.background, Rgb(0x10, 0x20, 0x30));
        assert_eq!(config.foreground, Rgb(255, 255, 255));
        assert!(config.palette.contains(&(1, Rgb(255, 0, 0))));
        assert_eq!(config.macos_option_as_alt, OptionAsAlt::Left);
        assert_eq!(config.scrollback_limit, 10_000_000);
    }

    /// 没写或写空时交给终端的是 `None`（不闪烁），只有显式写 true 才闪；终端那边怎么解释见
    /// `runode_terminal` 的测试 `cursor_is_steady_unless_configured_or_requested`。
    #[test]
    fn cursor_blinks_only_when_configured() {
        assert_eq!(Config::default().term_settings().cursor_blink, None);
        assert_eq!(load(&[""]).term_settings().cursor_blink, None);
        assert_eq!(load(&["cursor-style-blink = true\n"]).term_settings().cursor_blink, Some(true));
        assert_eq!(load(&["cursor-style-blink = false\n"]).term_settings().cursor_blink, Some(false));
        // Ghostty 那层开了、runode 这层写空：回到默认的不闪烁。
        assert_eq!(load(&["cursor-style-blink = true\n", "cursor-style-blink =\n"]).term_settings().cursor_blink, None);
    }

    #[test]
    fn scrollback_limit_has_a_floor() {
        let config = load(&["scrollback-limit = 0\n"]);
        assert_eq!(config.scrollback_limit, MIN_SCROLLBACK_LIMIT);
    }

    #[test]
    fn runode_overrides_ghostty_key_by_key() {
        let config = load(&[
            "font-family = A\nfont-family = B\nfont-size = 16\npalette = 2=#000001\npalette = 3=#000002",
            "font-family = C\npalette = 3=#000003",
        ]);
        // runode 设了字体就整列替换，没设的键保留 Ghostty 的值。
        assert_eq!(config.font_family, ["C"]);
        assert_eq!(config.font_size, 16.);
        assert!(config.palette.contains(&(2, Rgb(0, 0, 1))));
        assert!(config.palette.contains(&(3, Rgb(0, 0, 3))));
        assert!(!config.palette.contains(&(3, Rgb(0, 0, 2))));
    }

    #[test]
    fn empty_value_resets_to_default_and_bad_values_are_skipped() {
        let config = load(&["font-size = 20\nfont-size =\ncursor-style = triangle"]);
        assert_eq!(config.font_size, Config::default().font_size);
        assert_eq!(config.cursor_style, CursorStyle::Block);
    }

    #[test]
    fn file_tree_font_size_takes_positive_numbers() {
        assert_eq!(Config::default().file_tree_font_size, 13.);
        assert_eq!(load(&["file-tree-font-size = 15"]).file_tree_font_size, 15.);
        // 不是正数的跳过，保留前面的值。
        assert_eq!(load(&["file-tree-font-size = 15\nfile-tree-font-size = 0"]).file_tree_font_size, 15.);
        assert_eq!(load(&["file-tree-font-size = -3"]).file_tree_font_size, 13.);
    }

    #[test]
    fn preview_font_size_takes_positive_numbers() {
        assert_eq!(Config::default().preview_font_size, 13.);
        assert_eq!(load(&["preview-font-size = 12.5"]).preview_font_size, 12.5);
        assert_eq!(load(&["preview-font-size = 15\npreview-font-size = 0"]).preview_font_size, 15.);
        assert_eq!(load(&["preview-font-size = 15\npreview-font-size ="]).preview_font_size, 13.);
    }

    #[test]
    fn remote_access_is_off_by_default_and_takes_a_port() {
        let d = Config::default();
        assert_eq!((d.remote_access, d.remote_access_port), (false, 7866));
        let config = load(&["remote-access = true\nremote-access-port = 9000"]);
        assert_eq!((config.remote_access, config.remote_access_port), (true, 9000));
        // 不是端口的跳过，保留前面的值；值为空回到默认。
        for bad in ["0", "65536", "port"] {
            assert_eq!(
                load(&[&format!("remote-access-port = 9000\nremote-access-port = {bad}")]).remote_access_port,
                9000
            );
        }
        assert_eq!(load(&["remote-access-port = 9000\nremote-access-port ="]).remote_access_port, 7866);
    }

    #[test]
    fn file_tree_preview_click_defaults_to_single() {
        assert_eq!(Config::default().file_tree_preview_click, PreviewClick::Single);
        assert_eq!(load(&["file-tree-preview-click = double"]).file_tree_preview_click, PreviewClick::Double);
        assert_eq!(
            load(&["file-tree-preview-click = double\nfile-tree-preview-click ="]).file_tree_preview_click,
            PreviewClick::Single
        );
        // 认不出的值跳过，保留前面的值。
        assert_eq!(
            load(&["file-tree-preview-click = double\nfile-tree-preview-click = triple"]).file_tree_preview_click,
            PreviewClick::Double
        );
    }

    #[test]
    fn agent_alerts_can_be_turned_off() {
        let d = Config::default();
        assert!(d.agent_notifications);
        assert_eq!(d.agent_done_sound.as_deref(), Some("Glass"));
        assert_eq!(d.agent_blocked_sound.as_deref(), Some("Ping"));
        let config = load(&["agent-notifications = false\nagent-done-sound = none\nagent-blocked-sound = Funk\n\
             agent-notifications-exclude = codex, gemini\nagent-notifications-exclude = claude,codex"]);
        assert!(!config.agent_notifications);
        assert_eq!(config.agent_done_sound, None);
        assert_eq!(config.agent_blocked_sound.as_deref(), Some("Funk"));
        assert_eq!(config.agent_notifications_exclude, [AgentKind::Codex, AgentKind::Gemini, AgentKind::Claude]);
        // 有一个认不出的名字时整行跳过；空值清空，声音回到默认。
        let config = load(&["agent-notifications-exclude = codex\nagent-notifications-exclude = claude,nope"]);
        assert_eq!(config.agent_notifications_exclude, [AgentKind::Codex]);
        let config = load(&[
            "agent-notifications-exclude = codex\nagent-notifications-exclude =\nagent-done-sound = none\nagent-done-sound =",
        ]);
        assert!(config.agent_notifications_exclude.is_empty());
        assert_eq!(config.agent_done_sound.as_deref(), Some("Glass"));
        assert_eq!(load(&["agent-notifications-exclude = other"]).agent_notifications_exclude, [AgentKind::Other]);
    }

    #[test]
    fn explicit_keys_override_the_theme() {
        let dir = std::env::temp_dir().join(format!("runode-theme-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let theme = dir.join("T");
        std::fs::write(&theme, "background = #111111\nforeground = #222222\n").unwrap();
        let config = load(&[&format!("theme = {}\nforeground = #333333", theme.display())]);
        assert_eq!(config.background, Rgb(0x11, 0x11, 0x11));
        assert_eq!(config.foreground, Rgb(0x33, 0x33, 0x33));
    }

    #[test]
    fn config_fits_the_other_appearance_unless_the_theme_follows_it() {
        // `load` 按深色外观加载。
        let plain = load(&["theme = Dracula"]);
        assert!(!plain.theme_follows_appearance);
        assert!(plain.fits_appearance(true) && plain.fits_appearance(false));
        let pair = load(&["theme = light:Day, dark:Night"]);
        assert!(pair.theme_follows_appearance);
        assert!(pair.fits_appearance(true));
        assert!(!pair.fits_appearance(false));
        // 两边是同一个主题时外观不影响结果。
        assert!(load(&["theme = light:Same, dark:Same"]).fits_appearance(false));
        assert!(load(&[""]).fits_appearance(false));
    }

    #[test]
    fn colors_can_follow_the_cell() {
        let config = load(&[
            "cursor-color = cell-foreground\ncursor-text = cell-background\nselection-background = cell-background\nselection-foreground = #010203",
        ]);
        assert_eq!(config.cursor_color, Some(TerminalColor::CellForeground));
        assert_eq!(config.cursor_text, Some(TerminalColor::CellBackground));
        assert_eq!(config.selection_background, Some(TerminalColor::CellBackground));
        assert_eq!(config.selection_foreground, Some(TerminalColor::Rgb(Rgb(1, 2, 3))));
    }
}
