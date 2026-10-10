//! 把仓库里 `skills/files.txt` 列的 skill 文件编进二进制（`setup` 的 `FILES`）：加 skill 或 reference
//! 只改 `skills/` 下的文件和清单，不改代码。

use std::{fmt::Write as _, path::Path};

fn main() {
    let skills = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let list = skills.join("files.txt");
    println!("cargo::rerun-if-changed={}", list.display());
    let list = std::fs::read_to_string(&list).expect("skills/files.txt lists the skill files");
    let mut out = String::from("&[\n");
    for path in list.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let file = skills.join(path).canonicalize().unwrap_or_else(|err| panic!("skills/{path}: {err}"));
        writeln!(out, "    ({path:?}, include_str!({:?})),", file.display()).unwrap();
    }
    out.push(']');
    let generated = Path::new(&std::env::var("OUT_DIR").unwrap()).join("skill_files.rs");
    std::fs::write(generated, out).unwrap();
}
