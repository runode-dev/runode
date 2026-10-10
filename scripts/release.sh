#!/bin/bash
# 发版：写好中英双语的更新说明，改版本号、提交、打标签、推上去，之后由 release.yml 构建和发布。
#
# 更新说明存成 docs/releases/vX.Y.Z.md，和版本号一起提交；release.yml 发 Release 时拿它当说明，
# 没有这个文件时（比如在 Actions 页面手动发版）退回 GitHub 自动生成的说明。
#
# 草稿按上个标签以来的 feat、fix、perf 提交生成：本机有 claude 命令时让它照上一版的说明归纳成中英
# 两段，没有时（或它失败时）中文列出提交标题、英文留 TODO。草稿在编辑器（$EDITOR，默认 vi）里打开，
# 改好保存退出；里面还有 TODO 时不往下走。
#
# 带 --beta 时发成 beta：说明第一行写上 `<!-- prerelease -->`，release.yml 见到它就发成预发布、不标
# latest，自动更新拿不到；测好了转正，连标题里的 Beta 一起去掉：
# `gh release edit vX.Y.Z --prerelease=false --latest --title "Runode X.Y.Z"`。
#
# 带 --yes 时不开编辑器、不问确认，直接发已经写好的 docs/releases/vX.Y.Z.md，没写好时不发；给 agent
# 这类没有终端可交互的用。
#
# 用法：release.sh [--beta] [--yes] [版本号]，版本号是 x.y.z，不给时补丁号加一。
set -euo pipefail

cd "$(dirname "$0")/.."

die() { echo "$*" >&2; exit 1; }

beta=
yes=
while [[ "${1:-}" == --* ]]; do
  case "$1" in
    --beta) beta=1 ;;
    --yes) yes=1 ;;
    *) die "不认识的选项 $1" ;;
  esac
  shift
done

current=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
if [[ -n "${1:-}" ]]; then
  version=${1#v}
else
  IFS=. read -r major minor patch <<<"$current"
  version="$major.$minor.$((patch + 1))"
fi
tag="v$version"
notes="docs/releases/$tag.md"

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "版本号 $version 不是 x.y.z"
# 新版本要比现在的大，app 才会更新过去。
[[ "$current" != "$version" && "$(printf '%s\n%s\n' "$current" "$version" | sort -V | tail -1)" == "$version" ]] \
  || die "版本号 $version 不比现在的 $current 大"
[[ "$(git branch --show-current)" == main ]] || die "要在 main 上发版"
# 上次没发完留下的草稿不算改动。
[[ -z "$(git status --porcelain -- ":(exclude)$notes")" ]] || die "工作区有没提交的改动"
git fetch -q --tags origin main
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || die "本地 main 和 origin/main 不一致，先同步"
! git rev-parse -q --verify "refs/tags/$tag" >/dev/null || die "标签 $tag 已经有了"

# 上个标签以来某一类提交的标题，去掉前缀，一行一条。
commits() {
  local range=HEAD last
  last=$(git describe --tags --abbrev=0 2>/dev/null) && range="$last..HEAD"
  git log --no-merges --format=%s "$range" | sed -n "s/^$1\(([^)]*)\)\{0,1\}!\{0,1\}: */- /p"
}

if [[ ! -f "$notes" ]]; then
  [[ -z "$yes" ]] || die "--yes 只发写好的更新说明，先写好 $notes"
  # 上一版的说明给 claude 当格式的样子，要在新文件建出来之前找。
  previous=$(ls docs/releases/v*.md 2>/dev/null | sort -V | tail -1)
  mkdir -p docs/releases
  zh=$(
    for group in "feat:新功能" "fix:修复" "perf:性能"; do
      lines=$(commits "${group%%:*}")
      if [[ -n "$lines" ]]; then printf '### %s\n\n%s\n\n' "${group#*:}" "$lines"; fi
    done
  )
  en="TODO：把上面的中文归纳好、译成英文，标题用 ### Features、### Fixes、### Performance。"
  draft=$(printf '## 中文\n\n%s\n\n## English\n\n%s\n' "${zh:-TODO：写这一版的改动。}" "$en")
  if command -v claude >/dev/null && [[ -n "$zh" ]]; then
    echo "让 claude 归纳更新说明……" >&2
    draft=$(
      {
        if [[ -n "$previous" ]]; then printf 'Previous release notes:\n\n%s\n\n' "$(cat "$previous")"; fi
        printf 'Commits since the previous release:\n\n%s\n' "$zh"
      } | claude -p --no-session-persistence \
        "Write the release notes for the next version of Runode (a terminal app for AI coding agents) from the commits on stdin. Follow the previous notes' format and level of detail: a '## 中文' section with '### 新功能', '### 修复', '### 性能' (only the groups that have entries), then a '## English' section with the same bullets in English under '### Features', '### Fixes', '### Performance'. Merge related commits into one bullet, say what users notice rather than how it was done, give big features a short bold name, leave out changes users cannot notice (tests, refactors, CI), write each bullet as one line with no nested lists, and keep each group to about a dozen bullets. Output only the Markdown."
    ) || {
      echo "claude 归纳失败，草稿只列提交标题" >&2
      draft=$(printf '## 中文\n\n%s\n\n## English\n\n%s\n' "$zh" "$en")
    }
  fi
  printf '%s\n' "$draft" >"$notes"
fi

[[ -n "$yes" ]] || ${EDITOR:-vi} "$notes"
! grep -q TODO "$notes" || die "$notes 里还有 TODO，改好后重新运行（草稿留着）"
grep -q '^## 中文' "$notes" && grep -q '^## English' "$notes" || die "$notes 要有「## 中文」和「## English」两段"
# 标记跟着这次的 --beta 走：草稿可能是上次按另一种发法留下的。
marker='<!-- prerelease -->'
body=$(grep -vxF "$marker" "$notes")
if [[ -n "$beta" ]]; then printf '%s\n%s\n' "$marker" "$body"; else printf '%s\n' "$body"; fi >"$notes"

cat "$notes"
if [[ -z "$yes" ]]; then
  read -rp "发布 ${tag}${beta:+ beta}？[y/N] " answer
  [[ "$answer" == [yY] ]] || die "没发，草稿留在 $notes"
fi

sed -i.bak '/^\[workspace.package\]/,/^\[/s/^version = ".*"/version = "'"$version"'"/' Cargo.toml
rm Cargo.toml.bak
# 只更新 Cargo.lock 里 workspace 自己的包的版本，不动依赖。
cargo update --workspace
git add Cargo.toml Cargo.lock "$notes"
base=$(git rev-parse HEAD)
git commit -m "chore: 发布 $tag${beta:+ beta}"
git tag "$tag"
if ! git push --atomic origin main "$tag"; then
  # 推不上时撤掉这次的标签和提交、改回版本号，修好后直接重跑；更新说明留着当草稿。只在 HEAD 的上一个
  # 确实是发版前的提交时撤，撤的也只有这里改过的文件。
  git tag -d "$tag" >/dev/null
  if [[ "$(git rev-parse HEAD^)" == "$base" ]]; then
    git reset -q "$base"
    git checkout -- Cargo.toml Cargo.lock
    die "推送失败，已撤掉标签 $tag 和发版提交，草稿留在 $notes；修好后重新运行"
  fi
  die "推送失败，已删掉标签 $tag；HEAD 不是刚才的发版提交，没有撤提交，自己看一下 git log"
fi
echo "已推送 ${tag}，release.yml 接着构建和发布：gh run watch" >&2
