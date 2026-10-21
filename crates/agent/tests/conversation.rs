//! 从 agent 自己记的文件认出对话，接回的命令只带得进像对话号的东西。

use runode_agent::conversation::{current, resume_command};
use runode_types::agent::{AgentKind, Conversation};

fn home(name: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("rna-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".claude/sessions")).unwrap();
    home
}

#[test]
fn claude_conversations_are_read_by_pid() {
    let home = home("claude");
    let id = "c5539cb0-8025-413c-9be3-4f0187ac4bba";
    std::fs::write(home.join(".claude/sessions/42.json"), format!(r#"{{"pid":42,"sessionId":"{id}"}}"#)).unwrap();
    std::fs::write(home.join(".claude/sessions/43.json"), r#"{"sessionId":"$(rm -rf ~)"}"#).unwrap();
    let found = Conversation { kind: AgentKind::Claude, id: id.into() };
    assert_eq!(current(AgentKind::Claude, &[7, 42], &home), Some(found));
    assert_eq!(current(AgentKind::Claude, &[43], &home), None);
    assert_eq!(current(AgentKind::Codex, &[42], &home), None);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn only_id_like_conversations_are_resumed() {
    let claude = |id: &str| Conversation { kind: AgentKind::Claude, id: id.into() };
    assert_eq!(resume_command(&claude("ab-12")).as_deref(), Some("claude --resume ab-12"));
    assert_eq!(resume_command(&claude("a; rm -rf ~")), None);
    assert_eq!(resume_command(&claude("")), None);
    assert_eq!(resume_command(&Conversation { kind: AgentKind::Codex, id: "ab".into() }), None);
}
