#!/bin/bash
# 重启装好的 Runode.app，让 app 和宿主都换成磁盘上现在这份可执行文件，会话不断：先退出 app，再用
# 新的可执行文件拉起一个宿主（runode --host --take-over）接手旧宿主的会话，最后重新打开 app。
#
# 为什么要自己交接：app 启动时只在宿主的构建号和自己的不同时才让新宿主接手，还要先等 app 退出；
# 这里一步做完。构建号是版本号加 git 提交号，工作区有没提交的改动时打包脚本再加上改动的摘要，
# 所以改了代码没提交、重新 make install 后也换得掉宿主。装的还是同一个构建时宿主不交接（回
# `Busy`），这时没有要换的，只重开 app。
#
# 会话要留得下，配置里得开着 terminal-host（退出后保留会话）：这里退出 app 用的是 AppleScript 的 quit，
# app 当成从 Dock 退出处理，不弹确认框、只按配置办，没开时会话跟着结束。所以 app 开着、配置里又没开时
# 不重启，先说清楚。
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
config="$data/config.conf"
# 新宿主和 app 只带 launchd 平时给 app 的这几个变量起，和在程序坞里打开时一样：宿主开的终端都继承它的
# 环境，带上调用这个脚本的终端里的变量（CLAUDECODE、CLAUDE_CODE_*、RUNODE_SESSION 这类），终端里的
# 程序会以为自己跑在别的程序里面。`open` 也会把调用方的环境传给 app。SHELL 取账户记录里的登录 shell：
# 新宿主经 `env -i` 起，沿用这里给的 SHELL，而调用方的可能是 chsh 之前的旧值。
login_shell=$(id -P 2>/dev/null | awk -F: '{print $NF}' || true)
launch_env=(/usr/bin/env -i HOME="$HOME" USER="$USER" LOGNAME="${LOGNAME:-$USER}" SHELL="${login_shell:-${SHELL:-/bin/zsh}}"
  TMPDIR="${TMPDIR:-/tmp}" PATH=/usr/bin:/bin:/usr/sbin:/sbin)
for name in LANG SSH_AUTH_SOCK __CF_USER_TEXT_ENCODING; do
  if [[ -n "${!name:-}" ]]; then launch_env+=("$name=${!name}"); fi
done

if [[ ! -x "$bin" ]]; then
  echo "找不到 ${bin}，先 make install" >&2
  exit 1
fi

# 配置文件里 terminal-host 最后算下来是不是 true，见开头。照 app 读配置的规矩（`runode_config` 的
# `edit::active` 和 `Config::apply`）：只写键不写值（`terminal-host`）是恢复默认的 false；引号要两头
# 都有才去掉；值不是 true、false 的那行 app 不认，沿用之前的值。config-file 引进来的文件排在这个文件
# 之后读、能盖掉这里的值，这里不跟着去读，有引入时一律当作没开：看不准时宁可不重启，也不把会话结束掉。
keeps_sessions() {
  [[ -f $config ]] || return 1
  local value
  value=$(awk '
    /^[[:space:]]*#/ { next }
    {
      line = $0; gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
      if (line == "") next
      eq = index(line, "=")
      key = eq ? substr(line, 1, eq - 1) : line; gsub(/[[:space:]]+$/, "", key)
      value = eq ? substr(line, eq + 1) : ""; gsub(/^[[:space:]]+/, "", value)
      if (length(value) >= 2 && value ~ /^".*"$/) value = substr(value, 2, length(value) - 2)
      if (key == "config-file") { if (value != "") unsure = 1; next }
      # 键名两头带着这里去不掉的字符（BOM、不换行空格这类，app 会去掉）：认不准，也当作没开。
      if (key != "terminal-host") { if (index(key, "terminal-host") || index(key, "config-file")) unsure = 1; next }
      if (value == "") last = "false"
      else if (value == "true" || value == "false") last = value
    }
    END { print (unsure ? "unsure" : last) }
  ' "$config")
  [[ $value == true ]]
}

# 在后台跑之前查，免得说明只写进日志里。
if pgrep -fx "$bin" >/dev/null && ! keeps_sessions; then
  echo "配置里没开 terminal-host（退出后保留会话）：从这里退出 app 时会话会跟着结束，不重启了。" >&2
  echo "先在设置里打开「退出后保留会话」，或者在 $config 里写上 terminal-host = true，再重启。" >&2
  echo "$config 里有 config-file 引入别的文件时也不重启：这里核对不了引进来的值。" >&2
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

# 1. 退出 app。宿主单独跑时会话留在它那里；宿主跑在 app 里时，app 退出前把会话交给一个单独跑的宿主。
#    两样都要配置里开着 terminal-host，上面已经查过。
bundle_id=$(defaults read "$app/Contents/Info" CFBundleIdentifier)
app_pid=$(pgrep -fx "$bin" | head -n 1 || true)
if [[ -n "$app_pid" ]]; then
  echo "退出 app（${app_pid}）"
  osascript -e "tell application id \"$bundle_id\" to quit"
  # 把会话交给单独跑的宿主要拉起它、等它接手，宽裕地等。
  if ! wait_gone "$app_pid" 120; then
    echo "app 两分钟内没有退出（是不是在确认框上点了取消？），不重启了" >&2
    exit 1
  fi
fi

# 2. 让新的可执行文件接手旧宿主的会话。成了以后新宿主接着跑，旧宿主等它拉起的 shell 都结束再退出。
old_host=$(lsof -t "$socket" 2>/dev/null | head -n 1 || true)
if [[ -n "$old_host" ]]; then
  echo "让新宿主接手宿主 $old_host 的会话"
  # 新宿主把接手的结果按行写成 JSON 交给 fd 3 上的管道（`host_process::take_over`，和 app 拉起它时一样）：
  # 先是一行 `ready`，再是结果。读管道而不翻日志：日志会被 RUST_LOG 关掉，也会被别的日志冲走。
  # 管道放在只有自己能进的临时目录里，别人换不成指向别处的符号链接。
  status_dir=$(mktemp -d "${TMPDIR:-/tmp}/runode-restart.XXXXXX")
  status_fifo="$status_dir/status"
  mkfifo "$status_fifo"
  # 和 app 一样经 launchd 拉起（`runode_host::launch` 的 `launchd` 模块），让新宿主有自己的 coalition：从这里
  # 直接起的会留在这个终端所在的、早已没了主人的 coalition 里，之后从它的终端打开的 GUI 程序退出时，程序坞
  # 把宿主和所有会话当成那个程序留在后台的进程，点「停止在后台运行」就一起杀掉。launchd 不传描述符，任务里
  # 先跑一段 sh 把 FIFO 打开到 fd 3 上、写一行自己的 pid，再换成宿主。Label 的前缀和 app 的一样，退出后由
  # app 清掉。
  label="dev.runode.host.restart.$$"
  plist="$status_dir/job.plist"
  xml() { sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' <<<"$1"; }
  {
    printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0">\n<dict>\n'
    printf '\t<key>Label</key>\n\t<string>%s</string>\n\t<key>ProgramArguments</key>\n\t<array>\n' "$(xml "$label")"
    # shellcheck disable=SC2016 # $0、$$、$@ 留给任务里的 sh 展开
    for arg in /bin/sh -c 'exec 3>"$0" && echo $$ >&3 && exec "$@"' "$status_fifo" \
      "${launch_env[@]}" "$bin" --host --take-over; do
      printf '\t\t<string>%s</string>\n' "$(xml "$arg")"
    done
    printf '\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n\t<key>ProcessType</key>\n\t<string>Interactive</string>\n'
    printf '\t<key>AbandonProcessGroup</key>\n\t<true/>\n</dict>\n</plist>\n'
  } >"$plist"
  # 进程号重用时同名的旧任务可能还挂着（不在跑），先去掉，不然 bootstrap 报已经加载过。
  launchctl remove "$label" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$plist"
  exec 4<"$status_fifo"
  rm -rf "$status_dir"
  IFS= read -r -t 10 -u 4 new_host || true
  result=""
  # 读到结尾是新宿主没报结果就退出了；一行等太久（app 那边最多等 45 秒）当没接成。
  while IFS= read -r -t 60 -u 4 line; do
    [[ $line == *'"ready":true'* ]] && continue
    result=$line
    break
  done
  exec 4<&-
  if [[ $result == *'"ok":true'* ]] && kill -0 "$new_host" 2>/dev/null; then
    echo "新宿主 $new_host 接手了会话"
  elif [[ $result == *'"kind":"busy"'* ]]; then
    # 同构建的宿主不交接：在跑的已经是磁盘上这份代码（没重新 make install，或者改动和上次装的一样）。
    echo "在跑的宿主和装好的是同一个构建，不用换，只重开 app"
  elif [[ -z $result ]] && kill -0 "$new_host" 2>/dev/null; then
    # 和 app 一样不杀它：要是在旧宿主提交之后才卡住，会话已经在它手里；提交之前卡住的，旧宿主到期限会自己
    # 杀掉它、回滚。
    echo "新宿主 $new_host 没在期限内报结果，没动它，看 $host_log" >&2
    exit 1
  else
    # 报了没接成的会自己退出。
    echo "新宿主没有接手会话（${result:-没报结果就退出了}），看 $host_log" >&2
    exit 1
  fi
else
  echo "没有在跑的宿主，app 打开时会自己拉起"
fi

# 3. 重新打开 app，它会连上这个构建的宿主。
"${launch_env[@]}" open "$app"
echo "已重新打开 $app"
