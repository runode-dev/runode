//! shell 的种类、shell 集成的开关，以及 shell 集成报告的函数名和运行完的命令。

use std::path::{Path, PathBuf};

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

/// 配置项 `shell-integration-features` 里 runode 做的几项。写法和 Ghostty 一样；Ghostty 的
/// `sudo`、`title`、`ssh-env`、`ssh-terminfo`、`path` 也认，但不做。
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
    Elvish,
    Nushell,
    /// PowerShell 7（`pwsh`）和系统自带的 Windows PowerShell（`powershell`）。
    PowerShell,
    /// Windows 的命令提示符 `cmd.exe`。
    Cmd,
}

impl Shell {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "zsh" => Some(Self::Zsh),
            "bash" => Some(Self::Bash),
            "fish" => Some(Self::Fish),
            "elvish" => Some(Self::Elvish),
            "nushell" => Some(Self::Nushell),
            "powershell" => Some(Self::PowerShell),
            "cmd" => Some(Self::Cmd),
            _ => None,
        }
    }

    /// 按可执行文件的名字认 shell，不分大小写；登录 shell 的 argv[0] 可能带前缀 `-`，Windows 上的带
    /// `.exe`。nushell 的程序叫 `nu`，PowerShell 7 的叫 `pwsh`。
    pub fn detect(program: &str) -> Option<Self> {
        let name = Path::new(program).file_name()?.to_str()?.trim_start_matches('-').to_ascii_lowercase();
        match name.strip_suffix(".exe").unwrap_or(&name) {
            "nu" => Some(Self::Nushell),
            "pwsh" => Some(Self::PowerShell),
            name => Self::from_name(name),
        }
    }
}

/// shell 集成报告的名字。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ShellNames {
    /// shell 里定义的函数，看集成脚本装好了没有（有没有 `runode-reload`）。
    pub functions: Vec<String>,
}

/// shell 集成报告运行完的一条命令。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FinishedCommand {
    pub cmd: String,
    /// 命令在哪个目录里运行。
    #[serde(default, serialize_with = "crate::lossy_path::serialize_opt")]
    pub cwd: Option<PathBuf>,
    /// 退出码；shell 没报告时为空。
    #[serde(default)]
    pub exit: Option<i32>,
    /// 开始运行的时刻，Unix 秒。
    pub ts: u64,
}
