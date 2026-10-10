# Crate 分层

代码分在几个 crate 里，依赖只能自上而下。能单独运行的 app 放在 `apps/` 下（Rust 的 `apps/desktop` 和 Swift 的 `apps/ios`），给它们用的库放在 `crates/` 下。根目录的 `skills/` 放教 agent 用 runode 的 skill：`skills/runode/SKILL.md` 讲操作别的终端，`skills/runode-simulator/SKILL.md` 讲在模拟器页看着的模拟器上跑和测 app；`cli` 的 `runode setup` 把它们编进二进制（`SKILLS`），装成 Claude Code 和 Codex 的 skill。`apps/` 下不全是 Rust 项目，新加的 Rust app 要在根 `Cargo.toml` 的 `members` 和 `default-members` 里列出。目录名直接说明职责，包名是目录名加 `runode-` 前缀（`crates/terminal` 是 `runode-terminal`），只有桌面 app 的包名是 `runode`，让可执行文件仍叫 runode。包名带前缀是因为依赖树里已有 `dirs` 这类同名的第三方 crate，不加前缀会撞名，`cargo -p` 也会有歧义。

| 目录 | 职责 | 可以依赖 |
| --- | --- | --- |
| `paths` | 配置、数据和缓存放在哪：`Dirs::from_env()` 和每个文件的路径（含远程访问推送用着的广播频道），以及覆盖写用户手写过的文件（配置文件、`~/.codex/AGENTS.md`）的 `replace_file`：先写临时文件再改名换上，顺着符号链接写 | 只有 std |
| `shared-types` | 各端共用的纯数据：终端帧、网格、分屏布局、agent 状态、会话对外公布的状态、终端设置、读写剪贴板的规矩（`clipboard-write`、`clipboard-read` 的取值和一次读写的上限）、输入事件 | std、serde |
| `protocol` | 宿主进程和各个前端之间的消息：帧格式、控制消息、会话标识，请宿主在会话所在的仓库里读写 git 的消息（`git`），一个目录里能跑的 Makefile 目标和 package.json scripts（`project_tasks`），agent 等回答时推给手机的 Live Activity 的格式（`push`：attributes、content-state、APNs payload 和中转请求体），以及手机经网络连上来时的门禁（`remote`：门禁消息、签名的字节串、base64url、配对 URI，是远程访问线上格式的正式定义） | shared-types、serde、serde_json |
| `git` | 用 git 命令行读仓库的状态、逐行改动、分支、stash 和提交图，也做暂存（含按块暂存）、丢弃、提交、切换分支、stash 和与远端同步这些操作 | 只有 std |
| `preview` | 文件预览不碰界面的部分：读文件、判断是文本、图片还是二进制，语法高亮出调色板语义的颜色，把 Markdown 解析成块结构（标题、段落、列表、引用、代码块、表格、图片等，照 GitHub 认提示块、脚注、裸网址链接和 README 里常用的 HTML（图片、标题、链接、居中），给标题算锚点） | std、syntect、two-face、pulldown-cmark、regex |
| `agent-detect` | 认出终端前台在跑哪个 AI 编程 agent，判断它在干活、空闲还是等用户回答：按前台进程识别、识别规则的格式和求值（内置规则编进二进制）、状态去抖 | shared-types、serde、regex、toml |
| `ghostty-vt-sys` | libghostty-vt 的 C 接口绑定（bindgen 生成后提交在仓库里，`gen-bindings` 重新生成），构建脚本用 zig 从 `vendor/ghostty` 编译出库；源自 libghostty-rs，许可 MIT 或 Apache-2.0，库名沿用 `libghostty_vt_sys` | 只有 std |
| `ghostty-vt` | libghostty-vt 的安全封装：终端状态机、渲染状态、选区、搜索、按键和鼠标编码；源自 libghostty-rs，库名和依赖名沿用 `libghostty_vt`（`libghostty-vt`），代码里照旧 `use libghostty_vt` | ghostty-vt-sys、bitflags、int-enum |
| `terminal` | 终端会话：libghostty-vt 状态机接在 shell 的 PTY 上，shell 集成、命令历史；把前台进程、屏幕文字、标题和进度报告交给 `agent-detect` | shared-types、paths、agent-detect、ghostty-vt、portable-pty |
| `completion` | 按 Tab 的命令补全：命令规格、候选排序、生成器 | terminal、shared-types、paths |
| `prompt-highlight` | 提示符上输入的语法高亮：把命令行分成命令名、关键字、选项、字符串、变量、路径等几类，按 fast-syntax-highlighting 的默认主题定色；命令名和子命令借 `completion` 查 | completion、paths |
| `config` | Ghostty 兼容的配置文件、主题、快捷键写法和配置模板，生成 `TermSettings` | shared-types、paths |
| `host` | 管终端会话的宿主（只有 lib）：每个会话一个线程，持有 PTY 和权威的那份 VT，应答终端查询、认标题和 agent、记命令历史。跑在 app 进程里，或者由 app 拉起成单独一个进程（`runode --host`，配置项 `terminal-host`）；前端一律经一条连接按 protocol 的帧和它说话：桌面在同一个进程里时用 `Host::connect_pair` 的一对 socket，别的时候连 Unix socket。app 升级后，新版本拉起的新宿主（`Host::take_over`）以 `ClientKind::Successor` 连上旧宿主的 socket，接过各会话的 PTY 和监听的 socket，会话不断；前端（手机）请它在某个会话所在的仓库里读写 git 时，经 `git` 办；手机新建工作区时浏览电脑上的目录也由它列出来，没给目录时列 `paths` 给的家目录；手机会话卡片上能跑的 Makefile 目标和 package.json scripts 也由它从会话目录往上找、拼好命令行 | terminal、protocol、shared-types、git、paths、libc、serde_json |
| `remote-access` | 远程访问：TLS 1.3 监听（rustls，ring 后端，自签证书用 rcgen 生成）、门禁（验签、配对口令、限速）、设备表（含手机登记的推送 token）、Bonjour 公布，过了门禁的连接经调用方给的闭包接到宿主上；agent 等回答时经同一个闭包连宿主轮询会话，用广播频道推 Live Activity（直连 APNs 用 ring 签 JWT，或经官方中转；外调 curl 发 HTTP/2）；也给命令行用的配对口令文件、设备表和监听方状态 | protocol、shared-types、paths、rustls、rcgen、ring、libc、serde、serde_json |
| `update` | 桌面 app 的自动更新：读 GitHub 上最新 Release 里的版本清单 `latest.json`、比版本号，经 NSURLSession 下载这台 Mac 架构的 zip、解压到装着的 .app 旁边，核对新包是 Developer ID 签的、和在跑的这份出自同一个 Team ID、同一个 bundle id，app 退出时再核对一次、用 `renamex_np` 原子地换上 | serde、serde_json、tracing、libc、objc2、block2、objc2-foundation |
| `autostart` | 登录时自启：macOS 写 launchd 的 LaunchAgent，Linux 写 systemd 的用户服务，能拉起无界面的宿主（`runode --host`）或桌面 app（只有 macOS）；装没装看服务文件在不在，命令行（`runode service`）和设置页共用 | paths |
| `cli` | 命令行前端（`runode list`、`read`、`send`、`wait`、`open`、`kill`、`focus`、`setup`、`remote`、`service`）：经宿主的 Unix socket 按 protocol 说话，列会话、读屏幕、发输入、等 agent，请 app 开终端、切到终端；`setup` 把根目录 `skills/` 里的 skill 装给 agent；`service` 不经宿主，经 `autostart` 装上、去掉登录时自启；`remote` 不经宿主，经 `remote-access` 给手机配对、列出和撤销设备，配对成了以后问用户要不要在配置里打开 `terminal-host`（经 `config` 改配置文件），让退出 app 后远程访问留在后台 | protocol、shared-types、paths、config、remote-access、autostart、qrcode |
| `desktop` | GPUI 桌面 app：窗口、视图、菜单、窗口存档和 Info.plist；带子命令启动时交给 `cli`、带 `--host` 时是单独一个进程的宿主、带 `--host --take-over` 时是升级时接手旧宿主会话的新宿主，和命令行、宿主是同一个可执行文件；远程访问的监听开在宿主所在的那个进程里（`remote_access`）；右侧面板的模拟器页经用户自己装的 mobilecli 看和操作 iOS 模拟器、Android 模拟器（不打包它，它不是开源许可）；打包脚本按 `apps/desktop#` 找它的构建产物 | 以上全部（含 host、protocol、cli、remote-access）、GPUI |

不变量：

- 只有 `desktop` 能依赖 GPUI（`gpui-pre`、`gpui-pre-platform`）；`ghostty-vt`（libghostty-vt）和 portable-pty 只有 `terminal` 能直接依赖，`ghostty-vt-sys` 只有 `ghostty-vt` 能直接依赖；syntect 和 two-face 只有 `preview` 能直接依赖；rustls、rcgen 和 ring 只有 `remote-access` 能直接依赖。这几条由 `deny.toml` 守着，CI 里跑 `cargo deny check bans`。
- `shared-types` 只放数据，不依赖其他 runode crate，也不依赖终端仿真或界面；`paths`、`git`、`preview`、`update` 不依赖任何 runode crate，`ghostty-vt` 只依赖 `ghostty-vt-sys`。
- `protocol` 只依赖 `shared-types`，不碰终端仿真、PTY 和界面。消息里用到的类型，别的 crate 也要用的（网格尺寸、agent 状态、会话公布的状态等）放 `shared-types`，只在协议里用的（会话标识、帧、连接方式等）放 `protocol` 自己。
- `agent-detect` 只依赖 `shared-types`，不碰终端仿真、PTY 和界面：屏幕文字、前台进程组由 `terminal` 读好了交给它，用户规则目录由调用方从 `paths` 取来传进去。内置规则文件的出处和许可写在它的 `LICENSE-rules` 里。
- `host` 不依赖 GPUI，也不直接依赖 libghostty-vt 和 portable-pty：VT 和 PTY 经 `terminal` 的 `HostSession` 用。一个终端有两份 VT，宿主那份（`HostSession`）是权威的，只有它应答终端查询；界面那份（`Session`）只消费同样的字节流，改 VT 状态的操作（改尺寸、清屏、换主题）一律经宿主在输出流里标出位置后两边一起做。现在只有 `desktop` 能直接依赖 `host`（建进程内的宿主、跑 `runode --host`），由 `deny.toml` 守着；它和宿主说话也只经 `protocol`，不碰宿主的内部。别的前端经 `protocol` 连 `paths` 的 `host_socket_file` 上的 socket。
- `cli` 只经 socket 和宿主说话，不依赖 `host`、`terminal` 和 GPUI。`deny.toml` 连测试依赖一起查，所以它的测试也不起真宿主，对着 `tests/common` 里按 protocol 回话的假宿主跑；宿主那边的 socket 由 `host` 自己的测试管。宿主给每个 shell 设 `RUNODE_SESSION`、`RUNODE_SOCKET`（`protocol` 的 `ENV_SESSION`、`ENV_SOCKET`），桌面另设 `RUNODE_BIN`，命令行据此找到开它的那个 app 和自己所在的会话。
- 命令行的命令或选项一改（加、删、改名、改写法或说明），同一个提交里改齐三处：`cli` 的用法说明 `HELP`、`completion` 里 runode 自己的命令规格（`specs/runode.json`，有动态候选的还有 `runode_cli` 里的生成器）、教 agent 用 runode 的 skill（根目录 `skills/runode/SKILL.md`）。只给别的程序调的命令在规格里标 `hidden`，skill 里不写。`HELP` 和规格里的命令对不上时，`cli` 的测试 `help_and_completion_spec_list_the_same_commands` 会失败，但它不核对选项，skill 也没有测试守着，要自己记得改。
- `remote-access` 不依赖 `host`：门禁过了的连接经调用方给的闭包（`Connect`，通常是 `Host::connect_pair`）接到宿主上，它在两边搬字节，检查第一帧是 `client` 为 `mobile` 的 `Hello`，之后按帧头筛手机发来的控制帧：`Shutdown`、交接的几种消息、`UiReply`、`SetOptions`、`SetTheme` 不转给宿主，在发往手机的方向、两帧之间回一条 `HostMsg::Error`；登记推送的 `PushRegister` 也不转给宿主，由它按这台设备记进设备表，同样在两帧之间回 `HostMsg::Done`（输入等别的帧不解析，原样转）。宿主直接收到它时回 `Error`。推送 Live Activity 的一方（`push`）跟着监听：只有占着监听的进程推，它作为普通的 `ClientKind::Cli` 前端经 `Connect` 连宿主、只发 `ListSessions` 和 `ReadScreen`，不 `Attach`（不改 `SessionInfo::clients`），没有登记或关了推送时不连。监听开在宿主所在的进程里：宿主跑在 app 里时由 app 按配置开关，单独跑时由 `runode --host` 自己读配置开关；app 连着单独跑的宿主时不开。命令行和监听方之间只经数据目录里的文件（配对口令、设备表、监听方的锁和状态），不经宿主。线上格式以 `protocol` 的 `remote` 模块为准，`crates/protocol/tests/fixtures/remote` 里的样例手机端的测试也读，改格式要两边一起改。
- 宿主不管窗口：要界面办的请求（`ClientMsg::Open`、`ClientMsg::OpenWorkspace`、`ClientMsg::RenameWorkspace`、`ClientMsg::Reveal`、`ClientMsg::Layout`）宿主包成 `HostMsg::UiRequest` 转给 `Hello` 里说自己是 `ClientKind::Desktop` 的连接，桌面转到主线程去办，用 `ClientMsg::UiReply` 回话。界面的布局变了时桌面发 `ClientMsg::LayoutChanged`，宿主转成 `HostMsg::LayoutChanged` 告诉这条连接上问过 `Layout` 的前端（手机据此马上重新要布局），自己不记布局。会话里的程序读写系统剪贴板（OSC 52）也一样：宿主那份 VT 认出请求、按配置决定办不办，包成 `ClientMsg::WriteClipboard`、`ClientMsg::ReadClipboard` 交给最近和那个会话交互过的桌面，宿主自己不碰剪贴板；界面那份 VT 不认这些请求。
- 家目录和 runode 自己的配置、数据、缓存路径一律经 `paths` 取，不在别处读 HOME 或自己拼路径；别的程序的文件（比如 shell 的历史）按那个程序的规矩找。
- 新依赖先加进根 `Cargo.toml` 的 `[workspace.dependencies]`，各 crate 用 `xxx.workspace = true`；lint 规则在 `[workspace.lints]`，每个 crate 都写 `[lints] workspace = true`。
- 只经 crate 公开接口测的黑盒测试放在和 `src` 同级的 `tests/` 目录，按主题分文件，几个文件共用的辅助放 `tests/common/mod.rs`；测私有实现的单元测试留在 `src` 里的 `#[cfg(test)]` 模块。不为了搬测试把内部的东西改成 pub。`desktop` 是二进制 crate，`tests/` 引用不到它，测试都留在 `src` 里。

以后要加的 crate 放在这些位置，命名沿用同样的规则：

- `tui`（前端，放在 `apps/` 下）：和 `cli` 一样，经 protocol 跟宿主说话，不依赖 GPUI，也不直接依赖 libghostty-vt。

加了这些 crate 后，相应地更新 `deny.toml` 的 `wrappers` 和上面这张表。

# 命名

- crate 的目录名是 kebab-case 的领域名词，要说清它管的是什么，不能只说它做的一部分：`git` 读状态之外还做暂存、提交、分支和同步，所以不叫 `git-status`。
- 同一个泛词在两处出现时，各自加上领域限定，读代码时才分得清说的是哪一个：提示符上输入的高亮叫 `prompt-highlight`，和预览、补全里的高亮分开。
- 包名是 `runode-` 加目录名；`apps/` 下的包名等于可执行文件名，所以桌面 app 的包名是 `runode`。
- 模块名用 snake_case。有子模块的模块写成 `foo.rs` 加 `foo/` 目录，不用 `foo/mod.rs`；只有 `tests/common/mod.rs` 按 cargo 的惯例保留，这样 cargo 不把它当成一个单独的测试。
- 一个模块的单元测试超过三百行左右时挪到 `foo/tests.rs`，`foo.rs` 里只留 `#[cfg(test)] mod tests;`。
- 桌面 app 的模块按归属分组。顶层只放应用级的胶水（入口、关于面板、资源、菜单、快捷键、配置、语言、启动计时、提前拉起 shell、`--host` 进程、远程访问、自动更新）和几个功能模块：终端视图 `terminal_view`、窗口 `window`、连宿主的客户端 `host_client`、设置页 `settings`（铺在终端窗口里）。几处界面共用的 GPUI 部件和小工具（输入框、滚动条、悬停提示、文件图标、系统声音、共用的编辑动作、`hsla`）放进 `ui`，`ui` 不依赖任何功能模块。只被一个功能用的模块放进那个功能的目录：按键翻译和自绘字符在 `terminal_view` 下，开窗口、存档格式和 agent 提醒在 `window` 下。

# 无障碍

桌面 app 的界面都要让辅助工具读得到、按得动。辅助工具包括 VoiceOver，也包括 CUA 这类读 AX 树的自动化工具。新加或改动的界面在同一个提交里把无障碍一起做完。

- GPUI 只把同时有 `id` 和 `role` 的元素报给辅助工具。名字用 `aria_label`，写翻译过的文字；图标按钮的名字写它做什么，不写图标叫什么。状态用 `aria_selected`、`aria_expanded`、`aria_toggled`、`aria_value`。不可用的元素用 `ui::a11y` 的 `Disable`，被模态对话框盖住的部分用 `Hide`。
- 能点的东西辅助工具都要按得到。GPUI 默认把辅助工具的按下换成在元素中心合成一次点击，元素滚出可见区域或被盖住时就点空了。点击用 `ui::a11y` 的 `Press`，按下就办的用 `PressDown`；只能自己写鼠标处理时，另挂 `A11yPress::on_a11y_press`。展开、收起、设值、滚动这些操作也要另外用 `on_a11y_action` 登记。
- 自己画出来的内容（canvas、图片、视频帧）要另外给出辅助工具能读、能操作的节点。终端报成文本区域。模拟器页在画面上按位置摆出设备里的元素，按下就点那个元素，写字就打进去；画面四边另有滑动按钮，因为 GPUI 在 macOS 上不转辅助工具的滚动动作。macOS 把图片当叶子节点，挂在它下面的子节点报不出去，所以要摆子节点的容器用 `Role::Group`。
- 只给辅助工具用、算起来又费事的节点，看 `Window::is_a11y_active` 再决定画不画，没开时不画。比如 Git 面板里每行都画出的按钮、模拟器页摆的设备元素。
- 出错、空状态、加载中这些只起提示作用的文字，用 `Role::Label` 报出去；`project` 的 `panel_message` 已经这样做了。
- 改完用 CUA 的 `get_window_state` 看一遍 AX 树：每个按钮都要有名字，用 `AXPress` 按得动；状态变了，树里的状态也跟着变。
