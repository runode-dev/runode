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
fn usage_takes_over_claudes_status_line_and_keeps_the_previous_command() {
    let fake = FakeHost::start("setupstatus", |_| vec![]);
    let home = fake.env.dirs.home.clone().unwrap();
    let mut env = fake.env;
    env.dirs.data = Some(home.join(".runode"));
    let settings = home.join(".claude/settings.json");
    let chain = home.join(".runode/claude-statusline");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, r#"{"model":"opus","statusLine":{"type":"command","command":"echo mine","padding":1}}"#)
        .unwrap();

    let (code, out, err) = run("setup usage", &env);
    assert_eq!(code, exit::OK, "{err}");
    // 只有 Claude Code 装着，只改它的。
    assert_eq!(out, format!("installed usage reporting in {}\n", settings.display()));
    // 原来的命令存起来，别的设置和 statusLine 里别的项留着，键的先后不变。
    assert_eq!(std::fs::read_to_string(&chain).unwrap(), "echo mine");
    let installed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(installed["model"], "opus");
    assert_eq!(installed["statusLine"]["padding"], 1);
    let command = installed["statusLine"]["command"].as_str().unwrap().to_owned();
    assert!(command.contains("usage-hook claude;"), "{command}");
    let text = std::fs::read_to_string(&settings).unwrap();
    assert!(text.find("model") < text.find("statusLine"), "{text}");

    // 再装一次：已经是 runode 的了，存着的原命令不动。
    assert_eq!(run("setup usage", &env).0, exit::OK);
    assert_eq!(std::fs::read_to_string(&chain).unwrap(), "echo mine");

    // 不在 runode 的终端里时不起 runode，照旧跑原来的状态栏命令。
    assert_eq!(sh(&command, None), "mine\n");
}

/// 像 agent 那样用 sh 跑 `command`，标准输入给一个 JSON；`bin` 是 app 设的 `RUNODE_BIN`，为 `None` 时不在
/// runode 的终端里。返回标准输出，退出码不是 0 时失败。
fn sh(command: &str, bin: Option<&std::path::Path>) -> String {
    let mut sh = std::process::Command::new("/bin/sh");
    sh.args(["-c", command]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped());
    match bin {
        Some(bin) => sh.env("RUNODE_BIN", bin),
        None => sh.env_remove("RUNODE_BIN"),
    };
    let mut child = sh.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), b"{}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{command}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn usage_hooks_go_after_the_others_once() {
    let fake = FakeHost::start("setuphooks", |_| vec![]);
    let home = fake.env.dirs.home.clone().unwrap();
    let mut env = fake.env;
    env.dirs.data = Some(home.join(".runode"));
    let codex = home.join(".codex/hooks.json");
    let gemini = home.join(".gemini/settings.json");
    let pi = home.join(".pi/agent/extensions/runode-usage.ts");
    std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
    std::fs::create_dir_all(gemini.parent().unwrap()).unwrap();
    std::fs::create_dir_all(home.join(".pi")).unwrap();
    let theirs = r#"{"hooks":[{"type":"command","command":"theirs.sh"}]}"#;
    std::fs::write(&codex, format!(r#"{{"hooks":{{"Stop":[{theirs}]}}}}"#)).unwrap();
    std::fs::write(&gemini, format!(r#"{{"theme":"x","hooks":{{"AfterAgent":[{theirs}]}}}}"#)).unwrap();

    let (code, out, err) = run("setup usage", &env);
    assert_eq!(code, exit::OK, "{err}");
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    // 再装一次：已经挂着的不再加。
    assert_eq!(run("setup usage", &env).0, exit::OK);

    let read = |path: &std::path::Path| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let commands = |file: &serde_json::Value, event: &str| -> Vec<String> {
        file["hooks"][event]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|group| group["hooks"].as_array().unwrap())
            .map(|hook| hook["command"].as_str().unwrap().to_owned())
            .collect()
    };
    let codex = read(&codex);
    let stop = commands(&codex, "Stop");
    assert_eq!(stop.len(), 2, "{stop:?}");
    assert_eq!(stop[0], "theirs.sh");
    assert!(stop[1].contains("usage-hook codex;"), "{stop:?}");
    // 在 runode 的终端里交给 `RUNODE_BIN`，不在时读完输入就走。
    let bin = home.join("fake-runode");
    std::fs::write(&bin, "#!/bin/sh\ncat >/dev/null\necho \"$@\"\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    assert_eq!(sh(&stop[1], Some(&bin)), "usage-hook codex\n");
    assert_eq!(sh(&stop[1], None), "");
    assert_eq!(commands(&codex, "SessionStart").len(), 1);
    assert_eq!(commands(&codex, "PostToolUse").len(), 1);

    let gemini = read(&gemini);
    assert_eq!(gemini["theme"], "x");
    let after = commands(&gemini, "AfterAgent");
    assert_eq!(after.len(), 2, "{after:?}");
    assert!(after[1].contains("usage-hook gemini;"), "{after:?}");
    assert_eq!(gemini["hooks"]["AfterAgent"][1]["hooks"][0]["timeout"], 5000);

    assert!(std::fs::read_to_string(&pi).unwrap().contains(r#"["usage-hook", "pi"]"#));
}

#[test]
fn usage_needs_an_agent() {
    let fake = FakeHost::start("setupnone", |_| vec![]);
    let (code, _, err) = run("setup usage", &fake.env);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("found none of"), "{err}");
}

#[test]
fn usage_keeps_going_past_an_agent_it_cannot_set_up() {
    let fake = FakeHost::start("setuppartial", |_| vec![]);
    let home = fake.env.dirs.home.clone().unwrap();
    let gemini = home.join(".gemini/settings.json");
    let pi = home.join(".pi/agent/extensions/runode-usage.ts");
    std::fs::create_dir_all(gemini.parent().unwrap()).unwrap();
    std::fs::create_dir_all(home.join(".pi")).unwrap();
    // Gemini CLI 的设置文件可以写注释，这里读不了。
    std::fs::write(&gemini, "// mine\n{}").unwrap();

    let (code, _, err) = run("setup usage", &fake.env);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("cannot parse"), "{err}");
    assert!(err.contains(&format!("installed usage reporting in {}", pi.display())), "{err}");
    assert!(pi.exists());
    assert_eq!(std::fs::read_to_string(&gemini).unwrap(), "// mine\n{}");
}
