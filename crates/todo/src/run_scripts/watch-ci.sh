#!/bin/sh
# shellcheck shell=sh
# runode 启动时会用自带的版本覆盖本文件；要改就复制到别处，再把定时的先跑命令指向副本。
#
# 定时的先跑命令：查一次分支上的 GitHub Actions，某个 workflow 最新一次有结论的运行失败了、还没派过时，把失败
# 日志的末尾写到标准输出，runode 据此开任务；没事时标准输出为空。派过的 workflow 记在状态目录的 pending 里，
# 它在这个分支上重新通过之后再失败才会再派。workflow 在这个分支上最新的一次还在排队或在跑时这次不看它：新的
# 提交可能已经修好，等它跑完再说。日志一律写标准错误。
# 用法：watch-ci.sh [分支]，不给时用仓库的默认分支。
set -u

# 一次最多派几个 workflow，没轮到的下次再派。三个数一起管着标准输出的大小，要在宿主收得下的提示词里留出定时
# 自己的提示词。
MAX_RUNS=3
# 每个 workflow 留失败日志的最后几行。
LOG_LINES=40
# 日志每行最多留几个字节。
LINE_BYTES=300

TAB=$(printf '\t')
BOM=$(printf '\357\273\277')
STATE_DIR="$(dirname -- "$0")/state/${RUNODE_SCHEDULE:-manual}"
PENDING="$STATE_DIR/pending"

log() { printf 'watch-ci: %s\n' "$*" >&2; }

mkdir -p "$STATE_DIR" || exit 1
[ -f "$PENDING" ] || : >"$PENDING" || exit 1

branch=${1:-}
if [ -z "$branch" ]; then
  branch=$(gh repo view --json defaultBranchRef --jq .defaultBranchRef.name) || { log "gh repo view failed"; exit 1; }
  [ -n "$branch" ] || { log "cannot tell the default branch"; exit 1; }
fi

runs=$(gh run list --branch "$branch" --limit 60 \
  --json databaseId,workflowName,headSha,status,conclusion \
  --jq '.[] | [.databaseId, .workflowName, .headSha, .status, .conclusion] | @tsv') || { log "gh run list failed"; exit 1; }

# gh 按新到旧给，每个 workflow 只看最新一次有结论的运行（cancelled、skipped 不算结论）；在那之前碰到还没跑完的
# 就这次不看它，pending 照旧。最新一次通过的移出 pending；失败了、不在 pending 里的派，一次最多 MAX_RUNS 个。
# 输出 `send` 行是这次派的，`keep` 行是 pending 里留下的。
plan=$(printf '%s\n' "$runs" | PENDING_LIST=$(cat "$PENDING") MAX_RUNS=$MAX_RUNS awk -F "$TAB" '
  BEGIN {
    n = split(ENVIRON["PENDING_LIST"], lines, "\n")
    for (i = 1; i <= n; i++) if (lines[i] != "") pending[lines[i]] = 1
    limit = ENVIRON["MAX_RUNS"] + 0
  }
  NF < 5 || ($2 in latest) { next }
  $4 != "completed" { latest[$2] = 1; next }
  $5 == "cancelled" || $5 == "skipped" { next }
  { latest[$2] = 1 }
  $5 == "success" || $5 == "neutral" { delete pending[$2]; next }
  ($5 == "failure" || $5 == "timed_out" || $5 == "startup_failure") && !($2 in pending) && sent < limit {
    sent++
    print "send" FS $1 FS $2 FS $3 FS $5
  }
  END { for (wf in pending) print "keep" FS wf }
') || exit 1

kept=""
sent=""
sections=""
LOG_FILE="$STATE_DIR/run-log.tmp"
while IFS="$TAB" read -r kind a b c d; do
  case $kind in
    keep) kept="$kept$a
" ;;
    send)
      id=$a wf=$b conclusion=$d
      sha=$(printf '%.9s' "$c")
      # --log-failed 每行是 `job<TAB>step<TAB>时间戳 内容`（时间戳前可能有 BOM），只留内容。每个 job 从
      # `Post job cleanup.` 起是 runner 收尾的输出，和 `##[start-action`、`##[end-action` 标记一样丢掉，末尾留下的
      # 才是报错。按字节截，C 区域下 awk 的 substr 才按字节算。
      if gh run view "$id" --log-failed >"$LOG_FILE" </dev/null; then
        tail_lines=$(LC_ALL=C awk -v width="$LINE_BYTES" -v bom="$BOM" '{
          sub(/\r$/, "")
          job = $0
          sub(/\t.*/, "", job)
          sub(/^[^\t]*\t[^\t]*\t/, "")
          if (index($0, bom) == 1) $0 = substr($0, length(bom) + 1)
          sub(/^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9:.]+Z /, "")
          if ($0 == "Post job cleanup.") cleanup[job] = 1
          if ((job in cleanup) || index($0, "##[start-action") == 1 || index($0, "##[end-action") == 1) next
          print substr($0, 1, width)
        }' "$LOG_FILE" | tail -n "$LOG_LINES")
      else
        log "cannot fetch the failed log of run $id"
        tail_lines="(gh run view $id --log-failed failed; look at the run on GitHub)"
      fi
      rm -f "$LOG_FILE"
      sent="$sent$wf
"
      sections="$sections
## $wf (run $id, commit $sha, $conclusion)
$tail_lines
" ;;
  esac
done <<EOF
$plan
EOF

printf '%s%s' "$kept" "$sent" >"$PENDING.tmp" && mv -f "$PENDING.tmp" "$PENDING" || exit 1

if [ -z "$sent" ]; then
  log "nothing new failed on $branch"
  exit 0
fi
log "dispatching: $(printf '%s' "$sent" | tr '\n' ',' | sed 's/,$//')"
printf 'These GitHub Actions workflows failed on %s. The end of each failed log follows (gh run view <id> --log-failed prints the whole log).\n%s' \
  "$branch" "$sections"
