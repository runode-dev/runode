//! Windows 上真的 shell 加载仓库里的集成：cd 以后报告的目录跟着变，运行完的命令带着原文、目录和退出码。
//! 系统自带的 Windows PowerShell 和 cmd 一定测；PATH 上有 PowerShell 7、nushell，装了 Git for Windows 时也测它们。

#![cfg(windows)]

use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use runode_terminal::{
    host_session::HostSession,
    pty::{Pty, PtyEvent},
};
use runode_types::{grid::GridSize, settings::TermSettings, shell::FinishedCommand, shell::IntegrationMode};

const SIZE: GridSize = GridSize { cols: 100, rows: 30, cell_width_px: 8, cell_height_px: 16 };
const WAIT: Duration = Duration::from_secs(60);

/// 同一件事在各个 shell 里的写法。
struct Syntax {
    /// 进 `子目录 x`。
    cd: &'static str,
    /// shell 自己报错的命令，退出码是 1。
    fail: &'static str,
    /// 外部程序，退出码是 3。
    native: &'static str,
    /// 打印 ok。
    ok: &'static str,
}

const POWERSHELL: Syntax = Syntax {
    cd: "Set-Location '子目录 x'",
    fail: "Get-Item no-such-file",
    native: "cmd /c exit 3",
    ok: "Write-Output ok",
};
const NUSHELL: Syntax = Syntax { cd: "cd '子目录 x'", fail: "ls no-such-file", ok: "print ok", ..POWERSHELL };
/// Git Bash 把像路径的参数（`/c`）换成 Windows 路径，外部程序不用 cmd。
const BASH: Syntax = Syntax { cd: "cd '子目录 x'", fail: "false", native: "sh -c 'exit 3'", ok: "echo ok" };

struct RealShell {
    session: HostSession,
    output: mpsc::Receiver<Vec<u8>>,
    finished: Vec<FinishedCommand>,
    name: String,
}

impl RealShell {
    fn start(program: &str, dir: &Path) -> Self {
        let (tx, output) = mpsc::channel();
        let sink = Box::new(move |event| match event {
            PtyEvent::Output(data) => tx.send(data.to_vec()).is_ok(),
            PtyEvent::Exited => false,
        });
        let pty = Pty::spawn(SIZE, Some(program), Some(dir), IntegrationMode::Detect, sink).unwrap();
        let session = HostSession::new(SIZE, pty, Some(dir), &TermSettings::default()).unwrap();
        Self { session, output, finished: Vec::new(), name: program.into() }
    }

    fn wait_for(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let until = Instant::now() + WAIT;
        while !done(self) {
            let left = until.saturating_duration_since(Instant::now());
            match self.output.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(data) => {
                    self.session.feed(&data);
                    let finished = self.session.take_commands();
                    self.finished.extend(finished);
                }
                Err(mpsc::RecvTimeoutError::Timeout) if !left.is_zero() => {}
                Err(_) => panic!(
                    "{}: {what} never came; screen:\n{}",
                    self.name,
                    self.session.screen_text(Some(200)).unwrap()
                ),
            }
        }
    }

    /// 敲一行命令回车，等 shell 报告它运行完。shell 还没起来时敲的先排着，起来以后照样读到。
    fn run(&mut self, command: &str) -> FinishedCommand {
        let count = self.finished.len();
        self.session.write(format!("{command}\r").into_bytes());
        self.wait_for(&format!("the end of `{command}`"), |shell| shell.finished.len() > count);
        self.finished[count].clone()
    }
}

fn integrated(program: &str, syntax: &Syntax) {
    let stem = Path::new(program).file_stem().unwrap().to_string_lossy().into_owned();
    let dir = std::env::temp_dir().join(format!("runode-shell-{}-{stem}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let sub = dir.join("子目录 x");
    std::fs::create_dir_all(&sub).unwrap();
    let dir = long_path(&dir);
    let sub = long_path(&sub);
    let mut shell = RealShell::start(program, &dir);

    let cd = shell.run(syntax.cd);
    assert_eq!((cd.cmd.as_str(), cd.exit, cd.cwd.as_deref()), (syntax.cd, Some(0), Some(&*dir)), "{program}");
    // 进程自己的当前目录不跟着 shell 变，报告的跟着变。
    shell.wait_for("the new directory", |shell| shell.session.prompt_cwd().as_deref() == Some(&*sub));

    let native = shell.run(syntax.native);
    assert_eq!((native.exit, native.cwd.as_deref()), (Some(3), Some(&*sub)), "{program}");
    let failed = shell.run(syntax.fail);
    assert_eq!(failed.exit, Some(1), "{program}");
    // 和更早那条外部程序的退出码一样也照报。
    let again = shell.run(syntax.native);
    assert_eq!(again.exit, Some(3), "{program}");
    let ok = shell.run(syntax.ok);
    assert_eq!(ok.exit, Some(0), "{program}");
    let (text, truncated) = shell.session.command_output(1).unwrap();
    assert_eq!((text.trim(), truncated), ("ok", false), "{program}");
    drop(shell);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 临时目录可能是 8.3 短名（`ADMINI~1`），shell 报告的是长名。
fn long_path(path: &Path) -> PathBuf {
    let long = std::fs::canonicalize(path).unwrap();
    let text = long.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// 在本进程的 PATH 上找 `program`，给出完整路径：起 shell 时的 PATH 从注册表重读，测试另加的目录不在里面。
fn on_path(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let found = std::env::split_paths(&path).map(|dir| dir.join(program)).find(|path| path.is_file());
    assert!(
        found.is_some() || std::env::var_os("RUNODE_REQUIRE_SHELLS").is_none(),
        "设了 RUNODE_REQUIRE_SHELLS，却找不到 {program}"
    );
    found.map(|path| path.to_string_lossy().into_owned())
}

#[test]
fn windows_powershell_reports_its_directory_and_commands() {
    integrated("powershell.exe", &POWERSHELL);
}

#[test]
fn powershell_7_reports_its_directory_and_commands() {
    if let Some(pwsh) = on_path("pwsh.exe") {
        integrated(&pwsh, &POWERSHELL);
    }
}

/// 用户开了严格模式、自己的 prompt 看 `$?`、用 Write-Host 直接往屏幕上写时，命令照样报告得出来，
/// 用户的 prompt 看得到上一条命令失败了，它写的字也不算进命令的输出。
#[test]
fn windows_powershell_keeps_the_user_prompt_working() {
    let dir = long_path(&std::env::temp_dir());
    let mut shell = RealShell::start("powershell.exe", &dir);
    // 还没跑过外部程序，严格模式下读不得 LASTEXITCODE。
    let strict = shell.run("Set-StrictMode -Version Latest");
    assert_eq!(strict.exit, Some(0));
    // 换掉集成包着的那个 prompt，就像用户的 profile 里写的。
    shell.run("$__runode.prompt = { Write-Host ('custom ' + $?) -NoNewline; '> ' }");
    let failed = shell.run("Get-Item no-such-file");
    assert_eq!(failed.exit, Some(1));
    // 屏幕上的行去掉了行尾的空格。
    let prompt = |shell: &RealShell| {
        let screen = shell.session.screen_text(Some(200)).unwrap();
        screen.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or_default().trim_end().to_owned()
    };
    shell.wait_for("the failure in the prompt", |shell| prompt(shell) == "custom False>");
    let ok = shell.run("Write-Output ok");
    assert_eq!(ok.exit, Some(0));
    let (text, _) = shell.session.command_output(1).unwrap();
    assert_eq!(text.trim(), "ok");
    shell.wait_for("the success in the prompt", |shell| prompt(shell) == "custom True>");
}

/// Git Bash 里的路径写成 /c/Users/…，集成报告前换成 Windows 的写法。
#[test]
fn git_bash_reports_its_directory_and_commands() {
    let bash = Path::new(r"C:\Program Files\Git\bin\bash.exe");
    if bash.is_file() {
        integrated(&bash.to_string_lossy(), &BASH);
    }
}

#[test]
fn nushell_reports_its_directory_and_commands() {
    if let Some(nu) = on_path("nu.exe") {
        integrated(&nu, &NUSHELL);
    }
}

/// Git for Windows 的 `bin\bash.exe` 只是转发程序，真正的 bash 是它起的子进程：等着输入时前台算 shell 自己，
/// 跑着命令时不算。bash 先 fork 出一个自己，Windows 原生的程序由那个副本起，副本一直等它结束。
#[test]
fn git_bash_behind_its_forwarder_is_the_shell() {
    let bash = Path::new(r"C:\Program Files\Git\bin\bash.exe");
    if !bash.is_file() {
        return;
    }
    let dir = long_path(&std::env::temp_dir());
    let mut shell = RealShell::start(&bash.to_string_lossy(), &dir);
    shell.run("true");
    shell.wait_for("the shell in the foreground", |shell| shell.session.foreground_is_shell());
    let (_, name) = shell.session.foreground_program().unwrap();
    assert_eq!(name.as_deref(), Some("bash.exe"));
    shell.session.write(b"ping -n 30 127.0.0.1\r".to_vec());
    shell.wait_for("ping in the foreground", |shell| {
        shell
            .session
            .foreground_program()
            .and_then(|(_, name)| name)
            .is_some_and(|name| name.eq_ignore_ascii_case("ping.exe"))
    });
    assert!(!shell.session.foreground_is_shell());
}

/// cmd 只在提示符里标提示符和输入，不发报告：目录现读，最近一条命令的输出照样分得出来。
#[test]
fn cmd_marks_its_prompts() {
    let dir = std::env::temp_dir().join(format!("runode-shell-{}-cmd", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let sub = dir.join("子目录 x");
    std::fs::create_dir_all(&sub).unwrap();
    let dir = long_path(&dir);
    let sub = long_path(&sub);
    let mut shell = RealShell::start("cmd.exe", &dir);
    shell.session.write("cd \"子目录 x\"\r".as_bytes().to_vec());
    shell.wait_for("the new directory", |shell| shell.session.prompt_cwd().as_deref() == Some(&*sub));
    shell.session.write(b"echo one\r".to_vec());
    shell.session.write(b"echo two\r".to_vec());
    shell.wait_for("the output of `echo two`", |shell| {
        shell.session.command_output(1).is_ok_and(|(text, _)| text.trim() == "two")
    });
    let (text, truncated) = shell.session.command_output(2).unwrap();
    assert_eq!((text.trim(), truncated), ("one", false));
    assert!(shell.finished.is_empty(), "{:?}", shell.finished);
    drop(shell);
    let _ = std::fs::remove_dir_all(&dir);
}
