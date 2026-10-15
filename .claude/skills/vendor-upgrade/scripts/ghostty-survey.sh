#!/usr/bin/env bash
# 同步 ghostty 前的摸底：只读（除了 git fetch），把要判断的事实一次列出来，判断留给人和 agent。
#
# 用法：ghostty-survey.sh [目标上游 commit，默认 upstream/main] [--no-fetch]
# 在 runode 仓库根目录或任意子目录里跑都行。
set -euo pipefail

target=""
fetch=1
for arg in "$@"; do
  case "$arg" in
    --no-fetch) fetch=0 ;;
    *) target="$arg" ;;
  esac
done

root=$(git rev-parse --show-superproject-working-tree 2>/dev/null)
[ -n "$root" ] || root=$(git rev-parse --show-toplevel)
gh="$root/vendor/ghostty"
rs="$root/vendor/libghostty-rs"
build_rs="$rs/crates/libghostty-vt-sys/build.rs"

for dir in "$gh" "$rs"; do
  [ -e "$dir/.git" ] || { echo "子模块 $dir 没检出，先跑 make submodules" >&2; exit 1; }
done

if [ "$fetch" = 1 ]; then
  git -C "$gh" fetch --quiet upstream main
  git -C "$gh" fetch --quiet origin
  git -C "$rs" fetch --quiet upstream master
  git -C "$rs" fetch --quiet origin
fi

target=${target:-upstream/main}
new_base=$(git -C "$gh" rev-parse "$target^{commit}")
old_base=$(git -C "$gh" merge-base HEAD "$new_base")
pinned=$(sed -n 's/^const GHOSTTY_COMMIT: &str = "\([0-9a-f]*\)";/\1/p' "$build_rs")
nix_pin=$(sed -n 's|.*github:ghostty-org/ghostty/\([0-9a-f]*\)".*|\1|p' "$rs/flake.nix" | head -1)

short() { git -C "$gh" rev-parse --short "$1"; }

echo "# ghostty 同步摸底"
echo
echo "- fork HEAD：$(git -C "$gh" log -1 --format='%h %s')"
echo "- fork 当前基于的上游：$(short "$old_base")（$(git -C "$gh" log -1 --format=%cs "$old_base")）"
echo "- 目标上游：$(short "$new_base")（$(git -C "$gh" log -1 --format=%cs "$new_base")）"
echo "- libghostty-rs 的 GHOSTTY_COMMIT：${pinned:0:9}；flake.nix 的 ghostty 输入：${nix_pin:0:9}"
if [ "$pinned" != "$(git -C "$gh" rev-parse "$old_base")" ]; then
  echo "- **警告：GHOSTTY_COMMIT 不等于 fork 当前基于的上游，两边已经没对齐，先弄清楚再动**"
fi
[ "$pinned" = "$nix_pin" ] || echo "- **警告：GHOSTTY_COMMIT 和 flake.nix 的 ghostty 输入不一致**"
dirty_gh=$(git -C "$gh" status --porcelain)
dirty_rs=$(git -C "$rs" status --porcelain)
[ -z "$dirty_gh" ] || echo "- **警告：vendor/ghostty 有未提交的改动**"
[ -z "$dirty_rs" ] || echo "- **警告：vendor/libghostty-rs 有未提交的改动**"
[ "$(git -C "$gh" rev-parse HEAD)" = "$(git -C "$gh" rev-parse origin/main)" ] \
  || echo "- **警告：vendor/ghostty 的 HEAD 不等于 origin/main**"
[ "$(git -C "$rs" rev-parse HEAD)" = "$(git -C "$rs" rev-parse origin/main)" ] \
  || echo "- **警告：vendor/libghostty-rs 的 HEAD 不等于 origin/main**"

count=$(git -C "$gh" rev-list --count "$old_base..$new_base")
echo
echo "## 上游新增 $count 个提交"
if [ "$count" = 0 ]; then
  echo
  echo "上游没有新提交，ghostty 不用同步。"
fi

echo
echo "## runode 在 ghostty fork 上的修复提交"
echo
echo "\`git cherry\` 标 \`-\` 的表示同样的补丁已经在目标上游里（可以直接删）；标 \`+\` 的要逐个核对上游是否换了写法修了同一件事。"
echo
git -C "$gh" cherry -v "$new_base" HEAD | sed 's/^/    /'
for c in $(git -C "$gh" rev-list --reverse "$old_base..HEAD"); do
  echo
  echo "### $(git -C "$gh" log -1 --format='%h %s' "$c")"
  files=$(git -C "$gh" diff-tree --no-commit-id --name-only -r "$c")
  echo
  echo "改动的文件：$(echo $files)"
  echo
  echo "上游这段时间动过同样文件的提交："
  overlap=$(git -C "$gh" log --no-merges --format='%h %cs %s' "$old_base..$new_base" -- $files)
  if [ -n "$overlap" ]; then echo "$overlap" | sed 's/^/    /'; else echo "    （没有）"; fi
  tests=$(git -C "$gh" show "$c" | sed -n 's/^+test "\(.*\)" {$/\1/p')
  if [ -n "$tests" ]; then
    echo
    echo "这个提交带的 Zig 测试（用来验证上游的修法是否覆盖）："
    echo "$tests" | sed 's/^/    /'
  fi
done

[ "$count" = 0 ] || {
echo
echo "## libghostty-vt 的 C 接口变化（新功能多半在这里）"
echo
api=$(git -C "$gh" diff --stat "$old_base" "$new_base" -- include/ghostty/vt.h include/ghostty/vt src/terminal/c src/lib_vt.zig)
if [ -n "$api" ]; then echo "$api" | sed 's/^/    /'; else echo "    （没有变化）"; fi
echo
echo "动了这些地方的提交："
git -C "$gh" log --no-merges --format='    %h %cs %s' "$old_base..$new_base" -- include/ghostty/vt.h include/ghostty/vt src/terminal/c src/lib_vt.zig

echo
echo "## 终端核心（src/terminal，不含 c/）的提交"
echo
git -C "$gh" log --no-merges --format='    %h %cs %s' "$old_base..$new_base" -- src/terminal ':(exclude)src/terminal/c'

echo
echo "## 构建相关"
echo
zig_old=$(git -C "$gh" show "$old_base:build.zig.zon" | sed -n 's/.*minimum_zig_version = "\(.*\)".*/\1/p')
zig_new=$(git -C "$gh" show "$new_base:build.zig.zon" | sed -n 's/.*minimum_zig_version = "\(.*\)".*/\1/p')
echo "- minimum_zig_version：$zig_old → $zig_new"
[ "$zig_old" = "$zig_new" ] || echo "- **Zig 版本变了：根 flake.nix 的 zig、libghostty-rs 的 AGENTS.md 和 CI 都要跟**"
git -C "$gh" log --no-merges --format='    %h %cs %s' "$old_base..$new_base" -- build.zig build.zig.zon src/build

echo
echo "## 其余提交（apprt、macOS app、GTK、文档等，runode 用不到，扫一眼即可）"
echo
git -C "$gh" log --no-merges --format='    %h %s' "$old_base..$new_base" -- . ':(exclude)src/terminal' ':(exclude)include' ':(exclude)src/lib_vt.zig' ':(exclude)build.zig' ':(exclude)build.zig.zon' ':(exclude)src/build' | sed -n '1,60p'
}

rs_base=$(git -C "$rs" merge-base HEAD upstream/master)
echo
echo "## libghostty-rs"
echo
echo "fork 上 runode 加的提交（基于 Uzaaft 的 $(git -C "$rs" rev-parse --short "$rs_base")）。\`-\` 表示上游已有同样补丁："
echo
git -C "$rs" cherry -v upstream/master HEAD | sed 's/^/    /'
up_count=$(git -C "$rs" rev-list --count "$rs_base..upstream/master")
echo
echo "Uzaaft 上游在那之后新增 $up_count 个提交："
git -C "$rs" log --no-merges --format='    %h %cs %s' "$rs_base..upstream/master" | sed -n '1,60p'
