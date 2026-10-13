{
  # 只补系统里不一定有的 shell：终端的真 shell 测试起 bash、zsh、fish、elvish、nushell。用
  # `nix shell -c make test`，只往 PATH 前面加这几个 shell；Rust、zig 和 Xcode 照旧用自己装的。
  # 不做成 devShell：`nix develop` 的 stdenv 会把 GNU coreutils、sed 等排在系统的前面，进程名、
  # 命令行为都跟着变（`yes` 成了 `coreutils`）。
  description = "runode 测试用的 shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.buildEnv {
          name = "runode-shells";
          paths = with pkgs; [ bashInteractive zsh fish elvish nushell ];
        };
      });
    };
}
