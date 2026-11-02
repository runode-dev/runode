#!/usr/bin/env bash
# 经 ssh 在一台 Windows 机器上编出装到 SSH 远端用的 zip，取回到本机的 target/windows。开发构建连 Windows 远端时
# 不下载发布包，要用环境变量指着这样一个 zip。
#
# 带过去的是这里的 HEAD 加上工作区里没提交的改动（含没跟踪的文件）；子模块按 HEAD 记的提交从 GitHub 拉，子模块里
# 没提交、没推的改动带不过去。HEAD 相同时那边编出的构建号和这里的开发构建一样。zip 里的可执行文件是那台机器的架构：
# ARM64 的 Windows 两种都跑得了，x64 的只跑 x64 的。
#
# 那台机器上要装好编 runode 的工具（MSVC 生成工具、rustup、zig、git，ARM64 上另要 LLVM），都在 PATH 里；
# 代码放在它家目录下的 runode-zip，第一次要整个编一遍。
#
# 环境变量：WINDOWS_HOST 是 ssh 的主机名（默认 windows），WINDOWS_PROFILE 是 debug 或 release（默认 debug）。
set -euo pipefail

host=${WINDOWS_HOST:-windows}
profile=${WINDOWS_PROFILE:-debug}
root=$(git rev-parse --show-toplevel)
out=${CARGO_TARGET_DIR:-$root/target}/windows
name=runode-zip

work=$(mktemp -d)
ssh_opts=(-o ControlMaster=auto -o "ControlPath=$work/c" -o ControlPersist=60)
cleanup() {
	ssh "${ssh_opts[@]}" -O exit "$host" 2>/dev/null || true
	rm -rf "$work"
}
trap cleanup EXIT
# 那边的默认 shell 可能是 cmd 也可能是 PowerShell，每条只是一个程序加参数，两种都认。
run() { ssh "${ssh_opts[@]}" "$host" "$1"; }

head=$(git -C "$root" rev-parse HEAD)
run "git init -q $name"
run "git -C $name config core.autocrlf false"
have=$(run "git -C $name rev-parse -q --verify HEAD" 2>/dev/null | tr -d '\r' || true)

uploads=()
if [ "$have" != "$head" ]; then
	# 那边已有的提交这里也有时只带两者之间的。
	range=()
	if [ -n "$have" ] && git -C "$root" cat-file -e "$have^{commit}" 2>/dev/null; then range=("^$have"); fi
	git -C "$root" bundle create -q "$work/$name.bundle" HEAD ${range[@]+"${range[@]}"}
	uploads+=("$work/$name.bundle")
fi
# 用一份临时的暂存区比出工作区的全部改动，不动这里真正的暂存区。
cp "$(git -C "$root" rev-parse --path-format=absolute --git-path index)" "$work/index"
GIT_INDEX_FILE="$work/index" git -C "$root" add -A
GIT_INDEX_FILE="$work/index" git -C "$root" diff --cached --binary --ignore-submodules=all HEAD >"$work/$name.patch"
[ -s "$work/$name.patch" ] && uploads+=("$work/$name.patch")
if [ ${#uploads[@]} -gt 0 ]; then scp -q "${ssh_opts[@]}" "${uploads[@]}" "$host:"; fi

[ "$have" != "$head" ] && run "git -C $name fetch -q ../$name.bundle HEAD"
run "git -C $name checkout -q -f --detach $head"
run "git -C $name clean -fdq"
[ -s "$work/$name.patch" ] && run "git -C $name apply --whitespace=nowarn ../$name.patch"
run "git -C $name submodule --quiet update --init --recursive"
run "powershell -NoProfile -ExecutionPolicy Bypass -File $name\\scripts\\windows\\package-zip.ps1 -BuildProfile $profile -Output $name.zip"

mkdir -p "$out"
scp -q "${ssh_opts[@]}" "$host:$name.zip" "$out/runode-windows.zip"
echo "取回了 $out/runode-windows.zip，开发构建连 Windows 远端时带上："
echo "  RUNODE_SSH_BINARY_WINDOWS_X86_64=$out/runode-windows.zip make run"
