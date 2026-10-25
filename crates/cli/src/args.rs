//! 解析命令行参数。选项可以写在位置参数前后，`--` 之后的都当位置参数。

use std::{net::IpAddr, path::PathBuf, time::Duration};

use runode_autostart::Kind;
use runode_protocol::{Placement, todo::When};
use runode_types::input::parse_key_list;

use crate::{help, local_time::LocalTime, select::Selector};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Command {
    /// 打印说明：没给话题是总览，给了是 `help::PAGES` 里那一页。
    Help(Option<&'static str>),
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
        /// 打完字以后依次按的键，每项是 `runode_types::input::parse_keys` 认的写法。
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
        fold: Option<bool>,
        command: String,
    },
    Kill {
        session: Selector,
    },
    Focus {
        session: Option<Selector>,
    },
    Move {
        session: Option<Selector>,
        placement: Placement,
        near: Option<Selector>,
        focus: bool,
    },
    Swap {
        a: Selector,
        b: Selector,
        focus: bool,
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
    /// 装上登录时自启。
    ServiceInstall {
        kind: Kind,
    },
    ServiceUninstall {
        kind: Kind,
    },
    ServiceStatus,
    /// 往看板上加一张任务；`cwd` 没给时是当前目录，`agent` 没给时是 `runode_protocol::todo::DEFAULT_AGENT`。
    TodoAdd {
        title: String,
        prompt: String,
        cwd: Option<PathBuf>,
        agent: Option<String>,
        /// 要新开的工作树。
        worktree: bool,
        start: bool,
    },
    TodoList {
        json: bool,
    },
    /// 打印一张任务的全部内容；`id` 是标识或编号（`5`、`#5`）。
    TodoShow {
        id: String,
        json: bool,
    },
    TodoDone {
        id: String,
    },
    TodoStart {
        id: String,
    },
    TodoRemove {
        id: String,
    },
    /// 加一个定时；同 `TodoAdd`，只是工作树默认不开（`worktree` 是用户要了）。
    ScheduleAdd {
        title: String,
        prompt: String,
        cwd: Option<PathBuf>,
        agent: Option<String>,
        worktree: bool,
        timing: Timing,
    },
    ScheduleList {
        json: bool,
    },
    ScheduleRemove {
        id: String,
    },
    /// 打印给这种 shell 装的补全脚本。
    Completions {
        shell: Shell,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shell {
    Bash,
    Fish,
    Zsh,
}

/// `schedule add` 什么时候到点。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Timing {
    /// `--at`：本地的钟点。
    At(LocalTime),
    /// `--daily`、`--every`。
    Repeat(When),
}

/// `send` 要打的字。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Text {
    /// 参数里给的，可能为空（只按键或回车）。
    Given(String),
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
    // `--` 之后的第一个词在 `words` 里的位置：从这里起都是原样的字。
    let mut literal_from = None;
    let mut flags = Flags::default();
    let mut rest = args.iter().peekable();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--" => {
                literal_from.get_or_insert(words.len());
                words.extend(rest.by_ref().cloned());
            }
            "-h" | "--help" => flags.help = true,
            "-V" | "--version" => flags.version = true,
            "--json" => flags.json = true,
            "--enter" => flags.enter = true,
            "--wait" => flags.wait = true,
            "--paste" => flags.paste = true,
            "--new" => flags.new = true,
            "--key" => flags.keys.push(value(&mut rest, arg)?.to_owned()),
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
            "--prompt" => flags.prompt = Some(value(&mut rest, arg)?.to_owned()),
            "--agent" => flags.agent = Some(value(&mut rest, arg)?.to_owned()),
            "--no-worktree" => flags.no_worktree = true,
            "--start" => flags.start = true,
            "--worktree" => flags.worktree = true,
            "--at" => {
                let at = value(&mut rest, arg)?;
                flags.at =
                    Some(LocalTime::parse(at).ok_or_else(|| format!("--at takes \"YYYY-MM-DD HH:MM\", not {at}"))?);
            }
            "--daily" => {
                let at = value(&mut rest, arg)?;
                flags.daily =
                    Some(crate::local_time::clock(at).ok_or_else(|| format!("--daily takes HH:MM, not {at}"))?);
            }
            "--days" => flags.days = Some(days(value(&mut rest, arg)?)?),
            "--every" => {
                let minutes = value(&mut rest, arg)?;
                flags.every = Some(
                    minutes
                        .parse()
                        .ok()
                        .filter(|&n: &u32| n > 0)
                        .ok_or_else(|| format!("--every takes minutes, not {minutes}"))?,
                );
            }
            "--fold" | "--no-fold" => {
                let fold = arg == "--fold";
                if flags.fold.replace(fold).is_some_and(|before| before != fold) {
                    return Err("give only one of --fold and --no-fold".into());
                }
            }
            // 单独一个 `-` 是位置参数（`send` 从标准输入读）。
            option if option.starts_with('-') && option != "-" => return Err(format!("unknown option {option}")),
            _ => words.push(arg.clone()),
        }
    }
    // 逐项和合起来的总数（宿主同样用 `parse_key_list` 查）都在这里查：宿主拒绝按键时字已经打出去了。
    parse_key_list(&flags.keys).map_err(|err| format!("--key: {err}"))?;
    let topic = |word: Option<&String>| {
        word.map(|word| help::topic(word).ok_or_else(|| format!("no help for {word}"))).transpose()
    };
    if flags.help {
        return Ok(Command::Help(topic(words.first())?));
    }
    if flags.version {
        return Ok(Command::Version);
    }
    // tmux send-keys 的写法：`send` 最后单独一个 Enter 是按回车，agent 常这么写；放在 `--` 后面的才当字打。
    let enter_word =
        words.last().is_some_and(|word| word == "Enter") && literal_from.is_none_or(|from| from >= words.len());
    let Some((name, words)) = words.split_first() else {
        return Ok(Command::Help(None));
    };
    let session = |word: Option<&String>| word.map(|word| Selector::parse(word)).transpose();
    let command = match name.as_str() {
        "help" => {
            no_more(words, 1, name)?;
            Command::Help(topic(words.first())?)
        }
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
            let (target, mut text) = words.split_first().ok_or("send needs a SESSION")?;
            if enter_word && let Some((_, typed)) = text.split_last() {
                text = typed;
                flags.enter = true;
            }
            let text = match text {
                // `--` 后面的 `-` 是要打的字，不是读 stdin。
                [dash] if dash == "-" && literal_from.is_none_or(|from| from > 2) => Text::Stdin,
                words => Text::Given(words.join(" ")),
            };
            if text == Text::Given(String::new()) && flags.keys.is_empty() && !flags.enter {
                return Err("send needs TEXT, --key or --enter".into());
            }
            // 不等就没有超时可言；不报错的话这个选项被悄悄丢掉，用户以为它起了作用。
            if flags.timeout.is_some() && !flags.wait {
                return Err("--timeout needs --wait".into());
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
        "open" => {
            let placement = flags.placement.take().unwrap_or(Placement::Tab);
            let fold = flags.fold.take();
            // `--no-fold` 在新标签里无事可做，放行，免得脚本白白失败。
            if fold == Some(true) && placement == Placement::Tab {
                return Err("--fold needs --right or --down".into());
            }
            if fold == Some(true) && flags.focus {
                return Err("--fold and --focus cannot be used together".into());
            }
            Command::Open {
                placement,
                near: flags.near.take(),
                cwd: flags.cwd.take(),
                focus: std::mem::take(&mut flags.focus),
                fold,
                command: words.join(" "),
            }
        }
        "kill" => {
            no_more(words, 1, name)?;
            Command::Kill { session: Selector::parse(words.first().ok_or("kill needs a SESSION")?)? }
        }
        "focus" => {
            no_more(words, 1, name)?;
            Command::Focus { session: session(words.first())? }
        }
        "move" => {
            no_more(words, 1, name)?;
            Command::Move {
                session: session(words.first())?,
                placement: flags.placement.take().unwrap_or(Placement::Tab),
                near: flags.near.take(),
                focus: std::mem::take(&mut flags.focus),
            }
        }
        "swap" => {
            no_more(words, 2, name)?;
            let [a, b] = words else { return Err("swap needs two SESSIONs".into()) };
            Command::Swap { a: Selector::parse(a)?, b: Selector::parse(b)?, focus: std::mem::take(&mut flags.focus) }
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
                    // 空的前缀对得上每一台设备：`revoke "$DEVICE"` 里变量没设时不能撤销掉唯一的那台。
                    let device = words.first().filter(|device| !device.is_empty());
                    Command::RemoteRevoke { device: device.ok_or("remote revoke needs a DEVICE")?.clone() }
                }
                other => return Err(format!("remote knows pair, devices and revoke, not {other}")),
            }
        }
        "service" => {
            let (what, words) = words.split_first().ok_or("service needs install, uninstall or status")?;
            // 不写拉起什么时是宿主。
            let kind = |words: &[String], name: &str| -> Result<Kind, String> {
                no_more(words, 1, name)?;
                match words.first().map(String::as_str) {
                    None | Some("host") => Ok(Kind::Host),
                    Some("app") => Ok(Kind::App),
                    Some(other) => Err(format!("{name} knows host and app, not {other}")),
                }
            };
            match what.as_str() {
                "install" => Command::ServiceInstall { kind: kind(words, "service install")? },
                "uninstall" => Command::ServiceUninstall { kind: kind(words, "service uninstall")? },
                "status" => {
                    no_more(words, 0, "service status")?;
                    Command::ServiceStatus
                }
                other => return Err(format!("service knows install, uninstall and status, not {other}")),
            }
        }
        "todo" => {
            let (what, words) = words.split_first().ok_or("todo needs add, list, show, start, done or rm")?;
            match what.as_str() {
                "add" => {
                    if words.is_empty() {
                        return Err("todo add needs a TITLE".into());
                    }
                    Command::TodoAdd {
                        title: words.join(" "),
                        prompt: flags.prompt.take().unwrap_or_default(),
                        cwd: flags.cwd.take(),
                        agent: flags.agent.take(),
                        worktree: !std::mem::take(&mut flags.no_worktree),
                        start: std::mem::take(&mut flags.start),
                    }
                }
                "list" => {
                    no_more(words, 0, "todo list")?;
                    Command::TodoList { json: std::mem::take(&mut flags.json) }
                }
                "show" => {
                    no_more(words, 1, "todo show")?;
                    let id = words.first().ok_or("todo show needs an ID")?.clone();
                    Command::TodoShow { id, json: std::mem::take(&mut flags.json) }
                }
                "done" | "start" | "rm" => {
                    let name = format!("todo {what}");
                    no_more(words, 1, &name)?;
                    let id = words.first().ok_or_else(|| format!("{name} needs an ID"))?.clone();
                    match what.as_str() {
                        "done" => Command::TodoDone { id },
                        "start" => Command::TodoStart { id },
                        _ => Command::TodoRemove { id },
                    }
                }
                other => return Err(format!("todo knows add, list, show, start, done and rm, not {other}")),
            }
        }
        "schedule" => {
            let (what, words) = words.split_first().ok_or("schedule needs add, list or rm")?;
            match what.as_str() {
                "add" => {
                    if words.is_empty() {
                        return Err("schedule add needs a TITLE".into());
                    }
                    if flags.days.is_some() && flags.daily.is_none() {
                        return Err("--days needs --daily".into());
                    }
                    let timing = match (flags.at.take(), flags.daily.take(), flags.every.take()) {
                        (Some(at), None, None) => Timing::At(at),
                        (None, Some((hour, minute)), None) => {
                            Timing::Repeat(When::Daily { hour, minute, days: flags.days.take().unwrap_or(0) })
                        }
                        (None, None, Some(minutes)) => Timing::Repeat(When::Every { minutes }),
                        _ => return Err("schedule add needs one of --at, --daily and --every".into()),
                    };
                    Command::ScheduleAdd {
                        title: words.join(" "),
                        prompt: flags.prompt.take().unwrap_or_default(),
                        cwd: flags.cwd.take(),
                        agent: flags.agent.take(),
                        worktree: std::mem::take(&mut flags.worktree),
                        timing,
                    }
                }
                "list" => {
                    no_more(words, 0, "schedule list")?;
                    Command::ScheduleList { json: std::mem::take(&mut flags.json) }
                }
                "rm" => {
                    no_more(words, 1, "schedule rm")?;
                    Command::ScheduleRemove { id: words.first().ok_or("schedule rm needs an ID")?.clone() }
                }
                other => return Err(format!("schedule knows add, list and rm, not {other}")),
            }
        }
        "completions" => {
            no_more(words, 1, name)?;
            let shell = match words.first().map(String::as_str) {
                Some("bash") => Shell::Bash,
                Some("fish") => Shell::Fish,
                Some("zsh") => Shell::Zsh,
                Some(other) => return Err(format!("completions knows bash, fish and zsh, not {other}")),
                None => return Err("completions needs a shell: bash, fish or zsh".into()),
            };
            Command::Completions { shell }
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
    keys: Vec<String>,
    lines: Option<u32>,
    command: Option<u32>,
    timeout: Option<Duration>,
    until: Option<Until>,
    placement: Option<Placement>,
    near: Option<Selector>,
    cwd: Option<PathBuf>,
    focus: bool,
    fold: Option<bool>,
    addrs: Vec<IpAddr>,
    prompt: Option<String>,
    agent: Option<String>,
    no_worktree: bool,
    start: bool,
    worktree: bool,
    at: Option<LocalTime>,
    daily: Option<(u8, u8)>,
    days: Option<u8>,
    every: Option<u32>,
}

impl Flags {
    fn unused(&self) -> Option<&'static str> {
        [
            (self.json, "--json"),
            (self.enter, "--enter"),
            (self.wait, "--wait"),
            (self.paste, "--paste"),
            (self.new, "--new"),
            (!self.keys.is_empty(), "--key"),
            (self.lines.is_some(), "--lines"),
            (self.command.is_some(), "--command"),
            (self.timeout.is_some(), "--timeout"),
            (self.until.is_some(), "--for"),
            (self.placement.is_some(), "--tab, --right or --down"),
            (self.near.is_some(), "--near"),
            (self.cwd.is_some(), "--cwd"),
            (self.focus, "--focus"),
            (self.fold.is_some(), "--fold or --no-fold"),
            (!self.addrs.is_empty(), "--addr"),
            (self.prompt.is_some(), "--prompt"),
            (self.agent.is_some(), "--agent"),
            (self.no_worktree, "--no-worktree"),
            (self.start, "--start"),
            (self.worktree, "--worktree"),
            (self.at.is_some(), "--at"),
            (self.daily.is_some(), "--daily"),
            (self.days.is_some(), "--days"),
            (self.every.is_some(), "--every"),
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

/// 一周里的哪几天，`When::Daily` 的 `days`：`mon,wed,fri` 或者 `mon-fri` 这样的范围，第 0 位是星期日。
fn days(text: &str) -> Result<u8, String> {
    let day = |name: &str| {
        DAYS.iter()
            .position(|&day| day == name.trim().to_ascii_lowercase())
            .ok_or_else(|| format!("--days takes days like mon,wed or mon-fri, not {text}"))
    };
    let mut bits = 0u8;
    for part in text.split(',') {
        let (from, to) = match part.split_once('-') {
            Some((from, to)) => (day(from)?, day(to)?),
            None => (day(part)?, day(part)?),
        };
        // `fri-mon` 跨过周末，绕回星期日。
        let mut at = from;
        loop {
            bits |= 1 << at;
            if at == to {
                break;
            }
            at = (at + 1) % 7;
        }
    }
    Ok(bits)
}

/// 星期几的写法，从星期日起，同 `When::Daily` 的位。
pub(crate) const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

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
        assert_eq!(parse(""), Ok(Command::Help(None)));
        assert_eq!(parse("help"), Ok(Command::Help(None)));
        assert_eq!(parse("help wait"), Ok(Command::Help(Some("wait"))));
        assert_eq!(parse("send ab12 --help"), Ok(Command::Help(Some("send"))));
        assert_eq!(parse("-h remote pair"), Ok(Command::Help(Some("remote"))));
        assert!(parse("help frob").unwrap_err().contains("no help for frob"));
        assert!(parse("help wait send").unwrap_err().contains("does not take send"));
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
        assert_eq!(parse("send ab12 -- -"), Ok(send("ab12", "-")));
        assert_eq!(parse("send ab12 -- --enter"), Ok(send("ab12", "--enter")));
        // 末尾单独的 Enter 照 tmux 当回车，`--` 后面的才是字。
        let entered = |text: &str| {
            let mut command = send("ab12", text);
            if let Command::Send { enter, .. } = &mut command {
                *enter = true;
            }
            Ok(command)
        };
        assert_eq!(parse("send ab12 make run Enter"), entered("make run"));
        assert_eq!(parse("send ab12 Enter"), entered(""));
        assert!(matches!(parse("send ab12 - Enter"), Ok(Command::Send { text: Text::Stdin, enter: true, .. })));
        assert_eq!(parse("send ab12 -- press Enter"), Ok(send("ab12", "press Enter")));
        assert_eq!(parse("send ab12 press -- Enter"), Ok(send("ab12", "press Enter")));
        assert_eq!(parse("send ab12 Enter now"), Ok(send("ab12", "Enter now")));
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
        // 每项都合规、合起来超过 `MAX_KEYS` 下的也在发之前就报。
        let many = format!("send ab12 text{}", " --key a*1000".repeat(11));
        assert!(parse(&many).unwrap_err().contains("at most"));
        assert!(parse(&format!("send ab12 text{}", " --key a*1000".repeat(10))).is_ok());
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
    fn window_commands() {
        assert_eq!(
            parse("open"),
            Ok(Command::Open {
                placement: Placement::Tab,
                near: None,
                cwd: None,
                focus: false,
                fold: None,
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
                fold: None,
                command: "claude --model opus".into(),
            })
        );
        assert_eq!(
            parse("open --down --fold"),
            Ok(Command::Open {
                placement: Placement::Down,
                near: None,
                cwd: None,
                focus: false,
                fold: Some(true),
                command: String::new()
            })
        );
        assert!(matches!(parse("open --down --no-fold"), Ok(Command::Open { fold: Some(false), .. })));
        assert!(parse("open --down --fold --no-fold").unwrap_err().contains("only one"));
        assert!(parse("open --fold").unwrap_err().contains("--right or --down"));
        assert!(matches!(parse("open --no-fold"), Ok(Command::Open { fold: Some(false), .. })));
        assert!(parse("open --right --fold --focus").unwrap_err().contains("cannot be used together"));
        assert!(parse("kill ab12 --fold").unwrap_err().contains("--fold"));
        assert_eq!(parse("kill ab12"), Ok(Command::Kill { session: id("ab12") }));
        assert_eq!(parse("focus"), Ok(Command::Focus { session: None }));
        assert!(parse("open --right --down").unwrap_err().contains("only one"));
        assert!(parse("kill").unwrap_err().contains("SESSION"));
        assert!(parse("focus --right").unwrap_err().contains("does not take"));
        assert_eq!(
            parse("move"),
            Ok(Command::Move { session: None, placement: Placement::Tab, near: None, focus: false })
        );
        assert_eq!(
            parse("move ab12 --down --near right --focus"),
            Ok(Command::Move {
                session: Some(id("ab12")),
                placement: Placement::Down,
                near: Some(Selector::Beside(runode_types::pane::Direction::Right)),
                focus: true,
            })
        );
        assert!(parse("move ab12 --fold").unwrap_err().contains("--fold"));
        assert_eq!(parse("swap self ab12"), Ok(Command::Swap { a: Selector::Own, b: id("ab12"), focus: false }));
        assert!(parse("swap ab12").unwrap_err().contains("two SESSIONs"));
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
        assert_eq!(parse("service install"), Ok(Command::ServiceInstall { kind: Kind::Host }));
        assert_eq!(parse("service install app"), Ok(Command::ServiceInstall { kind: Kind::App }));
        assert_eq!(parse("service uninstall host"), Ok(Command::ServiceUninstall { kind: Kind::Host }));
        assert_eq!(parse("service status"), Ok(Command::ServiceStatus));
        assert!(parse("service install both").unwrap_err().contains("host and app"));
        assert!(parse("service status app").unwrap_err().contains("does not take"));
        assert!(parse("service").is_err());
        assert!(parse("remote").unwrap_err().contains("pair, devices or revoke"));
        assert!(parse("remote frob").unwrap_err().contains("not frob"));
        assert!(parse("remote revoke").unwrap_err().contains("DEVICE"));
        let empty = ["remote", "revoke", ""].map(str::to_owned);
        assert!(super::parse(&empty).unwrap_err().contains("DEVICE"));
        assert!(parse("remote pair --addr nowhere").unwrap_err().contains("IP address"));
        assert!(parse("remote devices --addr 127.0.0.1").unwrap_err().contains("does not take --addr"));
        assert!(parse("remote pair extra").unwrap_err().contains("does not take extra"));
    }

    #[test]
    fn todo_commands() {
        assert_eq!(
            parse("todo add fix the  login bug"),
            Ok(Command::TodoAdd {
                title: "fix the login bug".into(),
                prompt: String::new(),
                cwd: None,
                agent: None,
                worktree: true,
                start: false,
            })
        );
        assert_eq!(
            parse("todo add --cwd app --agent codex --prompt why? --no-worktree --start docs"),
            Ok(Command::TodoAdd {
                title: "docs".into(),
                prompt: "why?".into(),
                cwd: Some("app".into()),
                agent: Some("codex".into()),
                worktree: false,
                start: true,
            })
        );
        assert_eq!(parse("todo list --json"), Ok(Command::TodoList { json: true }));
        assert_eq!(parse("todo show 5"), Ok(Command::TodoShow { id: "5".into(), json: false }));
        assert_eq!(parse("todo show #5 --json"), Ok(Command::TodoShow { id: "#5".into(), json: true }));
        assert!(parse("todo show").unwrap_err().contains("ID"));
        assert_eq!(parse("todo done 0a1b2c3d"), Ok(Command::TodoDone { id: "0a1b2c3d".into() }));
        assert_eq!(parse("todo start 0a1b2c3d"), Ok(Command::TodoStart { id: "0a1b2c3d".into() }));
        assert_eq!(parse("todo rm 0a1b2c3d"), Ok(Command::TodoRemove { id: "0a1b2c3d".into() }));
        assert!(parse("todo start").unwrap_err().contains("ID"));
        assert!(parse("todo").unwrap_err().contains("add, list, show, start, done or rm"));
        assert!(parse("todo add").unwrap_err().contains("TITLE"));
        assert!(parse("todo done").unwrap_err().contains("ID"));
        assert!(parse("todo list --start").unwrap_err().contains("does not take --start"));
        assert!(parse("open --no-worktree").unwrap_err().contains("does not take --no-worktree"));
    }

    #[test]
    fn schedule_commands() {
        let add = |args: &[&str]| super::parse(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>());
        assert_eq!(
            add(&["schedule", "add", "daily", "report", "--at", "2026-10-13 14:00", "--worktree"]),
            Ok(Command::ScheduleAdd {
                title: "daily report".into(),
                prompt: String::new(),
                cwd: None,
                agent: None,
                worktree: true,
                timing: Timing::At(LocalTime { year: 2026, month: 10, day: 13, hour: 14, minute: 0 }),
            })
        );
        let timing = |args: &str| match parse(args) {
            Ok(Command::ScheduleAdd { timing, worktree: false, .. }) => Ok(timing),
            other => Err(format!("{other:?}")),
        };
        let daily = |days| Ok(Timing::Repeat(When::Daily { hour: 9, minute: 30, days }));
        assert_eq!(timing("schedule add r --daily 09:30"), daily(0));
        assert_eq!(timing("schedule add r --daily 9:30 --days mon-fri"), daily(0b011_1110));
        assert_eq!(timing("schedule add r --daily 09:30 --days sat,sun"), daily(0b100_0001));
        assert_eq!(timing("schedule add r --daily 09:30 --days fri-mon"), daily(0b110_0011));
        assert_eq!(timing("schedule add r --every 15"), Ok(Timing::Repeat(When::Every { minutes: 15 })));
        assert!(parse("schedule add r").unwrap_err().contains("one of --at, --daily and --every"));
        assert!(parse("schedule add r --every 5 --daily 09:00").unwrap_err().contains("one of"));
        assert!(parse("schedule add r --every 0").unwrap_err().contains("minutes"));
        assert!(parse("schedule add r --daily 25:00").unwrap_err().contains("HH:MM"));
        assert!(parse("schedule add r --at 2026-10-13").unwrap_err().contains("YYYY-MM-DD HH:MM"));
        assert!(parse("schedule add r --every 5 --days mon").unwrap_err().contains("--days needs --daily"));
        assert!(parse("schedule add r --daily 09:00 --days funday").unwrap_err().contains("--days"));
        assert!(parse("schedule add r --every 5 --no-worktree").unwrap_err().contains("does not take --no-worktree"));
        assert!(parse("todo add r --worktree").unwrap_err().contains("does not take --worktree"));
        assert_eq!(parse("schedule list --json"), Ok(Command::ScheduleList { json: true }));
        assert_eq!(parse("schedule rm 0a1b2c3d"), Ok(Command::ScheduleRemove { id: "0a1b2c3d".into() }));
        assert!(parse("schedule").unwrap_err().contains("add, list or rm"));
    }

    #[test]
    fn mistakes_are_reported() {
        assert!(parse("frobnicate").unwrap_err().contains("unknown command"));
        assert_eq!(parse("completions fish"), Ok(Command::Completions { shell: Shell::Fish }));
        assert!(parse("completions").unwrap_err().contains("needs a shell"));
        assert!(parse("completions ksh").unwrap_err().contains("not ksh"));
        assert!(parse("list --bogus").unwrap_err().contains("unknown option"));
        assert!(parse("list --enter").unwrap_err().contains("does not take --enter"));
        assert!(parse("read a b").unwrap_err().contains("does not take b"));
        assert!(parse("send").unwrap_err().contains("SESSION"));
        assert!(parse("send ab12 hi --timeout 5").unwrap_err().contains("--timeout needs --wait"));
        assert!(parse("wait ab12 --for sleeping").unwrap_err().contains("--for"));
        assert!(parse("wait ab12 --timeout soon").unwrap_err().contains("--timeout"));
        assert!(parse("read --lines").unwrap_err().contains("needs a value"));
        // 认不出的会话写法也算参数错误。
        assert!(parse("read sideways").unwrap_err().contains("sideways"));
        assert!(parse("read tab:x").unwrap_err().contains("tab:x"));
    }
}
