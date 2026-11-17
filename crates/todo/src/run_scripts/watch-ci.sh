#!/bin/sh
# shellcheck shell=sh
# runode 启动时会用自带的版本覆盖本文件；要改就复制到别处，再把定时的先跑命令指向副本。
#
# 定时的先跑命令：查一次分支上的 GitHub Actions，某个 workflow 最新一次有结论的运行失败了、还没派过时，把失败
# 日志的末尾写到标准输出，runode 据此开任务；没事时标准输出为空。派过的 workflow 记在状态目录的 pending 里，
# 它在这个分支上重新通过之后再失败才会再派。workflow 在这个分支上最新的一次还在排队或在跑时不看更早的运行：
# 新的提交可能已经修好；但这次运行里已经有 job 失败了就不等它跑完，按失败的 job 派。日志一律写标准错误。
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

# 读 `job<TAB>step<TAB>时间戳 内容` 格式的日志（时间戳前可能有 BOM），只留内容的最后几行。每个 job 从
# `Post job cleanup.` 起是 runner 收尾的输出，和 `##[start-action`、`##[end-action` 标记一样丢掉，末尾留下的
# 才是报错。按字节截，C 区域下 awk 的 substr 才按字节算。
log_tail() {
  LC_ALL=C awk -v width="$LINE_BYTES" -v bom="$BOM" '{
    sub(/\r$/, "")
    job = $0
    sub(/\t.*/, "", job)
    sub(/^[^\t]*\t[^\t]*\t/, "")
    if (index($0, bom) == 1) $0 = substr($0, length(bom) + 1)
    sub(/^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9:.]+Z /, "")
    if ($0 == "Post job cleanup.") cleanup[job] = 1
    if ((job in cleanup) || index($0, "##[start-action") == 1 || index($0, "##[end-action") == 1) next
    print substr($0, 1, width)
  }' "$1" | tail -n "$LOG_LINES"
}

# 一个 job 的原始日志。运行没跑完时 `gh run view` 不给日志，只能经 API 取单个 job 的；新版 gh 遇到日志里的终端
# 转义序列要 --allow-escape-sequences 才肯输出，旧版不认这个选项。
job_log() {
  gh api "repos/{owner}/{repo}/actions/jobs/$1/logs" --allow-escape-sequences 2>/dev/null </dev/null ||
    gh api "repos/{owner}/{repo}/actions/jobs/$1/logs" </dev/null
}

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

# gh 按新到旧给，每个 workflow 只看最新一次有结论的运行（cancelled、skipped、action_required 不算结论）；在那之前
# 碰到还没跑完的就不看更早的运行，它不在 pending 里时查它已经失败的 job，等人批准的（waiting）跳过，它不知道要
# 等多久。最新一次通过的移出 pending。
# 输出 `send` 行是失败了、不在 pending 里的运行，`probe` 行是要查 job 的运行，`keep` 行是 pending 里留下的。
plan=$(printf '%s\n' "$runs" | PENDING_LIST=$(cat "$PENDING") awk -F "$TAB" '
  BEGIN {
    n = split(ENVIRON["PENDING_LIST"], lines, "\n")
    for (i = 1; i <= n; i++) if (lines[i] != "") pending[lines[i]] = 1
  }
  NF < 5 || ($2 in latest) { next }
  $4 == "waiting" { next }
  $4 != "completed" {
    latest[$2] = 1
    if (!($2 in pending)) print "probe" FS $1 FS $2 FS $3
    next
  }
  $5 == "cancelled" || $5 == "skipped" || $5 == "action_required" { next }
  { latest[$2] = 1 }
  $5 == "success" || $5 == "neutral" { delete pending[$2]; next }
  ($5 == "failure" || $5 == "timed_out" || $5 == "startup_failure") && !($2 in pending) {
    print "send" FS $1 FS $2 FS $3 FS $5
  }
  END { for (wf in pending) print "keep" FS wf }
') || exit 1

kept=""
sent=""
count=0
sections=""
running=""
LOG_FILE="$STATE_DIR/run-log.tmp"
JOB_FILE="$STATE_DIR/job-log.tmp"
while IFS="$TAB" read -r kind a b c d; do
  case $kind in
    keep) kept="$kept$a
" ;;
    send | probe)
      [ "$count" -lt "$MAX_RUNS" ] || continue
      id=$a wf=$b
      sha=$(printf '%.9s' "$c")
      if [ "$kind" = send ]; then
        status=$d
        if gh run view "$id" --log-failed >"$LOG_FILE" </dev/null; then
          tail_lines=$(log_tail "$LOG_FILE")
        else
          log "cannot fetch the failed log of run $id"
          tail_lines="(gh run view $id --log-failed failed; look at the run on GitHub)"
        fi
      else
        job_list=$(gh run view "$id" --json jobs --jq '.jobs[] | [.databaseId, .name, .conclusion] | @tsv' </dev/null) ||
          { log "cannot list the jobs of run $id"; continue; }
        failed=$(printf '%s\n' "$job_list" | awk -F "$TAB" '$3 == "failure" || $3 == "timed_out" { print $1 FS $2 }')
        [ -n "$failed" ] || continue
        : >"$LOG_FILE"
        names=""
        while IFS="$TAB" read -r job_id job_name; do
          [ -n "$job_id" ] || continue
          names="$names, $job_name (job $job_id)"
          # API 给的日志行没有 `job<TAB>step<TAB>` 前缀，补上才能和 --log-failed 的一样处理。
          if job_log "$job_id" >"$JOB_FILE"; then
            awk -v job="$job_name" '{ print job "\t-\t" $0 }' "$JOB_FILE" >>"$LOG_FILE"
          else
            log "cannot fetch the log of job $job_id"
          fi
        done <<JOBS
$failed
JOBS
        status="failed in ${names#, } while the run is still going"
        tail_lines=$(log_tail "$LOG_FILE")
        [ -n "$tail_lines" ] || tail_lines="(cannot fetch the job logs; look at the run on GitHub)"
        running=1
      fi
      rm -f "$LOG_FILE" "$JOB_FILE"
      count=$((count + 1))
      sent="$sent$wf
"
      sections="$sections
## $wf (run $id, commit $sha, $status)
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
printf 'These GitHub Actions workflows failed on %s. The end of each failed log follows (gh run view <id> --log-failed prints the whole log).\n' "$branch"
if [ -n "$running" ]; then
  printf 'gh run view prints no logs for a run that is still going; gh api repos/{owner}/{repo}/actions/jobs/<job id>/logs prints the log of one finished job.\n'
fi
printf '%s' "$sections"
