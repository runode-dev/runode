//! 用法说明：`runode help` 打印总览，`runode help COMMAND`、`runode COMMAND --help` 打印那条命令的
//! 一页。

/// `runode help` 打印的总览：只列命令，细节在各命令的页里。
pub(crate) const OVERVIEW: &str = "\
Drive the terminals of the running runode app from the command line.

Usage: runode [COMMAND]

Without a command, runode opens its window. `rn` is a short name for runode:
`rn list` is `runode list`. Except remote and service, commands talk to the
running runode app; inside a runode terminal they find it through
RUNODE_SOCKET, and RUNODE_SESSION names the terminal they run in.

To teach your agents to use runode, install its skill:
  npx skills add runode-dev/runode --skill runode -g
and update it with `npx skills update runode -g`.

Commands:
  list         List the terminal sessions
  read         Print the text on a session's screen
  send         Type text and keys into a session
  wait         Wait for an agent, a command or some text on the screen
  open         Open a terminal in the app
  kill         End a session and close its pane
  focus        Show a session's pane and bring its window to the front
  todo         Put tasks for agents on the task board and start them
  schedule     Start a task for an agent at set times
  remote       Pair phones for remote access and manage them
  service      Start runode at login
  completions  Print a completion script for your shell
  help         Show this help, or the help of a command
  version      Show the version

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
    ("todo", TODO),
    ("schedule", SCHEDULE),
    ("remote", REMOTE),
    ("service", SERVICE),
    ("completions", COMPLETIONS),
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
While the app is closed, places and VIEW follow the layout it left behind, so
shown does not mean on screen.
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
  [TEXT...]  Words joined by spaces; `-` reads the text from stdin. A last
             word Enter presses Enter, as in tmux send-keys. Put `--` before
             text that starts with a dash or ends with the word Enter

Options:
      --paste         Paste TEXT instead of typing it
      --key KEY       Press KEY after the text, once per use. A KEY is ctrl-c,
                      alt-b, shift-tab, esc, enter, tab, up, pageup, f5 and
                      the like; 'down*3' (quoted for the shell) presses it
                      three times
      --enter         Press Enter at the end
      --wait          Wait after sending: for the agent like `wait --for done`
                      if one runs there (failing if it shows no activity
                      within 10 seconds, or printing blocked if it was asking
                      and the screen shows a different question by then), for
                      the command like `wait --for command` if Enter ran one
                      at a shell prompt with shell integration, else until the
                      screen is quiet for 2 seconds. It says which on stderr.
                      Waiting for a command refuses typed text with line
                      breaks: use --paste or join the commands with &&
      --timeout SECS  Stop waiting after SECS; only with --wait
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
      --new           With `--for text`: skip the lines already there. A line
                      that is cleared and printed again unchanged is not new
      --timeout SECS  Give up after SECS
  -h, --help          Show this help

Agent states print the state they reached. If runode is upgraded meanwhile, the
wait goes on (except `--for command`, which fails).
";

const OPEN: &str = "\
Open a terminal in the app and print the new session's id.

Usage: runode open [--tab | --right | --down] [--near SESSION] [--cwd DIR]
                   [--focus] [--fold | --no-fold] [-- COMMAND...]

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
      --fold          Open the split already folded to its title bar; needs
                      --right or --down and the cards window style, and
                      cannot be combined with --focus
      --no-fold       Keep the split unfolded. Without --fold or --no-fold, a
                      split opened from an agent's terminal is folded unless
                      the `agent-split-fold` setting is off
  -h, --help          Show this help

While the app is closed, `open` makes a new tab at the end of SESSION's
workspace whatever the placement, and does not switch to it.
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

const TODO: &str = "\
Put tasks for agents on runode's task board. Starting a task opens a terminal
of its own (not in any window until you open it from the board) and runs the
agent there with the prompt; by default in a new git worktree and branch, so
it does not touch your checkout.

Usage: runode todo add TITLE... [--prompt TEXT] [--cwd DIR] [--agent CMD]
                       [--no-worktree] [--start]
       runode todo list [--json]
       runode todo show ID [--json]
       runode todo start ID
       runode todo done ID
       runode todo rm ID

Commands:
  add    Add a task and print its id; with --start, also start it and print
         the id of the session its agent runs in
  list   List the tasks and their column on the board: todo (not started),
         in_progress (the agent is working), needs_you (the agent is asking,
         has stopped, or its terminal is gone) or done
  show   Print everything about one task: its column, prompt, worktree and
         branch, session, schedule and times; done tasks of any age too
  start  Start a task and print the id of its session; a task whose terminal
         is still running is not started again, one whose shell has exited
         starts in a new terminal. Making the worktree can take a while in a
         big repository
  done   Mark a task done. Once its terminal is gone or its shell has exited,
         the terminal, worktree and branch are removed, unless the worktree
         has uncommitted changes or the branch has unmerged commits
  rm     Remove a task; its terminal stays open while its shell runs

Arguments:
  TITLE...  With `add`: the title, words joined by spaces; also the prompt
            unless --prompt is given
  ID        A task, as `runode todo list` shows: its number (5 or #5) or
            its id

Options:
      --prompt TEXT  With `add`: the prompt for the agent
      --cwd DIR      With `add`: the directory to work in; defaults to the
                     current one
      --agent CMD    With `add`: the command the prompt is given to, such as
                     `codex --full-auto` (default claude)
      --no-worktree  With `add`: work in DIR itself, not in a new worktree.
                     Implied when DIR is not in a git repository
      --start        With `add`: start the task right away
      --json         With `list` and `show`: print JSON instead of text
  -h, --help         Show this help

Done tasks are listed for seven days; `show` finds them later too. Numbers
count up as tasks are added and are never given to another task.
";

const SCHEDULE: &str = "\
Start a task for an agent at set times: when a schedule is due, runode adds a
task to the task board (see `runode help todo`) and starts it. Times are this
computer's local time. A schedule missed while the computer slept or runode was
not running fires once when it is back; one whose last task is still working,
or could not start, is skipped.

Usage: runode schedule add TITLE... (--at TIME | --daily HH:MM [--days DAYS] |
                           --every MINUTES) [--prompt TEXT] [--cwd DIR]
                           [--agent CMD] [--worktree]
       runode schedule list [--json]
       runode schedule rm ID

Commands:
  add   Add a schedule and print its id
  list  List the schedules with when they fire and the next time they will
  rm    Remove a schedule; the tasks it started stay

Arguments:
  TITLE...  With `add`: the title of the tasks it starts, words joined by
            spaces; also the prompt unless --prompt is given
  ID        A schedule id, as `runode schedule list` shows

Options:
      --at TIME        With `add`: once, at TIME written \"YYYY-MM-DD HH:MM\"
      --daily HH:MM    With `add`: every day at HH:MM (24-hour clock)
      --days DAYS      With --daily: only on these days, like mon,wed,fri or
                       mon-fri
      --every MINUTES  With `add`: every MINUTES minutes
      --prompt TEXT    With `add`: the prompt for the agent
      --cwd DIR        With `add`: the directory to work in; defaults to the
                       current one
      --agent CMD      With `add`: the command the prompt is given to
                       (default claude)
      --worktree       With `add`: give each task a new git worktree and
                       branch; DIR must be in a git repository
      --json           With `list`: print JSON instead of a table
  -h, --help           Show this help
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
on Linux; the app is added to the login items on macOS 13 and later.
Installing a service you already have replaces its file. Neither
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
        phone is paired, so the phone can connect without the app open,
        and while the task board has schedules, so they start on time;
        otherwise it exits when idle
  app   The Runode app, opened at the next login (macOS only)

Options:
  -h, --help  Show this help
";

const COMPLETIONS: &str = "\
Print a completion script for your shell, for `runode` and `rn` alike.

Usage: runode completions bash|fish|zsh

The script asks runode for the candidates each time you press Tab, so it
completes the sessions, tasks and devices there are right now and follows
runode when it is upgraded. Install it with

  bash  echo 'eval \"$(runode completions bash)\"' >> ~/.bashrc
  fish  runode completions fish > ~/.config/fish/conf.d/runode.fish
  zsh   echo 'eval \"$(runode completions zsh)\"' >> ~/.zshrc
        (after the line that runs compinit)

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
them; while the app is closed, in the layout it left behind.
";
