//! shell 集成：启动 zsh、bash、fish 时自动加载一段脚本，让 shell 在提示符前后发 OSC 133
//! 标记。终端据此分清提示符、用户输入和命令输出，单击挪光标、删除选区和按命令跳转都靠它。
//!
//! 脚本编进二进制，第一次用到时写到用户自己的缓存目录里；用户的 shell 配置照常加载，
//! 不需要改动。

use std::{
    io,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use portable_pty::CommandBuilder;
use runode_shared_types::{
    settings::TermSettings,
    shell::{IntegrationMode, Shell},
};

/// 集成脚本定义的函数：换成一个新的 shell，重新读用户配置，集成和报告口令照旧。
pub(crate) const RELOAD_FUNCTION: &str = "runode-reload";

const ZSH_ENV: &str = include_str!("../shell-integration/zsh/.zshenv");
const ZSH_INTEGRATION: &str = include_str!("../shell-integration/zsh/runode-integration.zsh");
const BASH_RC: &str = include_str!("../shell-integration/bash/runode.bash");
const FISH_CONF: &str = include_str!("../shell-integration/fish/runode.fish");

/// 按 `mode` 给启动 `program` 的命令加上集成，并补上登录 shell 的参数；集成脚本开哪些功能
/// 按 `features`（见 `features`）。集成脚本写不出来或认不出 shell 时照常以登录 shell 启动，不注入。
///
/// 注入了集成时返回这个 shell 的报告口令：随机生成，经环境变量 `RUNODE_REPORT_TOKEN` 交给
/// 集成脚本，脚本读进不导出的变量后马上从环境里删掉，子进程继承不到。shell 报告 PATH 等信息时
/// 带上它，终端据此认出报告确实来自这个 shell，而不是被打印到屏幕上的别的输出伪造的。没注入
/// 或者生成不了口令时为 `None`，这时脚本不发报告。
pub fn prepare(mode: IntegrationMode, program: &str, features: &str, cmd: &mut CommandBuilder) -> Option<String> {
    let shell = match mode {
        IntegrationMode::Off => None,
        IntegrationMode::Detect => Shell::detect(program),
        IntegrationMode::Force(shell) => Some(shell),
    };
    let dir = shell.and_then(|_| match install_dir() {
        Ok(dir) => Some(dir),
        Err(err) => {
            tracing::warn!("shell integration unavailable: {err}");
            None
        }
    });
    let injected = match shell.zip(dir) {
        Some((Shell::Zsh, dir)) => {
            // zsh 从 ZDOTDIR 读启动文件；原来的值交给集成目录里的 .zshenv 还原。
            if let Some(original) = std::env::var_os("ZDOTDIR") {
                cmd.env("RUNODE_ZSH_ZDOTDIR", original);
            }
            cmd.env("ZDOTDIR", dir.join("zsh"));
            cmd.arg("-l");
            true
        }
        Some((Shell::Bash, dir)) => {
            // --rcfile 只对非登录的交互式 shell 有效，登录配置由脚本自己加载。
            cmd.arg("--rcfile");
            cmd.arg(dir.join("bash/runode.bash"));
            true
        }
        Some((Shell::Fish, dir)) => {
            let data = dir.join("fish-data");
            let mut dirs = vec![data.clone()];
            // XDG_DATA_DIRS 没设时 fish 用这两个默认目录，加了我们的之后要显式带上。
            let inherited = std::env::var_os("XDG_DATA_DIRS")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            dirs.extend(std::env::split_paths(&inherited));
            cmd.arg("-l");
            match std::env::join_paths(dirs) {
                Ok(joined) => {
                    cmd.env("XDG_DATA_DIRS", joined);
                    cmd.env("RUNODE_FISH_DATA_DIR", data);
                    true
                }
                Err(err) => {
                    tracing::warn!("shell integration unavailable: {err}");
                    false
                }
            }
        }
        None => {
            cmd.arg("-l");
            false
        }
    };
    if !injected {
        return None;
    }
    cmd.env("RUNODE_SHELL_FEATURES", features);
    let token = report_token()?;
    cmd.env("RUNODE_REPORT_TOKEN", &token);
    Some(token)
}

/// 经环境变量 `RUNODE_SHELL_FEATURES` 告诉集成脚本开哪些功能：逗号隔开的功能名，和 Ghostty 的
/// `GHOSTTY_SHELL_FEATURES` 一个写法。现在只有 `cursor`，带上提示符上的竖线闪不闪：
/// `cursor:blink` 或 `cursor:steady`，跟配置的 `cursor_blink` 走，没配时不闪。脚本读进不导出的
/// 变量后马上从环境里删掉，在里面运行的程序继承不到。
pub(crate) fn features(settings: &TermSettings) -> String {
    let mut features = Vec::new();
    if settings.shell_features.cursor {
        features.push(if settings.cursor_blink == Some(true) { "cursor:blink" } else { "cursor:steady" });
    }
    features.join(",")
}

/// 新的报告口令：128 位随机数，写成 32 个十六进制字符。读不到随机数时为 `None`。
fn report_token() -> Option<String> {
    use std::io::Read as _;
    let mut bytes = [0u8; 16];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut random| random.read_exact(&mut bytes));
    if let Err(err) = read {
        tracing::warn!("shell reports disabled, no random token: {err}");
        return None;
    }
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// 脚本所在的目录，每次启动应用时按二进制里的版本重写一遍。
fn install_dir() -> Result<&'static Path, String> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DIR.get_or_init(|| install().map_err(|err| err.to_string())).as_ref().map(PathBuf::as_path).map_err(Clone::clone)
}

fn install() -> io::Result<PathBuf> {
    let dir =
        runode_paths::Dirs::from_env().shell_integration_dir().ok_or_else(|| io::Error::other("no home directory"))?;
    for (path, contents) in [
        ("zsh/.zshenv", ZSH_ENV),
        ("zsh/runode-integration.zsh", ZSH_INTEGRATION),
        ("bash/runode.bash", BASH_RC),
        ("fish-data/fish/vendor_conf.d/runode.fish", FISH_CONF),
    ] {
        let path = dir.join(path);
        if std::fs::read_to_string(&path).is_ok_and(|existing| existing == contents) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_tokens_are_random_and_only_given_to_integrated_shells() {
        let token = report_token().unwrap();
        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(report_token().unwrap(), token);
        let mut cmd = CommandBuilder::new("/bin/zsh");
        assert_eq!(prepare(IntegrationMode::Off, "/bin/zsh", "cursor:steady", &mut cmd), None);
        assert_eq!(cmd.get_env("RUNODE_REPORT_TOKEN"), None);
        assert_eq!(cmd.get_env("RUNODE_SHELL_FEATURES"), None);
    }

    #[test]
    fn the_prompt_cursor_blinks_only_when_configured() {
        let mut settings = TermSettings::default();
        assert_eq!(features(&settings), "cursor:steady");
        settings.cursor_blink = Some(true);
        assert_eq!(features(&settings), "cursor:blink");
        settings.shell_features.cursor = false;
        assert_eq!(features(&settings), "");
    }
}
