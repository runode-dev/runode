#!/usr/bin/env bash
# 调试构建 iOS app，装到模拟器上启动。
#
# 用哪台模拟器：IOS_SIM 给了就用这个名字或 UDID 的那台；没给时用已经开着的那台，都没开就开第一台可用的
# iPhone。ghostty-vt 的 xcframework 还没编过时先编一份。
#
# 用法：IOS_SIM="iPhone 18 Pro" apps/ios/scripts/run-simulator.sh
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ios_dir="$(cd "$script_dir/.." && pwd)"
derived="$ios_dir/build/dd"
bundle_id=dev.runode.mobile

if [[ ! -d "$ios_dir/Frameworks/ghostty-vt.xcframework" ]]; then
  "$script_dir/build-ghostty-vt.sh"
fi

# 从 simctl 的 JSON 里挑一台可用的 iPhone：按名字或 UDID 找，没给时先挑开着的。
udid="$(xcrun simctl list devices available --json | /usr/bin/python3 -I -c '
import json, sys
want = sys.argv[1]
devices = [d for ds in json.load(sys.stdin)["devices"].values() for d in ds]
if want:
    picked = [d for d in devices if want in (d["name"], d["udid"])]
else:
    phones = [d for d in devices if d["name"].startswith("iPhone")]
    picked = [d for d in phones if d["state"] == "Booted"] or phones
print(picked[0]["udid"] if picked else "")
' "${IOS_SIM:-}")"
if [[ -z "$udid" ]]; then
  echo "找不到可用的模拟器${IOS_SIM:+：$IOS_SIM}，用 xcrun simctl list devices available 看有哪些" >&2
  exit 1
fi

xcrun simctl boot "$udid" 2>/dev/null || true
# 把模拟器窗口拿到前面；新版 Xcode 里 Simulator.app 换成了 DeviceHub.app。
open -a Simulator 2>/dev/null || open -a DeviceHub 2>/dev/null || true

xcodebuild -project "$ios_dir/Runode.xcodeproj" -scheme Runode -configuration Debug \
  -destination "platform=iOS Simulator,id=$udid" -derivedDataPath "$derived" -quiet build

xcrun simctl install "$udid" "$derived/Build/Products/Debug-iphonesimulator/Runode.app"
xcrun simctl launch --terminate-running-process "$udid" "$bundle_id"
