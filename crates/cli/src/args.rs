//! 解析命令行参数。选项可以写在位置参数前后，`--` 之后的都当位置参数。

use std::{path::PathBuf, time::Duration};

use runode_protocol::Placement;

/// 用法说明，`runode help` 打印它。
pub(crate) const HELP: &str = "\
usage: runode [COMMAND]

Without a command, runode opens its window. Commands talk to the running runode
app; inside a runode terminal they find it through RUNODE_SOCKET, and
RUNODE_SESSION names the terminal they run in.

A SESSION is a session id or any unique prefix of it, as `runode list` shows.

commands:
  list [--json]               list the terminal sessions; * marks your own
  read [SESSION] [--lines N]  print the text on the screen; N lines from the
                              bottom, scrollback included; SESSION defaults to
                              your own
  send SESSION [TEXT...] [--enter] [--wait] [--timeout SECS]
                              type TEXT (words joined by spaces; - reads stdin)
                              into the session; --enter presses Enter after it;
                              --wait then waits like `wait --for done`
  wait SESSION [--for STATE] [--timeout SECS]
                              wait for the session's agent. STATE is one of
                                stopped  not working: idle, asking you, or no
                                         agent (default)
                                done     working, then stopped
                                working, idle, blocked
                              prints the agent's state when it gets there
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
  help                        show this help
  version                     show the version

exit status: 0 done, 1 failed, 2 bad arguments, 3 the session exited,
124 timed out.
";

/// 一条命令。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Command {
    Help,
    Version,
    List { json: bool },
    Read { session: Option<String>, lines: Option<u32> },
    Send { session: String, text: Text, enter: bool, wait: bool, timeout: Option<Duration> },
    Wait { session: String, until: Until, timeout: Option<Duration> },
    Open { placement: Placement, near: Option<String>, cwd: Option<PathBuf>, focus: bool, command: String },
    Kill { session: String },
    Focus { session: Option<String> },
}

/// `send` 要打的字。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Text {
    /// 参数里给的，可能为空（只按回车）。
    Given(String),
    /// 从标准输入读。
    Stdin,
}

/// `wait` 等到什么时候。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Until {
    /// 不在干活：空闲、等回答，或者前台没有 agent。
    Stopped,
    /// 干过活又停下了。
    Done,
    Working,
    Idle,
    Blocked,
}

/// 解析参数，出错时返回说明。
pub(crate) fn parse(args: &[String]) -> Result<Command, String> {
    let mut words = Vec::new();
    let mut flags = Flags::default();
    let mut rest = args.iter();
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
            "--lines" => flags.lines = Some(value(&mut rest, arg)?.parse().map_err(|_| "--lines takes a count")?),
            "--timeout" => flags.timeout = Some(seconds(value(&mut rest, arg)?)?),
            "--for" => flags.until = Some(until(value(&mut rest, arg)?)?),
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
            "--near" => flags.near = Some(value(&mut rest, arg)?.to_owned()),
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
    let command = match name.as_str() {
        "help" => Command::Help,
        "version" => Command::Version,
        "list" => {
            no_more(words, 0, name)?;
            Command::List { json: std::mem::take(&mut flags.json) }
        }
        "read" => {
            no_more(words, 1, name)?;
            Command::Read { session: words.first().cloned(), lines: flags.lines.take() }
        }
        "send" => {
            let (session, text) = words.split_first().ok_or("send needs a SESSION")?;
            let text = match text {
                [dash] if dash == "-" => Text::Stdin,
                words => Text::Given(words.join(" ")),
            };
            if text == Text::Given(String::new()) && !flags.enter {
                return Err("send needs TEXT or --enter".into());
            }
            Command::Send {
                session: session.clone(),
                text,
                enter: std::mem::take(&mut flags.enter),
                wait: std::mem::take(&mut flags.wait),
                timeout: flags.timeout.take(),
            }
        }
        "wait" => {
            no_more(words, 1, name)?;
            let session = words.first().ok_or("wait needs a SESSION")?.clone();
            Command::Wait {
                session,
                until: flags.until.take().unwrap_or(Until::Stopped),
                timeout: flags.timeout.take(),
            }
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
            Command::Kill { session: words.first().ok_or("kill needs a SESSION")?.clone() }
        }
        "focus" => {
            no_more(words, 1, name)?;
            Command::Focus { session: words.first().cloned() }
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
    lines: Option<u32>,
    timeout: Option<Duration>,
    until: Option<Until>,
    placement: Option<Placement>,
    near: Option<String>,
    cwd: Option<PathBuf>,
    focus: bool,
}

impl Flags {
    fn unused(&self) -> Option<&'static str> {
        [
            (self.json, "--json"),
            (self.enter, "--enter"),
            (self.wait, "--wait"),
            (self.lines.is_some(), "--lines"),
            (self.timeout.is_some(), "--timeout"),
            (self.until.is_some(), "--for"),
            (self.placement.is_some(), "--tab, --right or --down"),
            (self.near.is_some(), "--near"),
            (self.cwd.is_some(), "--cwd"),
            (self.focus, "--focus"),
        ]
        .into_iter()
        .find_map(|(set, flag)| set.then_some(flag))
    }
}

fn value<'a>(rest: &mut impl Iterator<Item = &'a String>, option: &str) -> Result<&'a str, String> {
    rest.next().map(String::as_str).ok_or_else(|| format!("{option} needs a value"))
}

fn seconds(value: &str) -> Result<Duration, String> {
    value
        .parse::<f64>()
        .ok()
        .and_then(|secs| Duration::try_from_secs_f64(secs).ok())
        .ok_or_else(|| format!("--timeout takes seconds, not {value}"))
}

fn until(value: &str) -> Result<Until, String> {
    Ok(match value {
        "stopped" => Until::Stopped,
        "done" => Until::Done,
        "working" => Until::Working,
        "idle" => Until::Idle,
        "blocked" => Until::Blocked,
        other => return Err(format!("--for takes stopped, done, working, idle or blocked, not {other}")),
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

    #[test]
    fn commands_and_their_options() {
        assert_eq!(parse(""), Ok(Command::Help));
        assert_eq!(parse("list --json"), Ok(Command::List { json: true }));
        assert_eq!(parse("--lines 5 read ab12"), Ok(Command::Read { session: Some("ab12".into()), lines: Some(5) }));
        assert_eq!(
            parse("send ab12 hello  world --enter --wait --timeout 1.5"),
            Ok(Command::Send {
                session: "ab12".into(),
                text: Text::Given("hello world".into()),
                enter: true,
                wait: true,
                timeout: Some(Duration::from_millis(1500)),
            })
        );
        assert_eq!(
            parse("send ab12 -"),
            Ok(Command::Send { session: "ab12".into(), text: Text::Stdin, enter: false, wait: false, timeout: None })
        );
        assert_eq!(
            parse("send ab12 -- --enter"),
            Ok(Command::Send {
                session: "ab12".into(),
                text: Text::Given("--enter".into()),
                enter: false,
                wait: false,
                timeout: None,
            })
        );
        assert_eq!(
            parse("wait ab12 --for done"),
            Ok(Command::Wait { session: "ab12".into(), until: Until::Done, timeout: None })
        );
        assert_eq!(
            parse("wait ab12"),
            Ok(Command::Wait { session: "ab12".into(), until: Until::Stopped, timeout: None })
        );
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
                near: Some("ab12".into()),
                cwd: Some("/tmp".into()),
                focus: true,
                command: "claude --model opus".into(),
            })
        );
        assert_eq!(parse("kill ab12"), Ok(Command::Kill { session: "ab12".into() }));
        assert_eq!(parse("focus"), Ok(Command::Focus { session: None }));
        assert!(parse("open --right --down").unwrap_err().contains("only one"));
        assert!(parse("kill").unwrap_err().contains("SESSION"));
        assert!(parse("focus --right").unwrap_err().contains("does not take"));
    }

    #[test]
    fn mistakes_are_reported() {
        assert!(parse("frobnicate").unwrap_err().contains("unknown command"));
        assert!(parse("list --bogus").unwrap_err().contains("unknown option"));
        assert!(parse("list --enter").unwrap_err().contains("does not take --enter"));
        assert!(parse("read a b").unwrap_err().contains("does not take b"));
        assert!(parse("send").unwrap_err().contains("SESSION"));
        assert!(parse("send ab12").unwrap_err().contains("TEXT"));
        assert!(parse("wait ab12 --for sleeping").unwrap_err().contains("--for"));
        assert!(parse("wait ab12 --timeout soon").unwrap_err().contains("--timeout"));
        assert!(parse("read --lines").unwrap_err().contains("needs a value"));
    }
}
