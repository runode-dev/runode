<p align="center">
  <img src="apps/desktop/assets/icon.png" alt="Runode" width="128">
</p>

<h1 align="center">Runode</h1>

<p align="center">
  给 AI 编程 agent 准备的 macOS 原生终端
  <br>
  <a href="#安装">下载</a>
  ·
  <a href="#快速上手">快速上手</a>
  ·
  <a href="CONTRIBUTING.md">参与开发</a>
  ·
  <a href="README.md">English</a>
</p>

<p align="center">
  <img src="docs/screenshot.png" alt="Runode 窗口：左边 Claude Code 在等回答，右上 Pi 已经干完，右下是 Pi 跑的测试" width="900">
</p>

## 简介

同时开几个 Claude Code、Codex，最累的是来回切终端，看哪个干完了、哪个在等你回答。Runode 不用配置任何 hook，自己就能认出每个终端里跑的是哪个 agent、处在什么状态，需要你的时候提醒你。agent 也能反过来操作旁边的终端，自己跑测试、读输出。出门在外，可以在手机上接着看。

Runode 用 [Ghostty](https://ghostty.org) 的 libghostty-vt 做终端仿真，用 [GPUI](https://www.gpui.rs) 绘制界面，全程 GPU 渲染。

## 特性

- **认得出 agent**：认出 Claude Code、Codex、Gemini CLI、Cursor、OpenCode、Amp 等二十多种 agent，在标签和侧栏上标出它在工作、在等你，还是干完了你还没看。
- **提醒与跳转**：你没在看时发通知、出提示音；`⌘⇧A` 列出所有窗口里的 agent，`⌘⌥A` 直接跳到下一个需要你的。
- **可编程**：自带 `runode` 命令行，能列出、读取、操作每个终端。agent 能用它在旁边的窗格跑命令、等结果、读输出，或者指挥另一个 agent。
  <br><img src="docs/agents.gif" alt="两个 Claude Code 窗格：左边在问问题，右边在下方新窗格里跑 cargo test 并总结结果" width="900">
- **会话常驻**：打开 `terminal-host` 后，会话由独立的宿主进程持有，退出、重开、升级 app 都不断。
- **手机远程**：iOS app 和电脑配对，连接用 TLS 1.3 加密。在同一个网络里直连，不在时经 [iroh](https://iroh.computer) 按公钥连过来，能打洞就直连，打不通走中继。在手机上能看终端、回复 agent、跑命令、提交 git。
  <br><img src="docs/mobile.gif" alt="iOS app：Claude Code 在等回答、Codex 在干活，接着在 Git 页看 diff、暂存一个文件并提交" width="300">
- **远程桌面**：在手机上看到并操控整个 Mac 屏幕，走同一条连接，用硬件 HEVC 编码，延迟低。能切换显示器，电脑锁屏时用存着的密码解锁。
  <br><img src="docs/remote-desktop.gif" alt="iOS app 横屏显示 Mac 的屏幕：点按打开 Runode 的侧栏、新标签和看板" width="900">
- **语音输入**：在 Mac 和手机上都能说话输入到终端。默认用本地模型离线识别，也可以交给云端识别、用大模型润色。
- **看板**：给 agent 派活的看板。到了定的时间自动开终端、起 agent 做任务，任务的 PR 合并后自动标完成。agent 也能用 `runode todo`、`runode schedule` 操作它。
  <br><img src="docs/kanban.gif" alt="看板：新加一个任务，启动两个任务，它们挪到需要你，其中一个标为完成" width="900">
- **项目面板**：文件树、带语法高亮的文件预览，加上 Git 面板，能按块暂存、提交、切分支、推拉。
  <br><img src="docs/panels.gif" alt="文件树、Markdown 和 Rust 文件预览，以及 Git 面板暂存一块改动并提交" width="900">
- **兼容 Ghostty**：直接读你现有的 Ghostty 配置和主题。
- **原生**：Rust 写成，不是 Electron。界面有简体中文、繁体中文和英文，支持自动更新。

## 安装

在终端里运行：

```sh
curl -fsSL https://raw.githubusercontent.com/runode-dev/runode/main/scripts/install.sh | sh
```

它会把 Runode.app 装进「应用程序」，并把 `runode` 命令行链接到 PATH 上能写的 bin 目录（`~/.local/bin`、`~/bin`、`/opt/homebrew/bin`、`/usr/local/bin` 里第一个，都不行就用 `~/.local/bin`）。也可以手动下载对应芯片的 dmg：[Apple 芯片](https://github.com/runode-dev/runode/releases/latest/download/Runode-arm64.dmg) · [Intel](https://github.com/runode-dev/runode/releases/latest/download/Runode-x86_64.dmg)。打开后把 Runode 拖进「应用程序」。安装包用 Developer ID 签名并经过 Apple 公证，之后会自动更新。

历次版本见 [Releases](https://github.com/runode-dev/runode/releases)。iOS app 还没上架，需要[从源码构建](CONTRIBUTING.md)。

## 快速上手

装上 runode 的 skill，把用法教给你的 agent（Claude Code、Codex 等都能用）：

```sh
npx skills add runode-dev/runode --skill runode -g
npx skills update runode -g    # 以后更新到最新
```

之后 agent 会在需要时自己操作旁边的终端。你也可以亲手用：

```sh
runode list                                    # 列出所有终端和里面的 agent
runode send right 'cargo test' --enter --wait  # 在右边的窗格跑测试，等它结束
runode read right --command                    # 读回这条命令的输出
runode remote pair                             # 显示二维码，和手机配对
```

完整用法见 `runode help`。配置文件在 `~/.runode/config.conf`。`runode completions bash|fish|zsh` 打印 `runode` 和 `rn` 的 Tab 补全脚本，装到哪见 `runode help completions`。

## 常见问题

**和 Ghostty 是什么关系？**
Runode 不是 Ghostty 的分支。它把 libghostty-vt 当作终端仿真的库来用，界面、分屏、agent 识别都是自己做的。

**支持哪些 agent？**
任何在终端里跑的 agent 都能用。其中二十多种能认出状态，包括 Claude Code、Codex、Gemini CLI、Cursor、OpenCode、Amp、GitHub Copilot、Kimi、Qwen Code 等。识别规则可以自己加。

**支持哪些平台？**
macOS（Apple 芯片和 Intel 都支持），另有 iOS app 做远程访问。Linux（x86_64 和 aarch64）上能用安装脚本装命令行和终端宿主，没有桌面界面，用手机经远程访问连上来。桌面 app、终端宿主和命令行也能在 Windows 上从源码构建运行，还没有发布 Windows 版。

## 参与开发

欢迎提 issue 和 PR。构建方法见 [CONTRIBUTING.md](CONTRIBUTING.md)，代码结构和约定见 [AGENTS.md](AGENTS.md)。

## 许可

[Apache-2.0](LICENSE)
