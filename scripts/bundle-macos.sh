#!/bin/bash
# 发布构建 runode，打包成 Runode.app，再做成可拖进「应用程序」安装的 dmg，和 app 自动更新时下载的
# zip。产物在 target/<profile>/bundle/ 下。
#
# 用法：bundle-macos.sh [app|dmg]，默认 dmg；app 只打包到 Runode.app 为止。
#
# 环境变量：
#   CARGO          cargo 命令，默认 cargo
#   BUILD_PROFILE  cargo 的 profile，默认 release；产物在 target/<profile>/bundle/ 下。`make app`
#                  和 `make install` 用编得快的 local，只给本机用。
#   SIGN_IDENTITY  codesign 签名身份，比如 "Developer ID Application: 名字 (TEAMID)"。没给时用钥匙串里
#                  第一个有效的 Developer ID Application 证书，没有这种证书时退回 -（ad-hoc）。
#                  ad-hoc 签名的系统按 cdhash 认，每次重新打包都是另一个 app，隐私与安全里给过的
#                  权限就不算了；app 也只在新包和自己出自同一个 Team ID 时才自己更新，ad-hoc 签名的
#                  不更新。要强制 ad-hoc 就给 -。
#   NOTARY_PROFILE 公证用的 notarytool 钥匙串配置名（xcrun notarytool store-credentials 存的），或者
#   NOTARY_KEY、NOTARY_KEY_ID、NOTARY_ISSUER
#                  App Store Connect API 密钥（.p8 文件的路径、密钥 ID、Issuer ID），CI 里用这个。
#                  两样都没给时不公证；dmg 模式下给了就公证 zip 和 dmg，并把公证票据钉进 .app 和 dmg。
set -euo pipefail

target=${1:-dmg}
CARGO=${CARGO:-cargo}
BUILD_PROFILE=${BUILD_PROFILE:-release}
if [[ -z "${SIGN_IDENTITY:-}" ]]; then
  # 用证书的 SHA-1 指纹而不是名字：续签后新旧两张证书同名时，按名字签 codesign 会说有歧义。
  SIGN_IDENTITY=$(security find-identity -v -p codesigning |
    awk '/"Developer ID Application: / { print $2; exit }')
  SIGN_IDENTITY=${SIGN_IDENTITY:--}
fi

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

# 工作区有没提交的改动（含没被忽略的新文件）时，构建号带上改动内容的摘要，见 apps/desktop 的构建
# 脚本：改了代码没提交就重新打包，新宿主的构建号也和在跑的不同，才接得了手。同样的改动摘要不变，
# 不会每次打包都重编。
if [[ -n "$(git status --porcelain)" ]]; then
  untracked=$(git ls-files --others --exclude-standard)
  digest=$(
    {
      git diff HEAD --binary
      printf '%s\n' "$untracked"
      if [[ -n "$untracked" ]]; then git hash-object --stdin-paths <<<"$untracked"; fi
    } | shasum | cut -c1-7
  )
  export RUNODE_BUILD_TAG="dirty.$digest"
else
  unset RUNODE_BUILD_TAG
fi

# 构建脚本已把 Info.plist 写进 OUT_DIR 并嵌进二进制；从 cargo 的 JSON 消息里取它的位置，
# 保证 .app 里的信息表和二进制里的完全一致。
messages=$("$CARGO" build --profile "$BUILD_PROFILE" --message-format=json-render-diagnostics)
out_dir=$(jq -r 'select(.reason == "build-script-executed" and (.package_id | test("apps/desktop#"))) | .out_dir' <<<"$messages")
exe=$(jq -r 'select(.reason == "compiler-artifact" and .target.name == "runode" and .executable != null) | .executable' <<<"$messages")
plist="$out_dir/Info.plist"
[[ -f "$plist" && -x "$exe" ]] || { echo "找不到构建产物：$plist / $exe" >&2; exit 1; }

version=$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$plist")
arch=$(lipo -archs "$exe" | tr ' ' '-')
bundle_dir="$root/target/$BUILD_PROFILE/bundle"
app="$bundle_dir/Runode.app"
dmg="$bundle_dir/Runode-$version-$arch.dmg"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$exe" "$app/Contents/MacOS/runode"
# 命令行的短名字：app 把这个目录加进终端的 PATH，敲 rn 和敲 runode 一样。
ln -s runode "$app/Contents/MacOS/rn"
cp "$plist" "$app/Contents/Info.plist"

# 应用图标是 Icon Composer 的 .icon 编出来的 Assets.car，系统按统一的圆角方形裁剪，跟着切换深色
# 和染色；形状不合规的旧式 icns 在 macOS 26 起会被套进灰框缩小显示。runode.icns 给旧系统用。
# 两个文件是提交进仓库的产物，用 scripts/compile-icon.sh 生成。
cp apps/desktop/assets/compiled/Assets.car apps/desktop/assets/compiled/runode.icns "$app/Contents/Resources/"

# 公证要带安全时间戳；ad-hoc 签名没有时间戳可带。
sign_options=(--force --sign "$SIGN_IDENTITY")
if [[ "$SIGN_IDENTITY" != - ]]; then
    sign_options+=(--timestamp)
fi
codesign "${sign_options[@]}" --options runtime "$app"

if [[ "$target" == app ]]; then
    echo "$app"
    exit
fi

# 交给公证服务，等到有结果；没通过时打出公证日志再失败。
notarize() {
    local auth
    if [[ -n "${NOTARY_PROFILE:-}" ]]; then
        auth=(--keychain-profile "$NOTARY_PROFILE")
    else
        auth=(--key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
    fi
    local result id status
    result=$(xcrun notarytool submit "$1" "${auth[@]}" --wait --output-format json)
    id=$(jq -r .id <<<"$result")
    status=$(jq -r .status <<<"$result")
    if [[ "$status" != Accepted ]]; then
        echo "公证没通过（$status）：$1" >&2
        xcrun notarytool log "$id" "${auth[@]}" >&2 || true
        exit 1
    fi
}
notarizing=false
if [[ -n "${NOTARY_PROFILE:-}" || -n "${NOTARY_KEY:-}" ]]; then
    notarizing=true
fi

# 自动更新下载的 zip。ditto 打包时保留符号链接（rn）和扩展属性，解压后签名才对得上。公证要交 zip，
# 通过后把票据钉进 .app 再重新打包，没联网时 Gatekeeper 也认。
zip="$bundle_dir/Runode-$version-$arch.zip"
rm -f "$zip"
ditto -c -k --keepParent --sequesterRsrc "$app" "$zip"
if $notarizing; then
    notarize "$zip"
    xcrun stapler staple "$app"
    rm -f "$zip"
    ditto -c -k --keepParent --sequesterRsrc "$app" "$zip"
fi

# dmg 里放 .app 和指向 /Applications 的链接，打开后直接拖拽安装。
staging=$(mktemp -d)
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
rm -f "$dmg"
# diskutil image 的 --volumeName 新系统才有，CI 的 macOS 15 runner 上没有，那里退回 hdiutil；新系统上
# hdiutil 已经标成弃用。
if diskutil image create from --help 2>&1 | grep -q -- --volumeName; then
    diskutil image create from --volumeName Runode --format UDZO "$staging" "$dmg" >/dev/null
else
    hdiutil create -volname Runode -srcfolder "$staging" -format UDZO "$dmg" >/dev/null
fi
rm -rf "$staging"
if [[ "$SIGN_IDENTITY" != - ]]; then
    codesign "${sign_options[@]}" "$dmg"
fi
if $notarizing; then
    notarize "$dmg"
    xcrun stapler staple "$dmg"
fi

echo "$dmg"
echo "$zip"
