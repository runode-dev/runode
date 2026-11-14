//! `runode skill install`：不经宿主，装进临时的家目录。装法的细节由 `runode-agent-skill` 自己的测试管，这里
//! 看命令行打印了什么、正本和 agent 那边的位置有没有装上。

mod common;

use common::run;
use runode_agent_skill::BUNDLED;
use runode_cli::{Env, exit};
use runode_paths::Dirs;

#[test]
fn install_writes_the_skill_and_reports_each_place() {
    let root = std::env::temp_dir().join(format!("rns-{}-install", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    // Kiro 那边的位置上已经是用户自己的文件。
    std::fs::create_dir_all(home.join(".kiro/skills")).unwrap();
    std::fs::write(home.join(".kiro/skills/runode"), "mine").unwrap();
    // 命令从进程的环境变量读 Claude Code 的配置目录和 skills CLI 的 lock 位置，指到临时目录里，不碰跑测试的
    // 人自己的。
    // SAFETY: 这个测试程序只有这一个测试，这时还没有起别的线程读写环境变量。
    unsafe {
        std::env::set_var("CLAUDE_CONFIG_DIR", home.join(".claude"));
        std::env::set_var("XDG_STATE_HOME", root.join("state"));
    }
    let env =
        Env { dirs: Dirs { home: Some(home.clone()), ..Dirs::default() }, build: "test".into(), ..Env::default() };

    for _ in 0..2 {
        let (code, out, err) = run("skill install", &env);
        assert_eq!(code, exit::OK, "{err}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            [
                "Installed the runode skill in ~/.agents/skills/runode",
                "Claude Code reads it from ~/.claude/skills/runode",
                "Skipped ~/.kiro/skills/runode: something of yours is there, left untouched.",
            ]
        );
    }

    let runode = BUNDLED.iter().find(|skill| skill.name == "runode").unwrap();
    let canonical = home.join(".agents/skills/runode");
    assert_eq!(std::fs::read_to_string(canonical.join("SKILL.md")).unwrap(), runode.files[0].1);
    assert!(canonical.join(".runode-files").is_file());
    let linked = home.join(".claude/skills/runode");
    if cfg!(windows) {
        assert!(linked.join("SKILL.md").is_file());
    } else {
        assert_eq!(std::fs::read_link(&linked).unwrap(), canonical);
    }
    assert_eq!(std::fs::read_to_string(home.join(".kiro/skills/runode")).unwrap(), "mine");

    // 没有家目录时装不了，报出原因。
    let (code, _, err) = run("skill install", &Env { build: "test".into(), ..Env::default() });
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("cannot install the runode skill"), "{err}");

    let _ = std::fs::remove_dir_all(&root);
}
