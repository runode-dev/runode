---
name: verify
description: 提交前把 runode 的改动真正跑起来验证：先做静态检查和相关单测，再在隔离的家目录里构建、启动开发版的宿主或桌面 app，用 runode 命令行走终端和看板的主流程，界面改动交给 cua-operator 点按截图，最后看日志、留证据、关干净。改了 apps/desktop、crates/ 下的任何代码或 skills/runode，准备提交、或者用户说「跑起来看看」「验证一下」时用；只改文档时不用。
---

# 验证 runode 的改动

这个 skill 管桌面 app、宿主和命令行（Rust 工作区，macOS 上跑）。手机端（`apps/ios`、`apps/android`）各有自己的构建和模拟器流程，不在这里。

最要紧的一条：**不碰正在用的那个 runode**。这个会话多半就跑在 runode 的终端里，环境里的 `RUNODE_SOCKET` 指向用户自己的宿主，直接敲 `runode`、`rn`、`target/debug/runode list` 都会连到它上面去。所以：

- 起 runode、跑命令行，一律经本目录的 `scripts/sandbox.sh`。它换掉 `HOME`（runode 的配置、数据、socket、日志都在 `$HOME/.runode` 下），去掉 `RUNODE_SOCKET`、`RUNODE_SESSION`、`RUNODE_BIN`，再起 `target/debug/runode`。
- 不用 `make run`：它用真的家目录，和装好的 app 共用配置文件、看板之外的数据和 agent skill 的安装位置。
- 不用 `make install`、`make restart`：它们替换 `/Applications/Runode.app`、让新宿主接手用户的会话，这个会话所在的终端也在里面。
- 只关自己起的进程。`sandbox.sh stop` 只关沙盒里的 app、拿着沙盒宿主锁的宿主（都要跑的是当前的可执行文件）和它们的后代；`ps` 里看到的别的 `runode --host`（`/Applications/Runode.app` 的，或者别人起的开发版）不要动。

## 第一步：静态检查和单测

先跑这些，过了再起 app。命令都在仓库根目录跑，后面的耗时是增量构建、缓存是热的时候量的。

| 什么时候 | 命令 | 耗时 |
| --- | --- | --- |
| 每次 | `make fmt-check` | 1 秒 |
| 每次 | `cargo check --workspace --all-targets` | 改动处往下的 crate 要重查，1～25 秒 |
| 每次 | `make clippy CLIPPY_ARGS="-- -D warnings"`（和 CI 一样，警告算失败） | 约 30 秒 |
| 改了哪个 crate | `cargo test -p runode-<目录名>`，比如 `cargo test -p runode-cli` | cli 约 30 秒 |
| 改了 `apps/desktop` | `cargo test -p runode --bin runode -- <模块名>::`，比如 `-- onboarding::`；desktop 的测试都在 `src` 里 | 编好后不到 1 秒 |
| 改了 `skills/` | `cargo test -p runode-agent-skill` | 1～3 秒 |
| 改了依赖、加了 crate | `make layers`（cargo-deny 和 `scripts/check-layers.py`） | 1 秒 |

包名是目录名加 `runode-` 前缀，桌面 app 的包名是 `runode`，`apps/android/tunnel` 是 `runode-android-tunnel`，`--workspace` 已经带上它。推送中转 `apps/push-relay` 是 TypeScript 写的 Worker，改了它在那个目录里跑 `npm run check` 和 `npm test`。

改了命令行的命令或选项时，按 AGENTS.md 的规矩，同一个提交里还要改 `cli` 的 `help` 模块和 `skills/runode/`，后者没有测试守着，自己对一遍。整套 `make test` 输出很长、要几分钟，交给 test-runner 跑，不在这里跑。

有新的 warning 就当失败处理。

## 第二步：构建

```sh
cargo build -p runode
```

产物是 `target/debug/runode`（认 `CARGO_TARGET_DIR`），桌面 app、宿主（`--host`）和命令行是同一个可执行文件。改了底层 crate 后增量构建约 1 分钟，没改动时 1 秒。前提：macOS、Xcode（含 Metal 工具链）、CONTRIBUTING.md「构建」一节写的那一版 zig、子模块已拉（`make submodules`）。在 `.worktrees/` 下的工作树里构建，子模块要在那个工作树里另拉一次。

## 第三步：在沙盒里启动

`scripts/sandbox.sh` 的几个子命令（不带参数运行看完整说明）。下面的 `$S` 指这个脚本；Bash 工具的每条命令不保留变量，`S=` 要和用到它的命令写在同一条里，或者直接写路径：

```sh
S=.claude/skills/verify/scripts/sandbox.sh
$S host     # 只起宿主（runode --host --stay），给它一个空工作区；不开窗口，约 1 秒就绪
$S app      # 起桌面 app，它自己拉起宿主；等到宿主连得上、日志里出现 open_window，约 1 秒
$S rn ...   # 在沙盒里跑一条命令行，比如 $S rn list
$S smoke    # 命令行主流程，见下一步
$S logs     # 日志末尾和其中的 ERROR、WARN、panic；有 ERROR 或 panic 时退出码为 1
$S stop     # 关 app、关宿主（连同它们起的 shell 和子进程），删沙盒目录；KEEP=1 $S stop 留着目录
```

- 沙盒目录默认 `/tmp/runode-verify-<工作树路径的短哈希>`，每个工作树各用一个，`$S status` 第一行打印它；可用 `RUNODE_VERIFY_HOME` 换（路径要短：socket 要放进 104 字节，别用 `$TMPDIR`）。目录里有脚本放的标记文件，已经存在、又不是脚本建的目录它拒绝使用，`stop` 也不删。同一个工作树里的几个会话共用一个沙盒，不要同时验。
- 宿主或 app 已经在跑时，`host`、`app`、`smoke` 先核对它是不是当前的可执行文件起的、起了以后有没有重新编过；改完代码重新 `cargo build` 后它们会报错，要先 `$S stop` 再起，不会对着旧宿主验出假通过。
- `stop` 不关别的开着沙盒文件的进程（比如 `KEEP=1` 后 cd 进去看日志的 shell），只把它们列出来。
- 就绪靠有上限的轮询（默认 20 秒，`READY_SECS` 可改），超时或进程提前退出时报错并提示看 `logs`。
- `app` 默认把引导页记成走完，直接进主窗口；要验引导页时用 `ONBOARDING=1 $S app`（只对全新的沙盒有效）。
- 会话里的 shell 是用户的登录 shell（`SHELL` 或账户记录里的），但家目录是沙盒，不读用户自己的 shell 配置。
- 宿主不跟着 app 退出，这是设计如此；所以不管验证成败，**最后一定跑 `$S stop`**，再用 `$S status` 确认宿主和 app 都没在跑。

## 第四步：走主流程

按改了哪块挑着走，改动跨几块就都走。

### 终端会话和命令行（`host`、`terminal`、`protocol`、`cli`、`agent`、`todo`、`tasks`）

```sh
$S host && $S smoke; $S logs; $S stop
```

`smoke` 依次做：`version`；`open` 开一个终端，`list --json` 里有它；等提示符停下后 `send ... --enter --wait` 打一条 `echo`，要打印 `exit 0`；`read --command` 读回那一行；再打一条 `false`，退出码要是 4、打印 `exit 1`；`todo add --no-worktree` 加任务、`todo list --json` 里有它、`todo rm` 删掉；`kill` 关掉终端，`list` 里没了它。全程约 2 秒，最后一行打印 `smoke 通过`，哪一步不对就打印哪一步、退出码 1。

改的命令不在 smoke 里时，照 `$S rn help <命令>` 的用法用 `$S rn` 驱动，检查输出和退出码（退出码的含义在 `runode help` 的末尾）。会话开在沙盒的工作区里，`$S rn open` 打印会话号，后面的命令拿它当 SESSION。

### 桌面界面（`apps/desktop`、`ui`、`config`、`preview`、`files`、`git` 的界面部分）

```sh
$S app && $S smoke      # 先确认 app 连着的宿主是好的
```

然后交给 cua-operator，告诉它：

- 目标窗口属于 `$S app` 打印的那个 pid（可执行文件 `target/debug/runode`，进程名 `runode`）。`/Applications/Runode.app` 的窗口是用户正在用的，只读不碰，按 pid 区分，不按应用名。
- 第一次读 AX 树只有窗口按钮和菜单栏（GPUI 在辅助工具第一次来问时才建树），再读一次才是完整的。
- 截图存到会话的 scratchpad 目录，不放进沙盒（`stop` 会删沙盒）。
- 不做不可逆操作：不开远程访问、不配对设备、不装登录自启、不装 agent skill、不点检查更新。

要它走的流程和每步的预期（按改动挑）：

1. 主窗口：一个工作区，目录是沙盒目录（侧栏里显示 `~`），一个终端标签，看得到 shell 提示符。在终端里输入 `echo verify-ui` 回车，下一行出现 `verify-ui`。
2. 命令行和界面联动：你在 shell 里跑 `$S rn open --right --no-fold`，cua-operator 看到当前标签右边多出一个分屏；`$S rn list` 里这一行 VIEW 为 `shown`。再 `$S rn kill <会话号>`，分屏消失。
3. 标签和工作区：从菜单开新标签、新建工作区（目录选沙盒目录），每次之后 `$S rn list` 多一行，WIN、WS、TAB 和界面一致。
4. 改到的那块界面：从菜单或侧栏打开设置页、看板、文件树、git 面板或预览，找到改动的元素，在 AX 树里核对它的名字、状态（按钮要有名字、`AXPress` 按得动、状态变了树里跟着变），改动前后各截一张。
5. 引导页（改了 `onboarding` 时）：`$S stop` 后用 `ONBOARDING=1 $S app` 重起，窗口里是引导页；走完或跳过后，`$S rn list` 正常，沙盒里的 `.runode/onboarding-dev` 内容变成 `done`。

界面走完后照样 `$S logs`、`$S stop`。

### 系统通知和 agent 的状态（`window::agents`、`alerts`、`agent` 的规则）

macOS 上只有 .app 里的 runode 发系统通知，裸的 `target/debug/runode` 不发。先 `make app`（约半分钟，产物在 `target/local/bundle/Runode.app`，不装进 /Applications），每条 `$S` 命令前都带上 `RUNODE_VERIFY_BIN=target/local/bundle/Runode.app/Contents/MacOS/runode`，`stop` 也一样。它不是 debug 构建，沙盒里的文件名不带 `-dev`（`host.sock`、`host.log`），脚本按可执行文件的路径自己换。它和装好的 Runode 是同一个 bundle id，发出的通知算在用户的 Runode 名下、用它的通知授权。

agent 的状态靠前台进程和屏幕认，不用起真的 agent：用 C 编一个叫 `claude` 的小程序（脚本不行，前台进程名会是 bash），打印 Claude Code 权限提问的样子（一条 `─` 横线，下面 `Do you want to proceed?`、`❯ 1. Yes`、`2. No`、`Esc to cancel · Tab to amend`）后 `pause()`，源文件和产物放 `/tmp` 下自己的目录、用完删掉。`$S rn open --tab -- <它的路径>` 开在不切过去的标签里，几秒后 `$S rn list` 里这一行 AGENT 是 `Claude Code`、STATE 是 `blocked`、VIEW 是 `hidden`。

通知发没发看两处：`screencapture -x` 截整屏，右上角有横幅；通知中心的库 `~/Library/Group Containers/group.com.apple.usernoted/db2/db` 里 `record` 表多一行（`app` 表里 `dev.runode.app` 的 `app_id`），把 `data` 列 `writefile` 出来用 `plutil -p` 看 `titl` 和 `body`。用户自己的 Runode 也往这里记，按标题和正文里的沙盒路径认自己那条。

### 这里验不到的

SSH 主机、远程访问和手机配对、远程桌面、语音输入、自动更新要真的网络、设备或权限，沙盒不覆盖。改了这些时跑对应 crate 的单测，在汇报里写明没有实地验证。改了 `ssh` 或 `desktop` 里连 SSH 主机的部分（`host_client` 的 ssh、bridge）时跑 `make test-ssh`：经假 ssh 在本机临时家目录里测部署、桥接、断线重连和换构建后接手会话，会先另编一份构建号不同的 runode（留在 `target/debug/runode-test-old`），再编回正常的，全程约 30 秒。

## 判定和证据

通过要同时满足：

- 第一步的检查全过，没有新的 warning；
- `smoke` 打印 `smoke 通过`（改了界面时在 `$S app` 下也跑一遍）；
- `$S logs` 退出码为 0（没有 ERROR、没有 panic），WARN 逐条看过、能解释；`stop` 之后 app 日志里的 `lost the connection to the host` 是关宿主引起的，不算；
- 界面改动有 cua-operator 的逐步结果和截图，每步都和预期一致；
- `$S stop` 之后 `$S status` 显示宿主、app 都没在跑。

汇报里贴的证据：检查命令的结论行、`smoke` 的输出、`logs` 里 ERROR/WARN 那一段、cua-operator 的截图路径。

失败时先看哪里：

- `sandbox.sh` 报「就绪前退出」「连不上」：`KEEP=1 $S stop` 留下目录，看 `$S logs` 列的三份日志：宿主的 `.runode/cache/host-dev.log`（打包的 .app 是 `host.log`）、app 的 `.verify/app.log`、只起宿主时的 `.verify/host.out`。
- `open` 报 `there is no workspace to open it in`：宿主没读到工作区。`$S host` 只在 `.runode/run/host-layout-dev.json` 不存在时写一个空工作区；换个新沙盒重来。
- `send --wait` 不打印 `exit N`、stderr 说在等屏幕安静：shell 集成没报到，看 `$S rn read <会话号>` 屏幕上是不是卡在别的提示（比如 shell 的首次配置向导）。
- `cargo build` 报找不到 libghostty 或 zig：子模块没拉，或 zig 版本不对，见 CONTRIBUTING.md 的「构建」。
