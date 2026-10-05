//! 按前台进程认出 agent：进程名和别名、组长优先、argv[0]、经解释器跑的脚本、内联代码和
//! 符号链接的启动器。

use runode_agent_detect::{ForegroundJob, ForegroundProcess, agent_from_name, identify_job};
use runode_shared_types::agent::AgentKind;
use AgentKind::*;

fn process(pid: u32, name: &str, argv: &[&str]) -> ForegroundProcess {
    ForegroundProcess {
        pid,
        name: name.into(),
        argv0: None,
        argv: Some(argv.iter().map(|arg| (*arg).to_owned()).collect()),
    }
}

fn job(processes: Vec<ForegroundProcess>) -> ForegroundJob {
    ForegroundJob { leader: 123, processes }
}

fn alone(name: &str, argv: &[&str]) -> Option<AgentKind> {
    identify_job(&job(vec![process(123, name, argv)]))
}

#[test]
fn known_names_and_aliases() {
    for (name, kind) in [
        ("pi", Pi),
        ("claude", Claude),
        ("claude-code", Claude),
        ("CLAUDE", Claude),
        ("Codex", Codex),
        ("gemini", Gemini),
        ("cursor-agent", Cursor),
        ("devin-cli", Devin),
        ("agy", Antigravity),
        ("antigravity-cli", Antigravity),
        ("cline", Cline),
        ("omp", Omp),
        ("mastra-code", Mastracode),
        ("opencode2.exe", OpenCode),
        ("Kimi Code", Kimi),
        ("kiro-cli", Kiro),
        ("copilot", GithubCopilot),
        ("ghcs", GithubCopilot),
        ("droid", Droid),
        ("amp-local", Amp),
        ("grok-build", Grok),
        ("hermes-agent", Hermes),
        ("kilo-code", Kilo),
        ("qodercli", Qodercli),
        ("Qwen Code", Qwen),
        ("Letta Code", Letta),
        ("maki", Maki),
        ("muse-cli", Muse),
        ("muse-bin-0.1.0-R708.1", Muse),
        ("/home/user/.local/bin/muse-bin-0.2.1-R1215.1", Muse),
    ] {
        assert_eq!(agent_from_name(name), Some(kind), "{name}");
    }
    for kind in AgentKind::ALL {
        assert_eq!(agent_from_name(kind.label()), Some(kind));
    }
    for name in ["bash", "zsh", "vim", "node", "museum", "muse-helper", "musescore", "muse-bin", "muse-bin-", "muse-binary"] {
        assert_eq!(agent_from_name(name), None, "{name}");
    }
}

#[test]
fn the_leader_wins_when_it_is_an_agent() {
    let found = identify_job(&job(vec![process(7, "codex", &["codex"]), process(123, "claude", &["claude"])]));
    assert_eq!(found, Some(Claude));
    // 组长认不出时从整组里找。
    let found = identify_job(&job(vec![process(123, "npm", &["npm", "exec"]), process(7, "codex", &["codex"])]));
    assert_eq!(found, Some(Codex));
}

#[test]
fn argv0_set_by_the_program_counts() {
    // 原生安装的 claude 进程名是版本号，argv[0] 才是 claude。
    let mut claude = process(123, "2.1.3", &["claude"]);
    claude.argv0 = Some("claude".into());
    assert_eq!(identify_job(&job(vec![claude])), Some(Claude));
    let mut pi = process(123, "node", &["pi"]);
    pi.argv0 = Some("pi".into());
    assert_eq!(identify_job(&job(vec![pi])), Some(Pi));
}

#[test]
fn scripts_run_by_node_bun_python_and_shells() {
    assert_eq!(alone("node", &["node", "/path/to/bin/codex"]), Some(Codex));
    assert_eq!(alone("MainThread", &["node", "/home/user/.fnm/bin/qwen"]), Some(Qwen));
    assert_eq!(alone("MainThread", &["node", "/home/user/.fnm/bin/cline", "--tui"]), Some(Cline));
    assert_eq!(alone("node", &["node", "/usr/local/lib/node_modules/cline/bin/cline"]), Some(Cline));
    assert_eq!(alone("node", &["node", "--require", "x.js", "/opt/bin/gemini"]), Some(Gemini));
    assert_eq!(alone("node", &["node", "--", "/opt/bin/gemini"]), Some(Gemini));
    assert_eq!(alone("bash", &["bash", "/home/user/bin/pi"]), Some(Pi));
    assert_eq!(alone("python3.12", &["python3.12", "/home/user/.local/bin/hermes"]), Some(Hermes));
    assert_eq!(alone("bun", &["bun", "/home/u/.bun/install/global/node_modules/@oh-my-pi/pi-coding-agent/dist/cli.js"]), Some(Omp));
    assert_eq!(alone("node", &["node", "/usr/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js"]), Some(Pi));
    assert_eq!(alone("node", &["node", "/usr/lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js"]), Some(Pi));
    assert_eq!(alone("node", &["node", "/usr/lib/node_modules/mastracode/dist/cli.js"]), Some(Mastracode));
    assert_eq!(alone("node", &["node", "/usr/lib/node_modules/@moonshot-ai/kimi-code/dist/main.mjs"]), Some(Kimi));
    assert_eq!(alone("node", &["node", "/usr/lib/node_modules/@qwen-code/qwen-code/dist/index.js"]), Some(Qwen));
}

#[test]
fn code_given_inline_is_not_a_script() {
    for argv in [
        &["node", "-e", "codex"][..],
        &["node", "--eval=codex"],
        &["node", "-pcodex"],
        &["python3", "-c", "codex"],
        &["python3", "-m", "codex"],
        &["bash", "-c", "codex"],
        &["node", "/path/to/other.js", "cline"],
        &["node", "/path/to/cline-helper"],
    ] {
        assert_eq!(alone(argv[0], argv), None, "{argv:?}");
    }
    assert_eq!(alone("python3", &["python3", "/tmp/codex.py"]), None);
    assert_eq!(alone("python3", &["python3", "/tmp/codex"]), Some(Codex));
    assert_eq!(alone("MainThread", &["/path/to/other", "/path/to/cline"]), None);
    assert_eq!(alone("MainThread", &["node"]), None);
}

#[test]
fn only_interactive_letta_counts() {
    assert_eq!(alone("letta", &["letta", "--backend", "local"]), Some(Letta));
    assert_eq!(alone("MainThread", &["node", "/home/user/project/node_modules/.bin/letta", "--conversation", "c"]), Some(Letta));
    for args in [
        &["--prompt", "hello"][..],
        &["--output-format", "json"],
        &["--input-format=stream-json"],
        &["--max-turns=1"],
        &["server"],
        &["--backend", "local", "server"],
        &["fix this bug"],
        &["agents", "list"],
    ] {
        let mut argv = vec!["node", "/home/user/project/node_modules/.bin/letta"];
        argv.extend(args);
        assert_eq!(alone("MainThread", &argv), None, "{argv:?}");
    }
    assert_eq!(alone("node", &["node", "/home/user/src/letta-code/letta/build.js"]), None);
}

#[cfg(unix)]
#[test]
fn symlinked_launchers_resolve_to_the_real_file() {
    let dir = std::env::temp_dir().join(format!("runode-agent-identify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let real = dir.join("cursor-agent");
    std::fs::write(&real, "").unwrap();
    let link = dir.join("launcher");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_eq!(alone("launcher", &[link.to_str().unwrap()]), Some(Cursor));
    let _ = std::fs::remove_dir_all(&dir);
}
