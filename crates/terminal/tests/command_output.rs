//! 按 shell 集成标出的提示符读某条命令的输出（`read --command`）：先是手写的理想序列，后面
//! 是真的 bash、zsh、fish、elvish、nushell 加载仓库里的集成脚本发出的序列。系统里没有的 shell
//! 不测。

mod common;

use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use common::{PROMPT, idle_host};
use runode_terminal::{
    host_session::HostSession,
    pty::{Pty, PtyEvent, PtySink},
};
use runode_types::{grid::GridSize, settings::TermSettings, shell::IntegrationMode};

/// 40 列 12 行的宿主会话。
fn host() -> HostSession {
    let mut session = idle_host();
    session.resize(GridSize { cols: 40, rows: 12, cell_width_px: 8, cell_height_px: 16 });
    session
}

/// 在提示符上敲 `command` 回车，shell 报告开始运行；接着是 `output`。
fn run(command: &str, output: &str) -> Vec<u8> {
    let mut bytes = PROMPT.to_vec();
    bytes.extend_from_slice(command.as_bytes());
    bytes.extend_from_slice(b"\r\n\x1b]133;C\x07");
    bytes.extend_from_slice(output.as_bytes());
    bytes
}

/// 命令结束（退出码 `exit`）。
fn end(exit: i32) -> Vec<u8> {
    format!("\x1b]133;D;{exit}\x07").into_bytes()
}

#[test]
fn the_last_commands_are_counted_from_the_bottom() {
    let mut session = host();
    session.feed(&run("echo hi", "hi\r\n"));
    session.feed(&end(0));
    session.feed(&run("ls", "a\r\nb\r\n\r\n"));
    session.feed(&end(1));
    // 回到提示符，用户已经敲了一半：正等着输入的提示符不算一条命令。
    session.feed(PROMPT);
    session.feed(b"git st");
    assert_eq!(session.command_output(1).unwrap(), ("a\nb\n".into(), false));
    assert_eq!(session.command_output(2).unwrap(), ("hi\n".into(), false));
    let err = session.command_output(3).unwrap_err().to_string();
    assert!(err.contains("only 2 commands"), "{err}");
}

#[test]
fn a_running_command_is_read_to_the_bottom() {
    let mut session = host();
    session.feed(&run("echo one", "one\r\n"));
    session.feed(&end(0));
    session.feed(&run("make", ""));
    // 刚回车、还没输出时是空的，不会读成上一条。
    assert_eq!(session.command_output(1).unwrap(), (String::new(), false));
    session.feed(b"compiling\r\nstill going");
    assert_eq!(session.command_output(1).unwrap(), ("compiling\nstill going\n".into(), false));
    assert_eq!(session.command_output(2).unwrap(), ("one\n".into(), false));
}

#[test]
fn a_long_command_line_is_not_part_of_the_output() {
    let mut session = host();
    // 输入比一行长，软折行到第二行。
    session.feed(&run(&"x".repeat(50), "out\r\n"));
    session.feed(&end(0));
    session.feed(PROMPT);
    assert_eq!(session.command_output(1).unwrap(), ("out\n".into(), false));
}

#[test]
fn without_marks_it_asks_for_shell_integration() {
    let mut session = host();
    session.feed(b"$ ls\r\na b c\r\n$ ");
    let err = session.command_output(1).unwrap_err().to_string();
    assert!(err.contains("needs shell integration"), "{err}");
    // 全屏程序占着屏幕时也读不了。
    session.feed(&run("vim", "\x1b[?1049h~"));
    let err = session.command_output(1).unwrap_err().to_string();
    assert!(err.contains("full-screen"), "{err}");
}

#[test]
fn output_whose_start_scrolled_away_is_marked_truncated() {
    let mut session = host();
    // 不留回滚历史：输出一长，命令的提示符就被挤掉了。
    session.apply_theme(&TermSettings { scrollback_limit: 0, ..TermSettings::default() });
    let output: String = (1..=20).map(|i| format!("line {i}\r\n")).collect();
    session.feed(&run("seq 20", &output));
    session.feed(&end(0));
    session.feed(PROMPT);
    let (text, truncated) = session.command_output(1).unwrap();
    assert!(truncated);
    assert!(text.ends_with("line 19\nline 20\n"), "{text:?}");
    assert!(!text.contains("seq 20"), "{text:?}");
    // 再往前就什么都没了。
    assert!(session.command_output(2).is_err());
}

/// 一个真的 shell：在伪终端里加载仓库里的集成脚本（和 `shell_integration::prepare` 注入的是同一份），
/// 输出喂给宿主会话。环境是干净的，家目录是临时目录，提示符由 `rc` 设，不受跑测试的人自己的
/// 配置影响。集成脚本的功能按默认设置开（`cursor:steady`）。
struct RealShell {
    session: HostSession,
    output: mpsc::Receiver<Vec<u8>>,
    /// 提示符最后一行的第一个词，每出一个提示符屏幕上就多一行以它开头。
    prompt: &'static str,
    /// 提示符最后一行空着、等输入时的样子，按空白分开的各个词（含右侧提示符）。
    idle: &'static [&'static str],
    /// 已经出过的提示符。
    prompts: usize,
    name: String,
    /// shell 到现在为止的全部输出，查集成脚本发的光标序列用。
    raw: Vec<u8>,
    /// 临时的家目录，用完删掉。
    home: PathBuf,
    syntax: &'static Syntax,
}

/// 同一件事在各个 shell 里的写法和样子。
struct Syntax {
    /// 打印 a、b、c 三行。
    abc: &'static str,
    /// 什么都不打印，成功退出。
    nothing: &'static str,
    /// 打印 one，读一行输入，再打印 two。
    read: &'static str,
    /// 读输入时画在那一行上的提示符：fish 画 `read>`，别的什么都不画。
    read_prompt: &'static str,
    /// 空着回车算不算一条命令：elvish 只能在画提示符之前标出用户输入开始，提示符的文字被当成
    /// 了输入，算。
    blank_counts: bool,
}

const POSIX: Syntax = Syntax {
    abc: "printf 'a\\nb\\nc\\n'",
    nothing: "true",
    read: "echo one; read x; echo two",
    read_prompt: "",
    blank_counts: false,
};
const FISH: Syntax = Syntax { read_prompt: "read>", ..POSIX };
const ELVISH: Syntax = Syntax {
    abc: "print \"a\\nb\\nc\\n\"",
    read: "echo one; var x = (read-line); echo two",
    blank_counts: true,
    ..POSIX
};
const NUSHELL: Syntax = Syntax { nothing: "^true", read: "print one; input | ignore; print two", ..POSIX };

impl Drop for RealShell {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

const REAL_SIZE: GridSize = GridSize { cols: 40, rows: 12, cell_width_px: 8, cell_height_px: 16 };
const REAL_WAIT: Duration = Duration::from_secs(10);
/// 真 shell 的集成报告带的口令。
const REPORT_TOKEN: &str = "0123456789abcdef";

/// 仓库里的集成脚本所在的目录。
fn integration_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("shell-integration")
}

/// 找 `shell`：先照 PATH（自己装的新版在这里，`/bin/bash` 还是 3.2），再看 `/bin/`，跳过不能执行的同名文件。都没有时为 `None`。
fn find_shell(shell: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain([PathBuf::from("/bin")])
        .map(|dir| dir.join(shell))
        .find(|p| p.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0))
}

/// 给 sh 用的单引号字面量：路径里的单引号写成 `'\''`，不会截断引号或混进 shell 语法。
fn quoted(text: impl std::fmt::Display) -> String {
    format!("'{}'", text.to_string().replace('\'', r"'\''"))
}

/// 找到的 bash 的版本不低于 `major.minor`；没有 bash 时为假。
fn bash_at_least(major: u32, minor: u32) -> bool {
    let check = format!(
        "[ \"${{BASH_VERSINFO[0]}}\" -gt {major} ] || {{ [ \"${{BASH_VERSINFO[0]}}\" -eq {major} ] && [ \"${{BASH_VERSINFO[1]}}\" -ge {minor} ]; }}"
    );
    find_shell("bash").is_some_and(|bash| {
        std::process::Command::new(bash).args(["-c", &check]).status().is_ok_and(|status| status.success())
    })
}

impl RealShell {
    /// 起 `shell`（`bash`、`zsh`、`fish`、`elvish` 或 `nu`，见 `find_shell`），`rc` 写进它的启动配置；
    /// `name` 在同时跑的测试里各不相同，失败时也用它说是哪一个。系统里没有这个 shell 时为 `None`。
    fn start(shell: &str, name: &str, rc: &str, prompt: &'static str, idle: &'static [&'static str]) -> Option<Self> {
        let program = quoted(find_shell(shell)?.display());
        let home = std::env::temp_dir().join(format!("rn-cmdout-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // 和 `shell_integration::prepare` 一样：bash 用 --rcfile 起非登录的交互式 shell，脚本自己加载
        // 登录配置；zsh 把 ZDOTDIR 指到集成目录，那里的 .zshenv 还原 ZDOTDIR 后加载用户配置和集成；
        // fish、nushell 的数据目录加进 XDG_DATA_DIRS，它们自己加载里面的集成；elvish 用 -rc 读集成，
        // 集成再加载用户的 rc.elv。
        let data_dir = |name: &str, file: &str, script: &str| {
            let data = home.join(name);
            let target = data.join(file);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(integration_dir().join(script), target).unwrap();
            data.display().to_string()
        };
        let integration = |script: &str| quoted(integration_dir().join(script).display());
        let (rc_file, launch, syntax) = match shell {
            "bash" => (".bash_profile", format!("{program} --rcfile {}", integration("bash/runode.bash")), &POSIX),
            "zsh" => (".zshrc", format!("ZDOTDIR={} {program} -l", integration("zsh")), &POSIX),
            "fish" => {
                let data = data_dir("fish-data", "fish/vendor_conf.d/runode.fish", "fish/runode.fish");
                (
                    ".config/fish/config.fish",
                    format!(
                        "XDG_DATA_DIRS={} RUNODE_FISH_DATA_DIR={} {program} --features no-mark-prompt -l",
                        quoted(format!("{data}:/usr/local/share:/usr/share")),
                        quoted(&data)
                    ),
                    &FISH,
                )
            }
            "elvish" => {
                (".config/elvish/rc.elv", format!("{program} -rc {}", integration("elvish/runode.elv")), &ELVISH)
            }
            _ => {
                let data = data_dir("nushell-data", "nushell/vendor/autoload/runode.nu", "nushell/runode.nu");
                (
                    ".config/nushell/config.nu",
                    format!(
                        "XDG_CONFIG_HOME={} XDG_DATA_DIRS={} RUNODE_NU_DATA_DIR={} {program} -l",
                        quoted(home.join(".config").display()),
                        quoted(format!("{data}:/usr/local/share:/usr/share")),
                        quoted(&data)
                    ),
                    &NUSHELL,
                )
            }
        };
        let rc_file = home.join(rc_file);
        std::fs::create_dir_all(rc_file.parent().unwrap()).unwrap();
        std::fs::write(rc_file, rc).unwrap();
        let launcher = home.join("launch.sh");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nexec /usr/bin/env -i HOME={} PATH=/usr/bin:/bin TERM=xterm-256color LANG=en_US.UTF-8 \
                 BASH_SILENCE_DEPRECATION_WARNING=1 RUNODE_REPORT_TOKEN={REPORT_TOKEN} \
                 RUNODE_SHELL_FEATURES=cursor:steady {launch}\n",
                quoted(home.display())
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (tx, output) = mpsc::channel();
        let sink = |tx: mpsc::Sender<Vec<u8>>| -> PtySink {
            Box::new(move |event| match event {
                PtyEvent::Output(data) => tx.send(data.to_vec()).is_ok(),
                PtyEvent::Exited => false,
            })
        };
        // shell 由 launch.sh 起，集成和口令都是它给的；宿主那边要认这个口令，才采用 shell 的报告。
        // 照升级时接手会话的路子交出来、带上口令再接手，读到的输出照旧进同一个通道。
        let mut pty = Pty::spawn(
            REAL_SIZE,
            Some(launcher.to_str().unwrap()),
            Some(&home),
            IntegrationMode::Off,
            sink(tx.clone()),
        )
        .unwrap();
        let mut handoff = pty.release().unwrap();
        handoff.report_token = Some(REPORT_TOKEN.into());
        let pty = Pty::adopt(handoff, sink(tx)).unwrap();
        let session = HostSession::new(REAL_SIZE, pty, None, &TermSettings::default()).unwrap();
        let mut real =
            Self { session, output, prompt, idle, prompts: 0, name: name.into(), raw: Vec::new(), home, syntax };
        real.wait_for_prompt();
        Some(real)
    }

    /// 喂进收到的输出，直到 `done`；超时就失败，带上屏幕上的内容。
    fn wait_until(&mut self, what: &str, done: impl Fn(&HostSession) -> bool) {
        self.wait_for(what, |shell| done(&shell.session));
    }

    /// 喂进收到的输出，直到从 `from` 起的输出里有了 `needle`。
    fn wait_for_bytes(&mut self, what: &str, from: usize, needle: &[u8]) {
        self.wait_for(what, |shell| rfind(&shell.raw[from..], needle).is_some());
    }

    fn wait_for(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let until = Instant::now() + REAL_WAIT;
        while !done(self) {
            let left = until.saturating_duration_since(Instant::now());
            match self.output.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(data) => {
                    self.session.feed(&data);
                    self.raw.extend_from_slice(&data);
                }
                Err(mpsc::RecvTimeoutError::Timeout) if !left.is_zero() => {}
                // 超时，或者 shell 退出了。
                Err(_) => panic!("{}: {what} never came; screen:\n{}", self.name, self.screen()),
            }
        }
    }

    fn screen(&self) -> String {
        self.session.screen_text(Some(200)).unwrap()
    }

    /// 等到又出了一个提示符、空着等输入。
    fn wait_for_prompt(&mut self) {
        self.prompts += 1;
        let (prompt, idle, count) = (self.prompt, self.idle, self.prompts);
        self.wait_until(&format!("prompt #{count}"), |session| {
            let text = session.screen_text(Some(200)).unwrap();
            let shown = text.lines().filter(|line| line.split_whitespace().next() == Some(prompt)).count();
            let last = text.lines().last().unwrap_or("");
            shown == count && last.split_whitespace().eq(idle.iter().copied())
        });
    }

    /// 敲一行命令回车，等它跑完、回到提示符。
    fn run(&mut self, command: &str) {
        self.session.write(format!("{command}\r").into_bytes());
        self.wait_for_prompt();
    }

    fn output(&self, n: u32) -> String {
        let (text, truncated) = self.session.command_output(n).unwrap();
        assert!(!truncated, "{}: command {n} truncated", self.name);
        text
    }
}

/// 在真 shell 里跑一串命令，按提示符读各条命令的输出。
fn read_commands_from(shell: Option<RealShell>) {
    let Some(mut shell) = shell else { return };
    let (name, syntax) = (shell.name.clone(), shell.syntax);
    // 刚起来的 shell 还没跑过命令：开头打印的内容（如果有）算一条截断的，没有就是一条都没有。
    assert!(shell.session.command_output(1).map_or(true, |(_, truncated)| truncated), "{name}");

    // 多行输出。
    shell.run(syntax.abc);
    assert_eq!(shell.output(1), "a\nb\nc\n", "{name}");
    // 没有输出；之前那条是第二条。
    shell.run(syntax.nothing);
    assert_eq!(shell.output(1), "", "{name}");
    assert_eq!(shell.output(2), "a\nb\nc\n", "{name}");
    // 空着回车不算一条命令（elvish 除外，之后数到的都多一条）。
    shell.run("");
    let blank = u32::from(syntax.blank_counts);
    assert_eq!(shell.output(1), "", "{name}");
    assert_eq!(shell.output(2 + blank), "a\nb\nc\n", "{name}");
    // 命令行和输出都比终端宽，软折行。
    let wide = "y".repeat(45);
    shell.run(&format!("echo {wide}"));
    assert_eq!(shell.output(1), format!("{}\n{}\n", &wide[..40], &wide[40..]), "{name}");
    assert_eq!(shell.output(3 + blank), "a\nb\nc\n", "{name}");

    // 还在跑的命令读到底：读输入时只有第一行。
    shell.session.write(format!("{}\r", syntax.read).into_bytes());
    shell.wait_until("the first line", |session| session.screen_text(Some(200)).unwrap().contains("\none\n"));
    assert_eq!(shell.output(1), "one\n", "{name}");
    assert_eq!(shell.output(2), format!("{}\n{}\n", &wide[..40], &wide[40..]), "{name}");
    shell.session.write(b"\r".to_vec());
    shell.wait_for_prompt();
    // 回车被终端回显成一行，上面只有读输入的提示符。
    assert_eq!(shell.output(1), format!("one\n{}\ntwo\n", syntax.read_prompt), "{name}");
    assert_eq!(shell.output(4 + blank), "a\nb\nc\n", "{name}");
}

#[test]
fn real_bash_commands_are_read() {
    read_commands_from(RealShell::start("bash", "bash", "PS1='[b]\\$ '\n", "[b]$", &["[b]$"]));
}

/// 用户的 PROMPT_COMMAND 以 `;` 结尾时照样跑，集成也不出语法错误、照常标出提示符和退出码。
#[test]
fn real_bash_keeps_a_prompt_command_ending_in_a_semicolon() {
    let rc = "PS1='[b]\\$ '\nPROMPT_COMMAND='ran=yes;'\n";
    let Some(mut shell) = RealShell::start("bash", "bash-prompt-command", rc, "[b]$", &["[b]$"]) else { return };
    shell.run("false");
    shell.wait_for_bytes("the exit status of false", 0, b"\x1b]133;D;1\x07");
    shell.run("echo $ran");
    assert_eq!(shell.output(1), "yes\n");
    assert!(rfind(&shell.raw, b"syntax error").is_none(), "{}", shell.screen());
}

/// 用户的 PROMPT_COMMAND 看到的 $? 是上一条命令的退出码，不是集成记下退出码那一步的。
#[test]
fn real_bash_prompt_command_sees_the_exit_status() {
    let rc = "PS1='[b]\\$ '\nPROMPT_COMMAND='seen=$?'\n";
    let Some(mut shell) = RealShell::start("bash", "bash-prompt-status", rc, "[b]$", &["[b]$"]) else { return };
    shell.run("false");
    shell.run("echo $seen");
    assert_eq!(shell.output(1), "1\n");
}

/// bash 5.1 起数组形式的 PROMPT_COMMAND 每个元素都跑，都看到上一条命令的退出码；后面的元素改了
/// PS1，提示符照样标出来。
#[test]
fn real_bash_wraps_an_array_prompt_command() {
    if !bash_at_least(5, 1) {
        return;
    }
    let rc = "PROMPT_COMMAND=('seen=$?' 'PS1=\"[b]\\$ \"')\n";
    let Some(mut shell) = RealShell::start("bash", "bash-prompt-array", rc, "[b]$", &["[b]$"]) else { return };
    shell.run("false");
    shell.run("echo $seen");
    assert_eq!(shell.output(1), "1\n");
    assert_eq!(shell.output(2), "");
}

/// 稀疏的数组（没有下标 0）也按数组接上：用户的元素在标记之前跑，看到上一条命令的退出码。
#[test]
fn real_bash_wraps_a_sparse_array_prompt_command() {
    if !bash_at_least(5, 1) {
        return;
    }
    let rc = "PROMPT_COMMAND=([10]='seen=$?; PS1=\"[b]\\$ \"')\n";
    let Some(mut shell) = RealShell::start("bash", "bash-prompt-sparse", rc, "[b]$", &["[b]$"]) else { return };
    shell.run("false");
    shell.run("echo $seen");
    assert_eq!(shell.output(1), "1\n");
    assert_eq!(shell.output(2), "");
}

/// 用户开了 `set -e` 时，跑用户的 PROMPT_COMMAND 前换回退出码这一步不能让 shell 退出：`! true` 留下
/// 退出码 1，却不触发 errexit。
#[test]
fn real_bash_survives_errexit_while_restoring_the_exit_status() {
    let rc = "PS1='[b]\\$ '\nPROMPT_COMMAND='seen=$?'\nset -e\n";
    let Some(mut shell) = RealShell::start("bash", "bash-prompt-errexit", rc, "[b]$", &["[b]$"]) else { return };
    shell.run("! true");
    shell.run("echo $seen");
    assert_eq!(shell.output(1), "1\n");
}

#[test]
fn real_bash_commands_are_read_under_a_two_line_prompt() {
    read_commands_from(RealShell::start("bash", "bash-two-line", "PS1='\\W\\n[b]\\$ '\n", "[b]$", &["[b]$"]));
}

#[test]
fn real_zsh_commands_are_read() {
    read_commands_from(RealShell::start("zsh", "zsh", "PROMPT='[z]%% '\nRPROMPT='<r>'\n", "[z]%", &["[z]%", "<r>"]));
}

/// 两行的左提示符配上右侧提示符：右侧提示符画在第二行，那一行被标成了主提示符。
#[test]
fn real_zsh_commands_are_read_under_a_two_line_prompt() {
    read_commands_from(RealShell::start(
        "zsh",
        "zsh-two-line",
        "PROMPT=$'%1~\\n[z]%% '\nRPROMPT='<r>'\n",
        "[z]%",
        &["[z]%", "<r>"],
    ));
}

/// 程序退出时漏在终端里的 SGR 鼠标报告被集成吞掉，不留在命令行上。
fn drop_mouse_reports_in(shell: Option<RealShell>) {
    let Some(mut shell) = shell else { return };
    let name = shell.name.clone();
    shell.run("echo a\x1b[<51;72;30Mb\x1b[<0;3;4mc");
    assert_eq!(shell.output(1), "abc\n", "{name}");
}

#[test]
fn real_zsh_drops_mouse_reports() {
    drop_mouse_reports_in(RealShell::start("zsh", "zsh-mouse", "PROMPT='[z]%% '\n", "[z]%", &["[z]%"]));
}

/// readline 的 skip-csi-sequence 是 bash 4.2 才有的，更老的 bash（macOS 自带的 3.2）不测。
#[test]
fn real_bash_drops_mouse_reports() {
    if bash_at_least(4, 2) {
        drop_mouse_reports_in(RealShell::start("bash", "bash-mouse", "PS1='[b]\\$ '\n", "[b]$", &["[b]$"]));
    }
}

/// 提示符上的不闪的竖线和方块、命令开始前换回配置的样式：DECSCUSR 6、2、0。
const BAR: &[u8] = b"\x1b[6 q";
const BLOCK: &[u8] = b"\x1b[2 q";
const RESET: &[u8] = b"\x1b[0 q";

/// `needle` 在 `haystack` 里最后一次出现的位置。
fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|window| window == needle)
}

/// 提示符上光标是竖线，命令开始前换回配置的样式，输出之后的下一个提示符又换成竖线。zsh 在
/// 提示符画完以后才换光标，所以等到字节来了再看。
fn cursor_follows_the_prompt(shell: &mut RealShell) {
    let name = shell.name.clone();
    shell.wait_for_bytes("the bar at the first prompt", 0, BAR);
    let start = shell.raw.len();
    shell.run("echo out");
    shell.wait_for("the bar at the next prompt", |shell| {
        let raw = &shell.raw[start..];
        rfind(raw, b"out\r\n").zip(rfind(raw, BAR)).is_some_and(|(output, bar)| output < bar)
    });
    let raw = &shell.raw[start..];
    let reset = rfind(raw, RESET).unwrap_or_else(|| panic!("{name}: cursor not reset for the command"));
    assert!(reset < rfind(raw, b"out\r\n").unwrap(), "{name}: {:?}", String::from_utf8_lossy(raw));
}

/// zsh 的 vi 模式：命令模式里是方块，回到插入模式是竖线；用户自己的 zle-keymap-select 照样被调，
/// 在换完光标之后（这里它发一个带键位名的标题，从输出里认出它被调过）。
#[test]
fn real_zsh_cursor_follows_the_prompt_and_vi_mode() {
    let rc = "PROMPT='[z]%% '\nbindkey -v\nKEYTIMEOUT=1\n\
              zle-keymap-select() { print -n $'\\e]2;km-'$KEYMAP$'\\a' }\nzle -N zle-keymap-select\n";
    let Some(mut shell) = RealShell::start("zsh", "zsh-cursor", rc, "[z]%", &["[z]%"]) else { return };
    cursor_follows_the_prompt(&mut shell);

    let start = shell.raw.len();
    shell.session.write(b"\x1b".to_vec());
    shell.wait_for_bytes("the vicmd keymap", start, b"km-vicmd");
    let raw = &shell.raw[start..];
    let block = rfind(raw, BLOCK).expect("no block in vicmd");
    assert!(block < rfind(raw, b"km-vicmd").unwrap(), "{:?}", String::from_utf8_lossy(raw));

    let start = shell.raw.len();
    shell.session.write(b"i".to_vec());
    shell.wait_for_bytes("the main keymap", start, b"km-main");
    assert!(rfind(&shell.raw[start..], BAR).is_some(), "no bar back in insert mode");
}

/// bash 4.4 起在 PS0 里换回光标；更老的 bash（macOS 自带的 3.2）没有 PS0，不换光标。
#[test]
fn real_bash_cursor_follows_the_prompt() {
    let has_ps0 = bash_at_least(4, 4);
    let Some(mut shell) = RealShell::start("bash", "bash-cursor", "PS1='[b]\\$ '\n", "[b]$", &["[b]$"]) else { return };
    if has_ps0 {
        cursor_follows_the_prompt(&mut shell);
    } else {
        shell.run("echo out");
        assert!(rfind(&shell.raw, BAR).is_none() && rfind(&shell.raw, RESET).is_none(), "old bash touched the cursor");
    }
}

/// `runode-reload` 换成一个新的 shell：重新读了用户配置，集成照旧，报告带着同一个口令，口令
/// 不留在环境里。用户配置加载 `extra`，换之前才写进去；`check` 打印 `RELOADED` 的值（没有时
/// `no`）和口令在不在环境里（不在时 `gone`）。
fn reload_in(shell: Option<RealShell>, check: &str) {
    let Some(mut shell) = shell else { return };
    let name = shell.name.clone();
    std::fs::write(shell.home.join("extra"), "export RELOADED=yes\n").unwrap();
    let start = shell.raw.len();
    shell.run(" runode-reload");
    shell.wait_for_bytes("a report after the reload", start, b"\x1b]6973;0123456789abcdef;cwd=");
    shell.run(check);
    assert_eq!(shell.output(1), "yes gone\n", "{name}");
}

const POSIX_RELOAD_CHECK: &str = "echo ${RELOADED-no} ${RUNODE_REPORT_TOKEN-gone}";

/// 集成绑的换 shell 按键在 vi 命令模式里按下（`Session::reload_shell` 就是这么按的）：换成了新 shell，
/// 行上原来的命令（建出 `bad`）没被执行。`rc` 开 vi 模式、加载 `extra`。
fn reload_key_in_vi_command_mode(shell: Option<RealShell>) {
    let Some(mut shell) = shell else { return };
    let name = shell.name.clone();
    std::fs::write(shell.home.join("extra"), "export RELOADED=yes\n").unwrap();
    let start = shell.raw.len();
    // ESC 进 vi 命令模式；行编辑器按顺序读，不用等它切过去。
    shell.session.write(b"touch ~/bad\x1b".to_vec());
    shell.session.write(b"\x1b[6973~".to_vec());
    shell.wait_for_prompt();
    shell.wait_for_bytes("a report after the reload", start, b"\x1b]6973;0123456789abcdef;cwd=");
    shell.run("echo ${RELOADED-no}");
    assert_eq!(shell.output(1), "yes\n", "{name}");
    assert!(!shell.home.join("bad").exists(), "{name}: the line was run");
}

#[test]
fn real_zsh_reload_key_works_in_vi_command_mode() {
    let rc = "PROMPT='[z]%% '\nbindkey -v\nKEYTIMEOUT=1\n[[ -r ~/extra ]] && . ~/extra\n";
    reload_key_in_vi_command_mode(RealShell::start("zsh", "zsh-reload-key", rc, "[z]%", &["[z]%"]));
}

/// 集成比 .zshrc 先绑这个按键，.zshrc 里 `bindkey -d` 重置了键位也照样能按。
#[test]
fn real_zsh_reload_key_survives_a_keymap_reset_in_zshrc() {
    let rc = "PROMPT='[z]%% '\nbindkey -d\nbindkey -v\nKEYTIMEOUT=1\n[[ -r ~/extra ]] && . ~/extra\n";
    reload_key_in_vi_command_mode(RealShell::start("zsh", "zsh-reload-key-reset", rc, "[z]%", &["[z]%"]));
}

/// 4.4 以下的 bash 没绑这个按键（`runode-reload-key` 不报告），`Session::reload_shell` 照旧清输入再打命令。
#[test]
fn real_bash_reload_key_works_in_vi_command_mode() {
    if !bash_at_least(4, 4) {
        return;
    }
    let rc = "set -o vi\nPS1='[b]\\$ '\n[ -r ~/extra ] && . ~/extra\n";
    reload_key_in_vi_command_mode(RealShell::start("bash", "bash-reload-key", rc, "[b]$", &["[b]$"]));
}

#[test]
fn real_zsh_reloads_with_the_integration() {
    reload_in(
        RealShell::start("zsh", "zsh-reload", "PROMPT='[z]%% '\n[[ -r ~/extra ]] && . ~/extra\n", "[z]%", &["[z]%"]),
        POSIX_RELOAD_CHECK,
    );
}

/// zsh 报告函数名，带着集成的 `runode-reload`；在提示符之间新定义的，下一个提示符前也报告。
#[test]
fn real_zsh_reports_its_functions() {
    let rc = "PROMPT='[z]%% '\n";
    let Some(mut shell) = RealShell::start("zsh", "zsh-functions", rc, "[z]%", &["[z]%"]) else { return };
    shell.wait_for_bytes("runode-reload", 0, b"runode-reload");
    let start = shell.raw.len();
    // 名字排在最前，报告里紧跟着 %20，和回显的命令分得开。
    shell.run("aaa-func() { : }");
    shell.wait_for_bytes("the new function", start, b";functions=aaa-func%20");
}

#[test]
fn real_bash_reloads_with_the_integration() {
    reload_in(
        RealShell::start("bash", "bash-reload", "PS1='[b]\\$ '\n[ -r ~/extra ] && . ~/extra\n", "[b]$", &["[b]$"]),
        POSIX_RELOAD_CHECK,
    );
}

/// fish 的提示符：不打招呼，`[f]>` 加上 `extra`。
fn fish_rc(extra: &str) -> String {
    format!("set -g fish_greeting\nfunction fish_prompt; echo -n '[f]> '; end\n{extra}")
}

#[test]
fn real_fish_commands_are_read() {
    let rc = fish_rc("function fish_right_prompt; echo -n '<r>'; end\n");
    read_commands_from(RealShell::start("fish", "fish", &rc, "[f]>", &["[f]>", "<r>"]));
}

/// 包起来的提示符和右侧提示符看到的 $status、$pipestatus 是上一条命令的。
#[test]
fn real_fish_prompts_see_the_exit_status() {
    let rc = "set -g fish_greeting\n\
              function fish_prompt; set -g seen $status $pipestatus; echo -n '[f]> '; end\n\
              function fish_right_prompt; set -g right $status; echo -n '<r>'; end\n";
    let Some(mut shell) = RealShell::start("fish", "fish-prompt-status", rc, "[f]>", &["[f]>", "<r>"]) else {
        return;
    };
    shell.run("true | false");
    shell.run("echo $seen $right");
    assert_eq!(shell.output(1), "1 0 1 1\n");
}

#[test]
fn real_fish_cursor_follows_the_prompt() {
    let Some(mut shell) = RealShell::start("fish", "fish-cursor", &fish_rc(""), "[f]>", &["[f]>"]) else { return };
    cursor_follows_the_prompt(&mut shell);
}

#[test]
fn real_fish_reloads_with_the_integration() {
    reload_in(
        RealShell::start("fish", "fish-reload", &fish_rc("test -r ~/extra; and source ~/extra\n"), "[f]>", &["[f]>"]),
        "echo (set -q RELOADED; and echo $RELOADED; or echo no) (set -q RUNODE_REPORT_TOKEN; and echo $RUNODE_REPORT_TOKEN; or echo gone)",
    );
}

/// 集成加载时报告 `runode-reload`，提示符前报告百分号编码的目录，失败的命令报告退出码；宿主
/// 认得这些报告，把失败的命令记作在报告的目录里跑完、退出码是 1。
fn reports_functions_and_exit_status(shell: Option<RealShell>, fail: &str) {
    let Some(mut shell) = shell else { return };
    shell.wait_for_bytes("runode-reload", 0, b"\x1b]6973;0123456789abcdef;functions=runode-reload");
    // 宿主解码报告的目录，记作提示符所在的目录。
    let dir = shell.home.join("a b;c");
    std::fs::create_dir(&dir).unwrap();
    shell.run("cd 'a b;c'");
    shell.session.take_commands();
    let cwd = shell.session.meta().prompt_cwd.and_then(|cwd| cwd.canonicalize().ok());
    assert_eq!(cwd, dir.canonicalize().ok(), "{}", shell.name);
    let start = shell.raw.len();
    shell.run(fail);
    shell.wait_for_bytes("the exit status", start, b"\x1b]133;D;1");
    let finished = shell.session.take_commands();
    let failed = finished.iter().find(|command| command.cmd == fail);
    assert_eq!(failed.map(|command| command.exit), Some(Some(1)), "{}: {finished:?}", shell.name);
    let cwd = failed.and_then(|command| command.cwd.as_ref()?.canonicalize().ok());
    assert_eq!(cwd, dir.canonicalize().ok(), "{}", shell.name);
}

/// elvish 的提示符：`[e]>`，没有右侧提示符，加上 `extra`。
fn elvish_rc(extra: &str) -> String {
    format!("set edit:prompt = {{ put '[e]> ' }}\nset edit:rprompt = {{ put '' }}\n{extra}")
}

#[test]
fn real_elvish_commands_are_read() {
    read_commands_from(RealShell::start("elvish", "elvish", &elvish_rc(""), "[e]>", &["[e]>"]));
}

#[test]
fn real_elvish_cursor_follows_the_prompt() {
    let Some(mut shell) = RealShell::start("elvish", "elvish-cursor", &elvish_rc(""), "[e]>", &["[e]>"]) else {
        return;
    };
    cursor_follows_the_prompt(&mut shell);
}

/// 用户 rc.elv 里定义的函数在交互环境里照样能用。
#[test]
fn real_elvish_keeps_what_the_user_rc_defines() {
    let rc = elvish_rc("use str\nfn shout {|s| echo (str:to-upper $s) }\n");
    let Some(mut shell) = RealShell::start("elvish", "elvish-rc", &rc, "[e]>", &["[e]>"]) else { return };
    shell.run("shout hi");
    assert_eq!(shell.output(1), "HI\n", "{}", shell.screen());
}

#[test]
fn real_elvish_reports_functions_and_exit_status() {
    reports_functions_and_exit_status(
        RealShell::start("elvish", "elvish-status", &elvish_rc(""), "[e]>", &["[e]>"]),
        "/usr/bin/false",
    );
}

#[test]
fn real_elvish_reloads_with_the_integration() {
    reload_in(
        RealShell::start(
            "elvish",
            "elvish-reload",
            &elvish_rc("use os\nif (os:is-regular ~/extra) { set-env RELOADED yes }\n"),
            "[e]>",
            &["[e]>"],
        ),
        "echo (if (has-env RELOADED) { put $E:RELOADED } else { put no }) \
         (if (has-env RUNODE_REPORT_TOKEN) { put $E:RUNODE_REPORT_TOKEN } else { put gone })",
    );
}

/// nushell 的提示符：不打招呼，`[n]>` 加上 `extra`。
fn nushell_rc(extra: &str) -> String {
    format!(
        "$env.config.show_banner = false\n$env.PROMPT_COMMAND = {{|| '[n]' }}\n\
         $env.PROMPT_COMMAND_RIGHT = {{|| '' }}\n{extra}"
    )
}

#[test]
fn real_nushell_commands_are_read() {
    let rc = nushell_rc("$env.PROMPT_COMMAND_RIGHT = {|| '<r>' }\n");
    read_commands_from(RealShell::start("nu", "nushell", &rc, "[n]>", &["[n]>", "<r>"]));
}

#[test]
fn real_nushell_cursor_follows_the_prompt() {
    let Some(mut shell) = RealShell::start("nu", "nushell-cursor", &nushell_rc(""), "[n]>", &["[n]>"]) else {
        return;
    };
    cursor_follows_the_prompt(&mut shell);
}

#[test]
fn real_nushell_reports_functions_and_exit_status() {
    reports_functions_and_exit_status(
        RealShell::start("nu", "nushell-status", &nushell_rc(""), "[n]>", &["[n]>"]),
        "^false",
    );
}

#[test]
fn real_nushell_reloads_with_the_integration() {
    reload_in(
        RealShell::start(
            "nu",
            "nushell-reload",
            &nushell_rc("if ('~/extra' | path expand | path exists) { $env.RELOADED = 'yes' }\n"),
            "[n]>",
            &["[n]>"],
        ),
        "print $\"($env.RELOADED? | default no) ($env.RUNODE_REPORT_TOKEN? | default gone)\"",
    );
}
