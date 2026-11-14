//! 用法说明：`runode help` 打印总览，`runode help COMMAND`、`runode COMMAND --help` 打印那条命令的
//! 一页。

/// `runode help` 打印的总览：只列命令，细节在各命令的页里。
pub(crate) const OVERVIEW: &str = "\
Drive the terminals of the running Runode app from the command line.

Usage: runode [COMMAND]

Without a command, runode opens its window. `rn` is a short name for runode:
`rn list` is `runode list`. Except remote and service, commands talk to the
running Runode app; inside a Runode terminal they find it through
RUNODE_SOCKET, and RUNODE_SESSION names the terminal they run in. In a
terminal runode opened on an SSH host, they talk to the Runode on that host
and see only the terminals running there.

To teach your agents to use Runode, install its skill:
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
  move         Move a session's pane to another place in its window
  swap         Swap the panes of two sessions
  todo         Put tasks for agents on the task board and start them
  schedule     Start a task for an agent at set times
  remote       Pair phones and computers for remote access and manage them
  service      Start Runode at login, and upgrade the running host
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
    ("move", MOVE),
    ("swap", SWAP),
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
Print the text on a session's screen. A line the terminal wrapped comes back
as one line.

Usage: runode read [SESSION] [--lines N | --command [N]]

Arguments:
  [SESSION]  The session to read; defaults to your own. See `runode help
             session`

Options:
      --lines N      Print the last N screen rows, scrollback included
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
             word Enter presses Enter, as in tmux send-keys. A word that
             starts with a dash is taken for an option: put `--` before the
             text if any word of it does (echo -n) or it ends with the word
             Enter

Options:
      --paste         Paste TEXT instead of typing it
      --key KEY       Press KEY after the text, once per use. A KEY is ctrl-c,
                      alt-b, shift-tab, esc, enter, tab, up, pageup, f5 and
                      the like; 'down*3' (quoted for the shell) presses it
                      three times
      --enter         Press Enter at the end
      --wait          Wait after sending: for the agent like `wait --for done`
                      if one runs there (printing blocked if it starts asking
                      even before it was seen working, or if it was asking and
                      the screen shows a different question 10 seconds later;
                      failing if it shows no activity within 10 seconds), for
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
      --lines N       With `--for text`: look only at the last N screen rows
      --new           With `--for text`: skip the lines already there. A line
                      that is cleared and printed again unchanged is not new
      --timeout SECS  Give up after SECS
  -h, --help          Show this help

Agent states print the state they reached. If Runode is upgraded meanwhile, the
wait goes on (except `--for command`, which fails).
";

const OPEN: &str = "\
Open a terminal in the app and print the new session's id.

Usage: runode open [--tab | --right | --down] [--near SESSION] [--cwd DIR]
                   [--focus] [--fold | --no-fold] [-- COMMAND...]

Arguments:
  [COMMAND...]  Typed into the new shell: the words are joined by spaces
                and typed as they are, so quotes your shell removed are
                gone. For a command that needs quotes, pass it as one
                argument with the quotes inside: -- 'codex \"what is wrong?\"'

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

const MOVE: &str = "\
Move a session's pane to another place in the same window: beside another
pane, or into a new tab.

Usage: runode move [SESSION] [--tab | --right | --down] [--near SESSION]
                   [--focus]

Arguments:
  [SESSION]  The session to move; defaults to your own. See `runode help
             session`

Options:
      --tab           Move it into a new tab after the one NEAR is in
                      (default)
      --right         Put it to the right of NEAR's pane
      --down          Put it below NEAR's pane
      --near SESSION  Where to move it; defaults to your own session, else
                      the front window's pane. Must be in the same window
      --focus         Switch to the moved terminal; without it the app stays
                      where it is
  -h, --help          Show this help
";

const SWAP: &str = "\
Swap the panes of two sessions in the same window; they may be in different
tabs.

Usage: runode swap SESSION OTHER [--focus]

Arguments:
  SESSION  One session; `self` for your own. See `runode help session`
  OTHER    The session to swap it with

Options:
      --focus  Switch to SESSION after the swap
  -h, --help   Show this help
";

const TODO: &str = "\
Put tasks for agents on Runode's task board. Starting a task opens a terminal
of its own (not in any window until you open it from the board) and runs the
agent there with the prompt; by default in a new git worktree and branch, so
it does not touch your checkout.

Usage: runode todo add TITLE... [--prompt TEXT] [--cwd DIR] [--agent CMD]
                       [--model MODEL] [--effort EFFORT] [--skill NAME]...
                       [--no-worktree] [--auto] [--auto-done]
                       [--time-limit MINUTES] [--start | --queue | --after ID]
       runode todo list [--json]
       runode todo show ID [--json]
       runode todo start ID
       runode todo done ID
       runode todo rm ID
       runode todo report OUTCOME [TEXT... | -]
       runode todo queue ID [--after ID]
       runode todo unqueue ID
       runode todo limit [N]

Commands:
  add    Add a task and print its id; with --start, also start it and print
         the id of the session its agent runs in
  queue  Let Runode start a task that has not started: in board order, when
         fewer agents are working than the limit, and with --after only once
         that task is done. If that task fails, this one is marked failed
         too. One that cannot start is tried again after 1 and 5 more
         minutes, then marked failed
  unqueue
         Take a task out of the queue; it waits in todo for you to start it
  limit  Print how many agents may work at once (0, the default, is no
         limit); with N, set it. Tasks you start yourself do not wait for
         it, but count
  list   List the tasks and their column on the board: todo (not started),
         in_progress (the agent is working), needs_you (the agent is asking,
         has stopped, or its terminal is gone) or done (marked by you, or
         by Runode with --auto-done)
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
  report For the agent of a task, run in the task's own terminal: record how
         the task ended and a short result, shown on the board and by `show`.
         ok and silent let --auto-done mark it done (silent sends no phone
         notification); failed keeps it in needs_you and notifies the phone

Arguments:
  TITLE...  With `add`: the title, words joined by spaces; also the prompt
            unless --prompt is given
  ID        A task, as `runode todo list` shows: its number (5 or #5) or
            its id
  OUTCOME   With `report`: ok, silent (nothing worth telling) or failed
  TEXT...   With `report`: the result, words joined by spaces; `-` reads it
            from stdin

Options:
      --prompt TEXT  With `add`: the prompt for the agent
      --cwd DIR      With `add`: the directory to work in; defaults to the
                     current one
      --agent CMD    With `add`: the command the prompt is given to, such as
                     `codex --full-auto` (default claude)
      --model MODEL  With `add`: the model the agent uses, such as opus. The
                     agent must be one Runode knows (claude, codex, pi, ...),
                     not a whole command
      --effort EFFORT
                     With `add`: how hard the agent thinks, such as high; the
                     same agents as --model
      --skill NAME   With `add`: ask the agent to use this skill; once per
                     skill
      --no-worktree  With `add`: work in DIR itself, not in a new worktree.
                     Implied when DIR is not in a git repository
      --auto         With `add`: let the agent work without asking for
                     permission, for agents Runode knows how to tell
      --auto-done    With `add`: mark the task done once its agent has
                     worked and stopped, and end its terminal unless a
                     window shows it. Not while the agent asks a question
                     or after it exits; a question unanswered for 30
                     minutes marks the task failed instead
      --time-limit MINUTES
                     With `add`: mark the task failed if its agent is still
                     working or asking MINUTES after it starts, keeping the
                     bottom of its screen, and end its terminal unless a
                     window shows it
      --start        With `add`: start the task right away
      --queue        With `add`: queue the task (see `queue`)
      --after ID     With `add` and `queue`: queue the task to start once
                     task ID is done
      --json         With `list` and `show`: print JSON instead of text
  -h, --help         Show this help

Done tasks are listed for seven days; `show` finds them later too. Numbers
count up as tasks are added and are never given to another task.
";

const SCHEDULE: &str = "\
Start a task for an agent at set times: when a schedule is due, Runode adds a
task to the task board (see `runode help todo`) and queues it, so it starts at
once unless `runode todo limit` is reached. Times are this computer's local
time. A schedule missed while the computer slept or Runode was not running
fires once when it is back; one whose last task is still queued, working or
asking a question is skipped. A task that could not start is tried again after
1 and 5 more minutes and then marked failed, which no longer holds the schedule
back.

Usage: runode schedule add TITLE... (--at TIME | --daily HH:MM [--days DAYS] |
                           --every MINUTES | --cron EXPR) [--prompt TEXT]
                           [--cwd DIR] [--agent CMD] [--model MODEL]
                           [--effort EFFORT] [--skill NAME]... [--worktree]
                           [--auto] [--auto-done] [--time-limit MINUTES]
       runode schedule list [--json]
       runode schedule rm ID
       runode schedule pause ID
       runode schedule resume ID

Commands:
  add     Add a schedule and print its id
  list    List the schedules with when they fire and the next time they will
  rm      Remove a schedule; the tasks it started stay
  pause   Stop a schedule from firing until it is resumed
  resume  Let a paused schedule fire again, counting from now: the times it
          missed while paused do not fire

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
      --cron EXPR      With `add`: at the times a crontab line gives, as one
                       argument: minute hour day month weekday, each `*`, a
                       number, a range a-b or a list, with /STEP; weekday 0
                       or 7 is Sunday. \"30 9 * * 1-5\" is 9:30 on weekdays
      --prompt TEXT    With `add`: the prompt for the agent
      --cwd DIR        With `add`: the directory to work in; defaults to the
                       current one
      --agent CMD      With `add`: the command the prompt is given to
                       (default claude)
      --model MODEL, --effort EFFORT, --skill NAME, --auto,
      --time-limit MINUTES
                       With `add`: as for `runode todo add`
      --worktree       With `add`: give each task a new git worktree and
                       branch; DIR must be in a git repository
      --auto-done      With `add`: mark each task done once its agent has
                       worked and stopped (see `runode help todo`)
      --json           With `list`: print JSON instead of a table
  -h, --help           Show this help
";

const REMOTE: &str = "\
Pair phones and computers for remote access and manage them.

Usage: runode remote pair [--addr ADDR]...
       runode remote devices [--json]
       runode remote revoke DEVICE

Commands:
  pair     Pair a phone or another computer: shows a QR code and its link,
           valid for 5 minutes, and waits for the device. Scan the code with
           the phone, or paste the link into Runode on the other computer.
           Sets remote-access = true in the config if it is off (Runode must
           be running to pick it up); once paired, offers to set
           terminal-host = true so remote access keeps running after you
           quit Runode. Devices on another network reach this computer over
           the internet only with remote-access-address = 0.0.0.0
  devices  List the paired devices, phones and computers
  revoke   Unpair a device; it is disconnected within seconds

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
Start Runode at login: a launchd LaunchAgent on macOS, a systemd user service
on Linux, a value under HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run on
Windows; the app is added to the login items on macOS 13 and later.
Installing a service you already have replaces its file. Neither
install nor uninstall stops a host that is already running, so its sessions
stay open.

Usage: runode service install [host|app]
       runode service uninstall [host|app]
       runode service status
       runode service upgrade

Commands:
  install    Install the login service and, for the host, start it now if it
             is not running
  uninstall  Remove the login service; a running host keeps running
  status     Show which login services are installed
  upgrade    After installing a new Runode, hand the sessions of the running
             host to a host of this build, so they stay open. A host run by
             the systemd service is replaced through the service, which keeps
             running. Not on Windows, and not while the app is open

Arguments:
  host  The terminal host without a window (`runode --host`), the default.
        Started this way it keeps running in the background even with no
        sessions, so a phone with remote-access = true can connect without
        the app open and task board schedules start on time
  app   The Runode app, opened at the next login (macOS and Windows)

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
                the pane next to yours in that direction; a pane floating
                over the others is nobody's neighbor
  next, prev    the next or previous pane in your tab, wrapping around
  pane:N        pane N of your tab (of the front window's tab outside Runode)
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
