CARGO ?= cargo
# 传给 runode 的命令行参数，例如 make run ARGS="--foo"。
ARGS ?=
# 追加给 clippy 的参数，例如 CI 里 make clippy CLIPPY_ARGS="-- -D warnings" 让警告算失败。
CLIPPY_ARGS ?=

.DEFAULT_GOAL := help
.PHONY: help submodules build release run run-release run-ios app install dmg check test clippy fmt fmt-check clean

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

# 模拟器用哪台，例如 make run-ios IOS_SIM="iPhone 18 Pro"；不给时用开着的那台。
run-ios: ## 调试构建 iOS app，装到模拟器上启动
	IOS_SIM="$(IOS_SIM)" apps/ios/scripts/run-simulator.sh

app: ## 发布构建并打包 Runode.app，产物在 target/release/bundle
	CARGO=$(CARGO) scripts/bundle-macos.sh app

install: app ## 打包 Runode.app 并装到 /Applications，覆盖旧版本
	rm -rf /Applications/Runode.app
	ditto target/release/bundle/Runode.app /Applications/Runode.app

dmg: ## 发布构建并打包 Runode.app 与 dmg，产物在 target/release/bundle
	CARGO=$(CARGO) scripts/bundle-macos.sh

check: ## 只做类型检查，不生成二进制
	$(CARGO) check --workspace --all-targets

# 装了 cargo-nextest 就用它：各测试二进制并行跑，cargo test 是一个跑完再跑下一个。nextest 不跑
# 文档测试，工作区里现在一个都没有；以后加了要在这里补上 `$(CARGO) test --workspace --doc`。
test: ## 运行测试（装了 cargo-nextest 时并行跑）
	@if $(CARGO) nextest --version >/dev/null 2>&1; then \
		$(CARGO) nextest run --workspace; \
	else \
		$(CARGO) test --workspace; \
	fi

clippy: ## 运行 clippy
	$(CARGO) clippy --workspace --all-targets $(CLIPPY_ARGS)

fmt: ## 按 rustfmt.toml 格式化工作区里的 crate（不碰 vendor）
	$(CARGO) fmt

fmt-check: ## 检查格式，有要改的就失败
	$(CARGO) fmt -- --check

clean: ## 清理构建产物
	$(CARGO) clean
