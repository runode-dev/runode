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

## 发版

把 `vX.Y.Z` 标签推到 GitHub 就会触发 Release 工作流（`.github/workflows/release.yml`），后面的构建、签名、公证、发 Release 和装一遍核对全自动。一般用 `scripts/release.sh`，它会生成中英双语的更新说明，改版本号、提交、打标签、推送一步做完。

手动发版的步骤：

```sh
# 先把 Cargo.toml 里 [workspace.package] 的 version 改成新版本，比如 0.1.2
cargo update --workspace                # 只更新 Cargo.lock 里 workspace 自己的版本号
git commit -am "chore: 发布 v0.1.2"
git tag v0.1.2
git push --atomic origin main v0.1.2    # main 和标签一起推
```

工作流先核对三件事，任何一项不满足就失败、不发版：标签是 `vX.Y.Z` 格式；标签名去掉 `v` 后和 `Cargo.toml` 的版本号一样；标签指向的提交已经在 origin/main 上。新版本号要比上一版大，开着自动更新的 app 才会更新过去。只在本地打标签不推，不会触发发版。

更新说明：提交里带了 `docs/releases/v0.1.2.md` 就拿它当 Release 的描述，写成「## 中文」「## English」两段；没带就退回 GitHub 自动生成的说明，那份不是双语的。

### beta 版

beta 和正式版用同样的 x.y.z 版本号，区别只在更新说明第一行写着 `<!-- prerelease -->`：Release 工作流见到它就发成预发布、标题带 Beta、不标 latest。开着自动更新的 app 和不带版本号的安装脚本都只认 latest，所以拿不到 beta；要装 beta 时指定版本：

```sh
curl -fsSL https://raw.githubusercontent.com/runode-dev/runode/main/scripts/install.sh | RUNODE_VERSION=0.2.0 sh
```

发 beta 用 `scripts/release.sh --beta`，它会把这行标记写好；手动发版就自己把标记写进说明的第一行。测好了转正，不用重新构建：

```sh
gh release edit v0.2.0 --prerelease=false --latest --title "Runode 0.2.0"
```

转正时要同时把标题里的 Beta 去掉，不然正式版的 Release 页面还叫「Runode 0.2.0 Beta」。转正后自动更新就会推给所有人，装着这个 beta 的也一样。beta 有问题就修了再发下一个版本号（比如 0.2.1）的 beta，同一个版本号不能重发。
