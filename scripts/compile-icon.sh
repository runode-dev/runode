#!/bin/bash
# 把 apps/desktop/assets/runode.icon（Icon Composer 文档）用 Xcode 26 以上的 actool 编成
# Assets.car 和 runode.icns，放进 apps/desktop/assets/compiled/ 并提交。bundle-macos.sh 只拷贝这两个
# 文件：GitHub 的 macOS arm64 runner 上 actool 十有七八崩溃（AssetCatalogAgent 断连），不能放进 CI。
# 另把 apps/desktop/assets/icon.png 缩成几种尺寸装进 runode.ico，桌面 app 的构建脚本在 Windows 上把它
# 嵌进可执行文件。改了图标之后手动跑一次。
set -euo pipefail

cd "$(dirname "$0")/.."
out=apps/desktop/assets/compiled
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# 先编进临时目录，两个文件都出来了再换掉提交着的那两个：编失败时留着上一份能用的。
mkdir -p "$out" "$tmp/compiled"
# Windows 的图标铺满画布：先裁掉 icon.png 照 macOS 规矩四周留的空白（256 的画布里圆角方块占 24 到
# 231），再缩成各个尺寸。ICO 里各尺寸直接放 PNG（Windows Vista 起认），256 的尺寸在目录里写成 0。
sips -c 208 208 --cropOffset 24 24 apps/desktop/assets/icon.png --out "$tmp/tile.png" >/dev/null
sizes=(16 24 32 48 64 128 256)
for size in "${sizes[@]}"; do
    sips -z "$size" "$size" "$tmp/tile.png" --out "$tmp/$size.png" >/dev/null
done
python3 - "$tmp" "$out/runode.ico" "${sizes[@]}" <<'PY'
import struct, sys
tmp, out, sizes = sys.argv[1], sys.argv[2], [int(size) for size in sys.argv[3:]]
images = [open(f"{tmp}/{size}.png", "rb").read() for size in sizes]
offset = 6 + 16 * len(images)
head = struct.pack("<HHH", 0, 1, len(images))
for size, image in zip(sizes, images):
    head += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(image), offset)
    offset += len(image)
open(out, "wb").write(head + b"".join(images))
PY
xcrun actool apps/desktop/assets/runode.icon --compile "$tmp/compiled" \
    --output-format human-readable-text --errors \
    --output-partial-info-plist "$tmp/partial.plist" \
    --app-icon runode --include-all-app-icons --enable-on-demand-resources NO \
    --development-region en --target-device mac --platform macosx --minimum-deployment-target 11.0 >&2
# 旧版 Xcode 的 actool 不认 .icon，不报错也不出文件。
[[ -s "$tmp/compiled/Assets.car" && -s "$tmp/compiled/runode.icns" ]] \
    || { echo "actool 没生成图标（要 Xcode 26 以上）：$(xcodebuild -version | head -1)" >&2; exit 1; }
mv -f "$tmp/compiled/Assets.car" "$tmp/compiled/runode.icns" "$out/"
