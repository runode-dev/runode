---
name: runode
description: Drive other terminals in the runode app - open panes and tabs, list them, type commands or send control keys into them, wait for a command or another agent to finish, and read their output. Use it whenever you run inside a runode terminal (RUNODE_SESSION is set) and the task involves another terminal or another coding agent - tests, a build or a dev server in a pane next to yours, what a neighbouring pane printed, work handed to other agents. In particular use it whenever the user names agent CLIs to bring in (claude, codex, pi, gemini, opencode...), in any language - "use claude and pi to review this", "let codex fix it", a cross-review, a red team / blue team review, a second opinion. Start each named agent in a pane of its own and talk to it through runode instead of imitating it with your own subagents, even if it is the same agent as you. Also use it when the user only says "the pane on the right", "the other terminal" or "run it on the side" and never names runode.
---

# Driving other runode terminals

runode is a terminal app with panes, tabs, workspaces and windows. When you run
inside one of its terminals, the `runode` command can see and operate every
other terminal in the app. `RUNODE_SESSION` holds the id of your own terminal
and `RUNODE_SOCKET` tells `runode` where the app is; if `runode` is not on your
PATH, use `"$RUNODE_BIN"`. `rn` is a short name for it.

Run `runode help` for the commands and `runode help COMMAND` (for example
`runode help send`) for the full reference of one. The essentials follow.

## Find a terminal

```sh
runode list            # one row per terminal; * marks yours
runode list --json     # the same with every detail, for parsing
```

`REL` says where a terminal sits relative to yours (`left`, `right`, `up`,
`down`), `FG` is the program in front (`zsh`, `node`, `vim`...), `AGENT` and
`STATE` show an agent and whether it is `working`, `idle` or `blocked` (waiting
for an answer), `VIEW` is `shown`, `hidden` (another tab) or `bg` (in no
window).

Every command takes a SESSION that must match exactly one terminal; when it
matches several, runode lists them and fails, and you pick one by id.

Common forms: an id or any unique prefix of it, `self`, `left` `right` `up`
`down` (the pane next to yours), `agent:codex` or `agent:claude:idle`,
`title:server`, `cwd:app`. Every form, including tabs, windows and
workspaces, is in [references/sessions.md](references/sessions.md).

If there is no terminal to use, open one; it prints the new id:

```sh
id=$(runode open)                                    # a new tab after yours
id=$(runode open --right --cwd ~/src/app)            # split your pane
id=$(runode open --down -- npm run dev)              # and run a command in it
```

`--near SESSION` opens next to another terminal instead of yours; the new one
starts in that terminal's directory unless you give `--cwd`. Split a wide pane
`--right` and a narrow or tall one `--down`, and avoid splitting the same way
again and again until the panes are too thin to read.

`open` leaves the user's focus where it is unless you pass `--focus`. To show
the user a terminal later, `runode focus SESSION` brings its pane and window to
the front.

## Run a command and wait for it

```sh
runode send right 'cargo test' --enter --wait
```

`send` types the text, `--enter` presses Enter, and `--wait` waits for the
result. It picks how to wait and says so on stderr: for a shell prompt with
shell integration it waits for the command to finish, prints `exit N` and fails
with status 4 if N is not 0; for an agent it waits until the agent has worked
and stopped, and fails if the agent shows no activity within 10 seconds (Enter
may not have submitted the prompt, or it was a command that finishes at once);
otherwise until the screen has been quiet for 2 seconds.

When a wait fails or times out, the input may still have been delivered:
`runode read` the terminal before deciding what to send, and never send the
same prompt again blindly.

`--wait` is the reliable way to wait for something you just sent. A separate
`runode wait` only sees what happens after it starts: `--for command` waits for
the *next* command to finish, and `--new` ignores lines already on the screen,
so a command that finished before you started waiting is never seen. Use them
for things that are still to come:

```sh
runode wait right --for command --timeout 600     # next command finishes: exit N
runode wait right --for text 'Listening on' --new # a new line matches the regex
runode wait right --for quiet 3                   # screen unchanged for 3s
runode wait agent:codex --for done                # the agent worked, then stopped
runode wait agent:codex                           # the agent is not working now
```

Without `--for`, `wait` waits for `stopped`: the agent is idle, asking a
question, or gone. `--for working`, `idle` or `blocked` wait for that state.
`--for text` looks at the whole screen; `--lines N` widens it to the last N
lines, scrollback included.

`--timeout SECS` gives up with status 124. Status 3 means the terminal exited.
`--for command` needs shell integration; without it use `--for quiet` or
`--for text`.

## Read the output

```sh
runode read right --command       # output of the last command (needs shell integration)
runode read right --command 2     # the one before it
runode read right --lines 200     # the last 200 lines, scrollback included
runode read right                 # what is on the screen now
```

Prefer `--command` after running something: you get exactly that command's
output. When its start has scrolled away, runode says so on stderr.

## Control keys and pasting

```sh
runode send right --key ctrl-c                 # interrupt the running program
runode send right --paste "$(cat snippet.py)"  # paste instead of typing
```

Key names, repeats, the order one `send` uses and when to paste are in
[references/keys.md](references/keys.md).

## Typical workflows

Tests in the pane next to you, a dev server you check on later and a task for
another agent, as complete commands: [references/examples.md](references/examples.md).

## Work with several agents

When the user names agents ("use claude and pi", "ask codex too"), they mean
those programs running in runode terminals the user can watch, not subagents
of yours, even if one of them is the same agent as you. Start each one, give
it its task, and coordinate them yourself. A red team / blue team review, for
example: one agent attacks the change and lists problems, the other answers
each one (fix, or explain why it is not a problem), and you go back and forth
until they agree, then fix or let one of them fix. The blue team needs the red
team's list to start, so give it its task only once the list is there; a
"read ahead and wait for me" prompt only makes it look stuck while you wait.

```sh
red=$(runode open --right -- claude)
blue=$(runode open --near "$red" --down -- pi)
runode wait "$red" --for idle --timeout 60
runode wait "$blue" --for idle --timeout 60

# Ask for the report in a file: the screen holds only what fits on it.
runode send "$red" --paste 'Red team: review the uncommitted diff as an attacker.
Find bugs, security holes and broken edge cases. Write them to /tmp/red.md.' \
  --enter --wait --timeout 1800

runode send "$blue" --paste "Blue team: answer each finding in /tmp/red.md: fix
it, or say why it is not a problem. Write your answers to /tmp/blue.md." \
  --enter --wait --timeout 1800
```

- Wait for an agent to be `idle` before the first prompt. If it is `blocked`
  right away (a trust or login question), `runode read` it and answer, or ask
  the user.
- When agents really work at the same time (say each reviews half the
  change), run their `send ... --wait` in the background (`&`, then `wait`)
  so neither waits for the other.
- Long or multi-line prompts go with `--paste`; give file paths rather than
  pasting large content.
- Leave the agents' panes open when you are done: the user may want to read
  them or keep talking to them.

## iOS simulators and Android emulators

To build, run or test an app on a simulator or emulator, which the user watches
in runode's simulator page, use the runode-simulator skill.

## Be careful

- The other terminals belong to the user. Do not send keys or text to a
  terminal the user is working in, and never to `self`. Prefer a terminal you
  opened yourself, or ask the user which one to use.
- Check what is in front (`FG` in `runode list`, or `runode read`) before
  sending: text meant for a shell does damage when a REPL, an editor or an
  agent is in front.
- `ctrl-c` stops whatever runs there, and `runode kill` ends the terminal and
  closes its pane. Use them only on terminals you started or were asked to
  manage.
- Do not paste secrets: everything you send is visible on the user's screen.
- The app marks a terminal you operate on as driven by yours; the user sees
  it.
