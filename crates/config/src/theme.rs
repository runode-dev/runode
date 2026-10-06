//! 配色主题：按名字在用户的主题目录、已安装的 Ghostty 和编进二进制的主题里找。

use std::{path::PathBuf, sync::OnceLock};

/// `light:A,dark:B` 按系统外观取一个，否则原样返回。
pub(crate) fn pick_theme(value: &str, dark: bool) -> String {
    let want = if dark { "dark:" } else { "light:" };
    value.split(',').map(str::trim).find_map(|part| part.strip_prefix(want)).unwrap_or(value).trim().to_owned()
}

/// 编进二进制的配色主题，按名字排序。
pub(crate) static BUNDLED_THEMES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/themes.rs"));

pub(crate) enum Theme {
    File(PathBuf),
    Bundled(&'static str),
}

/// 主题可以是绝对路径，否则依次在 runode、Ghostty 的用户主题目录、
/// Ghostty 自带的主题目录里找同名文件，都没有再用内置的同名主题。Ghostty 自带的目录要向系统
/// 查，前面的目录里找到了就不查。
pub(crate) fn find_theme(name: &str) -> Option<Theme> {
    let paths = runode_paths::Dirs::from_env();
    let path = paths.expand_home(name);
    if path.is_absolute() {
        return path.is_file().then_some(Theme::File(path));
    }
    let bundled_dir = std::iter::once_with(|| ghostty_resources_dir().map(|dir| dir.join("themes"))).flatten();
    let dirs = paths.themes_dir().into_iter().chain(paths.ghostty_themes_dir()).chain(bundled_dir);
    if let Some(path) = theme_file(name, dirs) {
        return Some(Theme::File(path));
    }
    bundled_theme(name).map(Theme::Bundled)
}

/// 能选的主题名：用户的主题目录、Ghostty 的用户和自带主题目录里的文件，以及内置的主题，去重后
/// 按名字排序（不分大小写）。
pub fn theme_names() -> Vec<String> {
    let paths = runode_paths::Dirs::from_env();
    let bundled_dir = ghostty_resources_dir().map(|dir| dir.join("themes"));
    let dirs = paths.themes_dir().into_iter().chain(paths.ghostty_themes_dir()).chain(bundled_dir);
    let mut names: Vec<String> = dirs
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .chain(BUNDLED_THEMES.iter().map(|(name, _)| (*name).to_owned()))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup();
    names
}

/// 按顺序在 `dirs` 里找名为 `name` 的主题文件，找到就停，后面的目录不再取。
fn theme_file(name: &str, dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    dirs.into_iter().map(|dir| dir.join(name)).find(|path| path.is_file())
}

fn bundled_theme(name: &str) -> Option<&'static str> {
    BUNDLED_THEMES.binary_search_by(|(n, _)| (*n).cmp(name)).ok().map(|i| BUNDLED_THEMES[i].1)
}

/// Ghostty 的资源目录：在 Ghostty 里启动的进程有 `GHOSTTY_RESOURCES_DIR`；
/// 否则按 bundle id 向系统查已安装的 Ghostty.app。查一次要经 LaunchServices，结果在进程里
/// 记下，之后重载配置不再查；所以 app 开着时才装的 Ghostty 要等下次启动才找得到。
fn ghostty_resources_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(look_up_ghostty_resources_dir).clone()
}

fn look_up_ghostty_resources_dir() -> Option<PathBuf> {
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
    use runode_shared_types::color::{Rgb, TerminalColor};

    use super::*;
    use crate::{Config, parse::parse_entries};

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
                config.apply(&entry.key, &entry.value, false).unwrap_or_else(|err| panic!("{}: {err}", entry.origin));
            }
        }
    }

    #[test]
    fn theme_file_stops_at_the_first_match() {
        let dir = std::env::temp_dir().join(format!("runode-theme-file-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Mine"), "background = #000000\n").unwrap();
        // 前面的目录里找到了就不该再取后面的目录（它代表要向系统查的 Ghostty 自带目录）。
        let later = std::iter::once_with(|| -> PathBuf { panic!("looked past the directory that has the theme") });
        let found = theme_file("Mine", std::iter::once(dir.clone()).chain(later));
        assert_eq!(found, Some(dir.join("Mine")));
        // 前面都没有时才取后面的。
        let mut asked = false;
        let later = std::iter::once_with(|| {
            asked = true;
            dir.clone()
        });
        assert_eq!(theme_file("Mine", std::iter::once(dir.join("missing")).chain(later)), Some(dir.join("Mine")));
        assert!(asked);
        assert_eq!(theme_file("Other", [dir.clone()]), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn light_dark_theme_pair() {
        assert_eq!(pick_theme("light:Day, dark:Night", true), "Night");
        assert_eq!(pick_theme("light:Day, dark:Night", false), "Day");
        assert_eq!(pick_theme("Dracula", true), "Dracula");
    }
}
