//! 从窗口标题里分出 agent 的状态前缀，普通标题原样留着。

use runode_agent_detect::title::{display_title, split_status};
use runode_shared_types::agent::{Agent, AgentKind, AgentState};

fn agent(kind: AgentKind, state: AgentState) -> Agent {
    Agent { kind, state }
}

#[test]
fn splits_claude_and_codex_status_prefixes() {
    use AgentKind::*;
    use AgentState::*;
    assert_eq!(split_status("✳ 确认回复"), Some((agent(Claude, Idle), "确认回复")));
    assert_eq!(split_status("◑ Tabs UI/UX 优化"), Some((agent(Claude, Working), "Tabs UI/UX 优化")));
    assert_eq!(split_status("⠴ 美化图标 | runode"), Some((agent(Codex, Working), "美化图标 | runode")));
    assert_eq!(split_status("⠴ π - runode"), Some((agent(Pi, Working), "π - runode")));
    assert_eq!(display_title("✳ 确认回复"), "确认回复");
}

#[test]
fn leaves_ordinary_titles_alone() {
    assert_eq!(split_status("美化图标 | runode"), None);
    assert_eq!(split_status("vim"), None);
    // 状态字符后面必须跟空格，免得把正文的第一个字吃掉。
    assert_eq!(split_status("◐x"), None);
    assert_eq!(split_status(""), None);
    assert_eq!(display_title("◐x"), "◐x");
}
