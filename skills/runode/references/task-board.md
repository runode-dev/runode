# The task board

## Split the work into tasks

Make each task something one agent can finish on its own: one bug, one
feature, one module to migrate. Tasks run at the same time in separate
worktrees, so two tasks that edit the same lines end in merge conflicts; split
along files or modules, and leave work that depends on another task's result
for later.

The title is what the user sees on the board; keep it short. The prompt is
what the agent gets (the title, if you give no `--prompt`): the agent starts
with none of your context, so say what to change, where, how to check it
(which tests to run) and what to leave behind (a commit on its branch, a
report file). Give file paths rather than pasting large content.

```sh
runode todo add 'Migrate parser to the new AST' --start --prompt 'In src/parser, ...
Run cargo test -p parser. Commit on your branch when the tests pass.'
runode todo add 'Update the CLI docs' --agent 'codex --full-auto' --prompt '...'
```

- `--cwd DIR` picks the repository or directory (default: the current one).
  The worktree is made from it; outside a git repository the task works in
  DIR itself, and `--no-worktree` asks for that inside one too. If
  `~/.runode/tasks.json` has a `setup` entry for the project
  (`"setup": {"/path/to/repo": "pnpm install"}`), a fresh worktree runs it
  first and the agent starts only if it succeeds.
- `--agent CMD` is the command the prompt is given to (default `claude`).
- Without `--start` the task waits in the todo column for the user to start
  it. `--start` starts it at once and prints the task id, then the id of the
  session its agent runs in. Making the worktree can take a while in a big
  repository. `runode todo start ID` starts a task added earlier, or starts it
  again in a new terminal once its shell has exited.

## Follow the tasks

Every task has a number, shown as `#5` on the board and in `runode todo list`;
numbers count up and are never reused. Wherever a command takes a task ID,
the number works too (`5`; quote `'#5'`, since `#` starts a comment in some
shells). The user will often name a task by its number.

`runode todo list` shows each task's column: `todo` (not started),
`in_progress` (the agent is working), `needs_you` (the agent asks a question,
has stopped, or its terminal is gone) and `done`. `--json` adds the details:
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

A schedule adds a task and starts it whenever it is due, in this computer's
local time:

```sh
runode schedule add 'Nightly flaky-test hunt' --daily 02:00 --prompt '...'
runode schedule add 'Weekly report' --daily 09:00 --days mon --prompt '...'
runode schedule add 'Check the deploy' --every 30 --prompt '...'
runode schedule add 'Release notes' --at '2026-11-02 10:00' --prompt '...'
runode schedule list     # WHEN and NEXT, the next time it fires
runode schedule rm ID    # the tasks it started stay
```

Scheduled tasks work in DIR itself unless you add `--worktree` (DIR must then
be in a git repository), since a new worktree each time is rarely wanted for
reports and checks. A schedule missed while the computer slept fires once
when it wakes; one whose last task is still working, or could not start, is
skipped. Schedules
fire while runode runs, or its host runs in the background.
