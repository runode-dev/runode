//! shell 集成：启动 zsh、bash、fish、elvish、nushell 时自动加载一段脚本，让 shell 在提示符前后
//! 发 OSC 133 标记（nushell 自己会发，脚本只补报告）。终端据此分清提示符、用户输入和命令输出，
//! 单击挪光标、删除选区和按命令跳转都靠它。
//!
//! 脚本编进二进制，每次起 shell 前写到用户自己的缓存目录里（和二进制里一样的不重写）；用户的
//! shell 配置照常加载，不需要改动。

use std::{
    io,
    path::{Path, PathBuf},
};

use portable_pty::CommandBuilder;
use runode_types::{
    settings::TermSettings,
    shell::{IntegrationMode, Shell},
};

/// 集成脚本定义的函数：换成一个新的 shell，重新读用户配置，集成和报告口令照旧。
pub(crate) const RELOAD_FUNCTION: &str = "runode-reload";

/// zsh 和 bash（4.4 起）的集成在各个键位（emacs、vi 插入、vi 命令）里都绑着 `RELOAD_KEY`，按下就把整行
/// 换成 ` runode-reload` 执行；绑着的函数叫这个名字，集成报告了它就说明绑上了。
pub(crate) const RELOAD_KEY_FUNCTION: &str = "runode-reload-key";

/// 见 `RELOAD_KEY_FUNCTION`。终端不会发出这个序列，用户也按不出来。
pub(crate) const RELOAD_KEY: &[u8] = b"\x1b[6973~";

const ZSH_ENV: &str = include_str!("../shell-integration/zsh/.zshenv");
const ZSH_INTEGRATION: &str = include_str!("../shell-integration/zsh/runode-integration.zsh");
const BASH_RC: &str = include_str!("../shell-integration/bash/runode.bash");
const FISH_CONF: &str = include_str!("../shell-integration/fish/runode.fish");
const ELVISH_RC: &str = include_str!("../shell-integration/elvish/runode.elv");
const NUSHELL_CONF: &str = include_str!("../shell-integration/nushell/runode.nu");

/// 按 `mode` 给启动 `program` 的命令加上集成，并补上登录 shell 的参数；集成脚本开哪些功能
/// 按 `features`（见 `features`）。集成脚本写不出来或认不出 shell 时照常以登录 shell 启动，不注入。
/// `mode` 指定了种类而 `program` 认得出是另一种 shell 时，按程序名认出的那种注入。
///
/// 注入了集成时返回这个 shell 的报告口令和装着它的文件（见 `write_token_file`）。口令随机生成，
/// shell 报告 PATH 等信息时带上它，终端据此认出报告确实来自这个 shell，而不是被打印到屏幕上的
/// 别的输出伪造的。口令本身不放进环境变量：macOS 的 `ps -E`、Linux 的 `/proc/<pid>/environ` 读的
/// 是进程启动时的环境，脚本事后删掉变量也改不掉，同一用户的任何程序都读得到。环境里只给文件的
/// 路径（`RUNODE_REPORT_TOKEN_FILE`），集成脚本读出口令后马上删掉文件和这个变量；shell 没读就退出
/// 时由 `Pty` 删。没注入、生成不了口令或者写不出文件时为 `None`，这时脚本不发报告。
pub fn prepare(
    dirs: &runode_paths::Dirs,
    mode: IntegrationMode,
    program: &str,
    features: &str,
    cmd: &mut CommandBuilder,
) -> Option<(String, PathBuf)> {
    let shell = resolve(mode, program);
    let dir = shell.and_then(|_| match install(dirs) {
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
            // fish 4 自己也发 OSC 133，连 `read` 的提示符都标成提示符，还在跑的命令就被从中间切断；
            // 集成脚本自己标，关掉 fish 的。不认得这个功能名的老 fish 忽略它。
            cmd.args(["--features", "no-mark-prompt", "-l"]);
            prepend_data_dir(cmd, dir.join("fish-data"), "RUNODE_FISH_DATA_DIR")
        }
        Some((Shell::Nushell, dir)) => {
            // nushell 读完用户的 config.nu 以后加载数据目录里 nushell/vendor/autoload 下的脚本。
            cmd.arg("-l");
            prepend_data_dir(cmd, dir.join("nushell-data"), "RUNODE_NU_DATA_DIR")
        }
        Some((Shell::Elvish, dir)) => {
            // elvish 不自动加载别的脚本：换成集成的 rc，它先加载用户自己的 rc.elv。elvish 没有
            // 登录 shell 一说，`-l` 是个空操作，不加。
            cmd.arg("-rc");
            cmd.arg(dir.join("elvish/runode.elv"));
            true
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
    let file = match write_token_file(dirs, &token) {
        Ok(file) => file,
        Err(err) => {
            tracing::warn!("shell reports disabled, cannot hand over the token: {err}");
            return None;
        }
    };
    cmd.env("RUNODE_REPORT_TOKEN_FILE", &file);
    Some((token, file))
}

/// 把 `token` 写进运行时目录（只有自己能进）里一个新建的、只有自己能读的文件，返回它的路径。
/// 文件名另取随机数，免得被人抢先建好同名文件或者猜着去读。
fn write_token_file(dirs: &runode_paths::Dirs, token: &str) -> io::Result<PathBuf> {
    use std::{io::Write as _, os::unix::fs::OpenOptionsExt as _};
    let name = report_token().ok_or_else(|| io::Error::other("no random file name"))?;
    let dir = dirs.create_runtime_dir()?;
    remove_stale_token_files(&dir);
    let path = dir.join(format!("report-token-{name}"));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    if let Err(err) = writeln!(file, "{token}") {
        let _ = std::fs::remove_file(&path);
        return Err(err);
    }
    Ok(path)
}

/// 删掉 `dir` 里一天前写下的口令文件。shell 启动时就读、读完就删，还在的是没人会读的了：集成脚本没跑到
/// 那一步（比如用户配置里先 exec 了别的程序）的会话交接给了新宿主，没人记着它的文件；宿主崩溃时也会留下。
fn remove_stale_token_files(dir: &Path) {
    const STALE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry.file_name().to_string_lossy().starts_with("report-token-")
            && entry
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|at| at.elapsed().is_ok_and(|age| age > STALE));
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 按 `mode` 定给 `program` 注入哪种 shell 的集成，不注入时为 `None`。指定的种类只对认不出程序名的
/// shell 管用（比如一个叫 sh 的符号链接）。程序名认得出是另一种 shell 时按它自己的来：把 bash 的
/// --rcfile、elvish 的 -rc 这类参数传给 fish，fish 认不出就退出，终端刚开就关。
fn resolve(mode: IntegrationMode, program: &str) -> Option<Shell> {
    match mode {
        IntegrationMode::Off => None,
        IntegrationMode::Detect => Shell::detect(program),
        IntegrationMode::Force(forced) => match Shell::detect(program) {
            Some(detected) if detected != forced => {
                tracing::warn!(
                    "shell-integration is set to {forced:?} but {program} is {detected:?}; using its own integration"
                );
                Some(detected)
            }
            _ => Some(forced),
        },
    }
}

/// 把 `data` 排到 `XDG_DATA_DIRS` 最前面，fish、nushell 启动时从那里自动加载集成；`var` 告诉
/// 集成脚本加的是哪一项，脚本加载后把它去掉。拼不出来时不注入，返回 false。
fn prepend_data_dir(cmd: &mut CommandBuilder, data: PathBuf, var: &str) -> bool {
    let mut dirs = vec![data.clone()];
    // XDG_DATA_DIRS 没设时 fish、nushell 用这两个默认目录，加了我们的之后要显式带上。
    let inherited = std::env::var_os("XDG_DATA_DIRS")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(std::env::split_paths(&inherited));
    match std::env::join_paths(dirs) {
        Ok(joined) => {
            cmd.env("XDG_DATA_DIRS", joined);
            cmd.env(var, data);
            true
        }
        Err(err) => {
            tracing::warn!("shell integration unavailable: {err}");
            false
        }
    }
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

/// 把脚本写进 `dirs` 的集成目录，返回这个目录。每次起 shell 都写一遍，不记住写过：宿主跑着时用户
/// 可能清掉了缓存目录，那样 zsh 的 ZDOTDIR、bash 的 --rcfile 就指着不存在的文件，连用户自己的
/// 配置都不加载。内容没变时只读不写，几个会话同时起 shell 也不会互相踩（见 `install_into`）。
fn install(dirs: &runode_paths::Dirs) -> io::Result<PathBuf> {
    let dir = dirs.shell_integration_dir().ok_or_else(|| io::Error::other("no home directory"))?;
    install_into(&dir)?;
    Ok(dir)
}

/// 把脚本写进 `dir`，和二进制里一样的不重写。升级交接时旧宿主可能正好在起新 shell，所以整个换上
/// （`runode_paths::replace_file`），不原地覆盖：shell 读到的要么是旧脚本，要么是新脚本，不会是写了一半的。
fn install_into(dir: &Path) -> io::Result<()> {
    for (path, contents) in [
        ("zsh/.zshenv", ZSH_ENV),
        ("zsh/runode-integration.zsh", ZSH_INTEGRATION),
        ("bash/runode.bash", BASH_RC),
        // fish 的 conf.d 同名文件只加载一个，用户目录的优先：不能叫 runode.fish，那是 `runode completions fish`
        // 建议装进 ~/.config/fish/conf.d 的补全脚本的名字，装了它集成就不加载。
        ("fish-data/fish/vendor_conf.d/runode-integration.fish", FISH_CONF),
        ("elvish/runode.elv", ELVISH_RC),
        ("nushell-data/nushell/vendor/autoload/runode.nu", NUSHELL_CONF),
    ] {
        let path = dir.join(path);
        if std::fs::read_to_string(&path).is_ok_and(|existing| existing == contents) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        runode_paths::replace_file(&path, contents.as_bytes())?;
    }
    // 删掉旧名 runode.fish：和现名的脚本并存会各加载一遍。
    match std::fs::remove_file(dir.join("fish-data/fish/vendor_conf.d/runode.fish")) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    Ok(())
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
        // CommandBuilder 带着本进程的环境，测试本身跑在 runode 的终端里时那里可能也有这两个变量。
        cmd.env_clear();
        let dirs = runode_paths::Dirs::default();
        assert_eq!(prepare(&dirs, IntegrationMode::Off, "/bin/zsh", "cursor:steady", &mut cmd), None);
        assert_eq!(cmd.get_env("RUNODE_REPORT_TOKEN_FILE"), None);
        assert_eq!(cmd.get_env("RUNODE_SHELL_FEATURES"), None);
    }

    /// 口令不进环境变量（启动时的环境同一用户的程序都读得到），只交一个只有自己能读的文件。
    #[test]
    fn the_token_is_handed_over_in_a_private_file_not_the_environment() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = std::env::temp_dir().join(format!("runode-shell-integration-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let dirs = runode_paths::Dirs::from_vars(|key| (key == "HOME").then(|| home.clone().into()));
        let mut cmd = CommandBuilder::new("/bin/zsh");
        cmd.env_clear();
        let (token, file) = prepare(&dirs, IntegrationMode::Detect, "/bin/zsh", "", &mut cmd).unwrap();
        for (key, value) in cmd.iter_extra_env_as_str() {
            assert!(!value.contains(&token), "{key} carries the token");
        }
        assert_eq!(cmd.get_env("RUNODE_REPORT_TOKEN_FILE"), Some(file.as_os_str()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), format!("{token}\n"));
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(file.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        // 一天前没人读的口令文件下次写时清掉，刚写的留着。
        let stale = file.with_file_name("report-token-stale");
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 60 * 60);
        std::fs::File::create(&stale).unwrap().set_modified(old).unwrap();
        let (_, second) = prepare(&dirs, IntegrationMode::Detect, "/bin/zsh", "", &mut cmd).unwrap();
        assert!(!stale.exists() && file.exists() && second.exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_forced_shell_kind_yields_to_a_program_recognized_as_another_shell() {
        let force = |shell| IntegrationMode::Force(shell);
        // fish 认不出 bash 的 --rcfile，会直接退出。
        assert_eq!(resolve(force(Shell::Bash), "/opt/homebrew/bin/fish"), Some(Shell::Fish));
        assert_eq!(resolve(force(Shell::Elvish), "-zsh"), Some(Shell::Zsh));
        // 认不出名字的程序才按指定的办。
        assert_eq!(resolve(force(Shell::Bash), "/usr/local/bin/sh"), Some(Shell::Bash));
        assert_eq!(resolve(force(Shell::Zsh), "/bin/zsh"), Some(Shell::Zsh));
        assert_eq!(resolve(IntegrationMode::Detect, "/usr/local/bin/sh"), None);
        assert_eq!(resolve(IntegrationMode::Off, "/bin/zsh"), None);
    }

    #[test]
    fn scripts_are_replaced_whole_not_rewritten_in_place() {
        let dir = std::env::temp_dir().join(format!("runode-shell-integration-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let zshenv = dir.join("zsh/.zshenv");
        std::fs::create_dir_all(zshenv.parent().unwrap()).unwrap();
        std::fs::write(&zshenv, "old").unwrap();
        // 硬链接和原文件共用一个 inode：原地覆盖时它也跟着变，整个换上时它还是旧的那份。
        let reader = dir.join("reader");
        std::fs::hard_link(&zshenv, &reader).unwrap();
        install_into(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(&zshenv).unwrap(), ZSH_ENV);
        assert_eq!(std::fs::read_to_string(&reader).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(dir.join("bash/runode.bash")).unwrap(), BASH_RC);
        let leftovers: Vec<_> = std::fs::read_dir(dir.join("zsh"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        // 旧名字的 fish 集成换成新名字，不两份都留着。
        let fish = dir.join("fish-data/fish/vendor_conf.d");
        std::fs::write(fish.join("runode.fish"), "old").unwrap();
        install_into(&dir).unwrap();
        assert!(!fish.join("runode.fish").exists());
        assert_eq!(std::fs::read_to_string(fish.join("runode-integration.fish")).unwrap(), FISH_CONF);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 宿主跑着时用户清掉了缓存目录，下一次起 shell 时脚本又写回去。
    #[test]
    fn scripts_are_written_again_after_the_cache_is_cleared() {
        let home = std::env::temp_dir().join(format!("runode-shell-integration-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let dirs = runode_paths::Dirs::from_vars(|key| (key == "HOME").then(|| home.clone().into()));
        let dir = install(&dirs).unwrap();
        assert!(dir.starts_with(&home));
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(install(&dirs).unwrap(), dir);
        assert_eq!(std::fs::read_to_string(dir.join("zsh/runode-integration.zsh")).unwrap(), ZSH_INTEGRATION);
        assert_eq!(std::fs::read_to_string(dir.join("bash/runode.bash")).unwrap(), BASH_RC);
        let _ = std::fs::remove_dir_all(&home);
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
