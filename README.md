# Runode

给 AI 编程 agent 用的 macOS 终端工作台，基于 [libghostty-vt](https://github.com/ghostty-org/ghostty) 和 [GPUI](https://www.gpui.rs)。

- 分屏、标签、工作区和多窗口；认出每个终端里在跑哪个 agent（Claude Code、Codex 等），看它在干活、空闲还是等你回答。
- 终端会话由宿主进程持有，app 升级、重开时会话不断；可配置成退出 app 后宿主留在后台。
- `runode` 命令行能列出、读取、操作 app 里的每个终端，让 agent 驱动旁边的终端跑命令、等结果。
- iOS app 经局域网配对连上电脑，在手机上看终端、发输入、跑项目命令、读写 git。
- 配置文件兼容 Ghostty 的写法和主题。

## 构建

需要 macOS、Xcode（含 Metal 工具链）、[zig](https://ziglang.org) 0.16.0，Rust 版本由 `rust-toolchain.toml` 指定。

```sh
make submodules   # 拉 vendor 下的子模块
make run          # 调试构建并启动
make install      # 打包 Runode.app 并装到 /Applications
make help         # 列出全部目标
```

新版 Xcode 缺 Metal 编译器时先跑 `xcodebuild -downloadComponent MetalToolchain`。iOS app 用 `make run-ios`（模拟器）或 `make run-ios-device`（真机）。

## 使用

配置文件在 `~/.config/runode/config.conf`，也会读 Ghostty 的配置。

在 runode 的终端里，`runode`（短名 `rn`）可以操作其他终端：

```sh
runode list                                    # 列出所有终端
runode send right 'cargo test' --enter --wait  # 在右边的窗格跑命令并等它结束
runode read right --command                    # 读上一条命令的输出
runode setup claude                            # 把 runode 的 skill 装给 Claude Code（或 codex）
runode remote pair                             # 给手机配对（devices 列出、revoke 撤销）
```

完整说明见 `runode help` 和 [skills/runode/SKILL.md](skills/runode/SKILL.md)。

## 开发

```sh
make check        # 类型检查
make test         # 跑测试（装了 cargo-nextest 时并行）
make clippy
make fmt
cargo deny check bans   # 检查 crate 分层
```

代码分层、各 crate 的职责和约定见 [AGENTS.md](AGENTS.md)。

## 许可

[Apache-2.0](LICENSE)
