//! `runode setup`：把使用说明装进临时的家目录，装几次都一样，别的内容不动。

mod common;

use common::*;
use runode_cli::exit;

#[test]
fn claude_gets_a_skill() {
    let fake = FakeHost::start("setupclaude", |_| vec![]);
    let home = fake.env.dirs.home.clone().unwrap();
    let skill = home.join(".claude/skills/runode/SKILL.md");
    let (code, out, err) = run("setup claude", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, format!("installed {}\n", skill.display()));
    let installed = std::fs::read_to_string(&skill).unwrap();
    assert!(installed.starts_with("---\nname: runode\n"), "{installed}");
    assert!(installed.contains("runode send"));
    // 再装一次照样，内容不变；--print 只打印，和装的一样。
    assert_eq!(run("setup claude", &fake.env).0, exit::OK);
    assert_eq!(std::fs::read_to_string(&skill).unwrap(), installed);
    assert_eq!(run("setup claude --print", &fake.env).1, installed);
}

#[test]
fn codex_gets_a_skill_and_loses_the_old_section() {
    let fake = FakeHost::start("setupcodex", |_| vec![]);
    let home = fake.env.dirs.home.clone().unwrap();
    let skill = home.join(".agents/skills/runode/SKILL.md");
    let agents = home.join(".codex/AGENTS.md");
    std::fs::create_dir_all(agents.parent().unwrap()).unwrap();
    std::fs::write(&agents, "# Mine\n\nAlways run the tests.\n\n<!-- runode:begin -->\nold\n<!-- runode:end -->\n")
        .unwrap();
    let (code, out, err) = run("setup codex", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, format!("installed {}\n", skill.display()));
    assert!(std::fs::read_to_string(&skill).unwrap().starts_with("---\nname: runode\n"));
    // 早先装进 AGENTS.md 的那段删掉，别的内容留着。
    assert_eq!(std::fs::read_to_string(&agents).unwrap(), "# Mine\n\nAlways run the tests.\n");

    // 没有 AGENTS.md 时不去建它。
    let fresh = FakeHost::start("setupfresh", |_| vec![]);
    assert_eq!(run("setup codex", &fresh.env).0, exit::OK);
    assert!(!fresh.env.dirs.home.as_ref().unwrap().join(".codex").exists());
}
