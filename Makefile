CARGO ?= cargo
# 传给 runode 的命令行参数，例如 make run ARGS="--foo"。
ARGS ?=
# 追加给 clippy 的参数，例如 CI 里 make clippy CLIPPY_ARGS="-- -D warnings" 让警告算失败。
CLIPPY_ARGS ?=
# 命令行的短名字：可执行文件旁边一个叫 rn 的符号链接。app 把可执行文件所在的目录加进终端的
# PATH，开发版里也能敲 rn；打包时 scripts/bundle-macos.sh 在 Runode.app 里放同样的链接。
link_rn = @mkdir -p target/$(1) && ln -sfn runode target/$(1)/rn

.DEFAULT_GOAL := help
.PHONY: help submodules build release run run-release run-ios run-ios-device app install dmg check test clippy fmt fmt-check clean

help: ## 列出所有目标
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  %-16s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

submodules: ## 拉取 vendor 下的 ghostty、libghostty-rs 与 command-signatures 子模块
	git submodule update --init --recursive

build: ## 调试构建
	$(call link_rn,debug)
	$(CARGO) build

release: ## 发布构建
	$(call link_rn,release)
	$(CARGO) build --release

run: ## 调试构建并启动
	$(call link_rn,debug)
	$(CARGO) run -- $(ARGS)

run-release: ## 发布构建并启动
	$(call link_rn,release)
	$(CARGO) run --release -- $(ARGS)

# 模拟器用哪台，例如 make run-ios IOS_SIM="iPhone 18 Pro"；不给时用开着的那台。
run-ios: ## 调试构建 iOS app，装到模拟器上启动
	IOS_SIM="$(IOS_SIM)" apps/ios/scripts/run-simulator.sh

# 真机用哪台，例如 make run-ios-device IOS_DEVICE="我的 iPhone"；不给时用连着的那台 iPhone。
run-ios-device: ## 调试构建 iOS app，装到连着的真机上启动
	IOS_DEVICE="$(IOS_DEVICE)" apps/ios/scripts/run-device.sh

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
