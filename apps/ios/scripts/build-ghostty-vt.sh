#!/usr/bin/env bash
# 从仓库里的 vendor/ghostty 子模块编出 libghostty-vt 的 xcframework，放到 apps/ios/Frameworks/。
#
# 走 ghostty 自己的 -Demit-xcframework=true：它在 macOS 上按各个 Apple 平台分别配置目标、
# 编出静态库再用 xcodebuild 打包，iOS 切片（ios-arm64、ios-arm64-simulator）只在检测到 iOS SDK
# 时才编。不能用平铺的 -Dtarget=<ios> 加 --sysroot：simdutf 的 NEON 内联函数在通用基线上编不过，
# --sysroot 也会漏进 ghostty 构建中途要跑的本机工具。桌面那边 libghostty-vt-sys 的构建脚本走的
# 是同一条路。
#
# 用法：apps/ios/scripts/build-ghostty-vt.sh [Debug|ReleaseSafe|ReleaseFast|ReleaseSmall]
# 默认 ReleaseFast。zig 找不到时用 ZIG 环境变量指定。
set -euo pipefail

optimize="${1:-ReleaseFast}"
case "$optimize" in
  Debug | ReleaseSafe | ReleaseFast | ReleaseSmall) ;;
  *)
    echo "optimize 只能是 Debug、ReleaseSafe、ReleaseFast、ReleaseSmall：$optimize" >&2
    exit 2
    ;;
esac

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ios_dir="$(cd "$script_dir/.." && pwd)"
repo_dir="$(cd "$ios_dir/../.." && pwd)"
ghostty_dir="$repo_dir/vendor/ghostty"
build_dir="$ios_dir/build/ghostty-vt"
out_dir="$ios_dir/Frameworks"

zig="${ZIG:-$(command -v zig || true)}"
if [[ -z "$zig" && -x /opt/homebrew/bin/zig ]]; then
  zig=/opt/homebrew/bin/zig
fi
if [[ -z "$zig" ]]; then
  echo "找不到 zig，装上它或者用 ZIG=/path/to/zig 指定" >&2
  exit 1
fi
if [[ ! -f "$ghostty_dir/build.zig" ]]; then
  echo "vendor/ghostty 是空的，先 git submodule update --init vendor/ghostty" >&2
  exit 1
fi

mkdir -p "$build_dir"
# 缓存和安装目录都放在 apps/ios/build 下，不往子模块里写东西。
(
  cd "$ghostty_dir"
  "$zig" build \
    -Demit-lib-vt=true \
    -Demit-xcframework=true \
    -Dapp-runtime=none \
    "-Doptimize=$optimize" \
    -Dcpu=baseline \
    --prefix "$build_dir/install" \
    --cache-dir "$build_dir/zig-cache"
)

xcframework="$build_dir/install/lib/ghostty-vt.xcframework"
for slice in ios-arm64 ios-arm64-simulator; do
  if [[ ! -d "$xcframework/$slice" ]]; then
    echo "xcframework 里没有 ${slice}：ghostty 只在检测到 iOS SDK 时编 iOS 切片，到 Xcode 的设置里装上 iOS 平台" >&2
    exit 1
  fi
done

mkdir -p "$out_dir"
rm -rf "$out_dir/ghostty-vt.xcframework"
cp -R "$xcframework" "$out_dir/ghostty-vt.xcframework"
echo "已生成 $out_dir/ghostty-vt.xcframework（ghostty $(git -C "$ghostty_dir" rev-parse --short HEAD)，${optimize}）"
