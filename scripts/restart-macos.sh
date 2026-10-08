#!/bin/bash
# 重启装好的 Runode.app，让 app 和宿主都换成磁盘上现在这份可执行文件，会话不断：先退出 app，再用
# 新的可执行文件拉起一个宿主（runode --host --take-over）接手旧宿主的会话，最后重新打开 app。
#
# 为什么要自己交接：app 启动时只在宿主的构建号和自己的不同时才让新宿主接手，还要先等 app 退出；
# 这里一步做完。构建号是版本号加 git 提交号，工作区有没提交的改动时打包脚本再加上改动的摘要，
# 所以改了代码没提交、重新 make install 后也换得掉宿主。装的还是同一个构建时宿主不交接（回
# `Busy`），这时没有要换的，只重开 app。
#
# 用法：restart-macos.sh，一般经 make restart（装好新版本再重启用 make install restart）。
#
# 环境变量：
#   RUNODE_APP  要重启的 app，默认 /Applications/Runode.app
#
# 在 runode 自己的终端里跑时，退出 app 的那一刻这个终端的去留由宿主决定，所以脚本转到后台跑，
# 输出写进日志，重新打开 app 后去看。
set -euo pipefail

app=${RUNODE_APP:-/Applications/Runode.app}
bin="$app/Contents/MacOS/runode"
data="$HOME/.runode"
# 发布构建的宿主用 host.sock、host.log，调试构建的是 host-dev.*，见 runode_paths 的 HOST_NAME。
socket="$data/run/host.sock"
host_log="$data/cache/host.log"
log="$data/cache/restart.log"

if [[ ! -x "$bin" ]]; then
  echo "找不到 ${bin}，先 make install" >&2
  exit 1
fi

if [[ -n "${RUNODE_SESSION:-}" && -z "${RUNODE_RESTART_DETACHED:-}" ]]; then
  mkdir -p "$(dirname "$log")"
  RUNODE_RESTART_DETACHED=1 nohup "$0" "$@" >"$log" 2>&1 </dev/null &
  disown
  echo "在后台重启 Runode，过程写在 $log"
  exit 0
fi

# 等进程 $1 退出，最多 $2 秒；退出了返回 0。
wait_gone() {
  local pid=$1 seconds=$2
  for ((i = 0; i < seconds * 10; i++)); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 0.1
  done
  return 1
}

# 1. 退出 app。宿主单独跑时会话留在它那里；宿主跑在 app 里、配置要退出后保留会话时，app 退出前
#    把会话交给一个单独跑的宿主；都不是时会话跟着结束，和在菜单里退出一样。
bundle_id=$(defaults read "$app/Contents/Info" CFBundleIdentifier)
app_pid=$(pgrep -fx "$bin" | head -n 1 || true)
if [[ -n "$app_pid" ]]; then
  echo "退出 app（${app_pid}）"
  osascript -e "tell application id \"$bundle_id\" to quit"
  # 有 agent 在跑时 app 会先弹框确认，给用户留点时间。
  if ! wait_gone "$app_pid" 120; then
    echo "app 两分钟内没有退出（是不是在确认框上点了取消？），不重启了" >&2
    exit 1
  fi
fi

# 2. 让新的可执行文件接手旧宿主的会话。成了以后旧宿主自己退出，新宿主接着跑。
old_host=$(lsof -t "$socket" 2>/dev/null | head -n 1 || true)
if [[ -n "$old_host" ]]; then
  echo "让新宿主接手宿主 $old_host 的会话"
  nohup "$bin" --host --take-over >/dev/null 2>&1 </dev/null &
  new_host=$!
  disown
  # 等旧宿主交完退出；新宿主先退出了就是没接成，不用等满。
  for ((i = 0; i < 300; i++)); do
    kill -0 "$old_host" 2>/dev/null && kill -0 "$new_host" 2>/dev/null || break
    sleep 0.1
  done
  if ! kill -0 "$old_host" 2>/dev/null; then
    if ! kill -0 "$new_host" 2>/dev/null; then
      echo "新宿主接手后没在跑，看 $host_log" >&2
      exit 1
    fi
    echo "新宿主 $new_host 接手了会话"
  elif tail -n 20 "$host_log" | grep -q "host $new_host did not take over.*refused to hand over: Busy"; then
    # 同构建的宿主不交接：在跑的已经是磁盘上这份代码（没重新 make install，或者改动和上次装的一样）。
    echo "在跑的宿主和装好的是同一个构建，不用换，只重开 app"
  else
    echo "旧宿主 30 秒内没有交出会话，看 $host_log" >&2
    kill "$new_host" 2>/dev/null || true
    exit 1
  fi
else
  echo "没有在跑的宿主，app 打开时会自己拉起"
fi

# 3. 重新打开 app，它会连上这个构建的宿主。
open "$app"
echo "已重新打开 $app"
