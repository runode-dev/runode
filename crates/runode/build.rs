//! 把内嵌的 ghostty commit 编进程序，供「关于」面板显示。

use std::process::Command;

fn main() {
    let ghostty = "../../vendor/ghostty";
    // 子模块的 HEAD 存在父仓库的 .git/modules 下；切换子模块提交时它会变。
    println!("cargo:rerun-if-changed=../../.git/modules/vendor/ghostty/HEAD");
    let commit = Command::new("git")
        .args(["-C", ghostty, "rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=RUNODE_GHOSTTY_COMMIT={commit}");
}
