//! 新建任务时能挑的 skill 和自定义 agent。skill 照 Agent Skills 的标准格式（agentskills.io）：一个目录一个 skill，
//! 里面的 `SKILL.md` 开头是 YAML 的 front matter，必须写 `name` 和 `description`，`name` 只用小写字母、数字和
//! 连字符、和目录同名；不合格式的不列。到任务目录和它往上各层的 `.agents/skills`、`.claude/skills`，家目录下的
//! `.agents/skills`、`.claude/skills`、`.codex/skills` 里找。自定义 agent 照 Claude Code 的格式：`.claude/agents/`
//! 下的 `*.md`，front matter 同样要有 `name` 和 `description`，名字只能有字母、数字和 `-_.:`。
// shortcut: 插件带的 skill、agent（`插件:名字`）不列，要用时写进提示词；有人要时再读 `~/.claude/plugins`。

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use crate::kanban::templates::front_matter;

/// 找到的 skill 和自定义 agent 的名字，各自排好、不重。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) skills: Vec<String>,
    pub(crate) subagents: Vec<String>,
}

/// 在 `dir` 往上各层和家目录 `home` 下找，见模块说明。会读文件，放到后台调。
pub(crate) fn find(home: Option<&Path>, dir: &Path) -> Found {
    let roots: Vec<&Path> = dir.ancestors().chain(home).collect();
    let skill_dirs = roots
        .iter()
        .flat_map(|root| [root.join(".agents/skills"), root.join(".claude/skills")])
        .chain(home.map(|home| home.join(".codex/skills")));
    let mut skills = BTreeSet::new();
    for skills_dir in skill_dirs {
        for path in entries(&skills_dir) {
            let Some(folder) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else { continue };
            if let Some(name) = declared_name(&path.join("SKILL.md")).filter(|name| *name == folder && skill_name(name))
            {
                skills.insert(name);
            }
        }
    }
    let mut subagents = BTreeSet::new();
    for agents_dir in roots.iter().map(|root| root.join(".claude/agents")) {
        for path in entries(&agents_dir).filter(|path| path.extension().is_some_and(|ext| ext == "md")) {
            // 名字要拼进 shell 命令，不合 `subagent_name` 的宿主不收，不列。
            subagents.extend(declared_name(&path).filter(|name| runode_agent::models::subagent_name(name)));
        }
    }
    Found { skills: skills.into_iter().collect(), subagents: subagents.into_iter().collect() }
}

fn entries(dir: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir).into_iter().flatten().filter_map(|entry| Some(entry.ok()?.path()))
}

/// 文件 `file` 的 front matter 里写的 `name`；读不了、没有 front matter、缺 `name` 或 `description` 时为空。
/// `description` 可以写成多行（`>`、`|`），只看它在不在。
fn declared_name(file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let (fields, _) = front_matter(&text);
    let field = |key: &str| fields.iter().find(|(k, _)| *k == key).map(|(_, value)| *value);
    field("description")?;
    field("name").filter(|name| !name.is_empty()).map(str::to_owned)
}

/// 标准里 skill 的名字：1 到 64 个小写字母、数字和连字符，连字符不在头尾、不连着。
fn skill_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_skills_and_agents_from_the_project_up_and_in_home() {
        let root = std::env::temp_dir().join(format!("runode-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, project) = (root.join("home"), root.join("repo"));
        let write = |path: PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        let skill = |name: &str| format!("---\nname: {name}\ndescription: >\n  Does {name}.\n---\nBody");
        write(home.join(".claude/skills/review/SKILL.md"), &skill("review"));
        write(home.join(".agents/skills/tdd/SKILL.md"), &skill("tdd"));
        write(project.join(".agents/skills/tdd/SKILL.md"), &skill("tdd"));
        write(project.join(".claude/skills/lint/SKILL.md"), &skill("lint"));
        // 不合格式的不列：没有 front matter、缺 description、和目录不同名、名字里有大写。
        write(home.join(".claude/skills/plain/SKILL.md"), "no front matter");
        write(home.join(".claude/skills/terse/SKILL.md"), "---\nname: terse\n---\n");
        write(home.join(".claude/skills/folder/SKILL.md"), &skill("other"));
        write(home.join(".claude/skills/Loud/SKILL.md"), &skill("Loud"));
        write(home.join(".claude/skills/empty/README.md"), &skill("empty"));
        write(project.join(".claude/agents/reviewer.md"), "---\nname: strict-reviewer\ndescription: Reviews.\n---\n");
        write(home.join(".claude/agents/helper.md"), "---\nname: helper\ndescription: Helps.\n---\n");
        write(home.join(".claude/agents/nameless.md"), "---\ndescription: No name.\n---\n");
        write(home.join(".claude/agents/spaced.md"), "---\nname: two words\ndescription: Spaced.\n---\n");
        write(home.join(".claude/agents/notes.txt"), "---\nname: notes\ndescription: Not markdown.\n---\n");
        let found = find(Some(&home), &project.join("web/src"));
        assert_eq!(found.skills, ["lint", "review", "tdd"]);
        assert_eq!(found.subagents, ["helper", "strict-reviewer"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
