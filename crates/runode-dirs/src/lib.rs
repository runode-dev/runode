//! runode 读写的目录和文件的位置，统一从环境变量算出来。
//!
//! runode 自己的东西在所有系统上都放在同一个根目录：`$XDG_CONFIG_HOME/runode`，没设时
//! `~/.config/runode`。配置文件和要留着的数据（窗口布局的存档、命令历史）直接放在根目录，
//! 随时能重新生成的缓存（shell 集成脚本、首个终端的尺寸）放在根目录的 `cache/` 里。
//! Ghostty 自己的配置和主题照旧在它原来的位置读。

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// 各个目录的位置。环境变量没设、为空，或者连家目录都没有时，对应的字段为 `None`，
/// 由它算出的文件也都是 `None`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dirs {
    /// 家目录，即 `HOME`。
    pub home: Option<PathBuf>,
    /// 用户配置的根目录：`$XDG_CONFIG_HOME`，没设时 `~/.config`。Ghostty 的配置也在这下面。
    pub config: Option<PathBuf>,
    /// runode 自己的根目录，即 `config` 下的 `runode`。配置文件和要留着的数据放在这里。
    pub data: Option<PathBuf>,
    /// 可以重新生成的缓存，即 `data` 下的 `cache`。shell 会执行这里的集成脚本，所以放在
    /// 用户自己的目录里，而不是可能多人共用的临时目录。
    pub cache: Option<PathBuf>,
}

impl Dirs {
    /// 按当前进程的环境变量算出各个目录。
    pub fn from_env() -> Self {
        Self::from_vars(|key| std::env::var_os(key))
    }

    /// 按 `var` 给出的环境变量算出各个目录，空值当作没设。
    fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Self {
        let var = |key: &str| var(key).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = var("HOME");
        let config = var("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|home| home.join(".config")));
        let data = config.as_ref().map(|config| config.join("runode"));
        let cache = data.as_ref().map(|data| data.join("cache"));
        Self { home, config, data, cache }
    }

    /// runode 的配置文件。
    pub fn config_file(&self) -> Option<PathBuf> {
        self.data_file("config.conf")
    }

    /// 用户自己的配色主题目录，按名字找主题时最先找这里。
    pub fn themes_dir(&self) -> Option<PathBuf> {
        self.data_file("themes")
    }

    /// 窗口布局的存档。
    pub fn windows_file(&self) -> Option<PathBuf> {
        self.data_file("windows.json")
    }

    /// runode 自己记的命令历史。
    pub fn history_file(&self) -> Option<PathBuf> {
        self.data_file("history.jsonl")
    }

    /// 解出 shell 集成脚本的目录。
    pub fn shell_integration_dir(&self) -> Option<PathBuf> {
        self.cache_file("shell-integration")
    }

    /// 上次第一个终端的尺寸，启动时按它提前启动 shell。
    pub fn prespawn_size_file(&self) -> Option<PathBuf> {
        self.cache_file("first-terminal-size")
    }

    /// Ghostty 配置文件的位置，按 Ghostty 的加载顺序：XDG 目录的旧名与新名，macOS 上再加
    /// Application Support 的旧名与新名。
    pub fn ghostty_config_files(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self.config.iter().map(|config| config.join("ghostty")).collect();
        if cfg!(target_os = "macos") {
            dirs.extend(
                self.home
                    .iter()
                    .map(|home| home.join("Library/Application Support/com.mitchellh.ghostty")),
            );
        }
        dirs.iter()
            .flat_map(|dir| [dir.join("config"), dir.join("config.ghostty")])
            .collect()
    }

    /// Ghostty 用户自己的配色主题目录。
    pub fn ghostty_themes_dir(&self) -> Option<PathBuf> {
        Some(self.config.as_ref()?.join("ghostty/themes"))
    }

    /// 把开头的 `~/` 换成家目录；不以 `~/` 开头或没有家目录时原样返回。
    pub fn expand_home(&self, path: &str) -> PathBuf {
        match (path.strip_prefix("~/"), &self.home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(path),
        }
    }

    /// `path` 是不是家目录本身。
    pub fn is_home(&self, path: &Path) -> bool {
        self.home.as_deref() == Some(path)
    }

    fn data_file(&self, name: &str) -> Option<PathBuf> {
        Some(self.data.as_ref()?.join(name))
    }

    fn cache_file(&self, name: &str) -> Option<PathBuf> {
        Some(self.cache.as_ref()?.join(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(vars: &[(&str, &str)]) -> Dirs {
        let vars: Vec<(String, OsString)> = vars.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect();
        Dirs::from_vars(|key| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
    }

    #[test]
    fn everything_lives_under_the_config_home() {
        let dirs = dirs(&[("HOME", "/home/me")]);
        assert_eq!(dirs.config_file(), Some("/home/me/.config/runode/config.conf".into()));
        assert_eq!(dirs.windows_file(), Some("/home/me/.config/runode/windows.json".into()));
        assert_eq!(dirs.history_file(), Some("/home/me/.config/runode/history.jsonl".into()));
        assert_eq!(dirs.themes_dir(), Some("/home/me/.config/runode/themes".into()));
        assert_eq!(dirs.shell_integration_dir(), Some("/home/me/.config/runode/cache/shell-integration".into()));
        assert_eq!(dirs.prespawn_size_file(), Some("/home/me/.config/runode/cache/first-terminal-size".into()));
        assert_eq!(dirs.ghostty_themes_dir(), Some("/home/me/.config/ghostty/themes".into()));
    }

    #[test]
    fn xdg_config_home_moves_the_root() {
        let dirs = dirs(&[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "/xdg")]);
        assert_eq!(dirs.config_file(), Some("/xdg/runode/config.conf".into()));
        assert_eq!(dirs.ghostty_config_files()[..2], [PathBuf::from("/xdg/ghostty/config"), "/xdg/ghostty/config.ghostty".into()]);
        // 家目录不跟着变。
        assert_eq!(dirs.expand_home("~/x"), PathBuf::from("/home/me/x"));
    }

    #[test]
    fn empty_variables_count_as_unset() {
        let dirs = dirs(&[("HOME", ""), ("XDG_CONFIG_HOME", "")]);
        assert_eq!(dirs, Dirs::default());
        assert_eq!(dirs.config_file(), None);
        assert!(dirs.ghostty_config_files().is_empty());
        assert_eq!(dirs.expand_home("~/x"), PathBuf::from("~/x"));
    }

    #[test]
    fn recognizes_the_home_directory() {
        let dirs = dirs(&[("HOME", "/home/me")]);
        assert!(dirs.is_home(Path::new("/home/me")));
        assert!(!dirs.is_home(Path::new("/home")));
        assert!(!Dirs::default().is_home(Path::new("/")));
    }
}
