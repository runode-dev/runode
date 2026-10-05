//! 配色主题：按名字在用户的主题目录、已安装的 Ghostty 和编进二进制的主题里找。

use std::path::PathBuf;

/// `light:A,dark:B` 按系统外观取一个，否则原样返回。
pub(crate) fn pick_theme(value: &str, dark: bool) -> String {
    let want = if dark { "dark:" } else { "light:" };
    value
        .split(',')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(want))
        .unwrap_or(value)
        .trim()
        .to_owned()
}

/// 编进二进制的配色主题，按名字排序。
pub(crate) static BUNDLED_THEMES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/themes.rs"));

pub(crate) enum Theme {
    File(PathBuf),
    Bundled(&'static str),
}

/// 主题可以是绝对路径，否则依次在 runode、Ghostty 的用户主题目录、
/// Ghostty 自带的主题目录里找同名文件，都没有再用内置的同名主题。
pub(crate) fn find_theme(name: &str) -> Option<Theme> {
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
    use runode_model::color::{Rgb, TerminalColor};

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
                config
                    .apply(&entry.key, &entry.value, false)
                    .unwrap_or_else(|err| panic!("{}: {err}", entry.origin));
            }
        }
    }

    #[test]
    fn light_dark_theme_pair() {
        assert_eq!(pick_theme("light:Day, dark:Night", true), "Night");
        assert_eq!(pick_theme("light:Day, dark:Night", false), "Day");
        assert_eq!(pick_theme("Dracula", true), "Dracula");
    }
}
