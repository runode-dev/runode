//! 构建期信息：把应用信息表填上版本号后嵌进可执行文件，让未打包成 .app 时也有应用元数据，
//! 打包 .app 时也直接用这份表。

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    // 当前提交变化时重新生成构建号。路径交给 git 解析：工作树里 `.git` 是个文件，HEAD 在主仓库的
    // `worktrees/<名字>/` 下，手拼的路径不存在，cargo 会把它当成永远过期、每次都重编。
    for name in ["HEAD", "refs/heads"] {
        if let Some(path) = git_path(name) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    // 打包脚本在工作区有没提交的改动时设它（`dirty.<改动的摘要>`）：不然改了代码没提交就重新打包，
    // 构建号和在跑的宿主一样，新宿主接不了手（同构建的接手请求宿主一律回 `Busy`）。
    println!("cargo:rerun-if-env-changed=RUNODE_BUILD_TAG");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let commit = git_short_head().unwrap_or_else(|| "unknown".into());
    // 构建号是「0.1.0.<commit>」，带改动时是「0.1.0.<commit>-dirty.<摘要>」；关于面板的括号里只显示版本号后面那段。
    let revision = match env::var("RUNODE_BUILD_TAG") {
        Ok(tag) if !tag.is_empty() => format!("{commit}-{tag}"),
        _ => commit,
    };
    let build = format!("{version}.{revision}");
    println!("cargo:rustc-env=RUNODE_REVISION={revision}");
    // 宿主在握手时报这个构建号，前端的一样才给快照。
    println!("cargo:rustc-env=RUNODE_BUILD={build}");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // 模板里的 @VERSION@、@BUILD@ 换成实际版本号与构建号。
        println!("cargo:rerun-if-changed=Info.plist");
        let template = fs::read_to_string("Info.plist").unwrap();
        let plist = PathBuf::from(env::var("OUT_DIR").unwrap()).join("Info.plist");
        fs::write(&plist, template.replace("@VERSION@", &version).replace("@BUILD@", &build)).unwrap();
        // 裸可执行文件的 NSBundle.mainBundle 会读 __TEXT,__info_plist 段，
        // 系统关于面板因此能拿到名称、版本、版权，并按 CFBundleAllowMixedLocalizations
        // 跟随系统语言显示「版本」等字样。
        println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}", plist.display());
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // GPUI 拿编号为 1 的图标资源当窗口和任务栏的图标，资源管理器里的程序图标也是它。
        println!("cargo:rerun-if-changed=runode.rc");
        println!("cargo:rerun-if-changed=assets/compiled/runode.ico");
        embed_resource::compile("runode.rc", embed_resource::NONE).manifest_required().unwrap();
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

/// 仓库里 `name`（`HEAD`、`refs/heads`）实际所在的路径，主仓库和工作树里都对；不在仓库里时为 `None`。
fn git_path(name: &str) -> Option<String> {
    let out =
        Command::new("git").args(["rev-parse", "--git-path", name]).output().ok().filter(|out| out.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}
