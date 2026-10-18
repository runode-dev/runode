//! `Pick` 拼出的命令和从命令认回来的要对得上。

use runode_agent::models::{LAUNCHERS, Pick, listed, resume_command, same_kind, start_command};

/// 命令开头是 `command` 的那一项。
fn pick(command: &str, model: Option<&str>, effort: Option<&str>) -> Pick {
    let agent = LAUNCHERS.iter().position(|spec| spec.command == command).unwrap();
    Pick { agent, model: model.map(str::to_owned), effort: effort.map(str::to_owned) }
}

#[test]
fn commands_round_trip() {
    assert_eq!(LAUNCHERS[0].command, runode_types::todo::DEFAULT_AGENT);
    assert_eq!(Pick::parse("claude"), Some(Pick::default()));
    for (pick, command) in [
        (pick("claude", Some("opus"), Some("high")), "claude --model opus --effort high"),
        (pick("claude", Some("claude-fable-5-1[1m]"), None), "claude --model 'claude-fable-5-1[1m]'"),
        (pick("codex", None, Some("xhigh")), "codex -c model_reasoning_effort=xhigh"),
        (pick("pi", Some("google/gemini-3-flash"), Some("off")), "pi --model google/gemini-3-flash --thinking off"),
        (pick("opencode", Some("opencode/big-pickle"), None), "opencode -m opencode/big-pickle --prompt"),
        (pick("opencode", None, None), "opencode --prompt"),
        (pick("opencode2 mini", Some("opencode/x"), Some("high")), "opencode2 mini -m opencode/x'#'high --prompt"),
        (pick("copilot", None, Some("max")), "copilot --reasoning-effort max -i"),
        (pick("agy", Some("gemini-3.1-pro"), Some("low")), "agy --model gemini-3.1-pro --effort low -i"),
    ] {
        assert_eq!(pick.command(), command);
        assert_eq!(Pick::parse(command), Some(pick));
    }
    // 强度接在模型名后面的，不挑模型时不写强度。
    assert_eq!(pick("opencode2 mini", None, Some("high")).command(), "opencode2 mini --prompt");
    assert_eq!(Pick::parse("codex --full-auto"), None);
    assert_eq!(Pick::parse("claude --effort high --model opus"), None);
    assert_eq!(Pick::parse("claudex"), None);
    // 少了提示词前面的参数，就不是这样拼出来的。
    assert_eq!(Pick::parse("copilot"), None);
    // shell 会再解释的写法拼不回原样，当自己写的命令，免得保存时改了意思。
    assert_eq!(Pick::parse("claude --model \"opus\""), None);
    assert_eq!(Pick::parse("claude --model $MODEL"), None);
    assert_eq!(Pick::parse("claude --model a\\b"), None);
}

#[test]
fn lists_one_launcher_per_agent() {
    let opencode2 = LAUNCHERS.iter().position(|spec| spec.command == "opencode2 mini").unwrap();
    // 新版在前、菜单里只列它；找不到它时换成紧跟着的 1.x。
    assert!(listed(opencode2) && !listed(opencode2 + 1));
    assert_eq!(same_kind(opencode2).map(|agent| LAUNCHERS[agent].command), Some("opencode"));
    assert_eq!(same_kind(opencode2 + 1), None);
    assert!(listed(0) && same_kind(0).is_none());
    let labels: Vec<_> =
        (0..LAUNCHERS.len()).filter(|&agent| listed(agent)).map(|agent| LAUNCHERS[agent].label).collect();
    let mut unique = labels.clone();
    unique.dedup();
    assert_eq!(labels, unique);
}

/// 能指定对话号的开始时带上号，别家原样。
#[test]
fn start_commands_pin_the_session_where_they_can() {
    assert_eq!(start_command("claude --model opus", "S"), "claude --session-id S --model opus");
    assert_eq!(start_command("codex -m gpt-5", "S"), "codex -m gpt-5");
    assert_eq!(start_command("claudex", "S"), "claudex");
}

/// 续接的命令插在程序名后面，别的参数照留，末尾等提示词的参数去掉；能指定对话号的按号接回；认不出的、
/// 没有续接参数的不续。
#[test]
fn resume_commands_keep_the_other_arguments() {
    for (command, resumed) in [
        ("claude", Some("claude --resume S")),
        (
            "claude --model opus --dangerously-skip-permissions",
            Some("claude --resume S --model opus --dangerously-skip-permissions"),
        ),
        ("codex -m gpt-5", Some("codex resume --last -m gpt-5")),
        ("copilot --reasoning-effort max -i", Some("copilot --continue --reasoning-effort max")),
        ("opencode --prompt", Some("opencode --continue")),
        ("opencode2 mini -m opencode/x --prompt", None),
        ("claudex", None),
        ("/usr/local/bin/claude", None),
    ] {
        assert_eq!(resume_command(command, "S").as_deref(), resumed, "{command}");
    }
}
