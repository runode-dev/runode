#!/bin/bash
# 发布构建 runode，打包成 Runode.app，再做成可拖进「应用程序」安装的 dmg（app 自动更新
# 下载的也是它）。产物在 target/<profile>/bundle/ 下。
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
#   REMOTE         app 模式下默认把 build-remote.sh 交叉编译的另一种架构的 Mac 版、Windows 版和 Linux 版放进
#                  Contents/Resources/remote，本机打的包连这几种 SSH 远端时装它们（发布页上没有这个构建的包）；
#                  给 0 时不放。缺交叉编译的工具时跳过并提示。dmg 模式发的是发布构建，远端从发布页下载，不放。
#   NOTARY_PROFILE 公证用的 notarytool 钥匙串配置名（xcrun notarytool store-credentials 存的），或者
#   NOTARY_KEY、NOTARY_KEY_ID、NOTARY_ISSUER
#                  App Store Connect API 密钥（.p8 文件的路径、密钥 ID、Issuer ID），CI 里用这个。
#                  两样都没给时不公证；dmg 模式下给了就公证 .app 和 dmg，并把公证票据钉进 .app 和 dmg。
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
dmg="$bundle_dir/Runode-$version-macos-$arch.dmg"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$exe" "$app/Contents/MacOS/runode"
# 每个版本写同一个 Mach-O UUID，升级后接手过来的终端照样连得上局域网。
python3 -I scripts/set-macho-uuid.py "$app/Contents/MacOS/runode" "$arch"
# 命令行的短名字：app 把这个目录加进终端的 PATH，敲 rn 和敲 runode 一样。
ln -s runode "$app/Contents/MacOS/rn"
cp "$plist" "$app/Contents/Info.plist"

# 应用图标是 Icon Composer 的 .icon 编出来的 Assets.car，系统按统一的圆角方形裁剪，跟着切换深色
# 和染色；形状不合规的旧式 icns 在 macOS 26 起会被套进灰框缩小显示。runode.icns 给旧系统用。
# 两个文件是提交进仓库的产物，用 scripts/compile-icon.sh 生成。
cp apps/desktop/assets/compiled/Assets.car apps/desktop/assets/compiled/runode.icns "$app/Contents/Resources/"
# 终端字体 Hack Nerd Font Mono 编进了二进制，它的许可（MIT、Bitstream Vera）要求分发时带上许可证。
cp apps/desktop/assets/fonts/HackNerdFontMono/LICENSE.md "$app/Contents/Resources/HackNerdFontMono-LICENSE.md"

if [[ "$target" == app && "${REMOTE:-1}" != 0 ]]; then
    remote_profile=$BUILD_PROFILE
    [[ "$remote_profile" != debug ]] || remote_profile=dev
    status=0
    CARGO_PROFILE=$remote_profile scripts/build-remote.sh "$app/Contents/Resources/remote" >/dev/null || status=$?
    if [[ $status == 2 ]]; then
        echo "Runode.app 里不带远端用的构建：本机打的包连 Windows、Linux 和另一种架构的 Mac 时会装上发布页上的旧版" >&2
    elif [[ $status != 0 ]]; then
        exit "$status"
    fi
fi

# 公证要带安全时间戳；ad-hoc 签名没有时间戳可带。
sign_options=(--force --sign "$SIGN_IDENTITY")
if [[ "$SIGN_IDENTITY" != - ]]; then
    sign_options+=(--timestamp)
fi
codesign "${sign_options[@]}" --options runtime --entitlements apps/desktop/runode.entitlements "$app"

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
    # 公证队列积压或连接挂起时别一直等：发版的并发组不取消在跑的，这一步卡住后面所有发版都排队。
    # 退出码不作数（文档没写没通过、等超时时退出码是几），以输出的 status 为准；不然退出码非 0 时
    # `set -e` 直接退出，打不出下面的公证日志。
    result=$(xcrun notarytool submit "$1" "${auth[@]}" --wait --timeout 45m --output-format json) || true
    id=$(jq -r .id <<<"$result")
    status=$(jq -r .status <<<"$result")
    if [[ "$status" != Accepted ]]; then
        echo "公证没通过（${status}）：${1}" >&2
        [[ -z "$id" || "$id" == null ]] || xcrun notarytool log "$id" "${auth[@]}" >&2 || true
        exit 1
    fi
}
# stapler 从 Apple 的 CloudKit 取票据，CI 的 runner 上连接时不时断掉（NSURLErrorDomain -1005），隔一会儿再试。
staple() {
    local attempt
    for attempt in 1 2 3 4; do
        xcrun stapler staple "$1" && return
        [[ $attempt == 4 ]] || sleep $((attempt * 15))
    done
    echo "钉不上公证票据：$1" >&2
    exit 1
}
notarizing=false
if [[ -n "${NOTARY_PROFILE:-}" || -n "${NOTARY_KEY:-}" ]]; then
    notarizing=true
fi

# 公证要交 zip（ditto 打包时保留符号链接 rn 和扩展属性），通过后把票据钉进 .app，没联网时
# Gatekeeper 也认；zip 只用来提交，不发布。
if $notarizing; then
    zip_dir=$(mktemp -d)
    zip="$zip_dir/Runode.zip"
    ditto -c -k --keepParent --sequesterRsrc "$app" "$zip"
    notarize "$zip"
    staple "$app"
    rm -rf "$zip_dir"
fi

# dmg 里放 .app 和指向 /Applications 的链接，打开后直接拖拽安装。
staging=$(mktemp -d)
# 中途失败或被中断时也删掉，不留一份完整的 .app；成功时下面做完 dmg 就删，不占着磁盘等公证。
trap 'rm -rf "$staging"' EXIT
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
rm -f "$dmg"
# diskutil image 的 --volumeName 新系统才有，CI 的 macOS 15 runner 上没有，那里退回 hdiutil；新系统上
# hdiutil 已经标成弃用。
if diskutil image create from --help 2>&1 | grep -q -- --volumeName; then
    diskutil image create from --volumeName Runode --format UDZO "$staging" "$dmg" >/dev/null
else
    # GitHub 的 macOS runner 上 hdiutil 时不时报 Resource busy（镜像被系统占着），等一下重来就好。
    for attempt in 1 2 3 4 5; do
        hdiutil create -volname Runode -srcfolder "$staging" -format UDZO "$dmg" >/dev/null && break
        [[ $attempt -lt 5 ]] || exit 1
        echo "hdiutil create 失败，${attempt}0 秒后重试" >&2
        rm -f "$dmg"
        sleep $((attempt * 10))
    done
fi
rm -rf "$staging"
if [[ "$SIGN_IDENTITY" != - ]]; then
    codesign "${sign_options[@]}" "$dmg"
fi
if $notarizing; then
    notarize "$dmg"
    staple "$dmg"
fi

echo "$dmg"
