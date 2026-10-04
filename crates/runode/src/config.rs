//! 配置：兼容 Ghostty 的配置文件。
//!
//! 先按 Ghostty 自己的顺序读它的配置（XDG 目录，再到 macOS 的 Application
//! Support），再读 runode 自己的 `$XDG_CONFIG_HOME/runode/config`。语法与键名都和
//! Ghostty 相同；同一个键 runode 也设了时以 runode 为准。runode 不认识的键直接忽略，
//! 因为 Ghostty 的配置里大部分键与 runode 无关。

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use gpui::{App, Global, WindowAppearance};

use libghostty_vt::{key::OptionAsAlt, style::RgbColor, terminal::CursorStyle};

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CellHeight {
    Pixels(f32),
    Percent(f32),
}

/// 光标和选区的颜色：固定色，或者跟随所在单元格的前景、背景色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TerminalColor {
    Rgb(RgbColor),
    CellForeground,
    CellBackground,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// 依次尝试的字体族，第一个能解析的生效。
    pub font_family: Vec<String>,
    pub font_size: f32,
    pub adjust_cell_height: Option<CellHeight>,
    /// (左, 右)
    pub window_padding_x: (f32, f32),
    /// (上, 下)
    pub window_padding_y: (f32, f32),
    pub cursor_style: CursorStyle,
    /// `None` 表示默认闪烁，运行中的程序仍可改变。
    pub cursor_style_blink: Option<bool>,
    pub background: RgbColor,
    pub foreground: RgbColor,
    /// `None` 表示用前景色。
    pub cursor_color: Option<TerminalColor>,
    /// 实心块状光标下文字的颜色，`None` 表示用背景色。
    pub cursor_text: Option<TerminalColor>,
    /// `None` 表示取单元格的前景色，与下一项合起来就是反色。
    pub selection_background: Option<TerminalColor>,
    /// `None` 表示取单元格的背景色。
    pub selection_foreground: Option<TerminalColor>,
    /// 搜索匹配的背景色和文字色。
    pub search_background: TerminalColor,
    pub search_foreground: TerminalColor,
    /// 当前选中的那个搜索匹配的背景色和文字色。
    pub search_selected_background: TerminalColor,
    pub search_selected_foreground: TerminalColor,
    /// 覆盖默认 256 色中的若干项。
    pub palette: Vec<(u8, RgbColor)>,
    pub macos_option_as_alt: OptionAsAlt,
    /// 本次读到的全部文件（含主题和 config-file 引入的），供热重载监视。
    pub sources: Vec<PathBuf>,
    /// 加载时系统是否为深色外观，`theme = light:A,dark:B` 据此选了其中一个。
    pub dark: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_family: vec!["Hack Nerd Font Mono".into()],
            font_size: 14.,
            adjust_cell_height: None,
            window_padding_x: (2., 2.),
            window_padding_y: (0., 6.),
            cursor_style: CursorStyle::Block,
            cursor_style_blink: None,
            background: theme::BACKGROUND,
            foreground: theme::FOREGROUND,
            cursor_color: None,
            cursor_text: None,
            selection_background: None,
            selection_foreground: None,
            search_background: TerminalColor::Rgb(theme::SEARCH_BACKGROUND),
            search_foreground: TerminalColor::Rgb(theme::SEARCH_FOREGROUND),
            search_selected_background: TerminalColor::Rgb(theme::SEARCH_SELECTED_BACKGROUND),
            search_selected_foreground: TerminalColor::Rgb(theme::SEARCH_FOREGROUND),
            palette: theme::ANSI
                .iter()
                .enumerate()
                .map(|(i, c)| (i as u8, *c))
                .collect(),
            macos_option_as_alt: OptionAsAlt::False,
            sources: Vec::new(),
            dark: true,
        }
    }
}

/// 当前生效的配置。视图通过 `observe_global` 在重载后重新应用。
pub struct AppConfig(pub Arc<Config>);

impl Global for AppConfig {}

/// 两次检查配置文件是否变化的间隔。
const WATCH_INTERVAL: Duration = Duration::from_secs(1);

/// 加载配置并开始监视配置文件，保存后自动重载。
pub fn install(cx: &mut App) {
    reload(cx);
    cx.spawn(async move |cx| {
        let mut seen = None;
        loop {
            cx.background_executor().timer(WATCH_INTERVAL).await;
            let mut stamp = cx.update(|cx| watch_stamp(&cx.global::<AppConfig>().0));
            // 第一次只记录，之后有变化才重载。重载可能引入新的文件（比如换了主题），
            // 所以重载后按新配置重新记录，免得下一轮又因文件列表变化再重载一次。
            if seen.as_ref().is_some_and(|seen| *seen != stamp) {
                stamp = cx.update(|cx| {
                    reload(cx);
                    watch_stamp(&cx.global::<AppConfig>().0)
                });
            }
            seen = Some(stamp);
        }
    })
    .detach();
}

/// 重新读取全部配置文件并广播给各视图。
pub fn reload(cx: &mut App) {
    let dark = system_is_dark(cx);
    cx.set_global(AppConfig(Arc::new(Config::load(dark))));
}

/// 系统深浅色变了就重载，让 `theme = light:A,dark:B` 换到对应的主题。每个窗口都会
/// 收到外观变化，第一个窗口重载后外观已经对上，其余窗口直接跳过。
pub fn follow_appearance(cx: &mut App) {
    if system_is_dark(cx) != cx.global::<AppConfig>().0.dark {
        reload(cx);
    }
}

fn system_is_dark(cx: &App) -> bool {
    matches!(
        cx.window_appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// 所有可能的配置文件的修改时间。还不存在的文件也算在内，新建配置文件同样会触发重载。
fn watch_stamp(config: &Config) -> Vec<(PathBuf, Option<SystemTime>)> {
    let mut paths = ghostty_config_paths();
    paths.push(runode_config_path());
    paths.extend(config.sources.iter().cloned());
    paths.dedup();
    paths
        .into_iter()
        .map(|path| {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            (path, modified)
        })
        .collect()
}

/// 一条 `key = value`，带出处以便报错。
struct Entry {
    key: String,
    value: String,
    origin: String,
}

impl Config {
    /// 读取 Ghostty 与 runode 的配置。`dark` 用于 `theme = light:X,dark:Y`。
    pub fn load(dark: bool) -> Self {
        let mut sources = Vec::new();
        let ghostty: Vec<Entry> = ghostty_config_paths()
            .iter()
            .flat_map(|path| read_entries(path, &mut sources))
            .collect();
        let runode = read_entries(&runode_config_path(), &mut sources);
        Self::from_layers(&[ghostty, runode], dark, &mut sources)
    }

    fn from_layers(layers: &[Vec<Entry>], dark: bool, sources: &mut Vec<PathBuf>) -> Self {
        let mut config = Self::default();
        // 先套主题，再让显式写出的键覆盖它。后一层的主题覆盖前一层的。
        let theme = layers
            .iter()
            .flatten()
            .rfind(|e| e.key == "theme")
            .map(|e| e.value.clone());
        if let Some(theme) = theme.filter(|t| !t.is_empty()) {
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
    fn apply_layer(&mut self, entries: &[Entry]) {
        let mut seen = HashSet::new();
        for entry in entries {
            let first = seen.insert(entry.key.as_str());
            if let Err(err) = self.apply(&entry.key, &entry.value, first) {
                tracing::warn!("{}: {} = {}: {err}", entry.origin, entry.key, entry.value);
            }
        }
    }

    fn apply(&mut self, key: &str, value: &str, first_in_layer: bool) -> Result<(), String> {
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
                self.search_background =
                    if empty { defaults.search_background } else { parse_terminal_color(value)? };
            }
            "search-foreground" => {
                self.search_foreground =
                    if empty { defaults.search_foreground } else { parse_terminal_color(value)? };
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
            "macos-option-as-alt" => {
                self.macos_option_as_alt = match value {
                    "" | "false" => OptionAsAlt::False,
                    "true" => OptionAsAlt::True,
                    "left" => OptionAsAlt::Left,
                    "right" => OptionAsAlt::Right,
                    _ => return Err("expected true, false, left or right".into()),
                };
            }
            // 主题在应用各层之前已处理；其余 Ghostty 键 runode 用不上。
            _ => {}
        }
        Ok(())
    }
}

fn parse_f32(value: &str) -> Result<f32, String> {
    value.trim().parse().map_err(|_| "expected a number".into())
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

fn parse_color(value: &str) -> Result<RgbColor, String> {
    RgbColor::parse(value).map_err(|_| "not a color".into())
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

/// `light:A,dark:B` 按系统外观取一个，否则原样返回。
fn pick_theme(value: &str, dark: bool) -> String {
    let want = if dark { "dark:" } else { "light:" };
    value
        .split(',')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(want))
        .unwrap_or(value)
        .trim()
        .to_owned()
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
            let file = expand_home(file);
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
fn parse_entries(text: &str, name: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value);
        entries.push(Entry {
            key: key.trim().to_owned(),
            value: value.to_owned(),
            origin: format!("{name}:{}", n + 1),
        });
    }
    entries
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

/// 用文本编辑器打开 runode 自己的配置文件；文件还不存在时先建一个空的。
pub fn open(cx: &App) {
    let path = runode_config_path();
    let created = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::OpenOptions::new().create(true).append(true).open(&path).map(drop));
    if let Err(err) = created {
        tracing::warn!("failed to create {}: {err}", path.display());
        return;
    }
    // 配置文件没有扩展名，按扩展名找默认程序不可靠；macOS 上用 `open -t` 指定文本编辑器。
    // 等它退出要在别的线程里，免得卡住界面，也免得留下僵尸进程。
    if cfg!(target_os = "macos") {
        std::thread::spawn(move || {
            if let Err(err) = std::process::Command::new("open").arg("-t").arg(&path).status() {
                tracing::warn!("failed to open {}: {err}", path.display());
            }
        });
    } else {
        cx.open_with_system(&path);
    }
}

fn runode_config_path() -> PathBuf {
    config_dir().join("runode/config")
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

/// Ghostty 配置文件的位置，按加载顺序：XDG 目录的旧名与新名，macOS 上再加
/// Application Support 的旧名与新名。
fn ghostty_config_paths() -> Vec<PathBuf> {
    let xdg = config_dir().join("ghostty");
    let mut paths = vec![xdg.join("config"), xdg.join("config.ghostty")];
    if cfg!(target_os = "macos") {
        let support = home().join("Library/Application Support/com.mitchellh.ghostty");
        paths.extend([support.join("config"), support.join("config.ghostty")]);
    }
    paths
}

/// 编进二进制的配色主题，按名字排序。
static BUNDLED_THEMES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/themes.rs"));

enum Theme {
    File(PathBuf),
    Bundled(&'static str),
}

/// 主题可以是绝对路径，否则依次在 runode、Ghostty 的用户主题目录、
/// Ghostty 自带的主题目录里找同名文件，都没有再用内置的同名主题。
fn find_theme(name: &str) -> Option<Theme> {
    let path = expand_home(name);
    if path.is_absolute() {
        return path.is_file().then_some(Theme::File(path));
    }
    let mut dirs = vec![
        config_dir().join("runode/themes"),
        config_dir().join("ghostty/themes"),
    ];
    dirs.extend(ghostty_resources_dir().map(|dir| dir.join("themes")));
    if let Some(path) = dirs.into_iter().map(|dir| dir.join(name)).find(|p| p.is_file()) {
        return Some(Theme::File(path));
    }
    bundled_theme(name).map(Theme::Bundled)
}

fn bundled_theme(name: &str) -> Option<&'static str> {
    BUNDLED_THEMES
        .binary_search_by(|(n, _)| (*n).cmp(name))
        .ok()
        .map(|i| BUNDLED_THEMES[i].1)
}

/// Ghostty 的资源目录：在 Ghostty 里启动的进程有 `GHOSTTY_RESOURCES_DIR`；
/// 否则按 bundle id 向系统查已安装的 Ghostty.app。
fn ghostty_resources_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("GHOSTTY_RESOURCES_DIR") {
        return Some(dir.into());
    }
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSWorkspace;
        use objc2_foundation::NSString;
        let id = NSString::from_str("com.mitchellh.ghostty");
        let url = NSWorkspace::sharedWorkspace().URLForApplicationWithBundleIdentifier(&id)?;
        let app = PathBuf::from(url.path()?.to_string());
        return Some(app.join("Contents/Resources/ghostty"));
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(test)]
mod tests {
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

    fn load(layers: &[&str]) -> Config {
        let layers: Vec<_> = layers.iter().map(|t| entries(t)).collect();
        Config::from_layers(&layers, true, &mut Vec::new())
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
unknown-key = whatever
"#]);
        assert_eq!(config.font_family, ["Hack Nerd Font Mono", "Menlo"]);
        assert_eq!(config.font_size, 16.);
        assert_eq!(config.adjust_cell_height, Some(CellHeight::Percent(10.)));
        assert_eq!(config.window_padding_x, (4., 8.));
        assert_eq!(config.window_padding_y, (3., 3.));
        assert_eq!(config.cursor_style, CursorStyle::Bar);
        assert_eq!(config.cursor_style_blink, Some(false));
        assert_eq!(config.background, RgbColor { r: 0x10, g: 0x20, b: 0x30 });
        assert_eq!(config.foreground, RgbColor { r: 255, g: 255, b: 255 });
        assert!(config.palette.contains(&(1, RgbColor { r: 255, g: 0, b: 0 })));
        assert_eq!(config.macos_option_as_alt, OptionAsAlt::Left);
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
        assert!(config.palette.contains(&(2, RgbColor { r: 0, g: 0, b: 1 })));
        assert!(config.palette.contains(&(3, RgbColor { r: 0, g: 0, b: 3 })));
        assert!(!config.palette.contains(&(3, RgbColor { r: 0, g: 0, b: 2 })));
    }

    #[test]
    fn empty_value_resets_to_default_and_bad_values_are_skipped() {
        let config = load(&["font-size = 20\nfont-size =\ncursor-style = triangle"]);
        assert_eq!(config.font_size, Config::default().font_size);
        assert_eq!(config.cursor_style, CursorStyle::Block);
    }

    #[test]
    fn explicit_keys_override_the_theme() {
        let dir = std::env::temp_dir().join(format!("runode-theme-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let theme = dir.join("T");
        std::fs::write(&theme, "background = #111111\nforeground = #222222\n").unwrap();
        let config = load(&[&format!("theme = {}\nforeground = #333333", theme.display())]);
        assert_eq!(config.background, RgbColor { r: 0x11, g: 0x11, b: 0x11 });
        assert_eq!(config.foreground, RgbColor { r: 0x33, g: 0x33, b: 0x33 });
    }

    #[test]
    fn bundled_theme_sets_every_color() {
        let entries = parse_entries(bundled_theme("Catppuccin Mocha").unwrap(), "Catppuccin Mocha");
        let mut config = Config::default();
        config.apply_layer(&entries);
        assert_eq!(config.background, RgbColor { r: 0x1e, g: 0x1e, b: 0x2e });
        let rgb = |r, g, b| Some(TerminalColor::Rgb(RgbColor { r, g, b }));
        assert_eq!(config.cursor_color, rgb(0xf5, 0xe0, 0xdc));
        assert_eq!(config.cursor_text, rgb(0x1e, 0x1e, 0x2e));
        assert_eq!(config.selection_background, rgb(0x58, 0x5b, 0x70));
        assert!(config.palette.contains(&(15, RgbColor { r: 0xba, g: 0xc2, b: 0xde })));
        assert!(bundled_theme("No Such Theme").is_none());
    }

    #[test]
    fn every_bundled_theme_parses() {
        for (name, text) in BUNDLED_THEMES {
            let mut config = Config::default();
            for entry in parse_entries(text, name) {
                config
                    .apply(&entry.key, &entry.value, false)
                    .unwrap_or_else(|err| panic!("{}: {err}", entry.origin));
            }
        }
    }

    #[test]
    fn colors_can_follow_the_cell() {
        let config = load(&[
            "cursor-color = cell-foreground\ncursor-text = cell-background\nselection-background = cell-background\nselection-foreground = #010203",
        ]);
        assert_eq!(config.cursor_color, Some(TerminalColor::CellForeground));
        assert_eq!(config.cursor_text, Some(TerminalColor::CellBackground));
        assert_eq!(config.selection_background, Some(TerminalColor::CellBackground));
        assert_eq!(
            config.selection_foreground,
            Some(TerminalColor::Rgb(RgbColor { r: 1, g: 2, b: 3 }))
        );
    }

    #[test]
    fn light_dark_theme_pair() {
        assert_eq!(pick_theme("light:Day, dark:Night", true), "Night");
        assert_eq!(pick_theme("light:Day, dark:Night", false), "Day");
        assert_eq!(pick_theme("Dracula", true), "Dracula");
    }
}
