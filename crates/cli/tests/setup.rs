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
    runode_cli::setup(SetupTarget::Claude, home, &skills).unwrap();
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
