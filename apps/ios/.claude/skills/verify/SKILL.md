---
name: verify
description: 提交前把 iOS app 的改动真正跑起来验证：先跑 RunodeKit 的 swift test 和 Runode.xctestplan，再在专用模拟器上调试构建、启动，用演示模式或连上沙盒宿主走主流程，界面点按交给 mobile-operator，最后看日志、留证据、关干净。改了 apps/ios（Runode、RunodeKit、工程和脚本）或手机端解的协议样例（crates/protocol、crates/agent 的 tests/fixtures），准备提交、或者用户说「在模拟器上跑起来看看」时用；只改文档时不用。
---

# 验证 iOS app 的改动

这个 skill 管 `apps/ios`：App 入口 `Runode/` 和其余全部代码所在的 Swift 包 `RunodeKit`。桌面 app、宿主和命令行用仓库根目录的 verify skill，Android 端在 `apps/android` 里。

最要紧的两条：

- **只用专用模拟器**。别的模拟器上装着的 Runode 可能已经和用户自己的电脑配过对，在那里启动、点按会连到用户真的终端上去。所以不用 `make run-ios`（它挑开着的那台或第一台 iPhone），一律经本目录的 `scripts/sim.sh`：它只认名叫 `Runode Verify` 的那台模拟器，没有就建一台留着复用，每次 `up` 都把上面的 Runode 卸掉重装，只会连这次起的沙盒宿主。别的模拟器上的 app 和数据不碰。
- **宿主一律在沙盒里起**。要连电脑时用 `sim.sh host`，它经根目录 verify skill 的 `sandbox.sh` 起开发版宿主（换掉 `HOME`，沙盒目录和桌面验证的分开），远程访问只听 `127.0.0.1`、端口另挑一个空闲的，不往局域网公布。不在沙盒外面跑 `runode remote pair`，不拿用户自己的 runode 配对。

下面的 `$I` 指 `apps/ios/.claude/skills/verify/scripts/sim.sh`（不带参数运行看完整说明）。Bash 工具的每条命令不保留变量，`I=` 要和用到它的命令写在同一条里，或者直接写路径。命令都在仓库根目录跑，耗时是缓存热的时候量的。

## 第一步：静态检查和单测

| 什么时候 | 命令 | 耗时 |
| --- | --- | --- |
| 每次 | `swift test --package-path apps/ios/RunodeKit`（和 CI 一样，在 Mac 上跑，编不到 `#if os(iOS)` 里的界面代码） | 约 6 秒 |
| 只想先跑一块 | 上面加 `--filter RunodeProtocolTests`，target 名见 `RunodeKit` 的 `Package.swift` | 1 秒起 |
| 改了 `Runode/`、视图或任何 `#if os(iOS)` 的代码，提交前 | 下面的测试计划，在专用模拟器上跑 | 约 50 秒 |
| 改了界面文字 | `apps/ios/scripts/check-strings.sh`，打印「翻译齐全」才算过 | 编过一次后约 2 秒，否则约 20 秒 |
| 改了 `crates/protocol`、`crates/agent` 里手机端也解的格式 | `cargo test -p runode-protocol -p runode-agent`，再跑 `swift test`。`messages.json`、`launchers.json` 由 Rust 的类型生成，格式是有意改的就设 `RUNODE_BLESS=1` 重跑那个测试写出新样例；推送、远程访问等其余样例照规格手写，对不上时改回代码 | protocol 的样例测试约 3 秒 |

测试计划 `Runode.xctestplan` 跑 App 的 `RunodeTests` 和 RunodeKit 的四个测试 target，用模拟器的系统：

```sh
I=apps/ios/.claude/skills/verify/scripts/sim.sh
rm -rf /tmp/runode-ios-tests.xcresult
xcodebuild test -project apps/ios/Runode.xcodeproj -scheme Runode -testPlan Runode \
  -destination "platform=iOS Simulator,id=$($I device)" -derivedDataPath apps/ios/build/dd-test \
  -resultBundlePath /tmp/runode-ios-tests.xcresult -quiet
xcrun xcresulttool get test-results summary --path /tmp/runode-ios-tests.xcresult   # 看 result、totalTestCount、failedTests
rm -rf /tmp/runode-ios-tests.xcresult
```

- `$I device` 打印专用模拟器的 UDID，没有就建一台（不开机）。别把 destination 换成别的模拟器：测试要往上装 App。
- xcodebuild 自己开机、跑完自己关机，不用 `down`。`-only-testing:RunodeTests` 这类只跑一部分时，大头是编译和开机，省不了多少时间，直接跑全部。
- `RunodeSpeechTests` 不在测试计划里，只由 `swift test` 在 Mac 上跑（Mac 上识别本身报 unsupported）。

`-quiet` 下 xcodebuild 只打印警告和错误，有新的 warning 就当失败处理。整套 Rust 测试不在这里跑。

## 第二步：构建

`$I up` 里调 `apps/ios/scripts/run-simulator.sh`（`make run-ios` 背后的脚本）做 Debug 构建，派生数据在 `apps/ios/build/dd`，不用单独构建。前提：

- Xcode，并在 Xcode 的设置里装好 iOS 平台（模拟器系统）；手机端照最新的 Xcode 写，老版本编不过。
- libghostty-vt 的 xcframework：`apps/ios/Frameworks/ghostty-vt.xcframework` 不在时脚本自己调 `apps/ios/scripts/build-ghostty-vt.sh` 编，要 `vendor/ghostty` 子模块（`git submodule update --init vendor/ghostty`）和 zig（版本见 CONTRIBUTING.md）。它只在缺的时候编：`vendor/ghostty` 更新过就自己重跑一次 `build-ghostty-vt.sh`。新的工作树里这些都要重来一遍。
- 第一次构建要联网，SwiftPM 拉 iroh-ffi 和 sherpa-onnx 的预编译包。
- 要连宿主时另要 `cargo build -p runode`（产物 `target/debug/runode`，前提见根目录 verify skill）。宿主起着以后又重新编过，`sandbox.sh` 会报错要先停，`$I down` 再起。

## 第三步：启动和收尾

```sh
I=apps/ios/.claude/skills/verify/scripts/sim.sh
$I up                          # 专用模拟器开机，卸掉 Runode，构建、装上、启动，等 app 连上界面
$I launch [ARG...]             # 带参数重启 app，比如 $I launch -runode-demo terminal
$I host                        # 起沙盒宿主，开只听 127.0.0.1 的远程访问
$I pair                        # 不带参数重启 app，跑 runode remote pair，把配对链接交给 app
$I paired                      # 等界面上点完配对，打印配上的设备
$I sandbox rn ...              # 在沙盒宿主上跑命令行，比如 $I sandbox rn list；也能 $I sandbox smoke、$I sandbox logs
$I status                      # 模拟器、app、pair、沙盒宿主各在什么状态
$I shot /tmp/x.png             # 模拟器截图
$I logs                        # app 日志和崩溃报告；有崩溃报告时退出码为 1
$I down                        # 停 pair、关沙盒宿主并删沙盒、关 app，模拟器原本关着就关机，删状态目录
```

- 耗时：没改代码时 `up` 约 40 秒，大头是 xcodebuild；第一次建模拟器那回开机要多等几十秒，再加上整个构建。`launch` 约 3 秒，`host` 加 `pair` 约 4 秒，`down` 约 2 秒。
- 「连上界面」指 SpringBoard 的日志里出现了这个进程的场景连上，靠有上限的轮询（`READY_SECS`），进程先退出或超时都报错。这时首页刚开始画，演示数据和宿主的会话要再过一会儿才出来，截图或交给 mobile-operator 时让它等内容出现，不要据此判断空白。
- `up` 时模拟器窗口（Xcode 里的 DeviceHub）会打开；`down` 时它原本没开、也没有别的模拟器开着就一起关掉。
- 专用模拟器同一时间只给一个工作树用：一个工作树 `up` 之后、`down` 之前，另一个工作树跑 `up`、`launch`、`pair`、`paired`、`status`、`logs`、`shot`、`down` 都报错并说出是哪个工作树在用，不会动到那边的 app、pair 和状态。
- 不管验证成败，**最后一定跑 `$I down`**，再用 `$I status` 确认 app、pair、宿主都没在跑。专用模拟器本身留着，下次开机只要几秒；不想要了用 `xcrun simctl delete "$($I device)"` 删掉。

## 第四步：走主流程

按改了哪块挑着走，改动跨几块就都走。界面上的点按、输入、截图都交给 mobile-operator，告诉它：

- 设备是 `$I up` 打印的那台 `Runode Verify`（按 UDID 选），bundle id `dev.runode.mobile`。别的模拟器只读不碰。
- 截图存到会话的 scratchpad，每条流程改动的那一屏截一张；要你亲眼看的再点名。
- 屏幕内容要等加载出来再判断；系统弹「在“Runode”中打开？」、通知权限这类提示，按流程里写的点。
- 不点「远程桌面」（会采集这台 Mac 的真屏幕，可能弹系统的录屏授权），不下载语音识别模型，不在设置里改推送中转。

### 演示模式：界面（`RunodeFeatures` 的视图和视图模型、`Runode/`、主题、文字）

调试构建带 `-runode-demo` 启动时不连电脑，换成本地的假数据（`DemoComposition`），不用起宿主。再带一个词直接打开某一页：

| 启动 | 预期 |
| --- | --- |
| `$I launch -runode-demo` | 首页：顶上「1 个会话在等你」和一张等回答的卡片（选项 1、2、3）；「电脑」里一台在线的、一台 `homelab` 连不上带「重试」；底部「配对电脑」「新开终端」 |
| `$I launch -runode-demo list` | 演示电脑的会话列表：等你回答的 Claude、在干活的 Codex、普通的 zsh |
| `$I launch -runode-demo terminal` | Claude 的终端页：屏幕上是 Claude 在问要不要跑命令，底部「等你回答」和 1、2、3 按钮，下面一排 esc、ctrl、tab、方向键 |
| `$I launch -runode-demo shell` | 普通 shell 的终端页 |
| `$I launch -runode-demo git` | Git 页：分支 `main`、拉取推送同步按钮、提交说明、已暂存和改动两组文件 |
| `$I launch -runode-demo pair` | 配对页（演示模式下配不上，只看界面） |
| `$I launch -runode-demo settings` | 设置页 |

再带 `offline` 时连上一会儿后假装断线，看断线和重连的界面；带 `light` 时终端用浅色主题。让 mobile-operator 在对应那页找到改动的元素，核对文字、状态和可点性（按钮有无障碍名字、点了有反应），操作前后各截一张。改了界面文字时，再用英文和繁体各启动一次看改到的那页，比如 `$I launch -runode-demo settings -AppleLanguages '(en)'`、`'(zh-Hant)'`（只对这次启动生效，演示数据也跟着换语言）。

### 连真宿主：配对、会话列表、终端（`RunodeConnection`、`RunodeProtocol`、`RunodeTerminal`、终端页和会话列表的模型）

```sh
I=apps/ios/.claude/skills/verify/scripts/sim.sh
$I up && $I host && $I sandbox smoke    # 先确认宿主是好的，smoke 打印「smoke 通过」
$I sandbox rn open                      # 开一个终端，记下打印的会话号 SESSION
$I pair                                 # 把配对链接交给 app
```

交给 mobile-operator：

1. 配对：系统弹「在“Runode”中打开？」点「打开」；弹出「和这台电脑配对？」，卡片上是 `runode-verify` 和 `127.0.0.1:<端口>`，点「配对」；出现「配好了」「已和 runode-verify 配对」，点「去看会话」。这时你跑 `$I paired`，要打印 `Paired with ...`，下面的设备表里有这台手机。
2. 会话列表：`runode-verify` 的列表里有刚开的那个终端（目录是沙盒目录）。
3. 手机到电脑：打开那个终端，等提示符出来，用屏幕键盘输入 `echo verify-ios` 回车，屏幕上下一行是 `verify-ios`。你跑 `$I sandbox rn read SESSION | grep verify-ios`，要读得到。
4. 电脑到手机：你跑 `$I sandbox rn send SESSION "echo from-host" --enter`，手机终端上出现 `from-host`。
5. 新开和结束：在会话列表点「新建」里的「新开终端」，`$I sandbox rn list` 多一行；你跑 `$I sandbox rn kill SESSION`，手机列表里那一行消失。
6. 删除电脑（改了配对或电脑列表时）：首页长按 `runode-verify` 选「删除」并确认，你跑 `$I sandbox rn remote devices`，里面没有这台手机了。

走完照样 `$I logs`、`$I sandbox logs`、`$I down`。

### 这里验不到的

真机（`make run-ios-device`，要连着的 iPhone 和开发者账号的签名）、推送（模拟器拿不到 APNs 的 device token）、跨网络经 iroh 连（沙盒只听本机回环）、扫码（模拟器没有相机，配对一律走链接）、远程桌面和语音识别要真设备或真屏幕。改了这些跑对应 target 的单测，在汇报里写明没有实地验证。

## 判定和证据

通过要同时满足：

- 第一步的检查全过（测试计划的 `failedTests` 为 0，`check-strings.sh` 打印「翻译齐全」），没有新的 warning；
- `$I up` 和每次 `launch` 都打印「app 就绪」；
- 界面改动有 mobile-operator 的逐步结果和截图，每步都和预期一致；连宿主的流程里 `$I paired`、`rn read` 这些电脑这边的核对也对得上；
- `$I logs` 退出码为 0（没有崩溃报告），列出的 app 日志逐条看过、能解释；连了宿主时 `$I sandbox logs` 退出码也为 0；
- `$I down` 之后 `$I status` 显示 app、pair、宿主都没在跑。

`$I logs` 看的是从最近一次 `up` 起 App 自己的日志（subsystem `dev.runode.mobile`）、Runode 进程的 fault，以及 Mac 的 `~/Library/Logs/DiagnosticReports` 里 bundleID 是 `dev.runode.mobile` 的崩溃报告。Runode 进程的 error 级日志大多是系统框架的噪音，要看时自己查：

```sh
xcrun simctl spawn "$($I device)" log show --last 5m --style compact --predicate 'process == "Runode" AND messageType IN {16, 17}'
```

汇报里贴的证据：检查命令的结论行（swift test 每个包的 `Test run with N tests ... passed`、测试计划的 summary）、`up` 和 `launch` 的就绪行、`paired` 的输出、`logs` 的输出、mobile-operator 的截图路径。

失败时先看哪里：

- `up` 构建失败：它打印构建输出的末尾和完整日志的路径（状态目录里，`down` 时一起删）；还看不明白就去掉 `-quiet` 手动跑 `IOS_SIM="$($I device)" apps/ios/scripts/run-simulator.sh` 看完整报错；报找不到 ghostty 的头文件或 xcframework 时重跑 `apps/ios/scripts/build-ghostty-vt.sh`。
- 「在连上界面前退出了」：多半是启动就崩，等几秒跑 `$I logs` 看崩溃报告（报告写出来有延迟），打开 `.ips` 看 `exception` 和崩溃线程的栈。
- 「还没连上界面」：`$I shot` 截图看停在哪，常见是系统弹窗挡着。
- `paired` 超时：截图看手机停在哪一步；`$I status` 看 pair 还在不在等（配对口令过期就重跑 `$I pair`）；手机报连不上时看 `$I sandbox logs` 里宿主的远程访问日志，和 `$I host` 打印的端口对不对得上。
- 手机上看不到会话或终端不动：先 `$I sandbox rn list`、`$I sandbox rn read SESSION` 确认宿主那边是好的，再看 `$I sandbox logs`。
