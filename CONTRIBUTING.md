# 参与开发

## 构建

需要 macOS、Xcode（含 Metal 工具链）和 [zig](https://ziglang.org) 0.17.0，Rust 版本由 `rust-toolchain.toml` 指定。新版 Xcode 缺 Metal 编译器时，先跑 `xcodebuild -downloadComponent MetalToolchain`。

```sh
git clone --recursive https://github.com/runode-dev/runode.git
cd runode
make run              # 调试构建并启动
make install          # 打包 Runode.app 并装到 /Applications
make run-ios          # 构建 iOS app，装到模拟器上启动
make run-ios-device   # 构建 iOS app，装到连着的 iPhone 上
```

已经克隆过的话，用 `make submodules` 拉子模块。`make help` 列出所有目标。

Windows 用 Visual Studio 的 C++ 构建工具、同一版 zig 和 Rust。装有 GNU Make 时同样运行 `make run`，
它会先构建命令行 `rn.exe`，再启动桌面程序 `runode.exe`；发布构建用 `make run-release`。
没有 Make 时，在 PowerShell 里运行：

```powershell
cargo build -p runode --bin rn
cargo run -p runode
```

桌面构建会下载固定版本、核对 SHA-256，把 Microsoft 的 `conpty.dll`、`OpenConsole.exe` 和许可证放到
可执行文件旁；打包时三份文件也要一起带上。Windows 10 自带的 ConPTY 不转发鼠标模式，缺了这份运行库，
Codex、pi 等程序收不到滚轮。单独测试终端 crate 前，先准备测试程序用的运行库：

```powershell
powershell -NoProfile -File scripts/windows/setup-conpty.ps1 -Destination target/debug -IncludeTests
cargo test -p runode-terminal
```

## Android 开发

Android app 是 `apps/android` 下的 Gradle 工程，macOS 和 Linux 上都能编。需要 JDK 21、Android SDK、同一版 zig、rustup 和 [cargo-ndk](https://github.com/bbqsrc/cargo-ndk)。在 SDK Manager 里装上 `apps/android/gradle/libs.versions.toml` 里 `ndk` 和 `cmake` 两项写的那一版 NDK 和 CMake，再设好 `ANDROID_HOME`；脚本默认用 SDK 里版本最新的 NDK，装了好几版时用 `ANDROID_NDK_HOME` 指定。cargo-ndk 没装的话，`make android-tunnel` 会用 `cargo install` 装上。

app 要的三份原生库 Gradle 不会自己去编，第一次构建前，以及 ghostty 子模块、拨号库的代码或 sherpa-onnx 的版本变了以后，先在仓库根目录准备好，缺了哪份 Gradle 会直接报错：

```sh
make android-vt       # 从 vendor/ghostty 编 libghostty-vt（要 zig 和 NDK）
make android-tunnel   # 编 iroh 拨号库 librunode_tunnel.so（要 rustup、cargo-ndk 和 NDK）
make android-speech   # 下载离线识别用的 sherpa-onnx，按 SHA-256 核对
make android-test     # 跑 JVM 上的单元测试
make run-android      # 调试构建，装到连着的设备或模拟器上启动
```

设备测试在 `apps/android` 里跑 `./gradlew connectedDebugAndroidTest`，要先开好模拟器或连上手机。识别模型的测试从设备上的 `/data/local/tmp/runode-speech-models` 读模型，没放就跳过。

推送用 Firebase。Firebase 项目的配置 `google-services.json` 放在 `apps/android/app/` 下，它不进 git；没有这个文件 app 照样能编能跑，只是不登记推送。

CI 的 Android 工作流在 Linux 上做同样的准备，跑单元测试、Android lint 和拨号库的 clippy，再在 API 31 和 36 的模拟器上跑设备测试。

## 提交 PR 前

```sh
make fmt
make clippy
make test
cargo deny check bans   # 检查 crate 之间的依赖方向
```

终端的测试会起真的 bash、zsh、fish、elvish 和 nushell 跑集成脚本，系统里没有的 shell 不测（macOS 自带的 bash 还是 3.2，有几项要新 bash 的也不测）。CI 会装齐这几个 shell，并设 `RUNODE_REQUIRE_SHELLS=1`，缺了哪个就直接失败。

CI 会跑同样的检查。代码分在哪个 crate、能依赖谁，以及命名、注释和测试的约定，见 [AGENTS.md](AGENTS.md)。

## 发版

把 `vX.Y.Z` 标签推到 GitHub 就会触发 Release 工作流（`.github/workflows/release.yml`），后面的构建、签名、公证、发 Release 和装一遍核对全自动。一般用 `scripts/release.sh`，它会生成中英双语的更新说明（本机有 claude 时让它照上一版的说明归纳），改版本号、提交、打标签、推送一步做完；更新说明已经写好时，`--yes` 不开编辑器、不问确认，agent 也能一条命令发版。CI 和各平台的打包同时跑，两种 Mac 都在 Apple 芯片上编（x86_64 交叉编译），从推标签到公开大约 20 分钟。

上一个标签以来 `apps/ios` 或手机端用到的 crate（`protocol`、`types`、`remote`）有改动时，`scripts/release.sh` 推完标签会顺手触发 TestFlight 工作流（`.github/workflows/testflight.yml`，它本身只能手动触发，不随标签跑）；手动发版时自己在 Actions 页面或用 `gh workflow run testflight.yml` 跑一次。

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
