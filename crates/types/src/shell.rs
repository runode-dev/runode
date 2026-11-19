//! shell 的种类、shell 集成的开关，shell 集成报告的函数名和运行完的命令，以及往终端里打路径时照哪种
//! shell 的写法加引号。

use std::path::{Path, PathBuf};

use crate::session::SessionMeta;

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

/// 主机上装着、新建终端时可以挑的一个 shell。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ShellProfile {
    pub kind: ShellProfileKind,
    /// 起它时交给 PTY 的程序：PATH 上找得到的写程序名，否则写全路径。
    pub program: String,
    /// 没指定 shell 时宿主起的就是它。
    #[serde(default)]
    pub default: bool,
}

/// 可挑的 shell 是哪一种；显示的名字由前端按种类翻译。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellProfileKind {
    /// PowerShell 7（`pwsh.exe`）。
    PowerShell,
    /// 系统自带的 Windows PowerShell（`powershell.exe`）。
    WindowsPowerShell,
    /// 命令提示符（`cmd.exe`）。
    Cmd,
    /// Git for Windows 带的 bash。
    GitBash,
    /// 前端不认识的种类，前端不列出。
    #[serde(other)]
    Other,
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

/// 往终端里打路径、地址时照哪种 shell 的写法加引号。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quoting {
    /// sh、bash（包括 Windows 上的 Git Bash）、zsh：单引号，里面的单引号写成 `'\''`。
    Posix,
    /// fish：单引号里 `\\`、`\'` 仍是转义，反斜杠和单引号前各加一个反斜杠。
    Fish,
    /// PowerShell：单引号里什么都不展开，单引号写两遍；它把 `‘’‚‛` 也当单引号。
    PowerShell,
    /// elvish：单引号里什么都不展开，单引号写两遍。
    Elvish,
    /// cmd：双引号。Windows 的文件名里没有双引号。
    /// shortcut: cmd 在双引号里照样展开定义过的 `%名字%`，交互的 cmd 没有转义写法；有人撞上再拆开引号插 `^`。
    Cmd,
    /// nushell：单引号里不转义，本身带单引号时用 raw string `r#'…'#`。
    Nushell,
}

/// `text` 写成 PowerShell 的单引号字符串：里面什么都不展开，单引号写两遍，它当成单引号的 `‘’‚‛` 也写两遍。
pub fn powershell_literal(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('\'');
    for c in text.chars() {
        if matches!(c, '\'' | '‘' | '’' | '‚' | '‛') {
            quoted.push(c);
        }
        quoted.push(c);
    }
    quoted.push('\'');
    quoted
}

impl Quoting {
    /// 终端前台的 shell 的写法；前台不是 shell 或者认不出时按 `Quoting::platform`。
    pub fn of_session(meta: &SessionMeta) -> Self {
        let shell = meta.foreground.as_deref().filter(|_| meta.foreground_is_shell).and_then(Shell::detect);
        shell.map_or_else(Self::platform, Self::of_shell)
    }

    /// 程序名是 `program` 的 shell 的写法，认不出时按 `Quoting::platform`。
    pub fn of_program(program: &str) -> Self {
        Shell::detect(program).map_or_else(Self::platform, Self::of_shell)
    }

    /// 还不知道是哪种 shell、只知道系统时（远端新开的终端，`os` 是 `std::env::consts::OS` 的写法）：
    /// Windows 上用双引号，cmd 和 PowerShell 都认；别的照 sh 的写法。
    pub fn of_os(os: &str) -> Self {
        if os == "windows" { Self::Cmd } else { Self::Posix }
    }

    fn of_shell(shell: Shell) -> Self {
        match shell {
            Shell::Zsh | Shell::Bash => Self::Posix,
            Shell::Fish => Self::Fish,
            Shell::PowerShell => Self::PowerShell,
            Shell::Elvish => Self::Elvish,
            Shell::Cmd => Self::Cmd,
            Shell::Nushell => Self::Nushell,
        }
    }

    /// 不知道是哪种 shell 时（前台是 agent 这类程序）：Unix 上照 sh 的写法；Windows 上用双引号，
    /// agent 和各种 shell 都认，只是 PowerShell 和 Git Bash 在双引号里还会展开 `$`。
    fn platform() -> Self {
        if cfg!(windows) { Self::Cmd } else { Self::Posix }
    }

    /// 不用引号也不用转义的常见字符。
    fn is_plain(self, c: char) -> bool {
        c.is_alphanumeric()
            || match self {
                Self::Posix | Self::Fish => matches!(c, '/' | '.' | '_' | '-' | '+' | '@' | '%' | ':' | ',' | '='),
                // PowerShell 把 `,` 当数组、`@` 当展开，elvish 和 nushell 的裸字也不全认它们；`\` 在这几种里不是转义。
                Self::PowerShell | Self::Elvish | Self::Nushell => {
                    matches!(c, '/' | '\\' | '.' | '_' | '-' | '+' | ':')
                }
                // cmd 把 `,`、`=`、`;` 当参数之间的分隔。
                Self::Cmd => matches!(c, '/' | '\\' | '.' | '_' | '-' | '+' | '@' | ':'),
            }
    }

    /// `text` 只含常见字符时原样，否则括起来。开头是 `=` 或 `%` 时也括起来，zsh 会把 `=foo` 展开成命令的路径。
    pub fn quote(self, text: &str) -> String {
        let plain = !text.is_empty() && !text.starts_with(['=', '%']) && text.chars().all(|c| self.is_plain(c));
        if plain {
            return text.to_owned();
        }
        match self {
            Self::Posix => format!("'{}'", text.replace('\'', r"'\''")),
            Self::Fish => format!("'{}'", text.replace('\\', r"\\").replace('\'', r"\'")),
            Self::PowerShell => powershell_literal(text),
            Self::Elvish => format!("'{}'", text.replace('\'', "''")),
            Self::Cmd => format!("\"{text}\""),
            Self::Nushell if !text.contains('\'') => format!("'{text}'"),
            Self::Nushell => {
                let mut hashes = "#".to_owned();
                while text.contains(&format!("'{hashes}")) {
                    hashes.push('#');
                }
                format!("r{hashes}'{text}'{hashes}")
            }
        }
    }

    /// 和 `quote` 一样只放过常见字符；sh 和 fish 的写法里其余字符前面加反斜杠，像访达把文件拖进终端时那样，
    /// 别的写法里反斜杠不是转义，照 `quote` 括起来。
    /// 图片路径用这种写法：Claude Code 按「空格后跟 `/`」拆开几个路径，引号只在整段两头时才去掉，
    /// 几个带引号的路径连在一起它就认不出来了；反斜杠转义它和 Codex 都能还原。带换行时照 `quote` 括起来：
    /// 反斜杠加换行在 sh 里是续行，换行会被吃掉。
    pub fn escape(self, text: &str) -> String {
        if !matches!(self, Self::Posix | Self::Fish) || text.contains('\n') {
            return self.quote(text);
        }
        let mut escaped = String::with_capacity(text.len());
        for (i, c) in text.chars().enumerate() {
            let plain = self.is_plain(c) && !(i == 0 && matches!(c, '=' | '%'));
            if !plain {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        escaped
    }
}
