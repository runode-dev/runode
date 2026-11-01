#!/bin/bash
# 在 Mac 上交叉编译装到 SSH 远端的 runode：另一种架构的 Mac 用的可执行文件和 Windows 用的 zip。本机的开发
# 构建在发布页上没有对应的包，连这两种远端时就装它们：`make run REMOTE=1` 经 RUNODE_SSH_BINARY_<平台> 交给
# 桌面，`make install` 打进 Runode.app 的 Contents/Resources/remote。
#
# 用法：build-remote.sh <输出目录>
# 产物是输出目录下的 runode-macos-<架构> 和 runode-windows-x86_64.zip。
#
# 环境变量：
#   CARGO_PROFILE    Mac 版用的 cargo profile，默认 dev。Windows 版一律用 cross-windows。
#   RUNODE_BUILD_TAG 照常交给构建脚本，远端的构建号才和本机的对上。
#
# 要的工具：rustup（照 rust-toolchain.toml 取工具链，缺的 target 自动装）、cargo-xwin、LLVM 的 clang-cl 和
# llvm-lib、lld 的 lld-link、zig。Homebrew 上是 brew install rustup llvm lld zig 和
# cargo install --locked cargo-xwin。缺了时以退出码 2 退出，调用方可以跳过。
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
ZIG_LIBC="$libc_dir/libc.txt" CARGO_TARGET_DIR="$target_dir" \
    CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static" \
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
echo "$mac_out"
echo "$win_out"
