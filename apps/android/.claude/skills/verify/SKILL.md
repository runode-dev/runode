---
name: verify
description: 提交前把 Android app 的改动真正跑起来验证：先做单测和 lint，再查三份原生库要不要重编、构建调试包，装到模拟器上启动，连上沙盒里的宿主走配对、会话列表和终端这几条主流程，界面点按交给 mobile-operator，最后看 logcat 和宿主日志、收干净。改了 apps/android 下的任何代码（Kotlin 模块、资源、Gradle 配置、拨号库 tunnel），准备提交、或者用户说「在安卓上跑起来看看」「验证一下 Android」时用；只改文档时不用。
---

# 验证 Android app 的改动

这个 skill 管 `apps/android`：Gradle 工程里的 `protocol`、`connection`、`terminal`、`speech`、`features`、`app` 六个模块，和经 JNI 加载的 Rust 拨号库 `apps/android/tunnel`。电脑那一端（宿主、命令行）的改动照仓库根目录的 verify skill 验；iOS 在 `apps/ios` 自己的 skill 里。

几条规矩：

- **宿主只用沙盒里的。** app 要连的宿主一律经本目录的 `scripts/android.sh host` 起，它交给根目录 verify skill 的 `sandbox.sh`（换掉 `HOME`，不碰用户正在用的 `~/.runode`）。远程访问只听 `127.0.0.1`，端口用 `REMOTE_PORT`（默认和正式版的不同），局域网里看不到它。不要让 app 去配对用户自己的电脑。
- **只用模拟器。** 脚本只认 `emulator-*`，连着的真机不碰。真机验证要用户自己连好手机、点头之后另说，不是默认做法。
- **不动模拟器上已有的东西。** 不卸载、不 `pm clear`、不清 logcat（日志按时间过滤）；`installDebug` 是覆盖安装，app 原有的数据留着。配对会往 app 的电脑列表里加一台，`stop` 时还原成配对前的样子。模拟器由脚本起时不读写快照，AVD 平时的快速启动存档不受影响。
- **自己起的自己关。** 不管验证成败，最后一定跑 `android.sh stop`。

下面的 `$A` 指 `apps/android/.claude/skills/verify/scripts/android.sh`。Bash 工具的每条命令不保留变量，`A=` 要和用到它的命令写在同一条里，或者直接写路径。

## 第一步：单测和 lint

在 `apps/android` 里跑。耗时是 Gradle daemon 已起、缓存是热的时候量的；第一次起 daemon 多十秒左右。

| 什么时候 | 命令 | 耗时 |
| --- | --- | --- |
| 每次 | `./gradlew test`（就是根目录的 `make android-test`） | 没改动时 1 秒；测试本身全跑一遍约 5 秒，加上改动模块的重编 |
| 只改了一个模块 | `./gradlew :features:testDebugUnitTest`；`protocol` 是纯 JVM 模块，用 `:protocol:test`；只跑一个类加 `--tests '*PairingModelTests'` | 几秒 |
| 每次（提交前） | `./gradlew check`：模块依赖方向（`checkLayers`）、Android lint、单测，和 CI 一样 | 冷的约 3 分钟，之后增量几秒 |
| `check` 之后 | `$A lint-changed`：从 lint 报告里挑出落在改动文件上的问题 | 不到 1 秒 |
| 改了 `apps/android/tunnel` | 在仓库根目录跑 `cargo test -p runode-android-tunnel --locked`，和下面的 clippy | 测试 1～2 秒 |

lint 报告（`<模块>/build/reports/lint-results-debug.txt`）里本来就有一批 warning，看总数判断不了；`lint-changed` 只列改动的文件（相对 HEAD，含未跟踪的）上的问题，有就退出码为 1，报告比改动旧时会提醒重跑。lint 的 error（比如译文缺了、多了的 `MissingTranslation`、`ExtraTranslation`）直接让 `check` 失败。Kotlin 编译的 `w:` 行也算 warning。新的 warning 一律当失败处理。

拨号库的 clippy 要按 Android 目标跑才走得到 `cfg(target_os = "android")` 的代码，经 cargo-ndk，且要用 `rust-toolchain.toml` 定的那个工具链（PATH 上另有 Homebrew 的 cargo 时会报缺 Android 目标）：

```sh
PATH="$(dirname "$(rustup which cargo)"):$HOME/.cargo/bin:$PATH" \
ANDROID_NDK_HOME="$ANDROID_HOME/ndk/<libs.versions.toml 里 ndk 的版本>" CARGO_BUILD_WARNINGS=deny \
cargo ndk -t arm64-v8a -P 31 clippy -p runode-android-tunnel --locked -- -D warnings
```

设备测试（`androidTest`，`terminal`、`connection`、`features`、`speech` 有）要先 `$A boot`，再在 `apps/android` 里跑 `ANDROID_SERIAL=<序列号> ./gradlew connectedDebugAndroidTest`；改了这几个模块里有设备测试覆盖的部分（JNI、VT、Keystore、录音）时跑。它只装、卸各模块自己的测试包，不动 app。识别模型的测试在设备上没放模型时跳过。

## 第二步：准备和构建

前提：JDK 21、`ANDROID_HOME`、`gradle/libs.versions.toml` 里 `ndk` 和 `cmake` 两项写的那一版 NDK 和 CMake，编原生库还要 zig、rustup 和 cargo-ndk（在 `~/.cargo/bin`，脚本自己找得到）。细节在 CONTRIBUTING.md 的「Android 开发」。

app 要三份原生库，Gradle 不会自己编，缺了时 `preBuild` 直接报「找不到 …，先在仓库根目录跑 make android-…」。先查一遍：

```sh
$A prereqs
```

它查 JDK、NDK、CMake，和三份库在不在、过没过期，每项打印 `ok`、`缺` 或 `过期` 和该跑的命令，有缺的或过期的退出码为 1。只按它说的那份重编，别的不动（命令在仓库根目录跑）：

| 库 | 什么时候算过期 | 命令 | 耗时 |
| --- | --- | --- | --- |
| libghostty-vt | `vendor/ghostty` 在编完之后换过提交（子模块的 HEAD 比产物新），或者子模块里有比产物新的未提交改动 | `make android-vt` | 约 1 分钟 |
| librunode_tunnel.so | `apps/android/tunnel` 下有比它新的文件。`Cargo.lock` 比它新时只提醒：iroh、jni、tokio 这些它用的依赖变了才要重编 | `make android-tunnel` | 两个 ABI 约 1.5 分钟 |
| sherpa-onnx | 取出来的 `VERSION` 和 `scripts/fetch-sherpa-onnx.sh` 里写的版本不同 | `make android-speech` | 下载过的按哈希核对后直接解压 |

调试包：`./gradlew :app:assembleDebug`，产物 `app/build/outputs/apk/debug/app-debug.apk`。下一步的 `$A install` 会顺带构建，一般不用单独跑。

## 第三步：在模拟器上启动

```sh
A=apps/android/.claude/skills/verify/scripts/android.sh
$A boot      # 没有开着的模拟器时起一台 AVD（不开窗口），等到开机完成；已经开着就用它
$A install   # ./gradlew :app:installDebug 覆盖安装到这台模拟器
$A launch    # 冷启动 app，等到进程在、MainActivity 在前台
$A host      # 沙盒宿主 + 远程访问（只听 127.0.0.1）
$A pair      # 把配对链接交给 app，等到确认页出来；接着让 mobile-operator 点「配对」
$A paired    # 等宿主那边报配好了
$A rn ...    # 在沙盒里跑命令行，比如 $A rn list、$A rn open --cwd <沙盒目录>
$A logs      # app 的崩溃、ANR、E/F 行，和宿主日志；有问题时退出码为 1
$A shot /tmp/x.png   # 截一张屏
$A status    # 模拟器、app、宿主、配对进程现在怎样
$A stop      # 还原 app 的电脑列表，关宿主、删沙盒，关掉 boot 起的模拟器
```

不带参数运行看完整说明和环境变量。要点：

- **模拟器。** `boot` 用 `AVD` 指定的那台，没给时用 `emulator -list-avds` 的第一台；开着好几台时用 `ANDROID_SERIAL` 选。它打印序列号（一般是 `emulator-5554`），后面交给 mobile-operator。不读写快照，每次冷启动，本机量的是 17～42 秒；上限 `BOOT_SECS`。想看着它跑时 `WINDOW=1 $A boot`。只有 `boot` 自己起的模拟器 `stop` 才关；`KEEP_EMULATOR=1 $A stop` 留着它下一轮接着用，之后的 `stop` 照样关。
- **装和启动。** `install` 约 11～16 秒（APK 有一百多 MB，原生库占大头）。`launch` 带 `am start -S`，每次都是新进程，约 2～3 秒；它记下第一次启动的时刻，`logs` 从那里看起。
- **连宿主。** 模拟器里的 `10.0.2.2` 就是这台电脑的 `127.0.0.1`。`host` 往沙盒的配置文件写远程访问的几项（只听 `127.0.0.1`、端口 `REMOTE_PORT`、电脑名 `runode-verify`），宿主几秒内自己开监听，`host` 等到端口开出来，约 1～2 秒。`pair` 在沙盒里后台跑 `runode remote pair --addr 10.0.2.2`，把它打印的 `runode://pair?…` 链接经 `am start` 交给 app，等到界面里出现电脑名才返回，约 1～3 秒。配对链接几分钟后过期，过期了重跑 `pair`。
- **沙盒目录** 默认 `/tmp/runode-verify-android-<工作树路径的短哈希>`，和桌面验证用的分开；脚本的状态（模拟器的 pid 和日志、配对输出、启动时刻）在它旁边的 `.state` 目录。

## 第四步：走主流程

按改了哪块挑着走，改动跨几块就都走。界面点按、读屏、截图交给 mobile-operator，告诉它：

- 设备是 `$A boot` 打印的序列号，App 是 `dev.runode.mobile`。
- 界面语言跟着模拟器的系统语言，下面写的是中文界面的字，英文界面对应 `features/src/main/res/values/strings.xml` 里的那条。
- 截图存到会话的 scratchpad 目录，不放进沙盒（`stop` 会删沙盒）。
- 不做不可逆的事：不卸载、不清数据、不改系统设置、不在 app 里删别的电脑、不给 app 开通知以外的系统权限；配对只配 `runode-verify` 这一台。

### 启动和首页（任何改动都走）

`$A boot && $A install && $A launch`，然后 `$A shot` 截一张自己看，或者让 mobile-operator 读屏：没有配对过的电脑时首页是「连接你的电脑」和「配对电脑」按钮；模拟器上原来就配过别的电脑时是电脑列表，照样算正常。

只改了界面、又用不着真的宿主时，可以用调试构建的演示模式，不起宿主：`DEMO=<词> $A launch`，词见 `DemoComposition` 的文档（`list` 会话列表、`terminal` 等回答的终端、`shell` 普通 shell、`git`、`picker` 选目录、`settings` 设置页、`pair`、`paired` 配对页，加 `light` 浅色主题、`offline` 断线），用逗号连几个。演示的数据是写死的，换词要重新 `launch`。

### 配对（`connection` 的 `Pairing`、`GateClient`、`Link`，`protocol` 的 `PairingInvitation`、`Gate`，`PairingModel`、`PairingView`）

1. `$A host && $A pair`：app 停在配对页，底下弹出「和这台电脑配对？」，电脑名 `runode-verify`，地址 `10.0.2.2:<端口>, 127.0.0.1:<端口>`。mobile-operator 截一张，点「配对」。
2. `$A paired` 打印 `Paired with …`，`remote devices` 里多一台 `phone`。mobile-operator 看到「配好了」「已和 runode-verify 配对」，截一张。

### 会话列表（`HostConnection`、`protocol` 的 `Messages`、`Layout`，`SessionListModel`、`SessionListView`、`Home`）

先走完配对。

1. `$A rn open --cwd <沙盒目录>` 开一个终端，打印会话号；沙盒目录是 `host` 最后一行打印的那个。
2. mobile-operator 点「去看会话」（或首页里的 `runode-verify`）：标题 `runode-verify`，副标题「已连接，局域网直连」；「全部」计数和 `$A rn list` 的行数一致；沙盒目录名下有一行 `zsh`（或用户的登录 shell）、「空闲」。截一张。
3. `$A rn kill <会话号>`，那一行几秒内消失。

### 终端（`terminal` 模块、libghostty-vt、`TerminalModel`、`TerminalScreen`）

在会话列表里：

1. mobile-operator 点开那一行：终端页，标题是 shell 名，看得到提示符，底下一排 esc、ctrl、tab 和方向键。
2. 你跑 `$A rn send <会话号> "echo verify-android" --enter --wait`，打印 `exit 0`；app 里下一行出现 `verify-android`。截一张。
3. mobile-operator 点终端、用软键盘输入 `echo from-phone` 回车；你跑 `$A rn read <会话号>`，屏幕里有 `from-phone` 那两行。
4. 改了渲染、字体、颜色时，`DEMO=terminal,light $A launch` 再看一遍浅色主题。

### 新建会话、选目录（`DirectoryPickerModel`、`DirectoryPickerView`、`AgentLaunchers`）

在会话列表里点右上的「+」或目录那一行的「+」，按提示开一个普通 shell；`$A rn list` 多一行，app 里也多一行。选目录页可以先用 `DEMO=picker` 看界面。

### 设置、语言、主题（`SettingsView`、`Settings`、`AppLocales`、`Theme`）

首页右上的设置按钮。改到的那一项切过去再切回来，前后各截一张；改了译文时把 app 语言换成另外两种各看一眼（改完换回原来的）。

### Git 页（`GitModel`、`GitScreen`、`protocol` 的 `Git`）

要一个有改动的仓库：

```sh
D=<沙盒目录>/repo
mkdir -p "$D" && git -C "$D" init -q && echo a >"$D/a.txt" && git -C "$D" add a.txt &&
  git -C "$D" -c user.name=verify -c user.email=verify@example.invalid commit -qm init && echo b >>"$D/a.txt"
$A rn open --cwd "$D"
```

mobile-operator 在会话列表里打开这个会话的 Git 页：`a.txt` 在未暂存的改动里，点开看到加了一行 `b`。界面细节可以先用 `DEMO=git` 看。

### 这里验不到的

远程桌面要电脑那边有屏幕录制授权和采集进程，语音输入要下载识别模型和麦克风，推送要 `google-services.json` 和 Firebase，跨网络（iroh）要真的两张网，相机扫码在模拟器上只有假画面。改了这些时跑对应模块的单测和设备测试（`speech` 的识别测试要先把模型放到设备的 `/data/local/tmp/runode-speech-models`），界面部分能用 `DEMO=screen` 的看一眼，汇报里写明没有实地验证。

## 判定和证据

通过要同时满足：

- 第一步的检查全过，`lint-changed` 没列出问题，没有新的 warning；
- `prereqs` 全是 `ok`（重编过的写明重编了哪份）；
- `launch` 成功；走过的每条流程，mobile-operator 的逐步结果和截图都和预期一致，配对时 `paired` 成功；
- `$A logs` 退出码为 0：crash 缓冲区里没有 `dev.runode.mobile`，没有 ANR，沙盒宿主没有 ERROR、panic；app 的 E 行逐条看过、能解释；
- `$A stop` 之后 `$A status` 显示没有本脚本起的模拟器、远程访问没在听、沙盒没有了。

`logs` 里常见、和改动无关的行：没有 `google-services.json` 时 `FirebaseApp` 的初始化 warning，`ashmem` 的 `Pinning is deprecated`，`hiddenapi` 访问 `AccessibilityNodeInfo` 的 selection（mobile-operator 读界面树时出现），离开配对页时相机预览的 `BufferQueue has been abandoned` 和 CameraX 的 `underreport`；宿主日志里只听 `127.0.0.1` 时 `cannot advertise remote access over Bonjour` 的 WARN。

汇报里贴的证据：检查命令的结论行、`prereqs` 的输出、`paired` 的输出、`logs` 的分段标题和其中的 E 行、mobile-operator 的截图路径。

失败时先看哪里：

- Gradle 报「找不到 …libghostty-vt.a / librunode_tunnel.so / sherpa-onnx」：照 `prereqs` 说的 `make android-*` 重编。
- `boot` 超时或模拟器退出：看 `.state` 目录里的 `emulator.log`（`KEEP=1 $A stop` 留着它）；同一台 AVD 已经在 Android Studio 里开着时会撞锁，直接用开着的那台。
- `install` 报 `INSTALL_FAILED_UPDATE_INCOMPATIBLE`：模拟器上装的是签名不同的包（比如 release 构建），卸载会丢 app 的数据，先问用户。
- `launch` 等不到前台、或 `logs` 报崩溃：crash 缓冲区那段就是堆栈；原生崩溃看 `adb -s <序列号> logcat -d -b crash` 里的 `Fatal signal` 和 backtrace，多半是三份原生库和代码对不上，先 `prereqs`。
- `pair` 等不到确认页：`$A shot` 看停在哪；`$A logs` 看宿主日志里有没有 `remote access listening on [127.0.0.1:<端口>]`。`paired` 超时：确认页上的「配对」点了没有；app 里报配对失败时看宿主日志里 `runode_remote::gate` 的行。
- `host` 报端口被占：换个 `REMOTE_PORT`（`host`、`pair`、`status` 都要带同一个值）。
