//! 教 agent 用 runode：把使用说明装到 agent 读得到的地方。Claude Code 读
//! `~/.claude/skills/<名字>/SKILL.md` 这样的 skill；Codex 读 `~/.codex/AGENTS.md`，说明放在一对
//! `<!-- runode:begin -->`、`<!-- runode:end -->` 之间，再装一次只换掉这一段，文件里别的内容不动。

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

/// 使用说明，开头是 skill 的元数据（名字和什么时候用）。
const SKILL: &str = include_str!("../assets/skill/SKILL.md");
const BEGIN: &str = "<!-- runode:begin -->";
const END: &str = "<!-- runode:end -->";

/// 装给 `target` 的内容：Claude Code 是整份 skill，Codex 是去掉元数据、用标记包起来的一段。
pub(crate) fn text(target: SetupTarget) -> String {
    match target {
        SetupTarget::Claude => SKILL.into(),
        SetupTarget::Codex => format!("{BEGIN}\n{}{END}\n", body()),
    }
}

/// skill 去掉开头 `---` 之间的元数据。
fn body() -> &'static str {
    SKILL
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(SKILL, |(_, body)| body.trim_start_matches('\n'))
}

/// `setup` 往 `home` 下哪个文件装给 `target` 的使用说明。
pub fn setup_path(target: SetupTarget, home: &Path) -> PathBuf {
    match target {
        SetupTarget::Claude => home.join(".claude/skills/runode/SKILL.md"),
        SetupTarget::Codex => home.join(".codex/AGENTS.md"),
    }
}

/// 把使用说明装到 `home` 下 `target` 读的地方（见 `setup_path`），返回写的文件。可以重复执行：
/// skill 整个换掉，AGENTS.md 只换掉标记之间的那段，没有时加在末尾。
pub fn setup(target: SetupTarget, home: &Path) -> Result<PathBuf> {
    let path = setup_path(target, home);
    let contents = match target {
        SetupTarget::Claude => text(target),
        SetupTarget::Codex => {
            let existing = match std::fs::read_to_string(&path) {
                Ok(existing) => existing,
                Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
                Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
            };
            merge(&existing, &text(target))
        }
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    std::fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// 把用标记包着的 `section` 放进 `existing`：已经有一段时原地换掉，没有时空一行加在末尾。
fn merge(existing: &str, section: &str) -> String {
    if let Some(start) = existing.find(BEGIN)
        && let Some(end) = existing[start..].find(END).map(|end| start + end + END.len())
    {
        let rest = existing[end..].strip_prefix('\n').unwrap_or(&existing[end..]);
        return format!("{}{section}{rest}", &existing[..start]);
    }
    let mut merged = existing.to_owned();
    if !merged.is_empty() {
        if !merged.ends_with('\n') {
            merged.push('\n');
        }
        merged.push('\n');
    }
    merged.push_str(section);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_has_a_name_and_a_description() {
        assert!(SKILL.starts_with("---\nname: runode\ndescription: "));
        assert!(body().starts_with("# "), "{}", &body()[..40]);
    }

    #[test]
    fn the_section_replaces_itself() {
        let section = text(SetupTarget::Codex);
        assert!(section.starts_with(BEGIN) && section.ends_with(&format!("{END}\n")));
        assert_eq!(merge("", &section), section);
        let mine = "# My rules\nbe nice";
        let once = merge(mine, &section);
        assert_eq!(once, format!("{mine}\n\n{section}"));
        assert_eq!(merge(&once, &section), once);
        // 用户在那一段后面接着写的内容留着。
        let edited = format!("{once}more rules\n");
        let old = format!("{mine}\n\n{BEGIN}\nold text\n{END}\nmore rules\n");
        assert_eq!(merge(&old, &section), edited);
    }
}
