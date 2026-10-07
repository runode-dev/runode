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
  <a href="README.md">简体中文</a>
</p>

## About

Running several Claude Code and Codex sessions at once means constantly switching terminals to see which one finished and which one is waiting for you. Runode recognizes the agent in every terminal and tracks its state with no hooks to configure, and lets you know when one needs you. Agents can drive the terminals next to them too, running tests and reading the output themselves. When you're away from your desk, pick up where you left off on your phone.

Runode uses [Ghostty](https://ghostty.org)'s libghostty-vt for terminal emulation and [GPUI](https://www.gpui.rs) for a fully GPU-rendered UI.

## Features

- **Agent-aware**: recognizes 20+ agents, including Claude Code, Codex, Gemini CLI, Cursor, OpenCode and Amp, and marks each tab and sidebar entry as working, waiting for you, or done but unread.
- **Notifications and jumping**: notifies you and plays a sound when you're not looking. `⌘⇧A` lists the agents in every window, and `⌘⌥A` jumps straight to the next one that needs you.
- **Scriptable**: the built-in `runode` CLI lists, reads and drives every terminal. Agents use it to run commands in a neighbouring pane, wait for them, read the output, or direct another agent.
- **Persistent sessions**: with `terminal-host` on, sessions live in a separate host process and survive quitting, relaunching and upgrading the app.
- **Remote from your phone**: the iOS app pairs with your Mac over the local network through a TLS 1.3 connection. From your phone you can watch terminals, answer agents, run project tasks and commit to git.
- **Project panels**: a file tree, syntax-highlighted file previews, and a VS Code style Git panel that can stage hunks, commit, switch branches, and pull or push.
- **A smarter prompt**: Tab completion with descriptions, live syntax highlighting of what you type, and inline suggestions from your history.
- **Ghostty compatible**: reads your existing Ghostty config and themes.
- **Native**: written in Rust, not Electron. The UI comes in English, Simplified Chinese and Traditional Chinese, and the app updates itself.

## Install

Download the dmg for your Mac: [Apple silicon](https://github.com/runode-dev/runode/releases/latest/download/Runode-arm64.dmg) · [Intel](https://github.com/runode-dev/runode/releases/latest/download/Runode-x86_64.dmg). Open it and drag Runode into Applications. It is signed with a Developer ID, notarized by Apple, and keeps itself up to date.

Every version is on [Releases](https://github.com/runode-dev/runode/releases). The iOS app isn't on the App Store yet; [build it from source](CONTRIBUTING.md).

## Quick start

Inside a Runode terminal, teach your agent how to use runode:

```sh
runode setup claude    # or: runode setup codex
```

From then on the agent drives neighbouring terminals when it needs to. You can use it by hand as well:

```sh
runode list                                    # list every terminal and the agent in it
runode send right 'cargo test' --enter --wait  # run tests in the pane to the right and wait
runode read right --command                    # read back that command's output
runode remote pair                             # show a QR code to pair your phone
```

Run `runode help` for the full reference. The config file lives at `~/.config/runode/config.conf`.

## FAQ

**How does Runode relate to Ghostty?**
Runode is not a fork of Ghostty. It uses libghostty-vt as a terminal emulation library; the UI, splits and agent detection are its own.

**Which agents does it work with?**
Any agent that runs in a terminal. More than 20 have their state recognized, including Claude Code, Codex, Gemini CLI, Cursor, OpenCode, Amp, GitHub Copilot, Kimi and Qwen Code, and you can add your own detection rules.

**Which platforms are supported?**
macOS on both Apple silicon and Intel, plus an iOS app for remote access.

## Contributing

Issues and PRs are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for building and [AGENTS.md](AGENTS.md) for the code layout and conventions.

## License

[Apache-2.0](LICENSE)
