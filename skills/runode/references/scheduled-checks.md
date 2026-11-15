# Scripts for schedules that start an agent only when there is work

A schedule with `--run CMD` runs CMD when due and starts a task only when CMD
exits 0 with something on stdout; the rules are under "Start an agent only
when there is work" in [task-board.md](task-board.md). The scripts below
follow them; copy one into the repository (or `~/.runode/scripts/`), adapt
it, and run it by hand in DIR before scheduling it: it should print nothing
on stdout when there is nothing to do.

## The skeleton

Every script has the same four steps. Keep stdout for the prompt alone and
everything else on stderr.

```sh
#!/bin/sh
set -u
# State survives between runs; one directory per schedule.
state="${XDG_STATE_HOME:-$HOME/.local/state}/runode-checks/${RUNODE_SCHEDULE:-manual}"
mkdir -p "$state" || exit 1

# 1. Look. When the check itself breaks, exit non-zero: the run counts as
#    failed, shows in `runode schedule list`, and no task is started.
found=$(some-command) || exit 1

# 2. Nothing to do: exit 0 without printing.
if [ -z "$found" ]; then echo "nothing to do" >&2; exit 0; fi

# 3. Handed out already: exit 0 without printing, or the same work is handed
#    out again as soon as the last task is done.
key=$(printf '%s' "$found" | cksum | cut -d' ' -f1)
if grep -qxF "$key" "$state/seen" 2>/dev/null; then
  echo "handed out already" >&2; exit 0
fi

# 4. Remember it, then print the prompt. The schedule's --prompt comes
#    before this output, so print only what changes from run to run.
echo "$key" >> "$state/seen"
printf 'Found this:\n%s\n' "$found"
```

Keep the output well under 64 KiB (`tail` long logs): a longer output fails
the run, and step 4 has already remembered the work by then.

## CI failures on main

Hands out each workflow whose latest finished run on main failed, once; a
workflow is handed out again only after it has passed in between. Needs `gh`
logged in.

```sh
#!/bin/sh
set -u
state="${XDG_STATE_HOME:-$HOME/.local/state}/runode-checks/${RUNODE_SCHEDULE:-manual}"
mkdir -p "$state" && touch "$state/pending" || exit 1
latest=$(gh run list --branch main --status completed --limit 40 \
  --json databaseId,workflowName,conclusion --jq '
    map(select(.conclusion != "cancelled" and .conclusion != "skipped"))
    | group_by(.workflowName) | map(max_by(.databaseId))[]
    | [.databaseId, .conclusion, .workflowName] | @tsv') || exit 1
tab=$(printf '\t')
out=""
while IFS="$tab" read -r id conclusion workflow; do
  case "$conclusion" in
    success|neutral)
      grep -vxF "$workflow" "$state/pending" > "$state/pending.new"
      mv "$state/pending.new" "$state/pending" ;;
    failure|timed_out|startup_failure)
      grep -qxF "$workflow" "$state/pending" && continue
      log=$(gh run view "$id" --log-failed | tail -n 80 | cut -c 1-300)
      echo "$workflow" >> "$state/pending"
      out="$out

## $workflow (run $id)
$log" ;;
  esac
done <<EOF
$latest
EOF
if [ -z "$out" ]; then echo "no new failures on main" >&2; exit 0; fi
printf 'These workflows fail on main. The end of each failing log:%s\n' "$out"
```

```sh
runode schedule add 'Fix CI on main' --every 10 --worktree --auto --auto-done \
  --time-limit 90 --run 'sh scripts/ci-failures.sh' \
  --prompt 'Find the cause of these CI failures and fix it; run the failing checks
locally and commit on your branch. If the cause is outside the code (a flaky
runner, an expired secret), change nothing and say so.'
```

## New issues with a label

Hands out one open issue labelled `agent` per run, oldest first. The schedule
waits while its last task is unfinished, so the issues are worked one after
another.

```sh
#!/bin/sh
set -u
state="${XDG_STATE_HOME:-$HOME/.local/state}/runode-checks/${RUNODE_SCHEDULE:-manual}"
mkdir -p "$state" || exit 1
numbers=$(gh issue list --label agent --state open --limit 100 \
  --json number --jq 'map(.number) | sort | .[]') || exit 1
for n in $numbers; do
  grep -qxF "$n" "$state/seen" 2>/dev/null && continue
  issue=$(gh issue view "$n" --comments) || exit 1
  echo "$n" >> "$state/seen"
  printf 'GitHub issue #%s:\n\n%s\n' "$n" "$issue"
  exit 0
done
echo "no new issues" >&2
```

```sh
runode schedule add 'Work the agent issues' --every 30 --worktree --auto \
  --time-limit 120 --run 'sh scripts/new-issues.sh' \
  --prompt 'Fix this issue, add a test, and commit on your branch with "Fixes #N".'
```

## A command that fails

Runs any command and starts an agent only when it fails: a slow test suite,
a lint, `cargo audit`, `npm audit`. It hands out the failure again on the
next due run while it keeps failing (after the last task is done), which
suits daily checks.

```sh
#!/bin/sh
# Usage: on-failure.sh COMMAND [ARG...]
log=$(mktemp) || exit 1
trap 'rm -f "$log"' EXIT
if "$@" > "$log" 2>&1; then echo "passed: $*" >&2; exit 0; fi
printf '`%s` fails in %s. The end of its output:\n\n' "$*" "$(pwd)"
tail -n 150 "$log" | cut -c 1-300
```

```sh
runode schedule add 'Nightly full tests' --daily 03:00 --worktree --auto \
  --auto-done --time-limit 120 --run-timeout 60 \
  --run 'sh scripts/on-failure.sh cargo test --workspace' \
  --prompt 'Find why this fails and fix it; commit on your branch.'
```

Give `--run-timeout` room for the command itself; the default is 5 minutes.

## Something new upstream

Remembers the last value it saw and starts an agent when it changes; the
first run only remembers. Here a new release of a dependency; any command
that prints one short line works (a version, a hash of a page, a count).

```sh
#!/bin/sh
set -u
state="${XDG_STATE_HOME:-$HOME/.local/state}/runode-checks/${RUNODE_SCHEDULE:-manual}"
mkdir -p "$state" || exit 1
new=$(gh release view --repo owner/lib --json tagName --jq .tagName) || exit 1
old=$(cat "$state/last" 2>/dev/null)
if [ "$new" = "$old" ]; then echo "still $new" >&2; exit 0; fi
echo "$new" > "$state/last"
if [ -z "$old" ]; then echo "first run, remembering $new" >&2; exit 0; fi
printf 'owner/lib released %s (we use %s). Its release notes:\n\n' "$new" "$old"
gh release view "$new" --repo owner/lib --json body --jq .body | head -c 20000
```

```sh
runode schedule add 'Upgrade lib' --daily 09:00 --worktree --auto \
  --time-limit 60 --run 'sh scripts/new-release.sh' \
  --prompt 'Upgrade to this release, adapt the code to breaking changes, run the
tests, and commit on your branch.'
```
