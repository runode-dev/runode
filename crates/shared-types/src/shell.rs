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
