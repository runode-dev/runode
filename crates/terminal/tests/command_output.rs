//! 按 shell 集成标出的提示符读某条命令的输出（`read --command`）：先是手写的理想序列，后面
//! 是真的 bash、zsh 加载仓库里的集成脚本发出的序列。

mod common;

use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use common::{PROMPT, idle_host};
use runode_shared_types::{grid::GridSize, settings::TermSettings, shell::IntegrationMode};
use runode_terminal::{
    host_session::HostSession,
    pty::{Pty, PtyEvent},
};

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
}

impl Drop for RealShell {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

const REAL_SIZE: GridSize = GridSize { cols: 40, rows: 12, cell_width_px: 8, cell_height_px: 16 };
const REAL_WAIT: Duration = Duration::from_secs(10);

/// 仓库里的集成脚本所在的目录。
fn integration_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("shell-integration")
}

impl RealShell {
    /// 起 `/bin/` 下的 `shell`（`bash` 或 `zsh`），`rc` 写进它的启动配置；`name` 在同时跑的测试里
    /// 各不相同，失败时也用它说是哪一个。系统里没有这个 shell 时为 `None`。
    fn start(shell: &str, name: &str, rc: &str, prompt: &'static str, idle: &'static [&'static str]) -> Option<Self> {
        let program = format!("/bin/{shell}");
        if !Path::new(&program).exists() {
            return None;
        }
        let home = std::env::temp_dir().join(format!("rn-cmdout-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // 和 `shell_integration::prepare` 一样：bash 用 --rcfile 起非登录的交互式 shell，脚本自己加载
        // 登录配置；zsh 把 ZDOTDIR 指到集成目录，那里的 .zshenv 还原 ZDOTDIR 后加载用户配置和集成。
        let (rc_file, launch) = if shell == "bash" {
            let script = integration_dir().join("bash/runode.bash");
            (".bash_profile", format!("{program} --rcfile '{}'", script.display()))
        } else {
            (".zshrc", format!("ZDOTDIR='{}' {program} -l", integration_dir().join("zsh").display()))
        };
        std::fs::write(home.join(rc_file), rc).unwrap();
        let launcher = home.join("launch.sh");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nexec /usr/bin/env -i HOME='{}' PATH=/usr/bin:/bin TERM=xterm-256color LANG=en_US.UTF-8 \
                 BASH_SILENCE_DEPRECATION_WARNING=1 RUNODE_REPORT_TOKEN=0123456789abcdef \
                 RUNODE_SHELL_FEATURES=cursor:steady {launch}\n",
                home.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let (tx, output) = mpsc::channel();
        let sink = Box::new(move |event| match event {
            PtyEvent::Output(data) => tx.send(data.to_vec()).is_ok(),
            PtyEvent::Exited => false,
        });
        let pty =
            Pty::spawn(REAL_SIZE, Some(launcher.to_str().unwrap()), Some(&home), IntegrationMode::Off, sink).unwrap();
        let session = HostSession::new(REAL_SIZE, pty, None, &TermSettings::default()).unwrap();
        let mut real = Self { session, output, prompt, idle, prompts: 0, name: name.into(), raw: Vec::new(), home };
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
    let name = shell.name.clone();
    // 刚起来的 shell 还没跑过命令：开头打印的内容（如果有）算一条截断的，没有就是一条都没有。
    assert!(shell.session.command_output(1).map_or(true, |(_, truncated)| truncated), "{name}");

    // 多行输出。
    shell.run("printf 'a\\nb\\nc\\n'");
    assert_eq!(shell.output(1), "a\nb\nc\n", "{name}");
    // 没有输出；之前那条是第二条。
    shell.run("true");
    assert_eq!(shell.output(1), "", "{name}");
    assert_eq!(shell.output(2), "a\nb\nc\n", "{name}");
    // 空着回车不算一条命令。
    shell.run("");
    assert_eq!(shell.output(1), "", "{name}");
    assert_eq!(shell.output(2), "a\nb\nc\n", "{name}");
    // 命令行和输出都比终端宽，软折行。
    let wide = "y".repeat(45);
    shell.run(&format!("echo {wide}"));
    assert_eq!(shell.output(1), format!("{}\n{}\n", &wide[..40], &wide[40..]), "{name}");
    assert_eq!(shell.output(3), "a\nb\nc\n", "{name}");

    // 还在跑的命令读到底：`read` 等着输入时只有第一行。
    shell.session.write(b"echo one; read x; echo two\r".to_vec());
    shell.wait_until("the first line", |session| session.screen_text(Some(200)).unwrap().contains("\none\n"));
    assert_eq!(shell.output(1), "one\n", "{name}");
    assert_eq!(shell.output(2), format!("{}\n{}\n", &wide[..40], &wide[40..]), "{name}");
    shell.session.write(b"\r".to_vec());
    shell.wait_for_prompt();
    // 回车被终端回显成一个空行。
    assert_eq!(shell.output(1), "one\n\ntwo\n", "{name}");
    assert_eq!(shell.output(4), "a\nb\nc\n", "{name}");
}

#[test]
fn real_bash_commands_are_read() {
    read_commands_from(RealShell::start("bash", "bash", "PS1='[b]\\$ '\n", "[b]$", &["[b]$"]));
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
    let new_enough = std::process::Command::new("/bin/bash")
        .args(["-c", "[ \"${BASH_VERSINFO[0]}\" -gt 4 ] || { [ \"${BASH_VERSINFO[0]}\" -eq 4 ] && [ \"${BASH_VERSINFO[1]}\" -ge 2 ]; }"])
        .status()
        .is_ok_and(|status| status.success());
    if new_enough {
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
    let has_ps0 = std::process::Command::new("/bin/bash")
        .args(["-c", "[ \"${BASH_VERSINFO[0]}\" -gt 4 ] || { [ \"${BASH_VERSINFO[0]}\" -eq 4 ] && [ \"${BASH_VERSINFO[1]}\" -ge 4 ]; }"])
        .status()
        .is_ok_and(|status| status.success());
    let Some(mut shell) = RealShell::start("bash", "bash-cursor", "PS1='[b]\\$ '\n", "[b]$", &["[b]$"]) else { return };
    if has_ps0 {
        cursor_follows_the_prompt(&mut shell);
    } else {
        shell.run("echo out");
        assert!(rfind(&shell.raw, BAR).is_none() && rfind(&shell.raw, RESET).is_none(), "old bash touched the cursor");
    }
}

/// `runode-reload` 换成一个新的 shell：重新读了用户配置，集成照旧，报告带着同一个口令，口令
/// 不留在环境里。用户配置加载 `extra`，换之前才写进去。
fn reload_in(shell: Option<RealShell>) {
    let Some(mut shell) = shell else { return };
    let name = shell.name.clone();
    std::fs::write(shell.home.join("extra"), "export RELOADED=yes\n").unwrap();
    let start = shell.raw.len();
    shell.run(" runode-reload");
    shell.wait_for_bytes("a report after the reload", start, b"\x1b]6973;0123456789abcdef;cwd=");
    shell.run("echo ${RELOADED-no} ${RUNODE_REPORT_TOKEN-gone}");
    assert_eq!(shell.output(1), "yes gone\n", "{name}");
}

#[test]
fn real_zsh_reloads_with_the_integration() {
    reload_in(RealShell::start(
        "zsh",
        "zsh-reload",
        "PROMPT='[z]%% '\n[[ -r ~/extra ]] && . ~/extra\n",
        "[z]%",
        &["[z]%"],
    ));
}

#[test]
fn real_bash_reloads_with_the_integration() {
    reload_in(RealShell::start(
        "bash",
        "bash-reload",
        "PS1='[b]\\$ '\n[ -r ~/extra ] && . ~/extra\n",
        "[b]$",
        &["[b]$"],
    ));
}
