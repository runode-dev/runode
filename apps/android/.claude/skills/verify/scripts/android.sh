#!/usr/bin/env bash
# 把 Android app 的调试构建装到专用模拟器上跑起来，连上沙盒里的宿主，查日志，用完收干净。
# 宿主一律经仓库根目录 verify skill 的 sandbox.sh 起（换掉 HOME，不碰正在用的那个 runode），沙盒目录和
# 桌面验证用的分开。
#
# 只用一台专用的 AVD：别的模拟器上的 app 可能已经和用户自己的电脑配过对，在那里装、启动、点按会连上用户真的终端。
# 专用 AVD 按名字认，没有就用本机已装的系统镜像建一台，留着复用。连着的真机和别的模拟器一律不用。
# 专用 AVD 同一时间只给一个工作树用：开着、却不是这个工作树的 boot 起的时，boot 报错。
#
# 用法：android.sh prereqs|boot|install|launch|host|pair|paired|rn ARGS...|logs|shot FILE|lint-changed|status|stop
#   prereqs       查 JDK、SDK、NDK、CMake 和三份原生库（libghostty-vt、librunode_tunnel.so、sherpa-onnx）在不在、
#                 要不要重编；缺了或过期时退出码为 1，并打印该跑的 make 命令
#   boot          起专用 AVD（没有就先建；默认不开窗口、不读写快照），等到开机完成，清掉上面 app 的数据，让 app
#                 只认得这一轮的沙盒宿主；本脚本起的那台还开着时接着用它
#   install       ./gradlew :app:installDebug 覆盖安装到专用模拟器上
#   launch        冷启动 app（先结束旧进程），等到进程在、MainActivity 在前台；记下启动时刻给 logs 用。
#                 DEMO=<词> 时进调试构建的演示模式（`DemoComposition`），不连真的电脑
#   host          经 sandbox.sh 起宿主，开远程访问，只听 127.0.0.1，等到端口开出来
#   pair          在沙盒里跑 runode remote pair，把配对链接交给模拟器里的 app，等到「和这台电脑配对？」确认页出来
#   paired        等到宿主那边报配好了（要先有人在 app 里点「配对」）
#   rn            在沙盒里跑一条 runode 命令行（交给 sandbox.sh rn），比如 `android.sh rn list`
#   logs          从 launch 起的 app 日志：崩溃、ANR、app 自己的 E/F 行，加上沙盒宿主的日志；有崩溃、ANR、宿主
#                 ERROR 或 panic 时退出码为 1
#   shot FILE     截一张模拟器屏幕存到 FILE（PNG）
#   lint-changed  从 lint 报告里挑出落在改动文件（相对 HEAD，含未跟踪的）上的问题；有就退出码为 1
#   status        专用 AVD、app、宿主、配对进程的状态
#   stop          结束配对进程，关宿主、删沙盒，关掉本脚本起的模拟器；
#                 KEEP=1 留着沙盒目录、配对输出和模拟器日志，KEEP_EMULATOR=1 留着模拟器
#
# 环境变量：
#   RUNODE_VERIFY_AVD   专用 AVD 的名字，默认 Runode_Verify，要以它开头
#   WINDOW=1            boot 时开模拟器窗口
#   RUNODE_VERIFY_HOME  沙盒目录，默认 /tmp/runode-verify-android-<工作树路径的短哈希>；本脚本的状态放在它旁边
#                       的 <沙盒目录>.state
#   REMOTE_PORT         沙盒宿主的远程访问端口，默认 7877；要和正在用的 runode 的端口不同
#   BOOT_SECS           等模拟器开机的上限，默认 180 秒
#   READY_SECS          等 app、宿主、配对链接就绪的上限，默认 20 秒
#   PAIR_SECS           paired 等配好的上限，默认 60 秒
set -euo pipefail

ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
ANDROID_DIR="$ROOT/apps/android"
SANDBOX_SH="$ROOT/.claude/skills/verify/scripts/sandbox.sh"
export RUNODE_VERIFY_HOME="${RUNODE_VERIFY_HOME:-/tmp/runode-verify-android-$(printf '%s' "$ROOT" | shasum | cut -c1-8)}"
STATE="${RUNODE_VERIFY_HOME}.state"
BASE_AVD=Runode_Verify
NAME="${RUNODE_VERIFY_AVD:-$BASE_AVD}"
PKG=dev.runode.mobile
ACTIVITY="$PKG/.MainActivity"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
ADB=adb
if [ -n "$SDK" ] && [ -x "$SDK/platform-tools/adb" ]; then ADB="$SDK/platform-tools/adb"; fi
EMULATOR=emulator
if [ -n "$SDK" ] && [ -x "$SDK/emulator/emulator" ]; then EMULATOR="$SDK/emulator/emulator"; fi
AVDMANAGER=avdmanager
if [ -n "$SDK" ] && [ -x "$SDK/cmdline-tools/latest/bin/avdmanager" ]; then AVDMANAGER="$SDK/cmdline-tools/latest/bin/avdmanager"; fi
REMOTE_PORT="${REMOTE_PORT:-7877}"
BOOT_SECS="${BOOT_SECS:-180}"
READY_SECS="${READY_SECS:-20}"
PAIR_SECS="${PAIR_SECS:-60}"
# 单次 adb 调用（shell 命令、截图、界面树）的上限，秒。
ADB_SECS=30
# adb 只自动认控制台端口在这个范围里的模拟器；控制台用偶数端口，adb 用下一个。
FIRST_PORT=5554
LAST_PORT=5584
# Android 模拟器的网络里，这个地址就是这台电脑的 127.0.0.1。
HOST_ALIAS=10.0.2.2
# 沙盒宿主报给手机的电脑名，在 app 里和用户自己的电脑分得开。
HOST_NAME=runode-verify
SERIAL=""

die() { echo "android: $*" >&2; exit 1; }

case "$NAME" in
    "$BASE_AVD"*) ;;
    *) die "RUNODE_VERIFY_AVD 要以 ${BASE_AVD} 开头：本脚本会清这台上的 app 数据，不用别的 AVD" ;;
esac

mtime() { stat -c %Y "$1" 2>/dev/null || stat -f %m "$1"; }

# 多行并成一行，空格隔开。
oneline() { printf '%s' "$1" | tr '\n' ' '; }

# 跑一条命令，最多 $1 秒，超时就结束它并报错。
bounded() {
    try_bounded "$@" || {
        local rc=$?
        [ "$rc" -ne 124 ] || die "等了 ${1} 秒还没完：${*:2}"
        return "$rc"
    }
}

# 同 bounded 的上限，但超时只返回 124、不退出脚本，给有兜底的调用用。
try_bounded() {
    local secs="$1" pid deadline
    shift
    "$@" &
    pid=$!
    deadline=$((SECONDS + secs))
    while kill -0 "$pid" 2>/dev/null; do
        if [ "$SECONDS" -ge "$deadline" ]; then
            kill "$pid" 2>/dev/null || true
            return 124
        fi
        sleep 0.1
    done
    wait "$pid"
}

# adb 看得到的模拟器序列号，一行一个；带参数 any 时没就绪的（offline 等）也算。
emulators() {
    bounded "$ADB_SECS" "$ADB" devices |
        awk -v any="${1:-}" '$1 ~ /^emulator-[0-9]+$/ && (any != "" || $2 == "device") { print $1 }'
}

# 序列号 $1 那台模拟器跑的 AVD 名字；问不到时打印空行。
avd_name() {
    bounded 10 "$ADB" -s "$1" emu avd name 2>/dev/null | tr -d '\r' | sed -n 1p || true
}

# 进程 $1 是不是跑着专用 AVD 的模拟器（boot 起它时 -avd 后面紧跟着 -port）。
is_dedicated_emulator() {
    local cmd
    cmd="$(ps -p "$1" -o command= 2>/dev/null || true)"
    case "$cmd" in *" -avd $NAME -port "*) return 0 ;; esac
    return 1
}

# 开着的专用模拟器，序列号放进 SERIAL；没开时返回 1。先看 boot 记下的那台，再在 adb 看得到的模拟器里按 AVD 名字找。
find_serial() {
    [ -z "$SERIAL" ] || return 0
    local s
    s="$(cat "$STATE/emulator.serial" 2>/dev/null || true)"
    if [ -n "$s" ] && [ "$(avd_name "$s")" = "$NAME" ]; then
        SERIAL="$s"
        return 0
    fi
    for s in $(emulators any); do
        if [ "$(avd_name "$s")" = "$NAME" ]; then
            SERIAL="$s"
            return 0
        fi
    done
    return 1
}

# 只认本工作树 boot 起的那台：别的工作树起的专用模拟器上正验着别的改动。
need_serial() {
    find_serial || die "专用模拟器 ${NAME} 没开，先跑 android.sh boot"
    [ "$(cat "$STATE/emulator.serial" 2>/dev/null || true)" = "$SERIAL" ] ||
        die "专用模拟器 ${NAME}（${SERIAL}）不是这个工作树 boot 起的，到起它的地方操作，或那边 stop 之后再 boot"
}

adb_s() { bounded "$ADB_SECS" "$ADB" -s "$SERIAL" "$@"; }

# 设备上的一条 shell 命令，去掉行尾的 \r。
dev_sh() { adb_s shell "$@" | tr -d '\r'; }

installed() {
    local out
    out="$(dev_sh pm path "$PKG" 2>/dev/null || true)"
    grep -q '^package:' <<<"$out"
}

app_pid() { dev_sh pidof "$PKG" || true; }

# 屏幕的界面树里有没有正好是这段文字的节点。dump 一次要一两秒。
on_screen() {
    local tree
    tree="$(adb_s exec-out uiautomator dump /dev/tty 2>/dev/null || true)"
    grep -qF "text=\"$1\"" <<<"$tree"
}

cmd_prereqs() {
    local bad=0 v ndk cmake triple lib so dir ghostty_head newer dirty speech_version
    v="$(java -version 2>&1 | sed -n '1s/.*version "\([0-9]*\).*/\1/p' || true)"
    if [ "$v" = 21 ]; then echo "ok    JDK 21"; else echo "注意  java 的版本是 ${v:-（找不到 java）}，要 JDK 21"; bad=1; fi
    if [ -z "$SDK" ] || [ ! -d "$SDK" ]; then
        echo "缺    ANDROID_HOME 没设或目录不在"; bad=1
    else
        ndk="$(sed -n 's/^ndk = "\(.*\)"/\1/p' "$ANDROID_DIR/gradle/libs.versions.toml")"
        cmake="$(sed -n 's/^cmake = "\(.*\)"/\1/p' "$ANDROID_DIR/gradle/libs.versions.toml")"
        if [ -d "$SDK/ndk/$ndk" ]; then echo "ok    NDK $ndk"; else echo "缺    NDK ${ndk}：在 SDK Manager 里装"; bad=1; fi
        if [ -d "$SDK/cmake/$cmake" ]; then echo "ok    CMake $cmake"; else echo "缺    CMake ${cmake}：在 SDK Manager 里装"; bad=1; fi
    fi

    # libghostty-vt：编出来以后子模块换过提交，或者子模块里改过的文件比它新，就要重编。
    ghostty_head="$(git -C "$ROOT/vendor/ghostty" rev-parse --absolute-git-dir 2>/dev/null || true)/HEAD"
    for triple in aarch64-linux-android x86_64-linux-android; do
        dir="$ANDROID_DIR/build/ghostty-vt/$triple"
        lib="$dir/lib/libghostty-vt.a"
        if [ ! -f "$lib" ]; then
            echo "缺    ${lib}：make android-vt"; bad=1; continue
        fi
        if [ -f "$ghostty_head" ] && [ "$(mtime "$ghostty_head")" -gt "$(mtime "$dir")" ]; then
            echo "过期  libghostty-vt（${triple}）：vendor/ghostty 在编完之后换过提交，make android-vt"; bad=1; continue
        fi
        dirty="$(git -C "$ROOT/vendor/ghostty" status --porcelain --untracked-files=no 2>/dev/null | cut -c4- |
            while read -r f; do if [ "$ROOT/vendor/ghostty/$f" -nt "$dir" ]; then echo "$f"; fi; done || true)"
        if [ -n "$dirty" ]; then
            echo "过期  libghostty-vt（${triple}）：vendor/ghostty 里这些改动比它新，make android-vt：$(oneline "$dirty")"; bad=1; continue
        fi
        echo "ok    libghostty-vt（${triple}）"
    done

    # 拨号库：apps/android/tunnel 下有比它新的文件就要重编。
    for abi in arm64-v8a x86_64; do
        so="$ANDROID_DIR/connection/src/main/jniLibs/$abi/librunode_tunnel.so"
        if [ ! -f "$so" ]; then
            echo "缺    ${so}：make android-tunnel"; bad=1; continue
        fi
        newer="$(find "$ANDROID_DIR/tunnel" -type f -not -path '*/target/*' -newer "$so" 2>/dev/null | sed "s|^$ROOT/||" || true)"
        if [ -n "$newer" ]; then
            echo "过期  librunode_tunnel.so（${abi}）：这些文件比它新，make android-tunnel：$(oneline "$newer")"; bad=1; continue
        fi
        echo "ok    librunode_tunnel.so（${abi}）"
        if [ "$ROOT/Cargo.lock" -nt "$so" ]; then
            echo "      Cargo.lock 比它新：git log -p Cargo.lock 里 iroh、jni、tokio 这些拨号库的依赖变了才要重编"
        fi
    done

    # sherpa-onnx：取出来的版本要等于取的脚本里写的版本。
    speech_version="$(sed -n 's/^version="\(.*\)"/\1/p' "$ANDROID_DIR/scripts/fetch-sherpa-onnx.sh")"
    dir="$ANDROID_DIR/build/sherpa-onnx"
    if [ ! -f "$dir/classes.jar" ] || [ ! -f "$dir/jni/arm64-v8a/libsherpa-onnx-jni.so" ] ||
        [ ! -f "$dir/jni/x86_64/libsherpa-onnx-jni.so" ]; then
        echo "缺    ${dir}：make android-speech"; bad=1
    elif [ "$(cat "$dir/VERSION" 2>/dev/null)" != "$speech_version" ]; then
        echo "过期  sherpa-onnx 是 $(cat "$dir/VERSION" 2>/dev/null || echo 未知)，脚本要 ${speech_version}：make android-speech"; bad=1
    else
        echo "ok    sherpa-onnx ${speech_version}"
    fi
    return "$bad"
}

# 建专用 AVD 用的系统镜像：本机已装的、ABI 是 $3、API 不低于 $1（minSdk）的镜像里，优先不高于 $2（targetSdk）的
# 最高版本，其次高于它的最低版本；同版本里 google_apis 先于 google_apis_playstore 先于 default。
# 打印 sdkmanager 的包名，没有合适的打印空行。
pick_image() {
    local min="$1" target="$2" abi="$3" d props api tag rank
    for d in "$SDK"/system-images/*/*/"$abi"; do
        props="$d/source.properties"
        [ -f "$props" ] || continue
        api="$(sed -n 's/^AndroidVersion.ApiLevel=//p' "$props")"
        case "$api" in '' | *[!0-9]*) continue ;; esac
        [ "$api" -ge "$min" ] || continue
        tag="$(basename "$(dirname "$d")")"
        case "$tag" in
            google_apis) rank=0 ;;
            google_apis_playstore) rank=1 ;;
            default) rank=2 ;;
            *) continue ;;
        esac
        if [ "$api" -le "$target" ]; then
            echo "0 $((1000 - api)) $rank system-images;$(basename "$(dirname "$(dirname "$d")")");$tag;$abi"
        else
            echo "1 $api $rank system-images;$(basename "$(dirname "$(dirname "$d")")");$tag;$abi"
        fi
    done | sort -n -k1,1 -k2,2 -k3,3 | sed -n '1s/^[^ ]* [^ ]* [^ ]* //p'
}

# 专用 AVD 不在就建一台。只用已装的系统镜像，不下载。
ensure_avd() {
    local avds toml min target abi image devices device="" d out
    avds="$("$EMULATOR" -list-avds 2>/dev/null || true)"
    if grep -qx "$NAME" <<<"$avds"; then return 0; fi
    command -v "$AVDMANAGER" >/dev/null || die "找不到 avdmanager：在 SDK Manager 里装 Android SDK Command-line Tools"
    toml="$ANDROID_DIR/gradle/libs.versions.toml"
    min="$(sed -n 's/^min-sdk = "\(.*\)"/\1/p' "$toml")"
    target="$(sed -n 's/^target-sdk = "\(.*\)"/\1/p' "$toml")"
    case "$(uname -m)" in arm64 | aarch64) abi=arm64-v8a ;; *) abi=x86_64 ;; esac
    image="$(pick_image "$min" "$target" "$abi")"
    [ -n "$image" ] || die "本机没装能建专用 AVD 的系统镜像（要 ${abi}、API ${min} 以上的 Google APIs 或默认镜像），在 SDK Manager 里装一份，比如 sdkmanager \"system-images;android-${target};google_apis;${abi}\""
    devices="$("$AVDMANAGER" list device -c 2>/dev/null || true)"
    for d in medium_phone pixel; do
        if grep -qx "$d" <<<"$devices"; then device="$d"; break; fi
    done
    [ -n "$device" ] || die "avdmanager list device 里没有 medium_phone 或 pixel 这两种机型"
    echo "建专用 AVD ${NAME}：${image}，机型 ${device}"
    # 系统镜像里没有 devices.xml 时它也往 stderr 报 Error，成功时不打印。
    out="$("$AVDMANAGER" create avd -n "$NAME" -k "$image" -d "$device" <<<no 2>&1)" ||
        die "avdmanager 没能建出 ${NAME}：$out"
}

# 给专用模拟器挑一个空闲的控制台端口（偶数，它和下一个端口都没人用），打印出来。
free_port() {
    local taken p
    taken="$(emulators any)"
    for ((p = FIRST_PORT; p <= LAST_PORT; p += 2)); do
        if grep -qx "emulator-$p" <<<"$taken"; then continue; fi
        if lsof -nP -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1; then continue; fi
        if lsof -nP -iTCP:"$((p + 1))" -sTCP:LISTEN >/dev/null 2>&1; then continue; fi
        echo "$p"
        return 0
    done
    die "模拟器的端口 ${FIRST_PORT}～${LAST_PORT} 都被占了"
}

# 清掉专用模拟器上 app 的数据（电脑列表、配对密钥），这一轮的 app 只认得沙盒宿主。
clear_app() {
    installed || return 0
    local out
    out="$(dev_sh pm clear "$PKG")"
    [ "$out" = Success ] || die "清不掉 ${PKG} 的数据：$out"
    echo "已清掉 ${PKG} 的数据"
}

cmd_boot() {
    command -v "$EMULATOR" >/dev/null || die "找不到 emulator，设好 ANDROID_HOME"
    local pid="" port start=$SECONDS deadline=$((SECONDS + BOOT_SECS))
    if find_serial; then
        pid="$(cat "$STATE/emulator.pid" 2>/dev/null || true)"
        if [ -z "$pid" ] || [ "$(cat "$STATE/emulator.serial" 2>/dev/null || true)" != "$SERIAL" ] ||
            ! is_dedicated_emulator "$pid"; then
            die "专用 AVD ${NAME} 已经开着（${SERIAL}），不是这个工作树的 android.sh boot 起的：别的工作树在用时等它 stop，没人用时关掉它再来"
        fi
        echo "接着用本脚本起的专用模拟器：${SERIAL}（pid ${pid}）"
    else
        ensure_avd
        port="$(free_port)"
        SERIAL="emulator-$port"
        mkdir -p "$STATE"
        # -no-snapshot：不读也不写快照，每次冷启动；app 和数据照样留在 userdata 里。
        local args=(-avd "$NAME" -port "$port" -no-audio -no-boot-anim -no-snapshot)
        [ "${WINDOW:-0}" = 1 ] || args+=(-no-window)
        nohup "$EMULATOR" "${args[@]}" </dev/null >"$STATE/emulator.log" 2>&1 &
        pid=$!
        echo "$pid" >"$STATE/emulator.pid"
        echo "$SERIAL" >"$STATE/emulator.serial"
        echo "起专用模拟器 ${NAME}：${SERIAL}（pid ${pid}）"
    fi
    until [ "$(dev_sh getprop sys.boot_completed 2>/dev/null)" = 1 ]; do
        kill -0 "$pid" 2>/dev/null || die "模拟器退出了，看 $STATE/emulator.log"
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${BOOT_SECS} 秒 ${SERIAL} 还没开机完成，看 $STATE/emulator.log"
        sleep 0.5
    done
    clear_app
    echo "模拟器就绪：${SERIAL}（API $(dev_sh getprop ro.build.version.sdk)），用了 $((SECONDS - start)) 秒"
}

cmd_install() {
    need_serial
    (cd "$ANDROID_DIR" && ANDROID_SERIAL="$SERIAL" ./gradlew :app:installDebug --console=plain -q)
    echo "已装到 ${SERIAL}：$(dev_sh dumpsys package "$PKG" | sed -n '/^ *lastUpdateTime=/{s///p;q;}')"
}

cmd_launch() {
    need_serial
    installed || die "${SERIAL} 上没装 ${PKG}，先 android.sh install"
    mkdir -p "$STATE"
    # logs 从第一次 launch 算起，一轮验证里重装、重启几次都算进去。
    [ -s "$STATE/launch.time" ] || dev_sh date +%s >"$STATE/launch.time"
    local out pid top deadline=$((SECONDS + READY_SECS))
    local demo=()
    if [ -n "${DEMO:-}" ]; then demo=(--es demo "$DEMO"); fi
    out="$(dev_sh am start -W -S -n "$ACTIVITY" "${demo[@]+"${demo[@]}"}")"
    grep -q '^Status: ok' <<<"$out" || die "am start 没成功：$out"
    while :; do
        pid="$(app_pid)"
        top="$(dev_sh dumpsys activity activities | grep -m 1 'topResumedActivity' || true)"
        if [ -n "$pid" ] && grep -qF "$ACTIVITY" <<<"$top"; then break; fi
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${READY_SECS} 秒 app 还没在前台（pid ${pid:-无}），跑 android.sh logs 看"
        sleep 0.2
    done
    echo "app 已启动：pid ${pid}，$(sed -n 's/^TotalTime: /冷启动 /p' <<<"$out") 毫秒"
}

listening() { lsof -nP -iTCP@127.0.0.1:"$REMOTE_PORT" -sTCP:LISTEN >/dev/null 2>&1; }

cmd_host() {
    local conf="$RUNODE_VERIFY_HOME/.runode/config.conf" configured=0
    if grep -q '^remote-access = true' "$conf" 2>/dev/null; then configured=1; fi
    if [ "$configured" = 0 ] && lsof -nP -iTCP:"$REMOTE_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
        die "端口 ${REMOTE_PORT} 已经有别的进程在听，换一个 REMOTE_PORT"
    fi
    "$SANDBOX_SH" host
    if [ "$configured" = 0 ]; then
        # 单独跑的宿主盯着配置文件，改了几秒内开监听。terminal-host 预先开着，pair 配好后不再问要不要后台跑。
        {
            echo "remote-access = true"
            echo "remote-access-address = 127.0.0.1"
            echo "remote-access-port = $REMOTE_PORT"
            echo "remote-access-name = $HOST_NAME"
            echo "terminal-host = true"
        } >>"$conf"
    fi
    local deadline=$((SECONDS + READY_SECS))
    until listening; do
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${READY_SECS} 秒 127.0.0.1:${REMOTE_PORT} 还没开监听，看 sandbox.sh logs"
        sleep 0.1
    done
    echo "远程访问就绪：宿主听 127.0.0.1:${REMOTE_PORT}，模拟器里经 ${HOST_ALIAS}:${REMOTE_PORT} 连，电脑名 ${HOST_NAME}"
    echo "沙盒目录（会话的工作目录放这下面）：${RUNODE_VERIFY_HOME}"
}

stop_pair() {
    [ -s "$STATE/pair.pid" ] || return 0
    local pid cmd
    pid="$(cat "$STATE/pair.pid")"
    cmd="$(ps -p "$pid" -o command= 2>/dev/null || true)"
    case "$cmd" in
        *"$SANDBOX_SH rn remote pair "*)
            # sandbox.sh 不 exec，runode 是它的子进程。
            pkill -TERM -P "$pid" 2>/dev/null || true
            kill -TERM "$pid" 2>/dev/null || true
            ;;
    esac
    rm -f "$STATE/pair.pid"
}

cmd_pair() {
    need_serial
    installed || die "${SERIAL} 上没装 ${PKG}，先 android.sh install"
    listening || die "沙盒宿主的远程访问没开，先 android.sh host"
    mkdir -p "$STATE"
    # 配对页一打开就要相机扫码，app 没有相机权限时系统的权限框会盖住确认页。专用模拟器的相机是假的，直接给。
    dev_sh pm grant "$PKG" android.permission.CAMERA >/dev/null
    stop_pair
    nohup "$SANDBOX_SH" rn remote pair --addr "$HOST_ALIAS" </dev/null >"$STATE/pair.out" 2>&1 &
    local pid=$! uri="" out deadline=$((SECONDS + READY_SECS))
    echo "$pid" >"$STATE/pair.pid"
    until uri="$(grep -m 1 '^runode://pair?' "$STATE/pair.out" || true)"; [ -n "$uri" ]; do
        kill -0 "$pid" 2>/dev/null || die "runode remote pair 退出了：$(cat "$STATE/pair.out")"
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${READY_SECS} 秒 runode remote pair 还没打印配对链接"
        sleep 0.1
    done
    # 链接里有 &，交给设备上的 shell 时要引起来。
    out="$(dev_sh am start -W -a android.intent.action.VIEW -d "'$uri'" "$PKG")"
    grep -q '^Status: ok' <<<"$out" || die "没能在 app 里打开配对链接：$out"
    # 确认页从底下滑上来，等界面树里有了电脑名再交出去，不然紧跟着的点按会落空。
    until on_screen "$HOST_NAME"; do
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${READY_SECS} 秒确认页还没出来，android.sh shot 截一张看看"
    done
    echo "配对链接已在 app 里打开，停在确认页：电脑名 ${HOST_NAME}，地址 ${HOST_ALIAS}:${REMOTE_PORT}"
    echo "在 app 里点「配对」（英文界面是 Pair），再跑 android.sh paired；链接几分钟后过期"
}

cmd_paired() {
    [ -s "$STATE/pair.out" ] || die "还没有 android.sh pair"
    local deadline=$((SECONDS + PAIR_SECS)) pid
    pid="$(cat "$STATE/pair.pid" 2>/dev/null || true)"
    until grep -q '^Paired with ' "$STATE/pair.out"; do
        if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
            # 进程打印完 Paired with 就退出，退出前的最后几行可能刚落盘。
            grep -q '^Paired with ' "$STATE/pair.out" || die "配对没成：$(tail -n 3 "$STATE/pair.out")"
            break
        fi
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${PAIR_SECS} 秒还没配好；app 里点了「配对」没有？截一张图看看停在哪"
        sleep 0.2
    done
    grep '^Paired with ' "$STATE/pair.out"
    "$SANDBOX_SH" rn remote devices
}

cmd_logs() {
    local bad=0 since uid out pid
    if [ -s "$STATE/launch.time" ] && find_serial; then
        since="$(cat "$STATE/launch.time").000"
        uid="$(dev_sh pm list packages -U "$PKG" | sed -n "s/^package:$PKG uid:\([0-9]*\).*/\1/p")"
        echo "== 崩溃（crash 缓冲区，launch 以来，所有 app）"
        out="$(dev_sh logcat -d -b crash -T "$since")"
        if [ -n "$out" ]; then echo "$out" | tail -n 60; else echo "（没有）"; fi
        if grep -qF "$PKG" <<<"$out"; then bad=1; fi
        echo "== ANR"
        out="$(dev_sh logcat -d -b events,system -T "$since" | grep -E "am_anr.*$PKG|ANR in $PKG" || true)"
        if [ -n "$out" ]; then echo "$out"; bad=1; else echo "（没有）"; fi
        echo "== app 的 E、F 行（uid ${uid}，末尾 40 行）"
        # 过滤式要引起来再交给设备上的 shell，免得 * 被展开。
        out="$(dev_sh logcat -d -T "$since" --uid="$uid" "'*:E'" | grep -v '^--------- beginning of' || true)"
        if [ -n "$out" ]; then echo "$out" | tail -n 40; else echo "（没有）"; fi
        echo "== app 的 W 行：$(dev_sh logcat -d -T "$since" --uid="$uid" "'*:W'" | grep -c ' W ' || true) 条"
        pid="$(app_pid)"
        if [ -n "$pid" ]; then echo "== app 进程：pid ${pid}"; else echo "== app 进程：不在"; fi
    else
        echo "== app：没有 launch 记录或专用模拟器没开，不看 logcat"
    fi
    if [ -d "$RUNODE_VERIFY_HOME" ]; then
        "$SANDBOX_SH" logs || bad=1
    fi
    return "$bad"
}

cmd_shot() {
    local file="${1:-}"
    [ -n "$file" ] || die "用法：android.sh shot FILE"
    need_serial
    adb_s exec-out screencap -p >"$file"
    [ -s "$file" ] || die "截图是空的"
    echo "$file"
}

cmd_lint_changed() {
    local reports oldest files f report found=0
    # 各模块一份报告，最旧的那份排在最后。
    reports="$(ls -t "$ANDROID_DIR"/*/build/reports/lint-results-debug.txt 2>/dev/null || true)"
    [ -n "$reports" ] || die "没有 lint 报告，先在 apps/android 里跑 ./gradlew lint（或 check）"
    oldest="$(echo "$reports" | tail -n 1)"
    # lint 看的只有模块里的代码、资源和构建脚本。
    files="$( (git -C "$ROOT" diff --name-only HEAD -- apps/android
        git -C "$ROOT" ls-files --others --exclude-standard -- apps/android) | sort -u |
        grep -E '^apps/android/[a-z]+/(src/.*\.(kt|java|xml)|build\.gradle\.kts)$' || true)"
    [ -n "$files" ] || { echo "apps/android 的模块里没有改动"; return 0; }
    for f in $files; do
        if [ -f "$ROOT/$f" ] && [ "$ROOT/$f" -nt "$oldest" ]; then
            echo "注意：$f 比 lint 报告新，报告可能没算上它，先重跑 lint"
        fi
        for report in $reports; do
            if grep -E "^$ROOT/$f(:[0-9]+)?: (Error|Warning|Hint): " "$report"; then found=1; fi
        done
    done
    [ "$found" = 0 ] && echo "改动的文件在 lint 报告里没有问题"
    return "$found"
}

cmd_status() {
    local avds pid
    avds="$("$EMULATOR" -list-avds 2>/dev/null || true)"
    if grep -qx "$NAME" <<<"$avds"; then echo "专用 AVD：${NAME}"; else echo "专用 AVD：${NAME}（还没建，boot 时建）"; fi
    if find_serial; then
        echo "专用模拟器：${SERIAL} 开着"
        if [ -s "$STATE/emulator.pid" ] && [ "$(cat "$STATE/emulator.serial" 2>/dev/null || true)" = "$SERIAL" ] &&
            is_dedicated_emulator "$(cat "$STATE/emulator.pid")"; then
            echo "本脚本起的模拟器：pid $(cat "$STATE/emulator.pid")"
        fi
        if installed; then
            pid="$(app_pid)"
            echo "app：已装，进程 ${pid:-不在}"
        else
            echo "app：没装"
        fi
    else
        echo "专用模拟器：没开"
    fi
    if [ -s "$STATE/pair.pid" ] && kill -0 "$(cat "$STATE/pair.pid")" 2>/dev/null; then echo "配对进程：在等手机"; fi
    if listening; then echo "远程访问：127.0.0.1:${REMOTE_PORT} 在听"; fi
    if [ -d "$RUNODE_VERIFY_HOME" ]; then "$SANDBOX_SH" status; else echo "沙盒：没有"; fi
}

stop_emulator() {
    [ -s "$STATE/emulator.pid" ] || return 0
    local pid serial i
    pid="$(cat "$STATE/emulator.pid")"
    serial="$(cat "$STATE/emulator.serial" 2>/dev/null || true)"
    if ! kill -0 "$pid" 2>/dev/null; then
        :
    elif ! is_dedicated_emulator "$pid"; then
        echo "pid ${pid} 已经不是专用 AVD 的模拟器，不动它" >&2
    else
        if [ -n "$serial" ] && [ "$(avd_name "$serial")" = "$NAME" ]; then
            try_bounded 10 "$ADB" -s "$serial" emu kill >/dev/null 2>&1 || kill -TERM "$pid"
        else
            kill -TERM "$pid"
        fi
        for ((i = 0; i < 300; i++)); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        if kill -0 "$pid" 2>/dev/null; then
            echo "模拟器 30 秒内没退出，强制结束" >&2
            kill -KILL "$pid" 2>/dev/null || true
        fi
        echo "已关模拟器 ${serial:-}（pid ${pid}）"
    fi
    rm -f "$STATE/emulator.pid" "$STATE/emulator.serial"
    [ "${KEEP:-0}" = 1 ] || rm -f "$STATE/emulator.log"
}

cmd_stop() {
    if [ -d "$STATE" ]; then stop_pair; fi
    if [ -d "$RUNODE_VERIFY_HOME" ]; then "$SANDBOX_SH" stop; fi
    if [ -d "$STATE" ]; then
        rm -f "$STATE/launch.time"
        [ "${KEEP:-0}" = 1 ] || rm -f "$STATE/pair.out"
        [ "${KEEP_EMULATOR:-0}" = 1 ] || stop_emulator
        rmdir "$STATE" 2>/dev/null || echo "留着 ${STATE}：$(cd "$STATE" && printf '%s ' *)"
    fi
}

case "${1:-}" in
    prereqs) cmd_prereqs ;;
    boot) cmd_boot ;;
    install) cmd_install ;;
    launch) cmd_launch ;;
    host) cmd_host ;;
    pair) cmd_pair ;;
    paired) cmd_paired ;;
    rn) shift; "$SANDBOX_SH" rn "$@" ;;
    logs) cmd_logs ;;
    shot) cmd_shot "${2:-}" ;;
    lint-changed) cmd_lint_changed ;;
    status) cmd_status ;;
    stop) cmd_stop ;;
    *) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
