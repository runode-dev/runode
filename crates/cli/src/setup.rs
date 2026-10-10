//! 教 agent 用 runode：把使用说明装成 agent 按需加载的 skill，一份讲操作别的终端，一份讲在模拟器页
//! 看着的模拟器上跑和测 app。Claude Code 读 `~/.claude/skills/<名字>/SKILL.md`，Codex 读
//! `~/.agents/skills/<名字>/SKILL.md`，两边是同样的文件。
//! 装的是 GitHub 上仓库 main 里最新的一份（`SKILLS_URL`），只改 skill 不用发新版 app；下不到时用编进
//! 这一版的 `SKILLS`。装哪几个 skill 由这一版定，新加的 skill 要等 app 更新才装上。
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

/// 各个 skill 的名字和使用说明，说明开头是 skill 的元数据（名字和什么时候用）；两个 agent 装的都是这些。
pub(crate) const SKILLS: [(&str, &str); 2] = [
    ("runode", include_str!("../../../skills/runode/SKILL.md")),
    ("runode-simulator", include_str!("../../../skills/runode-simulator/SKILL.md")),
];
/// 仓库里放 skill 的目录在 GitHub 上的原始文件地址，下面是 `<名字>/SKILL.md`。
pub const SKILLS_URL: &str = "https://raw.githubusercontent.com/runode-dev/runode/main/skills";
/// 下回来的一份 skill 最多这么大，再大就不是 skill。
const MAX_SKILL_BYTES: usize = 256 * 1024;
const BEGIN: &str = "<!-- runode:begin -->";
const END: &str = "<!-- runode:end -->";

/// `setup` 往 `home` 下哪些文件装给 `target` 的使用说明，和 `SKILLS` 一一对应。
pub fn setup_paths(target: SetupTarget, home: &Path) -> Vec<PathBuf> {
    let dir = match target {
        SetupTarget::Claude => home.join(".claude/skills"),
        SetupTarget::Codex => home.join(".agents/skills"),
    };
    SKILLS.iter().map(|(name, _)| dir.join(name).join("SKILL.md")).collect()
}

/// 编进这一版的各个 skill，和 `SKILLS` 一一对应。
pub fn bundled_skills() -> Vec<String> {
    SKILLS.iter().map(|(_, skill)| (*skill).to_owned()).collect()
}

/// 从 `url`（通常是 `SKILLS_URL`）下最新的各个 skill，和 `SKILLS` 一一对应；有一个下不到或不像 skill
/// 就整个不用，返回 `None`，免得装上新旧混着的一套。外调 curl，最多等十秒。
pub fn fetch_skills(url: &str) -> Option<Vec<String>> {
    SKILLS.iter().map(|(name, _)| fetch_skill(url, name)).collect()
}

fn fetch_skill(url: &str, name: &str) -> Option<String> {
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--location", "--max-time", "10", "--proto", "=https", "--proto-redir", "=https"])
        .arg(format!("{url}/{name}/SKILL.md"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    as_skill(name, output.stdout)
}

/// 下回来的 `body` 像不像名叫 `name` 的 skill：不太大、是 UTF-8、开头是这个名字的元数据。挡住错误
/// 页面和改了名的文件。
fn as_skill(name: &str, body: Vec<u8>) -> Option<String> {
    let skill = String::from_utf8(body).ok().filter(|skill| skill.len() <= MAX_SKILL_BYTES)?;
    skill.starts_with(&format!("---\nname: {name}\n")).then_some(skill)
}

/// 把 `skills`（`fetch_skills` 或 `bundled_skills` 给的）装到 `home` 下 `target` 读的地方（见
/// `setup_paths`），返回写的文件。可以重复执行，每次整个换掉；给 Codex 装时顺带删掉早先写进
/// `~/.codex/AGENTS.md` 的那段。
pub fn setup(target: SetupTarget, home: &Path, skills: &[String]) -> Result<Vec<PathBuf>> {
    let paths = setup_paths(target, home);
    for (path, skill) in paths.iter().zip(skills) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
        }
        runode_paths::replace_file(path, skill.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
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
    Ok(paths)
}

/// 给 `target` 装过使用说明（`setup_paths` 里有文件在），又和 `skills` 不一样或缺了一份时重装，返回
/// 重装了没有。app 启动时拿 `fetch_skills` 下到的用，skill 更新了不用手动再装；没装过的不装，不替
/// 用户装上他没要的东西。
pub fn refresh(target: SetupTarget, home: &Path, skills: &[String]) -> Result<bool> {
    let mut installed = false;
    let mut stale = false;
    for (path, skill) in setup_paths(target, home).iter().zip(skills) {
        match std::fs::read(path) {
            Ok(existing) => {
                installed = true;
                stale |= existing != skill.as_bytes();
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => stale = true,
            Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
        }
    }
    if !(installed && stale) {
        return Ok(false);
    }
    setup(target, home, skills)?;
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
    fn only_a_skill_with_the_right_name_is_taken() {
        let skill = SKILLS[0].1;
        assert_eq!(as_skill("runode", skill.into()).as_deref(), Some(skill));
        assert_eq!(as_skill("runode-simulator", skill.into()), None);
        assert_eq!(as_skill("runode", b"<html>404</html>".to_vec()), None);
        assert_eq!(as_skill("runode", vec![0xff, 0xfe]), None);
        let huge = format!("---\nname: runode\n{}", "x".repeat(MAX_SKILL_BYTES));
        assert_eq!(as_skill("runode", huge.into()), None);
    }

    #[test]
    fn each_skill_has_its_name_and_a_description() {
        for (name, skill) in SKILLS {
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
