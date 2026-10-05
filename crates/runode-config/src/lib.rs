//! 配置：兼容 Ghostty 的配置文件。
//!
//! 先按 Ghostty 自己的顺序读它的配置（XDG 目录，再到 macOS 的 Application
//! Support），再读 runode 自己的 `$XDG_CONFIG_HOME/runode/config.conf`。语法与键名都和
//! Ghostty 相同；同一个键 runode 也设了时以 runode 为准。runode 不认识的键直接忽略，
//! 因为 Ghostty 的配置里大部分键与 runode 无关。runode 的配置文件不存在时，启动时会
//! 写一份全部注释掉的模板，列出支持的键和默认值。

pub mod color;
pub mod i18n;
pub mod keybind;

// 配置模板和动作说明的翻译，见 `i18n`；某种语言缺了某个键时取英文。
rust_i18n::i18n!("locales", fallback = "en");

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use runode_model::{
    color::{Rgb, TerminalColor},
    settings::{CursorStyle, OptionAsAlt, TermSettings},
    shell::{IntegrationMode, Shell},
    theme,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CellHeight {
    Pixels(f32),
    Percent(f32),
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
    pub background: Rgb,
    pub foreground: Rgb,
    /// `None` 表示用前景色。
    pub cursor_color: Option<TerminalColor>,
    /// 实心块状光标下文字的颜色，`None` 表示用背景色。
    pub cursor_text: Option<TerminalColor>,
    /// `None` 表示按背景深浅用一种统一的蓝色。
    pub selection_background: Option<TerminalColor>,
    /// `None` 时：配了选区底色就取单元格的背景色（底色设成单元格前景色时就是反色），
    /// 没配就保持文字原来的颜色。
    pub selection_foreground: Option<TerminalColor>,
    /// 搜索匹配的背景色和文字色。
    pub search_background: TerminalColor,
    pub search_foreground: TerminalColor,
    /// 当前选中的那个搜索匹配的背景色和文字色。
    pub search_selected_background: TerminalColor,
    pub search_selected_foreground: TerminalColor,
    /// 覆盖默认 256 色中的若干项。
    pub palette: Vec<(u8, Rgb)>,
    pub macos_option_as_alt: OptionAsAlt,
    pub shell_integration: IntegrationMode,
    /// 在 shell 提示符上输入时，按命令历史在光标后用灰字给出建议；关掉时也不读写命令历史。
    pub command_suggestions: bool,
    /// 按 Tab 时由 runode 弹出补全菜单（命令名，以及有规格的命令的参数）；关掉时 Tab 总是交给 shell。
    pub command_completions: bool,
    /// 界面语言，是 locales 里的某个语言标签；`None` 表示跟随系统。
    pub language: Option<String>,
    /// 叠在默认快捷键上的 `keybind`，按出现顺序；只认 runode 自己的配置文件。
    pub keybinds: Vec<Keybind>,
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
            shell_integration: IntegrationMode::Detect,
            command_suggestions: true,
            command_completions: true,
            language: None,
            keybinds: Vec::new(),
            sources: Vec::new(),
            dark: true,
        }
    }
}

/// 一条 `keybind`，由 `keybind::parse` 解析并校验。
#[derive(Clone, Debug, PartialEq)]
pub enum Keybind {
    /// `keybind = clear`：去掉此前的全部绑定，包括默认的。
    Clear,
    /// `触发键=unbind`，触发键已转成 GPUI 的写法。
    Unbind(String),
    /// `触发键=动作`，动作保留原文，绑定时再构造。
    Bind { keys: String, action: String },
}

/// runode 认的全部键，按配置模板里的顺序分组，组与组之间在模板里空一行。`apply` 只处理
/// 这里列出的键，模板也按这张表逐个写出，所以新增配置项只要在这里加上键名、在 `apply` 里
/// 解析、在 `template_values` 里给出默认值、在翻译里写好说明。`theme` 和 `config-file`
/// 在应用各层之前就已处理，列在这里是为了写进模板。
const KEYS: &[&[&str]] = &[
    &["language"],
    &["font-family", "font-size", "adjust-cell-height", "window-padding-x", "window-padding-y"],
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
        "shell-integration",
        "command-suggestions",
        "command-completions",
    ],
    &["config-file"],
    &["keybind"],
];

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
        // Ghostty 配置里的 keybind 针对的是另一套默认键位和动作，照搬过来含义会变，不读。
        let ghostty: Vec<Entry> = ghostty_config_paths()
            .iter()
            .flat_map(|path| read_entries(path, &mut sources))
            .filter(|e| e.key != "keybind")
            .collect();
        let runode = config_path()
            .map(|path| read_entries(&path, &mut sources))
            .unwrap_or_default();
        Self::from_layers(&[ghostty, runode], dark, &mut sources)
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

    /// 交给终端的那部分设置。
    pub fn term_settings(&self) -> TermSettings {
        TermSettings {
            background: self.background,
            foreground: self.foreground,
            palette: self.palette.clone(),
            cursor_style: self.cursor_style,
            cursor_blink: self.cursor_style_blink,
            cursor_color: self.cursor_color,
            cursor_text: self.cursor_text,
            selection_background: self.selection_background,
            selection_foreground: self.selection_foreground,
            search_background: self.search_background,
            search_foreground: self.search_foreground,
            search_selected_background: self.search_selected_background,
            search_selected_foreground: self.search_selected_foreground,
            option_as_alt: self.macos_option_as_alt,
        }
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
            "shell-integration" => {
                self.shell_integration = match value {
                    "" | "detect" => IntegrationMode::Detect,
                    "none" => IntegrationMode::Off,
                    // 这两种 shell 还没有集成脚本。
                    "elvish" | "nushell" => IntegrationMode::Off,
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
            let file = runode_dirs::Dirs::from_env().expand_home(file);
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

/// runode 的配置文件 `path` 不存在时写入 `template`；已存在（哪怕是空文件）就不动。
pub fn create_config_file(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(template(&crate::i18n::current()).as_bytes()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}

/// 配置模板，按 `locale` 写说明：每个支持的键上面是 `##` 说明，说的是能填的值，取自翻译里的
/// `config.<键名>`；下面是注释掉的默认值，取自 `Config::default`；末尾列出内置的主题名。
/// 全部是注释，所以写进去不会盖掉 Ghostty 配置里的同名键，以后内置默认值变了也照样生效。
fn template(locale: &str) -> String {
    let d = Config::default();
    let locales = crate::i18n::available().join(", ");

    let mut out = String::new();
    // 说明可以有多行，每行加 `## `。
    let note = |out: &mut String, text: &str| {
        for line in text.lines() {
            out.push_str(&format!("## {line}\n"));
        }
    };

    note(&mut out, &rust_i18n::t!("config.header", locale = locale));
    out.push('\n');
    // 一个键：上面是它的说明，下面是每个默认值一行；没有默认值时写一行空值。
    for (i, group) in KEYS.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for key in *group {
            let doc_key = format!("config.{}", key.replace('-', "_"));
            note(&mut out, &rust_i18n::t!(&doc_key, locale = locale, locales = locales));
            let values = template_values(&d, key);
            for value in values.iter().map(String::as_str).chain(values.is_empty().then_some("")) {
                let line = if value.is_empty() { format!("# {key} =\n") } else { format!("# {key} = {value}\n") };
                out.push_str(&line);
            }
        }
    }

    note(&mut out, &rust_i18n::t!("config.actions", locale = locale));
    let usages: Vec<String> = crate::keybind::ACTIONS
        .iter()
        .map(|a| match a.param {
            Some(param) => format!("{}:{param}", a.name),
            None => a.name.to_owned(),
        })
        .collect();
    let width = usages.iter().map(String::len).max().unwrap_or(0);
    for (usage, action) in usages.iter().zip(crate::keybind::ACTIONS) {
        let doc_key = format!("action.{}", action.name);
        let doc = rust_i18n::t!(&doc_key, locale = locale);
        out.push_str(&format!("##   {usage:width$}  {doc}\n"));
    }
    out.push('\n');

    note(&mut out, &rust_i18n::t!("config.themes", locale = locale));
    out.push_str(&wrap_names(BUNDLED_THEMES.iter().map(|(name, _)| *name)));
    out
}

/// 模板里一个键注释掉的默认值，每项一行；没有默认值的键（默认跟随别的设置或不设）为空。
fn template_values(d: &Config, key: &str) -> Vec<String> {
    let hex = |Rgb(r, g, b): Rgb| format!("#{r:02x}{g:02x}{b:02x}");
    let color = |c: TerminalColor| match c {
        TerminalColor::Rgb(c) => hex(c),
        TerminalColor::CellForeground => "cell-foreground".into(),
        TerminalColor::CellBackground => "cell-background".into(),
    };
    let pair = |(a, b): (f32, f32)| if a == b { a.to_string() } else { format!("{a},{b}") };
    match key {
        "font-family" => d.font_family.clone(),
        "font-size" => vec![d.font_size.to_string()],
        "window-padding-x" => vec![pair(d.window_padding_x)],
        "window-padding-y" => vec![pair(d.window_padding_y)],
        "background" => vec![hex(d.background)],
        "foreground" => vec![hex(d.foreground)],
        "search-background" => vec![color(d.search_background)],
        "search-foreground" => vec![color(d.search_foreground)],
        "search-selected-background" => vec![color(d.search_selected_background)],
        "search-selected-foreground" => vec![color(d.search_selected_foreground)],
        "palette" => d.palette.iter().map(|(i, c)| format!("{i}={}", hex(*c))).collect(),
        "cursor-style" => vec!["block".into()],
        "macos-option-as-alt" => vec!["false".into()],
        "shell-integration" => vec!["detect".into()],
        "command-suggestions" => vec!["true".into()],
        "command-completions" => vec!["true".into()],
        "keybind" => crate::keybind::DEFAULTS.iter().map(|k| k.to_string()).collect(),
        _ => Vec::new(),
    }
}

/// 把名字用「、」连起来，按宽度折成多行 `##` 注释。
fn wrap_names<'a>(names: impl Iterator<Item = &'a str>) -> String {
    const WIDTH: usize = 96;
    let mut out = String::new();
    let mut line = String::new();
    for name in names {
        if !line.is_empty() && line.chars().count() + name.chars().count() + 1 > WIDTH {
            out += &format!("##   {line}\n");
            line.clear();
        }
        if !line.is_empty() {
            line.push('、');
        }
        line.push_str(name);
    }
    if !line.is_empty() {
        out += &format!("##   {line}\n");
    }
    out
}

/// runode 自己的配置文件；没有家目录时为 `None`。
pub fn config_path() -> Option<PathBuf> {
    runode_dirs::Dirs::from_env().config_file()
}

/// Ghostty 配置文件的位置，按 Ghostty 的加载顺序。
fn ghostty_config_paths() -> Vec<PathBuf> {
    runode_dirs::Dirs::from_env().ghostty_config_files()
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
    let paths = runode_dirs::Dirs::from_env();
    let path = paths.expand_home(name);
    if path.is_absolute() {
        return path.is_file().then_some(Theme::File(path));
    }
    let mut dirs: Vec<PathBuf> = paths.themes_dir().into_iter().chain(paths.ghostty_themes_dir()).collect();
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

    #[test]
    fn default_config_gives_the_default_term_settings() {
        assert_eq!(Config::default().term_settings(), TermSettings::default());
    }

    fn load(layers: &[&str]) -> Config {
        let layers: Vec<_> = layers.iter().map(|t| entries(t)).collect();
        Config::from_layers(&layers, true, &mut Vec::new())
    }

    #[test]
    fn template_is_all_comments_and_lists_defaults() {
        for locale in crate::i18n::available() {
            template_round_trips(&template(&locale));
        }
    }

    fn template_round_trips(text: &str) {
        assert!(parse_entries(text, "template").is_empty());
        // 说明文字用 ##，注释掉的设置用一个 #：去掉一个 # 后说明仍是注释，设置全部生效，
        // 结果应与默认配置一致。
        for line in text.lines().filter(|l| l.starts_with("# ")) {
            let key = line[2..].split_once(" =").map(|(key, _)| key);
            assert!(
                key.is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')),
                "explanation should start with ##: {line}"
            );
        }
        let settings: String = text.lines().filter_map(|l| l.strip_prefix("# ")).map(|l| format!("{l}\n")).collect();
        assert!(settings.contains("background = #171618"));
        let mut config = load(&[&settings]);
        // 默认快捷键全部重写一遍，结果与不写一样。
        assert_eq!(config.keybinds.len(), crate::keybind::DEFAULTS.len());
        assert_eq!(crate::keybind::resolve(&config.keybinds), crate::keybind::resolve(&[]));
        config.keybinds.clear();
        assert_eq!(config, Config::default());
    }

    /// `KEYS` 里的每个键都要在模板里，上面有一行说明；说明缺了翻译时 `t!` 返回「语言.键名」。
    #[test]
    fn template_lists_every_key() {
        let keys: Vec<&str> = KEYS.iter().flat_map(|group| group.iter().copied()).collect();
        assert!(keys.len() > 20, "{keys:?}");
        let mut unique = keys.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "KEYS lists a key twice");
        let text = template(crate::i18n::FALLBACK);
        assert!(!text.contains("en.config.") && !text.contains("en.action."), "template has untranslated keys");
        let lines: Vec<&str> = text.lines().collect();
        for key in keys {
            let setting = format!("# {key} =");
            let first = lines.iter().position(|l| l.starts_with(&setting));
            let first = first.unwrap_or_else(|| panic!("template misses {key}"));
            // 紧挨着的上一行是说明能填什么值的 `##`。
            assert!(first > 0 && lines[first - 1].starts_with("## "), "{key} has no explanation above it");
        }
        let themes = &text[text.find("## Available themes").unwrap()..];
        assert!(BUNDLED_THEMES.iter().all(|(name, _)| themes.contains(name)));
        for action in crate::keybind::ACTIONS {
            assert!(text.contains(&format!("##   {}", action.name)), "template misses action {}", action.name);
        }
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
    fn bundled_theme_sets_every_color() {
        let entries = parse_entries(bundled_theme("Catppuccin Mocha").unwrap(), "Catppuccin Mocha");
        let mut config = Config::default();
        config.apply_layer(&entries);
        assert_eq!(config.background, Rgb(0x1e, 0x1e, 0x2e));
        let rgb = |r, g, b| Some(TerminalColor::Rgb(Rgb(r, g, b)));
        assert_eq!(config.cursor_color, rgb(0xf5, 0xe0, 0xdc));
        assert_eq!(config.cursor_text, rgb(0x1e, 0x1e, 0x2e));
        assert_eq!(config.selection_background, rgb(0x58, 0x5b, 0x70));
        assert!(config.palette.contains(&(15, Rgb(0xba, 0xc2, 0xde))));
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
            Some(TerminalColor::Rgb(Rgb(1, 2, 3)))
        );
    }

    #[test]
    fn light_dark_theme_pair() {
        assert_eq!(pick_theme("light:Day, dark:Night", true), "Night");
        assert_eq!(pick_theme("light:Day, dark:Night", false), "Day");
        assert_eq!(pick_theme("Dracula", true), "Dracula");
    }
}
