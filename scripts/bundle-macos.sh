#!/bin/bash
# 发布构建 runode，打包成 Runode.app，再做成可拖进「应用程序」安装的 dmg。
# 产物在 target/release/bundle/ 下。
#
# 环境变量：
#   CARGO          cargo 命令，默认 cargo
#   SIGN_IDENTITY  codesign 签名身份，默认 -（ad-hoc，只适合本机或自己用）
set -euo pipefail

CARGO=${CARGO:-cargo}
SIGN_IDENTITY=${SIGN_IDENTITY:--}

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

# 构建脚本已把 Info.plist 写进 OUT_DIR 并嵌进二进制；从 cargo 的 JSON 消息里取它的位置，
# 保证 .app 里的信息表和二进制里的完全一致。
messages=$("$CARGO" build --release --message-format=json-render-diagnostics)
out_dir=$(jq -r 'select(.reason == "build-script-executed" and (.package_id | test("crates/runode#"))) | .out_dir' <<<"$messages")
exe=$(jq -r 'select(.reason == "compiler-artifact" and .target.name == "runode" and .executable != null) | .executable' <<<"$messages")
plist="$out_dir/Info.plist"
[[ -f "$plist" && -x "$exe" ]] || { echo "找不到构建产物：$plist / $exe" >&2; exit 1; }

version=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$plist")
arch=$(lipo -archs "$exe" | tr ' ' '-')
bundle_dir="$root/target/release/bundle"
app="$bundle_dir/Runode.app"
dmg="$bundle_dir/Runode-$version-$arch.dmg"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$exe" "$app/Contents/MacOS/runode"
cp "$plist" "$app/Contents/Info.plist"

# 由 1024px 的源图生成 icns 所需的各档尺寸。
iconset=$(mktemp -d)/runode.iconset
mkdir -p "$iconset"
src=crates/runode/assets/icon.png
for size in 16 32 128 256 512; do
    sips -z $size $size "$src" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) "$src" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/runode.icns"
rm -rf "$(dirname "$iconset")"

codesign --force --sign "$SIGN_IDENTITY" --options runtime "$app"

# dmg 里放 .app 和指向 /Applications 的链接，打开后直接拖拽安装。
staging=$(mktemp -d)
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
rm -f "$dmg"
diskutil image create from --volumeName Runode --format UDZO "$staging" "$dmg" >/dev/null
rm -rf "$staging"

echo "$dmg"
