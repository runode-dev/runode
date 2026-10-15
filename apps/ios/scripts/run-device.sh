#!/usr/bin/env bash
# Release 构建 iOS app，装到连着的真机上启动。Debug 不做优化，手机上画一帧要多两成，刷新又按耗时限速，
# 帧率跟着掉，真机上试手感要用 Release；要断点调试时在 Xcode 里跑 Debug。
#
# 用哪台：IOS_DEVICE 给了就用这个名字或 UDID 的那台；没给时用第一台已经和这台电脑配对、连着的 iPhone
# （数据线或同一个网络都行）。ghostty-vt 的 xcframework 还没编过时先编一份。
#
# 签名用工程里设的开发团队，自动签名；描述文件缺了由 xcodebuild 去开发者账号里补，所以 Xcode 里要登录过
# 这个团队的账号。手机上要打开开发者模式；第一次装时还要在「设置 → 通用 → VPN 与设备管理」里信任开发者。
#
# 用法：IOS_DEVICE="我的 iPhone" apps/ios/scripts/run-device.sh
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ios_dir="$(cd "$script_dir/.." && pwd)"
derived="$ios_dir/build/dd"
bundle_id=dev.runode.mobile

if [[ ! -d "$ios_dir/Frameworks/ghostty-vt.xcframework" ]]; then
  "$script_dir/build-ghostty-vt.sh"
fi

devices_json="$(mktemp)"
trap 'rm -f "$devices_json"' EXIT
xcrun devicectl list devices --json-output "$devices_json" >/dev/null

# 从 devicectl 的 JSON 里挑一台真机：按名字或 UDID 找，没给时挑连着的 iPhone；没有连着的就不挑，
# 免得跑完耗时的 Release 构建才在安装时失败。
udid="$(/usr/bin/python3 -I -c '
import json, sys
want = sys.argv[2]
devices = [
    d for d in json.load(open(sys.argv[1]))["result"]["devices"]
    if d["hardwareProperties"].get("reality") == "physical" and d["hardwareProperties"].get("platform") == "iOS"
]
def connected(d):
    return d["connectionProperties"].get("pairingState") == "paired" and d["connectionProperties"].get("tunnelState") == "connected"
if want:
    picked = [
        d for d in devices
        if want in (d["deviceProperties"].get("name"), d["hardwareProperties"].get("udid"), d["identifier"])
    ]
else:
    phones = [d for d in devices if d["hardwareProperties"].get("deviceType") == "iPhone"]
    picked = [d for d in phones if connected(d)]
print(picked[0]["hardwareProperties"]["udid"] if picked else "")
' "$devices_json" "${IOS_DEVICE:-}")"
if [[ -z "$udid" ]]; then
  echo "找不到可用的真机${IOS_DEVICE:+：$IOS_DEVICE}，用 xcrun devicectl list devices 看有哪些" >&2
  exit 1
fi

xcodebuild -project "$ios_dir/Runode.xcodeproj" -scheme Runode -configuration Release \
  -destination "platform=iOS,id=$udid" -derivedDataPath "$derived" -allowProvisioningUpdates -quiet build

xcrun devicectl device install app --device "$udid" "$derived/Build/Products/Release-iphoneos/Runode.app"
xcrun devicectl device process launch --device "$udid" --terminate-existing "$bundle_id"
