#!/bin/bash
# 在 Mac 上交叉编译装到 SSH 远端的 runode：另一种架构的 Mac 用的可执行文件、Windows 用的 zip，有 docker 时
# 还有 x86_64 和 aarch64 的 Linux 可执行文件。本机的开发构建在发布页上没有对应的包，连这几种远端时就装它们：
# `make run REMOTE=1` 经 RUNODE_SSH_BINARY_<平台> 交给桌面，`make install` 打进 Runode.app 的
# Contents/Resources/remote。
#
# 用法：build-remote.sh <输出目录>
# 产物是输出目录下的 runode-macos-<架构>、runode-windows-x86_64.zip 和 runode-linux-<架构>；标准输出每行
# 一个 RUNODE_SSH_BINARY_<平台>=<产物>，可以直接交给 env。
#
# 环境变量：
#   CARGO_PROFILE    Mac 和 Linux 版用的 cargo profile，默认 dev。Windows 版一律用 cross-windows。
#   RUNODE_BUILD_TAG 照常交给构建脚本，远端的构建号才和本机的对上。
#
# 要的工具：rustup（照 rust-toolchain.toml 取工具链，缺的 target 自动装）、cargo-xwin、LLVM 的 clang-cl 和
# llvm-lib、lld 的 lld-link、zig。Homebrew 上是 brew install rustup llvm lld zig 和
# cargo install --locked cargo-xwin。缺了时以退出码 2 退出，调用方可以跳过。Linux 版要 docker（OrbStack 或
# Docker Desktop），连不上 docker 时只跳过 Linux。
set -euo pipefail

out=${1:?用法：build-remote.sh <输出目录>}
CARGO_PROFILE=${CARGO_PROFILE:-dev}
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
mkdir -p "$out"
out=$(cd "$out" && pwd)
# 和本机构建分开：工具链不一定是同一个，共用目录时两边的构建脚本会来回重编。
target_dir="$root/target/remote"

missing() {
    echo "build-remote: 缺 $1，跳过交叉编译远端用的 runode（$2）" >&2
    exit 2
}

[[ "$(uname -s)" == Darwin ]] || missing "macOS" "只在 Mac 上交叉编译"
command -v rustup >/dev/null || missing rustup "brew install rustup"
# Homebrew 的 rust 不带别的平台的标准库，用 rustup 的工具链。
cargo=$(rustup which cargo)
rustc=$(rustup which rustc)
toolchain_bin=$(dirname "$cargo")
export PATH="$toolchain_bin:$HOME/.cargo/bin:$PATH"
for formula in llvm lld; do
    if command -v brew >/dev/null && prefix=$(brew --prefix "$formula" 2>/dev/null) && [[ -d "$prefix/bin" ]]; then
        export PATH="$PATH:$prefix/bin"
    fi
done
command -v cargo-xwin >/dev/null || missing cargo-xwin "cargo install --locked cargo-xwin"
command -v clang-cl >/dev/null || missing clang-cl "brew install llvm"
command -v llvm-lib >/dev/null || missing llvm-lib "brew install llvm"
command -v lld-link >/dev/null || missing lld-link "brew install lld"
command -v zig >/dev/null || missing zig "brew install zig"

case "$(uname -m)" in
    arm64) mac_rust=x86_64-apple-darwin mac_arch=x86_64 ;;
    *) mac_rust=aarch64-apple-darwin mac_arch=arm64 ;;
esac
win_rust=x86_64-pc-windows-msvc
installed=$(rustup target list --installed)
for triple in "$mac_rust" "$win_rust"; do
    grep -qx "$triple" <<<"$installed" || rustup target add "$triple"
done

# 另一种架构的 Mac。
RUSTC="$rustc" CARGO_TARGET_DIR="$target_dir" \
    "$cargo" build -p runode --bin runode --profile "$CARGO_PROFILE" --target "$mac_rust"
case "$CARGO_PROFILE" in dev) mac_dir=debug ;; *) mac_dir=$CARGO_PROFILE ;; esac
mac_out="$out/runode-macos-$mac_arch"
cp "$target_dir/$mac_rust/$mac_dir/runode" "$mac_out.part"
# 和 Runode.app 里的一样写固定的 UUID，远端换了构建、新宿主接手后终端照样连得上局域网。
python3 -I scripts/set-macho-uuid.py "$mac_out.part" "$mac_arch"
# 用 Developer ID 签名时，远端给屏幕录制、辅助功能的授权换了新构建仍然有效；没有这种证书时 ad-hoc 签名。
identity=$(security find-identity -v -p codesigning | awk '/"Developer ID Application: / { print $2; exit }')
if [[ -n "$identity" ]]; then
    codesign --force --sign "$identity" --identifier dev.runode.remote-dev --options runtime --timestamp "$mac_out.part"
else
    codesign --force --sign - --identifier dev.runode.remote-dev "$mac_out.part"
fi
mv "$mac_out.part" "$mac_out"

# Windows：cargo-xwin 下载 MSVC 的 CRT 和 Windows SDK，C/C++ 用 clang-cl、链接用 lld-link 编。
xwin_root=$(cargo xwin env --target "$win_rust" | sed -n 's|^export LIB="\([^;]*\)/crt/lib/x86_64;.*|\1|p')
[[ -d "$xwin_root/sdk" ]] || { echo "build-remote: 找不到 cargo-xwin 下载的 Windows SDK" >&2; exit 1; }
# libghostty-vt 由 zig 编，zig 不在 Windows 上时找不到 MSVC 的 libc，要经 ZIG_LIBC 给它一份路径。这个文件也
# 用在 zig 编给本机跑的构建工具上，所以同时写上 macOS SDK，库目录里再放上 macOS 的库，两种目标都认得。
libc_dir="$target_dir/zig-libc"
rm -rf "$libc_dir"
mkdir -p "$libc_dir/crt"
mac_sdk=$(xcrun --show-sdk-path)
ln -s "$xwin_root"/sdk/lib/ucrt/x86_64/* "$mac_sdk"/usr/lib/*.tbd "$libc_dir/crt/"
cat >"$libc_dir/libc.txt" <<EOF
include_dir=$xwin_root/sdk/include/ucrt
sys_include_dir=$xwin_root/crt/include
crt_dir=$libc_dir/crt
msvc_lib_dir=$xwin_root/crt/lib/x86_64
kernel32_lib_dir=$xwin_root/sdk/lib/um/x86_64
gcc_dir=
darwin_sdk_dir=$mac_sdk
EOF
# sherpa-onnx 的预编译库用静态 CRT；lld-link 不像 link.exe 那样拿静态 CRT 补动态导入的符号，整个程序都用静态 CRT。
# 静态 CRT 的调试信息指着微软构建机上的 pdb，lld-link 每次都为找不到它报 LNK4099，忽略掉。
ZIG_LIBC="$libc_dir/libc.txt" CARGO_TARGET_DIR="$target_dir" \
    CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-ignore:4099" \
    cargo xwin build -p runode --profile cross-windows --target "$win_rust"

# 终端要的新版 ConPTY，版本和校验和跟 Windows 上准备它的脚本一致。
conpty_script=scripts/windows/setup-conpty.ps1
conpty_version=$(sed -n "s/^\$version = '\(.*\)'/\1/p" "$conpty_script")
conpty_sha=$(sed -n "s/^\$sha256 = '\(.*\)'/\1/p" "$conpty_script" | tr 'A-F' 'a-f')
conpty_cache="$root/target/conpty/$conpty_version"
conpty_archive="$conpty_cache/conpty.zip"
mkdir -p "$conpty_cache"
if [[ ! -f "$conpty_archive" ]] || [[ "$(shasum -a 256 "$conpty_archive" | cut -d' ' -f1)" != "$conpty_sha" ]]; then
    curl --fail --location --silent --show-error --retry 3 --connect-timeout 15 --max-time 120 \
        "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$conpty_version/microsoft.windows.console.conpty.$conpty_version.nupkg" \
        --output "$conpty_archive.download"
    [[ "$(shasum -a 256 "$conpty_archive.download" | cut -d' ' -f1)" == "$conpty_sha" ]] ||
        { echo "build-remote: ConPTY 包的 SHA-256 对不上" >&2; exit 1; }
    mv "$conpty_archive.download" "$conpty_archive"
fi

# 和发布的便携包一样的几个文件，远端用系统的 tar.exe 解开。
stage="$target_dir/windows-zip"
rm -rf "$stage"
mkdir -p "$stage"
win_dir="$target_dir/$win_rust/cross-windows"
cp "$win_dir/runode.exe" "$win_dir/rn.exe" scripts/windows/conpty-LICENSE.txt "$stage/"
unzip -q -j "$conpty_archive" runtimes/win-x64/native/conpty.dll build/native/runtimes/x64/OpenConsole.exe -d "$stage"
win_out="$out/runode-windows-x86_64.zip"
rm -f "$win_out.part"
(cd "$stage" && zip -q -X "$win_out.part" runode.exe rn.exe conpty.dll OpenConsole.exe conpty-LICENSE.txt)
mv "$win_out.part" "$win_out"
mac_env=$(tr '[:lower:]' '[:upper:]' <<<"$mac_arch")
echo "RUNODE_SSH_BINARY_MACOS_$mac_env=$mac_out"
echo "RUNODE_SSH_BINARY_WINDOWS_X86_64=$win_out"

# Linux：在 docker 里编，原生架构直接编，另一种交叉编。工具链装在镜像里，cargo 的下载、zig 的缓存和构建目录
# 放在 docker 卷里，下次接着用。拷出来时去掉调试信息（dev 构建带着它有一个多 G），留着符号表，panic 回溯照样有函数名。
if ! docker info >/dev/null 2>&1; then
    echo "build-remote: 连不上 docker，跳过 Linux 版（装 OrbStack 或 Docker Desktop 并启动）" >&2
    exit 0
fi
case "$(docker info --format '{{.Architecture}}')" in
    aarch64 | arm64) native=aarch64 cross=x86_64 cross_deb=amd64 zig_arch=aarch64 ;;
    *) native=x86_64 cross=aarch64 cross_deb=arm64 zig_arch=x86_64 ;;
esac
rust_version=$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
zig_version=$(sed -n 's/.*minimum_zig_version = "\(.*\)".*/\1/p' vendor/ghostty/build.zig.zon)
dockerfile=scripts/remote-linux.Dockerfile
image="runode-remote-linux:$rust_version-$zig_version-$(shasum "$dockerfile" | cut -c1-8)"
if ! docker image inspect "$image" >/dev/null 2>&1; then
    docker build -t "$image" -f "$dockerfile" --build-arg RUST_VERSION="$rust_version" \
        --build-arg ZIG_VERSION="$zig_version" --build-arg ZIG_ARCH="$zig_arch" --build-arg CROSS_ARCH="$cross_deb" \
        scripts >&2
fi
case "$CARGO_PROFILE" in dev) linux_dir=debug ;; *) linux_dir=$CARGO_PROFILE ;; esac
cross_env=$(tr '[:lower:]-' '[:upper:]_' <<<"$cross-unknown-linux-gnu")
cross_env_lower=$(tr - _ <<<"$cross-unknown-linux-gnu")
docker run --rm -v "$root:$root" -w "$root" -v "$out:/out" \
    -v runode-remote-linux-target:/target -v runode-remote-linux-cargo:/opt/cargo/registry \
    -v runode-remote-linux-zig:/zig-cache \
    -e CARGO_TARGET_DIR=/target -e ZIG_GLOBAL_CACHE_DIR=/zig-cache -e RUNODE_BUILD_TAG \
    -e "CARGO_TARGET_${cross_env}_LINKER=$cross-linux-gnu-gcc" -e "CC_$cross_env_lower=$cross-linux-gnu-gcc" \
    -e "CXX_$cross_env_lower=$cross-linux-gnu-g++" -e "AR_$cross_env_lower=$cross-linux-gnu-ar" \
    "$image" bash -ec "
        cargo build -p runode --bin runode --locked --profile $CARGO_PROFILE
        cargo build -p runode --bin runode --locked --profile $CARGO_PROFILE --target $cross-unknown-linux-gnu
        strip -g -o /out/runode-linux-$native.part /target/$linux_dir/runode
        $cross-linux-gnu-strip -g -o /out/runode-linux-$cross.part /target/$cross-unknown-linux-gnu/$linux_dir/runode
    " >&2
for arch in "$native" "$cross"; do
    mv "$out/runode-linux-$arch.part" "$out/runode-linux-$arch"
    echo "RUNODE_SSH_BINARY_LINUX_$(tr '[:lower:]' '[:upper:]' <<<"$arch")=$out/runode-linux-$arch"
done
