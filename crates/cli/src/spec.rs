//! 命令行的命令、子命令和选项值的写法：解析（`args`）、补全（`complete`）和报错的文字都从这里取。帮助页
//! 另外手写在 `help`。

use runode_autostart::Kind;

/// 命令和它们的一句话说明，帮助总览列出的命令和说明要和这里一样。
pub(crate) const COMMANDS: &[(&str, &str)] = &[
    ("list", "List the terminal sessions"),
    ("read", "Print the text on a session's screen"),
    ("send", "Type text and keys into a session"),
    ("wait", "Wait for an agent, a command or some text on the screen"),
    ("open", "Open a terminal in the app"),
    ("kill", "End a session and close its pane"),
    ("focus", "Show a session's pane and bring its window to the front"),
    ("move", "Move a session's pane to another place in its window"),
    ("swap", "Swap the panes of two sessions"),
    ("todo", "Put tasks for agents on the task board and start them"),
    ("schedule", "Start a task for an agent at set times"),
    ("remote", "Pair phones and computers for remote access and manage them"),
    ("service", "Start runode at login, and upgrade the running host"),
    ("completions", "Print a completion script for your shell"),
    ("help", "Show this help, or the help of a command"),
    ("version", "Show the version"),
];

/// 各条命令的子命令。
pub(crate) fn subcommands(command: &str) -> &'static [(&'static str, &'static str)] {
    match command {
        "todo" => &[
            ("add", "Add a task"),
            ("list", "List the tasks"),
            ("show", "Print everything about one task"),
            ("start", "Start a task"),
            ("done", "Mark a task done"),
            ("rm", "Remove a task"),
        ],
        "schedule" => &[("add", "Add a schedule"), ("list", "List the schedules"), ("rm", "Remove a schedule")],
        "remote" => &[("pair", "Pair a phone"), ("devices", "List the paired phones"), ("revoke", "Unpair a phone")],
        "service" => &[
            ("install", "Install the login service"),
            ("uninstall", "Remove the login service"),
            ("status", "Show which login services are installed"),
            ("upgrade", "Hand the running host's sessions to a host of this build"),
        ],
        "completions" => &[("bash", ""), ("fish", ""), ("zsh", "")],
        _ => &[],
    }
}

/// `service install`、`service uninstall` 能拉起的东西，第一个是不写时的。
pub(crate) fn service_kinds() -> Vec<(&'static str, &'static str)> {
    Kind::ALL
        .into_iter()
        .map(|kind| {
            let doc = match kind {
                Kind::Host => "The terminal host without a window (default)",
                Kind::App => "The Runode app (macOS only)",
            };
            (kind.name(), doc)
        })
        .collect()
}

/// `--for` 能等的东西。
pub(crate) const UNTIL: &[(&str, &str)] = &[
    ("stopped", "the agent is not working: idle, asking you, or no agent"),
    ("done", "the agent worked, then stopped"),
    ("working", "the agent is working"),
    ("idle", "the agent is idle"),
    ("blocked", "the agent is asking you"),
    ("command", "the next command at the shell prompt finished"),
    ("text", "a line on the screen matches a regex"),
    ("quiet", "the screen did not change for some seconds"),
];

/// `--for` 后面这种写法还要再跟一个值（`text REGEX`、`quiet SECS`）。
pub(crate) fn until_takes_value(kind: &str) -> bool {
    matches!(kind, "text" | "quiet")
}

/// `--command` 后面的这个词是它可以不给的次数：会话标识也可能全是数字，所以只把短的数字当次数，长的留给会话。
pub(crate) fn is_command_count(word: &str) -> bool {
    word.len() <= 4 && word.bytes().all(|b| b.is_ascii_digit())
}

/// `names` 连成「a, b and c」（`last` 是 and）或「a, b or c」（`last` 是 or）。
pub(crate) fn list<'a>(names: impl IntoIterator<Item = &'a str>, last: &str) -> String {
    let names: Vec<_> = names.into_iter().collect();
    match names.split_last() {
        Some((tail, [])) => (*tail).to_owned(),
        Some((tail, rest)) => format!("{} {last} {tail}", rest.join(", ")),
        None => String::new(),
    }
}

/// `command` 的子命令连成「a, b and c」或「a, b or c」。
pub(crate) fn subcommand_list(command: &str, last: &str) -> String {
    list(subcommands(command).iter().map(|&(name, _)| name), last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::parse;

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn every_until_parses_with_its_value() {
        for &(kind, _) in UNTIL {
            let value = match kind {
                "text" => " x",
                "quiet" => " 2",
                _ => "",
            };
            assert_eq!(until_takes_value(kind), !value.is_empty(), "{kind}");
            let line = format!("wait a --for {kind}{value}");
            assert!(parse(&words(&line)).is_ok(), "{line}: {:?}", parse(&words(&line)));
        }
    }

    #[test]
    fn every_subcommand_is_known_to_the_parser() {
        for &(command, _) in COMMANDS {
            for &(sub, _) in subcommands(command) {
                let line = format!("{command} {sub}");
                if let Err(err) = parse(&words(&line)) {
                    assert!(!err.contains(" knows "), "{line}: {err}");
                }
            }
        }
        assert_eq!(parse(&words("todo frob")).unwrap_err(), "todo knows add, list, show, start, done and rm, not frob");
    }

    #[test]
    fn service_kinds_follow_the_autostart_kinds() {
        let names: Vec<_> = service_kinds().into_iter().map(|(name, _)| name).collect();
        assert_eq!(names, ["host", "app"]);
        assert_eq!(parse(&words("service install x")).unwrap_err(), "service install knows host and app, not x");
    }

    #[test]
    fn short_numbers_after_command_are_counts() {
        assert!(is_command_count("3") && !is_command_count("12345") && !is_command_count("a1"));
    }
}
