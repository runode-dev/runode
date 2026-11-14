#!/usr/bin/env python3
"""核对 AGENTS.md 不变量里「只依赖谁」那几条：cargo metadata 列出的 runode crate 之间的依赖（连同开发依赖）
不能超出下面这张表。谁能依赖某个 crate 那一类规则在 deny.toml 里，由 `cargo deny check bans` 守。"""

import json
import subprocess
import sys

# crate 名 -> 它能直接依赖的 runode crate。
ONLY = {
    "runode-types": set(),
    "runode-paths": set(),
    "runode-git": set(),
    "runode-files": set(),
    "runode-preview": set(),
    "runode-update": set(),
    "runode-protocol": {"runode-types"},
    "runode-agent": {"runode-types"},
    "runode-android-tunnel": set(),
}


def main() -> int:
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"], check=True, capture_output=True, text=True
    ).stdout
    packages = {package["name"]: package for package in json.loads(out)["packages"]}
    missing = sorted(set(ONLY) - set(packages))
    if missing:
        print(f"check-layers: no such crate in the workspace: {', '.join(missing)}", file=sys.stderr)
        return 1
    failed = False
    for name, allowed in ONLY.items():
        used = {dep["name"] for dep in packages[name]["dependencies"] if dep["name"].startswith("runode")}
        for dep in sorted(used - allowed):
            print(f"check-layers: {name} must not depend on {dep}", file=sys.stderr)
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
