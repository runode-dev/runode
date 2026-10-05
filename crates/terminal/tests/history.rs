//! 命令历史的公开接口：记录的 JSON 写法、`Suggester` 按前缀和目录逐步缩小的建议、下一个词的
//! 边界和哪些命令不记。

use std::path::Path;

use runode_terminal::history::{Entry, History, Suggester, next_word, worth_recording};

fn entry(cmd: &str, cwd: Option<&str>) -> Entry {
    Entry { cmd: cmd.into(), cwd: cwd.map(Into::into), exit: None, ts: 0 }
}

#[test]
fn entries_round_trip_through_json() {
    let e = Entry { cmd: "cargo test".into(), cwd: Some("/x".into()), exit: Some(1), ts: 9 };
    let line = serde_json::to_string(&e).unwrap();
    assert_eq!(line, r#"{"cmd":"cargo test","cwd":"/x","exit":1,"ts":9}"#);
    assert_eq!(serde_json::from_str::<Entry>(&line).unwrap(), e);
}

#[test]
fn suggester_matches_a_full_scan_while_narrowing() {
    let mut history = History::from_entries(vec![
        entry("cargo build", Some("/a")),
        entry("cargo test", None),
        entry("cat file", None),
    ]);
    let mut suggester = Suggester::default();
    let a = Some(Path::new("/a"));
    assert_eq!(suggester.suggest(&history, "c", a), Some("argo build".into()));
    assert_eq!(suggester.suggest(&history, "ca", None), Some("t file".into()));
    assert_eq!(suggester.suggest(&history, "car", None), Some("go test".into()));
    assert_eq!(suggester.suggest(&history, "cargo t", None), Some("est".into()));
    // 删掉字符后前缀变短，要重新扫。
    assert_eq!(suggester.suggest(&history, "cargo ", a), Some("build".into()));
    // 历史变了，旧的候选作废。
    history.push(entry("cargo clippy", Some("/a")));
    assert_eq!(suggester.suggest(&history, "cargo c", a), Some("lippy".into()));
    assert_eq!(suggester.suggest(&history, "cargo b", a), Some("uild".into()));
    assert_eq!(suggester.suggest(&history, " ", a), None);
}

#[test]
fn next_word_stops_at_spaces_and_path_separators() {
    assert_eq!(next_word(" status --short"), " status");
    assert_eq!(next_word("atus --short"), "atus");
    assert_eq!(next_word("src/runode/main.rs"), "src/");
    assert_eq!(next_word("/usr/bin"), "/usr/");
    assert_eq!(next_word("bin"), "bin");
    assert_eq!(next_word("  "), "  ");
}

#[test]
fn commands_starting_with_a_space_are_not_recorded() {
    assert!(worth_recording("ls"));
    assert!(!worth_recording(" secret"));
    assert!(!worth_recording("   "));
}
