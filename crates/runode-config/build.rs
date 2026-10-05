//! 构建期把内置的配色主题编进二进制。

use std::{env, fs, path::PathBuf};

fn main() {
    bundle_themes();
}

/// 生成按名字排序的 `(名字, 内容)` 表，供 `BUNDLED_THEMES` 使用。
fn bundle_themes() {
    let dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("themes");
    // 指向目录时，目录里任何文件变动都会触发重新生成。
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        // 隐藏文件（如 Finder 生成的 .DS_Store）不是主题，二进制内容也进不了 include_str!。
        .filter(|name| name != "LICENSE" && !name.starts_with('.'))
        .collect();
    names.sort();

    let mut out = String::from("&[\n");
    for name in &names {
        out += &format!("    ({name:?}, include_str!({:?})),\n", dir.join(name));
    }
    out += "]\n";
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out_dir.join("themes.rs"), out).unwrap();
}
