//! 会话对外公布的状态 `SessionMeta` 的 JSON 读写。

use runode_shared_types::{
    agent::{Agent, AgentKind, AgentState, AgentUsage},
    session::{DriveAction, Driver, SessionMeta},
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
        agent_usage: Some(AgentUsage {
            model: Some("Opus".into()),
            context_tokens: Some(15500),
            ..AgentUsage::default()
        }),
        cwd: Some("/tmp/中文".into()),
        prompt_cwd: Some("/tmp".into()),
        foreground_is_shell: false,
        shell_path: Some("/usr/bin:/bin".into()),
        shell_names: ShellNames { aliases: vec!["ll".into()], ..ShellNames::default() }.into(),
        foreground: Some("vim".into()),
        driver: Some(Driver {
            by: Some("0123456789abcdef0123456789abcdef".into()),
            action: DriveAction::Keys,
            at_ms: 7,
        }),
        pid: Some(4242),
    };
    let json = serde_json::to_string(&meta).unwrap();
    assert!(json.contains(r#""kind":"github_copilot","state":"blocked""#), "{json}");
    assert_eq!(serde_json::from_str::<SessionMeta>(&json).unwrap(), meta);
}

/// 操作记录：`by` 可以缺，新的一方才有的操作读成 `Unknown`，整份状态照样读得了。
#[test]
fn driver_tolerates_missing_and_newer_fields() {
    let meta: SessionMeta =
        serde_json::from_str(r#"{"driver":{"action":"teleport","at_ms":5},"foreground":"zsh"}"#).unwrap();
    assert_eq!(meta.driver, Some(Driver { by: None, action: DriveAction::Unknown, at_ms: 5 }));
    assert_eq!(meta.foreground.as_deref(), Some("zsh"));
    let json = serde_json::to_string(&Driver { by: None, action: DriveAction::ClearScreen, at_ms: 1 }).unwrap();
    assert_eq!(json, r#"{"by":null,"action":"clear_screen","at_ms":1}"#);
}
