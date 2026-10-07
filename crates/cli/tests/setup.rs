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
fn codex_gets_a_section_in_its_agents_file() {
    let fake = FakeHost::start("setupcodex", |_| vec![]);
    let home = fake.env.dirs.home.clone().unwrap();
    let agents = home.join(".codex/AGENTS.md");
    std::fs::create_dir_all(agents.parent().unwrap()).unwrap();
    std::fs::write(&agents, "# Mine\n\nAlways run the tests.\n").unwrap();
    let (code, _, err) = run("setup codex", &fake.env);
    assert_eq!(code, exit::OK, "{err}");
    let once = std::fs::read_to_string(&agents).unwrap();
    assert!(once.starts_with("# Mine\n\nAlways run the tests.\n\n<!-- runode:begin -->\n"), "{once}");
    assert!(once.ends_with("<!-- runode:end -->\n"), "{once}");
    // skill 的元数据不进 AGENTS.md。
    assert!(!once.contains("name: runode"), "{once}");
    assert_eq!(run("setup codex", &fake.env).0, exit::OK);
    let twice = std::fs::read_to_string(&agents).unwrap();
    assert_eq!(twice, once);
    assert_eq!(twice.matches("<!-- runode:begin -->").count(), 1);

    // --print 不写文件。
    let fresh = FakeHost::start("setupprint", |_| vec![]);
    let (code, out, _) = run("setup codex --print", &fresh.env);
    assert_eq!(code, exit::OK);
    assert!(out.starts_with("<!-- runode:begin -->"), "{out}");
    assert!(!fresh.env.dirs.home.as_ref().unwrap().join(".codex").exists());
}
