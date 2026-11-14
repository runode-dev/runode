#!/usr/bin/env bash
# 从仓库里的 vendor/ghostty 子模块编出 Android 用的 libghostty-vt，放到 apps/android/build/ghostty-vt/<三元组>/：
# lib/libghostty-vt.a 和 include/ghostty/。terminal 模块的 CMake 按 ABI 取对应三元组的那份。
#
# 每个 ABI 平铺地编一次 -Dtarget=<三元组>.31（.31 是 API 级别，等于 app 的 minSdk，链的是 NDK 里 API 31 的
# libc 桩，用不到更新的 bionic 符号）。ghostty 自己按 ANDROID_NDK_HOME 找 NDK 的 sysroot 和 libc++ 头文件，
# 并把共享库按 16 KB 页对齐。
#
# 用的是静态库，链进 JNI 壳，最后只出一个 librunode_vt.so：
# - 静态库只引用 libc 和 libm。simdutf、highway 虽是 C++ 写的，却不需要 libc++ 的运行时，JNI 壳不用带
#   libc++_shared.so。
# - 共享库的 SONAME 带版本号（libghostty-vt.so.1），APK 只打包、加载 lib*.so，得拷成不带版本号的名字再改
#   SONAME，macOS 上要另装 patchelf；链静态库没有这一步，链接器还能丢掉 JNI 壳用不到的部分。
#
# 用法：apps/android/scripts/build-ghostty-vt.sh [Debug|ReleaseSafe|ReleaseFast|ReleaseSmall]
# 默认 ReleaseFast。zig 找不到时用 ZIG 环境变量指定；NDK 默认取 $ANDROID_HOME/ndk/ 下版本最新的那个，
# 用 ANDROID_NDK_HOME 指定别的。
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
android_dir="$(cd "$script_dir/.." && pwd)"
repo_dir="$(cd "$android_dir/../.." && pwd)"
ghostty_dir="$repo_dir/vendor/ghostty"
build_dir="$android_dir/build/ghostty-vt"
min_sdk=31

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

ndk="${ANDROID_NDK_HOME:-}"
if [[ -z "$ndk" ]]; then
  sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
  if [[ -z "$sdk" ]]; then
    echo "找不到 Android SDK，设 ANDROID_HOME，或者用 ANDROID_NDK_HOME 直接指定 NDK" >&2
    exit 1
  fi
  if [[ -d "$sdk/ndk" ]]; then
    ndk="$(find "$sdk/ndk" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
  fi
fi
if [[ -z "$ndk" || ! -d "$ndk/toolchains/llvm/prebuilt" ]]; then
  echo "找不到 Android NDK${ndk:+（$ndk）}，在 SDK Manager 里装上 NDK，或者用 ANDROID_NDK_HOME 指定" >&2
  exit 1
fi

mkdir -p "$build_dir"
# 缓存、依赖包和安装目录都放在 apps/android/build 下，不往子模块里写东西：ZIG_LOCAL_PKG_DIR 不设时，
# zig 把下载的依赖包解到 vendor/ghostty/zig-pkg。
for triple in aarch64-linux-android x86_64-linux-android; do
  rm -rf "${build_dir:?}/$triple"
  (
    cd "$ghostty_dir"
    ANDROID_NDK_HOME="$ndk" ZIG_LOCAL_PKG_DIR="$build_dir/zig-pkg" "$zig" build \
      -Demit-lib-vt=true \
      -Dapp-runtime=none \
      "-Dtarget=$triple.$min_sdk" \
      "-Doptimize=$optimize" \
      --prefix "$build_dir/$triple" \
      --cache-dir "$build_dir/zig-cache"
  )
  if [[ ! -f "$build_dir/$triple/lib/libghostty-vt.a" ]]; then
    echo "zig 没有编出 $build_dir/$triple/lib/libghostty-vt.a" >&2
    exit 1
  fi
done

echo "已生成 $build_dir/{aarch64,x86_64}-linux-android（ghostty $(git -C "$ghostty_dir" rev-parse --short HEAD)，${optimize}，NDK $(basename "$ndk")）"
