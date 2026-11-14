//! 把仓库根目录 `skills/` 下的 skill 全部编进二进制（`BUNDLED`）：加 skill 或 reference 只加文件，不改代码。

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

/// 生成按名字排序的 `Skill` 表，每个 skill 的文件 `SKILL.md` 在前、reference 按名字排在后面。
/// skill 下只能有 `SKILL.md` 和 `references/<主题>.md`，别的文件让构建失败。
fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    // 指向目录时，目录里（连同子目录）任何文件变动都会触发重新生成。
    println!("cargo::rerun-if-changed={}", root.display());

    let mut out = String::from("&[\n");
    for (name, dir) in visible(&root) {
        assert!(dir.is_dir(), "skills/{name} is not a skill directory");
        let mut files = Vec::new();
        for (file, path) in visible(&dir) {
            match file.as_str() {
                "SKILL.md" if path.is_file() => files.insert(0, ("SKILL.md".to_owned(), path)),
                "references" if path.is_dir() => {
                    for (reference, path) in visible(&path) {
                        assert!(
                            reference.ends_with(".md") && path.is_file(),
                            "skills/{name}/references/{reference}: a skill may only hold SKILL.md and references/*.md"
                        );
                        files.push((format!("references/{reference}"), path));
                    }
                }
                _ => panic!("skills/{name}/{file}: a skill may only hold SKILL.md and references/*.md"),
            }
        }
        assert!(files.first().is_some_and(|(file, _)| file == "SKILL.md"), "skills/{name} has no SKILL.md");
        writeln!(out, "    Skill {{ name: {name:?}, files: &[").unwrap();
        for (file, path) in files {
            let path = path.canonicalize().unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            writeln!(out, "        ({file:?}, include_str!({:?})),", path.display()).unwrap();
        }
        writeln!(out, "    ] }},").unwrap();
    }
    out.push(']');
    let generated = Path::new(&std::env::var("OUT_DIR").unwrap()).join("skills.rs");
    fs::write(generated, out).unwrap();
}

/// `dir` 下按名字排序的条目，隐藏文件（Finder 留下的 `.DS_Store` 这类）不算。
fn visible(dir: &Path) -> Vec<(String, PathBuf)> {
    let entries = fs::read_dir(dir).unwrap_or_else(|err| panic!("{}: {err}", dir.display()));
    let mut entries: Vec<(String, PathBuf)> = entries
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name().into_string().unwrap(), entry.path())
        })
        .filter(|(name, _)| !name.starts_with('.'))
        .collect();
    entries.sort();
    entries
}
