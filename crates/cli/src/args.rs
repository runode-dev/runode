//! 解析命令行参数。选项可以写在位置参数前后，`--` 之后的都当位置参数。

use std::{net::IpAddr, path::PathBuf, time::Duration};

use runode_protocol::Placement;
use runode_shared_types::input::parse_keys;

use crate::{SetupTarget, select::Selector};

/// 用法说明，`runode help` 打印它。改了命令或选项，`runode-completion` 里 runode 自己的命令规格
/// 也要跟着改，按 Tab 才补得出来。
pub(crate) const HELP: &str = "\
usage: runode [COMMAND]

rn is a short name for runode: `rn list` is `runode list`.
Without a command, runode opens its window. Commands talk to the running runode
app; inside a runode terminal they find it through RUNODE_SOCKET, and
RUNODE_SESSION names the terminal they run in.

SESSION picks one terminal; it must match exactly one, else runode lists the
candidates. It is one of
  ID            a session id or any unique prefix of it, as `runode list` shows
  self, .       your own terminal
  left, right, up, down
                the pane next to yours in that direction
  next, prev    the next or previous pane in your tab, wrapping around
  pane:N        pane N of your tab (of the front window's tab outside runode)
  tab:N, tab:N.M
                the focused pane of tab N in your workspace, or its pane M
  win:W/..., ws:K/...
                look in window W or workspace K instead: win:2, win:2/tab:1.2,
                ws:3/pane:1
  title:TEXT    the title contains TEXT, ignoring case
  agent:KIND[:STATE]
                runs that agent (claude, codex, ...), in that state
  cwd:DIR       works in DIR; a bare name matches the last part of the path
Windows, workspaces, tabs and panes count from 1 in the order the app shows
them. The positional forms need a runode window.

commands:
  list [--json]               list the terminal sessions: * marks your own, REL
                              where they sit next to yours, FG the program in
                              front, VIEW shown, hidden (another tab) or bg (in
                              no window)
  read [SESSION] [--lines N] [--command [N]]
                              print the text on the screen; --lines: N lines
                              from the bottom, scrollback included; --command:
                              the output of the Nth last command (default 1),
                              which needs shell integration. SESSION defaults
                              to your own
  send SESSION [TEXT...] [--paste] [--key KEY]... [--enter] [--wait]
       [--timeout SECS]       type TEXT (words joined by spaces; - reads stdin)
                              into the session, or paste it with --paste; then
                              press each KEY; then Enter with --enter. A KEY is
                              ctrl-c, alt-b, shift-tab, esc, enter, tab, up,
                              pageup, f5 and the like; 'down*3' (quoted for
                              the shell) presses it three times. --wait then
                              waits: for the agent like --for done if one runs
                              there (failing if it shows no activity within
                              10 seconds), for the command like --for command if
                              Enter ran one at a shell prompt with shell
                              integration, else until the screen is quiet for
                              2 seconds; it says which on stderr
  wait SESSION [--for UNTIL] [--timeout SECS]
                              wait until UNTIL, one of
                                stopped  the agent is not working: idle, asking
                                         you, or no agent (default)
                                done     the agent worked, then stopped
                                working, idle, blocked
                                         the agent is in that state
                                command  the next command at the shell prompt
                                         finished; prints exit N and fails with
                                         status 4 if N is not 0
                                text REGEX [--lines N] [--new]
                                         a line on the screen (or in the last N
                                         lines) matches REGEX; --new skips the
                                         lines already there; prints the line
                                quiet SECS
                                         the screen did not change for SECS
                              agent states print the state they reached. If
                              runode is upgraded meanwhile, the wait goes on
                              (except --for command, which fails)
  open [--tab|--right|--down] [--near SESSION] [--cwd DIR] [--focus]
       [-- COMMAND...]        open a terminal in the app: a new tab after the
                              one SESSION is in (default), or split SESSION's
                              pane to the right or down. SESSION defaults to
                              your own, else the front window's pane; DIR to
                              SESSION's directory. Without --focus the app
                              stays where it is. COMMAND is typed into the new
                              shell. Prints the new session's id
  kill SESSION                end the session and close its pane
  focus [SESSION]             show the session's pane and bring its window to
                              the front; SESSION defaults to your own
  setup claude|codex [--print]
                              teach the agent to use runode: installs a skill
                              in ~/.claude/skills/runode or
                              ~/.agents/skills/runode; --print shows it instead
  remote pair [--addr ADDR]...
                              pair a phone for remote access: shows a QR code
                              and its link, valid for 5 minutes, and waits for
                              the phone; --addr also offers ADDR (say
                              127.0.0.1 for a simulator on this Mac). Needs
                              remote-access = true in the config; once paired,
                              offers to set terminal-host = true so remote
                              access keeps running after you quit runode
  remote devices [--json]     list the paired phones
  remote revoke DEVICE        unpair a phone (an id or a unique prefix of it,
                              as `runode remote devices` shows); it is
                              disconnected within seconds
  help                        show this help
  version                     show the version

exit status: 0 done, 1 failed, 2 bad arguments, 3 the session exited,
4 the command waited for failed, 124 timed out.
";

/// 一条命令。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Command {
    Help,
    Version,
    List {
        json: bool,
    },
    Read {
        session: Option<Selector>,
        lines: Option<u32>,
        /// 读倒数第几条命令的输出。
        command: Option<u32>,
    },
    Send {
        session: Selector,
        text: Text,
        /// 文字按粘贴发，不逐字打。
        paste: bool,
        /// 打完字以后依次按的键，每项是 `runode_shared_types::input::parse_keys` 认的写法。
        keys: Vec<String>,
        enter: bool,
        wait: bool,
        timeout: Option<Duration>,
    },
    Wait {
        session: Selector,
        until: Until,
        timeout: Option<Duration>,
    },
    Open {
        placement: Placement,
        near: Option<Selector>,
        cwd: Option<PathBuf>,
        focus: bool,
        command: String,
    },
    Kill {
        session: Selector,
    },
    Focus {
        session: Option<Selector>,
    },
    Setup {
        target: SetupTarget,
        print: bool,
    },
    /// 给手机配对远程访问。`addrs` 是除了本机地址以外另外放进配对 URI 的地址，排在前面。
    RemotePair {
        addrs: Vec<IpAddr>,
    },
    RemoteDevices {
        json: bool,
    },
    /// 撤销一台配对过的设备；`device` 是它的标识或者标识的前缀。
    RemoteRevoke {
        device: String,
    },
}

/// `send` 要打的字。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Text {
    /// 参数里给的，可能为空（只按键或回车）。
    Given(String),
    /// 从标准输入读。
    Stdin,
}

/// `wait` 等到什么时候。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Until {
    /// 不在干活：空闲、等回答，或者前台没有 agent。
    Stopped,
    /// 干过活又停下了。
    Done,
    Working,
    Idle,
    Blocked,
    /// shell 集成报告下一条命令运行完了。
    Command,
    /// 屏幕上（`lines` 给了时是最后这么多行里）有一行对上 `pattern`；`new` 时开始等的时候已经在
    /// 的行不算。`pattern` 已经确认能编译。
    Text {
        pattern: String,
        lines: Option<u32>,
        new: bool,
    },
    /// 屏幕上的字这么久没变。
    Quiet(Duration),
}

/// 解析参数，出错时返回说明。
pub(crate) fn parse(args: &[String]) -> Result<Command, String> {
    let mut words = Vec::new();
    let mut flags = Flags::default();
    let mut rest = args.iter().peekable();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--" => {
                words.extend(rest.by_ref().cloned());
            }
            "-h" | "--help" => flags.help = true,
            "-V" | "--version" => flags.version = true,
            "--json" => flags.json = true,
            "--enter" => flags.enter = true,
            "--wait" => flags.wait = true,
            "--paste" => flags.paste = true,
            "--new" => flags.new = true,
            "--print" => flags.print = true,
            "--key" => {
                let key = value(&mut rest, arg)?;
                parse_keys(key).map_err(|err| format!("--key: {err}"))?;
                flags.keys.push(key.to_owned());
            }
            "--lines" => flags.lines = Some(value(&mut rest, arg)?.parse().map_err(|_| "--lines takes a count")?),
            "--command" => {
                // 次数可以不给。会话标识也可能全是数字，所以只把短的数字当次数，长的留给会话。
                let count = rest.next_if(|next| next.len() <= 4 && next.bytes().all(|b| b.is_ascii_digit()));
                flags.command = Some(count.map_or(Ok(1), |count| command_count(count))?);
            }
            option if option.starts_with("--command=") => {
                flags.command = Some(command_count(&option["--command=".len()..])?);
            }
            "--timeout" => flags.timeout = Some(seconds(value(&mut rest, arg)?, arg)?),
            "--for" => flags.until = Some(until(&mut rest)?),
            "--tab" | "--right" | "--down" => {
                let placement = match arg.as_str() {
                    "--tab" => Placement::Tab,
                    "--right" => Placement::Right,
                    _ => Placement::Down,
                };
                if flags.placement.replace(placement).is_some_and(|before| before != placement) {
                    return Err("give only one of --tab, --right and --down".into());
                }
            }
            "--near" => flags.near = Some(Selector::parse(value(&mut rest, arg)?)?),
            "--addr" => {
                let addr = value(&mut rest, arg)?;
                flags.addrs.push(addr.parse().map_err(|_| format!("--addr takes an IP address, not {addr}"))?);
            }
            "--cwd" => flags.cwd = Some(value(&mut rest, arg)?.into()),
            "--focus" => flags.focus = true,
            // 单独一个 `-` 是位置参数（`send` 从标准输入读）。
            option if option.starts_with('-') && option != "-" => return Err(format!("unknown option {option}")),
            _ => words.push(arg.clone()),
        }
    }
    if flags.help {
        return Ok(Command::Help);
    }
    if flags.version {
        return Ok(Command::Version);
    }
    let Some((name, words)) = words.split_first() else {
        return Ok(Command::Help);
    };
    let session = |word: Option<&String>| word.map(|word| Selector::parse(word)).transpose();
    let command = match name.as_str() {
        "help" => Command::Help,
        "version" => Command::Version,
        "list" => {
            no_more(words, 0, name)?;
            Command::List { json: std::mem::take(&mut flags.json) }
        }
        "read" => {
            no_more(words, 1, name)?;
            let command = flags.command.take();
            if command.is_some() && flags.lines.is_some() {
                return Err("give only one of --lines and --command".into());
            }
            Command::Read { session: session(words.first())?, lines: flags.lines.take(), command }
        }
        "send" => {
            let (target, text) = words.split_first().ok_or("send needs a SESSION")?;
            let text = match text {
                [dash] if dash == "-" => Text::Stdin,
                words => Text::Given(words.join(" ")),
            };
            if text == Text::Given(String::new()) && flags.keys.is_empty() && !flags.enter {
                return Err("send needs TEXT, --key or --enter".into());
            }
            Command::Send {
                session: Selector::parse(target)?,
                text,
                paste: std::mem::take(&mut flags.paste),
                keys: std::mem::take(&mut flags.keys),
                enter: std::mem::take(&mut flags.enter),
                wait: std::mem::take(&mut flags.wait),
                timeout: flags.timeout.take(),
            }
        }
        "wait" => {
            no_more(words, 1, name)?;
            let target = words.first().ok_or("wait needs a SESSION")?;
            let mut until = flags.until.take().unwrap_or(Until::Stopped);
            if let Until::Text { lines, new, .. } = &mut until {
                *lines = flags.lines.take();
                *new = std::mem::take(&mut flags.new);
            }
            Command::Wait { session: Selector::parse(target)?, until, timeout: flags.timeout.take() }
        }
        "open" => Command::Open {
            placement: flags.placement.take().unwrap_or(Placement::Tab),
            near: flags.near.take(),
            cwd: flags.cwd.take(),
            focus: std::mem::take(&mut flags.focus),
            command: words.join(" "),
        },
        "kill" => {
            no_more(words, 1, name)?;
            Command::Kill { session: Selector::parse(words.first().ok_or("kill needs a SESSION")?)? }
        }
        "focus" => {
            no_more(words, 1, name)?;
            Command::Focus { session: session(words.first())? }
        }
        "setup" => {
            no_more(words, 1, name)?;
            let target = match words.first().map(String::as_str) {
                Some("claude") => SetupTarget::Claude,
                Some("codex") => SetupTarget::Codex,
                Some(other) => return Err(format!("setup knows claude and codex, not {other}")),
                None => return Err("setup needs claude or codex".into()),
            };
            Command::Setup { target, print: std::mem::take(&mut flags.print) }
        }
        "remote" => {
            let (what, words) = words.split_first().ok_or("remote needs pair, devices or revoke")?;
            match what.as_str() {
                "pair" => {
                    no_more(words, 0, "remote pair")?;
                    Command::RemotePair { addrs: std::mem::take(&mut flags.addrs) }
                }
                "devices" => {
                    no_more(words, 0, "remote devices")?;
                    Command::RemoteDevices { json: std::mem::take(&mut flags.json) }
                }
                "revoke" => {
                    no_more(words, 1, "remote revoke")?;
                    Command::RemoteRevoke { device: words.first().ok_or("remote revoke needs a DEVICE")?.clone() }
                }
                other => return Err(format!("remote knows pair, devices and revoke, not {other}")),
            }
        }
        other => return Err(format!("unknown command {other}")),
    };
    flags.unused().map_or(Ok(command), |flag| Err(format!("{name} does not take {flag}")))
}

/// 读到的选项。命令用掉的取走，剩下的就是这条命令不认的。
#[derive(Default)]
struct Flags {
    help: bool,
    version: bool,
    json: bool,
    enter: bool,
    wait: bool,
    paste: bool,
    new: bool,
    print: bool,
    keys: Vec<String>,
    lines: Option<u32>,
    command: Option<u32>,
    timeout: Option<Duration>,
    until: Option<Until>,
    placement: Option<Placement>,
    near: Option<Selector>,
    cwd: Option<PathBuf>,
    focus: bool,
    addrs: Vec<IpAddr>,
}

impl Flags {
    fn unused(&self) -> Option<&'static str> {
        [
            (self.json, "--json"),
            (self.enter, "--enter"),
            (self.wait, "--wait"),
            (self.paste, "--paste"),
            (self.new, "--new"),
            (self.print, "--print"),
            (!self.keys.is_empty(), "--key"),
            (self.lines.is_some(), "--lines"),
            (self.command.is_some(), "--command"),
            (self.timeout.is_some(), "--timeout"),
            (self.until.is_some(), "--for"),
            (self.placement.is_some(), "--tab, --right or --down"),
            (self.near.is_some(), "--near"),
            (self.cwd.is_some(), "--cwd"),
            (self.focus, "--focus"),
            (!self.addrs.is_empty(), "--addr"),
        ]
        .into_iter()
        .find_map(|(set, flag)| set.then_some(flag))
    }
}

fn value<'a>(rest: &mut impl Iterator<Item = &'a String>, option: &str) -> Result<&'a str, String> {
    rest.next().map(String::as_str).ok_or_else(|| format!("{option} needs a value"))
}

fn seconds(value: &str, option: &str) -> Result<Duration, String> {
    value
        .parse::<f64>()
        .ok()
        .and_then(|secs| Duration::try_from_secs_f64(secs).ok())
        .ok_or_else(|| format!("{option} takes seconds, not {value}"))
}

fn command_count(value: &str) -> Result<u32, String> {
    value.parse().ok().filter(|&n| n > 0).ok_or_else(|| format!("--command takes a count from 1, not {value}"))
}

/// `--for` 后面的写法；`text` 和 `quiet` 还要再取一个值。
fn until<'a>(rest: &mut impl Iterator<Item = &'a String>) -> Result<Until, String> {
    Ok(match value(rest, "--for")? {
        "stopped" => Until::Stopped,
        "done" => Until::Done,
        "working" => Until::Working,
        "idle" => Until::Idle,
        "blocked" => Until::Blocked,
        "command" => Until::Command,
        "text" => {
            let pattern = value(rest, "--for text")?;
            regex::Regex::new(pattern).map_err(|err| format!("--for text: {err}"))?;
            Until::Text { pattern: pattern.to_owned(), lines: None, new: false }
        }
        "quiet" => Until::Quiet(seconds(value(rest, "--for quiet")?, "--for quiet")?),
        other => {
            return Err(format!(
                "--for takes stopped, done, working, idle, blocked, command, text REGEX or quiet SECS, not {other}"
            ));
        }
    })
}

fn no_more(words: &[String], most: usize, name: &str) -> Result<(), String> {
    match words.get(most) {
        Some(extra) => Err(format!("{name} does not take {extra}")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &str) -> Result<Command, String> {
        super::parse(&args.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
    }

    fn id(prefix: &str) -> Selector {
        Selector::parse(prefix).unwrap()
    }

    fn send(session: &str, text: &str) -> Command {
        Command::Send {
            session: id(session),
            text: Text::Given(text.into()),
            paste: false,
            keys: Vec::new(),
            enter: false,
            wait: false,
            timeout: None,
        }
    }

    #[test]
    fn commands_and_their_options() {
        assert_eq!(parse(""), Ok(Command::Help));
        assert_eq!(parse("list --json"), Ok(Command::List { json: true }));
        assert_eq!(
            parse("--lines 5 read ab12"),
            Ok(Command::Read { session: Some(id("ab12")), lines: Some(5), command: None })
        );
        let Ok(Command::Send { enter, wait, timeout, .. }) =
            parse("send ab12 hello  world --enter --wait --timeout 1.5")
        else {
            panic!("not a send");
        };
        assert_eq!((enter, wait, timeout), (true, true, Some(Duration::from_millis(1500))));
        assert_eq!(parse("send ab12 hello  world"), Ok(send("ab12", "hello world")));
        assert!(matches!(parse("send ab12 -"), Ok(Command::Send { text: Text::Stdin, .. })));
        assert_eq!(parse("send ab12 -- --enter"), Ok(send("ab12", "--enter")));
        assert_eq!(
            parse("wait ab12 --for done"),
            Ok(Command::Wait { session: id("ab12"), until: Until::Done, timeout: None })
        );
        assert_eq!(parse("wait ab12"), Ok(Command::Wait { session: id("ab12"), until: Until::Stopped, timeout: None }));
    }

    #[test]
    fn keys_and_paste() {
        assert_eq!(
            parse("send left --key ctrl-c --key down*3 --enter"),
            Ok(Command::Send {
                session: Selector::parse("left").unwrap(),
                text: Text::Given(String::new()),
                paste: false,
                keys: vec!["ctrl-c".into(), "down*3".into()],
                enter: true,
                wait: false,
                timeout: None,
            })
        );
        assert!(matches!(parse("send ab12 --paste some text"), Ok(Command::Send { paste: true, .. })));
        // 写错的键在发出去之前就报，算参数错误。
        assert!(parse("send ab12 --key ctrl-?").unwrap_err().contains("--key"));
        assert!(parse("send ab12 --key down*0").unwrap_err().contains("--key"));
        assert!(parse("send ab12").unwrap_err().contains("TEXT, --key or --enter"));
    }

    #[test]
    fn reading_a_command() {
        let read = |session: Option<&str>, command| Command::Read { session: session.map(id), lines: None, command };
        assert_eq!(parse("read --command"), Ok(read(None, Some(1))));
        assert_eq!(parse("read --command 2"), Ok(read(None, Some(2))));
        assert_eq!(parse("read ab12 --command 3"), Ok(read(Some("ab12"), Some(3))));
        assert_eq!(parse("read --command=2 ab12"), Ok(read(Some("ab12"), Some(2))));
        // 长的数字是会话标识的前缀，不是次数。
        assert_eq!(parse("read --command 12345678"), Ok(read(Some("12345678"), Some(1))));
        assert!(parse("read --command 0").unwrap_err().contains("--command"));
        assert!(parse("read --command --lines 3").unwrap_err().contains("only one"));
    }

    #[test]
    fn waiting_for_commands_text_and_quiet() {
        let wait = |until| Ok(Command::Wait { session: id("ab12"), until, timeout: None });
        assert_eq!(parse("wait ab12 --for command"), wait(Until::Command));
        assert_eq!(
            parse("wait ab12 --for text error|warn --lines 50 --new"),
            wait(Until::Text { pattern: "error|warn".into(), lines: Some(50), new: true })
        );
        assert_eq!(parse("wait ab12 --for quiet 2.5"), wait(Until::Quiet(Duration::from_millis(2500))));
        assert!(parse("wait ab12 --for text (").unwrap_err().contains("--for text"));
        assert!(parse("wait ab12 --for text").unwrap_err().contains("needs a value"));
        assert!(parse("wait ab12 --for quiet").unwrap_err().contains("needs a value"));
        assert!(parse("wait ab12 --for quiet soon").unwrap_err().contains("seconds"));
        assert!(parse("wait ab12 --for done --new").unwrap_err().contains("does not take --new"));
        assert!(parse("wait ab12 --lines 5").unwrap_err().contains("does not take --lines"));
    }

    #[test]
    fn setup_targets() {
        assert_eq!(parse("setup claude"), Ok(Command::Setup { target: SetupTarget::Claude, print: false }));
        assert_eq!(parse("setup codex --print"), Ok(Command::Setup { target: SetupTarget::Codex, print: true }));
        assert!(parse("setup").unwrap_err().contains("claude or codex"));
        assert!(parse("setup vim").unwrap_err().contains("not vim"));
    }

    #[test]
    fn window_commands() {
        assert_eq!(
            parse("open"),
            Ok(Command::Open {
                placement: Placement::Tab,
                near: None,
                cwd: None,
                focus: false,
                command: String::new()
            })
        );
        assert_eq!(
            parse("open --right --near ab12 --cwd /tmp --focus -- claude --model opus"),
            Ok(Command::Open {
                placement: Placement::Right,
                near: Some(id("ab12")),
                cwd: Some("/tmp".into()),
                focus: true,
                command: "claude --model opus".into(),
            })
        );
        assert_eq!(parse("kill ab12"), Ok(Command::Kill { session: id("ab12") }));
        assert_eq!(parse("focus"), Ok(Command::Focus { session: None }));
        assert!(parse("open --right --down").unwrap_err().contains("only one"));
        assert!(parse("kill").unwrap_err().contains("SESSION"));
        assert!(parse("focus --right").unwrap_err().contains("does not take"));
    }

    #[test]
    fn remote_access_commands() {
        assert_eq!(parse("remote pair"), Ok(Command::RemotePair { addrs: Vec::new() }));
        assert_eq!(
            parse("remote pair --addr 127.0.0.1 --addr ::1"),
            Ok(Command::RemotePair { addrs: vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()] })
        );
        assert_eq!(parse("remote devices --json"), Ok(Command::RemoteDevices { json: true }));
        assert_eq!(parse("remote revoke 0a1b"), Ok(Command::RemoteRevoke { device: "0a1b".into() }));
        assert!(parse("remote").unwrap_err().contains("pair, devices or revoke"));
        assert!(parse("remote frob").unwrap_err().contains("not frob"));
        assert!(parse("remote revoke").unwrap_err().contains("DEVICE"));
        assert!(parse("remote pair --addr nowhere").unwrap_err().contains("IP address"));
        assert!(parse("remote devices --addr 127.0.0.1").unwrap_err().contains("does not take --addr"));
        assert!(parse("remote pair extra").unwrap_err().contains("does not take extra"));
    }

    #[test]
    fn mistakes_are_reported() {
        assert!(parse("frobnicate").unwrap_err().contains("unknown command"));
        assert!(parse("list --bogus").unwrap_err().contains("unknown option"));
        assert!(parse("list --enter").unwrap_err().contains("does not take --enter"));
        assert!(parse("read a b").unwrap_err().contains("does not take b"));
        assert!(parse("send").unwrap_err().contains("SESSION"));
        assert!(parse("wait ab12 --for sleeping").unwrap_err().contains("--for"));
        assert!(parse("wait ab12 --timeout soon").unwrap_err().contains("--timeout"));
        assert!(parse("read --lines").unwrap_err().contains("needs a value"));
        // 认不出的会话写法也算参数错误。
        assert!(parse("read sideways").unwrap_err().contains("sideways"));
        assert!(parse("read tab:x").unwrap_err().contains("tab:x"));
    }

    /// 用法说明里的每条命令（连同 `setup claude|codex` 这样写死的词）在补全的命令规格里都有，
    /// 规格里不隐藏的子命令也都写进了用法说明：两边各改各的时，漏了的那边会让这里失败。
    #[test]
    fn help_and_completion_spec_list_the_same_commands() {
        let spec: serde_json::Value = serde_json::from_str(include_str!("../../completion/specs/runode.json")).unwrap();
        let names = |value: &serde_json::Value| -> Vec<String> {
            match value {
                serde_json::Value::String(name) => vec![name.clone()],
                serde_json::Value::Array(names) => names.iter().filter_map(|n| n.as_str().map(str::to_owned)).collect(),
                _ => Vec::new(),
            }
        };
        let subcommands = |node: &serde_json::Value| node["subcommands"].as_array().cloned().unwrap_or_default();
        // 一层里能接的词：子命令名和参数的固定候选。
        let words = |node: &serde_json::Value| -> Vec<String> {
            let mut words: Vec<String> = subcommands(node).iter().flat_map(|sub| names(&sub["name"])).collect();
            let args = match &node["args"] {
                serde_json::Value::Array(args) => args.clone(),
                serde_json::Value::Null => Vec::new(),
                arg => vec![arg.clone()],
            };
            for suggestion in args.iter().flat_map(|arg| arg["suggestions"].as_array().cloned().unwrap_or_default()) {
                words.extend(names(if suggestion.is_string() { &suggestion } else { &suggestion["name"] }));
            }
            words
        };

        let commands = HELP.split_once("\ncommands:\n").unwrap().1.split_once("\n\n").unwrap().0;
        let mut documented = Vec::new();
        for line in commands.lines().filter(|line| line.starts_with("  ") && !line.starts_with("   ")) {
            let mut node = spec.clone();
            // 用法和说明之间隔着好几个空格，说明里的词不算。
            let usage = line.trim_start().split("  ").next().unwrap();
            let path: Vec<&str> = usage
                .split_whitespace()
                .take_while(|word| word.chars().all(|c| c.is_ascii_lowercase() || c == '|'))
                .collect();
            documented.push(path[0].to_owned());
            for word in &path {
                for alternative in word.split('|') {
                    assert!(words(&node).iter().any(|w| w == alternative), "`{line}`: the spec lacks {alternative}");
                }
                if let Some(sub) =
                    subcommands(&node).into_iter().find(|sub| names(&sub["name"]).iter().any(|n| n == word))
                {
                    node = sub;
                }
            }
        }
        for sub in subcommands(&spec).iter().filter(|sub| sub["hidden"] != true) {
            for name in names(&sub["name"]) {
                assert!(documented.contains(&name), "the help lacks {name}");
            }
        }
    }
}
