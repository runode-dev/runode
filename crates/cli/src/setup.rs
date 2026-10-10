//! 教 agent 用 runode：把使用说明装成 agent 按需加载的 skill，一份讲操作别的终端，一份讲在模拟器页
//! 看着的模拟器上跑和测 app。Claude Code 读 `~/.claude/skills/<名字>/SKILL.md`，Codex 读
//! `~/.agents/skills/<名字>/SKILL.md`，两边是同样的文件。
//! 早先给 Codex 装在 `~/.codex/AGENTS.md` 的 `<!-- runode:begin -->`、`<!-- runode:end -->` 之间，
//! 再装时把这一段删掉，文件里别的内容不动。

use std::{
    io,
    path::{Path, PathBuf},
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

/// 把使用说明装到 `home` 下 `target` 读的地方（见 `setup_paths`），返回写的文件。可以重复执行，
/// 每次整个换掉；给 Codex 装时顺带删掉早先写进 `~/.codex/AGENTS.md` 的那段。
pub fn setup(target: SetupTarget, home: &Path) -> Result<Vec<PathBuf>> {
    let paths = setup_paths(target, home);
    for (path, (_, skill)) in paths.iter().zip(SKILLS) {
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
