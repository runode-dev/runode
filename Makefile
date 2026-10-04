CARGO ?= cargo
# 传给 runode 的命令行参数，例如 make run ARGS="--foo"。
ARGS ?=

.DEFAULT_GOAL := help
.PHONY: help submodules build release run run-release check test clippy clean

help: ## 列出所有目标
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  %-12s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

submodules: ## 拉取 vendor 下的 ghostty 与 libghostty-rs 子模块
	git submodule update --init --recursive

build: ## 调试构建
	$(CARGO) build

release: ## 发布构建
	$(CARGO) build --release

run: ## 调试构建并启动
	$(CARGO) run -- $(ARGS)

run-release: ## 发布构建并启动
	$(CARGO) run --release -- $(ARGS)

check: ## 只做类型检查，不生成二进制
	$(CARGO) check --all-targets

test: ## 运行测试
	$(CARGO) test

clippy: ## 运行 clippy
	$(CARGO) clippy --all-targets

clean: ## 清理构建产物
	$(CARGO) clean
