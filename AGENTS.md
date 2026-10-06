# Crate 分层

代码分在几个 crate 里，依赖只能自上而下。能单独运行的 app 放在 `apps/` 下（Rust 的 `apps/desktop` 和 Swift 的 `apps/ios`），给它们用的库放在 `crates/` 下。`apps/` 下不全是 Rust 项目，新加的 Rust app 要在根 `Cargo.toml` 的 `members` 和 `default-members` 里列出。目录名直接说明职责，包名是目录名加 `runode-` 前缀（`crates/terminal` 是 `runode-terminal`），只有桌面 app 的包名是 `runode`，让可执行文件仍叫 runode。包名带前缀是因为依赖树里已有 `dirs` 这类同名的第三方 crate，不加前缀会撞名，`cargo -p` 也会有歧义。

| 目录 | 职责 | 可以依赖 |
| --- | --- | --- |
| `paths` | 配置、数据和缓存放在哪：`Dirs::from_env()` 和每个文件的路径 | 只有 std |
| `shared-types` | 各端共用的纯数据：终端帧、网格、分屏布局、agent 状态、会话对外公布的状态、终端设置、读写剪贴板的规矩（`clipboard-write`、`clipboard-read` 的取值和一次读写的上限）、输入事件 | std、serde |
| `protocol` | 宿主进程和各个前端之间的消息：帧格式、控制消息、会话标识，以及手机经网络连上来时的门禁（`remote`：门禁消息、签名的字节串、base64url、配对 URI，是远程访问线上格式的正式定义） | shared-types、serde、serde_json |
| `git` | 用 git 命令行读仓库的状态、逐行改动、分支、stash 和提交图，也做暂存（含按块暂存）、丢弃、提交、切换分支、stash 和与远端同步这些操作 | 只有 std |
| `preview` | 文件预览不碰界面的部分：读文件、判断是文本、图片还是二进制，语法高亮出调色板语义的颜色 | std、syntect、two-face |
| `agent-detect` | 认出终端前台在跑哪个 AI 编程 agent，判断它在干活、空闲还是等用户回答：按前台进程识别、识别规则的格式和求值（内置规则编进二进制）、状态去抖 | shared-types、serde、regex、toml |
| `terminal` | 终端会话：libghostty-vt 状态机接在 shell 的 PTY 上，shell 集成、命令历史；把前台进程、屏幕文字、标题和进度报告交给 `agent-detect` | shared-types、paths、agent-detect、libghostty-vt、portable-pty |
| `completion` | 按 Tab 的命令补全：命令规格、候选排序、生成器 | terminal、shared-types、paths |
| `prompt-highlight` | 提示符上输入的语法高亮：把命令行分成命令名、关键字、选项、字符串、变量、路径等几类，按 fast-syntax-highlighting 的默认主题定色；命令名和子命令借 `completion` 查 | completion、paths |
| `config` | Ghostty 兼容的配置文件、主题、快捷键写法和配置模板，生成 `TermSettings` | shared-types、paths |
| `host` | 管终端会话的宿主（只有 lib）：每个会话一个线程，持有 PTY 和权威的那份 VT，应答终端查询、认标题和 agent、记命令历史。跑在 app 进程里，或者由 app 拉起成单独一个进程（`runode --host`，配置项 `terminal-host`）；前端一律经一条连接按 protocol 的帧和它说话：桌面在同一个进程里时用 `Host::connect_pair` 的一对 socket，别的时候连 Unix socket。app 升级后，新版本拉起的新宿主（`Host::take_over`）以 `ClientKind::Successor` 连上旧宿主的 socket，接过各会话的 PTY 和监听的 socket，会话不断 | terminal、protocol、shared-types、libc |
| `remote-access` | 远程访问：TLS 1.3 监听（rustls，ring 后端，自签证书用 rcgen 生成）、门禁（验签、配对口令、限速）、设备表、Bonjour 公布，过了门禁的连接经调用方给的闭包接到宿主上；也给命令行用的配对口令文件、设备表和监听方状态 | protocol、paths、rustls、rcgen、ring、libc、serde、serde_json |
| `cli` | 命令行前端（`runode list`、`read`、`send`、`wait`、`open`、`kill`、`focus`、`remote`）：经宿主的 Unix socket 按 protocol 说话，列会话、读屏幕、发输入、等 agent，请 app 开终端、切到终端；`remote` 不经宿主，经 `remote-access` 给手机配对、列出和撤销设备 | protocol、shared-types、paths、remote-access、qrcode |
| `desktop` | GPUI 桌面 app：窗口、视图、菜单、窗口存档和 Info.plist；带子命令启动时交给 `cli`、带 `--host` 时是单独一个进程的宿主、带 `--host --take-over` 时是升级时接手旧宿主会话的新宿主，和命令行、宿主是同一个可执行文件；远程访问的监听开在宿主所在的那个进程里（`remote_access`）；打包脚本按 `apps/desktop#` 找它的构建产物 | 以上全部（含 host、protocol、cli、remote-access）、GPUI |

不变量：

- 只有 `desktop` 能依赖 GPUI（`gpui-pre`、`gpui-pre-platform`）；libghostty-vt 和 portable-pty 只有 `terminal` 能直接依赖；syntect 和 two-face 只有 `preview` 能直接依赖；rustls、rcgen 和 ring 只有 `remote-access` 能直接依赖。这几条由 `deny.toml` 守着，CI 里跑 `cargo deny check bans`。
- `shared-types` 只放数据，不依赖其他 runode crate，也不依赖终端仿真或界面；`paths`、`git`、`preview` 不依赖任何 runode crate。
- `protocol` 只依赖 `shared-types`，不碰终端仿真、PTY 和界面。消息里用到的类型，别的 crate 也要用的（网格尺寸、agent 状态、会话公布的状态等）放 `shared-types`，只在协议里用的（会话标识、帧、连接方式等）放 `protocol` 自己。
- `agent-detect` 只依赖 `shared-types`，不碰终端仿真、PTY 和界面：屏幕文字、前台进程组由 `terminal` 读好了交给它，用户规则目录由调用方从 `paths` 取来传进去。内置规则文件的出处和许可写在它的 `LICENSE-rules` 里。
- `host` 不依赖 GPUI，也不直接依赖 libghostty-vt 和 portable-pty：VT 和 PTY 经 `terminal` 的 `HostSession` 用。一个终端有两份 VT，宿主那份（`HostSession`）是权威的，只有它应答终端查询；界面那份（`Session`）只消费同样的字节流，改 VT 状态的操作（改尺寸、清屏、换主题）一律经宿主在输出流里标出位置后两边一起做。现在只有 `desktop` 能直接依赖 `host`（建进程内的宿主、跑 `runode --host`），由 `deny.toml` 守着；它和宿主说话也只经 `protocol`，不碰宿主的内部。别的前端经 `protocol` 连 `paths` 的 `host_socket_file` 上的 socket。
- `cli` 只经 socket 和宿主说话，不依赖 `host`、`terminal` 和 GPUI。`deny.toml` 连测试依赖一起查，所以它的测试也不起真宿主，对着 `tests/common` 里按 protocol 回话的假宿主跑；宿主那边的 socket 由 `host` 自己的测试管。宿主给每个 shell 设 `RUNODE_SESSION`、`RUNODE_SOCKET`（`protocol` 的 `ENV_SESSION`、`ENV_SOCKET`），桌面另设 `RUNODE_BIN`，命令行据此找到开它的那个 app 和自己所在的会话。
- `remote-access` 不依赖 `host`：门禁过了的连接经调用方给的闭包（`Connect`，通常是 `Host::connect_pair`）接到宿主上，它在两边搬字节，检查第一帧是 `client` 为 `mobile` 的 `Hello`，之后按帧头筛手机发来的控制帧：`Shutdown`、交接的几种消息、`UiReply`、`SetOptions`、`SetTheme` 不转给宿主，在发往手机的方向、两帧之间回一条 `HostMsg::Error`（输入等别的帧不解析，原样转）。监听开在宿主所在的进程里：宿主跑在 app 里时由 app 按配置开关，单独跑时由 `runode --host` 自己读配置开关；app 连着单独跑的宿主时不开。命令行和监听方之间只经数据目录里的文件（配对口令、设备表、监听方的锁和状态），不经宿主。线上格式以 `protocol` 的 `remote` 模块为准，`crates/protocol/tests/fixtures/remote` 里的样例手机端的测试也读，改格式要两边一起改。
- 宿主不管窗口：要界面办的请求（`ClientMsg::Open`、`ClientMsg::Reveal`、`ClientMsg::Layout`）宿主包成 `HostMsg::UiRequest` 转给 `Hello` 里说自己是 `ClientKind::Desktop` 的连接，桌面转到主线程去办，用 `ClientMsg::UiReply` 回话。会话里的程序读写系统剪贴板（OSC 52）也一样：宿主那份 VT 认出请求、按配置决定办不办，包成 `ClientMsg::WriteClipboard`、`ClientMsg::ReadClipboard` 交给最近和那个会话交互过的桌面，宿主自己不碰剪贴板；界面那份 VT 不认这些请求。
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
- 桌面 app 的模块按归属分组。顶层只放应用级的胶水（入口、关于面板、资源、菜单、快捷键、配置、语言、启动计时、提前拉起 shell、`--host` 进程、远程访问）和几个功能模块：终端视图 `terminal_view`、窗口 `window`、连宿主的客户端 `host_client`、设置窗口 `settings`。几处界面共用的 GPUI 部件和小工具（输入框、滚动条、悬停提示、文件图标、系统声音、共用的编辑动作、`hsla`）放进 `ui`，`ui` 不依赖任何功能模块。只被一个功能用的模块放进那个功能的目录：按键翻译和自绘字符在 `terminal_view` 下，开窗口、存档格式和 agent 提醒在 `window` 下。
