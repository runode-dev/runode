CARGO ?= cargo
# 传给 runode 的命令行参数，例如 make run ARGS="--foo"。
ARGS ?=
# 追加给 clippy 的参数，例如 CI 里 make clippy CLIPPY_ARGS="-- -D warnings" 让警告算失败。
CLIPPY_ARGS ?=
# app 把可执行文件所在的目录加进终端的 PATH，开发版里也要能敲 rn。
# Windows 用独立的控制台程序 rn.exe；cargo run 只编默认的 runode，要先编好 rn。
ifeq ($(OS),Windows_NT)
prepare_run = $(CARGO) build -p runode --bin rn $(if $(filter release,$(1)),--release)
else
# Unix 用符号链接；打包时 scripts/bundle-macos.sh 在 Runode.app 里放同样的链接。
link_rn = @mkdir -p target/$(1) && ln -sfn runode target/$(1)/rn
prepare_run = $(call link_rn,$(1))
endif

.DEFAULT_GOAL := help
.PHONY: help submodules build release run run-release run-ios run-ios-device app install restart dmg check test clippy fmt fmt-check clean

help: ## 列出所有目标
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  %-16s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

submodules: ## 拉取 vendor 下的 ghostty 与 libghostty-rs 子模块
	git submodule update --init --recursive

build: ## 调试构建
	$(call link_rn,debug)
	$(CARGO) build

release: ## 发布构建
	$(call link_rn,release)
	$(CARGO) build --release

run: ## 调试构建并启动
	$(call prepare_run,debug)
	$(CARGO) run -p runode -- $(ARGS)

run-release: ## 发布构建并启动
	$(call prepare_run,release)
	$(CARGO) run --release -p runode -- $(ARGS)

# 模拟器用哪台，例如 make run-ios IOS_SIM="iPhone 18 Pro"；不给时用开着的那台。
run-ios: ## 调试构建 iOS app，装到模拟器上启动
	IOS_SIM="$(IOS_SIM)" apps/ios/scripts/run-simulator.sh

# 真机用哪台，例如 make run-ios-device IOS_DEVICE="我的 iPhone"；不给时用连着的那台 iPhone。
run-ios-device: ## Release 构建 iOS app，装到连着的真机上启动
	IOS_DEVICE="$(IOS_DEVICE)" apps/ios/scripts/run-device.sh

# 本机自己用的包按 Cargo.toml 里的 local profile 编，不开 LTO，改一点代码后重编快得多；
# make install APP_PROFILE=release 装和发给用户一样的构建。
APP_PROFILE ?= local

app: ## 构建并打包 Runode.app（local profile，编得快），产物在 target/local/bundle
	CARGO=$(CARGO) BUILD_PROFILE=$(APP_PROFILE) scripts/bundle-macos.sh app

# 先拷到 /Applications 里的临时名，拷成了再和旧的换：拷到一半失败（磁盘满、被打断）时旧的还在。
# 上次在两次 mv 之间断了时只剩 .Runode.app.old，先把它挪回去再删临时名；换新的没换成时也挪回去。
install: app ## 打包 Runode.app 并装到 /Applications，覆盖旧版本
	if [ ! -e /Applications/Runode.app ] && [ -e /Applications/.Runode.app.old ]; then \
		mv /Applications/.Runode.app.old /Applications/Runode.app; \
	fi
	rm -rf /Applications/.Runode.app.new /Applications/.Runode.app.old
	ditto target/$(APP_PROFILE)/bundle/Runode.app /Applications/.Runode.app.new
	if [ -e /Applications/Runode.app ]; then mv /Applications/Runode.app /Applications/.Runode.app.old || exit 1; fi; \
	if ! mv /Applications/.Runode.app.new /Applications/Runode.app; then \
		if [ -e /Applications/.Runode.app.old ]; then mv /Applications/.Runode.app.old /Applications/Runode.app; fi; \
		exit 1; \
	fi
	rm -rf /Applications/.Runode.app.old

# 重开 app 时它只在宿主的构建号不同时才换宿主，还要先等 app 退出；用这个一步让新宿主接手会话。
restart: ## 重启装好的 Runode.app，宿主也换成新装的（make install restart 装好再重启）
	scripts/restart-macos.sh

# `make install restart` 带着 -j 时也先装好再重启；只 `make restart` 时不重新打包。
ifneq ($(filter install,$(MAKECMDGOALS)),)
restart: install
endif

dmg: ## 发布构建并打包 Runode.app、dmg 和自动更新用的 zip，产物在 target/release/bundle
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
