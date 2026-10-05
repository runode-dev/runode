//! 用户目录里的规则覆盖内置的同名规则，写错的退回内置的并留下警告。

use runode_agent_detect::{RuleBook, Signals, Verdict};
use runode_shared_types::agent::AgentKind;

#[test]
fn user_rules_override_the_builtin_ones() {
    let dir = std::env::temp_dir().join(format!("runode-agent-rules-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let rule = |id: &str| format!("id = \"{id}\"\n[[rules]]\nid = \"mine\"\nstate = \"blocked\"\ncontains = [\"MARK\"]\n");
    std::fs::write(dir.join("codex.toml"), rule("codex")).unwrap();
    // id 对不上、写错了的不用，退回内置的并留下警告。
    std::fs::write(dir.join("gemini.toml"), rule("claude")).unwrap();
    std::fs::write(dir.join("amp.toml"), "id = ").unwrap();
    // 没有内置规则的 agent 也能用用户规则。
    std::fs::write(dir.join("omp.toml"), rule("omp")).unwrap();
    let book = RuleBook::new(Some(&dir));
    let signals = Signals { screen: "MARK", ..Default::default() };
    let codex = book.rules(AgentKind::Codex).unwrap();
    let verdict = codex.evaluate(signals);
    assert!(matches!(verdict, Verdict::Matched { rule: "mine", .. }), "{verdict:?}");
    assert!(book.rules(AgentKind::Omp).is_some());
    assert!(book.rules(AgentKind::Gemini).is_some());
    assert!(book.rules(AgentKind::Amp).is_some());
    assert_eq!(book.take_warnings().len(), 2);
    assert!(book.take_warnings().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
