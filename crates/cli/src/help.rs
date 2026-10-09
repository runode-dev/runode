//! 用法说明：`runode help` 打印总览，`runode help COMMAND`、`runode COMMAND --help` 打印那条命令的
//! 一页。改了命令或选项，`runode-completion` 里 runode 自己的命令规格也要跟着改，按 Tab 才补得出来。
//! 每页的 `Usage:` 块里，命令名（连同 `setup claude|codex` 这样写死的词）要和规格对得上，
//! `args` 的测试 `help_and_completion_spec_list_the_same_commands` 会核对。

/// `runode help` 打印的总览：只列命令，细节在各命令的页里。
pub(crate) const OVERVIEW: &str = "\
Drive the terminals of the running runode app from the command line.

Usage: runode [COMMAND]

Without a command, runode opens its window. `rn` is a short name for runode:
`rn list` is `runode list`. Except setup, remote and service, commands talk to
the running runode app; inside a runode terminal they find it through
RUNODE_SOCKET, and RUNODE_SESSION names the terminal they run in.

Commands:
  list     List the terminal sessions
  read     Print the text on a session's screen
  send     Type text and keys into a session
  wait     Wait for an agent, a command or some text on the screen
  open     Open a terminal in the app
  kill     End a session and close its pane
  focus    Show a session's pane and bring its window to the front
  setup    Teach an agent to use runode
  remote   Pair phones for remote access and manage them
  service  Start runode at login
  help     Show this help, or the help of a command
  version  Show the version

Options:
  -h, --help     Show this help; after a command, the help of that command
  -V, --version  Show the version

Run `runode help COMMAND` for the options of a command, and `runode help
session` for how to name a session.

Exit status: 0 done, 1 failed, 2 bad arguments, 3 the session exited,
4 the command waited for failed, 124 timed out.
";

/// `runode help` 后面能接的词和对应的页：每条命令一页，另有一页讲怎么指定会话。
pub(crate) const PAGES: &[(&str, &str)] = &[
    ("list", LIST),
    ("read", READ),
    ("send", SEND),
    ("wait", WAIT),
    ("open", OPEN),
    ("kill", KILL),
    ("focus", FOCUS),
    ("setup", SETUP),
    ("remote", REMOTE),
    ("service", SERVICE),
    ("session", SESSION),
];

/// `word` 是哪一页的名字。
pub(crate) fn topic(word: &str) -> Option<&'static str> {
    PAGES.iter().map(|&(name, _)| name).find(|&name| name == word)
}

/// 要打印的说明：没给话题是总览，给了是那一页。
pub(crate) fn text(topic: Option<&str>) -> &'static str {
    topic.and_then(|topic| PAGES.iter().find(|&&(name, _)| name == topic)).map_or(OVERVIEW, |&(_, page)| page)
}

const LIST: &str = "\
List the terminal sessions.

Usage: runode list [--json]

Options:
      --json  Print JSON with every detail instead of a table
  -h, --help  Show this help

Columns: * marks your own terminal, REL where a session sits next to yours,
FG the program in front, VIEW shown, hidden (another tab) or bg (in no window).
";

const READ: &str = "\
Print the text on a session's screen.

Usage: runode read [SESSION] [--lines N | --command [N]]

Arguments:
  [SESSION]  The session to read; defaults to your own. See `runode help
             session`

Options:
      --lines N      Print N lines from the bottom, scrollback included
      --command [N]  Print the output of the Nth last command (default 1);
                     needs shell integration
  -h, --help         Show this help
";

const SEND: &str = "\
Type text and keys into a session.

Usage: runode send SESSION [TEXT...] [--paste] [--key KEY]... [--enter]
                   [--wait] [--timeout SECS]

Arguments:
  SESSION    The session to type into. See `runode help session`
  [TEXT...]  Words joined by spaces; `-` reads the text from stdin. Put `--`
             before text that starts with a dash

Options:
      --paste         Paste TEXT instead of typing it
      --key KEY       Press KEY after the text, once per use. A KEY is ctrl-c,
                      alt-b, shift-tab, esc, enter, tab, up, pageup, f5 and
                      the like; 'down*3' (quoted for the shell) presses it
                      three times
      --enter         Press Enter at the end
      --wait          Wait after sending: for the agent like `wait --for
                      done` if one runs there (failing if it shows no
                      activity within 10 seconds), for the command like `wait
                      --for command` if Enter ran one at a shell prompt with
                      shell integration, else until the screen is quiet for 2
                      seconds. It says which on stderr
      --timeout SECS  Stop waiting after SECS
  -h, --help          Show this help
";

const WAIT: &str = "\
Wait until an agent, a command or the screen reaches a state.

Usage: runode wait SESSION [--for UNTIL] [--timeout SECS]

Arguments:
  SESSION  The session to watch. See `runode help session`

Options:
      --for UNTIL     What to wait for, one of
                        stopped  the agent is not working: idle, asking you,
                                 or no agent (default)
                        done     the agent worked, then stopped
                        working, idle, blocked
                                 the agent is in that state
                        command  the next command at the shell prompt
                                 finished; prints exit N and fails with
                                 status 4 if N is not 0
                        text REGEX
                                 a line on the screen matches REGEX; prints
                                 the line
                        quiet SECS
                                 the screen did not change for SECS
      --lines N       With `--for text`: look only at the last N lines
      --new           With `--for text`: skip the lines already there
      --timeout SECS  Give up after SECS
  -h, --help          Show this help

Agent states print the state they reached. If runode is upgraded meanwhile, the
wait goes on (except `--for command`, which fails).
";

const OPEN: &str = "\
Open a terminal in the app and print the new session's id.

Usage: runode open [--tab | --right | --down] [--near SESSION] [--cwd DIR]
                   [--focus] [-- COMMAND...]

Arguments:
  [COMMAND...]  Typed into the new shell

Options:
      --tab           Open a new tab after the one SESSION is in (default)
      --right         Split SESSION's pane to the right
      --down          Split SESSION's pane downward
      --near SESSION  The session to open next to; defaults to your own, else
                      the front window's pane. See `runode help session`
      --cwd DIR       Start in DIR; defaults to SESSION's directory
      --focus         Switch to the new terminal; without it the app stays
                      where it is
  -h, --help          Show this help
";

const KILL: &str = "\
End a session and close its pane.

Usage: runode kill SESSION

Arguments:
  SESSION  The session to end. See `runode help session`

Options:
  -h, --help  Show this help
";

const FOCUS: &str = "\
Show a session's pane and bring its window to the front.

Usage: runode focus [SESSION]

Arguments:
  [SESSION]  The session to show; defaults to your own. See `runode help
             session`

Options:
  -h, --help  Show this help
";

const SETUP: &str = "\
Teach an agent to use runode: installs a skill in ~/.claude/skills/runode
(claude) or ~/.agents/skills/runode (codex).

Usage: runode setup claude|codex [--print]

Options:
      --print  Show the skill instead of installing it
  -h, --help   Show this help
";

const REMOTE: &str = "\
Pair phones for remote access and manage them.

Usage: runode remote pair [--addr ADDR]...
       runode remote devices [--json]
       runode remote revoke DEVICE

Commands:
  pair     Pair a phone: shows a QR code and its link, valid for 5 minutes,
           and waits for the phone. Sets remote-access = true in the config
           if it is off (runode must be running to pick it up); once paired,
           offers to set terminal-host = true so remote access keeps running
           after you quit runode
  devices  List the paired phones
  revoke   Unpair a phone; it is disconnected within seconds

Arguments:
  DEVICE  With `revoke`: a device id or a unique prefix of it, as `runode
          remote devices` shows

Options:
      --addr ADDR  With `pair`: also offer ADDR to the phone, once per use
                   (say 127.0.0.1 for a simulator on this Mac)
      --json       With `devices`: print JSON instead of a table
  -h, --help       Show this help
";

const SERVICE: &str = "\
Start runode at login: a launchd LaunchAgent on macOS, a systemd user service
on Linux. Installing a service you already have replaces its file. Neither
install nor uninstall stops a host that is already running, so its sessions
stay open.

Usage: runode service install [host|app]
       runode service uninstall [host|app]
       runode service status

Commands:
  install    Install the login service and, for the host, start it now if it
             is not running
  uninstall  Remove the login service; a running host keeps running
  status     Show which login services are installed

Arguments:
  host  The terminal host without a window (`runode --host`), the default.
        It keeps running in the background while remote-access = true and a
        phone is paired, so the phone can connect without the app open;
        otherwise it exits when idle
  app   The Runode app, opened at the next login (macOS only)

Options:
  -h, --help  Show this help
";

const SESSION: &str = "\
How to name a session.

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
";
