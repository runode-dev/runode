#!/usr/bin/env bash
# 把 Android app 的调试构建装到模拟器上跑起来，连上沙盒里的宿主，查日志，用完收干净。
# 宿主一律经仓库根目录 verify skill 的 sandbox.sh 起（换掉 HOME，不碰正在用的那个 runode），沙盒目录和
# 桌面验证用的分开。只认模拟器（emulator-*），不碰连着的真机。
#
# 用法：android.sh prereqs|boot|install|launch|host|pair|paired|rn ARGS...|logs|shot FILE|lint-changed|status|stop
#   prereqs       查 JDK、SDK、NDK、CMake 和三份原生库（libghostty-vt、librunode_tunnel.so、sherpa-onnx）在不在、
#                 要不要重编；缺了或过期时退出码为 1，并打印该跑的 make 命令
#   boot          没有开着的模拟器时起一台（默认不开窗口、不读写快照），等到开机完成
#   install       ./gradlew :app:installDebug 装到这台模拟器上，保留 app 原有的数据
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
#   status        模拟器、app、宿主、配对进程的状态
#   stop          结束配对进程，把 app 的电脑列表还原成配对前的样子，关宿主、删沙盒，关掉本脚本起的模拟器；
#                 KEEP=1 留着沙盒目录，KEEP_EMULATOR=1 留着模拟器
#
# 环境变量：
#   ANDROID_SERIAL      用哪台模拟器，例如 emulator-5554；只开着一台时不用给
#   AVD                 boot 起哪台 AVD，默认 emulator -list-avds 列出的第一台
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
PKG=dev.runode.mobile
ACTIVITY="$PKG/.MainActivity"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
ADB=adb
if [ -n "$SDK" ] && [ -x "$SDK/platform-tools/adb" ]; then ADB="$SDK/platform-tools/adb"; fi
EMULATOR=emulator
if [ -n "$SDK" ] && [ -x "$SDK/emulator/emulator" ]; then EMULATOR="$SDK/emulator/emulator"; fi
REMOTE_PORT="${REMOTE_PORT:-7877}"
BOOT_SECS="${BOOT_SECS:-180}"
READY_SECS="${READY_SECS:-20}"
PAIR_SECS="${PAIR_SECS:-60}"
# Android 模拟器的网络里，这个地址就是这台电脑的 127.0.0.1。
HOST_ALIAS=10.0.2.2
# 沙盒宿主报给手机的电脑名，在 app 里和用户自己的电脑分得开。
HOST_NAME=runode-verify
# app 的电脑列表（`FileMachineStore.defaultFile`），相对 app 的数据目录。
MACHINES=no_backup/machines.json
SERIAL=""

die() { echo "android: $*" >&2; exit 1; }

mtime() { stat -c %Y "$1" 2>/dev/null || stat -f %m "$1"; }

# 连着的、已就绪的模拟器序列号，一行一个。
emulators() {
    "$ADB" devices 2>/dev/null | awk '$1 ~ /^emulator-[0-9]+$/ && $2 == "device" { print $1 }'
}

# 定下这次用哪台模拟器，放进 SERIAL。
need_serial() {
    [ -z "$SERIAL" ] || return 0
    if [ -n "${ANDROID_SERIAL:-}" ]; then
        case "$ANDROID_SERIAL" in
            emulator-*) SERIAL="$ANDROID_SERIAL" ;;
            *) die "ANDROID_SERIAL=${ANDROID_SERIAL} 不是模拟器；这个脚本只用模拟器" ;;
        esac
        return 0
    fi
    local list count
    list="$(emulators)"
    count="$(printf '%s' "$list" | grep -c . || true)"
    case "$count" in
        0) die "没有开着的模拟器，先跑 android.sh boot" ;;
        1) SERIAL="$list" ;;
        *) die "开着好几台模拟器（$(echo $list)），用 ANDROID_SERIAL 指定一台" ;;
    esac
}

adb_s() { "$ADB" -s "$SERIAL" "$@"; }

# 设备上的一条 shell 命令，去掉行尾的 \r。
dev_sh() { adb_s shell "$@" | tr -d '\r'; }

installed() { dev_sh pm path "$PKG" | grep -q '^package:'; }

app_pid() { dev_sh pidof "$PKG" || true; }

# 屏幕的界面树里有没有正好是这段文字的节点。dump 一次要一两秒。
on_screen() { adb_s exec-out uiautomator dump /dev/tty 2>/dev/null | grep -qF "text=\"$1\""; }

cmd_prereqs() {
    local bad=0 v ndk cmake triple lib so dir ghostty_head newer dirty speech_version
    v="$(java -version 2>&1 | sed -n '1s/.*version "\([0-9]*\).*/\1/p')"
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
    ghostty_head="$(git -C "$ROOT/vendor/ghostty" rev-parse --absolute-git-dir 2>/dev/null)/HEAD"
    for triple in aarch64-linux-android x86_64-linux-android; do
        dir="$ANDROID_DIR/build/ghostty-vt/$triple"
        lib="$dir/lib/libghostty-vt.a"
        if [ ! -f "$lib" ]; then
            echo "缺    ${lib}：make android-vt"; bad=1; continue
        fi
        if [ -f "$ghostty_head" ] && [ "$(mtime "$ghostty_head")" -gt "$(mtime "$dir")" ]; then
            echo "过期  libghostty-vt（${triple}）：vendor/ghostty 在编完之后换过提交，make android-vt"; bad=1; continue
        fi
        dirty="$(git -C "$ROOT/vendor/ghostty" status --porcelain --untracked-files=no | cut -c4- |
            while read -r f; do [ "$ROOT/vendor/ghostty/$f" -nt "$dir" ] && echo "$f"; done)"
        if [ -n "$dirty" ]; then
            echo "过期  libghostty-vt（${triple}）：vendor/ghostty 里这些改动比它新，make android-vt：$(echo $dirty)"; bad=1; continue
        fi
        echo "ok    libghostty-vt（${triple}）"
    done

    # 拨号库：apps/android/tunnel 下有比它新的文件就要重编。
    for abi in arm64-v8a x86_64; do
        so="$ANDROID_DIR/connection/src/main/jniLibs/$abi/librunode_tunnel.so"
        if [ ! -f "$so" ]; then
            echo "缺    ${so}：make android-tunnel"; bad=1; continue
        fi
        newer="$(find "$ANDROID_DIR/tunnel" -type f -not -path '*/target/*' -newer "$so" | sed "s|^$ROOT/||")"
        if [ -n "$newer" ]; then
            echo "过期  librunode_tunnel.so（${abi}）：这些文件比它新，make android-tunnel：$(echo $newer)"; bad=1; continue
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

cmd_boot() {
    local list
    list="$(emulators)"
    if [ -n "${ANDROID_SERIAL:-}" ] || [ -n "$list" ]; then
        need_serial
        [ "$(dev_sh getprop sys.boot_completed)" = 1 ] || die "${SERIAL} 还没开机完成"
        echo "用开着的模拟器：${SERIAL}（$(dev_sh getprop ro.build.version.release)，API $(dev_sh getprop ro.build.version.sdk)）"
        return
    fi
    command -v "$EMULATOR" >/dev/null || die "找不到 emulator，设好 ANDROID_HOME"
    local avd="${AVD:-}"
    [ -n "$avd" ] || avd="$("$EMULATOR" -list-avds 2>/dev/null | grep -v '^INFO' | head -n 1)"
    [ -n "$avd" ] || die "没有 AVD，先在 Android Studio 的 Device Manager 里建一台（API 31 以上）"
    mkdir -p "$STATE"
    # -no-snapshot：不读也不写快照，不动这台 AVD 平时的快速启动存档；app 和数据照样留在 userdata 里。
    local args=(-avd "$avd" -no-audio -no-boot-anim -no-snapshot)
    [ "${WINDOW:-0}" = 1 ] || args+=(-no-window)
    nohup "$EMULATOR" "${args[@]}" </dev/null >"$STATE/emulator.log" 2>&1 &
    local pid=$! deadline=$((SECONDS + BOOT_SECS))
    echo "$pid" >"$STATE/emulator.pid"
    echo "起模拟器 ${avd}（pid ${pid}）"
    while [ -z "$SERIAL" ]; do
        kill -0 "$pid" 2>/dev/null || die "模拟器退出了，看 $STATE/emulator.log"
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${BOOT_SECS} 秒 adb 还没看到模拟器，看 $STATE/emulator.log"
        SERIAL="$(emulators | head -n 1)"
        [ -n "$SERIAL" ] || sleep 0.5
    done
    echo "$SERIAL" >"$STATE/emulator.serial"
    until [ "$(dev_sh getprop sys.boot_completed 2>/dev/null)" = 1 ]; do
        kill -0 "$pid" 2>/dev/null || die "模拟器退出了，看 $STATE/emulator.log"
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${BOOT_SECS} 秒 ${SERIAL} 还没开机完成，看 $STATE/emulator.log"
        sleep 0.5
    done
    echo "模拟器就绪：${SERIAL}（API $(dev_sh getprop ro.build.version.sdk)），用了 $((SECONDS + BOOT_SECS - deadline)) 秒"
}

cmd_install() {
    need_serial
    (cd "$ANDROID_DIR" && ANDROID_SERIAL="$SERIAL" ./gradlew :app:installDebug --console=plain -q)
    echo "已装到 ${SERIAL}：$(dev_sh dumpsys package "$PKG" | sed -n 's/^ *lastUpdateTime=//p' | head -n 1)"
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
    local pid
    pid="$(cat "$STATE/pair.pid")"
    # sandbox.sh 不 exec，runode 是它的子进程。
    pkill -TERM -P "$pid" 2>/dev/null || true
    kill -TERM "$pid" 2>/dev/null || true
    rm -f "$STATE/pair.pid"
}

cmd_pair() {
    need_serial
    installed || die "${SERIAL} 上没装 ${PKG}，先 android.sh install"
    listening || die "沙盒宿主的远程访问没开，先 android.sh host"
    mkdir -p "$STATE"
    # 第一次配对前把 app 的电脑列表存一份，stop 时还原：下一轮验证的配对页和电脑列表不带这一轮的沙盒。
    if [ ! -s "$STATE/machines.saved" ]; then
        dev_sh "run-as $PKG sh -c 'if [ -f $MACHINES ]; then cp $MACHINES $MACHINES.verify && echo saved; else echo absent; fi'" \
            >"$STATE/machines.saved"
        grep -qxE 'saved|absent' "$STATE/machines.saved" ||
            die "存不下 app 的电脑列表（run-as 只对调试构建有用）：$(cat "$STATE/machines.saved")"
    fi
    stop_pair
    nohup "$SANDBOX_SH" rn remote pair --addr "$HOST_ALIAS" </dev/null >"$STATE/pair.out" 2>&1 &
    local pid=$! uri="" deadline=$((SECONDS + READY_SECS))
    echo "$pid" >"$STATE/pair.pid"
    until uri="$(grep -m 1 '^runode://pair?' "$STATE/pair.out" || true)"; [ -n "$uri" ]; do
        kill -0 "$pid" 2>/dev/null || die "runode remote pair 退出了：$(cat "$STATE/pair.out")"
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${READY_SECS} 秒 runode remote pair 还没打印配对链接"
        sleep 0.1
    done
    # 链接里有 &，交给设备上的 shell 时要引起来。
    dev_sh am start -W -a android.intent.action.VIEW -d "'$uri'" "$PKG" | grep -q '^Status: ok' ||
        die "没能在 app 里打开配对链接"
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
            die "配对没成：$(tail -n 3 "$STATE/pair.out")"
        fi
        [ "$SECONDS" -lt "$deadline" ] || die "等了 ${PAIR_SECS} 秒还没配好；app 里点了「配对」没有？截一张图看看停在哪"
        sleep 0.2
    done
    grep '^Paired with ' "$STATE/pair.out"
    "$SANDBOX_SH" rn remote devices
}

cmd_logs() {
    local bad=0 since uid out pid
    if [ -s "$STATE/launch.time" ] && need_serial; then
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
        echo "== app：没有 launch 记录或模拟器没开，不看 logcat"
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
    local reports oldest files f found=0
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
        if grep -hE "^$ROOT/$f(:[0-9]+)?: (Error|Warning|Hint): " $reports; then found=1; fi
    done
    [ "$found" = 0 ] && echo "改动的文件在 lint 报告里没有问题"
    return "$found"
}

cmd_status() {
    local list pid
    list="$(emulators)"
    echo "模拟器：$(echo ${list:-没开})"
    if [ -s "$STATE/emulator.pid" ]; then
        pid="$(cat "$STATE/emulator.pid")"
        if kill -0 "$pid" 2>/dev/null; then echo "本脚本起的模拟器：pid ${pid}（$(cat "$STATE/emulator.serial" 2>/dev/null)）"; fi
    fi
    if [ -n "$list" ] && need_serial; then
        if installed; then
            pid="$(app_pid)"
            echo "app：已装，进程 ${pid:-不在}"
        else
            echo "app：没装"
        fi
    fi
    if [ -s "$STATE/pair.pid" ] && kill -0 "$(cat "$STATE/pair.pid")" 2>/dev/null; then echo "配对进程：在等手机"; fi
    if listening; then echo "远程访问：127.0.0.1:${REMOTE_PORT} 在听"; fi
    if [ -d "$RUNODE_VERIFY_HOME" ]; then "$SANDBOX_SH" status; else echo "沙盒：没有"; fi
}

restore_machines() {
    [ -s "$STATE/machines.saved" ] || return 0
    local list
    list="$(emulators)"
    if [ -z "$list" ] || ! need_serial; then
        echo "模拟器没开，app 的电脑列表没还原" >&2
        return 0
    fi
    dev_sh am force-stop "$PKG"
    if [ "$(cat "$STATE/machines.saved")" = saved ]; then
        dev_sh "run-as $PKG mv -f $MACHINES.verify $MACHINES"
    else
        dev_sh "run-as $PKG rm -f $MACHINES"
    fi
    rm -f "$STATE/machines.saved"
    echo "已还原 app 的电脑列表"
}

stop_emulator() {
    [ -s "$STATE/emulator.pid" ] || return 0
    local pid serial i
    pid="$(cat "$STATE/emulator.pid")"
    serial="$(cat "$STATE/emulator.serial" 2>/dev/null || true)"
    if kill -0 "$pid" 2>/dev/null; then
        if [ -n "$serial" ]; then "$ADB" -s "$serial" emu kill >/dev/null 2>&1 || true; else kill -TERM "$pid"; fi
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
    rm -f "$STATE/emulator.pid" "$STATE/emulator.serial" "$STATE/emulator.log"
}

cmd_stop() {
    if [ -d "$STATE" ]; then
        stop_pair
        restore_machines
    fi
    if [ -d "$RUNODE_VERIFY_HOME" ]; then "$SANDBOX_SH" stop; fi
    if [ -d "$STATE" ]; then
        rm -f "$STATE/launch.time"
        [ "${KEEP:-0}" = 1 ] || rm -f "$STATE/pair.out"
        [ "${KEEP_EMULATOR:-0}" = 1 ] || stop_emulator
        rmdir "$STATE" 2>/dev/null || echo "留着 ${STATE}：$(ls "$STATE" | tr '\n' ' ')"
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
