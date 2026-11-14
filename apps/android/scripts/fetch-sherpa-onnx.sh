#!/usr/bin/env bash
# 取 Android app 离线识别用的 sherpa-onnx：k2-fsa/sherpa-onnx 在 GitHub Release 上发的官方 AAR，按写死的 SHA-256 核对，
# 取出 Kotlin API 的 classes.jar 和 arm64-v8a、x86_64 两个 ABI 的 JNI 库，放到 apps/android/build/sherpa-onnx/。
# speech 模块把 classes.jar 当本地 jar 依赖、把 jni/ 当 jniLibs 目录。不直接依赖 AAR 文件：库模块打 AAR 时
# 不收本地的 .aar 依赖。
#
# 版本和 iOS 用的 sherpa-onnx xcframework 一致，升级时两边一起换。用 static-link-onnxruntime 那份：onnxruntime
# 静态链进 libsherpa-onnx-jni.so，每个 ABI 只有这一个 .so，比另带 libonnxruntime.so 的那份小。
#
# 用法：apps/android/scripts/fetch-sherpa-onnx.sh
set -euo pipefail

version="2.0.0"
aar="sherpa-onnx-static-link-onnxruntime-$version.aar"
sha256="59366e37cf2760b419e895d2872aa1664027fd721f6e4c827cdb0e805088fcbe"
url="https://github.com/k2-fsa/sherpa-onnx/releases/download/v$version/$aar"
abis=(arm64-v8a x86_64)

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
android_dir="$(cd "$script_dir/.." && pwd)"
out_dir="$android_dir/build/sherpa-onnx"
download_dir="$android_dir/build/downloads"

digest() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

mkdir -p "$download_dir"
archive="$download_dir/$aar"
if [[ ! -f "$archive" || "$(digest "$archive")" != "$sha256" ]]; then
  rm -f "$archive" "$archive.part"
  curl -fL --retry 3 -o "$archive.part" "$url"
  actual="$(digest "$archive.part")"
  if [[ "$actual" != "$sha256" ]]; then
    rm -f "$archive.part"
    echo "$aar 的 SHA-256 是 $actual，应为 $sha256" >&2
    exit 1
  fi
  mv "$archive.part" "$archive"
fi

staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
members=(classes.jar)
for abi in "${abis[@]}"; do
  members+=("jni/$abi/libsherpa-onnx-jni.so")
done
unzip -q "$archive" "${members[@]}" -d "$staging"

rm -rf "${out_dir:?}"
mkdir -p "$out_dir"
mv "$staging/classes.jar" "$out_dir/classes.jar"
mv "$staging/jni" "$out_dir/jni"
printf '%s\n' "$version" >"$out_dir/VERSION"

echo "已取出 sherpa-onnx ${version} 到 ${out_dir}（classes.jar，jni/{$(IFS=,; echo "${abis[*]}")}/libsherpa-onnx-jni.so）"
