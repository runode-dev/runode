//! 配置模板：runode 的配置文件不存在时写进去的那一份，全部注释掉，列出支持的键、能填的值和默认值；
//! 文件已经有了的，把后来新加的键按模板补进去。

use std::path::Path;

use runode_shared_types::{
    agent::AgentKind,
    color::{Rgb, TerminalColor},
};

use crate::{Config, parse::KEYS, theme::BUNDLED_THEMES};

/// runode 的配置文件 `path` 不存在时写入 `template`；已存在时按 `fill_missing_keys` 补上文件里
/// 还没有的键，没有要补的就不动。
pub fn create_config_file(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let locale = crate::i18n::current();
    match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(template(&locale).as_bytes()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let text = std::fs::read_to_string(path)?;
            let filled = fill_missing_keys(&text, &locale);
            if filled != text { std::fs::write(path, filled) } else { Ok(()) }
        }
        Err(err) => Err(err),
    }
}

/// 一行设置的键名：`key = value`，或注释掉的 `# key = value`；说明行和别的行为 `None`。
fn setting_key(line: &str) -> Option<&str> {
    let line = line.trim();
    let line = line.strip_prefix('#').unwrap_or(line).trim_start();
    let (key, _) = line.split_once('=')?;
    let key = key.trim_end();
    (!key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')).then_some(key)
}

/// 配置文件的内容 `text` 里补上 `KEYS` 里有、文件里一行都没写到（写了的和注释掉的都算写到）
/// 的键：说明和注释掉的默认值和模板里一样，插在 `KEYS` 里排在它前面、文件里写到了的那个键
/// 后面，跨了组就先空一行。前面的键一个都没写到的不补，所以空文件或自己从头写的文件不会被
/// 塞进整份模板。
fn fill_missing_keys(text: &str, locale: &str) -> String {
    let d = Config::default();
    let locales = crate::i18n::available().join(", ");
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let keys: Vec<(usize, &str)> =
        KEYS.iter().enumerate().flat_map(|(group, keys)| keys.iter().map(move |key| (group, *key))).collect();
    for (ix, &(group, key)) in keys.iter().enumerate() {
        if lines.iter().any(|line| setting_key(line) == Some(key)) {
            continue;
        }
        let Some((at, same_group)) = keys[..ix].iter().rev().find_map(|&(prev_group, prev)| {
            let last = lines.iter().rposition(|line| setting_key(line) == Some(prev))?;
            Some((last + 1, prev_group == group))
        }) else {
            continue;
        };
        let mut block = String::new();
        if !same_group {
            block.push('\n');
        }
        block.push_str(&key_block(&d, key, locale, &locales));
        lines.splice(at..at, block.lines().map(str::to_owned));
    }
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// 模板里的一个键：上面是它的说明，每行加 `## `；下面是每个默认值一行，没有默认值时写一行空值。
fn key_block(d: &Config, key: &str, locale: &str, locales: &str) -> String {
    let mut out = String::new();
    let doc_key = format!("config.{}", key.replace('-', "_"));
    // 能填的 agent 短名，`agent-notifications-exclude` 的说明里列出来。
    let agents: Vec<&str> = AgentKind::ALL.iter().map(|kind| kind.label()).chain(["other"]).collect();
    let agents = agents.join(", ");
    for line in rust_i18n::t!(&doc_key, locale = locale, locales = locales, agents = agents).lines() {
        out.push_str(&format!("## {line}\n"));
    }
    let values = template_values(d, key);
    for value in values.iter().map(String::as_str).chain(values.is_empty().then_some("")) {
        let line = if value.is_empty() { format!("# {key} =\n") } else { format!("# {key} = {value}\n") };
        out.push_str(&line);
    }
    out
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
    for (i, group) in KEYS.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for key in *group {
            out.push_str(&key_block(&d, key, locale, &locales));
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
        "file-tree-font-size" => vec![d.file_tree_font_size.to_string()],
        "file-tree-preview-click" => vec!["single".into()],
        "preview-font-size" => vec![d.preview_font_size.to_string()],
        "background" => vec![hex(d.background)],
        "foreground" => vec![hex(d.foreground)],
        "search-background" => vec![color(d.search_background)],
        "search-foreground" => vec![color(d.search_foreground)],
        "search-selected-background" => vec![color(d.search_selected_background)],
        "search-selected-foreground" => vec![color(d.search_selected_foreground)],
        "palette" => d.palette.iter().map(|(i, c)| format!("{i}={}", hex(*c))).collect(),
        "cursor-style" => vec!["block".into()],
        "macos-option-as-alt" => vec!["false".into()],
        "scrollback-limit" => vec![d.scrollback_limit.to_string()],
        "shell-integration" => vec!["detect".into()],
        "command-suggestions" => vec!["true".into()],
        "command-completions" => vec!["true".into()],
        "agent-notifications" => vec![d.agent_notifications.to_string()],
        "agent-done-sound" => d.agent_done_sound.iter().cloned().collect(),
        "agent-blocked-sound" => d.agent_blocked_sound.iter().cloned().collect(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse_entries, tests::load};

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

    /// 删掉模板里的 `keys`：键的设置行和紧挨在上面的说明行；删空了一组时连同组前的空行。
    fn without(text: &str, keys: &[&str]) -> String {
        let mut lines: Vec<&str> = text.lines().collect();
        for key in keys {
            let setting = lines.iter().position(|line| setting_key(line) == Some(*key)).unwrap();
            let mut start = setting;
            while start > 0 && lines[start - 1].starts_with("## ") {
                start -= 1;
            }
            let mut end = setting + 1;
            while end < lines.len() && setting_key(lines[end]) == Some(*key) {
                end += 1;
            }
            if lines[start - 1].is_empty() && lines.get(end).is_none_or(|line| line.is_empty()) {
                start -= 1;
            }
            lines.drain(start..end);
        }
        lines.iter().map(|line| format!("{line}\n")).collect()
    }

    #[test]
    fn fills_keys_added_after_the_file_was_written() {
        for locale in crate::i18n::available() {
            let full = template(&locale);
            // 一组里缺了后面几个、整组都缺、多值的键缺了、组中间后来加的键缺了，补回来都和
            // 模板一样。
            for keys in [
                &["preview-font-size", "file-tree-preview-click"][..],
                &["language"],
                &["palette"],
                &["scrollback-limit"],
            ] {
                let old = without(&full, keys);
                assert_ne!(old, full, "{keys:?}");
                if keys == ["language"] {
                    // 排在最前面的键前面没有可以挨着的键，不补。
                    assert_eq!(fill_missing_keys(&old, &locale), old);
                } else {
                    assert_eq!(fill_missing_keys(&old, &locale), full, "{keys:?}");
                }
            }
            let group = without(&full, &["file-tree-font-size", "file-tree-preview-click", "preview-font-size"]);
            assert_eq!(fill_missing_keys(&group, &locale), full);
            // agent 通知这一组是后来加的，旧配置文件整组都没有。
            let agents =
                ["agent-notifications", "agent-notifications-exclude", "agent-done-sound", "agent-blocked-sound"];
            assert_eq!(fill_missing_keys(&without(&full, &agents), &locale), full);
            assert_eq!(fill_missing_keys(&full, &locale), full);
        }
        // 用户自己写的设置算写到了，空文件不补。
        let mine = "font-size = 16\n";
        assert!(fill_missing_keys(mine, "en").starts_with("font-size = 16\n## "));
        assert!(!fill_missing_keys(mine, "en").contains("# font-size ="));
        assert_eq!(fill_missing_keys("", "en"), "");
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
}
