//! VT 回调累积下来、UI 关心的变化：标题和 agent 状态、响铃，以及 shell 集成报告的命令步骤、
//! PATH 和各种名字。

use std::cell::{Cell as StdCell, RefCell};

use runode_model::{
    agent::{Agent, AgentKind, AgentState},
    shell::ShellNames,
};

use super::{Session, log_err};
use crate::{
    agent, history,
    prompt_input::{self, PromptInput},
};

/// VT 回调累积下来、UI 关心的变化。
#[derive(Default)]
pub(super) struct Effects {
    pub(super) title_changed: StdCell<bool>,
    pub(super) bell: StdCell<bool>,
    /// 最近一次 OSC 9;4 进度报告是不是在进行中。
    pub(super) progress: StdCell<Option<bool>>,
    /// shell 集成报告的命令步骤，按到达的先后，由 `take_commands` 取走。
    pub(super) prompts: RefCell<Vec<PromptEvent>>,
    /// shell 集成用 `SHELL_REPORT` 报告的 shell 自己的 PATH，见 `Session::shell_path`。
    pub(super) shell_path: RefCell<Option<std::ffi::OsString>>,
    /// shell 集成用 `SHELL_REPORT` 报告的别名、函数、内建命令和关键字，见 `Session::shell_names`。
    pub(super) shell_names: RefCell<ShellNames>,
    /// 启动 shell 时交给集成脚本的报告口令（见 `shell_integration::prepare`），`SHELL_REPORT`
    /// 带的口令和它一致才采用；没有口令时一条报告都不采用。
    pub(super) report_token: RefCell<Option<String>>,
    /// 带口令的 `command` 报告：shell 说紧接着的 OSC 133;C 是它真的开始运行命令。外层为 `None`
    /// 表示没收到；里面是命令原文，shell 拿不到原文时为 `None`，到时从屏幕上读。由下一个
    /// 133;C 取走，先来了 133;A、133;B 或 133;D 就作废，免得被之后伪造的 133;C 借用。
    pub(super) command_report: RefCell<Option<Option<String>>>,
}

/// shell 集成在显示提示符时、内容和上次报告的不一样时用的私有 OSC：
/// `ESC ] 6973;<口令>;<字段>=<百分号编码的值> BEL`。字段是 `path`（PATH）或 `aliases`、
/// `functions`、`builtins`、`keywords`（名字之间用空格分开）、`alias_values`（一行一个
/// 「名字<Tab>值」）。另有 `command`：shell 开始运行命令时紧接在 OSC 133;C 前面发，值是命令
/// 原文（拿不到时为空），见 `Effects::command_report`。
///
/// 记进历史的命令同样只认这条报告，133;C 自带的命令原文一律不用：伪造的命令进了历史，就会被
/// 当成灰字建议推给用户。
///
/// 任何打印到终端的内容（`cat` 一个文件、ssh 远端的输出）都能写出这样的序列，而报告的 PATH
/// 会被补全拿去跑命令，所以只认带着本 shell 口令的报告。口令只留在 shell 自己的变量里，子进程的
/// 环境里没有，在 shell 里运行的程序打印不出它。只看是否处在提示符状态挡不住伪造：输出里可以先
/// 伪造一个 OSC 133;D。
pub(super) const SHELL_REPORT: &[u8] = b"6973;";

/// 留给未知 OSC 的最多字节数：函数很多的 shell 报告的函数名能有几十 KB。更长的被截断，
/// 不采用。
pub(super) const UNKNOWN_SEQUENCE_MAX_BYTES: usize = 256 * 1024;

impl Effects {
    /// 记下 shell 集成的一条报告 `<口令>;<字段>=<百分号编码的值>`。口令不对、缺口令或者
    /// 这个 shell 没有口令时整条丢掉；不认识的字段不管。
    pub(super) fn shell_report(&self, report: &[u8]) {
        let Some(semicolon) = report.iter().position(|&b| b == b';') else {
            return;
        };
        let (token, report) = (&report[..semicolon], &report[semicolon + 1..]);
        let trusted = self
            .report_token
            .try_borrow()
            .is_ok_and(|expected| expected.as_deref().is_some_and(|expected| same_token(expected.as_bytes(), token)));
        if !trusted {
            return;
        }
        let Some(eq) = report.iter().position(|&b| b == b'=') else {
            return;
        };
        let (field, value) = (&report[..eq], percent_decode(&report[eq + 1..]));
        if field == b"command" {
            if let Ok(mut pending) = self.command_report.try_borrow_mut() {
                *pending = Some((!value.is_empty()).then(|| String::from_utf8_lossy(&value).into_owned()));
            }
            return;
        }
        if field == b"path" {
            if let Ok(mut path) = self.shell_path.try_borrow_mut() {
                use std::os::unix::ffi::OsStringExt as _;
                *path = Some(std::ffi::OsString::from_vec(value));
            }
            return;
        }
        let Ok(mut names) = self.shell_names.try_borrow_mut() else {
            return;
        };
        if field == b"alias_values" {
            let text = String::from_utf8_lossy(&value);
            names.alias_values =
                text.lines().filter_map(|line| line.split_once('\t')).map(|(n, v)| (n.to_owned(), v.to_owned())).collect();
            return;
        }
        let list = match field {
            b"aliases" => &mut names.aliases,
            b"functions" => &mut names.functions,
            b"builtins" => &mut names.builtins,
            b"keywords" => &mut names.keywords,
            _ => return,
        };
        *list = String::from_utf8_lossy(&value).split_whitespace().map(str::to_owned).collect();
    }
}

/// `Effects::prompts` 里的一步。
pub(super) enum PromptEvent {
    /// 提示符画完，shell 等着输入（OSC 133;B）。
    InputStart,
    /// 命令开始运行（OSC 133;C），带着要记进历史的命令。前面没有带口令的 `command` 报告
    /// （可能是伪造的）、或者报告没带原文又没能从屏幕上读到时为 `None`，不记。
    OutputStart(Option<String>),
    /// 命令运行结束（OSC 133;D），带着退出码。
    CommandEnd(Option<i32>),
}

impl Session {
    /// 把 PTY 输出喂给 VT，返回标题或 agent 状态是否变化。
    pub fn feed(&mut self, data: &[u8]) -> bool {
        self.terminal.vt_write(data);
        let mut changed = false;
        // 不少 shell 每次出提示符都重发一遍同样的标题，agent 工作时每一帧转圈都改一次标题，
        // 只有去掉状态前缀后的标题或状态真变了才算。
        if self.effects.title_changed.take() {
            let raw = self.terminal.title().ok().unwrap_or_default();
            let (title, title_agent) = match agent::split_status(raw) {
                Some((state, rest)) => (rest, Some(state)),
                // codex 空闲时不带前缀：刚才还在报告状态的 agent 只要仍在前台，就是停下来了。
                None => (
                    raw,
                    self.title_agent
                        .filter(|_| !self.pty.foreground_is_shell())
                        .map(|agent| Agent { state: AgentState::Idle, ..agent }),
                ),
            };
            let title = (!title.is_empty()).then(|| title.to_owned());
            changed |= title != self.title;
            self.title = title;
            self.title_agent = title_agent;
        }
        // pi 工作中每秒重发一次进度；停下时清掉进度，回到 shell 的不再算 agent。
        if let Some(active) = self.effects.progress.take() {
            self.progress = if active {
                Some(true)
            } else {
                (!self.pty.foreground_is_shell()).then_some(false)
            };
        }
        let progress_agent = |state| {
            let kind = match self.title_agent {
                Some(agent) => agent.kind,
                None if self.title.as_deref().is_some_and(agent::is_pi_title) => AgentKind::Pi,
                None => AgentKind::Other,
            };
            Agent { kind, state }
        };
        let agent = match self.progress {
            Some(true) => Some(progress_agent(AgentState::Working)),
            progress => self.title_agent.or(progress.map(|_| progress_agent(AgentState::Idle))),
        };
        changed |= agent != self.agent;
        self.agent = agent;
        changed
    }

    /// 取走 shell 集成报告运行完了的命令，带着运行的目录和退出码，按结束的先后。每次 `feed`
    /// 之后调用：shell 回到提示符时在这里记下它的目录，之后开始运行的命令就算在那个目录里。
    pub fn take_commands(&mut self) -> Vec<history::Entry> {
        let events = self.effects.prompts.take();
        let mut finished = Vec::new();
        for event in events {
            match event {
                PromptEvent::InputStart => self.prompt_cwd = self.cwd(),
                PromptEvent::OutputStart(command) => {
                    self.running = command.map(|command| {
                        history::Entry::now(command, self.prompt_cwd.clone().or_else(|| self.cwd()))
                    });
                }
                // 有的 shell 每次出提示符都报告一次结束，没在运行的命令时不算。
                PromptEvent::CommandEnd(exit) => {
                    if let Some(mut entry) = self.running.take() {
                        entry.exit = exit;
                        finished.push(entry);
                    }
                }
            }
        }
        finished
    }

    /// 光标停在 shell 提示符上时正在编辑的那条输入，见 `prompt_input::read`。
    pub fn prompt_input(&self) -> Option<PromptInput> {
        log_err("read prompt input", prompt_input::read(&self.terminal)).flatten()
    }

    /// shell 集成报告的 shell 自己的 PATH（每次显示提示符时 PATH 变了才报告）；还没报告过时
    /// 为 `None`。补全跑生成器命令时用它，和用户在 shell 里能找到的命令一致。
    pub fn shell_path(&self) -> Option<std::ffi::OsString> {
        self.effects.shell_path.borrow().clone()
    }

    /// shell 集成报告的别名、函数、内建命令和关键字（内容变了才报告）；没报告过的为空。
    pub fn shell_names(&self) -> ShellNames {
        self.effects.shell_names.borrow().clone()
    }

    /// shell 最近一次等着输入时所在的目录；还没等过输入时现读一次。
    pub fn prompt_cwd(&self) -> Option<std::path::PathBuf> {
        self.prompt_cwd.clone().or_else(|| self.cwd())
    }

    /// 重新读取终端的前台进程，返回 `fallback_title` 或 `agent` 是否变化。
    pub fn refresh_fallback_title(&mut self) -> bool {
        // 还没启动时没有前台进程，标题保持起始目录的名字。
        if !self.pty.started() {
            return false;
        }
        // agent 退出、回到 shell 后，它留下的标题不再代表任何状态。
        let agent_gone = self.agent.is_some() && self.pty.foreground_is_shell();
        if agent_gone {
            self.agent = None;
            self.title_agent = None;
            self.progress = None;
        }
        let title = self.pty.foreground_title();
        if title == self.fallback_title {
            return agent_gone;
        }
        self.fallback_title = title;
        true
    }

    pub fn take_bell(&self) -> bool {
        self.effects.bell.take()
    }
}

/// 两个口令是否相同。逐字节比完全部内容再下结论，耗时不随第一个不同字节的位置变化，
/// 不会因此泄露口令的内容。
fn same_token(expected: &[u8], given: &[u8]) -> bool {
    expected.len() == given.len() && expected.iter().zip(given).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}

/// 解开百分号编码；`%` 后面不是两位十六进制数时原样保留。
fn percent_decode(bytes: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (bytes.get(i + 1).and_then(|&b| hex(b)), bytes.get(i + 2).and_then(|&b| hex(b)))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::testing::*;

    #[test]
    fn agent_status_prefix_is_split_from_the_title() {
        let mut session = idle_session();
        assert!(session.feed("\x1b]0;⠋ 美化图标 | runode\x07".as_bytes()));
        assert_eq!(session.title.as_deref(), Some("美化图标 | runode"));
        assert_eq!(session.agent, Some(Agent { kind: AgentKind::Codex, state: AgentState::Working }));
        // 转圈换一帧不算标题变化。
        assert!(!session.feed("\x1b]0;⠙ 美化图标 | runode\x07".as_bytes()));
        assert!(session.feed("\x1b]0;✳ 美化图标\x07".as_bytes()));
        assert_eq!(session.title.as_deref(), Some("美化图标"));
        assert_eq!(session.agent, Some(Agent { kind: AgentKind::Claude, state: AgentState::Idle }));
        // 前台已经回到 shell（这里的 `cat`），不带前缀的标题不再算 agent 的。
        assert!(session.feed(b"\x1b]0;~\x07"));
        assert_eq!(session.agent, None);
    }

    #[test]
    fn progress_report_marks_the_agent_working() {
        let mut session = idle_session();
        let pi_working = Some(Agent { kind: AgentKind::Pi, state: AgentState::Working });
        assert!(session.feed("\x1b]0;π - runode\x07\x1b]9;4;3\x07".as_bytes()));
        assert_eq!(session.agent, pi_working);
        // 工作中的保活重发和不带前缀的新标题都不改变状态。
        assert!(!session.feed(b"\x1b]9;4;3\x07"));
        assert!(session.feed("\x1b]0;π - 问候 - runode\x07".as_bytes()));
        assert_eq!(session.agent, pi_working);
        // 前台是 shell（这里的 `cat`）时清掉进度，就不再算 agent。
        assert!(session.feed(b"\x1b]9;4;0\x07"));
        assert_eq!(session.agent, None);
    }

    #[test]
    fn finished_commands_carry_the_prompt_directory_and_exit_code() {
        let mut session = reporting_session();
        session.feed(PROMPT);
        assert!(session.take_commands().is_empty());
        let cwd = session.prompt_cwd();
        assert_eq!(session.prompt_input().map(|input| input.text), Some(String::new()));
        session.feed(b"git st");
        let input = session.prompt_input().unwrap();
        assert_eq!((input.before_cursor(), input.at_end), ("git st", true));
        // 报告没带原文，从屏幕上读。
        session.feed(format!("atus\r\n\x1b]6973;{TOKEN};command=\x07\x1b]133;C\x07clean\r\n").as_bytes());
        // 命令还在跑，没有结束报告。
        assert!(session.take_commands().is_empty());
        assert_eq!(session.prompt_input(), None);
        session.feed(b"\x1b]133;D;1\x07");
        let commands = session.take_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!((commands[0].cmd.as_str(), commands[0].exit, &commands[0].cwd), ("git status", Some(1), &cwd));
        // 没在运行命令时的结束报告（有的 shell 每次出提示符都发）不算。
        session.feed(b"\x1b]133;D;0\x07");
        assert!(session.take_commands().is_empty());
    }

    #[test]
    fn the_shell_reports_its_path() {
        let mut session = reporting_session();
        assert_eq!(session.shell_path(), None);
        session.feed(format!("\x1b]6973;{TOKEN};path=/usr/bin%3A/opt/my%20bin\x07").as_bytes());
        assert_eq!(session.shell_path(), Some("/usr/bin:/opt/my bin".into()));
        // 别的私有 OSC 不算。
        session.feed(format!("\x1b]69730;{TOKEN};path=/x\x07\x1b]6973;{TOKEN};other=1\x1b\\").as_bytes());
        assert_eq!(session.shell_path(), Some("/usr/bin:/opt/my bin".into()));
        // 各种名字，空格编码成 %20；很长的列表也收得下。
        let functions: Vec<String> = (0..3000).map(|i| format!("function_number_{i}")).collect();
        let mut report = format!("\x1b]6973;{TOKEN};functions=").into_bytes();
        report.extend(functions.join("%20").bytes());
        report.extend(
            format!(
                "\x07\x1b]6973;{TOKEN};aliases=ll%20gs\x07\x1b]6973;{TOKEN};alias_values=ll%09ls%20-l%0Ags%09git%20status%0A\x07"
            )
            .bytes(),
        );
        session.feed(&report);
        let names = session.shell_names();
        assert_eq!(names.aliases, ["ll", "gs"]);
        assert_eq!(names.alias_values, [("ll".into(), "ls -l".into()), ("gs".into(), "git status".into())]);
        assert_eq!(names.functions, functions);
        assert!(names.builtins.is_empty());
        assert_eq!(percent_decode(b"a%2fb%zz%4"), b"a/b%zz%4");
    }

    #[test]
    fn shell_reports_without_the_right_token_are_ignored() {
        let mut session = reporting_session();
        let wrong = "0123456789abcdef0123456789abcdee";
        // 口令不对、少一位、缺口令（旧格式）、空口令，都不采用。
        session.feed(format!("\x1b]6973;{wrong};path=/evil\x07").as_bytes());
        session.feed(format!("\x1b]6973;{};path=/evil\x07", &TOKEN[1..]).as_bytes());
        session.feed(b"\x1b]6973;path=/evil\x07\x1b]6973;;path=/evil\x07\x1b]6973;aliases=git\x07");
        assert_eq!(session.shell_path(), None);
        assert!(session.shell_names().aliases.is_empty());
        // 先伪造命令结束、回到提示符也没用。
        session.feed(format!("\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07\x1b]6973;{wrong};path=/evil\x07").as_bytes());
        assert_eq!(session.shell_path(), None);
        session.feed(format!("\x1b]6973;{TOKEN};path=/usr/bin\x07").as_bytes());
        assert_eq!(session.shell_path(), Some("/usr/bin".into()));

        // 没注入集成、没有口令的 shell 报告什么都不采用。
        let mut session = idle_session();
        session.feed(format!("\x1b]6973;{TOKEN};path=/evil\x07\x1b]6973;;path=/evil\x07").as_bytes());
        session.feed(b"\x1b]6973;path=/evil\x07");
        assert_eq!(session.shell_path(), None);

        assert!(same_token(b"abc", b"abc"));
        assert!(!same_token(b"abc", b"abd") && !same_token(b"abc", b"ab") && !same_token(b"", b"a"));
    }

    #[test]
    fn the_command_line_sent_by_the_shell_wins_over_the_screen() {
        let mut session = reporting_session();
        session.feed(PROMPT);
        // 屏幕上只看得到一部分（比如被插件改写过），shell 报告的原文用百分号编码，分号、
        // 换行和 ESC 都原样还原；133;C 自带的原文不用。
        session.feed(
            format!("echo\r\n\x1b]6973;{TOKEN};command=echo%20a%3Bb%0Ac%1B\x07\x1b]133;C;cmdline_url=other\x07\x1b]133;D;0\x07")
                .as_bytes(),
        );
        let commands = session.take_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].cmd, "echo a;b\nc\x1b");
    }

    #[test]
    fn forged_command_starts_stay_out_of_the_history() {
        let mut session = reporting_session();
        let wrong = "0123456789abcdef0123456789abcdee";
        let forged = [
            // 程序输出里伪造整套提示符和带原文的命令开始。
            "\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07evil\r\n\x1b]133;C;cmdline_url=evil\x07\x1b]133;D;0\x07".to_owned(),
            // 口令不对的 command 报告不算。
            format!("\x1b]133;A\x07$ \x1b]133;B\x07evil\r\n\x1b]6973;{wrong};command=evil\x07\x1b]133;C\x07\x1b]133;D;0\x07"),
            // 真的报告之后先来了提示符或命令结束，就不能再被伪造的 133;C 借用。
            format!("\x1b]6973;{TOKEN};command=ls\x07\x1b]133;D;0\x07\x1b]133;C;cmdline_url=evil\x07\x1b]133;D;0\x07"),
            format!("\x1b]6973;{TOKEN};command=ls\x07\x1b]133;A\x07$ \x1b]133;B\x07evil\r\n\x1b]133;C\x07\x1b]133;D;0\x07"),
            format!("\x1b]6973;{TOKEN};command=ls\x07\x1b]133;B\x07evil\r\n\x1b]133;C\x07\x1b]133;D;0\x07"),
        ];
        for bytes in forged {
            session.feed(bytes.as_bytes());
            assert!(session.take_commands().is_empty(), "{bytes:?}");
        }
        // 没有口令的 shell 不记任何命令。
        let mut session = idle_session();
        session.feed(PROMPT);
        session.feed(format!("ls\r\n\x1b]6973;{TOKEN};command=ls\x07\x1b]133;C\x07\x1b]133;D;0\x07").as_bytes());
        assert!(session.take_commands().is_empty());
    }

    #[test]
    fn full_reset_clears_the_title() {
        let mut session = idle_session();
        assert!(session.feed(b"\x1b]2;hello\x07"));
        assert_eq!(session.title.as_deref(), Some("hello"));
        assert!(session.feed(b"\x1bc"));
        assert_eq!(session.title, None);
    }
}
