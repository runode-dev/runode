{
  # 编译和测试要的工具都从这里来：rust-toolchain.toml 指定的 Rust（经 rust-overlay）、zig、cargo-nextest、
  # cargo-deny，和终端的真 shell 测试起的 bash、zsh、fish、elvish、nushell。用 `nix shell -c make test`，
  # 只往 PATH 前面加这些；只有 Xcode（Metal 编译器、macOS SDK、链接器）nix 给不了，仍用本机装的。
  # 不做成 devShell：`nix develop` 的 stdenv 会把 GNU coreutils、sed 等排在系统的前面，进程名、
  # 命令行为都跟着变（`yes` 成了 `coreutils`）。
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
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.buildEnv {
          name = "runode-tools";
          paths = with pkgs; [
            (rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
            # vendor/ghostty 的 build.zig.zon 要求的版本（minimum_zig_version）。
            zig_0_16
            cargo-nextest
            cargo-deny
            bashInteractive
            zsh
            fish
            elvish
            nushell
          ];
        };
      });
    };
}
