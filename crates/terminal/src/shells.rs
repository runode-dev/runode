//! 主机上装着的 shell 和命令。新建终端时可以挑的 shell（`available`）只在 Windows 上列：命令提示符和 Windows
//! PowerShell 一定有，PowerShell 7 和 Git Bash 看装没装；别的系统上新建终端用用户的登录 shell，不列。按 PATH
//! 找 shell 能跑的命令（`find_command`、`find_commands`）各系统都做。

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use runode_types::shell::{ShellProfile, ShellProfileKind};

/// 命令文件名在程序名后面接的扩展名。Windows 上只认程序和批处理（npm 装的 agent 是 `.cmd`），不照搬
/// `PATHEXT`：那里还有 `.js`、`.vbs` 这类交给脚本宿主打开的，同名的脚本不是要找的命令。
const COMMAND_EXTENSIONS: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };

/// `dirs` 里按先后找到的第一个叫 `program` 的命令文件，文件名带着找到的扩展名。
pub fn find_command(dirs: &[PathBuf], program: &str) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| {
        COMMAND_EXTENSIONS.iter().map(|ext| dir.join(format!("{program}{ext}"))).find(|file| is_command(file))
    })
}

/// `programs` 里每个在 `path` 上找得到时它的文件名（带着找到的扩展名），找不到时为 `None`，和 `programs`
/// 一一对应。`path` 为空时按新起的 shell 会拿到的 PATH（`pty::spawn_path`）找，只搜
/// `search_dirs`。
pub fn find_commands(path: Option<&OsStr>, programs: &[String]) -> Vec<Option<String>> {
    let path = path.map(OsStr::to_os_string).or_else(crate::pty::spawn_path);
    let dirs = search_dirs(path.as_deref());
    programs
        .iter()
        .map(|program| Some(find_command(&dirs, program)?.file_name()?.to_string_lossy().into_owned()))
        .collect()
}

/// PATH 是 `path` 时找命令的目录：只要绝对路径的项，空项和相对路径落在调用方的当前目录，那里的同名文件
/// 不是装着的命令。
pub(crate) fn search_dirs(path: Option<&OsStr>) -> Vec<PathBuf> {
    path.map(|path| std::env::split_paths(path).filter(|dir| dir.is_absolute()).collect()).unwrap_or_default()
}

/// `file` 是 shell 能跑的命令：Unix 上是有可执行位的文件（符号链接按它指向的算），Windows 上扩展名已经说明了。
fn is_command(file: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(file) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// 这台机器上可以挑的 shell，依次是 PowerShell 7、Windows PowerShell、命令提示符、Git Bash，`pty::default_shell`
/// 那一个标成默认。按新起的 shell 会拿到的 PATH（`pty::spawn_path`）找，读注册表要时间，不在界面线程上调。
pub fn available() -> Vec<ShellProfile> {
    #[cfg(windows)]
    {
        let path = crate::pty::spawn_path();
        let programs = ["ProgramW6432", "ProgramFiles"].into_iter().filter_map(std::env::var_os).map(PathBuf::from);
        let user = std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("Programs"));
        let mut shells = listed(path.as_deref(), programs.chain(user).map(|dir| dir.join("Git")));
        mark_default(&mut shells, &crate::pty::default_shell_in(path.as_deref()));
        shells
    }
    #[cfg(not(windows))]
    Vec::new()
}

/// `path` 是 PATH，`installs` 是 PATH 上没有 `git.exe` 时 Git for Windows 可能装在的目录。
#[cfg_attr(not(any(windows, test)), expect(dead_code, reason = "只在 Windows 上列"))]
fn listed(path: Option<&OsStr>, installs: impl IntoIterator<Item = PathBuf>) -> Vec<ShellProfile> {
    let dirs = search_dirs(path);
    let profile = |kind, program: &str| ShellProfile { kind, program: program.to_owned(), default: false };
    let mut shells = Vec::new();
    if dirs.iter().any(|dir| dir.join("pwsh.exe").is_file()) {
        shells.push(profile(ShellProfileKind::PowerShell, "pwsh.exe"));
    }
    shells.push(profile(ShellProfileKind::WindowsPowerShell, "powershell.exe"));
    shells.push(profile(ShellProfileKind::Cmd, "cmd.exe"));
    // System32 下的 `bash.exe` 是 WSL 的，只认 Git 安装目录里的。
    let bash = dirs
        .iter()
        .map(|dir| dir.join("git.exe"))
        .filter(|git| git.is_file())
        .filter_map(|git| git_root(&git))
        .chain(installs)
        .map(|root| root.join("bin").join("bash.exe"))
        .find(|bash| bash.is_file());
    if let Some(bash) = bash {
        shells.push(profile(ShellProfileKind::GitBash, &bash.to_string_lossy()));
    }
    shells
}

/// 把起的程序是 `default` 的那一项标成默认，不分大小写。
#[cfg_attr(not(any(windows, test)), expect(dead_code, reason = "只在 Windows 上列"))]
fn mark_default(shells: &mut [ShellProfile], default: &str) {
    for shell in shells {
        shell.default = shell.program.eq_ignore_ascii_case(default);
    }
}

/// `git.exe` 所在的 Git for Windows 安装目录：它在安装目录的 `cmd`、`bin` 或 `mingw64\bin` 下。
#[cfg_attr(not(any(windows, test)), expect(dead_code, reason = "只在 Windows 上列"))]
fn git_root(git: &Path) -> Option<PathBuf> {
    let named = |dir: &Path, name: &str| dir.file_name().is_some_and(|dir| dir.eq_ignore_ascii_case(name));
    let dir = git.parent()?;
    let root = if named(dir, "cmd") {
        dir.parent()?
    } else if named(dir, "bin") {
        let up = dir.parent()?;
        if named(up, "mingw64") { up.parent()? } else { up }
    } else {
        return None;
    };
    Some(root.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时目录下按 `files` 建好空文件，返回这个目录。
    fn tree(name: &str, files: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("rdr-shells-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for file in files {
            let file = root.join(file);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "").unwrap();
        }
        root
    }

    fn kinds(shells: &[ShellProfile]) -> Vec<ShellProfileKind> {
        shells.iter().map(|shell| shell.kind).collect()
    }

    #[test]
    fn always_lists_windows_powershell_and_cmd() {
        let shells = listed(None, []);
        assert_eq!(kinds(&shells), [ShellProfileKind::WindowsPowerShell, ShellProfileKind::Cmd]);
        assert_eq!(shells[0].program, "powershell.exe");
        assert_eq!(shells[1].program, "cmd.exe");
    }

    #[test]
    fn lists_powershell_7_first_when_it_is_on_the_path() {
        let root = tree("pwsh", &["PowerShell/7/pwsh.exe"]);
        let path = std::env::join_paths([root.join("PowerShell").join("7")]).unwrap();
        let shells = listed(Some(&path), []);
        assert_eq!(
            shells[0],
            ShellProfile { kind: ShellProfileKind::PowerShell, program: "pwsh.exe".into(), default: false }
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_git_bash_from_git_on_the_path() {
        let root = tree("git-path", &["Git/cmd/git.exe", "Git/bin/bash.exe"]);
        let path = std::env::join_paths([root.join("Git").join("cmd")]).unwrap();
        let shells = listed(Some(&path), []);
        let bash = root.join("Git").join("bin").join("bash.exe");
        assert_eq!(shells.last().unwrap().kind, ShellProfileKind::GitBash);
        assert_eq!(shells.last().unwrap().program, bash.to_string_lossy());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_git_bash_where_it_is_usually_installed() {
        let root = tree("git-install", &["Git/bin/bash.exe"]);
        let shells = listed(None, [root.join("Missing"), root.join("Git")]);
        assert_eq!(kinds(&shells).last(), Some(&ShellProfileKind::GitBash));
        assert!(kinds(&listed(None, [root.join("Missing")])).iter().all(|kind| *kind != ShellProfileKind::GitBash));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn marks_the_shell_started_by_default() {
        let mut shells = listed(None, []);
        mark_default(&mut shells, "PowerShell.exe");
        let defaults: Vec<_> = shells.iter().map(|shell| shell.default).collect();
        assert_eq!(defaults, [true, false]);
    }

    /// 找到的命令按请求的先后一一对应，带着找到的扩展名；空的 PATH 什么都找不到。
    #[test]
    fn finds_commands_on_the_given_path() {
        let name = if cfg!(windows) { "claude.cmd" } else { "claude" };
        let root = tree("commands", &[&format!("bin/{name}")]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let file = root.join("bin").join(name);
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([root.join("bin")]).unwrap();
        let programs = ["codex".to_owned(), "claude".to_owned()];
        assert_eq!(find_commands(Some(&path), &programs), [None, Some(name.to_owned())]);
        assert_eq!(find_commands(Some(OsStr::new("")), &programs), [None, None]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_root_is_above_cmd_bin_or_mingw64_bin() {
        let root = Path::new("/Git");
        assert_eq!(git_root(&root.join("cmd").join("git.exe")).as_deref(), Some(root));
        assert_eq!(git_root(&root.join("BIN").join("git.exe")).as_deref(), Some(root));
        assert_eq!(git_root(&root.join("mingw64").join("bin").join("git.exe")).as_deref(), Some(root));
        assert_eq!(git_root(&root.join("libexec").join("git.exe")), None);
    }
}
