//! 会话对外公布的状态 `SessionMeta` 的 JSON 读写。

use runode_shared_types::{
    agent::{Agent, AgentKind, AgentState},
    session::SessionMeta,
    shell::ShellNames,
};

#[test]
fn missing_fields_read_as_defaults() {
    let meta: SessionMeta = serde_json::from_str(r#"{"title":"vim","unknown":1}"#).unwrap();
    assert_eq!(meta, SessionMeta { title: Some("vim".into()), ..SessionMeta::default() });
}

#[test]
fn round_trips_through_json() {
    let meta = SessionMeta {
        title: Some("修 bug".into()),
        fallback_title: Some("runode".into()),
        agent: Some(Agent { kind: AgentKind::GithubCopilot, state: AgentState::Blocked }),
        cwd: Some("/tmp/中文".into()),
        prompt_cwd: Some("/tmp".into()),
        foreground_is_shell: false,
        shell_path: Some("/usr/bin:/bin".into()),
        shell_names: ShellNames { aliases: vec!["ll".into()], ..ShellNames::default() },
    };
    let json = serde_json::to_string(&meta).unwrap();
    assert!(json.contains(r#""kind":"github_copilot","state":"blocked""#), "{json}");
    assert_eq!(serde_json::from_str::<SessionMeta>(&json).unwrap(), meta);
}
