//! 构建期信息：把 Info.plist 嵌进可执行文件，让未打包成 .app 时也有应用元数据。

use std::{env, fs, path::PathBuf, process::Command};

const COPYRIGHT: &str = "Copyright (c) runode-dev 2026-present. All rights reserved.";

fn main() {
    // 当前提交变化时重新生成构建号。
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let commit = git_short_head().unwrap_or_else(|| "unknown".into());
    // 关于面板显示为「版本 0.1.0 (0.1.0.<commit>)」。
    let build = format!("{version}.{commit}");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = PathBuf::from(env::var("OUT_DIR").unwrap()).join("Info.plist");
        fs::write(&plist, info_plist(&version, &build)).unwrap();
        // 裸可执行文件的 NSBundle.mainBundle 会读 __TEXT,__info_plist 段，
        // 系统关于面板因此能拿到名称、版本、版权，并按 CFBundleAllowMixedLocalizations
        // 跟随系统语言显示「版本」等字样。
        println!(
            "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}

fn git_short_head() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn info_plist(version: &str, build: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>runode</string>
    <key>CFBundleDisplayName</key><string>runode</string>
    <key>CFBundleIdentifier</key><string>dev.runode.app</string>
    <key>CFBundleShortVersionString</key><string>{version}</string>
    <key>CFBundleVersion</key><string>{build}</string>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleAllowMixedLocalizations</key><true/>
    <key>NSHumanReadableCopyright</key><string>{COPYRIGHT}</string>
</dict>
</plist>
"#
    )
}
