<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **runode** (225 symbols, 587 relationships, 20 execution flows).

> Index stale? Run `node .gitnexus/run.cjs analyze --index-only` from the project root — it auto-selects an available runner. No `.gitnexus/run.cjs` yet? Bootstrap with `npx`, `bunx`, or `pnpm dlx` — e.g. `bunx gitnexus@latest analyze` (npm 11 npx crash; #1939).

## Always Do

- **MUST run impact before editing.** Use `impact({target: "symbolName", direction: "upstream"})` or `node .gitnexus/run.cjs impact "symbolName" --direction upstream --repo .`; report callers, processes, and risk. Never substitute grep for graph analysis.
- **MUST analyze graph changes before committing.** Use `detect_changes({scope: "all"})` (MCP) or `node .gitnexus/run.cjs detect-changes --scope all --repo .` (CLI fallback). `partial: true` or `truncated: true` is not a clean check — a zero means unseen, not unaffected; re-run it. For regression review: `detect_changes({scope: "compare", base_ref: "main"})` or `node .gitnexus/run.cjs detect-changes --scope compare --base-ref "main" --repo .`.
- MUST warn on HIGH/CRITICAL `risk` pre-edit; never use `riskSharedAxes` to waive a HIGH/CRITICAL `risk` warning. Compare File/symbol: MCP File omits axes; Graph-RAG expands File.
- **MUST treat `risk: UNKNOWN` as unresolved, not as low.** An empty caller set is not evidence the symbol is unused — it can also mean the callers are not resolvable by the index (plain-object property access, dynamic dispatch, cross-language calls). `impact` pairs `UNKNOWN` with a `riskNote` saying so. Confirm with a text search before treating the symbol as safe to change or delete; do not proceed on the strength of a zero.
- **MUST use `query({search_query: "concept"})` for concepts/flows, `context({name: "symbolName"})` for a named symbol, or `impact` for blast radius, on read-only callers, dependencies, imports, or execution flow.** Graph first; text search only for empty/`UNKNOWN`/literals.
- For security review, `explain({target: "fileOrSymbol"})` lists taint findings (source→sink flows; needs `analyze --pdg`).

## Never Do

- NEVER edit a function, class, or method before MCP/CLI impact analysis.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis, and never read `UNKNOWN` as an all-clear — it means the walk could not answer, which is the one verdict that requires confirming by other means.
- NEVER rename symbols with find-and-replace — use `rename` which understands the call graph.
- NEVER commit before MCP/CLI graph change analysis.

## Resources

| Resource | Use for |
| --- | --- |
| `gitnexus://repo/runode/context` | Codebase overview, check index freshness |
| `gitnexus://repo/runode/clusters` | All functional areas |
| `gitnexus://repo/runode/processes` | All execution flows |
| `gitnexus://repo/runode/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
| --- | --- |
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->

# Crate 分层

代码分在几个 crate 里，依赖只能自上而下。能单独运行的 app 放在 `apps/` 下（现在只有 `apps/desktop`），给它们用的库放在 `crates/` 下。`apps/` 下不全是 Rust 项目，新加的 Rust app 要在根 `Cargo.toml` 的 `members` 和 `default-members` 里列出。目录名直接说明职责，包名是目录名加 `runode-` 前缀（`crates/terminal` 是 `runode-terminal`），只有桌面 app 的包名是 `runode`，让可执行文件仍叫 runode。包名带前缀是因为依赖树里已有 `dirs` 这类同名的第三方 crate，不加前缀会撞名，`cargo -p` 也会有歧义。

| 目录 | 职责 | 可以依赖 |
| --- | --- | --- |
| `paths` | 配置、数据和缓存放在哪：`Dirs::from_env()` 和每个文件的路径 | 只有 std |
| `shared-types` | 各端共用的纯数据：终端帧、网格、分屏布局、agent 状态、会话对外公布的状态、终端设置、输入事件 | std、serde |
| `protocol` | 宿主进程和各个前端之间的消息：帧格式、控制消息、会话标识 | shared-types、serde、serde_json |
| `git` | 用 git 命令行读仓库的状态、逐行改动、分支、stash 和提交图，也做暂存（含按块暂存）、丢弃、提交、切换分支、stash 和与远端同步这些操作 | 只有 std |
| `preview` | 文件预览不碰界面的部分：读文件、判断是文本、图片还是二进制，语法高亮出调色板语义的颜色 | std、syntect、two-face |
| `agent-detect` | 认出终端前台在跑哪个 AI 编程 agent，判断它在干活、空闲还是等用户回答：按前台进程识别、识别规则的格式和求值（内置规则编进二进制）、状态去抖 | shared-types、serde、regex、toml |
| `terminal` | 终端会话：libghostty-vt 状态机接在 shell 的 PTY 上，shell 集成、命令历史；把前台进程、屏幕文字、标题和进度报告交给 `agent-detect` | shared-types、paths、agent-detect、libghostty-vt、portable-pty |
| `completion` | 按 Tab 的命令补全：命令规格、候选排序、生成器 | terminal、shared-types、paths |
| `highlight` | 提示符上输入的语法高亮：把命令行分成命令名、关键字、选项、字符串、变量、路径等几类，按 fast-syntax-highlighting 的默认主题定色；命令名和子命令借 `completion` 查 | completion、paths |
| `config` | Ghostty 兼容的配置文件、主题、快捷键写法和配置模板，生成 `TermSettings` | shared-types、paths |
| `host` | 管终端会话的宿主（只有 lib）：每个会话一个线程，持有 PTY 和权威的那份 VT，应答终端查询、认标题和 agent、记命令历史。跑在 app 进程里，或者由 app 拉起成单独一个进程（`runode --host`，配置项 `terminal-host`）；前端一律经一条连接按 protocol 的帧和它说话：桌面在同一个进程里时用 `Host::connect_pair` 的一对 socket，别的时候连 Unix socket | terminal、protocol、shared-types、libc |
| `cli` | 命令行前端（`runode list`、`read`、`send`、`wait`、`open`、`kill`、`focus`）：经宿主的 Unix socket 按 protocol 说话，列会话、读屏幕、发输入、等 agent，请 app 开终端、切到终端 | protocol、shared-types、paths |
| `desktop` | GPUI 桌面 app：窗口、视图、菜单、窗口存档和 Info.plist；带子命令启动时交给 `cli`、带 `--host` 时是单独一个进程的宿主，和命令行、宿主是同一个可执行文件；打包脚本按 `apps/desktop#` 找它的构建产物 | 以上全部（含 host、protocol、cli）、GPUI |

不变量：

- 只有 `desktop` 能依赖 GPUI（`gpui-pre`、`gpui-pre-platform`）；libghostty-vt 和 portable-pty 只有 `terminal` 能直接依赖；syntect 和 two-face 只有 `preview` 能直接依赖。这几条由 `deny.toml` 守着，CI 里跑 `cargo deny check bans`。
- `shared-types` 只放数据，不依赖其他 runode crate，也不依赖终端仿真或界面；`paths`、`git`、`preview` 不依赖任何 runode crate。
- `protocol` 只依赖 `shared-types`，不碰终端仿真、PTY 和界面。消息里用到的类型，别的 crate 也要用的（网格尺寸、agent 状态、会话公布的状态等）放 `shared-types`，只在协议里用的（会话标识、帧、连接方式等）放 `protocol` 自己。
- `agent-detect` 只依赖 `shared-types`，不碰终端仿真、PTY 和界面：屏幕文字、前台进程组由 `terminal` 读好了交给它，用户规则目录由调用方从 `paths` 取来传进去。内置规则文件的出处和许可写在它的 `LICENSE-rules` 里。
- `host` 不依赖 GPUI，也不直接依赖 libghostty-vt 和 portable-pty：VT 和 PTY 经 `terminal` 的 `HostSession` 用。一个终端有两份 VT，宿主那份（`HostSession`）是权威的，只有它应答终端查询；界面那份（`Session`）只消费同样的字节流，改 VT 状态的操作（改尺寸、清屏、换主题）一律经宿主在输出流里标出位置后两边一起做。现在只有 `desktop` 能直接依赖 `host`（建进程内的宿主、跑 `runode --host`），由 `deny.toml` 守着；它和宿主说话也只经 `protocol`，不碰宿主的内部。别的前端经 `protocol` 连 `paths` 的 `host_socket_file` 上的 socket。
- `cli` 只经 socket 和宿主说话，不依赖 `host`、`terminal` 和 GPUI。`deny.toml` 连测试依赖一起查，所以它的测试也不起真宿主，对着 `tests/common` 里按 protocol 回话的假宿主跑；宿主那边的 socket 由 `host` 自己的测试管。宿主给每个 shell 设 `RUNODE_SESSION`、`RUNODE_SOCKET`（`protocol` 的 `ENV_SESSION`、`ENV_SOCKET`），桌面另设 `RUNODE_BIN`，命令行据此找到开它的那个 app 和自己所在的会话。
- 宿主不管窗口：要界面办的请求（`ClientMsg::Open`、`ClientMsg::Reveal`、`ClientMsg::Layout`）宿主包成 `HostMsg::UiRequest` 转给 `Hello` 里说自己是 `ClientKind::Desktop` 的连接，桌面转到主线程去办，用 `ClientMsg::UiReply` 回话。
- 家目录和 runode 自己的配置、数据、缓存路径一律经 `paths` 取，不在别处读 HOME 或自己拼路径；别的程序的文件（比如 shell 的历史）按那个程序的规矩找。
- 新依赖先加进根 `Cargo.toml` 的 `[workspace.dependencies]`，各 crate 用 `xxx.workspace = true`；lint 规则在 `[workspace.lints]`，每个 crate 都写 `[lints] workspace = true`。
- 只经 crate 公开接口测的黑盒测试放在和 `src` 同级的 `tests/` 目录，按主题分文件，几个文件共用的辅助放 `tests/common/mod.rs`；测私有实现的单元测试留在 `src` 里的 `#[cfg(test)]` 模块。不为了搬测试把内部的东西改成 pub。`desktop` 是二进制 crate，`tests/` 引用不到它，测试都留在 `src` 里。

以后要加的 crate 放在这些位置，命名沿用同样的规则：

- `tui`、`mobile`（各个前端，放在 `apps/` 下）：和 `cli` 一样，经 protocol 跟宿主说话，不依赖 GPUI，也不直接依赖 libghostty-vt。

加了这些 crate 后，相应地更新 `deny.toml` 的 `wrappers` 和上面这张表。
