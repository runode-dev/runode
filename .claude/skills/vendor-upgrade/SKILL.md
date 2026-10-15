---
name: vendor-upgrade
description: runode 升级 vendor 依赖的流程：同步 ghostty 上游（vendor/ghostty 的 fork 变基、libghostty-rs 的 GHOSTTY_COMMIT 和绑定一起挪）、跟进 Uzaaft 的 libghostty-rs、升级 gpui-pre 并重放 vendor/gpui-pre-macos 上的改动。核对上游的每个新提交：上游修了 runode 自己修过的问题就删掉我们的提交、改用上游的；上游加了新功能或改了行为要单独醒目地提醒用户。只要用户提到升级/同步/更新 vendor、子模块、ghostty、libghostty、libghostty-rs、gpui、gpui-pre，或者问上游有什么新东西、我们的 fork 补丁还要不要，都用这个 skill，哪怕没说"升级"二字。
---

# 升级 vendor 依赖

runode 的 vendor 下有三样东西，各自的来历和不变量写在根 `AGENTS.md`（"终端仿真用的 libghostty-vt…"、"同步 ghostty 上游时…"、"GPUI 取自…"三段），动手前先读那三段，这里不重复，只讲怎么升、怎么核对。

| 目录 | 是什么 | 上游 | runode 的改动在哪 |
| --- | --- | --- | --- |
| `vendor/ghostty` | 子模块，runode-dev/ghostty 的 fork | `upstream` = ghostty-org/ghostty 的 `main` | fork `main` 上基于上游的几个修复提交，一处一个 |
| `vendor/libghostty-rs` | 子模块，runode-dev/libghostty-rs 的 fork | `upstream` = Uzaaft/libghostty-rs 的 `master` | fork `main` 上的提交：新接口、修复、`GHOSTTY_COMMIT` 和重新生成的绑定 |
| `vendor/gpui-pre-macos` | 普通目录，crates.io 上 gpui-pre-macos 某版的原样副本 | crates.io 的 gpui-pre（Zed 的 gpui 快照） | runode 仓库里只动这个目录的提交 |

用户没说升哪个时，先跑摸底（下面第一步）看哪边有更新，把结果讲给用户再动手。ghostty 和 libghostty-rs 必须一起动，不能只挪一个：构建编的是 `vendor/ghostty` 的源码，绑定却是照 `GHOSTTY_COMMIT` 那版头文件生成的，两边不一致时编得过，运行时却读写错内存。

## 总的做法

1. **摸底**。ghostty 这边跑 `scripts/ghostty-survey.sh`（本 skill 目录下，可传目标上游 commit，默认 `upstream/main`），它会 fetch 两个子模块，列出 fork 基于哪个上游、`GHOSTTY_COMMIT` 和 nix 输入是否对齐、上游新增的提交按"C 接口 / 终端核心 / 构建 / 其余"分好组、我们每个修复提交碰的文件上游有没有人动过、每个修复带的测试名，以及 libghostty-rs 的 fork 提交和 Uzaaft 上游的新提交。脚本报警告（没对齐、工作区不干净、HEAD 不等于 origin/main）时先停下来跟用户说清楚，不要在一个说不清的状态上继续。gpui 这边见 `references/gpui.md` 的摸底。
2. **核对我们自己的补丁**，见下一节。这是这个 skill 最要紧的部分。
3. **找新功能和行为变化**，见再下一节。
4. **动手**：ghostty 和 libghostty-rs 照 `references/ghostty.md`，gpui 照 `references/gpui.md`。
5. **验证**：fork 各自的测试、runode 的 `make check`、`make test`、`make clippy CLIPPY_ARGS="-- -D warnings"`、`make fmt-check`、`cargo deny check bans`。工具链怎么取（macOS 上 `nix shell <runode 根> -c`）、缺 Metal Toolchain 时怎么办、在 worktree 里怎么初始化子模块，见 `references/ghostty.md` 的"环境"。哪项没跑、为什么没跑，报告里照实写。
6. **报告**，格式见最后一节；推送 fork 之前等用户点头。

## 核对我们自己的补丁：上游修了，就用上游的

我们在 fork 上的每个修复，本来就是替上游先修的。上游自己修了同一个问题时，删掉我们的提交、用上游的修法，哪怕上游的写法和我们的不一样：留着自己的版本，以后每次变基都要和上游那段代码打架，两套修法叠在一起还可能互相抵消或者修过头。反过来，上游没修的就原样保留、变基过去。

判断"上游修没修"不能只看提交标题像不像，要拿证据：

- `git cherry` 标 `-` 的，补丁已经原样进了上游，直接删。
- 标 `+` 的，看脚本列出的"上游动过同样文件的提交"，读它们的 diff 和提交说明（ghostty 的提交说明常写着修的是哪个 issue），判断是不是同一件事。
- 最有力的证据是**拿我们提交里带的测试去跑没有我们修复的新上游**：测试过了，说明上游的修法覆盖了我们的场景，删掉我们的提交；测试失败，说明上游没修或只修了一部分。具体怎么跑见 `references/ghostty.md`。
- 只修了一部分时，不要两套都留：在上游修法之上把我们还需要的那一点重写成一个更小的提交，并在报告里写明上游修了哪部分、我们还补了哪部分。
- 上游的修法和我们的意图相反（比如上游认定那是对的行为），或者看不出是不是同一件事时，停下来问用户，不要自己挑。

libghostty-rs 也是同一个道理，规矩在 `AGENTS.md`：Uzaaft 上游已经有了同样能力、只是名字或形状不同的，删掉 fork 上我们的提交，**改 runode 去适应上游的接口**，不改 fork 去迁就 runode。gpui 的副本同理，上游做了同样的事就删掉对应的提交。

还有第三处容易漏的：runode 自己的代码里有些地方跟着 libghostty 的现状写，上游一变它们就该跟着变。有三种：

- **绕着局限写的**，注释里通常会说"libghostty 没有…""还不…"之类（例如 `snapshot_format` 自己编一份快照去读格式版本，因为 libghostty 没单独给出这个数）。新上游给了正式的做法就改用上游的。
- **照抄了 libghostty 的某条规则**，注释里会说"按它…同样的规则"。上游改了规则（尤其是安全修复），runode 的副本要照改，否则 runode 还留着上游已经修掉的漏洞。例如 `Session::paste` 自己判断粘贴安不安全，上游把未开 bracketed paste 时的回车也算成不安全后，这里要跟着改。读上游提交时，看到改的是一条规则，就去 runode 里搜有没有同一条规则的副本。
- **测试里断言了局限**，比如 `replay_loses_what_the_formatter_cannot_express` 列着重放会丢的东西。上游补上了某样，这条断言就会挂；挂了先确认是上游有意的改进，再把它改成"保留"的断言（挪进对应的 `…_keeps_…` 测试），注释一起改。

在 `crates/terminal` 和 `crates/host` 里搜这几类，没把握的列进报告让用户定。动手改之前先看本地 main 上还没推的提交（`git log origin/main..main`），用户可能已经照着上游修过了。这些改动是跟着上游修，不算擅自接新功能，但都要在报告的"行为变化"里写上。

## 新功能和行为变化：单独、醒目地提醒用户

用户明确要求过：上游有新功能时要特别提醒。这些东西不能淹在一长串提交列表里，也不要擅自接进 runode，因为接不接、怎么接是产品决定。把它们挑出来放在报告最前面，每一条说清：

- 它是什么（一句人话，附上游提交号或 PR 号）；
- 是 libghostty-vt 的 C 接口新增，还是只在 Ghostty app 里（runode 用不上的就不用列）；
- libghostty-rs 有没有封装它，要用的话 fork 上要加什么；
- 对 runode 有什么用，或者会不会影响 runode 现有行为。

要挑出来的有这几类，按对 runode 的影响排：

1. **会改变 runode 现有行为的**：宿主那份 VT 是唯一应答终端查询的，上游新增或改了对某个查询的应答（比如开始回 DECXCPR、DECRQM 的结果变了），终端里的程序看到的就变了；粘贴保护、换行、reflow、选区、格式化输出的行为变化也算。这类即使不用做任何事，也要让用户知道。
2. **C 接口的破坏性变化**：改名、删除、常量值变了、结构体字段变了。这些会让 libghostty-rs 或 runode 编不过，或者更糟、编得过但语义变了，必须处理。
3. **新能力**：新的 C 接口、新的回调、新支持的 VT 序列或 OSC，runode 也许想用。
4. **快照格式版本变了**：宿主升级接手时，格式不同会退回重放（`a_different_snapshot_format_falls_back_to_a_replay` 测的就是这个），能用但接手更慢，值得一提。
5. 构建要求变了：Zig 最低版本（要跟着改根 `flake.nix` 的 zig、libghostty-rs 的 `AGENTS.md` 和 CI）。

gpui 这边对应的是 gpui-pre 的公开接口变化和新能力，`desktop` 和 `ui` 可能要跟着改或者能用上。

## 提交信息

runode 仓库里照用户的规范（`chore: 同步 ghostty 上游并更新 vendor 子模块` 这样的中文第一行，正文用 `- ` 要点写清：变基到上游哪个 commit、新增多少提交、哪些修复保留、哪些因为上游修了而删掉、绑定有没有破坏性变化、runode 跟着改了什么）。两个 fork 里照它们各自已有的风格写英文：ghostty 用 `terminal: …` 这类"模块: 小写短句"，libghostty-rs 用 `feat:`/`fix:`/`chore:` 加英文短句，正文同样写原因。派 agent 提交时把这套写法写进提示。

## 推送和确认

改写 fork 的历史是对外的、很难撤回的操作：ghostty fork 变基后要 `git push --force-with-lease`，libghostty-rs 跟进 Uzaaft 时也一样。所以本地全部做完、验证完、报告给了用户之后，**等用户确认再推**。推的顺序是 ghostty fork → libghostty-rs fork → runode，保证 runode 的子模块指针只指向已经推上去的提交。推之前给旧的 fork HEAD 留一个本地备份分支（`runode-backup-<日期>`），出了问题能找回来。runode 仓库本身只在用户要求时提交和推送。

## 报告格式

```markdown
# vendor 升级：<ghostty 05dcf88 → 22fa134 / gpui-pre 0.3.9 → 0.3.10>

## ⚠️ 需要你看的新功能和行为变化
（按上面五类排，每条：是什么、上游提交、libghostty-rs 有没有、对 runode 的影响和建议。没有就写"没有"。）

## 我们的补丁
| 提交 | 处理 | 依据 |
（保留 / 删掉，改用上游 <提交> / 缩小重写；依据写测试结果或读 diff 的结论）

## runode 跟着改了什么
## 验证
（每项命令和结果；没跑的写原因）
## 待你确认后推送
（要推的 fork 分支和 force 与否、备份分支名）
```
