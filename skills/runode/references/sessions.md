# Naming a session

Every runode command takes a SESSION that must match exactly one terminal; when
it matches several, runode lists them and fails, and you pick one by id.

| SESSION | means |
| --- | --- |
| `3fa9c2d1` | an id or any unique prefix of it, from `runode list` |
| `self` or `.` | your own terminal |
| `left` `right` `up` `down` | the pane next to yours |
| `next` `prev` | the next or previous pane in your tab |
| `pane:2` | pane 2 of your tab |
| `tab:3`, `tab:3.2` | tab 3 of your workspace (its focused pane, or pane 2) |
| `win:2/tab:1`, `ws:2/pane:1` | look in window 2 or workspace 2 |
| `title:server` | the title contains "server" (ignoring case) |
| `agent:codex`, `agent:claude:idle` | runs that agent, optionally in that state |
| `cwd:~/src/app`, `cwd:app` | works in that directory (or one named `app`) |

Positions count from 1 in the order the app shows them. Positional forms need
an open runode window; ids, titles, agents and directories always work.
