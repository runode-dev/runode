#!/usr/bin/env bash
# 查界面文字的翻译有没有漏。
#
# 界面文字的键就是简体中文原文，翻译在 String Catalog 里：app 的在 Runode/Localizable.xcstrings，小组件扩展
# 的在 RunodeWidgets/Localizable.xcstrings。RunodeKit 里的代码不带 bundle 参数，运行时查的是 app（或扩展）
# 自己的 catalog，Xcode 不会替包里的代码把新文字收进来，所以靠这个脚本：让编译器抽出代码里所有可本地化的
# 字符串，和 catalog 对一遍。报三种问题，有就以非零退出：
#   - 代码里有、catalog 里没有的键；
#   - catalog 里缺英文或繁体翻译的键；
#   - 没被当成可本地化字符串的中文字面量（多半是该包 String(localized:) 却直接写了字符串）。
#
# 用法：apps/ios/scripts/check-strings.sh
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ios_dir="$(cd "$script_dir/.." && pwd)"
derived="$ios_dir/build/dd-strings"

xcodebuild -project "$ios_dir/Runode.xcodeproj" -scheme Runode -configuration Debug \
  -destination 'generic/platform=iOS Simulator' -derivedDataPath "$derived" \
  ARCHS=arm64 SWIFT_EMIT_LOC_STRINGS=YES build -quiet

/usr/bin/python3 -I - "$derived" "$ios_dir" <<'EOF'
import glob, json, os, re, sys

derived, root = sys.argv[1], sys.argv[2]
languages = ["en", "zh-Hant"]
# 只在调试构建里有的演示模式，里面是假数据，不翻译。
skip = {"DemoComposition.swift"}

def catalog(path):
    return json.load(open(os.path.join(root, path)))["strings"]

catalogs = {"app": catalog("Runode/Localizable.xcstrings"), "widgets": catalog("RunodeWidgets/Localizable.xcstrings")}
problems = []
located = set()
for f in glob.glob(derived + "/**/arm64/*.stringsdata", recursive=True):
    data = json.load(open(f))
    source = data["source"]
    if os.path.basename(source) in skip:
        continue
    which = "widgets" if "/RunodeWidgets/" in source else "app"
    for table, entries in data["tables"].items():
        for entry in entries:
            line = entry["location"]["startingLine"]
            located.add((source, line))
            if table != "Localizable" or entry["key"] not in catalogs[which]:
                problems.append(f"{os.path.relpath(source, root)}:{line}: 不在 {which} 的 catalog 里：{entry['key']!r}")

for which, strings in catalogs.items():
    for key, value in strings.items():
        have = value.get("localizations", {})
        missing = [lang for lang in languages if lang not in have]
        if missing:
            problems.append(f"{which} 的 catalog 里 {key!r} 缺 {', '.join(missing)} 的翻译")

for folder in ["Runode", "RunodeWidgets", "RunodeKit/Sources"]:
    for path in glob.glob(f"{root}/{folder}/**/*.swift", recursive=True):
        if os.path.basename(path) in skip:
            continue
        for line, code in enumerate(open(path), 1):
            code = code.split(" //")[0]
            if code.strip().startswith("//") or not re.search(r'"[^"]*[\u4e00-\u9fff]', code):
                continue
            if (path, line) not in located:
                problems.append(f"{os.path.relpath(path, root)}:{line}: 中文字面量没有本地化：{code.strip()}")

print("\n".join(problems) or "翻译齐全")
sys.exit(1 if problems else 0)
EOF
