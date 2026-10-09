//! 按前台进程认出 agent：进程名和别名、组长优先、argv[0]、经解释器跑的脚本、内联代码和
//! 符号链接的启动器。

use AgentKind::*;
use runode_agent_detect::{ForegroundJob, ForegroundProcess, agent_from_name, identify_job};
use runode_shared_types::agent::AgentKind;

fn process(pid: u32, name: &str, argv: &[&str]) -> ForegroundProcess {
    ForegroundProcess {
        pid,
        name: name.into(),
        exe: None,
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
        ("augment", Auggie),
        ("continue-cli", ContinueCli),
        ("open-hands", OpenHands),
        ("trae-cli", Trae),
        ("trae-agent", Trae),
        ("codebuddy-code", CodeBuddy),
        ("iflow-cli", Iflow),
        ("mistral-vibe", MistralVibe),
        ("pdx", Plandex),
        ("muse-bin-0.1.0-R708.1", Muse),
        ("/home/user/.local/bin/muse-bin-0.2.1-R1215.1", Muse),
    ] {
        assert_eq!(agent_from_name(name), Some(kind), "{name}");
    }
    for kind in AgentKind::ALL {
        assert_eq!(agent_from_name(kind.label()), Some(kind));
    }
    for name in
        ["bash", "zsh", "vim", "node", "museum", "muse-helper", "musescore", "muse-bin", "muse-bin-", "muse-binary"]
    {
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
    assert_eq!(alone("bash", &["bash", "/home/user/.local/bin/pi"]), Some(Pi));
    assert_eq!(alone("python3.12", &["python3.12", "/home/user/.local/bin/hermes"]), Some(Hermes));
    assert_eq!(
        alone("bun", &["bun", "/home/u/.bun/install/global/node_modules/@oh-my-pi/pi-coding-agent/dist/cli.js"]),
        Some(Omp)
    );
    assert_eq!(alone("node", &["node", "/usr/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js"]), Some(Pi));
    assert_eq!(
        alone("node", &["node", "/usr/lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js"]),
        Some(Pi)
    );
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
    assert_eq!(
        alone("MainThread", &["node", "/home/user/project/node_modules/.bin/letta", "--conversation", "c"]),
        Some(Letta)
    );
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

fn at(exe: &str, argv: &[&str]) -> Option<AgentKind> {
    let mut process = process(123, basename(exe), argv);
    process.exe = Some(exe.into());
    identify_job(&job(vec![process]))
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap()
}

#[test]
fn ambiguous_names_need_an_install_location() {
    // 同名的编辑器、自己编的程序、随手写的脚本都不算。
    assert_eq!(at("/usr/local/bin/kilo", &["kilo", "notes.txt"]), None);
    assert_eq!(alone("kilo", &["kilo", "notes.txt"]), None);
    assert_eq!(at("/home/user/src/amp/target/debug/amp", &["./amp"]), None);
    assert_eq!(at("/opt/homebrew/bin/node", &["node", "cn.js"]), None);
    assert_eq!(alone("bash", &["bash", "/home/user/bin/pi"]), None);
    // 从 npm 全局目录、~/.local/bin、Homebrew 装的照样认。
    assert_eq!(at("/usr/local/lib/node_modules/@kilocode/cli-darwin-arm64/bin/kilo", &["kilo"]), Some(Kilo));
    assert_eq!(alone("node", &["node", "/usr/local/lib/node_modules/@sourcegraph/amp/bin/amp"]), Some(Amp));
    assert_eq!(at("/home/user/.local/bin/goose", &["goose"]), Some(Goose));
    assert_eq!(at("/opt/homebrew/Cellar/crush/0.7.0/bin/crush", &["crush"]), Some(Crush));
    assert_eq!(alone("python3", &["python3", "/home/user/.local/bin/vibe"]), Some(MistralVibe));
    // 带 agent 字样的别名和不撞名的 agent 不受影响。
    assert_eq!(at("/home/user/bin/trae-cli", &["trae-cli"]), Some(Trae));
    assert_eq!(at("/home/user/src/claude", &["claude"]), Some(Claude));
}

#[cfg(unix)]
#[test]
fn npm_bin_links_count_as_installed() {
    // npm 全局的 bin 目录里是指向 node_modules 的符号链接，顺着链接找到安装位置。
    let dir = std::env::temp_dir().join(format!("runode-agent-installed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let package = dir.join("lib/node_modules/@sourcegraph/amp/dist");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(package.join("main.js"), "").unwrap();
    let link = dir.join("bin/amp");
    std::os::unix::fs::symlink(package.join("main.js"), &link).unwrap();
    assert_eq!(alone("node", &["node", link.to_str().unwrap()]), Some(Amp));
    let mine = dir.join("bin/kilo");
    std::fs::write(&mine, "").unwrap();
    assert_eq!(alone("node", &["node", mine.to_str().unwrap()]), None);
    let _ = std::fs::remove_dir_all(&dir);
}
