# Crate 分层

代码分在几个 crate 里，依赖只能自上而下。能单独运行的 app 放在 `apps/` 下（Rust 的 `apps/desktop`、`apps/push-relay` 和 Swift 的 `apps/ios`），给它们用的库放在 `crates/` 下。`apps/` 下不全是 Rust 项目，新加的 Rust app 要在根 `Cargo.toml` 的 `members` 和 `default-members` 里列出；只在服务器上跑的推送中转 `push-relay` 只列在 `members`，开发桌面时不用跟着编它的 HTTP 服务，`make` 的检查、测试和 clippy 带 `--workspace`，照样管它。包名是目录名加 `runode-` 前缀（`crates/terminal` 是 `runode-terminal`），因为依赖树里已有 `dirs` 这类同名的第三方 crate，不加前缀会撞名，`cargo -p` 也会有歧义；`apps/` 下的包名等于可执行文件名，所以桌面 app 的包名是 `runode`。

根目录的 `skills/` 放教 agent 用 runode 的 skill：`skills/runode/SKILL.md` 是入口，不是每次都用得上的细节放在 `references/<主题>.md`，由 `SKILL.md` 链过去，skill 下只能放这两种文件。加 skill 或 reference 只加文件，不改代码（`cli` 的测试核对元数据和链接）。用户用 `npx skills add runode-dev/runode --skill runode -g` 从 GitHub 上的 main 装、用 `npx skills update` 更新，runode 自己不装 skill，所以只改 skill 推到 main 就到了用户那里，不用发版。

| 目录 | 职责 | 可以依赖 |
| --- | --- | --- |
| `paths` | 配置、数据和缓存放在哪（`Dirs`；Linux 上宿主的 socket 和锁按机器分目录，家目录在 NFS 上共享时各台机器各跑各的宿主），以及覆盖写用户手写过的文件（`replace_file`）；Windows 上私有目录只让自己、SYSTEM 和管理员进 | std、windows-sys（Windows） |
| `types` | 各端共用的纯数据：终端帧、网格、分屏布局、agent 状态、会话标识、看板、终端设置、剪贴板的规矩、输入事件、语音识别的模型 | std、serde |
| `protocol` | 宿主和各前端之间的消息；手机远程连接（`remote`）和推送通知（`push`）的线上格式以它为准 | types、serde、serde_json |
| `git` | 用 git 命令行读仓库，做暂存、提交、分支、stash、工作树和同步 | 只有 std |
| `todo` | 看板的 SQLite 库（`Store`）和定时（`next_fire`），以及本地时间和 Unix 时间的换算（`local_tm`、`mktime`）；宿主在跑时改看板仍经宿主的消息，它才会告诉在看看板的前端 | types、rusqlite、libc |
| `tasks` | 一个目录里能跑的命令：`tasks.json`、Makefile 目标、package.json scripts | types、paths、serde_json |
| `preview` | 文件预览不碰界面的部分：读文件、语法高亮、解析 Markdown；界面是 `desktop` 的 `preview_panel` | std、syntect、two-face、pulldown-cmark、regex |
| `agent` | 认出终端前台跑的 AI 编程 agent，判断它在干活、空闲还是等回答；起 agent 时挑模型和强度、续上次的对话，向 agent 本身问模型清单（`models`） | types、serde、serde_json、regex、toml、memchr、libc |
| `terminal` | 终端会话：libghostty-vt 状态机接在 shell 的 PTY 上（Windows 上是 ConPTY，`pty` 的 `conpty`），shell 集成，认出运行完的命令 | types、paths、agent、libghostty-vt、portable-pty、memchr、libc（Unix）、windows-sys（Windows） |
| `config` | Ghostty 兼容的配置文件、主题、快捷键和配置模板，生成 `TermSettings`。模板只有英文，配置项的说明写在 `docs`，动作的说明写在 `ActionSpec::doc`，译文都在 `desktop` 的 locales 里 | types、paths、tracing、objc2-foundation、objc2-app-kit |
| `host` | 管终端会话的宿主（只有 lib），跑在 app 进程里或单独一个进程（`runode --host`）：持有 PTY 和权威的 VT，升级时新宿主接手旧宿主的会话、会话不断（Windows 上不交接，会话跟着重启）；也替前端读写 git、列目录和命令，持有看板 | terminal、protocol、types、git、paths、todo、tasks、agent、libc（Unix）、uds_windows 和 windows-sys（Windows）、anyhow、tracing |
| `remote` | 远程访问：TLS 监听、门禁、设备表、Bonjour（macOS 交给系统，别的系统用 mdns-sd 自己应答），给手机推通知；不在同一个网络时经 iroh 按公钥连过来（`tunnel`，打洞直连，打不通走中继），流上照样跑 TLS 和门禁 | protocol、types、paths、rustls、rcgen、ring、iroh、tokio、socket2、mdns-sd（macOS 以外）、libc（Unix）、uds_windows 和 windows-sys（Windows）、serde、serde_json、tracing |
| `screen` | 远程桌面在电脑这边：采集屏幕、编成低延迟的硬件 HEVC（`Capture`），把手机发来的键鼠输入注入系统（`Injector`）；macOS 上用 ScreenCaptureKit 和 VideoToolbox，Windows 上用 DXGI 桌面复制和 Media Foundation；传输和协议不在这里 | types、tracing、objc2、block2、dispatch2、objc2-screen-capture-kit、objc2-video-toolbox、objc2-core-media、objc2-core-video、objc2-core-graphics（macOS）、windows（Windows） |
| `speech` | 离线语音转文字：识别模型的清单和下载（调系统的 curl，按大小和 SHA-256 核对，国内先试 ModelScope 上 runode 的镜像、海外先试 HuggingFace），录麦克风（`Recorder`，`recording` 特性，无头的 Linux 构建不开），用 sherpa-onnx 先切句再识别（`Recognizer`）；也能交给云端识别、用大模型润色（`cloud`，同样调系统的 curl） | types、sherpa-onnx、cpal（`recording`）、sha2、serde_json、anyhow、tracing |
| `update` | 桌面 app 的自动更新：查 GitHub Release、下载、核对签名，退出时原子地换上 | serde、serde_json、tracing、libc、objc2、block2、objc2-foundation |
| `autostart` | 登录时自启（launchd、systemd、`SMAppService`、Windows 当前用户的 `Run` 键），命令行和设置页共用 | paths、objc2、objc2-foundation、objc2-service-management（macOS）、windows-sys（Windows） |
| `ui` | 桌面几处界面共用的 GPUI 部件（照 shadcn 起名，variant、size 选样子）、全 app 一份的主题 `Theme`（只能在 render 和 paint 里读）、所有图标、界面字号和无障碍的共用写法；界面文字由调用方翻译好交进来 | types、paths、GPUI、tracing、unicode-segmentation、objc2-app-kit、objc2-foundation（macOS）、raw-window-handle 和 windows-sys（Windows） |
| `cli` | 命令行前端（`runode list`、`send`、`todo` 等），经宿主的 Unix socket（Windows 上是 AF_UNIX）按 protocol 说话；`remote`、`service` 不经宿主 | protocol、types、paths、config、remote、autostart、todo、qrcode、libc、uds_windows 和 windows-sys（Windows） |
| `desktop` | GPUI 桌面 app：窗口、视图、菜单、窗口存档；带子命令时交给 `cli`，带 `--host` 时是单独的宿主进程，带 `--screen` 时是远程桌面的采集进程（`screen_process`，宿主所在的进程按需拉起，崩了不连累终端会话），带 `--speech` 时是语音输入的识别子进程（`dictation::worker`，本机录的音和手机交来的录音都交给它，同样为了崩了不连累窗口和终端会话，闲了就退出、把模型占的内存还给系统），和命令行、宿主是同一个可执行文件；Windows 上它是界面程序，命令行另由同一个包里的控制台程序 `rn` 跑；远程访问的监听开在宿主所在的进程里（`remote_access`） | 以上全部、GPUI |
| `push-relay` | 官方 iOS App 的推送中转（`runode-push`，跑在 push.runode.dev）：把手机交来的 device token 封成句柄（`handle`），电脑交回句柄时解开、按句柄限频、签 JWT 发给 APNs（`apns`）；不存状态，部署文件在它的 `deploy` 目录 | protocol、axum、tokio、reqwest、rustls、ring、serde_json、tracing、tracing-subscriber |

终端仿真用的 libghostty-vt 不在 `crates/` 下，取自子模块 `vendor/libghostty-rs`。它是 runode-dev 对 Uzaaft 的 libghostty-rs（MIT 或 Apache-2.0）的 fork，runode 用它的 `main` 分支，里面有安全封装 `libghostty-vt` 和 C 接口绑定 `libghostty-vt-sys`；绑定由 bindgen 生成后提交在那边，重新生成时在 fork 的一份单独克隆里设 `GHOSTTY_SOURCE_DIR` 指向 runode 的 `vendor/ghostty`，跑 `cargo run -p libghostty-vt-sys --features bindgen-tool --bin gen-bindings`；别在子模块里跑，`vendor/rustfmt.toml` 关掉了 vendor 下的格式化，生成的绑定不会排版。它自成一个 workspace，根 `Cargo.toml` 按路径依赖它，`vendor` 在 `exclude` 里，所以本仓库的 lint 不管它。`.cargo/config.toml` 设的 `GHOSTTY_SOURCE_DIR` 让 `libghostty-vt-sys` 的构建脚本从 `vendor/ghostty` 编出库，不去网上拉。runode 要用而上游还没有的接口加在 `main` 上，一样东西一个提交，好在以后变基；上游已有同样能力、只是名字或形状不同的，改 runode 去适应，不改 fork。

同步 ghostty 上游时两个子模块一起动。`vendor/ghostty` 是 runode-dev/ghostty 的 fork，等于上游 main 加几个 runode 的修复提交，一处修复一个提交，彼此独立：把这组提交一起变基到上游最新，用 `git push --force-with-lease` 推上 fork；上游自己修好了哪处，就删掉对应的提交。同一次同步里，在 `vendor/libghostty-rs` 里把 `libghostty-vt-sys` 构建脚本的 `GHOSTTY_COMMIT` 改成 ghostty fork 所基于的那个上游 commit，用 `gen-bindings` 重新生成绑定，提交并推上 fork 的 `main`；要跟进 Uzaaft 的上游时，也是把 `main` 变基到上游的 `master` 上。最后在 runode 里一起挪两个子模块的指针，绑定有变化就看 `terminal` 要不要跟着改。两边必须对齐，因为构建时编的是 `vendor/ghostty` 的源码，绑定却是照 `GHOSTTY_COMMIT` 那版头文件生成的，不一致时结构体布局可能对不上，编得过，运行时却读写错内存。

GPUI 取自 crates.io 上的 gpui-pre（Zed 的 gpui 的快照），原样使用，根 `Cargo.toml` 里 gpui-pre 和 gpui-pre-platform 用同一个 `=` 版本钉住、一起改。要改 GPUI 平台层的行为时，先在 `desktop` 里经 raw-window-handle 拿到原生窗口自己做，比如建窗口时关掉 Metal 图层的 `displaySyncEnabled`（`window` 的 `open`）、问要不要让程序读剪贴板时自己弹 NSAlert（`window` 的 `clipboard`）；平台层内部才做得到的提给上游。

不变量：

- 只有 `desktop` 和它的部件库 `ui` 能依赖 GPUI（`gpui-pre`；`gpui-pre-platform` 只有 `desktop`）；`libghostty-vt` 和 portable-pty 只有 `terminal` 能直接依赖，`libghostty-vt-sys` 只有 `libghostty-vt` 能直接依赖；syntect 和 two-face 只有 `preview` 能直接依赖；rustls、rcgen、ring、iroh 和 mdns-sd 只有 `remote` 能直接依赖（iroh 自己的 crate 用 rustls、ring 不算），推送中转 `push-relay` 自己连 APNs、封句柄，也直接用 rustls 和 ring；rusqlite 只有 `todo` 能直接依赖；sherpa-onnx 只有 `speech` 能直接依赖；ScreenCaptureKit 和 VideoToolbox 的绑定只有 `screen` 能直接依赖；只有 `desktop` 能直接依赖 `host`，只有 `desktop` 和 `host` 能直接依赖 `terminal`。这几条由 `deny.toml` 守着，CI 里跑 `cargo deny check bans`；加 crate 时相应地更新它的 `wrappers` 和上面这张表。
- `types` 只放数据，不依赖其他 runode crate，也不依赖终端仿真或界面；`paths`、`git`、`preview`、`update` 不依赖任何 runode crate。
- `protocol` 只依赖 `types`，不碰终端仿真、PTY 和界面。消息里用到的类型，别的 crate 也要用的放 `types`，只在协议里用的（帧、连接方式等）放 `protocol` 自己；`protocol` 把消息里用到的 `types` 类型转出去，前端照旧从 `protocol` 引。
- `agent` 只依赖 `types`，不碰终端仿真、PTY 和界面：屏幕文字、前台进程组由 `terminal` 读好了交给它，用户规则目录由调用方从 `paths` 取来传进去。
- `host` 不依赖 GPUI，VT 和 PTY 经 `terminal` 的 `HostSession` 用。一个终端有两份 VT，宿主那份（`HostSession`）是权威的，只有它应答终端查询；界面那份（`Session`）只消费同样的字节流，改 VT 状态的操作（改尺寸、清屏、换主题）一律经宿主在输出流里标出位置后两边一起做。前端（桌面也一样）和宿主说话只经 `protocol`，不碰宿主的内部：桌面在同一个进程里时用 `Host::connect_pair` 的一对 socket，别的前端连 `paths` 的 `host_socket_file`。
- 宿主进程自己担着 macOS 的责任进程（`disclaim_responsibility`），打包时每个版本写同一个 Mach-O UUID，交出会话的旧宿主等它的 shell 都结束了才退出，升级接手后的终端才连得上局域网。所以宿主平常单独一个进程跑，跑在 app 里的宿主不把会话交出去（shell 算在 app 名下，app 一退出就连不上）。改宿主的拉起方式或打包脚本前，先读这几处的说明。
- 宿主不管窗口：要界面办的请求包成 `HostMsg::UiRequest` 转给 `ClientKind::Desktop` 的连接，桌面用 `ClientMsg::UiReply` 回话。没有桌面连着时，布局、开终端、新建和改名工作区由宿主照记着的布局自己办（`host` 的 `workspaces`）。会话里的程序读写剪贴板（OSC 52）、要求改窗口尺寸（CSI 8 t）也是宿主那份 VT 认出来、交给桌面决定办不办，宿主自己不碰剪贴板，也不因此改会话尺寸。
- `cli` 只经 socket 和宿主说话，不依赖 `host`、`terminal` 和 GPUI。`deny.toml` 连测试依赖一起查，所以它的测试对着 `tests/common` 里按 protocol 回话的假宿主跑。宿主给每个 shell 设 `RUNODE_SESSION`、`RUNODE_SOCKET`，桌面另设 `RUNODE_BIN`，命令行据此找到开它的那个 app 和自己所在的会话。
- 命令行的命令或选项一改（加、删、改名、改写法或说明），同一个提交里改齐两处：`cli` 的用法说明（`help` 模块的总览 `OVERVIEW` 和 `PAGES` 里那条命令的页）、教 agent 用 runode 的 skill（`skills/runode/`）。只给别的程序调的命令 skill 里不写。skill 没有测试守着，要自己记得改。
- `remote` 不依赖 `host`：过了门禁的连接经调用方给的闭包（`Connect`）接到宿主上，手机发来的控制帧由它筛过才转。监听开在宿主所在的进程里，app 连着单独跑的宿主时不开；只有占着监听的进程推通知。命令行和监听方之间只经数据目录里的文件，不经宿主。线上格式以 `protocol` 的 `remote` 模块为准，`crates/protocol/tests/fixtures/remote` 里的样例手机端的测试也读，改格式要两边一起改。推送中转的请求体（`protocol` 的 `push` 模块里的 `Relay*`）同理，改了同一个提交里改 `push-relay` 和手机端。
- 家目录和 runode 自己的配置、数据、缓存路径一律经 `paths` 取，不在别处读 HOME 或自己拼路径；别的程序的文件（比如 shell 的历史）按那个程序的规矩找。
- 新依赖先加进根 `Cargo.toml` 的 `[workspace.dependencies]`，各 crate 用 `xxx.workspace = true`；lint 规则在 `[workspace.lints]`，每个 crate 都写 `[lints] workspace = true`。
- 只经 crate 公开接口测的黑盒测试放在和 `src` 同级的 `tests/` 目录，按主题分文件，几个文件共用的辅助放 `tests/common/mod.rs`；测私有实现的单元测试留在 `src` 里的 `#[cfg(test)]` 模块。不为了搬测试把内部的东西改成 pub。`desktop` 是二进制 crate，`tests/` 引用不到它，测试都留在 `src` 里。

# 命名

- crate 的目录名是 kebab-case 的领域名词，要说清它管的是什么，不能只说它做的一部分：`git` 读状态之外还做暂存、提交、分支和同步，所以不叫 `git-status`。
- 同一个泛词在两处出现时，各自加上领域限定，读代码时才分得清说的是哪一个：比如预览里读文件、高亮这些不碰界面的部分叫 `preview`，预览栏的界面是 `desktop` 里的 `preview_panel`。
- 模块名用 snake_case。有子模块的模块写成 `foo.rs` 加 `foo/` 目录，不用 `foo/mod.rs`；只有 `tests/common/mod.rs` 按 cargo 的惯例保留，这样 cargo 不把它当成一个单独的测试。
- 一个模块的单元测试超过三百行左右时挪到 `foo/tests.rs`，`foo.rs` 里只留 `#[cfg(test)] mod tests;`。
- 界面代码只分两处：能复用的部件和所有图标放在 `ui`，入口用 `use runode_ui as ui` 接进来，各模块写 `crate::ui::…`；部件要回调视图的写成对视图类型泛型（`RowButton<V>`、`MenuHost`）。只有桌面用的整页视图（`terminal_view`、`settings`、`github_actions`、`file_tree`、`git_panel`、`preview_panel`、`kanban`）是 `desktop` 的顶层模块，各是独立的 Entity，和窗口只经 `set_input` 和自己的事件说话；`window` 下同名的模块是把它们接进窗口的接入层。不为单个页开 crate。`desktop` 顶层另外只放应用级的胶水，只被一个功能用的模块放进那个功能的目录。

# 桌面界面

桌面界面（`apps/desktop` 和 `crates/ui`）的无障碍、界面字号和重画的规则在 `apps/desktop/AGENTS.md`，改界面前先读。
