#!/usr/bin/env bash
# 在隔离的家目录里起开发版 runode（宿主或整个 app），用命令行驱动它，用完关掉、删干净。
# 隔离靠换 HOME：runode 的配置、数据、socket 和日志都在 `$HOME/.runode` 下，Ghostty 的配置在
# `$HOME/.config` 下；再去掉宿主给终端设的 RUNODE_SOCKET、RUNODE_SESSION、RUNODE_BIN，命令行才不会
# 连到正在用的那个 runode。换了 HOME 后连配置、看板和窗口存档也是沙盒自己的一份。
#
# 用法：sandbox.sh host|app|rn ARGS...|smoke|status|logs|stop
#   host    只起宿主（`runode --host --stay`），给它一个空工作区，等到命令行连得上
#   app     起桌面 app（它自己拉起宿主），等到宿主连得上、窗口开出来；ONBOARDING=1 时第一次启动打开引导页；
#           接着副屏时窗口开在副屏上，不挡主屏
#   rn      在沙盒里跑一条 runode 命令行，比如 `sandbox.sh rn list --json`
#   smoke   用命令行走一遍主流程：开终端、打命令、等它跑完、读输出、看板加删任务、关终端
#   status  沙盒目录和里面在跑什么
#   logs    宿主和 app 的日志末尾，并列出 ERROR、WARN 和 panic；有 ERROR 或 panic 时退出码为 1
#   stop    关掉 app 和宿主（连同它们起的 shell 和子进程），删掉沙盒目录；KEEP=1 时留着目录看日志
#
# 宿主或 app 已经在跑时，host、app、smoke 先核对它是不是当前的 $RUNODE_VERIFY_BIN 起的、起了以后可执行文件
# 有没有重新编过；不是就报错，要先 stop 再起，免得新代码对着旧宿主验出假通过。
#
# 环境变量：
#   RUNODE_VERIFY_HOME  沙盒目录，默认 /tmp/runode-verify-<工作树路径的短哈希>，每个工作树各用一个；
#                       已有、又不是本脚本建的目录不用。socket 路径要放进 104 字节，别放到 $TMPDIR 下
#   RUNODE_VERIFY_BIN   要跑的 runode，默认当前工作树的 target/debug/runode（认 CARGO_TARGET_DIR）；也可以是
#                       `make app` 打出的 Runode.app 里的 Contents/MacOS/runode
#   READY_SECS          等就绪的上限，默认 20 秒
set -euo pipefail

ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
SANDBOX="${RUNODE_VERIFY_HOME:-/tmp/runode-verify-$(printf '%s' "$ROOT" | shasum | cut -c1-8)}"
# 沙盒目录里的标记文件：没有它的目录不当沙盒用，`stop` 也不删。
MARK=.runode-verify-sandbox
READY_SECS="${READY_SECS:-20}"
BIN="${RUNODE_VERIFY_BIN:-${CARGO_TARGET_DIR:-$ROOT/target}/debug/runode}"
# 写成绝对路径：起进程用的就是这个字符串，`ps -o comm=` 报回来的也是它，靠它认进程。
if [ -d "$(dirname "$BIN")" ]; then BIN="$(cd "$(dirname "$BIN")" && pwd -P)/$(basename "$BIN")"; fi
STATE="$SANDBOX/.verify"
# 起宿主或 app 时记下可执行文件的样子（修改时间、大小、inode），之后据此认出重新编过。
STAMP="$STATE/bin.stamp"
# 带 debug_assertions 编的 runode 的文件名带 -dev（`runode_paths` 的 HOST_NAME、HOST_LAYOUT_FILE、ONBOARDING_FILE），
# 按可执行文件在不在 debug 目录下认；macOS 上 runtime_dir 不按机器分层。
case "$BIN" in */debug/*) DEV=-dev ;; *) DEV= ;; esac
SOCK="$SANDBOX/.runode/run/host$DEV.sock"
LOCK="$SANDBOX/.runode/run/host$DEV.lock"
HOST_LOG="$SANDBOX/.runode/cache/host$DEV.log"
HOST_LAYOUT="$SANDBOX/.runode/run/host-layout$DEV.json"
ONBOARDING_FILE="$SANDBOX/.runode/onboarding$DEV"
WINDOWS_FILE="$SANDBOX/.runode/windows$DEV.json"
APP_LOG="$STATE/app.log"
# 沙盒的环境。写成数组、直接展开在命令前：放进函数再后台跑时 `$!` 是子 shell，不是 runode。
SANDBOX_ENV=(env -u RUNODE_SOCKET -u RUNODE_SESSION -u RUNODE_BIN -u XDG_CONFIG_HOME -u XDG_DATA_HOME
    -u ZDOTDIR HOME="$SANDBOX")

die() { echo "sandbox: $*" >&2; exit 1; }

# 去掉颜色的日志：app 往 stderr 打的日志带 ANSI 颜色，字段名和值之间也夹着。
plain_log() { sed $'s/\x1b\\[[0-9;]*m//g' "$1"; }

prepare() {
    [ -x "$BIN" ] || die "找不到 ${BIN}，先跑 cargo build -p runode"
    [ "${#SOCK}" -lt 104 ] || die "沙盒路径太长，socket 放不进 sun_path：$SOCK"
    if [ -e "$SANDBOX" ] && [ ! -e "$SANDBOX/$MARK" ]; then
        die "$SANDBOX 已经存在，又不是本脚本建的沙盒；换一个 RUNODE_VERIFY_HOME"
    fi
    mkdir -p "$STATE"
    # 空的 .zshrc：登录 shell 是 zsh 时不弹新用户向导。
    touch "$SANDBOX/$MARK" "$SANDBOX/.zshrc"
}

bin_stamp() { stat -f '%m:%z:%i' "$BIN"; }

# 进程的可执行文件（完整路径）。
exe_of() { ps -p "$1" -o comm= 2>/dev/null || true; }

# 拿着沙盒宿主锁文件的进程号，不管它是谁起的。
lock_holder() {
    [ -e "$LOCK" ] || return 0
    lsof -t -- "$LOCK" 2>/dev/null | head -n 1 || true
}

# 沙盒的宿主：拿着锁、跑的是 $BIN 的那个进程。
host_pid() {
    local pid
    pid="$(lock_holder)"
    if [ -n "$pid" ] && [ "$(exe_of "$pid")" = "$BIN" ]; then echo "$pid"; fi
}

# 沙盒的 app：记下的进程号还活着、跑的是 $BIN 时才算，进程号被别的程序复用了不算。
app_pid() {
    [ -s "$STATE/app.pid" ] || return 0
    local pid
    pid="$(cat "$STATE/app.pid")"
    if [ "$(exe_of "$pid")" = "$BIN" ]; then echo "$pid"; fi
}

# 在跑的宿主（和 app）是不是当前的 $BIN 起的、起了以后 $BIN 有没有重新编过；不对就报错。
check_fresh() {
    local holder
    holder="$(lock_holder)"
    if [ -n "$holder" ] && [ "$(exe_of "$holder")" != "$BIN" ]; then
        die "沙盒里的宿主（pid ${holder}）跑的是 $(exe_of "$holder")，不是 ${BIN}；先用起它的那个 RUNODE_VERIFY_BIN 跑 stop"
    fi
    if [ ! -s "$STAMP" ] || [ "$(cat "$STAMP")" != "$(bin_stamp)" ]; then
        die "沙盒里的宿主或 app 是 $BIN 重新编译之前起的；先 sandbox.sh stop 再起"
    fi
}

host_ready() {
    [ -S "$SOCK" ] && "${SANDBOX_ENV[@]}" "$BIN" list >/dev/null 2>&1
}

# 等宿主连得上，最多 READY_SECS 秒；给了进程号时它先退出就报错。
wait_host() {
    local tries=$((READY_SECS * 10)) i
    for ((i = 0; i < tries; i++)); do
        host_ready && return 0
        if [ -n "${1:-}" ] && ! kill -0 "$1" 2>/dev/null; then
            die "进程 $1 在就绪前退出了，看 sandbox.sh logs"
        fi
        sleep 0.1
    done
    die "等了 ${READY_SECS} 秒宿主还连不上（${SOCK}），看 sandbox.sh logs"
}

cmd_host() {
    prepare
    if host_ready; then
        check_fresh
        echo "宿主已在跑：pid $(host_pid)"
        return
    fi
    # 没有界面时命令行的 `open` 要开在已有的工作区里，宿主启动时从这份布局读回一个。
    if [ ! -e "$HOST_LAYOUT" ]; then
        mkdir -p "$(dirname "$HOST_LAYOUT")"
        printf '[{"index":1,"front":true,"workspaces":[{"index":1,"dir":"%s","active":true,"tabs":[]}]}]\n' \
            "$SANDBOX" >"$HOST_LAYOUT"
    fi
    bin_stamp >"$STAMP"
    # --stay：命令之间隔得久也不因空闲退出，由 stop 关掉。
    "${SANDBOX_ENV[@]}" nohup "$BIN" --host --stay </dev/null >"$STATE/host.out" 2>&1 &
    wait_host "$!"
    echo "宿主就绪：pid $(host_pid)，socket $SOCK"
}

# 第一块不是主屏的屏幕：「UUID 宽 高」（宽高按点，UUID 小写），只有一块屏时什么也不打印。
secondary_display() {
    osascript -l JavaScript -e '
ObjC.import("AppKit");
ObjC.bindFunction("CGMainDisplayID", ["unsigned int", []]);
ObjC.bindFunction("CGDisplayCreateUUIDFromDisplayID", ["void *", ["unsigned int"]]);
ObjC.bindFunction("CFUUIDCreateString", ["id", ["void *", "void *"]]);
const screens = $.NSScreen.screens;
let found = "";
for (let i = 0; i < screens.count && !found; i++) {
    const screen = screens.objectAtIndex(i);
    const id = screen.deviceDescription.objectForKey("NSScreenNumber").unsignedIntValue;
    if (id === $.CGMainDisplayID()) continue;
    const uuid = ObjC.unwrap($.CFUUIDCreateString(null, $.CGDisplayCreateUUIDFromDisplayID(id)));
    found = [uuid.toLowerCase(), screen.frame.size.width, screen.frame.size.height].join(" ");
}
found' 2>/dev/null || true
}

# 还没有窗口存档、接着副屏时，写一份把窗口放在副屏正中的存档。窗口里要有一个 workspace，
# 不然 app 不恢复这个窗口。
place_on_secondary() {
    [ -e "$WINDOWS_FILE" ] && return
    local uuid width height
    read -r uuid width height <<<"$(secondary_display)" || true
    [ -n "${uuid:-}" ] || return 0
    local w=960 h=620
    mkdir -p "$(dirname "$WINDOWS_FILE")"
    cat >"$WINDOWS_FILE" <<EOF
{"version": 1, "windows": [{
  "bounds": {"display": "$uuid", "mode": "windowed", "x": $(((${width%.*} - w) / 2)), "y": $(((${height%.*} - h) / 2)), "width": $w, "height": $h},
  "workspaces": [{"name": "~", "dir": "$SANDBOX", "active": 0,
    "tabs": [{"root": {"type": "leaf", "cwd": null}, "focused": 0, "zoomed": false}]}],
  "active": 0, "preview_wrap": false, "preview_source": false, "show_ignored": false,
  "hide_dotfiles": false, "git_tree": false, "git_hide_subs": false, "git_hide_worktrees": false,
  "file_search_tree": false, "file_search_options": {}, "git_graph_collapsed": false,
  "github_actions_heights": [null, null, null]
}]}
EOF
}

cmd_app() {
    prepare
    local pid
    pid="$(app_pid)"
    if [ -n "$pid" ]; then
        check_fresh
        echo "app 已在跑：pid $pid"
        return
    fi
    # app 会连上已经在跑的宿主（比如先跑过 host），那个宿主也要是当前的可执行文件起的。
    if host_ready; then check_fresh; else bin_stamp >"$STAMP"; fi
    # 全新的数据目录第一次启动会打开引导页；不验引导页时先记成走完（`onboarding` 的 Stage::Done）。
    if [ "${ONBOARDING:-0}" != 1 ] && [ ! -e "$ONBOARDING_FILE" ]; then
        mkdir -p "$(dirname "$ONBOARDING_FILE")"
        echo "done" >"$ONBOARDING_FILE"
    fi
    place_on_secondary
    # runode::startup 的计时点里有 open_window：窗口开出来、画过第一帧以后才打。
    "${SANDBOX_ENV[@]}" RUST_LOG="${RUST_LOG:-info,runode::startup=info}" nohup "$BIN" </dev/null >"$APP_LOG" 2>&1 &
    pid=$!
    echo "$pid" >"$STATE/app.pid"
    wait_host "$pid"
    local tries=$((READY_SECS * 10)) i
    for ((i = 0; i < tries; i++)); do
        plain_log "$APP_LOG" | grep -q 'node="open_window"' && break
        kill -0 "$pid" 2>/dev/null || die "app 在开出窗口前退出了，看 sandbox.sh logs"
        sleep 0.1
    done
    [ "$i" -lt "$tries" ] || die "等了 ${READY_SECS} 秒 app 还没开出窗口，看 sandbox.sh logs"
    [ ! -e "$WINDOWS_FILE.bad" ] || echo "sandbox: 窗口存档的格式对不上，窗口开在了主屏；照 window::persist::format 改 place_on_secondary" >&2
    echo "app 就绪：pid ${pid}，宿主 pid $(host_pid)"
    echo "窗口归进程 $pid 所有（可执行文件 ${BIN}，不是 /Applications/Runode.app），界面操作按这个 pid 找窗口"
}

cmd_rn() {
    "${SANDBOX_ENV[@]}" "$BIN" "$@"
}

# smoke 的一步：跑一条命令行，stdout 照常输出；退出码不是 0 时报出是哪一步、退出码和它的 stderr。
step() {
    local what="$1" rc=0
    shift
    cmd_rn "$@" 2>"$STATE/step.err" || rc=$?
    [ "$rc" = 0 ] || die "${what}失败（退出码 ${rc}）：$(cat "$STATE/step.err")"
}

# 列表里有没有这个 id；JSON 里的字段名不管，只认 id 出现过。
listed() { grep -q "\"$2\"" <<<"$1"; }

cmd_smoke() {
    host_ready || die "宿主没在跑，先 sandbox.sh host 或 sandbox.sh app"
    check_fresh
    local marker="verify-$$-$RANDOM" id out task rc list
    out="$(step version version)" || exit 1
    echo "version: $out"

    id="$(step open open --cwd "$SANDBOX")" || exit 1
    [ -n "$id" ] || die "open 没打印会话号"
    echo "open: $id"
    list="$(step "list --json" list --json)" || exit 1
    listed "$list" "$id" || die "list --json 里没有 $id"
    echo "list: 有 $id"

    # shell 的集成脚本报到之前 `send --wait` 认不出命令，先等新 shell 的提示符停下来。
    step "等新 shell 的提示符（wait --for quiet）" wait "$id" --for quiet 1 --timeout 15 >/dev/null || exit 1
    out="$(step "send --wait（echo）" send "$id" "echo $marker" --enter --wait --timeout 15)" || exit 1
    [ "$out" = "exit 0" ] || die "send --wait 打印的是：${out}（应为 exit 0）；stderr：$(cat "$STATE/step.err")"
    echo "send --wait: $out"
    out="$(step "read --command" read "$id" --command)" || exit 1
    grep -qx "$marker" <<<"$out" || die "read --command 里没有 ${marker}，读到的是：$out"
    echo "read --command: 读到 $marker"
    rc=0
    out="$(cmd_rn send "$id" false --enter --wait --timeout 15 2>"$STATE/step.err")" || rc=$?
    if [ "$rc" != 4 ] || [ "$out" != "exit 1" ]; then
        die "失败的命令：退出码 ${rc}、打印 ${out}（应为 4、exit 1）；stderr：$(cat "$STATE/step.err")"
    fi
    echo "send --wait（失败的命令）: 退出码 4，$out"

    task="$(step "todo add" todo add "verify $marker" --no-worktree --cwd "$SANDBOX")" || exit 1
    [ -n "$task" ] || die "todo add 没打印任务号"
    list="$(step "todo list --json" todo list --json)" || exit 1
    listed "$list" "$task" || die "todo list --json 里没有 $task"
    step "todo rm" todo rm "$task" >/dev/null || exit 1
    list="$(step "todo list --json" todo list --json)" || exit 1
    if listed "$list" "$task"; then die "todo rm 之后还有 $task"; fi
    echo "todo: 加了又删了 $task"

    step kill kill "$id" || exit 1
    list="$(step "list --json" list --json)" || exit 1
    if listed "$list" "$id"; then die "kill 之后 list 里还有 $id"; fi
    echo "kill: $id 已关"
    echo "smoke 通过"
}

cmd_status() {
    echo "沙盒：$SANDBOX"
    echo "可执行文件：$BIN"
    local pid holder
    pid="$(host_pid)"
    holder="$(lock_holder)"
    if [ -n "$pid" ]; then
        echo "宿主：pid $pid"
    elif [ -n "$holder" ]; then
        echo "宿主：pid ${holder}，跑的是 $(exe_of "$holder")，不是上面的可执行文件"
    else
        echo "宿主：没在跑"
    fi
    pid="$(app_pid)"
    if [ -n "$pid" ]; then echo "app：pid $pid"; else echo "app：没在跑"; fi
}

cmd_logs() {
    local file found=0 plain
    for file in "$HOST_LOG" "$APP_LOG" "$STATE/host.out"; do
        [ -s "$file" ] || continue
        plain="$(plain_log "$file")"
        echo "== ${file}（末尾 20 行）"
        tail -n 20 <<<"$plain"
        echo "== ${file} 里的 ERROR、WARN、panic"
        grep -E ' (ERROR|WARN) |panicked' <<<"$plain" || echo "（没有）"
        if grep -qE ' ERROR |panicked' <<<"$plain"; then found=1; fi
    done
    return "$found"
}

# 给定进程的所有后代，一行一个。
descendants() {
    local table queue=("$@") parent child
    table="$(ps -axo pid=,ppid=)"
    while [ "${#queue[@]}" -gt 0 ]; do
        parent="${queue[0]}"
        queue=("${queue[@]:1}")
        while read -r child; do
            echo "$child"
            queue+=("$child")
        done < <(awk -v p="$parent" '$2 == p { print $1 }' <<<"$table")
    done
}

# 等进程退出，最多 $2 秒；退出了返回 0。
wait_gone() {
    local tries=$(($2 * 10)) i
    for ((i = 0; i < tries; i++)); do
        kill -0 "$1" 2>/dev/null || return 0
        sleep 0.1
    done
    return 1
}

stop_pid() {
    local pid="$1" what="$2"
    kill -0 "$pid" 2>/dev/null || return 0
    kill -TERM "$pid" 2>/dev/null || true
    if ! wait_gone "$pid" 10; then
        echo "${what}（pid ${pid}）10 秒内没退出，强制结束" >&2
        kill -KILL "$pid" 2>/dev/null || true
        wait_gone "$pid" 5 || die "${what}（pid ${pid}）结束不了"
    fi
    echo "已关 ${what}（pid ${pid}）"
}

cmd_stop() {
    [ -d "$SANDBOX" ] || { echo "沙盒不在：$SANDBOX"; return; }
    [ -e "$SANDBOX/$MARK" ] || die "$SANDBOX 不是本脚本建的沙盒，不碰它"
    local app host holder kids pid left
    holder="$(lock_holder)"
    host="$(host_pid)"
    if [ -n "$holder" ] && [ -z "$host" ]; then
        die "拿着沙盒宿主锁的 pid $holder 跑的是 $(exe_of "$holder")，不是 ${BIN}；用起它的那个 RUNODE_VERIFY_BIN 再跑 stop"
    fi
    app="$(app_pid)"
    # 先记下后代：宿主一退出，它起的 shell 就过继给 launchd，再也按父进程认不出来。
    # shellcheck disable=SC2086 # 进程号为空时不传参数
    kids="$(descendants $app $host)"
    # 后代的 pid 连同启动时间一起记下：宿主先退出时它们可能已经没了，pid 被复用就对不上，跳过不杀。
    local started=() i=0
    for pid in $kids; do started+=("$(ps -o lstart= -p "$pid" 2>/dev/null || true)"); done
    [ -z "$app" ] || stop_pid "$app" app
    rm -f "$STATE/app.pid"
    # 宿主不跟着 app 退出，收到 SIGTERM 时结束所有会话再退出。
    [ -z "$host" ] || stop_pid "$host" 宿主
    for pid in $kids; do
        if [ -n "${started[$i]}" ] && [ "$(ps -o lstart= -p "$pid" 2>/dev/null || true)" = "${started[$i]}" ]; then
            stop_pid "$pid" "宿主或 app 起的进程"
        fi
        i=$((i + 1))
    done
    # 会话里 double-fork 出去的守护进程（gpg-agent 等）已过继给 launchd，按环境里的 HOME=$SANDBOX 认；
    # 系统自带的程序（/usr/bin 等）ps -E 看不到环境，漏掉的由下面的 lsof 列出。
    for pid in $(ps -Eww -o pid=,command= -U "$(id -u)" 2>/dev/null | H="HOME=$SANDBOX" awk '{for (i = 2; i <= NF; i++) if ($i == ENVIRON["H"]) { print $1; break }}'); do
        [ "$pid" = "$$" ] || stop_pid "$pid" "沙盒会话留下的后台进程"
    done
    # 别的还开着沙盒里文件的进程（比如 cd 进来看日志的 shell）不是这里起的，只报不关。
    left="$(lsof -t +D "$SANDBOX" 2>/dev/null | sort -u || true)"
    for pid in $left; do
        echo "pid ${pid}（$(exe_of "$pid")）还开着沙盒里的文件，不是本脚本起的，没动它" >&2
    done
    if [ "${KEEP:-0}" = 1 ]; then
        echo "留着沙盒目录：$SANDBOX"
    else
        rm -rf "$SANDBOX"
        echo "已删沙盒目录：$SANDBOX"
    fi
}

case "${1:-}" in
    host) cmd_host ;;
    app) cmd_app ;;
    rn) shift; cmd_rn "$@" ;;
    smoke) cmd_smoke ;;
    status) cmd_status ;;
    logs) cmd_logs ;;
    stop) cmd_stop ;;
    *) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
