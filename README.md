<p align="center">
  <img src="apps/desktop/assets/icon.png" alt="Runode" width="128">
</p>

<h1 align="center">Runode</h1>

<p align="center">
  A native macOS terminal built for AI coding agents
  <br>
  <a href="#install">Download</a>
  ·
  <a href="#quick-start">Quick start</a>
  ·
  <a href="CONTRIBUTING.md">Contributing</a>
  ·
  <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <img src="docs/screenshot.png" alt="Runode with Claude Code waiting for an answer, Pi done, and the tests Pi ran in the pane below" width="900">
</p>

## About

Running several Claude Code and Codex sessions at once means constantly switching terminals to see which one finished and which one is waiting for you. Runode recognizes the agent in every terminal and tracks its state with no hooks to configure, and lets you know when one needs you. Agents can drive the terminals next to them too, running tests and reading the output themselves. When you're away from your desk, pick up where you left off on your phone.

Runode uses [Ghostty](https://ghostty.org)'s libghostty-vt for terminal emulation and [GPUI](https://www.gpui.rs) for a fully GPU-rendered UI.

## Features

- **Agent-aware**: recognizes 20+ agents, including Claude Code, Codex, Gemini CLI, Cursor, OpenCode and Amp, and marks each tab and sidebar entry as working, waiting for you, or done but unread.
- **Notifications and jumping**: notifies you and plays a sound when you're not looking. `⌘⇧A` lists the agents in every window, and `⌘⌥A` jumps straight to the next one that needs you.
- **Scriptable**: the built-in `runode` CLI lists, reads and drives every terminal. Agents use it to run commands in a neighbouring pane, wait for them, read the output, or direct another agent.
  <br><img src="docs/agents.gif" alt="Two Claude Code panes: the left one asks a question, the right one runs cargo test in a new pane below and summarizes it" width="900">
- **Persistent sessions**: with `terminal-host` on, sessions live in a separate host process and survive quitting, relaunching and upgrading the app.
- **Remote from your phone**: the iOS app pairs with your Mac over a TLS 1.3 connection. On the same network it connects directly; elsewhere it reaches your Mac through [iroh](https://iroh.computer) by public key, punching a direct path and falling back to a relay. From your phone you can watch terminals, answer agents, run tasks and commit to git.
  <br><img src="docs/mobile.gif" alt="iOS app: Claude Code waits for an answer while Codex works, then the Git page shows a diff, stages a file and commits" width="300">
- **Remote desktop**: see and control your Mac's whole screen from your phone over the same connection, with low-latency hardware HEVC. It can switch displays and unlock a locked screen with a password you saved.
  <br><img src="docs/remote-desktop.gif" alt="iOS app in landscape showing a Mac's screen: taps open Runode's sidebar, a new tab and the task board" width="900">
- **Voice input**: dictate into the terminal on your Mac or your phone. Speech is recognized offline with a local model; cloud recognition and polishing the text with an LLM are optional.
- **Task board**: a kanban board for agent work. A schedule opens a terminal and starts an agent on a task at the time you set, and a task whose PR gets merged is marked done. Agents use it too, through `runode todo` and `runode schedule`.
  <br><img src="docs/kanban.gif" alt="Task board: a new task is added, two tasks start, move to Needs You, and one is marked done" width="900">
- **Project panels**: a file tree, syntax-highlighted file previews, and a Git panel that can stage hunks, commit, switch branches, and pull or push.
  <br><img src="docs/panels.gif" alt="File tree, Markdown and Rust previews, and the Git panel staging a hunk and committing" width="900">
- **Ghostty compatible**: reads your existing Ghostty config and themes.
- **Native**: written in Rust, not Electron. The UI comes in English, Simplified Chinese and Traditional Chinese, and the app updates itself.

## Install

Run this in a terminal:

```sh
curl -fsSL https://raw.githubusercontent.com/runode-dev/runode/main/scripts/install.sh | sh
```

It installs Runode.app into Applications and links the `runode` command line into the first writable bin directory on your PATH (`~/.local/bin`, `~/bin`, `/opt/homebrew/bin`, `/usr/local/bin`; falls back to `~/.local/bin`). You can also download the dmg for your Mac yourself from the [latest release](https://github.com/runode-dev/runode/releases/latest). Open it and drag Runode into Applications. It is signed with a Developer ID, notarized by Apple, and keeps itself up to date.

Every version is on [Releases](https://github.com/runode-dev/runode/releases). The iOS app isn't on the App Store yet; [build it from source](CONTRIBUTING.md).

## Quick start

Teach your agents how to use runode by installing its skill (works with Claude Code, Codex and other agents):

```sh
npx skills add runode-dev/runode --skill runode -g
npx skills update runode -g    # later, to get the latest version
```

From then on the agent drives neighbouring terminals when it needs to. You can use it by hand as well:

```sh
runode list                                    # list every terminal and the agent in it
runode send right 'cargo test' --enter --wait  # run tests in the pane to the right and wait
runode read right --command                    # read back that command's output
runode remote pair                             # show a QR code to pair your phone
```

Run `runode help` for the full reference. The config file lives at `~/.runode/config.conf`. Tab completion for `runode` and `rn` comes from `runode completions bash|fish|zsh` (see `runode help completions` for where to put it).

## FAQ

**How does Runode relate to Ghostty?**
Runode is not a fork of Ghostty. It uses libghostty-vt as a terminal emulation library; the UI, splits and agent detection are its own.

**Which agents does it work with?**
Any agent that runs in a terminal. More than 20 have their state recognized, including Claude Code, Codex, Gemini CLI, Cursor, OpenCode, Amp, GitHub Copilot, Kimi and Qwen Code, and you can add your own detection rules.

**Which platforms are supported?**
macOS on both Apple silicon and Intel, plus an iOS app for remote access. On Linux (x86_64 and aarch64) the install script sets up the command line and the terminal host without a desktop UI; connect to it from your phone over remote access. On Windows (x86_64) download `Runode-<version>-windows-x86_64-setup.exe` from the [latest release](https://github.com/runode-dev/runode/releases/latest); it installs per user and puts the `rn` command line on your PATH. The installer is signed with a self-signed certificate, so SmartScreen warns on the first run.

## Contributing

Issues and PRs are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for building and [AGENTS.md](AGENTS.md) for the code layout and conventions.

## License

[Apache-2.0](LICENSE)
