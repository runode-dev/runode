//! macOS 上不起 git 就判断它跑不跑得起来。没装命令行开发者工具时 `/usr/bin/git` 是 xcode-select 装的壳子，
//! 一执行就弹出「要安装命令行开发者工具吗？」的对话框；壳子只是转去执行选中的开发者目录里的 git，所以先问
//! `xcode-select -p` 选中的是哪个目录、那里有没有 git。`xcode-select` 只有 `--install` 弹框，`-p` 只打印路径，
//! 没有开发者目录时以非零状态退出。

use std::{
    ffi::OsStr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

/// xcode-select 装的 git 壳子。
const SHIM: &str = "/usr/bin/git";

/// 没设 `PATH` 时按名字找程序的目录，和 `posix_spawnp` 的默认一样。
const DEFAULT_PATH: &str = "/usr/bin:/bin";

/// 查到跑不起来以后，过了这么久再有人要跑 git 就重查：装好命令行工具后不用重开 Runode 就能用上。
const MISSING_RECHECK: Duration = Duration::from_secs(2);

/// 查到跑得起来以后，过了这么久再重查：命令行工具被删掉、`PATH` 变了时也认得出来。
const USABLE_RECHECK: Duration = Duration::from_secs(60);

/// 上一次查的结果和查的时候。
#[derive(Clone, Copy)]
struct Checked {
    usable: bool,
    at: Instant,
}

static LAST: Mutex<Option<Checked>> = Mutex::new(None);

/// 按名字起 `git` 会不会执行到弹框的壳子以外的、跑得起来的 git。结果记着，按 `MISSING_RECHECK`、`USABLE_RECHECK`
/// 过期；`recheck` 时不看记着的，当场查。查的时候拿着锁，同时要跑 git 的线程等它查完，不各查一遍。
pub(crate) fn git_runnable(recheck: bool) -> bool {
    let mut last = LAST.lock().unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    if let Some(usable) = last.filter(|_| !recheck).and_then(|checked| still_valid(checked, now)) {
        return usable;
    }
    let path = std::env::var_os("PATH");
    let found = find_on_path(path.as_deref().unwrap_or(OsStr::new(DEFAULT_PATH)), is_executable);
    let usable = usable(found.as_deref(), selected_developer_dir, is_executable);
    *last = Some(Checked { usable, at: now });
    usable
}

/// 记着的结果还没过期时是它。
fn still_valid(checked: Checked, now: Instant) -> Option<bool> {
    let ttl = if checked.usable { USABLE_RECHECK } else { MISSING_RECHECK };
    (now.saturating_duration_since(checked.at) < ttl).then_some(checked.usable)
}

/// 按名字起 `git` 时执行的是 `PATH` 里哪一个：按顺序第一个能执行的文件。空的一项不算。
fn find_on_path(path: &OsStr, is_executable: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join("git"))
        .find(|git| is_executable(git))
}

/// `found`（`find_on_path` 找到的）跑不跑得起来。找不到时不行；不是壳子时行；是壳子时要 `developer_dir`
/// （`selected_developer_dir`）给出选中的目录，而且那里的 `usr/bin/git` 能执行：`DEVELOPER_DIR` 设了不存在的
/// 目录时 `xcode-select -p` 照样打印它、正常退出。
fn usable(
    found: Option<&Path>,
    developer_dir: impl FnOnce() -> Option<PathBuf>,
    is_executable: impl Fn(&Path) -> bool,
) -> bool {
    match found {
        None => false,
        Some(git) if git != Path::new(SHIM) => true,
        Some(_) => developer_dir().is_some_and(|dir| is_executable(&dir.join("usr/bin/git"))),
    }
}

/// `xcode-select -p` 给出的开发者目录，它失败时为空。按绝对路径起，不经 `PATH`。
fn selected_developer_dir() -> Option<PathBuf> {
    let mut command = Command::new("/usr/bin/xcode-select");
    command.arg("-p").stdin(Stdio::null()).stderr(Stdio::null());
    let output = command.output().ok()?;
    developer_dir(output.status.success(), &output.stdout)
}

/// 从 `xcode-select -p` 的退出状态和标准输出读出开发者目录：要正常退出，去掉结尾的换行后不是空的。
fn developer_dir(success: bool, stdout: &[u8]) -> Option<PathBuf> {
    if !success {
        return None;
    }
    let dir = stdout.strip_suffix(b"\n").unwrap_or(stdout);
    let dir: &OsStr = std::os::unix::ffi::OsStrExt::from_bytes(dir);
    (!dir.is_empty()).then(|| PathBuf::from(dir))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files<'a>(present: &'a [&str]) -> impl Fn(&Path) -> bool + 'a {
        move |path| present.iter().any(|file| path == Path::new(file))
    }

    #[test]
    fn finds_the_first_git_on_the_path() {
        let present = ["/usr/bin/git", "/opt/homebrew/bin/git"];
        let found = |path: &str| find_on_path(OsStr::new(path), files(&present));
        assert_eq!(found("/opt/homebrew/bin:/usr/bin:/bin"), Some(PathBuf::from("/opt/homebrew/bin/git")));
        assert_eq!(found("/usr/bin:/opt/homebrew/bin"), Some(PathBuf::from(SHIM)));
        assert_eq!(found("::/usr/bin/:/bin"), Some(PathBuf::from(SHIM)));
        assert_eq!(found("/bin:/usr/local/bin"), None);
        assert_eq!(found(""), None);
    }

    #[test]
    fn git_outside_usr_bin_is_used_without_asking_xcode_select() {
        let asked = std::cell::Cell::new(false);
        let developer_dir = || {
            asked.set(true);
            None
        };
        assert!(usable(Some(Path::new("/opt/homebrew/bin/git")), developer_dir, files(&[])));
        assert!(!asked.get());
        assert!(!usable(None, || None, files(&[])));
    }

    #[test]
    fn the_shim_needs_a_selected_developer_dir_with_git() {
        let shim = Some(Path::new(SHIM));
        let clt = "/Library/Developer/CommandLineTools";
        let xcode = "/Applications/Xcode.app/Contents/Developer";
        let present = [SHIM, "/Library/Developer/CommandLineTools/usr/bin/git"];
        assert!(usable(shim, || Some(clt.into()), files(&present)));
        let xcode_git = ["/Applications/Xcode.app/Contents/Developer/usr/bin/git"];
        assert!(usable(shim, || Some(xcode.into()), files(&xcode_git)));
        // 没装命令行工具：xcode-select -p 失败。
        assert!(!usable(shim, || None, files(&[SHIM])));
        // 选中的目录里没有 git（`DEVELOPER_DIR` 指着不存在的目录，或者 Xcode 被删了），装着的命令行工具也不算：
        // 壳子只认选中的那个。
        assert!(!usable(shim, || Some("/nonexistent".into()), files(&present)));
        assert!(!usable(shim, || Some(xcode.into()), files(&present)));
    }

    #[test]
    fn reads_the_developer_dir_from_xcode_select() {
        let dir = Some(PathBuf::from("/Library/Developer/CommandLineTools"));
        assert_eq!(developer_dir(true, b"/Library/Developer/CommandLineTools\n"), dir);
        assert_eq!(developer_dir(true, b"/Library/Developer/CommandLineTools"), dir);
        assert_eq!(developer_dir(true, b"\n"), None);
        assert_eq!(developer_dir(true, b""), None);
        assert_eq!(developer_dir(false, b"/Library/Developer/CommandLineTools\n"), None);
    }

    #[test]
    fn rechecks_a_missing_git_sooner_than_a_usable_one() {
        let at = Instant::now();
        let missing = Checked { usable: false, at };
        let usable = Checked { usable: true, at };
        assert_eq!(still_valid(missing, at), Some(false));
        assert_eq!(still_valid(missing, at + MISSING_RECHECK), None);
        assert_eq!(still_valid(usable, at + MISSING_RECHECK), Some(true));
        assert_eq!(still_valid(usable, at + USABLE_RECHECK), None);
    }
}
