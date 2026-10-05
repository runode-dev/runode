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

代码分在 `crates/` 下的几个 crate 里，依赖只能自上而下。目录名直接说明职责，包名是目录名加 `runode-` 前缀（`crates/terminal` 是 `runode-terminal`），只有桌面 app 的包名是 `runode`，让可执行文件仍叫 runode。包名带前缀是因为依赖树里已有 `dirs` 这类同名的第三方 crate，不加前缀会撞名，`cargo -p` 也会有歧义。

| 目录 | 职责 | 可以依赖 |
| --- | --- | --- |
| `paths` | 配置、数据和缓存放在哪：`Dirs::from_env()` 和每个文件的路径 | 只有 std |
| `shared-types` | 各端共用的纯数据：终端帧、网格、分屏布局、agent 状态、终端设置、输入事件 | std、serde |
| `git-status` | 用 git 命令行读仓库的状态和逐行改动 | 只有 std |
| `preview` | 文件预览不碰界面的部分：读文件、判断是文本、图片还是二进制，语法高亮出调色板语义的颜色 | std、syntect、two-face |
| `agent-detect` | 认出终端前台在跑哪个 AI 编程 agent，判断它在干活、空闲还是等用户回答：按前台进程识别、识别规则的格式和求值（内置规则编进二进制）、状态去抖 | shared-types、serde、regex、toml |
| `terminal` | 终端会话：libghostty-vt 状态机接在 shell 的 PTY 上，shell 集成、命令历史；把前台进程、屏幕文字、标题和进度报告交给 `agent-detect` | shared-types、paths、agent-detect、libghostty-vt、portable-pty |
| `completion` | 按 Tab 的命令补全：命令规格、候选排序、生成器 | terminal、shared-types、paths |
| `config` | Ghostty 兼容的配置文件、主题、快捷键写法和配置模板，生成 `TermSettings` | shared-types、paths |
| `desktop` | GPUI 桌面 app：窗口、视图、菜单、窗口存档和 Info.plist；打包脚本按 `crates/desktop#` 找它的构建产物 | 以上全部、GPUI |

不变量：

- 只有 `desktop` 能依赖 GPUI（`gpui-pre`、`gpui-pre-platform`）；libghostty-vt 和 portable-pty 只有 `terminal` 能直接依赖，对外的接口一律用 `shared-types` 的类型；syntect 和 two-face 只有 `preview` 能直接依赖。这几条由 `deny.toml` 守着，CI 里跑 `cargo deny check bans`。
- `shared-types` 只放数据，不依赖其他 runode crate，也不依赖终端仿真或界面；`paths`、`git-status`、`preview` 不依赖任何 runode crate。
- `agent-detect` 只依赖 `shared-types`，不碰终端仿真、PTY 和界面：屏幕文字、前台进程组由 `terminal` 读好了交给它，用户规则目录由调用方从 `paths` 取来传进去。内置规则文件的出处和许可写在它的 `LICENSE-rules` 里。
- 家目录和 runode 自己的配置、数据、缓存路径一律经 `paths` 取，不在别处读 HOME 或自己拼路径；别的程序的文件（比如 shell 的历史）按那个程序的规矩找。
- 新依赖先加进根 `Cargo.toml` 的 `[workspace.dependencies]`，各 crate 用 `xxx.workspace = true`；lint 规则在 `[workspace.lints]`，每个 crate 都写 `[lints] workspace = true`。

以后要加的 crate 放在这些位置，命名沿用同样的规则：

- `protocol`（宿主和各个前端之间的消息）：和 shared-types 同层，只依赖 shared-types 和 serde。
- `host`（不带界面、管终端会话的宿主进程）：在 terminal、completion、config 之上，不依赖 GPUI。
- `cli`、`tui`、`mobile`（各个前端）：和 desktop 同层，经 protocol 跟宿主说话，不依赖 GPUI，也不直接依赖 libghostty-vt。

加了这些 crate 后，相应地更新 `deny.toml` 的 `wrappers` 和上面这张表。
