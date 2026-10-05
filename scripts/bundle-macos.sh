#!/bin/bash
# 发布构建 runode，打包成 Runode.app，再做成可拖进「应用程序」安装的 dmg。
# 产物在 target/release/bundle/ 下。
#
# 用法：bundle-macos.sh [app|dmg]，默认 dmg；app 只打包到 Runode.app 为止。
#
# 环境变量：
#   CARGO          cargo 命令，默认 cargo
#   SIGN_IDENTITY  codesign 签名身份，默认 -（ad-hoc，只适合本机或自己用）
#
# 编译应用图标要用 Xcode 自带的 actool，需要装 Xcode。
set -euo pipefail

target=${1:-dmg}
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

# 应用图标用 Icon Composer 的 .icon 文档，由 Xcode 的 actool 编译成 Assets.car，系统按
# 统一的圆角方形裁剪，跟着切换深色和染色；形状不合规的旧式 icns 在 macOS 26 起会被套进
# 灰框缩小显示。actool 同时生成给旧系统用的 runode.icns。
icon_build=$(mktemp -d)
xcrun actool crates/runode/assets/runode.icon --compile "$app/Contents/Resources" \
    --output-format human-readable-text --errors \
    --output-partial-info-plist "$icon_build/partial.plist" \
    --app-icon runode --include-all-app-icons --enable-on-demand-resources NO \
    --development-region en --target-device mac --platform macosx --minimum-deployment-target 11.0 >&2
rm -rf "$icon_build"

codesign --force --sign "$SIGN_IDENTITY" --options runtime "$app"

if [[ "$target" == app ]]; then
    echo "$app"
    exit
fi

# dmg 里放 .app 和指向 /Applications 的链接，打开后直接拖拽安装。
staging=$(mktemp -d)
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
rm -f "$dmg"
diskutil image create from --volumeName Runode --format UDZO "$staging" "$dmg" >/dev/null
rm -rf "$staging"

echo "$dmg"
