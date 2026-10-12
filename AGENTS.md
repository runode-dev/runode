# Crate 分层

代码分在几个 crate 里，依赖只能自上而下。能单独运行的 app 放在 `apps/` 下（Rust 的 `apps/desktop` 和 Swift 的 `apps/ios`），给它们用的库放在 `crates/` 下。根目录的 `skills/` 放教 agent 用 runode 的 skill：`skills/runode/SKILL.md` 讲操作别的终端，不是每次都用得上的细节放在 skill 下的 `references/<主题>.md`，由 `SKILL.md` 链过去；要装的文件都列在 `skills/files.txt`，`cli` 构建时照它把文件编进二进制（`FILES`），加 skill 或 reference 只加文件、改清单，不改代码（测试核对 `skills/` 下的文件都在清单里、`SKILL.md` 链到的也都在）。`cli` 的 `runode setup` 装成 Claude Code 和 Codex 的 skill：装的是 GitHub 上 main 里最新的一套（`SKILLS_URL`，先下清单再照着下文件），下不到时用编进二进制的；桌面 app 启动时把装过的换成最新的，所以只改 skill 推到 main 就到了用户那里，不用发版。skill 下只能放 `SKILL.md` 和 `references/*.md`，下到的清单里有别的路径时整套不用，免得文件写到 skill 目录外面去。装过哪些文件记在 skill 目录下的 `.runode-files`，下到的清单里去掉的文件下次装时删掉（下不到、装的是编进去的那套时不删）。`apps/` 下不全是 Rust 项目，新加的 Rust app 要在根 `Cargo.toml` 的 `members` 和 `default-members` 里列出。目录名直接说明职责，包名是目录名加 `runode-` 前缀（`crates/terminal` 是 `runode-terminal`），只有桌面 app 的包名是 `runode`，让可执行文件仍叫 runode。包名带前缀是因为依赖树里已有 `dirs` 这类同名的第三方 crate，不加前缀会撞名，`cargo -p` 也会有歧义。

| 目录 | 职责 | 可以依赖 |
| --- | --- | --- |
| `paths` | 配置、数据和缓存放在哪：`Dirs::from_env()` 和每个文件的路径，以及覆盖写用户手写过的文件（配置文件）的 `replace_file`：先写临时文件再改名换上，顺着符号链接写 | 只有 std |
| `shared-types` | 各端共用的纯数据：终端帧、网格、分屏布局、agent 状态、会话的标识（`SessionId`）和对外公布的状态、任务板上的任务和定时（`todo`）、一个目录里能跑的项目命令（`project_tasks`）、终端设置、读写剪贴板的规矩（`clipboard-write`、`clipboard-read` 的取值和一次读写的上限）、输入事件 | std、serde |
| `protocol` | 宿主进程和各个前端之间的消息：帧格式、控制消息、会话标识，请宿主在会话所在的仓库里读写 git 的消息（`git`），列项目命令、读写任务板的消息（`ListProjectTasks`、`ListTodos` 这几条，任务、定时和项目命令的数据在 `shared-types`，这里转出去），卡片在哪一列和拖拽放下时做什么（`todo` 的 `column`、`drop_action`），agent 等回答、任务做完时推给手机的通知的格式（`push`：通知内容、APNs payload 和请求头里的值、中转请求体，等回答的走中转的 v2 接口，任务做完的走 v3），以及手机经网络连上来时的门禁（`remote`：门禁消息、签名的字节串、base64url、配对 URI，是远程访问线上格式的正式定义） | shared-types、serde、serde_json |
| `git` | 用 git 命令行读仓库的状态、逐行改动、分支、stash 和提交图，也做暂存（含按块暂存）、丢弃、提交、切换分支、stash、新建和删除工作树，以及与远端同步这些操作 | 只有 std |
| `todo` | 任务板的库和定时：任务和定时存在一个 SQLite 文件里（`Store`：增删改查、排序、编号，旧库升级，比自己新的库不碰），定时按本地的墙上时间算下一次到点（`next_fire`）；宿主拿着它开任务，别的程序也能直接读写，但宿主在跑时改任务板仍经宿主的消息，它才会告诉在看任务板的前端 | shared-types、rusqlite、libc |
| `project-tasks` | 一个目录里能跑的项目命令：runode 根目录 `tasks.json` 里自己加的、往上最近的 Makefile 的目标和 package.json 的 scripts，拼好在这个目录里能直接跑的命令行（`list`） | shared-types、paths、serde_json |
| `preview` | 文件预览不碰界面的部分（预览栏的界面在 `preview-panel`）：读文件、判断是文本、图片还是二进制，语法高亮出调色板语义的颜色，把 Markdown 解析成块结构（标题、段落、列表、引用、代码块、表格、图片等，照 GitHub 认提示块、脚注、裸网址链接和 README 里常用的 HTML（图片、标题、链接、居中），给标题算锚点） | std、syntect、two-face、pulldown-cmark、regex |
| `agent-detect` | 认出终端前台在跑哪个 AI 编程 agent，判断它在干活、空闲还是等用户回答：按前台进程识别、识别规则的格式和求值（内置规则编进二进制）、状态去抖 | shared-types、serde、regex、toml |
| `command-history` | 命令历史：shell 自己的历史文件，加上 runode 记下的命令（带目录和退出码）；宿主记、界面读，输入时的灰字建议和补全里各命令的常用程度从这里来 | shared-types、paths、serde、serde_json |
| `terminal` | 终端会话：libghostty-vt 状态机接在 shell 的 PTY 上，shell 集成，认出运行完的命令交给 `command-history`；把前台进程、屏幕文字、标题和进度报告交给 `agent-detect` | shared-types、paths、agent-detect、command-history、libghostty-vt、portable-pty |
| `completion` | 按 Tab 的命令补全：命令规格、候选排序、生成器；字宽由调用方给（`CellWidth`），不碰终端仿真 | command-history、shared-types、paths |
| `prompt-highlight` | 提示符上输入的语法高亮：把命令行分成命令名、关键字、选项、字符串、变量、路径等几类，按 fast-syntax-highlighting 的默认主题定色；命令名和子命令借 `completion` 查 | completion、paths |
| `config` | Ghostty 兼容的配置文件、主题、快捷键写法和配置模板，生成 `TermSettings` | shared-types、paths |
| `host` | 管终端会话的宿主（只有 lib）：每个会话一个线程，持有 PTY 和权威的那份 VT，应答终端查询、认标题和 agent、记命令历史。跑在 app 进程里，或者由 app 拉起成单独一个进程（`runode --host`，配置项 `terminal-host`）；前端一律经一条连接按 protocol 的帧和它说话：桌面在同一个进程里时用 `Host::connect_pair` 的一对 socket，别的时候连 Unix socket。app 升级后，新版本拉起的新宿主（`Host::take_over`）以 `ClientKind::Successor` 连上旧宿主的 socket，接过各会话的 PTY 和监听的 socket，会话不断。交出会话的旧宿主留到它拉起的 shell 都结束才退出：macOS 按 shell 的责任进程（拉起它的宿主）认本地网络权限，宿主一退出，shell 里的程序就连不上局域网；宿主自己也得是自己的责任进程，否则挂在拉起它的进程名下（`make restart` 的脚本从终端里拉起新宿主时就是旧宿主），那个进程一退出就全被拦，所以宿主一启动就带 disclaim 原地重新执行自己一次（`disclaim_responsibility`），谁拉起的都一样；责任进程退出后系统的查询接口返回进程自己，看着和自己担着一样，验的时候看宿主 fork 出的子进程算在谁名下；为了让升级后的系统仍把旧宿主认作 Runode，打包时每个版本的程序都写同一个 Mach-O UUID（按架构定），发布流程核对它；前端（手机）请它在某个会话所在的仓库里读写 git 时，经 `git` 办；手机新建工作区时浏览电脑上的目录也由它列出来，没给目录时列 `paths` 给的家目录；手机会话卡片上能跑的 Makefile 目标和 package.json scripts 也由它经 `project-tasks` 列出来；任务板（派给 agent 的任务和定时）也由它持有：存在 `paths` 给的 SQLite 库里（经 `todo` 读写，开任务的是 `host` 的 `todos`），拿着监听的宿主打开（`Host::serve_todos`），开始任务时在任务目录或给它新开的工作树里开会话、等提示符出来再打 agent 的命令（提示词经环境变量交给 shell），到点的定时开新任务，完成了的任务的工作树和已合并的分支随后清掉，有定时时单独跑的宿主不因空闲退出，开始交出会话以后不再改任务板 | terminal、command-history、protocol、shared-types、git、paths、todo、project-tasks、libc、serde_json |
| `remote-access` | 远程访问：TLS 1.3 监听（rustls，ring 后端，自签证书用 rcgen 生成）、门禁（验签、配对口令、限速）、设备表（含手机登记的推送 token）、Bonjour 公布，过了门禁的连接经调用方给的闭包接到宿主上；agent 等回答、任务板上的任务做完时经同一个闭包连宿主轮询会话和任务，给手机推通知（直连 APNs 用 ring 签 JWT，或经官方中转；外调 curl 发 HTTP/2）；也给命令行用的配对口令文件、设备表和监听方状态 | protocol、shared-types、paths、rustls、rcgen、ring、libc、serde、serde_json |
| `update` | 桌面 app 的自动更新：读 GitHub 上最新 Release 里的版本清单 `latest.json`、比版本号，经 NSURLSession 下载这台 Mac 架构的 zip、解压到装着的 .app 旁边，核对新包是 Developer ID 签的、和在跑的这份出自同一个 Team ID、同一个 bundle id，app 退出时再核对一次、用 `renamex_np` 原子地换上 | serde、serde_json、tracing、libc、objc2、block2、objc2-foundation |
| `autostart` | 登录时自启：macOS 写 launchd 的 LaunchAgent，Linux 写 systemd 的用户服务，能拉起无界面的宿主（`runode --host`）或桌面 app（只有 macOS）；装没装看服务文件在不在，命令行（`runode service`）和设置页共用 | paths |
| `ui` | 桌面几处界面共用的 GPUI 部件和小工具：单行、多行输入框（`input`、`textarea`：照 shadcn 分成画边框的组件 `Input`、`Textarea` 和管编辑的 entity `InputState`、`TextareaState`），就地输入框（`inline_edit`），按钮和开关按钮（`button`：照 shadcn 起名的 `Button`，`ButtonVariant` 选样子、`ButtonSize` 选尺寸，和只有图标的 `Toggle`；调用方接着写的样式盖在组件自己的上面），开关（`switch` 的 `Switch`，可以把旁边的文字放进同一个节点），卡片（`card` 的 `Card`）和徽标（`badge` 的 `Badge`，`BadgeVariant` 选样子；两样不一定报给辅助工具，`new` 不要 id，要报时另用它们自己的 `id`），对话框的面板（`dialog` 的 `Dialog`：只管面板本身，遮罩、把下面藏起来不给辅助工具读和焦点由调用方管），浮层的外框（`popover` 的 `Popover`：选择器、状态栏的浮层和菜单都用它，摆在哪、role 和点到外面就关由调用方管），右键菜单和按钮下面的菜单（`menu`，菜单存在哪个视图里由它实现的 `MenuHost` 说），树形列表行尾的一排图标按钮（`row_buttons`，每个是一个 `Button`），面板外框、面板顶上那一条、展开箭头和改动行数的标签（`panel`，卡片样式由调用方传进来；分隔线的颜色是 `Theme::border`），主题 token 和改动颜色（`theme`：照 shadcn 起名的底色、字色、边框色，全 app 一份 `Theme`，每个窗口画的时候一开头按本窗口的配色换上，所以只能在 render 和 paint 里读），标签上关闭按钮的边长、拖动卡片和落点提示（`tabs`），滚动条，悬停提示，界面各档字号（`font_size`，字号经全局的 `UiFontSize` 由 app 设进来），按文件类型区分的图标和部件自己画的图标（连同 SVG，app 的资源要并上 `icons::FILES`），系统声音，共用的编辑动作和关标签、关分屏、放大分屏这几个动作（`actions`），把一组相对路径按目录排成树（`path_tree`，Git 面板和文件树的搜索结果共用），让辅助工具认得界面的共用写法（`a11y`），`hsla`、`base_name`、`follow_move`、`drag_window`；不依赖桌面 app 的功能模块，界面文字由调用方翻译好了交进来 | shared-types、paths、GPUI、objc2-app-kit、objc2-foundation |
| `github-actions` | 右侧面板的 GitHub Actions 页（`ActionsPanel`，单独一个视图）：经用户自己装的 gh 看运行、job 和 step，管工作流、secret 和 variable，状态栏上固定的工作流（`render_pinned`）；仓库、分支、PATH 和固定的工作流由 app 经 `set_input` 交进来，在终端里跑 gh、开文件、存档经 `ActionsEvent` 交给 app；页上文字的翻译在它自己的 locales 里，状态图标出自 vscode-github-actions（MIT） | shared-types、ui、GPUI、rust-i18n、serde、serde_json |
| `preview-panel` | 窗口右边的预览栏（`PreviewPanel`，单独一个视图，一个窗口一个）：文件、diff 和 Markdown 排版的标签，行号旁的改动标记、按行选中、自动换行，标签的右键菜单存在它自己身上；读文件、高亮和解析 Markdown 经 `preview`。各 workspace 的标签都在它里面，当前是哪个 workspace 由 app 经 `set_workspace` 说，git 状态经 `set_git`、字体栏宽和预览字号经 `set_input` 交进来，颜色和语法高亮的 ANSI 色画的时候从 `ui` 的主题读；文件树定位、出现收起和放大还原、开关要存档、把路径打进终端、按块暂存经 `PreviewEvent` 交给 app；字在终端里占几格由 app 给（`cell_width`）；页上文字的翻译在它自己的 locales 里 | shared-types、preview、git、ui、GPUI、rust-i18n、futures |
| `cli` | 命令行前端（`runode list`、`read`、`send`、`wait`、`open`、`kill`、`focus`、`todo`、`schedule`、`setup`、`remote`、`service`）：经宿主的 Unix socket 按 protocol 说话，列会话、读屏幕、发输入、等 agent，请 app 开终端、切到终端，在任务板上加、开始、完成和删掉任务，加、列出和删掉定时（定时的钟点经 libc 按本机本地时间换算）；`setup` 把根目录 `skills/` 里的 skill 装给 agent；`service` 不经宿主，经 `autostart` 装上、去掉登录时自启；`remote` 不经宿主，经 `remote-access` 给手机配对、列出和撤销设备，配对成了以后问用户要不要在配置里打开 `terminal-host`（经 `config` 改配置文件），让退出 app 后远程访问留在后台 | protocol、shared-types、paths、config、remote-access、autostart、qrcode、libc |
| `desktop` | GPUI 桌面 app：窗口、视图、菜单、窗口存档和 Info.plist；带子命令启动时交给 `cli`、带 `--host` 时是单独一个进程的宿主、带 `--host --take-over` 时是升级时接手旧宿主会话的新宿主，和命令行、宿主是同一个可执行文件；远程访问的监听开在宿主所在的那个进程里（`remote_access`）；右侧面板的 GitHub Actions 页（`github-actions`）要看日志、触发工作流、填 secret 的值时在分出的终端里交给 gh，固定的工作流显示在状态栏左边；右边的预览栏（`preview-panel`）由 `window` 的 `preview` 接进窗口：交给它当前 workspace 和 git 状态，替它定位文件树、打路径、按块暂存，存它的开关；铺满窗口的任务板（`todo_board`）照宿主给的任务和会话状态把卡片分成四列，拖拽就是做那一列的动作，和它平级的一页列定时，任务会话在这里打开时才接进窗口；打包脚本按 `apps/desktop#` 找它的构建产物 | 以上全部（含 host、protocol、cli、remote-access）、GPUI |

终端仿真用的 libghostty-vt 不在 `crates/` 下，取自子模块 `vendor/libghostty-rs`。它是 runode-dev 对 Uzaaft 的 libghostty-rs（MIT 或 Apache-2.0）的 fork，runode 用它的 `main` 分支，里面有安全封装 `libghostty-vt` 和 C 接口绑定 `libghostty-vt-sys`；绑定由 bindgen 生成后提交在那边，重新生成时在 fork 的一份单独克隆里设 `GHOSTTY_SOURCE_DIR` 指向 runode 的 `vendor/ghostty`，跑 `cargo run -p libghostty-vt-sys --features bindgen-tool --bin gen-bindings`；别在子模块里跑，`vendor/rustfmt.toml` 关掉了 vendor 下的格式化，生成的绑定不会排版。它自成一个 workspace，根 `Cargo.toml` 按路径依赖它，`vendor` 在 `exclude` 里，所以本仓库的 lint 不管它。`.cargo/config.toml` 设的 `GHOSTTY_SOURCE_DIR` 让 `libghostty-vt-sys` 的构建脚本从 `vendor/ghostty` 编出库，不去网上拉。runode 要用而上游还没有的接口加在 `main` 上，一样东西一个提交，好在以后变基；上游已有同样能力、只是名字或形状不同的，改 runode 去适应，不改 fork。

同步 ghostty 上游时两个子模块一起动。`vendor/ghostty` 是 runode-dev/ghostty 的 fork，等于上游 main 加几个 runode 的修复提交，一处修复一个提交，彼此独立：把这组提交一起变基到上游最新，用 `git push --force-with-lease` 推上 fork；上游自己修好了哪处，就删掉对应的提交。同一次同步里，在 `vendor/libghostty-rs` 里把 `libghostty-vt-sys` 构建脚本的 `GHOSTTY_COMMIT` 改成 ghostty fork 所基于的那个上游 commit，用 `gen-bindings` 重新生成绑定，提交并推上 fork 的 `main`；要跟进 Uzaaft 的上游时，也是把 `main` 变基到上游的 `master` 上。最后在 runode 里一起挪两个子模块的指针，绑定有变化就看 `terminal` 要不要跟着改。两边必须对齐，因为构建时编的是 `vendor/ghostty` 的源码，绑定却是照 `GHOSTTY_COMMIT` 那版头文件生成的，不一致时结构体布局可能对不上，编得过，运行时却读写错内存。

GPUI 取自 crates.io 上的 gpui-pre（Zed 的 gpui 的快照），只有它的 macOS 平台层 `gpui-pre-macos` 换成了 `vendor/gpui-pre-macos` 下的一份副本，由根 `Cargo.toml` 的 `[patch.crates-io]` 指过去。它不是子模块：先把 crates.io 上那一版的源码原样复制进来提交一次，runode 的改动在这之后一处一个提交，都只动这个目录。它和别的 vendor 一样在 workspace 的 `exclude` 里，本仓库的 lint 和 `vendor/rustfmt.toml` 都不管它；作为路径依赖 cargo 不再压掉它的警告，所以它自己的清单里放行了 `deprecated`（cocoa 的弃用警告）。`deny.toml` 里它本来就是 `gpui-pre` 的 wrapper，只有 `desktop` 和它的界面库能依赖 GPUI 的规则不变。runode 的改动现在只有一处：空闲时停掉 CVDisplayLink。原版窗口看得见时 display link 一直按刷新率回调，什么都不画也每秒把进程唤醒几十上百次；现在实现了 `PlatformWindow::frame_waker`，100 毫秒里既没画也没人要帧就退订 link（Claude Code 干活时平均约 75 毫秒出一帧，短了会每帧启停一次 link、每次新建一个线程），GPUI 再要帧时经它重新订阅、并马上在主队列画一帧。升级 gpui-pre 时：根 `Cargo.toml` 里 gpui-pre 的 `=` 版本和这份副本的版本要一起改（不一致时 cargo 只警告 patch 没用上，照旧用 crates.io 上的那份）；把新版本的源码原样复制成一个提交，再把之后只动这个目录的提交逐个重放上去，上游已经做了同样的事（macOS 自己实现了 `frame_waker`、空闲时不再空转）就删掉对应的提交，一处都不剩时删掉副本和 patch。

不变量：

- 只有 `desktop` 和它的界面库 `ui`、`github-actions`、`preview-panel` 能依赖 GPUI（`gpui-pre`；`gpui-pre-platform` 只有 `desktop`）；`libghostty-vt` 和 portable-pty 只有 `terminal` 能直接依赖，`libghostty-vt-sys` 只有 `libghostty-vt` 能直接依赖；syntect 和 two-face 只有 `preview` 能直接依赖；rustls、rcgen 和 ring 只有 `remote-access` 能直接依赖；rusqlite 只有 `todo` 能直接依赖。这几条由 `deny.toml` 守着，CI 里跑 `cargo deny check bans`。
- `shared-types` 只放数据，不依赖其他 runode crate，也不依赖终端仿真或界面；`paths`、`git`、`preview`、`update` 不依赖任何 runode crate。
- `protocol` 只依赖 `shared-types`，不碰终端仿真、PTY 和界面。消息里用到的类型，别的 crate 也要用的（网格尺寸、agent 状态、会话公布的状态等）放 `shared-types`，只在协议里用的（帧、连接方式等）放 `protocol` 自己；`protocol` 把消息里用到的 `shared-types` 类型转出去，前端照旧从 `protocol` 引。
- `agent-detect` 只依赖 `shared-types`，不碰终端仿真、PTY 和界面：屏幕文字、前台进程组由 `terminal` 读好了交给它，用户规则目录由调用方从 `paths` 取来传进去。内置规则文件的出处和许可写在它的 `LICENSE-rules` 里。
- `host` 不依赖 GPUI，也不直接依赖 libghostty-vt 和 portable-pty：VT 和 PTY 经 `terminal` 的 `HostSession` 用。一个终端有两份 VT，宿主那份（`HostSession`）是权威的，只有它应答终端查询；界面那份（`Session`）只消费同样的字节流，改 VT 状态的操作（改尺寸、清屏、换主题）一律经宿主在输出流里标出位置后两边一起做。现在只有 `desktop` 能直接依赖 `host`（建进程内的宿主、跑 `runode --host`），由 `deny.toml` 守着；它和宿主说话也只经 `protocol`，不碰宿主的内部。别的前端经 `protocol` 连 `paths` 的 `host_socket_file` 上的 socket。
- `cli` 只经 socket 和宿主说话，不依赖 `host`、`terminal` 和 GPUI。`deny.toml` 连测试依赖一起查，所以它的测试也不起真宿主，对着 `tests/common` 里按 protocol 回话的假宿主跑；宿主那边的 socket 由 `host` 自己的测试管。宿主给每个 shell 设 `RUNODE_SESSION`、`RUNODE_SOCKET`（`protocol` 的 `ENV_SESSION`、`ENV_SOCKET`），桌面另设 `RUNODE_BIN`，命令行据此找到开它的那个 app 和自己所在的会话。
- 命令行的命令或选项一改（加、删、改名、改写法或说明），同一个提交里改齐三处：`cli` 的用法说明 `HELP`、`completion` 里 runode 自己的命令规格（`specs/runode.json`，有动态候选的还有 `runode_cli` 里的生成器）、教 agent 用 runode 的 skill（根目录 `skills/runode/SKILL.md` 和它的 `references/`）。只给别的程序调的命令在规格里标 `hidden`，skill 里不写。`HELP` 和规格里的命令对不上时，`cli` 的测试 `help_and_completion_spec_list_the_same_commands` 会失败，但它不核对选项，skill 也没有测试守着，要自己记得改。
- `remote-access` 不依赖 `host`：门禁过了的连接经调用方给的闭包（`Connect`，通常是 `Host::connect_pair`）接到宿主上，它在两边搬字节，检查第一帧是 `client` 为 `mobile` 的 `Hello`，之后按帧头筛手机发来的控制帧：`Shutdown`、交接的几种消息、`UiReply`、`SetOptions`、`SetTheme` 不转给宿主，在发往手机的方向、两帧之间回一条 `HostMsg::Error`；登记推送的 `PushRegister` 也不转给宿主，由它按这台设备记进设备表，同样在两帧之间回 `HostMsg::Done`（输入等别的帧不解析，原样转）。宿主直接收到它时回 `Error`。推送通知的一方（`push`）跟着监听：只有占着监听的进程推，它作为普通的 `ClientKind::Cli` 前端经 `Connect` 连宿主、只发 `ListSessions`、`ReadScreen` 和 `ListTodos`（宿主在 `Welcome` 的 `todos` 里说认得任务板时才发，回来的 `TodosChanged` 不理），不 `Attach`（不改 `SessionInfo::clients`），没有登记或关了推送时不连。任务做完的通知经中转时发到 `/v3/notify`（请求体 `v` 为 3、带 `kind`），等回答的照旧发到 `/v2/notify`、请求体不变：旧中转不认得 `kind`，发到 v2 会被拼成等回答；旧中转对 v3 回 404，算推不了、不重试。监听开在宿主所在的进程里：宿主跑在 app 里时由 app 按配置开关，单独跑时由 `runode --host` 自己读配置开关；app 连着单独跑的宿主时不开。命令行和监听方之间只经数据目录里的文件（配对口令、设备表、监听方的锁和状态），不经宿主。线上格式以 `protocol` 的 `remote` 模块为准，`crates/protocol/tests/fixtures/remote` 里的样例手机端的测试也读，改格式要两边一起改。
- 宿主不管窗口：要界面办的请求（`ClientMsg::Open`、`ClientMsg::OpenWorkspace`、`ClientMsg::RenameWorkspace`、`ClientMsg::Reveal`、`ClientMsg::Layout`）宿主包成 `HostMsg::UiRequest` 转给 `Hello` 里说自己是 `ClientKind::Desktop` 的连接，桌面转到主线程去办，用 `ClientMsg::UiReply` 回话。界面的布局变了时桌面发 `ClientMsg::LayoutChanged`，连同新布局，宿主转成 `HostMsg::LayoutChanged` 告诉这条连接上问过 `Layout` 的前端（手机据此马上重新要布局），并记下这份布局。没有桌面连着时（app 没开、宿主单独跑着），`Layout`、`Open`、`OpenWorkspace`、`RenameWorkspace` 由宿主照记着的布局自己办（`host` 的 `workspaces`：开终端一律开成工作区末尾的新标签，`Reveal` 仍回 `Error`）；桌面再连上时恢复窗口前用 `Layout` 要回这份布局（桌面自己发的 `Layout` 宿主不转，自己回），把多出来的会话和改过的名字接进存档（`format::adopt`）。退出时关窗口引起的布局变化桌面不报，宿主手里留着退出前的布局；升级交接时这份布局跟着交给新宿主。会话里的程序读写系统剪贴板（OSC 52）也一样：宿主那份 VT 认出请求、按配置决定办不办，包成 `ClientMsg::WriteClipboard`、`ClientMsg::ReadClipboard` 交给最近和那个会话交互过的桌面，宿主自己不碰剪贴板；界面那份 VT 不认这些请求。
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
- 桌面 app 的模块按归属分组。顶层只放应用级的胶水（入口、关于面板、资源、菜单、快捷键、配置、语言、启动计时、提前拉起 shell、`--host` 进程、远程访问、自动更新）和几个功能模块：终端视图 `terminal_view`、窗口 `window`、连宿主的客户端 `host_client`、设置页 `settings`（铺在终端窗口里）。几处界面共用的 GPUI 部件和小工具（输入框、按钮、菜单、面板外框、行尾按钮、滚动条、悬停提示、文件图标、系统声音、共用的编辑动作、界面字号、`hsla`）放在单独的 crate `ui` 里，入口用 `use runode_ui as ui` 接进来，各模块照旧写 `crate::ui::…`；部件要回调视图的写成对视图类型泛型（`RowButton<V>`、`MenuHost`），界面文字由调用方翻译好交进去。只被一个功能用的模块放进那个功能的目录：按键翻译和自绘字符在 `terminal_view` 下，开窗口、存档格式和 agent 提醒在 `window` 下。

# 无障碍

桌面 app 的界面都要让辅助工具读得到、按得动。辅助工具包括 VoiceOver，也包括 CUA 这类读 AX 树的自动化工具。新加或改动的界面在同一个提交里把无障碍一起做完。

- GPUI 只把同时有 `id` 和 `role` 的元素报给辅助工具。名字用 `aria_label`，写翻译过的文字；图标按钮的名字写它做什么，不写图标叫什么。状态用 `aria_selected`、`aria_expanded`、`aria_toggled`、`aria_value`。不可用的元素用 `ui::a11y` 的 `Disable`，被模态对话框盖住的部分用 `Hide`。
- 元素的 `id` 按它显示的东西取（运行、文件、段的名字或 id），不按在列表里的行号：GPUI 按元素 id 给辅助工具的节点编号，读到数据、上面多出几行时行号一变，VoiceOver 的焦点就跳，按下会按到别的行上。悬停这类只管当前一帧的状态仍可以按行号记。
- 能点的东西辅助工具都要按得到。GPUI 默认把辅助工具的按下换成在元素中心合成一次点击，元素滚出可见区域或被盖住时就点空了。点击用 `ui::a11y` 的 `Press`，按下就办的用 `PressDown`；只能自己写鼠标处理时，另挂 `A11yPress::on_a11y_press`。展开、收起、设值、滚动这些操作也要另外用 `on_a11y_action` 登记。
- 自己画出来的内容（canvas、图片、视频帧）要另外给出辅助工具能读、能操作的节点。终端报成文本区域。macOS 把图片当叶子节点，挂在它下面的子节点报不出去，所以要摆子节点的容器用 `Role::Group`。
- 只给辅助工具用、算起来又费事的节点，看 `Window::is_a11y_active` 再决定画不画，没开时不画。比如 Git 面板里每行都画出的按钮。
- 缓存的视图（`.cached()`）照搬上一帧时跳过 prepaint，报给辅助工具的节点就丢了，所以辅助工具开着时不缓存，见窗口的 `Slot` 和终端分屏。
- 出错、空状态、加载中这些只起提示作用的文字，用 `Role::Label` 报出去；`project` 的 `panel_message` 已经这样做了。
- 改完用 CUA 的 `get_window_state` 看一遍 AX 树：每个按钮都要有名字，用 `AXPress` 按得动；状态变了，树里的状态也跟着变。列表要在数据读进来、行数变了以后再按一遍下面的行，只按第一行看不出节点编号跟着行号变的问题。

# 界面字号

桌面 app 界面上的字只用 `ui::font_size` 的几档 `FontSize`，不写 `text_size(px(..))`；终端和预览栏的正文另有字号配置，不在此列，挂在预览栏下面的标签条这类界面要自己设回 `FontSize`。窗口的 rem 设成配置项 `ui-font-size`（限在 11 到 20），各档都按 rem 写，改了配置整个界面一起等比缩放。

- 要装下文字的行高、栏高、按钮高和定宽的文字栏用 `scaled(点数)`，点数按基准字号 12 写；图标、留白、分隔线和装字符图标（×、+、转圈）的定宽方框仍写 `px`。标题栏、标签栏、分屏标题条和状态栏的高度跟着 macOS 窗口按钮定死，也写 `px`。
- 拿这些长度做算术（估宽度、拖动的上下限、拖动时的预览）时乘 `font_size::scale(cx)`，或者先把可用宽度除以它，折成基准字号下的点数再比。
- 列表记着量过的行高（GPUI 的 `list` 和 `ListState`）时，配置一变就要让它重新量。

# 重画

终端一有输出、转圈每动一格，GPUI 就把通知的视图和它上面的各层视图标成要重画。`WindowView` 在最上面，几乎每帧都要重画，所以它自己直接画的东西要少，别的都放进缓存的视图。

- 窗口里不跟着终端输出变的几块（侧栏、标题栏、预览栏、右侧面板、状态栏）各是一个缓存的视图（`window` 的 `Slot`），终端也是，没变时照搬上一帧。新加的窗口外框界面放进已有的一块，或者在 `Slots` 里加一块。缓存的视图按自己的根元素排版，父元素的 flex 拉伸撑不到它，根元素要自己写满宽高。
- `Slot` 只在 `WindowView` 自己通知时重画，它读的终端状态变了不会让它重画，所以要有一条路让 `WindowView` 通知：标题、agent、目录、前台程序这些会话状态经 `TerminalEvent::MetaChanged`，配色经 `follow_colors`。读终端的别的状态之前，先确认它变了时会走到 `WindowView` 的通知。
- 会自己动的东西（动画、倒计时）做成单独的小视图，像标题栏的 `Spinner`，动一下只通知它自己。挂在 `WindowView` 直接画的元素上，每动一下都会通知 `WindowView`，所有 `Slot` 跟着重画。这种小视图别用 `use_keyed_state` 存，它会把小视图的通知转给外层视图。
- render 里发现别的视图要重画时，用 `cx.defer` 等这一帧画完再通知：画的时候发的通知不会再要一帧，要等别的东西触发下一次重画才生效。
- 改了重画相关的代码，在真 app 里量：同一个窗口布局下交替跑改前、改后的版本，窗口放在最前面，用 `top` 看界面进程的 CPU，用 `sample` 看主线程的时间花在哪。CUA 一读 AX 树，辅助工具模式就打开了，`Slot` 和终端都不再缓存，所以验缓存时截图用 `screencapture -l`、点击用 CGEvent，不读 AX 树；无障碍另外用 CUA 验。
