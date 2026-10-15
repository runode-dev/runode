{
  # 编译和测试要的工具都从这里来：rust-toolchain.toml 指定的 Rust（经 rust-overlay）、zig、cargo-nextest、
  # cargo-deny、git，和终端的真 shell 测试起的 bash、zsh、fish、elvish、nushell。
  #
  # macOS 上用 `nix shell -c make test`，只往 PATH 前面加这些；只有 Xcode（Metal 编译器、macOS SDK、
  # 链接器）nix 给不了，仍用本机装的。不做成 devShell：`nix develop` 的 stdenv 会把 GNU coreutils、sed 等
  # 排在系统的前面，进程名、命令行为都跟着变（`yes` 成了 `coreutils`）。
  #
  # Linux 上用 `nix develop -c make test`：GPUI 要链的 xkbcommon、wayland、X11、fontconfig、vulkan 这些
  # 库只有经 devShell 的 pkg-config 才找得到（`nix shell` 只加 PATH，不设 PKG_CONFIG_PATH），C 编译器和
  # 链接器也由 stdenv 给；Linux 的系统工具本来就是 GNU 的，没有上面那个顾虑。库的清单和 CI 在 Ubuntu 上
  # apt 装的那组一样。
  description = "runode 编译和测试用的工具";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAll =
        f:
        nixpkgs.lib.genAttrs systems (
          system:
          f (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
      # 各平台都要的工具。
      tools =
        pkgs:
        with pkgs;
        [
          (rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
          # vendor/ghostty 的 build.zig.zon 要求的版本（minimum_zig_version）。
          zig_0_16
          cargo-nextest
          cargo-deny
          # git 的测试和宿主读仓库都起 git 命令行。
          git
          bashInteractive
          zsh
          fish
          elvish
          nushell
        ];
      # GPUI 在 Linux 上要链的库，和 ci.yml 里 check-linux 用 apt 装的一组对应。
      linuxLibs =
        pkgs:
        with pkgs;
        [
          libxkbcommon
          wayland
          xorg.libX11
          xorg.libxcb
          fontconfig
          freetype
          vulkan-loader
          openssl
          alsa-lib
          zstd
        ];
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.buildEnv {
          name = "runode-tools";
          paths = tools pkgs;
        };
      });

      devShells = forAll (
        pkgs:
        nixpkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          default = pkgs.mkShell {
            # 宿主的测试用 pgrep 确认 shell 退了没有。
            packages = tools pkgs ++ [ pkgs.procps ];
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = linuxLibs pkgs;
          };
        }
      );
    };
}
