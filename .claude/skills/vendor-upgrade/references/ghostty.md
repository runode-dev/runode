# 同步 ghostty 上游和 libghostty-rs

下面的 `$R` 指 runode 根目录的绝对路径（`git rev-parse --show-toplevel`），`$S` 指本会话的 scratchpad 目录，`<old>` 是 fork 当前基于的上游 commit（等于 `GHOSTTY_COMMIT`），`<new>` 是目标上游 commit，摸底脚本都会打出来。命令都在 runode 根目录跑。

## 目录

0. 环境
1. 备份
2. 逐个核对 ghostty fork 上的修复
3. 把保留的修复挪到新上游
4. libghostty-rs：挪 `GHOSTTY_COMMIT`、重新生成绑定
5. libghostty-rs：跟进 Uzaaft 上游
6. runode 这边
7. 推送

## 0. 环境

- **工具链**：用本机装的 zig（版本见 `CONTRIBUTING.md`）和 `rust-toolchain.toml` 指定的 Rust。
- **Metal Toolchain**：`desktop`（包名 `runode`）和 `ui` 编 GPUI 时要用 Xcode 的 Metal 编译器。`xcrun -sdk macosx metal --version` 报 "missing Metal Toolchain" 时，这两个 crate 编不了，和升级无关；不要自己去装（`xcodebuild -downloadComponent MetalToolchain` 改的是系统），先用 `--exclude runode --exclude runode-ui` 把其余 crate 测掉，在报告里写明没测这两个和原因，让用户决定要不要装。
- **在 runode 的 worktree 里做**（用户要求隔离时）：从**本地** main 开（`git worktree add <路径> -b <分支> main`），本地 main 常有还没推的提交，从 `origin/main` 开会漏掉它们，跟着上游改 runode 时可能重复做本地已经做过的事。会话自带的建 worktree 功能默认从 origin 开，而且它的隔离模式不让在子模块里跑 git，所以用 `git worktree add` 自己建。新 worktree 的子模块要自己初始化：
  ```sh
  git submodule update --init --reference <主检出>/vendor/libghostty-rs vendor/libghostty-rs
  git submodule update --init --depth 300 vendor/ghostty   # 主检出的 ghostty 常是浅克隆，不能当 --reference
  git -C vendor/ghostty remote add upstream https://github.com/ghostty-org/ghostty.git
  git -C vendor/ghostty fetch --depth 400 origin main && git -C vendor/ghostty fetch --depth 400 upstream main
  git -C vendor/libghostty-rs remote add upstream https://github.com/Uzaaft/libghostty-rs.git
  git -C vendor/libghostty-rs fetch origin && git -C vendor/libghostty-rs fetch upstream master
  # 两个子模块都：git checkout -B main && git branch -u origin/main
  ```
  深度要够覆盖从 `<old>` 到 `<new>`，不够时摸底脚本算不出 merge-base，再加深。摸底时传 `--no-fetch`，免得它在浅克隆上不带深度去 fetch。
- **提交身份**：新克隆出来的子模块没有 `user.name`、`user.email`，git 会自己编一个 `用户名@主机名` 当提交人。动手前看一眼 fork 历史上的提交人（`git log -1 --format='%cn <%ce>'`），在子模块里 `git config user.name/user.email` 设成一样的。

## 1. 备份

```sh
git -C vendor/ghostty branch runode-backup-$(date +%Y%m%d) HEAD
git -C vendor/libghostty-rs branch runode-backup-$(date +%Y%m%d) HEAD
```

## 2. 逐个核对 ghostty fork 上的修复

对摸底里标 `+` 的每个修复提交，在一个单独的工作树里拿它的测试去跑新上游，**不带它的修复代码**。ghostty 的测试和代码写在同一个 `.zig` 文件里（`test "…" { … }` 块），本 skill 的 `scripts/extract-test-hunks.py` 能从提交里只摘出改测试的 hunk（新加的测试块，以及改了已有测试里期望值的地方）：

```sh
K=<本 skill 目录>
git -C vendor/ghostty worktree add --detach "$S/gh-check" <new>
for c in <标 + 的提交>; do
  git -C vendor/ghostty show --format= $c | python3 -I $K/scripts/extract-test-hunks.py > "$S/tests-$c.patch"
  git -C "$S/gh-check" apply --3way "$S/tests-$c.patch"
done
```

摘之前先 `git show` 看一眼每个提交：测试要是用到了提交里新加的非测试辅助函数，脚本摘不到，要手工补上；上游把那段代码改了名、挪了位置导致测试编不过时，照上游的新写法改测试，意思不变。

然后一次编译跑掉所有这些测试（`-Dtest-filter` 可以给多个，按子串匹配；编译最费时间，合成一次跑能省很多）：

```sh
cd "$S/gh-check"
zig build test-lib-vt --summary all -Dtest-filter="<测试名 1>" -Dtest-filter="<测试名 2>" …
```

`test-lib-vt` 收了 `src/terminal` 下的测试；改动不在 libghostty-vt 里时换成 `zig build test`。失败的测试和第一条断言这样看（输出带颜色码，同一份失败会打两遍）：

```sh
grep -a -A1 "' failed" <日志> | sed 's/\x1b\[[0-9;]*m//g'
```

按提交逐个下结论：

- 它的测试全过：上游修好了，删掉这个提交，报告里写上游是哪个提交修的（多半就在摸底列的"动过同样文件的提交"里）。
- 失败：上游没修。保留。
- 部分过：上游修了一部分，在新上游上把还缺的那部分重写成更小的提交。

核完删掉工作树：`git -C vendor/ghostty worktree remove --force "$S/gh-check"`。

## 3. 把保留的修复挪到新上游

不能用交互式变基，按顺序 cherry-pick 保留的提交，删掉的就不 pick：

```sh
git -C vendor/ghostty checkout -B main <new>
git -C vendor/ghostty cherry-pick <保留的提交，按原顺序>
```

有冲突时解决冲突，保持"一处修复一个提交、彼此独立"：解决时不要把两个修复揉进一个提交，也不要顺手改别的。解决完把每个保留提交带的测试在新的 `main` 上跑一遍，再跑一遍 `zig build test-lib-vt --summary failures` 全套（很慢的话至少把 `src/terminal` 相关的过滤跑掉）。

如果某个保留的修复碰了 `include/`（C 头文件），记下来：nix 构建用的是 `GHOSTTY_COMMIT` 那版上游的头文件，不带我们的改动，两者会不一致，要在报告里提给用户。

## 4. libghostty-rs：挪 `GHOSTTY_COMMIT`、重新生成绑定

`GHOSTTY_COMMIT` 要等于 ghostty fork 所基于的上游 commit，也就是 `<new>` 的完整 SHA。三处一起改：

- `crates/libghostty-vt-sys/build.rs` 的 `const GHOSTTY_COMMIT`；
- `flake.nix` 里 `inputs.ghostty.url` 的那个 SHA（flake 的检查会核对这两处一致）；
- `flake.lock` 里 `ghostty` 节点（有 nix 就 `nix flake update ghostty`；没有 nix 时只动 ghostty 节点的 `rev`、`narHash`、`lastModified`，narHash 要能算对，算不了就停下来告诉用户，别乱填）。

重新生成绑定要在子模块之外的一份单独克隆里跑，因为 `vendor/rustfmt.toml` 关掉了 vendor 下的格式化，在子模块里生成的绑定不排版：

```sh
git clone vendor/libghostty-rs "$S/rs-gen"
cp vendor/libghostty-rs/crates/libghostty-vt-sys/build.rs "$S/rs-gen/crates/libghostty-vt-sys/build.rs"
cd "$S/rs-gen"
GHOSTTY_SOURCE_DIR=$R/vendor/ghostty \
  cargo run -p libghostty-vt-sys --features bindgen-tool --bin gen-bindings
cp crates/libghostty-vt-sys/src/bindings.rs $R/vendor/libghostty-rs/crates/libghostty-vt-sys/src/bindings.rs
```

（`GHOSTTY_SOURCE_DIR` 要绝对路径，所以用 `$R`。）

然后**仔细读绑定的 diff**，这是发现 C 接口变化最可靠的地方：

```sh
git -C vendor/libghostty-rs diff --stat
git -C vendor/libghostty-rs diff crates/libghostty-vt-sys/src/bindings.rs
```

- 只有新增的函数、常量、类型：是新功能，记进报告的"新功能"一节，说明 `libghostty-vt` 还没封装。
- 有删除、改名、常量值变了、结构体字段变了：是破坏性变化。在 `crates/libghostty-vt` 里改封装（fork 上一样东西一个提交），保证 `cargo test` 过，并在报告里写清楚 runode 要不要跟着改。

在 libghostty-rs 里验证（让它用 runode 的 ghostty 源码，不去网上拉）：

```sh
cd vendor/libghostty-rs
GHOSTTY_SOURCE_DIR=$R/vendor/ghostty cargo test
```

改了 crate 版本号时还要 `cargo update -w --manifest-path example/Cargo.toml`，见它的 `AGENTS.md`。

提交照先例，比如 `chore: update vt-sys and the nix ghostty input to <new 前七位>`，正文写：新 pin 只加了接口还是有破坏性变化、哪些常量值不变、flake.lock 怎么动的。

## 5. libghostty-rs：跟进 Uzaaft 上游

摸底里"Uzaaft 上游新增 N 个提交"不是 0 时：

1. 读这些提交，和 fork 上我们的提交逐个比对（`git cherry` 标 `-` 的直接删）。上游加了和我们一样的能力、只是名字或形状不同的，**删掉我们的提交、改 runode 去用上游的接口**，这是 `AGENTS.md` 定的规矩。
2. 上游新加的、我们没有的接口，记进报告的"新功能"。
3. 把保留的提交挪到 `upstream/master` 上：`git checkout -B main upstream/master`，再按顺序 cherry-pick 保留的提交。`GHOSTTY_COMMIT` 相关的 `chore` 提交和上游的 pin 冲突时，以我们这次要的 `<new>` 为准。
4. 同样跑 `cargo test`。

## 6. runode 这边

```sh
git add vendor/ghostty vendor/libghostty-rs   # 两个指针一起挪
make check
make test
make clippy CLIPPY_ARGS="-- -D warnings"
cargo deny check bans
```

- 绑定有破坏性变化时，看 `crates/terminal` 要不要跟着改；接口改名的，跟着改名，不在 runode 里写兼容层。
- 在 `crates/terminal`、`crates/host` 里搜绕着 libghostty 局限写的注释（`rg -n "libghostty" crates/terminal crates/host`，看说"没有""还不""只能"的），新上游给了正式做法的改用上游的。
- 快照格式版本可以这样看有没有变：比较新旧 `src/terminal/snapshot` 里的版本常量（`git -C vendor/ghostty diff <old> <new> -- src/terminal/snapshot`），变了就写进报告。
- 新上游改了终端查询的应答时，host 和 terminal 里断言应答内容的测试可能会挂，挂了先确认是上游有意改的行为，再改测试，并在报告的"行为变化"里写上。

runode 的提交照用户的规范，例子见 SKILL.md 的"提交信息"。只在用户要求时提交。

## 7. 推送

等用户确认之后：

子模块的 `origin` 是 https 地址，本机没有 GitHub 的 https 凭据，推送走 ssh 地址（runode 主仓库也是 ssh）。推之前用 `git ls-remote` 确认远端 `main` 还是你变基前的那个提交，`--force-with-lease` 写明期望值：

```sh
git -C vendor/ghostty ls-remote origin main
git -C vendor/ghostty push --force-with-lease=main:<变基前的 fork HEAD> git@github.com:runode-dev/ghostty.git main:main
git -C vendor/libghostty-rs push git@github.com:runode-dev/libghostty-rs.git main:main   # 变基到了 Uzaaft 上游时同样带 --force-with-lease
```

在 worktree 里做的，推完把主检出的两个子模块 `fetch` 后 `checkout -B main origin/main` 挪到新提交上，再把 runode 的改动搬到本地 main，只提交这几个路径（主检出常有别的未提交改动，不要一起带进去）。

再推 runode（如果用户要推）。推完可以删掉备份分支，或者留给用户决定。
