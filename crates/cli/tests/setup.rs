//! `runode setup`：把使用说明装进临时的家目录，装几次都一样，别的内容不动。

mod common;

use common::*;
use runode_cli::{SetupTarget, exit};

#[test]
fn claude_gets_a_skill() {
    let fake = FakeHost::start("setupclaude", |_| vec![]);
    let home = fake.env.dirs.home.clone().unwrap();
    let skill = home.join(".claude/skills/runode/SKILL.md");
    let simulator = home.join(".claude/skills/runode-simulator/SKILL.md");
    let paths = runode_cli::setup_paths(SetupTarget::Claude, &home);
    assert!(paths.contains(&skill) && paths.contains(&simulator));
    assert!(paths.contains(&home.join(".claude/skills/runode/references/keys.md")));
    let (code, out, err) = run("setup claude", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let listed: String = paths.iter().map(|path| format!("installed {}\n", path.display())).collect();
    assert_eq!(out, listed);
    let installed = std::fs::read_to_string(&skill).unwrap();
    let installed_simulator = std::fs::read_to_string(&simulator).unwrap();
    assert!(installed_simulator.starts_with("---\nname: runode-simulator\n"), "{installed_simulator}");
    assert!(installed.starts_with("---\nname: runode\n"), "{installed}");
    assert!(installed.contains("runode send"));
    // 再装一次照样，内容不变；--print 只打印，和装的一样。
    assert_eq!(run("setup claude", &fake.env).0, exit::OK);
    assert_eq!(std::fs::read_to_string(&skill).unwrap(), installed);
    let printed = run("setup claude --print", &fake.env).1;
    for (path, content) in runode_cli::bundled_skills() {
        assert!(printed.contains(&format!("==> {path} <==\n{content}")), "{path}");
        assert_eq!(std::fs::read_to_string(home.join(".claude/skills").join(&path)).unwrap(), content);
    }
}

#[test]
fn codex_gets_a_skill_and_loses_the_old_section() {
    let fake = FakeHost::start("setupcodex", |_| vec![]);
    let home = fake.env.dirs.home.clone().unwrap();
    let skill = home.join(".agents/skills/runode/SKILL.md");
    let simulator = home.join(".agents/skills/runode-simulator/SKILL.md");
    let agents = home.join(".codex/AGENTS.md");
    std::fs::create_dir_all(agents.parent().unwrap()).unwrap();
    std::fs::write(&agents, "# Mine\n\nAlways run the tests.\n\n<!-- runode:begin -->\nold\n<!-- runode:end -->\n")
        .unwrap();
    let (code, out, err) = run("setup codex", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert!(out.contains(&format!("installed {}\n", skill.display())), "{out}");
    assert!(out.contains(&format!("installed {}\n", simulator.display())), "{out}");
    assert!(std::fs::read_to_string(&skill).unwrap().starts_with("---\nname: runode\n"));
    // 早先装进 AGENTS.md 的那段删掉，别的内容留着。
    assert_eq!(std::fs::read_to_string(&agents).unwrap(), "# Mine\n\nAlways run the tests.\n");

    // 没有 AGENTS.md 时不去建它。
    let fresh = FakeHost::start("setupfresh", |_| vec![]);
    assert_eq!(run("setup codex", &fresh.env).0, exit::OK);
    assert!(!fresh.env.dirs.home.as_ref().unwrap().join(".codex").exists());
}

#[test]
fn refresh_rewrites_only_installed_skills() {
    let fake = FakeHost::start("setuprefresh", |_| vec![]);
    let home = fake.env.dirs.home.as_deref().unwrap();
    let skills = runode_cli::bundled_skills();
    // 没装过的不装。
    assert!(!runode_cli::refresh(SetupTarget::Claude, home, &skills).unwrap());
    assert!(!home.join(".claude").exists());

    // 装过的：旧了、缺了一份都重装成这一版的；一样时不动。
    runode_cli::setup(SetupTarget::Claude, home, &skills, true).unwrap();
    let skill = home.join(".claude/skills/runode/SKILL.md");
    let simulator = home.join(".claude/skills/runode-simulator/SKILL.md");
    let current = std::fs::read_to_string(&skill).unwrap();
    assert!(!runode_cli::refresh(SetupTarget::Claude, home, &skills).unwrap());
    std::fs::write(&skill, "old").unwrap();
    std::fs::remove_file(&simulator).unwrap();
    assert!(runode_cli::refresh(SetupTarget::Claude, home, &skills).unwrap());
    assert_eq!(std::fs::read_to_string(&skill).unwrap(), current);
    assert!(simulator.exists());
    // 只装了 Claude 的，Codex 那边照样不碰。
    assert!(!runode_cli::refresh(SetupTarget::Codex, home, &skills).unwrap());
    assert!(!home.join(".agents").exists());
}

#[test]
fn a_skill_dropped_from_the_list_is_removed() {
    let fake = FakeHost::start("setupdrop", |_| vec![]);
    let home = fake.env.dirs.home.as_deref().unwrap();
    let skills = home.join(".claude/skills");
    let file = |path: &str, content: &str| (path.to_owned(), content.to_owned());
    let old = vec![
        file("runode/SKILL.md", "---\nname: runode\n"),
        file("gone/SKILL.md", "---\nname: gone\n"),
        file("gone/references/a.md", "# A\n"),
        file("kept/SKILL.md", "---\nname: kept\n"),
        file("kept/references/b.md", "# B\n"),
    ];
    runode_cli::setup(SetupTarget::Claude, home, &old, true).unwrap();
    std::fs::write(skills.join("kept/notes.md"), "mine").unwrap();
    std::fs::write(skills.join("other.md"), "not ours").unwrap();

    // 断网时装的是编进这一版的那套，可能比 main 旧，少了的不删。
    let new = vec![file("runode/SKILL.md", "---\nname: runode\n"), file("kept/SKILL.md", "---\nname: kept\n")];
    runode_cli::setup(SetupTarget::Claude, home, &new, false).unwrap();
    assert!(skills.join("gone/references/a.md").exists());

    // 下到的清单里去掉的 skill 整个删掉；还在的 skill 里去掉的 reference 删掉，用户自己放的文件留着。
    assert!(runode_cli::refresh(SetupTarget::Claude, home, &new).unwrap());
    assert!(!skills.join("gone").exists());
    assert!(!skills.join("kept/references").exists());
    assert!(skills.join("kept/SKILL.md").exists() && skills.join("kept/notes.md").exists());
    assert!(skills.join("other.md").exists());
    assert!(!runode_cli::refresh(SetupTarget::Claude, home, &new).unwrap());

    // 用户删掉了 skill，只剩记录时不再装回去。
    for name in ["runode", "kept"] {
        std::fs::remove_dir_all(skills.join(name)).unwrap();
    }
    assert!(!runode_cli::refresh(SetupTarget::Claude, home, &new).unwrap());
    assert!(!skills.join("runode").exists());
}
