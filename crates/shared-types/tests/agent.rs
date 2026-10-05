//! agent 种类的公开常量和标签。

use runode_shared_types::agent::AgentKind;

#[test]
fn labels_are_unique() {
    let mut labels: Vec<_> = AgentKind::ALL.iter().map(|kind| kind.label()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(labels.len(), AgentKind::ALL.len());
    assert!(!AgentKind::ALL.contains(&AgentKind::Other));
}
