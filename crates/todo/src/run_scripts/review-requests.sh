#!/bin/sh
# shellcheck shell=sh
# runode 启动时会用自带的版本覆盖本文件；要改就复制到别处，再把定时的先跑命令指向副本。
#
# 定时的先跑命令：找请你评审、还没派过的拉取请求（不算草稿），挑编号最小的一个，把它的说明和改到的文件写到
# 标准输出，runode 据此开任务；没有时标准输出为空。派过的记在状态目录的 seen 里（编号和 head 的 commit），
# 推了新 commit，或者离开列表后又请求你评审时会再派。日志一律写标准错误。
# 用法：review-requests.sh
set -u

TAB=$(printf '\t')
STATE_DIR="$(dirname -- "$0")/state/${RUNODE_SCHEDULE:-manual}"
SEEN="$STATE_DIR/seen"

log() { printf 'review-requests: %s\n' "$*" >&2; }

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

mkdir -p "$STATE_DIR" || exit 1
[ -f "$SEEN" ] || : >"$SEEN" || exit 1

list=$(gh pr list --state open --search "draft:false review-requested:@me" --limit 100 \
  --json number,headRefOid --jq '.[] | [.number, .headRefOid] | @tsv') || { log "gh pr list failed"; exit 1; }
seen_list=$(cat "$SEEN") || exit 1

# 编号和 head 都记在 seen 里的是派过的，seen 只留这些；其余的按编号从小到大等着派。
kept=$(printf '%s\n' "$list" | SEEN_LIST=$seen_list awk -F "$TAB" '
  BEGIN { n = split(ENVIRON["SEEN_LIST"], lines, "\n"); for (i = 1; i <= n; i++) seen[lines[i]] = 1 }
  NF >= 2 && (($1 " " $2) in seen) { print $1 " " $2 }
') || exit 1
pick=$(printf '%s\n' "$list" | SEEN_LIST=$seen_list awk -F "$TAB" '
  BEGIN { n = split(ENVIRON["SEEN_LIST"], lines, "\n"); for (i = 1; i <= n; i++) seen[lines[i]] = 1 }
  NF >= 2 && !(($1 " " $2) in seen) { print }
' | sort -n | head -n 1) || exit 1

if [ -z "$pick" ]; then
  if [ -n "$kept" ]; then printf '%s\n' "$kept"; fi >"$SEEN.tmp" && mv -f "$SEEN.tmp" "$SEEN" || exit 1
  log "no new review requests"
  exit 0
fi

number=${pick%%"$TAB"*}
head=${pick#*"$TAB"}
view=$(gh pr view "$number" </dev/null) || { log "gh pr view $number failed"; exit 1; }
files=$(gh pr diff "$number" --name-only </dev/null) || { log "gh pr diff $number failed"; exit 1; }
view=$(clip "$view" "$MAX_TEXT" "gh pr view $number shows all of it")
files=$(clip "$files" "$MAX_TEXT" "gh pr diff $number --name-only lists all of them")

{
  if [ -n "$kept" ]; then printf '%s\n' "$kept"; fi
  printf '%s %s\n' "$number" "$head"
} >"$SEEN.tmp" && mv -f "$SEEN.tmp" "$SEEN" || exit 1
log "dispatching pull request #$number"
printf 'Pull request #%s (head %s)\n\n%s\n\nFiles changed:\n%s\n' "$number" "$head" "$view" "$files"
