//! shell 的种类、shell 集成的开关，以及 shell 集成报告的名字。

use std::path::Path;

/// 配置项 `shell-integration` 的取值。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IntegrationMode {
    /// 按 shell 的程序名判断用哪一种。
    #[default]
    Detect,
    /// 不注入。
    Off,
    /// 不管程序名，当作这种 shell 注入。
    Force(Shell),
}

/// 配置项 `shell-integration-features` 里 runode 做的几项。写法和 Ghostty 一样，Ghostty 的
/// `sudo`、`title`、`ssh-env`、`ssh-terminfo`、`path` 也认，但不做：标题由宿主按前台程序和目录
/// 自己定，`TERM` 是 `xterm-256color`，不用另装 terminfo，runode 所在的目录宿主已经加进了 `PATH`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ShellFeatures {
    /// 在提示符上把光标换成竖线，zsh 的 vi 命令模式里换成方块，跑命令前恢复成配置的样式。
    pub cursor: bool,
}

impl Default for ShellFeatures {
    fn default() -> Self {
        Self { cursor: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }

    /// 按可执行文件的名字认 shell；登录 shell 的 argv[0] 可能带前缀 `-`。
    pub fn detect(program: &str) -> Option<Self> {
        let name = Path::new(program).file_name()?.to_str()?;
        Self::from_name(name.trim_start_matches('-'))
    }
}

/// shell 集成报告的各种名字，补命令名时用。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ShellNames {
    pub aliases: Vec<String>,
    /// 别名展开成什么：`(名字, 值)`。
    pub alias_values: Vec<(String, String)>,
    pub functions: Vec<String>,
    pub builtins: Vec<String>,
    pub keywords: Vec<String>,
}
