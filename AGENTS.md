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

代码分在 `crates/` 下的几个 crate 里，依赖只能自上而下：

| crate | 职责 | 可以依赖 |
| --- | --- | --- |
| `runode-dirs` | 配置、数据和缓存放在哪：`Dirs::from_env()` 和每个文件的路径 | 只有 std |
| `runode-model` | 各端共用的纯数据：终端帧、网格、分屏布局、agent 状态、终端设置、输入事件 | std、serde |
| `runode-git` | 用 git 命令行读仓库的状态和逐行改动 | 只有 std |
| `runode-term` | 终端会话：libghostty-vt 状态机接在 shell 的 PTY 上，shell 集成、命令历史、默认配色 | model、dirs、libghostty-vt、portable-pty |
| `runode-completion` | 按 Tab 的命令补全：命令规格、候选排序、生成器 | term、model、dirs |
| `runode-config` | Ghostty 兼容的配置文件、主题、快捷键写法和配置模板，生成 `TermSettings` | model、dirs |
| `runode`（app，目录名不能改，打包脚本按它找产物） | GPUI 窗口、视图、菜单、窗口存档和 Info.plist | 以上全部、GPUI |

不变量：

- 只有 `runode` 能依赖 GPUI（`gpui-pre`、`gpui-pre-platform`）；libghostty-vt 和 portable-pty 只有 `runode-term` 能直接依赖，对外的接口一律用 `runode-model` 的类型。这两条由 `deny.toml` 守着，CI 里跑 `cargo deny check bans`。
- `runode-model` 只放数据，不依赖其他 runode crate，也不依赖终端仿真或界面；`runode-dirs`、`runode-git` 不依赖任何 runode crate。
- 家目录和 runode 自己的配置、数据、缓存路径一律经 `runode-dirs` 取，不在别处读 HOME 或自己拼路径；别的程序的文件（比如 shell 的历史）按那个程序的规矩找。
- 新依赖先加进根 `Cargo.toml` 的 `[workspace.dependencies]`，各 crate 用 `xxx.workspace = true`；lint 规则在 `[workspace.lints]`，每个 crate 都写 `[lints] workspace = true`。

以后要加的 crate 放在这些位置：

- `runode-protocol`（宿主和各个前端之间的消息）：和 model 同层，只依赖 model 和 serde。
- `runode-host`（不带界面、管终端会话的宿主进程）：和 term、completion、config 同层或在它们之上，不依赖 GPUI。
- `runode-cli`、`runode-tui`（命令行和终端界面前端）：和 app 同层，经 protocol 跟宿主说话，不依赖 GPUI，也不直接依赖 libghostty-vt。

加了这些 crate 后，相应地更新 `deny.toml` 的 `wrappers` 和上面这张表。
