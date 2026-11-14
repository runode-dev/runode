#!/usr/bin/env bash
# 编 Android app 经 JNI 加载的 iroh 拨号库（runode-android-tunnel），放到 connection 模块的
# src/main/jniLibs/<ABI>/librunode_tunnel.so，Gradle 原样打进 APK。
#
# 用仓库 rust-toolchain.toml 定的那个 rustup 工具链编：先给它装上两个 Android 目标，再把它的 bin 目录放到 PATH
# 最前面，PATH 上另有 Homebrew 之类装的 cargo 时也不会用错。cargo-ndk 按 ABI 找 NDK 里的 clang 当链接器和 C 编译器
# （ring 有 C 和汇编），-P 31 是 app 的 minSdk。NDK 28 起链接器默认按 16 KB 页对齐，不用另加参数。
#
# 用法：apps/android/scripts/build-tunnel.sh [profile]
# 默认用根 Cargo.toml 的 android profile（按体积优化）；调试时可以给 dev。NDK 默认取 $ANDROID_HOME/ndk/ 下版本
# 最新的那个，用 ANDROID_NDK_HOME 指定别的。
set -euo pipefail

profile="${1:-android}"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
android_dir="$(cd "$script_dir/.." && pwd)"
repo_dir="$(cd "$android_dir/../.." && pwd)"
out_dir="$android_dir/connection/src/main/jniLibs"
min_sdk=31

if ! command -v rustup >/dev/null; then
  echo "找不到 rustup：要用它装 rust-toolchain.toml 定的工具链和 Android 目标" >&2
  exit 1
fi
cd "$repo_dir"
rustup target add aarch64-linux-android x86_64-linux-android
toolchain_bin="$(dirname "$(rustup which cargo)")"
export PATH="$toolchain_bin:${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"

if ! command -v cargo-ndk >/dev/null; then
  cargo install cargo-ndk --locked
fi

if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
  sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
  if [[ -z "$sdk" ]]; then
    echo "找不到 Android SDK，设 ANDROID_HOME，或者用 ANDROID_NDK_HOME 直接指定 NDK" >&2
    exit 1
  fi
  if [[ -d "$sdk/ndk" ]]; then
    ANDROID_NDK_HOME="$(find "$sdk/ndk" -mindepth 1 -maxdepth 1 -type d | sort -V | tail -n 1)"
  fi
fi
if [[ -z "${ANDROID_NDK_HOME:-}" || ! -d "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt" ]]; then
  echo "找不到 Android NDK${ANDROID_NDK_HOME:+（$ANDROID_NDK_HOME）}，在 SDK Manager 里装上 NDK，或者用 ANDROID_NDK_HOME 指定" >&2
  exit 1
fi
export ANDROID_NDK_HOME

cargo ndk -t arm64-v8a -t x86_64 -P "$min_sdk" build --profile "$profile" -p runode-android-tunnel

# 不用 cargo-ndk 的 -o：iroh 和 iroh-relay 自己也声明了 cdylib，-o 会把它们编出的 .so 一起拷进 jniLibs、打进 APK。
target_dir="${CARGO_TARGET_DIR:-$repo_dir/target}"
profile_dir="$profile"
if [[ "$profile" == dev ]]; then
  profile_dir=debug
fi
for pair in arm64-v8a:aarch64-linux-android x86_64:x86_64-linux-android; do
  abi="${pair%%:*}"
  library="$target_dir/${pair#*:}/$profile_dir/librunode_tunnel.so"
  if [[ ! -f "$library" ]]; then
    echo "cargo-ndk 没有编出 $library" >&2
    exit 1
  fi
  rm -rf "${out_dir:?}/$abi"
  mkdir -p "$out_dir/$abi"
  cp "$library" "$out_dir/$abi/"
done

echo "已生成 $out_dir/{arm64-v8a,x86_64}/librunode_tunnel.so（${profile}，NDK $(basename "$ANDROID_NDK_HOME")）"
