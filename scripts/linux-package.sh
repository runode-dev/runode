#!/bin/bash
# 把带界面的 Linux 版 runode（开了 linux-desktop 特性）打成 AppImage、deb 和 rpm，发版时用：
#
#   scripts/linux-package.sh <版本> <x86_64|aarch64> <runode 可执行文件> <输出目录>
#
# 三个包里装的是同一个可执行文件、同一份应用菜单项和图标。AppImage 自己更新
# （`runode_update` 的 `linux`），deb、rpm 交给系统的包管理器升级；装 deb、rpm 时 rn 是指向 runode 的链接。
# 打包工具按下面钉住的版本下载，核对过校验和才用；要在同架构的机器上跑（appimagetool 是这个架构的程序）。
set -euo pipefail

[[ $# == 4 ]] || { echo "用法：$0 <版本> <x86_64|aarch64> <runode 可执行文件> <输出目录>" >&2; exit 2; }
version=$1
arch=$2
binary=$3
out=$4
root=$(cd "$(dirname "$0")/.." && pwd)
id=dev.runode.app

appimagetool_version=1.9.1
runtime_version=20251108
nfpm_version=2.48.0
case $arch in
    x86_64)
        appimagetool_sum=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
        runtime_sum=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
        nfpm_arch=x86_64
        nfpm_sum=6d7f9a28b2f397934403950fcbba83f598b08d02edbe79a0815b3b4897341750
        package_arch=amd64
        ;;
    aarch64)
        appimagetool_sum=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
        runtime_sum=00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444
        nfpm_arch=arm64
        nfpm_sum=a9a75213763783b7669719e505474c748c5ad7dc66591a195619deb50f85c2f4
        package_arch=arm64
        ;;
    *) echo "不认识的架构：$arch" >&2; exit 2 ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

# 下载 $1 到 $2，SHA-256 要是 $3。
fetch() {
    curl -fsSL --retry 3 -o "$2" "$1"
    echo "$3  $2" | sha256sum -c --quiet -
}

fetch "https://github.com/AppImage/appimagetool/releases/download/$appimagetool_version/appimagetool-$arch.AppImage" \
    "$work/appimagetool" "$appimagetool_sum"
fetch "https://github.com/AppImage/type2-runtime/releases/download/$runtime_version/runtime-$arch" \
    "$work/runtime" "$runtime_sum"
fetch "https://github.com/goreleaser/nfpm/releases/download/v$nfpm_version/nfpm_${nfpm_version}_Linux_$nfpm_arch.tar.gz" \
    "$work/nfpm.tar.gz" "$nfpm_sum"
chmod +x "$work/appimagetool"
tar -xzf "$work/nfpm.tar.gz" -C "$work" nfpm

# 编进可执行文件的 agent 识别规则和语法定义要求附带许可声明。
docs=$work/doc
mkdir -p "$docs"
cp "$root/LICENSE" "$docs/LICENSE"
cp "$root/crates/agent/LICENSE-rules" "$docs/agent-rules-LICENSE.txt"
cp "$root/crates/preview/LICENSE-syntaxes" "$docs/syntaxes-LICENSE.txt"
assets=$root/apps/desktop/assets

# AppImage：AppRun 直接是可执行文件的链接，运行时把镜像挂上后执行它，`APPDIR`、`APPIMAGE` 由运行时设。
app=$work/AppDir
mkdir -p "$app/usr/bin" "$app/usr/share/doc/runode"
install -m 755 "$binary" "$app/usr/bin/runode"
ln -s usr/bin/runode "$app/AppRun"
cp "$assets/linux/$id.desktop" "$app/$id.desktop"
cp "$assets/icon.png" "$app/$id.png"
ln -s "$id.png" "$app/.DirIcon"
cp "$docs"/* "$app/usr/share/doc/runode/"
# appimagetool 自己也是 AppImage，在没有 FUSE 的容器和 runner 上要解开了跑。
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=$arch "$work/appimagetool" --no-appstream --comp zstd \
    --runtime-file "$work/runtime" "$app" "$out/Runode-$version-linux-$arch.AppImage"

# deb 和 rpm：依赖是可执行文件启动时就要链接的库（`ldd` 列出的，glibc 和 libstdc++ 除外），加上运行时 dlopen 的
# Vulkan 加载器（GPUI 用它画界面）；没有显卡驱动的机器靠 Mesa 的软件 Vulkan，列成推荐。两边的包名不同。
cat >"$work/nfpm.yaml" <<EOF
name: runode
arch: $package_arch
platform: linux
version: $version
# 仓库没有对外的联系邮箱，维护者照 Windows 安装包的发布者写，联系方式给项目主页。
maintainer: Runode <https://github.com/runode-dev/runode>
description: A terminal built for AI coding agents
vendor: Runode
homepage: https://github.com/runode-dev/runode
license: Apache-2.0
section: x11
priority: optional
contents:
  - src: $binary
    dst: /usr/bin/runode
    file_info:
      mode: 0755
  - src: runode
    dst: /usr/bin/rn
    type: symlink
  - src: $assets/linux/$id.desktop
    dst: /usr/share/applications/$id.desktop
  - src: $assets/icon.png
    dst: /usr/share/icons/hicolor/256x256/apps/$id.png
  - src: $assets/icon.svg
    dst: /usr/share/icons/hicolor/scalable/apps/$id.svg
  - src: $docs/LICENSE
    dst: /usr/share/doc/runode/LICENSE
  - src: $docs/agent-rules-LICENSE.txt
    dst: /usr/share/doc/runode/agent-rules-LICENSE.txt
  - src: $docs/syntaxes-LICENSE.txt
    dst: /usr/share/doc/runode/syntaxes-LICENSE.txt
overrides:
  deb:
    depends:
      - libxcb1
      - libxkbcommon0
      - libxkbcommon-x11-0
      - libasound2 | libasound2t64
      - libvulkan1
    recommends:
      - mesa-vulkan-drivers
  rpm:
    depends:
      - libxcb
      - libxkbcommon
      - libxkbcommon-x11
      - alsa-lib
      - vulkan-loader
    recommends:
      - mesa-vulkan-drivers
EOF
"$work/nfpm" package --config "$work/nfpm.yaml" --packager deb --target "$out/Runode-$version-linux-$arch.deb"
"$work/nfpm" package --config "$work/nfpm.yaml" --packager rpm --target "$out/Runode-$version-linux-$arch.rpm"
ls -l "$out"
