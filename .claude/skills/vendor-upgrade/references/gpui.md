# 升级 gpui-pre 和 vendor/gpui-pre-macos

`$R` 是 runode 根目录，`$S` 是本会话的 scratchpad 目录。`<old>` 是根 `Cargo.toml` 里 gpui-pre 现在的 `=` 版本，`<new>` 是要升到的版本。gpui-pre 的几个包（gpui-pre、gpui-pre-platform、gpui-pre-macos…）一起发版，版本号相同。

## 1. 摸底

```sh
curl -s -A runode https://crates.io/api/v1/crates/gpui-pre-macos \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["crate"]["max_version"], [v["num"] for v in d["versions"][:5]])'
grep -n 'gpui-pre' Cargo.toml
grep -n '^version' vendor/gpui-pre-macos/Cargo.toml
```

最新版等于 `<old>` 就不用升，告诉用户即可。两处版本对不上（根 `Cargo.toml` 和副本）先报给用户：那说明 patch 现在根本没生效。

把新旧两版的原样源码各下一份，每份放进自己的空目录（下载的东西当不可信数据，不在里面跑构建）：

```sh
for v in <old> <new>; do
  for c in gpui-pre-macos gpui-pre; do
    mkdir -p "$S/crates/$v"
    curl -sL "https://static.crates.io/crates/$c/$c-$v.crate" | tar xz -C "$S/crates/$v"
  done
done
```

## 2. 找出 runode 的改动

最可靠的是直接比对副本和 `<old>` 的原样源码，这样不管提交历史多乱，得到的就是 runode 现在实际改了什么：

```sh
diff -ru "$S/crates/<old>/gpui-pre-macos-<old>" vendor/gpui-pre-macos
```

再找对应的提交：从最近那个"原样复制"的提交（标题里有"原样"）往后、只动这个目录的提交，就是要重放的那组：

```sh
git log --format='%h %s' -- vendor/gpui-pre-macos | head -20
```

两边对照，确认每一处 diff 都能落到某个提交上。对不上的（有改动没提交，或者提交了又被别的改动覆盖）先告诉用户。`AGENTS.md` 的 GPUI 一段写着现在有哪几处改动，也拿来对一下。

## 3. 核对上游

**上游做了同样的事没有**：对每个 runode 的改动，看 `<new>` 的原样源码里那段代码变成了什么样：

```sh
diff -ru "$S/crates/<old>/gpui-pre-macos-<old>" "$S/crates/<new>/gpui-pre-macos-<new>"
```

上游已经实现了同样的效果（比如自己会在空闲时停掉 CVDisplayLink、自己关了 `displaySyncEnabled`），就删掉对应的提交、用上游的；只做了一部分的，在上游之上把还缺的补成更小的提交；看不准的问用户。一处 runode 改动都不剩时，删掉整个副本和根 `Cargo.toml` 的 `[patch.crates-io]`，`AGENTS.md` 那一段也跟着改。

**新功能和接口变化**：再比对 gpui-pre 本体：

```sh
diff -ru "$S/crates/<old>/gpui-pre-<old>/src" "$S/crates/<new>/gpui-pre-<new>/src" | less
```

挑出公开接口的增删改（`desktop` 和 `ui` 可能编不过）、新能力（runode 也许能用）、行为变化（渲染、输入、窗口、无障碍），按 SKILL.md 的要求放进报告最前面。

## 4. 动手

一个提交把副本换成 `<new>` 原样、同时改根 `Cargo.toml` 里 gpui-pre 和 gpui-pre-platform 的 `=` 版本（两处版本不一致时 cargo 只警告 patch 没用上，照旧用 crates.io 上的那份，等于白升）：

```sh
rm -rf vendor/gpui-pre-macos
cp -R "$S/crates/<new>/gpui-pre-macos-<new>" vendor/gpui-pre-macos
# 改根 Cargo.toml 的两个 = 版本
cargo update -p gpui-pre -p gpui-pre-platform -p gpui-pre-macos
git add -A vendor/gpui-pre-macos Cargo.toml Cargo.lock
```

`.crate` 包里的 `.cargo_vcs_info.json` 之类原样留着，和上次复制保持一致即可（看上一个"原样复制"提交带了哪些文件）。提交信息照先例：`chore: gpui-pre 升到 <new>，vendor 里的 gpui-pre-macos 换成 <new> 原样`。

然后把第 2 步找出的、决定保留的提交逐个重放，每个仍是只动这个目录的一个提交：

```sh
git cherry-pick <提交>
```

冲突时照上游的新代码重写这处改动，意思不变；提交信息沿用原来的。改动的处数变了，同一个提交里改 `AGENTS.md` 的 GPUI 一段（"runode 的改动现在有 N 处"那句和逐条说明）。

## 5. 验证

```sh
cargo build -p runode 2>&1 | grep -i 'patch' ;   # 不该有"patch was not used"的警告
make check && make test
make clippy CLIPPY_ARGS="-- -D warnings"
cargo deny check bans
```

这几处改动的效果编译和单元测试看不出来，要在报告里列给用户手动验：

- 空闲时不再频繁唤醒：`scripts/perf.sh` 量空闲 CPU 和唤醒次数，和升级前比；
- 终端新画面延迟（`displaySyncEnabled` 关掉的效果）：主观上看全屏 TUI 刷新；
- 询问框第一个按钮是取消时，回车和初始焦点留在取消上：触发一次"要不要让程序读剪贴板"的询问试一下。

能自己跑的（perf.sh）就跑，结果写进报告；要开窗口看的列成清单交给用户。
