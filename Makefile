CARGO ?= cargo
# 传给 runode 的命令行参数，例如 make run ARGS="--foo"。
ARGS ?=
# 追加给 clippy 的参数，例如 CI 里 make clippy CLIPPY_ARGS="-- -D warnings" 让警告算失败。
CLIPPY_ARGS ?=

.DEFAULT_GOAL := help
.PHONY: help submodules build release run run-release app install dmg check test clippy fmt fmt-check clean

help: ## 列出所有目标
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  %-12s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

submodules: ## 拉取 vendor 下的 ghostty、libghostty-rs 与 command-signatures 子模块
	git submodule update --init --recursive

build: ## 调试构建
	$(CARGO) build

release: ## 发布构建
	$(CARGO) build --release

run: ## 调试构建并启动
	$(CARGO) run -- $(ARGS)

run-release: ## 发布构建并启动
	$(CARGO) run --release -- $(ARGS)

app: ## 发布构建并打包 Runode.app，产物在 target/release/bundle
	CARGO=$(CARGO) scripts/bundle-macos.sh app

install: app ## 打包 Runode.app 并装到 /Applications，覆盖旧版本
	rm -rf /Applications/Runode.app
	ditto target/release/bundle/Runode.app /Applications/Runode.app

dmg: ## 发布构建并打包 Runode.app 与 dmg，产物在 target/release/bundle
	CARGO=$(CARGO) scripts/bundle-macos.sh

check: ## 只做类型检查，不生成二进制
	$(CARGO) check --workspace --all-targets

test: ## 运行测试
	$(CARGO) test --workspace

clippy: ## 运行 clippy
	$(CARGO) clippy --workspace --all-targets $(CLIPPY_ARGS)

fmt: ## 按 rustfmt.toml 格式化工作区里的 crate（不碰 vendor）
	$(CARGO) fmt

fmt-check: ## 检查格式，有要改的就失败
	$(CARGO) fmt -- --check

clean: ## 清理构建产物
	$(CARGO) clean
