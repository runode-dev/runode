# 参与开发

## 构建

需要 macOS、Xcode（含 Metal 工具链）和 [zig](https://ziglang.org) 0.16.0，Rust 版本由 `rust-toolchain.toml` 指定。新版 Xcode 缺 Metal 编译器时，先跑 `xcodebuild -downloadComponent MetalToolchain`。

```sh
git clone --recursive https://github.com/runode-dev/runode.git
cd runode
make run              # 调试构建并启动
make install          # 打包 Runode.app 并装到 /Applications
make run-ios          # 构建 iOS app，装到模拟器上启动
make run-ios-device   # 构建 iOS app，装到连着的 iPhone 上
```

已经克隆过的话，用 `make submodules` 拉子模块。`make help` 列出所有目标。

## 提交 PR 前

```sh
make fmt
make clippy
make test
cargo deny check bans   # 检查 crate 之间的依赖方向
```

CI 会跑同样的检查。代码分在哪个 crate、能依赖谁，以及命名、注释和测试的约定，见 [AGENTS.md](AGENTS.md)。
