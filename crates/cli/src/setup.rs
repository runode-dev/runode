//! 教 agent 用 runode：把使用说明装成 agent 按需加载的 skill，一份讲操作别的终端，一份讲在模拟器页
//! 看着的模拟器上跑和测 app。Claude Code 读 `~/.claude/skills/<名字>/`，Codex 读
//! `~/.agents/skills/<名字>/`，两边是同样的文件：`SKILL.md`，加上它按需让 agent 去读的
//! `references/<主题>.md`。
//! 装的是 GitHub 上仓库 main 里最新的一套（`SKILLS_URL`）：先下文件清单 `files.txt`，再照着下每个
//! 文件，只改 skill 或加文件都不用发新版 app；下不到时用编进这一版的 `FILES`。
//! 早先给 Codex 装在 `~/.codex/AGENTS.md` 的 `<!-- runode:begin -->`、`<!-- runode:end -->` 之间，
//! 再装时把这一段删掉，文件里别的内容不动。

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result};

/// 给哪个 agent 装。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupTarget {
    Claude,
    Codex,
}

/// 一套 skill 文件：相对 skill 目录的路径（`<名字>/SKILL.md`、`<名字>/references/<主题>.md`）和内容。
/// `SKILL.md` 开头是 skill 的元数据（名字和什么时候用）；两个 agent 装的都是这些。
pub type SkillFiles = Vec<(String, String)>;

/// 编进这一版的 skill 文件，构建时照仓库里的清单 `skills/files.txt` 生成，顺序也一样。
pub(crate) const FILES: &[(&str, &str)] = include!(concat!(env!("OUT_DIR"), "/skill_files.rs"));
/// 仓库里放 skill 的目录在 GitHub 上的原始文件地址，下面是 `files.txt` 和清单里的各个文件。
pub const SKILLS_URL: &str = "https://raw.githubusercontent.com/runode-dev/runode/main/skills";
/// 下回来的一个文件最多这么大，再大就不是 skill。
const MAX_FILE_BYTES: usize = 256 * 1024;
/// 清单最多列这么多文件。
const MAX_FILES: usize = 64;
const BEGIN: &str = "<!-- runode:begin -->";
const END: &str = "<!-- runode:end -->";

fn skills_dir(target: SetupTarget, home: &Path) -> PathBuf {
    match target {
        SetupTarget::Claude => home.join(".claude/skills"),
        SetupTarget::Codex => home.join(".agents/skills"),
    }
}

/// 这一版往 `home` 下给 `target` 装哪些文件，和 `FILES` 一一对应；下到的清单可能多几个。
pub fn setup_paths(target: SetupTarget, home: &Path) -> Vec<PathBuf> {
    let dir = skills_dir(target, home);
    FILES.iter().map(|(path, _)| dir.join(path)).collect()
}

/// 编进这一版的 skill 文件。
pub fn bundled_skills() -> SkillFiles {
    FILES.iter().map(|(path, content)| ((*path).to_owned(), (*content).to_owned())).collect()
}

/// 从 `url`（通常是 `SKILLS_URL`）下最新的一套 skill 文件：先下清单 `files.txt`，再下清单里的每个。
/// 清单里有一个路径不对、一个文件下不到或不像 skill，就整套不用，返回 `None`，免得装上新旧混着的
/// 一套。外调 curl，每个文件最多等十秒。
pub fn fetch_skills(url: &str) -> Option<SkillFiles> {
    let list = String::from_utf8(download(&format!("{url}/files.txt"))?).ok()?;
    let paths: Vec<&str> = list.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    if paths.is_empty() || paths.len() > MAX_FILES || !paths.iter().all(|path| is_skill_path(path)) {
        return None;
    }
    paths
        .into_iter()
        .map(|path| Some((path.to_owned(), as_skill_file(path, download(&format!("{url}/{path}"))?)?)))
        .collect()
}

fn download(url: &str) -> Option<Vec<u8>> {
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--location", "--max-time", "10", "--proto", "=https", "--proto-redir", "=https"])
        .arg(url)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// 清单里的路径只能是 `<名字>/SKILL.md` 或 `<名字>/references/<主题>.md`，名字和主题只用小写字母、
/// 数字和 `-`：下回来的清单不能让文件写到 skill 目录外面去。
fn is_skill_path(path: &str) -> bool {
    let plain = |part: &str| {
        !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    match path.split('/').collect::<Vec<_>>()[..] {
        [name, "SKILL.md"] => plain(name),
        [name, "references", file] => plain(name) && file.strip_suffix(".md").is_some_and(plain),
        _ => false,
    }
}

/// 下回来的 `body` 像不像清单说的那个文件：不太大、是 UTF-8；`SKILL.md` 开头是这个名字的元数据，
/// reference 开头是标题。挡住错误页面和放错地方的文件。
fn as_skill_file(path: &str, body: Vec<u8>) -> Option<String> {
    let text = String::from_utf8(body).ok().filter(|text| text.len() <= MAX_FILE_BYTES)?;
    let looks_right = match path.strip_suffix("/SKILL.md") {
        Some(name) => text.starts_with(&format!("---\nname: {name}\n")),
        None => text.starts_with("# "),
    };
    looks_right.then_some(text)
}

/// 把 `files`（`fetch_skills` 或 `bundled_skills` 给的）装到 `home` 下 `target` 读的地方，返回写的
/// 文件。可以重复执行，每次整个换掉；给 Codex 装时顺带删掉早先写进 `~/.codex/AGENTS.md` 的那段。
/// shortcut: 清单里去掉的文件留在用户那里，没有 SKILL.md 指着就不会被读到；要是哪天会误导 agent，再
/// 在装之前清掉 `references/` 里清单外的文件。
pub fn setup(target: SetupTarget, home: &Path, files: &[(String, String)]) -> Result<Vec<PathBuf>> {
    let dir = skills_dir(target, home);
    let mut written = Vec::new();
    for (path, content) in files {
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
        }
        runode_paths::replace_file(&path, content.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        written.push(path);
    }
    if target == SetupTarget::Codex {
        let agents = home.join(".codex/AGENTS.md");
        match std::fs::read_to_string(&agents) {
            Ok(existing) => {
                if let Some(rest) = remove_section(&existing) {
                    runode_paths::replace_file(&agents, rest.as_bytes())
                        .with_context(|| format!("failed to write {}", agents.display()))?;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("failed to read {}", agents.display())),
        }
    }
    Ok(written)
}

/// 给 `target` 装过使用说明（`setup_paths` 或 `files` 里有文件在），又和 `files` 不一样或缺了文件时
/// 重装，返回重装了没有。app 启动时拿 `fetch_skills` 下到的用，skill 更新了不用手动再装；没装过的
/// 不装，不替用户装上他没要的东西。
pub fn refresh(target: SetupTarget, home: &Path, files: &[(String, String)]) -> Result<bool> {
    let dir = skills_dir(target, home);
    let mut installed = setup_paths(target, home).iter().any(|path| path.exists());
    let mut stale = false;
    for (path, content) in files {
        let path = dir.join(path);
        match std::fs::read(&path) {
            Ok(existing) => {
                installed = true;
                stale |= existing != content.as_bytes();
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => stale = true,
            Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
        }
    }
    if !(installed && stale) {
        return Ok(false);
    }
    setup(target, home, files)?;
    Ok(true)
}

/// 删掉 `existing` 里用标记包着的那段，连同它前面用来隔开的空行；没有这一段时返回 `None`。
fn remove_section(existing: &str) -> Option<String> {
    let start = existing.find(BEGIN)?;
    let end = start + existing[start..].find(END)? + END.len();
    let rest = existing[end..].strip_prefix('\n').unwrap_or(&existing[end..]);
    let before = &existing[..start];
    let before = if rest.is_empty() { before.trim_end_matches('\n') } else { before };
    let mut out = format!("{before}{rest}");
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_paths_inside_a_skill_are_taken() {
        for path in ["runode/SKILL.md", "other-skill/SKILL.md", "runode/references/keys.md"] {
            assert!(is_skill_path(path), "{path}");
        }
        for path in [
            "../SKILL.md",
            "runode/../../x/SKILL.md",
            "/etc/SKILL.md",
            "runode/references/../SKILL.md",
            "runode/references/keys.txt",
            "runode/other/keys.md",
            "Runode/SKILL.md",
            "runode/references/.md",
            "SKILL.md",
        ] {
            assert!(!is_skill_path(path), "{path}");
        }
    }

    #[test]
    fn only_a_file_that_looks_right_is_taken() {
        let skill = FILES[0].1;
        assert_eq!(as_skill_file("runode/SKILL.md", skill.into()).as_deref(), Some(skill));
        assert_eq!(as_skill_file("other-skill/SKILL.md", skill.into()), None);
        assert_eq!(as_skill_file("runode/references/keys.md", b"# Keys\n".to_vec()).as_deref(), Some("# Keys\n"));
        assert_eq!(as_skill_file("runode/references/keys.md", b"<html>404</html>".to_vec()), None);
        assert_eq!(as_skill_file("runode/SKILL.md", vec![0xff, 0xfe]), None);
        let huge = format!("---\nname: runode\n{}", "x".repeat(MAX_FILE_BYTES));
        assert_eq!(as_skill_file("runode/SKILL.md", huge.into()), None);
    }

    #[test]
    fn every_skill_file_is_listed_and_every_link_resolves() {
        // skills/ 下的文件都在清单里，忘了列的装不上。
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        let mut on_disk = Vec::new();
        let mut dirs = vec![root.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.file_name().is_some_and(|name| name != "files.txt") {
                    on_disk.push(path.strip_prefix(&root).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        let listed: Vec<&str> = FILES.iter().map(|(path, _)| *path).collect();
        for path in &on_disk {
            assert!(listed.contains(&path.as_str()), "skills/{path} is not in skills/files.txt");
        }
        assert!(listed.len() <= MAX_FILES);
        for (path, content) in FILES {
            assert!(is_skill_path(path), "{path}");
            assert!(as_skill_file(path, (*content).into()).is_some(), "{path}");
            // SKILL.md 让 agent 去读的 reference 都在清单里。
            let Some(name) = path.strip_suffix("/SKILL.md") else { continue };
            for (at, _) in content.match_indices("](references/") {
                let link = &content[at + 2..];
                let link = &link[..link.find(')').unwrap()];
                assert!(listed.contains(&format!("{name}/{link}").as_str()), "{path} links to {link}");
            }
        }
    }

    #[test]
    fn each_skill_has_its_name_and_a_description() {
        for &(path, skill) in FILES {
            let Some(name) = path.strip_suffix("/SKILL.md") else { continue };
            assert!(skill.starts_with(&format!("---\nname: {name}\ndescription: ")), "{name}");
            // Codex 和 skill 规范都不认超过 1024 个字符的描述，整个 skill 不会被加载。
            let description = skill.lines().nth(2).unwrap().trim_start_matches("description: ");
            assert!(description.chars().count() <= 1024, "{name}: {}", description.chars().count());
        }
    }

    #[test]
    fn the_old_section_is_removed() {
        let section = format!("{BEGIN}\nold text\n{END}\n");
        assert_eq!(remove_section("# My rules\nbe nice\n"), None);
        assert_eq!(remove_section(&section).as_deref(), Some(""));
        assert_eq!(
            remove_section(&format!("# My rules\nbe nice\n\n{section}")).as_deref(),
            Some("# My rules\nbe nice\n")
        );
        // 用户在那一段后面接着写的内容留着。
        assert_eq!(
            remove_section(&format!("# Mine\n\n{section}more rules\n")).as_deref(),
            Some("# Mine\n\nmore rules\n")
        );
    }
}
