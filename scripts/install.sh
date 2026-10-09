#!/bin/sh
# 一行装上 Runode：
#
#   curl -fsSL https://raw.githubusercontent.com/runode-dev/runode/main/scripts/install.sh | sh
#
# macOS：下载这台 Mac 架构的最新版 Runode.app，装进 /Applications（不能写时装进 ~/Applications），
# 再把命令行 runode 和短名字 rn 链接到 bin 目录里。app 之后自己会自动更新，命令行跟着 app 走。
# Linux：没有桌面 app，下载只有命令行和终端宿主的 runode（`runode --host`、`runode remote pair`，
# 手机经远程访问连上来用），装进 bin 目录。
#
# 环境变量：
#   RUNODE_VERSION      装哪个版本，比如 0.3.0；不给时装最新版。
#   RUNODE_BIN_DIR      命令行装到哪；默认挑 PATH 上第一个能写的 ~/.local/bin、~/bin、/opt/homebrew/bin、/usr/local/bin，
#                       都不行时用 ~/.local/bin 并提醒加进 PATH。
#   RUNODE_APP_DIR      （macOS）Runode.app 装到哪；默认 /Applications。
#   RUNODE_LIB_DIR      （Linux）可执行文件放在哪，bin 目录里只放指向它的链接；默认 ~/.local/lib/runode。
set -eu

repo=runode-dev/runode

# 命令行默认装进已经在 PATH 上、又能写的 bin 目录，装完直接能敲 runode。
default_bin_dir() {
    for d in "$HOME/.local/bin" "$HOME/bin" /opt/homebrew/bin /usr/local/bin; do
        case ":$PATH:" in
            *":$d:"*) if [ -d "$d" ] && [ -w "$d" ]; then printf '%s' "$d"; return; fi ;;
        esac
    done
    printf '%s' "$HOME/.local/bin"
}
bin_dir=${RUNODE_BIN_DIR:-$(default_bin_dir)}

say() { printf '%s\n' "$*"; }
fail() {
    printf 'runode 安装失败：%s\n' "$*" >&2
    exit 1
}

# 发布页的下载地址：给了版本时是那个版本，否则是最新版。
download_base() {
    if [ -n "${RUNODE_VERSION:-}" ]; then
        printf 'https://github.com/%s/releases/download/v%s' "$repo" "${RUNODE_VERSION#v}"
    else
        printf 'https://github.com/%s/releases/latest/download' "$repo"
    fi
}

# 第三个参数给了就显示下载进度（装包用），清单这类小文件不显示。
fetch() {
    if command -v curl >/dev/null 2>&1; then
        if [ -n "${3:-}" ]; then curl -fL# --retry 3 -o "$2" "$1"; else curl -fsSL --retry 3 -o "$2" "$1"; fi
    elif command -v wget >/dev/null 2>&1; then
        if [ -n "${3:-}" ]; then wget -O "$2" "$1"; else wget -q -O "$2" "$1"; fi
    else
        fail "需要 curl 或 wget"
    fi
}

# 按同一个发布里的 SHA256SUMS 核对下载下来的文件 $1（发布里叫 $2），对不上就不装。
verify() {
    fetch "$(download_base)/SHA256SUMS" "$tmp/SHA256SUMS"
    expected=$(awk -v name="$2" '$2 == name || $2 == "*" name { print $1 }' "$tmp/SHA256SUMS")
    [ -n "$expected" ] || fail "校验和清单里没有 $2"
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$1" | cut -d ' ' -f 1)
    else
        actual=$(shasum -a 256 "$1" | cut -d ' ' -f 1)
    fi
    [ "$actual" = "$expected" ] || fail "$2 的校验和对不上，下载的文件可能不完整或被改过"
}

# 把命令行链接（或者放）进 bin 目录，不在 PATH 上时提醒一下。
link_cli() {
    mkdir -p "$bin_dir"
    ln -sfn "$1" "$bin_dir/runode"
    ln -sfn "$1" "$bin_dir/rn"
    case ":$PATH:" in
        *":$bin_dir:"*) ;;
        *) say "提示：$bin_dir 不在 PATH 上，把这一行加进 shell 的配置文件：export PATH=\"$bin_dir:\$PATH\"" ;;
    esac
}

install_macos() {
    # Rosetta 下 uname -m 也报 x86_64，按硬件判断，Apple 芯片一律装 arm64。
    if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ]; then arch=arm64; else arch=x86_64; fi
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    # 自动更新用的 zip 按版本号起名，地址从版本清单里读。
    say "正在查找 Runode 的 macOS 版（${arch}）……"
    fetch "$(download_base)/latest.json" "$tmp/latest.json"
    url=$(tr -d '\n' <"$tmp/latest.json" | grep -o "\"$arch\": *\"[^\"]*\"" | sed 's/.*"\([^"]*\)"$/\1/')
    [ -n "$url" ] || fail "版本清单里没有 $arch 的安装包"

    say "正在下载 $url"
    fetch "$url" "$tmp/Runode.zip" progress
    verify "$tmp/Runode.zip" "${url##*/}"
    # ditto 解压才保留符号链接和扩展属性，签名对得上。
    ditto -x -k "$tmp/Runode.zip" "$tmp"
    [ -d "$tmp/Runode.app" ] || fail "安装包里没有 Runode.app"

    app_dir=${RUNODE_APP_DIR:-/Applications}
    if ! { [ -d "$app_dir" ] && [ -w "$app_dir" ]; }; then
        [ -n "${RUNODE_APP_DIR:-}" ] && fail "$app_dir 不能写"
        app_dir=$HOME/Applications
        mkdir -p "$app_dir"
    fi
    if pgrep -xq runode 2>/dev/null; then
        say "提示：Runode 正在运行，装好后重开一次才换成新版本。"
    fi
    # 先挪开旧的再放新的：直接覆盖会把新旧两份文件混在一起。
    rm -rf "$app_dir/Runode.app.old"
    [ -e "$app_dir/Runode.app" ] && mv "$app_dir/Runode.app" "$app_dir/Runode.app.old"
    mv "$tmp/Runode.app" "$app_dir/Runode.app"
    rm -rf "$app_dir/Runode.app.old"

    link_cli "$app_dir/Runode.app/Contents/MacOS/runode"
    say "已装好 $app_dir/Runode.app，命令行在 $bin_dir/runode。打开 Runode：open -a Runode"
}

install_linux() {
    case $(uname -m) in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) fail "不支持这个架构：$(uname -m)" ;;
    esac
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT

    url="$(download_base)/runode-linux-$arch.tar.gz"
    say "正在下载 $url"
    fetch "$url" "$tmp/runode.tar.gz" progress
    verify "$tmp/runode.tar.gz" "runode-linux-${arch}.tar.gz"
    tar -xzf "$tmp/runode.tar.gz" -C "$tmp"
    [ -f "$tmp/runode" ] || fail "安装包里没有 runode"

    # 可执行文件放在 bin 目录旁边的 lib 目录里，bin 里只放链接，和 macOS 一样有 runode、rn 两个名字。
    lib_dir=${RUNODE_LIB_DIR:-$HOME/.local/lib/runode}
    mkdir -p "$lib_dir"
    # 先放成临时名再改名：正在跑的宿主还开着旧文件，直接覆盖会写坏它。
    install -m 755 "$tmp/runode" "$lib_dir/runode.new"
    mv -f "$lib_dir/runode.new" "$lib_dir/runode"

    link_cli "$lib_dir/runode"
    say "已装好 $bin_dir/runode。Linux 版没有桌面界面，用手机连上来：在 ~/.runode/config.conf 里写上"
    say "remote-access = true，后台跑起 runode --host，再用 runode remote pair 和手机配对。"
}

case $(uname -s) in
    Darwin) install_macos ;;
    Linux) install_linux ;;
    *) fail "不支持这个系统：$(uname -s)" ;;
esac
