//! Shell 补全。照 nix 的做法：候选由可执行文件自己算，各个 shell 只装一个薄脚本
//! （`runode completions bash|fish|zsh` 打印的那个），按了 Tab 时跑
//! `RUNODE_GET_COMPLETIONS=N runode <命令行上的词>`，把打印出来的候选交给 shell。这样命令一改
//! 补全跟着改，会话、任务和设备也是按了 Tab 时现查的。
//!
//! 输出的第一行说除了候选 shell 还能补什么：`normal` 什么也不补，`files` 补文件，`dirs` 补目录；
//! 后面每行一个候选，候选和说明之间一个制表符。候选已经按正在打的词筛过（bash 不自己筛）。

use std::{
    io::Write,
    time::{Duration, Instant},
};

use runode_protocol::{ClientMsg, HostMsg, SessionInfo};
use runode_types::todo::{Schedule, Todo};

use crate::{
    Env,
    args::Shell,
    client::Connection,
    commands::{SHORT_ID, plain, state_name, title},
    help,
};

/// 给 `shell` 装的补全脚本：`RUNODE_GET_COMPLETIONS` 问候选，`runode` 和 `rn` 都补。
pub(crate) fn script(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => include_str!("../completions/runode.bash"),
        Shell::Fish => include_str!("../completions/runode.fish"),
        Shell::Zsh => include_str!("../completions/runode.zsh"),
    }
}

/// 问宿主要会话和任务最多等这么久（连上、握手和回话一起算）：补全卡住比少几个候选糟。
const HOST_TIMEOUT: Duration = Duration::from_secs(2);

/// 除了候选，shell 还能补什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Also {
    Nothing,
    Files,
    Dirs,
}

impl Also {
    fn name(self) -> &'static str {
        match self {
            Self::Nothing => "normal",
            Self::Files => "files",
            Self::Dirs => "dirs",
        }
    }
}

/// 一个选项的值是什么。
#[derive(Clone, Copy)]
enum Value {
    /// 不带值。
    None,
    Session,
    Dir,
    Choice(&'static [(&'static str, &'static str)]),
    /// `--key`：键名。
    Key,
    /// `--for`：`text`、`quiet` 后面还跟一个词。
    Until,
    /// 带值，但没有候选（数字、时间、文字）。
    Free,
}

struct Opt {
    flag: &'static str,
    doc: &'static str,
    value: Value,
    /// 能写几次（`--key`、`--addr`）；写过一次的别的选项不再提示。
    repeatable: bool,
}

const fn opt(flag: &'static str, doc: &'static str, value: Value) -> Opt {
    Opt { flag, doc, value, repeatable: false }
}

const HELP: Opt = opt("--help", "Show the help of this command", Value::None);
const JSON: Opt = opt("--json", "Print JSON instead of text", Value::None);
const CWD: Opt = opt("--cwd", "The directory to work in", Value::Dir);
const PROMPT: Opt = opt("--prompt", "The prompt for the agent", Value::Free);
const AGENT: Opt = opt("--agent", "The command the prompt is given to", Value::Choice(AGENTS));
const TIMEOUT: Opt = opt("--timeout", "Stop waiting after SECS", Value::Free);
const LINES: Opt = opt("--lines", "Only the last N lines", Value::Free);

const AGENTS: &[(&str, &str)] = &[("claude", "Claude Code"), ("codex", "Codex")];

const READ_OPTS: &[Opt] =
    &[LINES, opt("--command", "The output of the last (or the Nth last) command", Value::None), HELP];
const SEND_OPTS: &[Opt] = &[
    opt("--paste", "Paste the text instead of typing it", Value::None),
    Opt { flag: "--key", doc: "Press a key after the text", value: Value::Key, repeatable: true },
    opt("--enter", "Press Enter at the end", Value::None),
    opt("--wait", "Wait for the agent or the command afterwards", Value::None),
    TIMEOUT,
    HELP,
];
const WAIT_OPTS: &[Opt] = &[
    opt("--for", "What to wait for", Value::Until),
    LINES,
    opt("--new", "With --for text: skip the lines already there", Value::None),
    TIMEOUT,
    HELP,
];
const OPEN_OPTS: &[Opt] = &[
    opt("--tab", "Open a new tab (default)", Value::None),
    opt("--right", "Split the pane to the right", Value::None),
    opt("--down", "Split the pane downward", Value::None),
    opt("--near", "The session to open next to", Value::Session),
    opt("--cwd", "Start in this directory", Value::Dir),
    opt("--focus", "Switch to the new terminal", Value::None),
    opt("--fold", "Open the split folded to its title bar", Value::None),
    opt("--no-fold", "Keep the split unfolded", Value::None),
    HELP,
];
const MOVE_OPTS: &[Opt] = &[
    opt("--tab", "Move it into a new tab after NEAR's tab (default)", Value::None),
    opt("--right", "Put it to the right of NEAR's pane", Value::None),
    opt("--down", "Put it below NEAR's pane", Value::None),
    opt("--near", "The session to move it next to", Value::Session),
    opt("--focus", "Switch to the moved terminal", Value::None),
    HELP,
];
const SWAP_OPTS: &[Opt] = &[opt("--focus", "Switch to the first session", Value::None), HELP];
const TODO_ADD_OPTS: &[Opt] = &[
    PROMPT,
    CWD,
    AGENT,
    opt("--no-worktree", "Work in the directory itself, not in a new worktree", Value::None),
    opt("--start", "Start the task right away", Value::None),
    HELP,
];
const SCHEDULE_ADD_OPTS: &[Opt] = &[
    opt("--at", "Once, at \"YYYY-MM-DD HH:MM\"", Value::Free),
    opt("--daily", "Every day at HH:MM", Value::Free),
    opt("--days", "With --daily: only on these days", Value::Choice(DAY_CHOICES)),
    opt("--every", "Every MINUTES minutes", Value::Free),
    PROMPT,
    CWD,
    AGENT,
    opt("--worktree", "Give each task a new git worktree and branch", Value::None),
    HELP,
];
const PAIR_OPTS: &[Opt] =
    &[Opt { flag: "--addr", doc: "Also offer this address to the phone", value: Value::Free, repeatable: true }, HELP];

/// 每条命令和它的子命令（`todo add` 这些）的选项；没有子命令的命令 `sub` 是 `None`。
fn options(command: &str, sub: Option<&str>) -> &'static [Opt] {
    match (command, sub) {
        ("list", _) => &[JSON, HELP],
        ("read", _) => READ_OPTS,
        ("send", _) => SEND_OPTS,
        ("wait", _) => WAIT_OPTS,
        ("open", _) => OPEN_OPTS,
        ("move", _) => MOVE_OPTS,
        ("swap", _) => SWAP_OPTS,
        ("todo", Some("add")) => TODO_ADD_OPTS,
        ("todo", Some("list" | "show")) | ("schedule", Some("list")) | ("remote", Some("devices")) => &[JSON, HELP],
        ("schedule", Some("add")) => SCHEDULE_ADD_OPTS,
        ("remote", Some("pair")) => PAIR_OPTS,
        _ => &[HELP],
    }
}

const DAY_CHOICES: &[(&str, &str)] = &[
    ("mon-fri", "weekdays"),
    ("sat,sun", "the weekend"),
    ("sun", "Sunday"),
    ("mon", "Monday"),
    ("tue", "Tuesday"),
    ("wed", "Wednesday"),
    ("thu", "Thursday"),
    ("fri", "Friday"),
    ("sat", "Saturday"),
];

const UNTIL: &[(&str, &str)] = &[
    ("stopped", "the agent is not working: idle, asking you, or no agent"),
    ("done", "the agent worked, then stopped"),
    ("working", "the agent is working"),
    ("idle", "the agent is idle"),
    ("blocked", "the agent is asking you"),
    ("command", "the next command at the shell prompt finished"),
    ("text", "a line on the screen matches a regex"),
    ("quiet", "the screen did not change for some seconds"),
];

/// `--key` 提示的键名，同 `runode_types::input::parse_keys` 认的写法。
const KEYS: &[(&str, &str)] = &[
    ("enter", ""),
    ("esc", ""),
    ("tab", ""),
    ("shift-tab", ""),
    ("ctrl-c", ""),
    ("ctrl-d", ""),
    ("ctrl-u", ""),
    ("ctrl-l", ""),
    ("ctrl-z", ""),
    ("up", ""),
    ("down", ""),
    ("left", ""),
    ("right", ""),
    ("pageup", ""),
    ("pagedown", ""),
    ("home", ""),
    ("end", ""),
    ("backspace", ""),
    ("delete", ""),
    ("space", ""),
    ("f1", ""),
    ("f2", ""),
    ("f3", ""),
    ("f4", ""),
    ("f5", ""),
    ("f6", ""),
    ("f7", ""),
    ("f8", ""),
    ("f9", ""),
    ("f10", ""),
    ("f11", ""),
    ("f12", ""),
];

/// 命令和它们的一句话说明，同 `help::OVERVIEW`。
const COMMANDS: &[(&str, &str)] = &[
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
    ("remote", "Pair phones for remote access and manage them"),
    ("service", "Start runode at login, and upgrade the running host"),
    ("completions", "Print a completion script for your shell"),
    ("help", "Show this help, or the help of a command"),
    ("version", "Show the version"),
];

/// 各条命令的子命令。
fn subcommands(command: &str) -> &'static [(&'static str, &'static str)] {
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

const SERVICE_KINDS: &[(&str, &str)] =
    &[("host", "The terminal host without a window (default)"), ("app", "The Runode app (macOS only)")];

/// 在 runode 的终端里才有意义的会话写法。
const RELATIVE_SESSIONS: &[(&str, &str)] = &[
    ("self", "your own terminal"),
    ("left", "the pane to the left of yours"),
    ("right", "the pane to the right of yours"),
    ("up", "the pane above yours"),
    ("down", "the pane below yours"),
    ("next", "the next pane in your tab"),
    ("prev", "the previous pane in your tab"),
];

/// 打印第 `index` 个词（从 0 数，`args` 不含程序名）的候选；`args` 里 `index` 之后的词不看。
pub(crate) fn complete(args: &[String], index: usize, env: &Env, out: &mut dyn Write) -> std::io::Result<()> {
    let previous = &args[..index.min(args.len())];
    let current = args.get(index).map_or("", String::as_str);
    let (also, mut items) = candidates(previous, current, env);
    // 候选一行一个、制表符后面是说明，目录名里带这两个字符的写不进去，丢掉。
    items.retain(|(word, _)| word.starts_with(current) && !word.contains(['\t', '\n']));
    writeln!(out, "{}", also.name())?;
    for (word, doc) in items {
        writeln!(out, "{word}\t{}", plain(&doc))?;
    }
    Ok(())
}

/// 前面的词读到哪了。
struct Line<'a> {
    command: Option<&'a str>,
    /// 命令后面的位置参数，`--` 后面的也算。
    positionals: Vec<&'a str>,
    /// 写过的选项。
    seen: Vec<&'a str>,
    /// 正在打的是这个选项的值。
    value_of: Option<&'static Opt>,
    /// 正在打的词在 `--` 后面。
    literal: bool,
}

fn read(previous: &[String]) -> Line<'_> {
    let mut line = Line { command: None, positionals: Vec::new(), seen: Vec::new(), value_of: None, literal: false };
    let mut i = 0;
    while let Some(word) = previous.get(i) {
        i += 1;
        if line.literal {
            line.positionals.push(word);
            continue;
        }
        if word == "--" {
            line.literal = true;
            continue;
        }
        if !word.starts_with('-') || word == "-" {
            if line.command.is_none() {
                line.command = Some(word);
            } else {
                line.positionals.push(word);
            }
            continue;
        }
        line.seen.push(word);
        let opt = match line.command {
            Some(command) => find_option(command, line.positionals.first().copied(), word),
            // 选项也能写在命令前面（见 `args::parse`），这时还不知道是哪条命令的，只看它带不带值。
            None => any_option(word),
        };
        let Some(opt) = opt else { continue };
        match opt.value {
            Value::None => {
                // `--command [N]` 的次数可以不给，短的数字才算，见 `args::parse`。
                if word == "--command"
                    && previous.get(i).is_some_and(|next| next.len() <= 4 && next.bytes().all(|b| b.is_ascii_digit()))
                {
                    i += 1;
                }
            }
            Value::Until => {
                // `--for text REGEX`、`--for quiet SECS` 各多吃一个词。
                let next = previous.get(i).map(String::as_str);
                i += 1;
                if next.is_some_and(|next| next == "text" || next == "quiet") {
                    i += 1;
                }
            }
            _ => i += 1,
        }
        if i > previous.len() {
            line.value_of = Some(opt);
        }
    }
    line
}

fn find_option(command: &str, sub: Option<&str>, flag: &str) -> Option<&'static Opt> {
    options(command, sub).iter().find(|opt| opt.flag == flag)
}

/// 随便哪条命令（和子命令）的这个选项；同名的选项在各条命令里带的值是同一种。
fn any_option(flag: &str) -> Option<&'static Opt> {
    COMMANDS
        .iter()
        .flat_map(|&(command, _)| {
            let subs = subcommands(command).iter().map(|&(sub, _)| Some(sub));
            std::iter::once(None).chain(subs).map(move |sub| options(command, sub))
        })
        .flatten()
        .find(|opt| opt.flag == flag)
}

/// 候选和 shell 另外能补的东西。
fn candidates(previous: &[String], current: &str, env: &Env) -> (Also, Vec<(String, String)>) {
    let line = read(previous);
    if let Some(opt) = line.value_of
        && !line.literal
    {
        return match opt.value {
            Value::Session => (Also::Nothing, sessions(env)),
            Value::Dir => (Also::Dirs, Vec::new()),
            Value::Choice(choices) => (Also::Nothing, owned(choices)),
            Value::Key => (Also::Nothing, owned(KEYS)),
            Value::Until => (
                Also::Nothing,
                if previous.last().is_some_and(|last| last == "--for") { owned(UNTIL) } else { Vec::new() },
            ),
            Value::None | Value::Free => (Also::Nothing, Vec::new()),
        };
    }
    let Some(command) = line.command else {
        let mut items = owned(COMMANDS);
        if current.starts_with('-') {
            items = vec![("--help".into(), "Show the help".into()), ("--version".into(), "Show the version".into())];
        }
        return (Also::Nothing, items);
    };
    let sub = line.positionals.first().copied();
    if current.starts_with('-') && !line.literal {
        let items = options(command, sub)
            .iter()
            .filter(|opt| opt.repeatable || !line.seen.contains(&opt.flag))
            .map(|opt| (opt.flag.to_owned(), opt.doc.to_owned()))
            .collect();
        return (Also::Nothing, items);
    }
    let n = line.positionals.len();
    match (command, sub, n) {
        ("help", _, 0) => {
            (Also::Nothing, help::PAGES.iter().map(|&(name, _)| (name.to_owned(), String::new())).collect())
        }
        ("read" | "send" | "wait" | "kill" | "focus" | "move" | "swap", _, 0) | ("swap", _, 1) => {
            (Also::Nothing, sessions(env))
        }
        ("open", _, _) if line.literal => (Also::Files, Vec::new()),
        ("todo" | "schedule" | "remote" | "service" | "completions", None, 0) => {
            (Also::Nothing, owned(subcommands(command)))
        }
        ("todo", Some(sub @ ("show" | "start" | "done" | "rm")), 1) => (Also::Nothing, todos(env, sub)),
        ("schedule", Some("rm"), 1) => (Also::Nothing, schedules(env)),
        ("remote", Some("revoke"), 1) => (Also::Nothing, devices(env)),
        ("service", Some("install" | "uninstall"), 1) => (Also::Nothing, owned(SERVICE_KINDS)),
        _ => (Also::Nothing, Vec::new()),
    }
}

fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|&(word, doc)| (word.to_owned(), doc.to_owned())).collect()
}

/// 会话：宿主里的每个会话的短标识，前台的 agent 和标题做说明；在 runode 的终端里还有 `self`、`left`
/// 这些相对的写法；另有在跑的 agent 种类（`agent:claude`）和工作目录的名字（`cwd:runode`）。宿主没开时
/// 只有相对的写法。
fn sessions(env: &Env) -> Vec<(String, String)> {
    let mut items = Vec::new();
    if env.session.is_some() {
        items.extend(owned(RELATIVE_SESSIONS));
    }
    let Some(sessions) = list_sessions(env) else { return items };
    let mut agents: Vec<&str> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    for info in &sessions {
        let mut doc = String::new();
        if let Some(agent) = info.meta.agent {
            doc = format!("{} {}", agent.kind.display_name(), state_name(agent.state));
            if !agents.contains(&agent.kind.label()) {
                agents.push(agent.kind.label());
            }
        } else if let Some(foreground) = &info.meta.foreground {
            doc = foreground.clone();
        }
        if let Some(title) = title(&info.meta)
            && !title.is_empty()
        {
            if !doc.is_empty() {
                doc.push_str(": ");
            }
            doc.push_str(&title);
        }
        if info.exited {
            doc = format!("exited: {doc}");
        }
        if let Some(name) = info.meta.cwd.as_deref().and_then(|cwd| cwd.file_name()).and_then(|name| name.to_str())
            && !dirs.iter().any(|known| known == name)
        {
            dirs.push(name.to_owned());
        }
        items.push((info.id.to_string()[..SHORT_ID].to_owned(), doc));
    }
    items.extend(agents.iter().map(|kind| (format!("agent:{kind}"), format!("the terminal running {kind}"))));
    items.extend(dirs.iter().map(|dir| (format!("cwd:{dir}"), format!("the terminal working in {dir}"))));
    items
}

fn list_sessions(env: &Env) -> Option<Vec<SessionInfo>> {
    let deadline = Instant::now() + HOST_TIMEOUT;
    let connection = Connection::open_by(env, deadline).ok()?;
    connection.send(&ClientMsg::ListSessions).ok()?;
    match connection.reply_by(deadline).ok()? {
        HostMsg::SessionList { sessions } => Some(sessions),
        _ => None,
    }
}

fn board(env: &Env) -> Option<(Vec<Todo>, Vec<Schedule>)> {
    let deadline = Instant::now() + HOST_TIMEOUT;
    let connection = Connection::open_by(env, deadline).ok()?;
    if !connection.todos {
        return None;
    }
    let req = connection.req();
    connection.send(&ClientMsg::ListTodos { req }).ok()?;
    match connection.reply_by(deadline).ok()? {
        HostMsg::Todos { todos, schedules, .. } => Some((todos, schedules)),
        _ => None,
    }
}

/// 任务：有编号的按编号（`5`，`#5` 在 shell 里是注释），没有的按标识；`show` 之外做完的不提示。
fn todos(env: &Env, sub: &str) -> Vec<(String, String)> {
    let Some((todos, _)) = board(env) else { return Vec::new() };
    todos
        .iter()
        .filter(|todo| sub == "show" || todo.done_at.is_none())
        .map(|todo| {
            let word = if todo.number == 0 { todo.id.clone() } else { todo.number.to_string() };
            (word, todo.spec.title.clone())
        })
        .collect()
}

fn schedules(env: &Env) -> Vec<(String, String)> {
    let Some((_, schedules)) = board(env) else { return Vec::new() };
    schedules.iter().map(|schedule| (schedule.id.clone(), schedule.spec.title.clone())).collect()
}

fn devices(env: &Env) -> Vec<(String, String)> {
    runode_remote::list_devices(&env.dirs)
        .map(|devices| devices.iter().map(|device| (device.device_id.to_string(), device.name.clone())).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 帮助页里提到的每个选项补全都认得，补全认得的每个选项帮助页里也都有：两边一起改。
    #[test]
    fn options_match_the_help_pages() {
        let pages: &[(&str, &[Option<&str>])] = &[
            ("list", &[None]),
            ("read", &[None]),
            ("send", &[None]),
            ("wait", &[None]),
            ("open", &[None]),
            ("kill", &[None]),
            ("focus", &[None]),
            ("move", &[None]),
            ("swap", &[None]),
            ("todo", &[Some("add"), Some("list"), Some("show"), Some("start"), Some("done"), Some("rm")]),
            ("schedule", &[Some("add"), Some("list"), Some("rm")]),
            ("remote", &[Some("pair"), Some("devices"), Some("revoke")]),
            ("service", &[Some("install"), Some("uninstall"), Some("status"), Some("upgrade")]),
            ("completions", &[Some("bash")]),
        ];
        for &(command, subs) in pages {
            // 反引号里的是别的命令的例子（`wait --for done`、`runode --host`），不算。
            let page: String = help::text(Some(command)).split('`').step_by(2).collect();
            let in_page: Vec<&str> = page
                .split(|c: char| c.is_whitespace() || c == '[' || c == ']' || c == '(' || c == ')' || c == '|')
                .filter(|word| word.starts_with("--"))
                .map(|word| word.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()))
                .filter(|word| word.len() > 2)
                .collect();
            let known: Vec<&str> = subs.iter().flat_map(|&sub| options(command, sub)).map(|opt| opt.flag).collect();
            for flag in &in_page {
                assert!(known.contains(flag), "{command}: help mentions {flag} but completion does not know it");
            }
            for flag in known {
                assert!(
                    in_page.contains(&flag),
                    "{command}: completion offers {flag} but the help does not mention it"
                );
            }
        }
    }

    #[test]
    fn commands_match_the_overview() {
        for &(name, doc) in COMMANDS {
            let line = help::OVERVIEW.lines().find(|line| line.trim_start().starts_with(&format!("{name} ")));
            let line = line.unwrap_or_else(|| panic!("{name} is not in the overview"));
            assert_eq!(line.trim_start().trim_start_matches(name).trim(), doc, "{name}");
        }
        let listed =
            help::OVERVIEW.lines().skip_while(|line| *line != "Commands:").skip(1).take_while(|line| !line.is_empty());
        assert_eq!(listed.count(), COMMANDS.len(), "the overview lists a command completion does not know");
    }

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_owned).collect()
    }

    fn complete_offline(line: &str) -> (Also, Vec<String>) {
        let args = words(line);
        let index = args.len() - 1;
        let (also, mut items) = candidates(&args[..index], &args[index], &Env::default());
        items.retain(|(word, _)| word.starts_with(&args[index]));
        (also, items.into_iter().map(|(word, _)| word).collect())
    }

    #[test]
    fn the_first_word_is_a_command() {
        let (_, items) = complete_offline("");
        assert_eq!(items[0], "list");
        assert!(items.contains(&"completions".to_owned()));
        let (_, items) = complete_offline("se");
        assert_eq!(items, ["send", "service"]);
        let (_, items) = complete_offline("--");
        assert_eq!(items, ["--help", "--version"]);
    }

    #[test]
    fn options_follow_the_command_and_vanish_once_used() {
        let (_, items) = complete_offline("open --");
        assert!(items.contains(&"--right".to_owned()));
        let (_, items) = complete_offline("open --right --");
        assert!(!items.contains(&"--right".to_owned()));
        // 能写几次的还在。
        let (_, items) = complete_offline("send a --key up --k");
        assert_eq!(items, ["--key"]);
    }

    #[test]
    fn values_of_options_are_completed() {
        assert_eq!(complete_offline("wait a --for ").1, UNTIL.iter().map(|&(w, _)| w).collect::<Vec<_>>());
        assert_eq!(complete_offline("wait a --for text ").1, Vec::<String>::new());
        assert_eq!(complete_offline("wait a --for text x --t").1, ["--timeout"]);
        assert_eq!(complete_offline("send a --key ct").1, ["ctrl-c", "ctrl-d", "ctrl-u", "ctrl-l", "ctrl-z"]);
        assert_eq!(complete_offline("open --cwd ").0, Also::Dirs);
        assert_eq!(complete_offline("schedule add x --days mon").1, ["mon-fri", "mon"]);
        assert_eq!(complete_offline("todo add x --agent ").1, ["claude", "codex"]);
    }

    #[test]
    fn subcommands_and_their_words() {
        assert_eq!(complete_offline("todo ").1, ["add", "list", "show", "start", "done", "rm"]);
        assert_eq!(
            complete_offline("todo add --").1,
            ["--prompt", "--cwd", "--agent", "--no-worktree", "--start", "--help"]
        );
        assert_eq!(complete_offline("service install ").1, ["host", "app"]);
        assert_eq!(complete_offline("service install host ").1, Vec::<String>::new());
        assert_eq!(complete_offline("completions ").1, ["bash", "fish", "zsh"]);
        assert_eq!(complete_offline("help se").1, ["send", "service", "session"]);
    }

    #[test]
    fn after_the_double_dash_everything_is_text() {
        assert_eq!(complete_offline("open -- ").0, Also::Files);
        assert_eq!(complete_offline("open -- --").1, Vec::<String>::new());
        assert_eq!(complete_offline("send a -- --").1, Vec::<String>::new());
    }

    #[test]
    fn read_counts_the_command_number() {
        // `--command 2` 吃掉次数，接着补会话；`--command` 后面也直接能补会话（没有宿主时没有候选）。
        assert_eq!(complete_offline("read --command 2 --l").1, ["--lines"]);
        assert_eq!(complete_offline("read --command --l").1, ["--lines"]);
    }

    #[test]
    fn options_before_the_command_take_their_values() {
        // `args::parse` 认选项写在命令前面：值不当成命令。
        assert_eq!(complete_offline("--cwd /tmp open --r").1, ["--right"]);
        assert_eq!(complete_offline("--lines 5 read --c").1, ["--command"]);
        assert_eq!(complete_offline("--cwd ").0, Also::Dirs);
        let env = Env { session: Some("abc".into()), ..Env::default() };
        let (_, items) = candidates(&words("--lines 5 read"), "", &env);
        assert_eq!(items.first().map(|(word, _)| word.as_str()), Some("self"));
    }

    #[test]
    fn relative_sessions_need_a_runode_terminal() {
        let env = Env { session: Some("abc".into()), ..Env::default() };
        let (_, items) = candidates(&words("read"), "", &env);
        assert_eq!(items.first().map(|(word, _)| word.as_str()), Some("self"));
        assert!(candidates(&words("read"), "", &Env::default()).1.is_empty());
    }
}
