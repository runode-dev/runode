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

#[test]
fn statusline_takes_over_and_keeps_the_previous_command() {
    let fake = FakeHost::start("setupstatus", |_| vec![]);
    let home = fake.env.dirs.home.clone().unwrap();
    let mut env = fake.env;
    env.dirs.data = Some(home.join(".runode"));
    let settings = home.join(".claude/settings.json");
    let chain = home.join(".runode/claude-statusline");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, r#"{"model":"opus","statusLine":{"type":"command","command":"echo mine","padding":1}}"#)
        .unwrap();

    let (code, out, err) = run("setup statusline", &env);
    assert_eq!(code, exit::OK, "{err}");
    assert_eq!(out, format!("installed the status line in {}\n", settings.display()));
    // 原来的命令存起来，别的设置和 statusLine 里别的项留着，键的先后不变。
    assert_eq!(std::fs::read_to_string(&chain).unwrap(), "echo mine");
    let installed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(installed["model"], "opus");
    assert_eq!(installed["statusLine"]["padding"], 1);
    let command = installed["statusLine"]["command"].as_str().unwrap();
    assert!(command.starts_with("\"${RUNODE_BIN:-") && command.ends_with("\" statusline"), "{command}");
    let text = std::fs::read_to_string(&settings).unwrap();
    assert!(text.find("model") < text.find("statusLine"), "{text}");

    // 再装一次：已经是 runode 的了，存着的原命令不动。
    assert_eq!(run("setup statusline", &env).0, exit::OK);
    assert_eq!(std::fs::read_to_string(&chain).unwrap(), "echo mine");
}
