#!/bin/sh
# shellcheck shell=sh
# runode 启动时会用自带的版本覆盖本文件；要改就复制到别处，再把定时的先跑命令指向副本。
#
# 定时的先跑命令：找带某个标签、还开着、没派过的 issue，挑编号最小的一个，把它的内容和评论写到标准输出，
# runode 据此开任务；没有时标准输出为空。派过的编号记在状态目录的 seen 里，离开列表（关掉、摘了标签）的从
# seen 里清掉。日志一律写标准错误。
# 用法：new-issues.sh <标签>
set -u

STATE_DIR="$(dirname -- "$0")/state/${RUNODE_SCHEDULE:-manual}"
SEEN="$STATE_DIR/seen"

log() { printf 'new-issues: %s\n' "$*" >&2; }

# 正文、评论和文件清单各自最多留几个字节，一起管着标准输出的大小，要在宿主收得下的提示词里留出定时自己的
# 提示词。截掉的部分由 agent 自己用 gh 再读。
MAX_TEXT=16000

# 打印 $1，超过 $2 字节时只留开头，再接一行说明。
clip() {
  if [ "$(printf '%s' "$1" | wc -c)" -gt "$2" ]; then
    printf '%s' "$1" | head -c "$2"
    printf '\n[cut here; %s]' "$3"
  else
    printf '%s' "$1"
  fi
}

label=${1:-}
if [ -z "$label" ]; then
  log "usage: new-issues.sh <label>"
  exit 2
fi

mkdir -p "$STATE_DIR" || exit 1
[ -f "$SEEN" ] || : >"$SEEN" || exit 1

list=$(gh issue list --state open --label "$label" --limit 100 --json number --jq '.[].number') \
  || { log "gh issue list failed"; exit 1; }
seen_list=$(cat "$SEEN") || exit 1

# 记在 seen 里的是派过的，seen 只留还在列表里的；其余的按编号从小到大等着派。
kept=$(printf '%s\n' "$list" | SEEN_LIST=$seen_list awk '
  BEGIN { n = split(ENVIRON["SEEN_LIST"], lines, "\n"); for (i = 1; i <= n; i++) seen[lines[i]] = 1 }
  $1 != "" && ($1 in seen) { print $1 }
') || exit 1
number=$(printf '%s\n' "$list" | SEEN_LIST=$seen_list awk '
  BEGIN { n = split(ENVIRON["SEEN_LIST"], lines, "\n"); for (i = 1; i <= n; i++) seen[lines[i]] = 1 }
  $1 != "" && !($1 in seen) { print $1 }
' | sort -n | head -n 1) || exit 1

if [ -z "$number" ]; then
  if [ -n "$kept" ]; then printf '%s\n' "$kept"; fi >"$SEEN.tmp" && mv -f "$SEEN.tmp" "$SEEN" || exit 1
  log "no new issues labeled $label"
  exit 0
fi

# 不接终端时 --comments 只打印评论，正文要另取一次。
view=$(gh issue view "$number" </dev/null) || { log "gh issue view $number failed"; exit 1; }
comments=$(gh issue view "$number" --comments </dev/null) || { log "gh issue view $number --comments failed"; exit 1; }
view=$(clip "$view" "$MAX_TEXT" "gh issue view $number shows all of it")
comments=$(clip "$comments" "$MAX_TEXT" "gh issue view $number --comments shows all of them")

{
  if [ -n "$kept" ]; then printf '%s\n' "$kept"; fi
  printf '%s\n' "$number"
} >"$SEEN.tmp" && mv -f "$SEEN.tmp" "$SEEN" || exit 1
log "dispatching issue #$number"
printf 'Issue #%s\n\n%s\n\nComments:\n%s\n' "$number" "$view" "$comments"
