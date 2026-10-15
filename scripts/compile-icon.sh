#!/bin/bash
# 把 apps/desktop/assets/runode.icon（Icon Composer 文档）用 Xcode 26 以上的 actool 编成
# Assets.car 和 runode.icns，放进 apps/desktop/assets/compiled/ 并提交。bundle-macos.sh 只拷贝这两个
# 文件：GitHub 的 macOS arm64 runner 上 actool 十有七八崩溃（AssetCatalogAgent 断连），不能放进 CI。
# 改了 runode.icon 之后手动跑一次。
set -euo pipefail

cd "$(dirname "$0")/.."
out=apps/desktop/assets/compiled
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# 先编进临时目录，两个文件都出来了再换掉提交着的那两个：编失败时留着上一份能用的。
mkdir -p "$out" "$tmp/compiled"
xcrun actool apps/desktop/assets/runode.icon --compile "$tmp/compiled" \
    --output-format human-readable-text --errors \
    --output-partial-info-plist "$tmp/partial.plist" \
    --app-icon runode --include-all-app-icons --enable-on-demand-resources NO \
    --development-region en --target-device mac --platform macosx --minimum-deployment-target 11.0 >&2
# 旧版 Xcode 的 actool 不认 .icon，不报错也不出文件。
[[ -s "$tmp/compiled/Assets.car" && -s "$tmp/compiled/runode.icns" ]] \
    || { echo "actool 没生成图标（要 Xcode 26 以上）：$(xcodebuild -version | head -1)" >&2; exit 1; }
mv -f "$tmp/compiled/Assets.car" "$tmp/compiled/runode.icns" "$out/"
