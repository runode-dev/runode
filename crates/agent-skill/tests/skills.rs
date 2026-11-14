//! 仓库里教 agent 用 runode 的 skill（`skills/`）：每个 skill 只有 `SKILL.md` 和 `references/<主题>.md`，
//! 元数据齐全，链到的 reference 都在，编进二进制的和盘上的一样。

use std::path::Path;

use runode_agent_skill::BUNDLED;

#[test]
fn bundled_skills_match_the_files_on_disk() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let mut on_disk = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().unwrap().to_string_lossy().starts_with('.') {
                continue;
            }
            if path.is_dir() {
                dirs.push(path);
            } else {
                let relative = path.strip_prefix(&root).unwrap();
                let relative: Vec<_> = relative.components().map(|part| part.as_os_str().to_string_lossy()).collect();
                on_disk.push((relative.join("/"), std::fs::read_to_string(&path).unwrap()));
            }
        }
    }
    on_disk.sort();
    let mut bundled: Vec<(String, String)> = BUNDLED
        .iter()
        .flat_map(|skill| {
            skill.files.iter().map(|(path, content)| (format!("{}/{path}", skill.name), content.to_string()))
        })
        .collect();
    bundled.sort();
    assert_eq!(bundled, on_disk);
    for skill in BUNDLED {
        assert_eq!(skill.files[0].0, "SKILL.md", "{}", skill.name);
    }
}

#[test]
fn every_skill_is_well_formed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let mut skills = 0;
    for entry in std::fs::read_dir(&root).unwrap() {
        let dir = entry.unwrap().path();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        // Finder 留下的 .DS_Store 这类隐藏文件不算。
        if name.starts_with('.') {
            continue;
        }
        assert!(dir.is_dir(), "skills/{name} is not a skill directory");
        skills += 1;
        let skill = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        assert!(skill.starts_with(&format!("---\nname: {name}\ndescription: ")), "{name}");
        // Codex 和 skill 规范都不认超过 1024 个字符的描述，整个 skill 不会被加载。
        let description = skill.lines().nth(2).unwrap().trim_start_matches("description: ");
        assert!(description.chars().count() <= 1024, "{name}: {}", description.chars().count());
        for entry in std::fs::read_dir(&dir).unwrap() {
            let file = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(file.starts_with('.') || file == "SKILL.md" || file == "references", "skills/{name}/{file}");
        }
        if let Ok(references) = std::fs::read_dir(dir.join("references")) {
            for entry in references {
                let path = entry.unwrap().path();
                let file = path.file_name().unwrap().to_string_lossy().into_owned();
                if !file.starts_with('.') {
                    assert!(file.ends_with(".md"), "skills/{name}/references/{file}");
                    assert!(
                        std::fs::read_to_string(&path).unwrap().starts_with("# "),
                        "skills/{name}/references/{file}"
                    );
                }
            }
        }
        // SKILL.md 让 agent 去读的 reference 都在。
        for (at, _) in skill.match_indices("](references/") {
            let link = &skill[at + 2..];
            let link = &link[..link.find(')').unwrap()];
            assert!(dir.join(link).is_file(), "skills/{name}/SKILL.md links to {link}");
        }
    }
    assert!(skills > 0);
}
