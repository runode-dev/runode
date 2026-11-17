#!/usr/bin/env bash
# 在专用的模拟器上调试构建、启动 iOS app；要连真的电脑时，经仓库根目录 verify skill 的 sandbox.sh 起一个沙盒
# 宿主，开只听 127.0.0.1 的远程访问，再把配对链接交给 app。
#
# 只用专用的那台模拟器：别的模拟器上的 Runode 可能已经和用户自己的电脑配过对，在那里点按会动到用户真的终端。
# 专用的这台按名字认，没有就建一台，留着复用（第一次开机慢）；每次 up 都卸掉重装 app，它只认得沙盒宿主。
#
# 用法：sim.sh device|up|launch [ARG...]|host|pair|paired|sandbox ARG...|status|logs|shot FILE|down
#   device   打印专用模拟器的 UDID，没有就先建一台；不开机
#   up       找到或建好专用模拟器并开机，卸掉上面的 Runode，调试构建（run-simulator.sh）、装上、启动，等它连上界面
#   launch   重启 app，ARG 原样交给它（比如 -runode-demo terminal），等它连上界面
#   host     用 sandbox.sh 起沙盒宿主，开只听 127.0.0.1 的远程访问，等监听起来
#   pair     不带参数重启 app，在沙盒里跑 runode remote pair，拿到链接用 simctl openurl 交给 app；接下来的点按
#            由界面操作完成
#   paired   等 pair 配上设备，最多 PAIR_SECS 秒，打印配上的设备
#   sandbox  原样交给 sandbox.sh，沙盒目录用本脚本的，比如 sim.sh sandbox rn list
#   status   模拟器、app、宿主和配对各在什么状态
#   logs     从 up 起 app 自己的日志、Runode 进程的 fault 和崩溃报告；有崩溃报告时退出码为 1
#   shot     模拟器截图存到 FILE
#   down     停 pair、关沙盒宿主（删沙盒目录）、关 app；模拟器在 up 之前关着就关机；删状态目录
#
# 专用模拟器只有一台，同一时间只给一个工作树用：up 记下是哪个工作树，在它 down 之前，别的工作树跑读状态的
# 子命令（up、launch、pair、paired、status、logs、shot、down）都报错。
#
# 环境变量：
#   RUNODE_VERIFY_SIM       专用模拟器的名字，默认 Runode Verify，要以它开头
#   RUNODE_VERIFY_SIM_TYPE  建专用模拟器时的机型名，比如 "iPad Pro 11-inch (M5)"；默认最新 iOS 上第一台 iPhone 的机型
#   RUNODE_VERIFY_HOME      沙盒目录，默认 /tmp/runode-ios-<工作树路径的短哈希>，和桌面验证的沙盒分开
#   READY_SECS              等 app 连上界面、等远程访问监听的上限，默认 30 秒
#   BOOT_SECS               等模拟器开机的上限，默认 180 秒
#   PAIR_SECS               paired 等配对的上限，默认 60 秒
set -euo pipefail

BASE_NAME="Runode Verify"
NAME="${RUNODE_VERIFY_SIM:-$BASE_NAME}"
BUNDLE=dev.runode.mobile
READY_SECS="${READY_SECS:-30}"
BOOT_SECS="${BOOT_SECS:-180}"
PAIR_SECS="${PAIR_SECS:-60}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(git -C "$HERE" rev-parse --show-toplevel)"
IOS="$ROOT/apps/ios"
SANDBOX_SH="$ROOT/.claude/skills/verify/scripts/sandbox.sh"
export RUNODE_VERIFY_HOME="${RUNODE_VERIFY_HOME:-/tmp/runode-ios-$(printf '%s' "$ROOT" | shasum | cut -c1-8)}"
SANDBOX="$RUNODE_VERIFY_HOME"
CONFIG="$SANDBOX/.runode/config.conf"
LISTENER="$SANDBOX/.runode/remote-access/listener.json"
STATE=/tmp/runode-ios-verify
# 沙盒宿主在手机上显示的电脑名。
HOST_NAME=runode-verify

die() { echo "sim: $*" >&2; exit 1; }

case "$NAME" in
    "$BASE_NAME"*) ;;
    *) die "RUNODE_VERIFY_SIM 要以「${BASE_NAME}」开头：本脚本会卸装这台上的 app，不用别的模拟器" ;;
esac

now() { date '+%Y-%m-%d %H:%M:%S'; }

# 专用模拟器的 UDID；没有时打印空行。
find_udid() {
    xcrun simctl list devices available --json | /usr/bin/python3 -I -c '
import json, sys
for ds in json.load(sys.stdin)["devices"].values():
    for d in ds:
        if d["name"] == sys.argv[1]:
            print(d["udid"]); sys.exit()
print()
' "$NAME"
}

# 建专用模拟器用的机型和系统：给了机型名时取支持它的最新系统；没给时取最新的 iOS（同版本里支持机型最多的那份，
# 不挑只给个别机型的特殊系统），用它上面第一台 iPhone 的机型。打印「机型 系统」两个标识。
pick_type() {
    /usr/bin/python3 -I -c '
import json, subprocess, sys
want = sys.argv[1]
def simctl(*args):
    return json.loads(subprocess.check_output(["xcrun", "simctl", "list", *args, "--json"]))
runtimes = [r for r in simctl("runtimes")["runtimes"] if r["isAvailable"] and r["platform"] == "iOS"]
runtimes.sort(key=lambda r: ([int(p) for p in r["version"].split(".")], len(r.get("supportedDeviceTypes", []))), reverse=True)
if want:
    for r in runtimes:
        for t in r.get("supportedDeviceTypes", []):
            if t["name"] == want:
                print(t["identifier"], r["identifier"]); sys.exit()
    sys.exit("没有支持「%s」的 iOS 系统，用 xcrun simctl list devicetypes 看机型名" % want)
devices = simctl("devices", "available")["devices"]
for r in runtimes:
    for d in devices.get(r["identifier"], []):
        if d["name"].startswith("iPhone"):
            print(d["deviceTypeIdentifier"], r["identifier"]); sys.exit()
sys.exit("找不到可用的 iPhone 模拟器，到 Xcode 的设置里装上 iOS 平台")
' "${RUNODE_VERIFY_SIM_TYPE:-}"
}

ensure_udid() {
    local udid picked
    udid="$(find_udid)"
    if [ -z "$udid" ]; then
        picked="$(pick_type)"
        echo "建专用模拟器「${NAME}」：$picked" >&2
        # shellcheck disable=SC2086 # 两个标识里没有空格，按空格拆开
        udid="$(xcrun simctl create "$NAME" $picked)"
    fi
    echo "$udid"
}

device_state() {
    xcrun simctl list devices --json | /usr/bin/python3 -I -c '
import json, sys
for ds in json.load(sys.stdin)["devices"].values():
    for d in ds:
        if d["udid"] == sys.argv[1]:
            print(d["state"]); sys.exit()
print("Missing")
' "$1"
}

# 状态目录归别的工作树（它跑的 up）时报错；没有状态或归本工作树时什么都不做。
check_owner() {
    local owner
    owner="$(cat "$STATE/root" 2>/dev/null || true)"
    [ -z "$owner" ] || [ "$owner" = "$ROOT" ] ||
        die "专用模拟器正被工作树 ${owner} 用着（状态目录 ${STATE}），到那边操作，或等那边 sim.sh down 之后再来；那个工作树已经不在时，删掉 ${STATE} 再来"
}

# up 记下的 UDID；没跑过 up 时报错。
state_udid() {
    check_owner
    [ -s "$STATE/udid" ] || die "还没跑 sim.sh up"
    cat "$STATE/udid"
}

# 跑一条命令，最多 $1 秒，超时就结束它并报错。
bounded() {
    local secs="$1" pid deadline
    shift
    "$@" &
    pid=$!
    deadline=$((SECONDS + secs))
    while kill -0 "$pid" 2>/dev/null; do
        if [ "$SECONDS" -ge "$deadline" ]; then
            kill "$pid" 2>/dev/null || true
            die "等了 ${secs} 秒还没完：$*"
        fi
        sleep 0.2
    done
    wait "$pid"
}

gui_running() { pgrep -qx DeviceHub || pgrep -qx Simulator; }

# 等 app 进程 $1 连上界面：SpringBoard 记下它的场景连上了（启动参数处理完、首页开始画）。最多 READY_SECS 秒，
# 进程先退出就报错。$2 是启动前的时间，查日志从这里开始。
wait_ready() {
    local udid pid="$1" since="$2" deadline connected
    udid="$(state_udid)"
    deadline=$((SECONDS + READY_SECS))
    while :; do
        kill -0 "$pid" 2>/dev/null || die "app（pid ${pid}）在连上界面前退出了，看 sim.sh logs"
        # 先收完整输出再匹配：直接管给 grep -q 时 grep 提前退出，log show 吃 SIGPIPE，pipefail 下这一轮判失败。
        connected="$(bounded "$READY_SECS" xcrun simctl spawn "$udid" log show --start "$since" --style compact --predicate \
            "process == \"SpringBoard\" AND eventMessage CONTAINS \"Client process connected\" AND eventMessage CONTAINS \":${pid}]\"" \
            2>/dev/null || true)"
        case "$connected" in *":${pid}]"*) return 0 ;; esac
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${READY_SECS} 秒 app（pid ${pid}）还没连上界面，截个图看看：sim.sh shot /tmp/x.png"
        sleep 0.5
    done
}

# simctl launch 打印的「dev.runode.mobile: 1234」里的进程号。
launched_pid() {
    local pid="${1##*: }"
    case "$pid" in '' | *[!0-9]*) die "simctl 没打印进程号：$1" ;; esac
    echo "$pid"
}

cmd_up() {
    local udid since pid
    check_owner
    mkdir -p "$STATE"
    echo "$ROOT" >"$STATE/root"
    udid="$(ensure_udid)"
    echo "$udid" >"$STATE/udid"
    # 只在第一次 up 时记：down 照这个决定关不关机、关不关模拟器窗口。
    if [ ! -e "$STATE/booted-before" ]; then
        if [ "$(device_state "$udid")" = Booted ]; then echo 1; else echo 0; fi >"$STATE/booted-before"
        if gui_running; then echo 1; else echo 0; fi >"$STATE/gui-before"
    fi
    since="$(now)"
    echo "$since" >"$STATE/since"
    [ "$(device_state "$udid")" = Booted ] || xcrun simctl boot "$udid"
    bounded "$BOOT_SECS" xcrun simctl bootstatus "$udid" -b >/dev/null
    # 全新安装：配过的电脑、设置都清掉，只会连这次的沙盒宿主。
    xcrun simctl terminate "$udid" "$BUNDLE" 2>/dev/null || true
    xcrun simctl uninstall "$udid" "$BUNDLE"
    if ! IOS_SIM="$udid" "$IOS/scripts/run-simulator.sh" >"$STATE/build.log" 2>&1; then
        tail -n 40 "$STATE/build.log" >&2
        die "构建、安装或启动失败，上面是输出末尾，完整的在 $STATE/build.log"
    fi
    pid="$(launched_pid "$(grep "^${BUNDLE}: " "$STATE/build.log" | tail -n 1 || true)")"
    echo "$pid" >"$STATE/app.pid"
    wait_ready "$pid" "$since"
    echo "app 就绪：pid ${pid}，模拟器「${NAME}」${udid}，bundle id $BUNDLE"
}

cmd_launch() {
    local udid since out pid
    udid="$(state_udid)"
    since="$(now)"
    out="$(bounded "$READY_SECS" xcrun simctl launch --terminate-running-process "$udid" "$BUNDLE" "$@")"
    pid="$(launched_pid "$out")"
    echo "$pid" >"$STATE/app.pid"
    wait_ready "$pid" "$since"
    echo "app 就绪：pid ${pid}${*:+，参数 $*}"
}

listener_field() {
    /usr/bin/python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1])).get(sys.argv[2]) or "")' "$LISTENER" "$1"
}

cmd_host() {
    local port deadline
    [ -x "$SANDBOX_SH" ] || die "找不到 ${SANDBOX_SH}：要仓库根目录的 verify skill"
    "$SANDBOX_SH" host
    if [ ! -s "$LISTENER" ]; then
        # 只听本机回环、换一个空闲端口：不和用户自己的 runode 抢端口，Bonjour 也不往局域网里公布。
        if ! grep -q '^remote-access' "$CONFIG" 2>/dev/null; then
            port="$(/usr/bin/python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
            printf 'remote-access = true\nremote-access-address = 127.0.0.1\nremote-access-port = %s\nremote-access-name = %s\n' \
                "$port" "$HOST_NAME" >>"$CONFIG"
        fi
        deadline=$((SECONDS + READY_SECS))
        until [ -s "$LISTENER" ]; do
            [ "$SECONDS" -lt "$deadline" ] || die "等了 ${READY_SECS} 秒远程访问还没起来，看 sim.sh sandbox logs"
            sleep 0.2
        done
    fi
    [ "$(listener_field bound)" = 127.0.0.1 ] || die "远程访问没有只听 127.0.0.1，看 $CONFIG"
    echo "远程访问就绪：127.0.0.1:$(listener_field port)，电脑名 $HOST_NAME"
}

# 进程 $1 是不是 cmd_pair 起的那个 sandbox.sh：pair 配上后自己退出，记下的进程号可能已被别的进程复用。
pair_alive() {
    case "$(ps -p "$1" -o command= 2>/dev/null || true)" in
        *"$SANDBOX_SH rn remote pair") return 0 ;;
        *) return 1 ;;
    esac
}

# 停掉还在等手机的 pair：它是 sandbox.sh 的子进程，收到 SIGTERM 时删掉口令文件再退出。
stop_pair() {
    local pid i
    [ -s "$STATE/pair.pid" ] || return 0
    pid="$(cat "$STATE/pair.pid")"
    rm -f "$STATE/pair.pid"
    pair_alive "$pid" || return 0
    pkill -TERM -P "$pid" 2>/dev/null || true
    for ((i = 0; i < 50; i++)); do
        pair_alive "$pid" || { echo "已停 pair（pid ${pid}）"; return 0; }
        sleep 0.1
    done
    pkill -KILL -P "$pid" 2>/dev/null || true
    if pair_alive "$pid"; then kill -KILL "$pid" 2>/dev/null || true; fi
    echo "pair（pid ${pid}）5 秒内没退出，强制结束" >&2
}

cmd_pair() {
    local udid uri deadline pid
    udid="$(state_udid)"
    [ -s "$LISTENER" ] || die "远程访问没开，先 sim.sh host"
    stop_pair
    # 不带参数重启一次：演示模式（-runode-demo）下配对页是假的，配不上。
    cmd_launch
    "$SANDBOX_SH" rn remote pair </dev/null >"$STATE/pair.out" 2>&1 &
    pid=$!
    echo "$pid" >"$STATE/pair.pid"
    deadline=$((SECONDS + READY_SECS))
    until uri="$(grep -o 'runode://pair?[^[:space:]]*' "$STATE/pair.out")"; do
        kill -0 "$pid" 2>/dev/null || die "pair 没打出链接就退出了：$(cat "$STATE/pair.out")"
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${READY_SECS} 秒 pair 还没打出链接"
        sleep 0.1
    done
    xcrun simctl openurl "$udid" "$uri"
    echo "配对链接已交给 app：$uri"
    # shellcheck disable=SC1111 # 弯引号是系统弹窗原文
    echo "接下来在模拟器上：系统问「在“Runode”中打开？」点「打开」，弹出「和这台电脑配对？」（${HOST_NAME}）点「配对」；然后 sim.sh paired"
}

cmd_paired() {
    local deadline pid
    check_owner
    [ -e "$STATE/pair.out" ] || die "还没跑 sim.sh pair"
    pid="$(cat "$STATE/pair.pid" 2>/dev/null || true)"
    deadline=$((SECONDS + PAIR_SECS))
    until grep -q '^Paired with' "$STATE/pair.out"; do
        # 退出前可能刚打完那一行，再看一眼。
        if { [ -z "$pid" ] || ! pair_alive "$pid"; } && ! grep -q '^Paired with' "$STATE/pair.out"; then
            die "pair 没配上就退出了：$(tail -n 3 "$STATE/pair.out")"
        fi
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${PAIR_SECS} 秒还没配上；截个图看模拟器停在哪：sim.sh shot /tmp/x.png"
        sleep 0.2
    done
    grep '^Paired with' "$STATE/pair.out"
    "$SANDBOX_SH" rn remote devices
}

cmd_status() {
    local udid pid
    check_owner
    echo "专用模拟器：${NAME}"
    udid="$(find_udid)"
    if [ -z "$udid" ]; then echo "  还没建"; else echo "  ${udid}，$(device_state "$udid")"; fi
    pid="$(cat "$STATE/app.pid" 2>/dev/null || true)"
    if [ -n "$pid" ] && ps -p "$pid" -o command= 2>/dev/null | grep -q "/Runode.app/Runode"; then echo "app：pid $pid"; else echo "app：没在跑（或已由别处重启）"; fi
    pid="$(cat "$STATE/pair.pid" 2>/dev/null || true)"
    if [ -n "$pid" ] && pair_alive "$pid"; then echo "pair：在等手机，pid $pid"; else echo "pair：没在跑"; fi
    # sandbox.sh 那行「app」说的是桌面 app，这里用不到。
    if [ -d "$SANDBOX" ]; then "$SANDBOX_SH" status | grep -v '^app：'; else echo "沙盒：不在（${SANDBOX}）"; fi
}

cmd_logs() {
    local udid since crashes
    udid="$(state_udid)"
    since="$(cat "$STATE/since")"
    echo "== 从 $since 起 app 自己的日志（subsystem ${BUNDLE}）和 Runode 进程的 fault，末尾 40 行"
    xcrun simctl spawn "$udid" log show --start "$since" --style compact --predicate \
        "subsystem == \"$BUNDLE\" OR (process == \"Runode\" AND messageType == 17)" 2>/dev/null |
        sed 1d | tail -n 40 | grep . || echo "（没有）"
    echo "== 从 $since 起的崩溃报告"
    # 模拟器里的 app 崩溃时报告写在 Mac 的诊断报告目录里，过几秒才出现；按报告头里的 bundleID 认。
    crashes="$(find "$HOME/Library/Logs/DiagnosticReports" -maxdepth 1 -name '*.ips' -newer "$STATE/since" \
        -exec grep -l "\"bundleID\":\"$BUNDLE\"" {} + 2>/dev/null || true)"
    if [ -z "$crashes" ]; then
        echo "（没有）"
        return 0
    fi
    echo "$crashes"
    return 1
}

cmd_shot() {
    local udid
    [ -n "${1:-}" ] || die "用法：sim.sh shot FILE"
    udid="$(state_udid)"
    xcrun simctl io "$udid" screenshot "$1" >/dev/null 2>&1 || die "截图失败"
    echo "截图：$1"
}

cmd_down() {
    local udid
    check_owner
    stop_pair
    if [ -d "$SANDBOX" ]; then "$SANDBOX_SH" stop; fi
    if [ -s "$STATE/udid" ]; then
        udid="$(cat "$STATE/udid")"
        xcrun simctl terminate "$udid" "$BUNDLE" 2>/dev/null || true
        if [ "$(cat "$STATE/booted-before" 2>/dev/null || echo 1)" = 0 ] && [ "$(device_state "$udid")" = Booted ]; then
            bounded 60 xcrun simctl shutdown "$udid"
            echo "已关机：${NAME}"
        fi
        # 模拟器窗口是 up 打开的、也没有别的模拟器开着时一起关掉。
        if [ "$(cat "$STATE/gui-before" 2>/dev/null || echo 1)" = 0 ] &&
            ! xcrun simctl list devices booted | grep -q '(Booted)'; then
            osascript -e 'tell application id "com.apple.dt.Devices" to quit' >/dev/null 2>&1 ||
                osascript -e 'tell application id "com.apple.iphonesimulator" to quit' >/dev/null 2>&1 || true
        fi
    fi
    rm -rf "$STATE"
    echo "已收干净：状态目录 $STATE 已删"
}

case "${1:-}" in
    device) ensure_udid ;;
    up) cmd_up ;;
    launch) shift; cmd_launch "$@" ;;
    host) cmd_host ;;
    pair) cmd_pair ;;
    paired) cmd_paired ;;
    sandbox) shift; "$SANDBOX_SH" "$@" ;;
    status) cmd_status ;;
    logs) cmd_logs ;;
    shot) shift; cmd_shot "${1:-}" ;;
    down) cmd_down ;;
    *) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
