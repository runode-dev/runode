//! 把 skill 装进临时的家目录：正本、记录、agent 那边的链接，已经有东西时怎么办，`refresh` 什么时候重装，
//! 以及 skills CLI 的 lock 文件。

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use runode_agent_skill::{BUNDLED, Env, Skill, State, install, refresh, state};
use runode_paths::Dirs;

/// 测完删掉的临时家目录。
struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("runode-agent-skill-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// 家目录是这里、另设了 `vars` 的环境。
    fn env(&self, vars: &[(&str, &Path)]) -> Env {
        let var = |key: &str| match key {
            "HOME" | "USERPROFILE" => Some(self.0.clone().into()),
            _ => vars.iter().find(|(name, _)| *name == key).map(|(_, value)| value.into()),
        };
        Env::from_dirs(&Dirs::from_vars(var), var)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    fn mkdir(&self, relative: &str) -> PathBuf {
        let dir = self.path(relative);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn runode() -> &'static Skill {
    BUNDLED.iter().find(|skill| skill.name == "runode").unwrap()
}

fn record(skill: &Skill) -> String {
    skill.files.iter().map(|(path, _)| format!("{path}\n")).collect()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn first_install_writes_the_canonical_copy_and_a_record() {
    let home = Home::new();
    let env = home.env(&[]);
    assert_eq!(state(&env, "runode"), State::Missing);
    let report = install(&env, runode()).unwrap();
    let canonical = home.path(".agents/skills/runode");
    assert_eq!((report.skill, &report.canonical, report.left_as_is), ("runode", &canonical, false));
    for (path, content) in runode().files {
        assert_eq!(read(&canonical.join(path)), *content, "{path}");
    }
    assert_eq!(read(&canonical.join(".runode-files")), record(runode()));
    // 没装 Claude Code 这类 agent 时不替它建目录。
    assert!(report.linked.is_empty() && report.skipped.is_empty());
    assert!(!home.path(".claude").exists());
    assert_eq!(state(&env, "runode"), State::Installed { by_runode: true, current: true });
}

#[test]
fn installed_agents_get_a_link_and_installing_again_changes_nothing() {
    let home = Home::new();
    home.mkdir(".claude");
    home.mkdir(".kiro");
    let env = home.env(&[]);
    let first = install(&env, runode()).unwrap();
    let claude = home.path(".claude/skills/runode");
    let kiro = home.path(".kiro/skills/runode");
    assert_eq!(first.linked, vec![("Claude Code", claude.clone()), ("Kiro CLI", kiro)]);
    assert!(!home.path(".gemini").exists());
    assert_eq!(read(&claude.join("SKILL.md")), runode().files[0].1);
    #[cfg(unix)]
    assert_eq!(fs::read_link(&claude).unwrap(), home.path(".agents/skills/runode"));

    let second = install(&env, runode()).unwrap();
    assert_eq!(second, first);
    assert_eq!(read(&home.path(".agents/skills/runode/.runode-files")), record(runode()));
}

#[test]
fn claude_config_dir_is_where_claude_gets_its_link() {
    let home = Home::new();
    home.mkdir(".claude");
    let config = home.mkdir("claude-config");
    let env = home.env(&[("CLAUDE_CONFIG_DIR", &config)]);
    let report = install(&env, runode()).unwrap();
    assert_eq!(report.linked, vec![("Claude Code", config.join("skills/runode"))]);
    assert!(!home.path(".claude/skills").exists());

    // 设了但目录不在：当作没装 Claude Code。
    let other = Home::new();
    let env = other.env(&[("CLAUDE_CONFIG_DIR", &other.path("missing"))]);
    assert!(install(&env, runode()).unwrap().linked.is_empty());
}

#[cfg(unix)]
#[test]
fn a_canonical_copy_that_is_a_symlink_is_left_alone() {
    let home = Home::new();
    let elsewhere = home.mkdir("checkout/runode");
    fs::write(elsewhere.join("SKILL.md"), "mine").unwrap();
    home.mkdir(".agents/skills");
    let canonical = home.path(".agents/skills/runode");
    std::os::unix::fs::symlink(&elsewhere, &canonical).unwrap();
    home.mkdir(".claude");
    let env = home.env(&[]);
    assert_eq!(state(&env, "runode"), State::Elsewhere);

    let report = install(&env, runode()).unwrap();
    assert!(report.left_as_is);
    assert_eq!(report.canonical, canonical);
    assert_eq!(fs::read_link(&canonical).unwrap(), elsewhere);
    assert_eq!(read(&elsewhere.join("SKILL.md")), "mine");
    assert!(!elsewhere.join(".runode-files").exists());
    assert_eq!(fs::read_link(home.path(".claude/skills/runode")).unwrap(), canonical);
    assert!(!refresh(&env).unwrap());
    assert_eq!(read(&elsewhere.join("SKILL.md")), "mine");
}

/// agent 的 skills 目录本身链到 `~/.agents/skills`：那边的 skill 就是正本，装和重装都不能删掉它。
#[cfg(unix)]
#[test]
fn an_agent_skills_directory_linked_to_the_canonical_one_keeps_the_canonical_copy() {
    let home = Home::new();
    home.mkdir(".agents/skills");
    home.mkdir(".claude");
    std::os::unix::fs::symlink("../.agents/skills", home.path(".claude/skills")).unwrap();
    let canonical = home.path(".agents/skills/runode");
    let env = home.env(&[]);
    for _ in 0..2 {
        let report = install(&env, runode()).unwrap();
        assert_eq!(report.linked, vec![("Claude Code", home.path(".claude/skills/runode"))]);
        assert!(!fs::symlink_metadata(&canonical).unwrap().file_type().is_symlink());
        assert_eq!(read(&canonical.join("SKILL.md")), runode().files[0].1);
    }
    assert_eq!(state(&env, "runode"), State::Installed { by_runode: true, current: true });
}

#[cfg(unix)]
#[test]
fn an_agent_link_pointing_elsewhere_is_skipped_and_one_to_the_canonical_copy_is_kept() {
    let home = Home::new();
    let elsewhere = home.mkdir("other/runode");
    home.mkdir(".claude/skills");
    home.mkdir(".kiro/skills");
    let claude = home.path(".claude/skills/runode");
    let kiro = home.path(".kiro/skills/runode");
    std::os::unix::fs::symlink(&elsewhere, &claude).unwrap();
    // skills CLI 建的是相对链接。
    std::os::unix::fs::symlink("../../.agents/skills/runode", &kiro).unwrap();
    home.mkdir(".agents/skills/runode");

    let report = install(&home.env(&[]), runode()).unwrap();
    assert_eq!(report.skipped, vec![claude.clone()]);
    assert_eq!(report.linked, vec![("Kiro CLI", kiro.clone())]);
    assert_eq!(fs::read_link(&claude).unwrap(), elsewhere);
    assert_eq!(fs::read_link(&kiro).unwrap(), Path::new("../../.agents/skills/runode"));
}

#[test]
fn an_agent_directory_runode_copied_is_replaced_and_another_gets_the_files() {
    let home = Home::new();
    let claude = home.mkdir(".claude/skills/runode");
    fs::write(claude.join("SKILL.md"), "old").unwrap();
    home.mkdir(".claude/skills/runode/references");
    fs::write(claude.join("references/stale.md"), "old").unwrap();
    fs::write(claude.join(".runode-files"), "SKILL.md\nreferences/stale.md\n").unwrap();
    let kiro = home.mkdir(".kiro/skills/runode");
    fs::write(kiro.join("notes.md"), "mine").unwrap();

    let report = install(&home.env(&[]), runode()).unwrap();
    assert_eq!(report.linked, vec![("Claude Code", claude.clone()), ("Kiro CLI", kiro.clone())]);
    #[cfg(unix)]
    assert_eq!(fs::read_link(&claude).unwrap(), home.path(".agents/skills/runode"));
    assert!(!claude.join("references/stale.md").exists());
    assert_eq!(read(&claude.join("SKILL.md")), runode().files[0].1);
    // 别人放的目录：文件写进去、带上记录，自己的文件留着。
    assert_eq!(read(&kiro.join("SKILL.md")), runode().files[0].1);
    assert_eq!(read(&kiro.join(".runode-files")), record(runode()));
    assert_eq!(read(&kiro.join("notes.md")), "mine");
}

/// agent 那边是用户自己放的一份 skill（没有记录）：不覆盖，算跳过。
#[test]
fn an_agent_skill_of_the_user_is_skipped() {
    let home = Home::new();
    let claude = home.mkdir(".claude/skills/runode");
    fs::write(claude.join("SKILL.md"), "mine").unwrap();

    let report = install(&home.env(&[]), runode()).unwrap();
    assert_eq!(report.skipped, vec![claude.clone()]);
    assert!(report.linked.is_empty());
    assert_eq!(read(&claude.join("SKILL.md")), "mine");
    assert!(!claude.join(".runode-files").exists());
}

/// 写过记录的目录里另有用户的文件：再装、或者 refresh 重装时都只覆盖写，用户的文件一直在。
#[test]
fn files_of_the_user_in_an_agent_directory_survive_reinstalling() {
    let home = Home::new();
    let kiro = home.mkdir(".kiro/skills/runode");
    fs::write(kiro.join("notes.md"), "mine").unwrap();
    let env = home.env(&[]);
    install(&env, runode()).unwrap();
    install(&env, runode()).unwrap();
    assert_eq!(read(&kiro.join("notes.md")), "mine");
    assert_eq!(read(&kiro.join("SKILL.md")), runode().files[0].1);

    fs::write(home.path(".agents/skills/runode/SKILL.md"), "old").unwrap();
    assert!(refresh(&env).unwrap());
    assert_eq!(read(&kiro.join("notes.md")), "mine");
    assert_eq!(read(&kiro.join("SKILL.md")), runode().files[0].1);
    assert!(kiro.is_dir() && !fs::symlink_metadata(&kiro).unwrap().file_type().is_symlink());
}

/// runode 放的副本里用户后加了文件（Windows 上的副本、或者别处复制来的）：不换成链接，只覆盖写，用户的文件留着。
#[test]
fn a_runode_copy_holding_files_of_the_user_is_not_replaced() {
    let home = Home::new();
    let claude = home.mkdir(".claude/skills/runode");
    fs::write(claude.join("SKILL.md"), "old").unwrap();
    fs::write(claude.join(".runode-files"), "SKILL.md\n").unwrap();
    home.mkdir(".claude/skills/runode/references");
    fs::write(claude.join("references/mine.md"), "mine").unwrap();
    let env = home.env(&[]);
    for _ in 0..2 {
        install(&env, runode()).unwrap();
        assert!(!fs::symlink_metadata(&claude).unwrap().file_type().is_symlink());
        assert_eq!(read(&claude.join("references/mine.md")), "mine");
        assert_eq!(read(&claude.join("SKILL.md")), runode().files[0].1);
        assert_eq!(read(&claude.join(".runode-files")), record(runode()));
    }
}

#[test]
fn files_dropped_from_a_skill_are_removed() {
    static OLD: Skill = Skill { name: "demo", files: &[("SKILL.md", "v1"), ("references/a.md", "# A\n")] };
    static NEW: Skill = Skill { name: "demo", files: &[("SKILL.md", "v2")] };
    let home = Home::new();
    let env = home.env(&[]);
    install(&env, &OLD).unwrap();
    let dir = home.path(".agents/skills/demo");
    fs::write(dir.join("notes.md"), "mine").unwrap();

    install(&env, &NEW).unwrap();
    assert_eq!(read(&dir.join("SKILL.md")), "v2");
    assert!(!dir.join("references").exists());
    assert_eq!(read(&dir.join("notes.md")), "mine");
    assert_eq!(read(&dir.join(".runode-files")), "SKILL.md\n");
}

#[test]
fn taking_over_a_copy_from_the_skills_cli_keeps_files_it_did_not_write() {
    let home = Home::new();
    let canonical = home.mkdir(".agents/skills/runode");
    fs::write(canonical.join("SKILL.md"), "from npx").unwrap();
    fs::write(canonical.join("extra.md"), "kept").unwrap();
    let env = home.env(&[]);
    assert_eq!(state(&env, "runode"), State::Installed { by_runode: false, current: false });

    install(&env, runode()).unwrap();
    assert_eq!(read(&canonical.join("SKILL.md")), runode().files[0].1);
    assert_eq!(read(&canonical.join("extra.md")), "kept");
    assert_eq!(state(&env, "runode"), State::Installed { by_runode: true, current: true });
}

#[test]
fn refresh_reinstalls_only_what_runode_installed_and_is_out_of_date() {
    let home = Home::new();
    home.mkdir(".claude");
    let env = home.env(&[]);
    // 没装过的不装。
    assert!(!refresh(&env).unwrap());
    assert!(!home.path(".agents").exists());

    // 别的工具装的（没有记录）不碰。
    let canonical = home.mkdir(".agents/skills/runode");
    fs::write(canonical.join("SKILL.md"), "from npx").unwrap();
    assert!(!refresh(&env).unwrap());
    assert_eq!(read(&canonical.join("SKILL.md")), "from npx");

    install(&env, runode()).unwrap();
    assert!(!refresh(&env).unwrap());

    // 改了、缺了的换回这一版，agent 那边缺的链接一起补上。
    fs::write(canonical.join("SKILL.md"), "old").unwrap();
    let reference = canonical.join(runode().files[1].0);
    fs::remove_file(&reference).unwrap();
    fs::remove_file(home.path(".claude/skills/runode")).unwrap();
    assert_eq!(state(&env, "runode"), State::Installed { by_runode: true, current: false });
    assert!(refresh(&env).unwrap());
    assert_eq!(read(&canonical.join("SKILL.md")), runode().files[0].1);
    assert!(reference.exists());
    assert!(home.path(".claude/skills/runode/SKILL.md").exists());
    assert!(!refresh(&env).unwrap());
}

const LOCK: &str = r#"{
  "version": 3,
  "skills": {
    "other": {
      "source": "someone/else",
      "sourceType": "github"
    },
    "runode": {
      "source": "runode-dev/runode",
      "sourceType": "github",
      "skillPath": "skills/runode/SKILL.md"
    },
    "last": {
      "source": "someone/else"
    }
  },
  "dismissed": {},
  "lastSelectedAgents": [
    "claude-code"
  ]
}"#;

#[test]
fn installing_removes_the_skills_cli_lock_entry() {
    let home = Home::new();
    let lock = home.path(".agents/.skill-lock.json");
    home.mkdir(".agents");
    fs::write(&lock, LOCK).unwrap();
    install(&home.env(&[]), runode()).unwrap();
    let expected = r#"{
  "version": 3,
  "skills": {
    "other": {
      "source": "someone/else",
      "sourceType": "github"
    },
    "last": {
      "source": "someone/else"
    }
  },
  "dismissed": {},
  "lastSelectedAgents": [
    "claude-code"
  ]
}"#;
    assert_eq!(read(&lock), expected);
}

#[test]
fn the_lock_under_xdg_state_home_is_used() {
    let home = Home::new();
    let state_home = home.mkdir("state");
    let lock = home.mkdir("state/skills").join(".skill-lock.json");
    fs::write(&lock, format!("{LOCK}\n")).unwrap();
    let untouched = home.mkdir(".agents").join(".skill-lock.json");
    fs::write(&untouched, LOCK).unwrap();
    install(&home.env(&[("XDG_STATE_HOME", &state_home)]), runode()).unwrap();
    let text = read(&lock);
    assert!(!text.contains("runode-dev/runode") && text.contains("\"other\"") && text.ends_with("}\n"), "{text}");
    assert_eq!(read(&untouched), LOCK);
}

#[test]
fn a_lock_it_does_not_understand_or_an_entry_from_elsewhere_is_left_alone() {
    let cases = [
        LOCK.replace("\"version\": 3", "\"version\": 4"),
        LOCK.replace("runode-dev/runode", "fork/runode"),
        "not json".to_owned(),
    ];
    for text in cases {
        let home = Home::new();
        let lock = home.mkdir(".agents").join(".skill-lock.json");
        fs::write(&lock, &text).unwrap();
        install(&home.env(&[]), runode()).unwrap();
        assert_eq!(read(&lock), text);
    }
}

#[test]
fn refresh_leaves_the_lock_alone() {
    let home = Home::new();
    let env = home.env(&[]);
    install(&env, runode()).unwrap();
    let lock = home.path(".agents/.skill-lock.json");
    fs::write(&lock, LOCK).unwrap();
    fs::write(home.path(".agents/skills/runode/SKILL.md"), "old").unwrap();
    assert!(refresh(&env).unwrap());
    assert_eq!(read(&lock), LOCK);
}
