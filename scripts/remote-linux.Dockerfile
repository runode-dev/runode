# 交叉编译 SSH 远端用的 runode 时，Linux 版在这个镜像里编：Ubuntu 22.04（glibc 2.35），编出的程序在 Ubuntu 22.04、
# Debian 12 以后的发行版上跑。按 docker 的原生架构起，另一种架构用 Ubuntu 的 crossbuild 工具链交叉编：sherpa-onnx
# 的预编译静态库按 libstdc++ 编，只能用 gcc 链接。
FROM ubuntu:22.04
ARG RUST_VERSION
ARG ZIG_VERSION
ARG ZIG_ARCH
ARG CROSS_ARCH
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get -o Acquire::Retries=5 update \
    && apt-get -o Acquire::Retries=5 install -y --no-install-recommends \
       build-essential "crossbuild-essential-$CROSS_ARCH" ca-certificates curl git xz-utils \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/opt/zig:$PATH
# 组件照 rust-toolchain.toml 装齐：容器用完就删，缺的组件 rustup 每次起来都要现下。
RUN curl -sSf --retry 3 https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path \
       --default-toolchain "$RUST_VERSION" --component llvm-tools \
       --target x86_64-unknown-linux-gnu,aarch64-unknown-linux-gnu
RUN curl -sSfL --retry 3 "https://ziglang.org/download/$ZIG_VERSION/zig-$ZIG_ARCH-linux-$ZIG_VERSION.tar.xz" | tar -xJ -C /opt \
    && mv "/opt/zig-$ZIG_ARCH-linux-$ZIG_VERSION" /opt/zig
# 仓库从 Mac 上挂进来，属主和容器里的用户不同；构建脚本要用 git 读提交号。
RUN git config --global --add safe.directory '*'
