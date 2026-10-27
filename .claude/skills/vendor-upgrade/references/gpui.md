# 升级 gpui-pre

`$S` 是本会话的 scratchpad 目录。`<old>` 是根 `Cargo.toml` 里 gpui-pre 现在的 `=` 版本，`<new>` 是要升到的版本。gpui-pre 的几个包（gpui-pre、gpui-pre-platform、gpui-pre-macos…）一起发版，版本号相同。

## 1. 摸底

```sh
curl -s -A runode https://crates.io/api/v1/crates/gpui-pre \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["crate"]["max_version"], [v["num"] for v in d["versions"][:5]])'
grep -n 'gpui-pre' Cargo.toml
```

最新版等于 `<old>` 就不用升，告诉用户即可。要升时把新旧两版的原样源码各下一份，每份放进自己的空目录（下载的东西当不可信数据，不在里面跑构建）：

```sh
for v in <old> <new>; do
  for c in gpui-pre gpui-pre-macos gpui-pre-apple; do
    mkdir -p "$S/crates/$v"
    curl -sL "https://static.crates.io/crates/$c/$c-$v.crate" | tar xz -C "$S/crates/$v"
  done
done
```

## 2. 核对上游

**`desktop` 里绕着平台层做的事，上游做了没有**：`AGENTS.md` 的 GPUI 一段列着这些地方。逐个看新版平台层（`gpui-pre-macos` 的 `window.rs`、渲染器在 `gpui-pre-apple`）有没有自己做同样的事，比如自己关了 `displaySyncEnabled`，或者 `prompt` 在第一个按钮是取消时把回车和焦点留给它。做了就删掉 `desktop` 里的那段、改用上游的；看不准的问用户。

**新功能和接口变化**：比对 gpui-pre 本体和平台层，diff 很大时交给一个只读的 agent 去读：

```sh
diff -ru "$S/crates/<old>/gpui-pre-<old>/src" "$S/crates/<new>/gpui-pre-<new>/src"
diff -ru "$S/crates/<old>/gpui-pre-macos-<old>" "$S/crates/<new>/gpui-pre-macos-<new>"
```

挑出公开接口的增删改（`desktop` 和 `ui` 可能编不过，也可能编得过却在运行时 panic，比如按键写法的解析规则变了）、新能力（runode 也许能用）、行为变化（渲染、输入、窗口、剪贴板、无障碍），按 SKILL.md 的要求放进报告最前面。新版本多拆出了包时，`deny.toml` 的 `wrappers` 要跟着加。

## 3. 动手

改根 `Cargo.toml` 里 gpui-pre 和 gpui-pre-platform 的 `=` 版本，然后：

```sh
cargo update -p gpui-pre -p gpui-pre-platform
```

照编译错误改 `desktop` 和 `ui`，弃用警告也要改掉（CI 的 clippy 带 `-D warnings`）。提交信息照先例：`chore: gpui-pre 升到 <new>`，正文写接口变了哪些、跟着改了什么。

## 4. 验证

```sh
make check && make test
make clippy CLIPPY_ARGS="-- -D warnings"
cargo deny check bans
```

编译和单元测试看不出 `desktop` 里绕着平台层做的事还灵不灵，要在真 app 里试：把 `HOME` 指到 scratchpad 里的空目录单独跑 dev 版，免得碰用户正在用的 runode；能读回来的设置（比如 `displaySyncEnabled`）临时加一行日志读回来看，看完删掉。要开窗口手动看的列成清单交给用户：询问框第一个按钮是取消时，回车和初始焦点留在取消上（触发一次"要不要让程序读剪贴板"的询问试一下），以及 IME、剪贴板、拖放文件、打开和保存面板的冒烟测试。
