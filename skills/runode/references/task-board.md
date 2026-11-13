# The task board

## Split the work into tasks

Make each task something one agent can finish on its own: one bug, one
feature, one module to migrate. Tasks run at the same time in separate
worktrees, so two tasks that edit the same lines end in merge conflicts; split
along files or modules, and queue work that depends on another task's result
with `--after` (below).

The title is what the user sees on the board; keep it short. The prompt is
what the agent gets (the title, if you give no `--prompt`): the agent starts
with none of your context, so say what to change, where, how to check it
(which tests to run) and what to leave behind (a commit on its branch, a
report file). Give file paths rather than pasting large content.

```sh
runode todo add 'Migrate parser to the new AST' --start --prompt 'In src/parser, ...
Run cargo test -p parser. Commit on your branch when the tests pass.'
runode todo add 'Update the CLI docs' --agent codex --model gpt-5 --effort high --prompt '...'
```

- `--cwd DIR` picks the repository or directory (default: the current one).
  The worktree is made from it; outside a git repository the task works in
  DIR itself, and `--no-worktree` asks for that inside one too. If
  `~/.runode/tasks.json` has a `setup` entry for the project
  (`"setup": {"/path/to/repo": "pnpm install"}`), a fresh worktree runs it
  first and the agent starts only if it succeeds.
- `--agent CMD` is the command the prompt is given to (default `claude`).
  `--model` and `--effort` pick the model and how hard it thinks; they need
  `--agent` to be an agent runode knows by name (`claude`, `codex`, `pi`,
  `opencode`, ...), not a whole command such as `codex --full-auto`.
- `--skill NAME` (once per skill) tells the agent which skills to use.
- `--auto` lets the agent work without asking for permission. Only add it
  when the user wants the task to run unattended: the agent then edits files
  and runs commands without asking.
- `--auto-done` moves the task to done by itself once its agent has worked
  and stopped, and ends its terminal unless a window shows it (the worktree
  and branch then go as with `todo done`). It stays in needs_you while the
  agent asks a question or after it exits. Use it for tasks whose result is
  a report or a commit nobody needs to look at first. The agent is told to
  end with `runode todo report ok|silent|failed 'summary'`: the summary
  stays on the task (`runode todo show`), `silent` sends no phone
  notification, and `failed` keeps the task in needs_you and notifies the
  phone. If the agent reports nothing, the bottom of its screen is kept
  instead. Longer reports still belong in a file. A question left
  unanswered for 30 minutes marks the task failed and ends its terminal
  unless a window shows it.
- `--time-limit MINUTES` marks the task failed if its agent is still working
  or asking for an answer that long after it starts (a finished agent waiting
  for review is left alone), keeps the bottom of its screen as the result and ends its
  terminal unless a window shows it. Give unattended tasks one so a stuck
  agent does not run forever.
- Without `--start` the task waits in the todo column for the user to start
  it. `--start` starts it at once and prints the task id, then the id of the
  session its agent runs in. Making the worktree can take a while in a big
  repository. `runode todo start ID` starts a task added earlier, or starts it
  again in a new terminal once its shell has exited.
- `--queue` instead lets runode start the task itself, in board order, as soon
  as fewer agents are working than `runode todo limit` allows (no limit unless
  the user set one). `--after ID` queues it to start once task ID is done, so
  you can hand out a whole chain at once. It only orders the tasks: a new
  worktree starts from what DIR has checked out, not from task ID's branch,
  so tell the agent to merge that branch first (its name is in
  `runode todo show ID`) or have both work in DIR with `--no-worktree`. If
  task ID fails, the queued one is marked failed too.
  `runode todo queue ID [--after ID]` and `runode todo unqueue ID` change an
  existing task. Respect a limit the user set; change it with
  `runode todo limit N` only when asked.

## Follow the tasks

Every task has a number, shown as `#5` on the board and in `runode todo list`;
numbers count up and are never reused. Wherever a command takes a task ID,
the number works too (`5`; quote `'#5'`, since `#` starts a comment in some
shells). The user will often name a task by its number.

`runode todo list` shows each task's column: `todo` (not started; `todo
(queued)` when runode will start it),
`in_progress` (the agent is working), `needs_you` (the agent asks a question,
has stopped, or its terminal is gone) and `done` (marked by the user or by
`--auto-done`). `--json` adds the details:
the session, the worktree and the branch. Done tasks are listed for seven
days.

`runode todo show ID` prints everything about one task, however long ago it
was done: its column, prompt, worktree and branch, session (and whether it
is still open), schedule and times. `--json` gives the same as JSON.

A task's terminal is not in any window until the user opens it from the
board, but the session commands work on its session id:

```sh
runode wait "$session" --for done --timeout 3600   # the agent worked, then stopped
runode read "$session" --lines 80
```

When a task needs the user, tell them rather than answering for them unless
they asked you to. `runode todo done ID` marks a task done once its work is
merged or no longer wanted; once its terminal is gone or its shell has
exited, the terminal, worktree and branch are removed, unless they hold
uncommitted changes or unmerged commits. `runode todo rm ID` drops a task from
the board and leaves its terminal open while its shell runs.

## Schedules

A schedule adds a task and queues it whenever it is due, in this computer's
local time; it starts at once unless the `runode todo limit` is reached:

```sh
runode schedule add 'Nightly flaky-test hunt' --daily 02:00 --prompt '...'
runode schedule add 'Weekly report' --daily 09:00 --days mon --prompt '...'
runode schedule add 'Check the deploy' --every 30 --auto-done --prompt '...'
runode schedule add 'Standup notes' --cron '30 9 * * 1-5' --prompt '...'
runode schedule add 'Release notes' --at '2026-11-02 10:00' --prompt '...'
runode schedule list     # WHEN and NEXT, the next time it fires
runode schedule pause ID # NEXT shows paused; resume ID counts from then on
runode schedule rm ID    # the tasks it started stay
```

`--cron` takes one crontab line (minute hour day month weekday) for times the
other forms cannot say.

Scheduled tasks work in DIR itself unless you add `--worktree` (DIR must then
be in a git repository), since a new worktree each time is rarely wanted for
reports and checks. `--auto-done` applies to every task the schedule starts,
so routine checks do not pile up in needs_you; so do `--model`, `--effort`,
`--skill`, `--auto` and `--time-limit`, and a schedule nobody watches usually
wants `--auto --auto-done --time-limit MINUTES`. A schedule missed while the
computer slept fires once when it wakes; one whose last task is still
queued, working or asking is skipped. A task that could not start is tried again
after 1 and 5 more minutes, then marked failed. Schedules
fire while runode runs, or its host runs in the background.
