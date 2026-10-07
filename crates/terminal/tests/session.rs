//! 界面那份会话：不应答任何终端查询，宿主公布的状态带来标题、agent 和目录。

mod common;

use common::capturing_session;
use runode_shared_types::{agent::Agent, grid::GridSize, session::SessionMeta};

/// 界面这份 VT 不应答任何查询：喂进 DA、DSR、XTVERSION、OSC 颜色、Kitty 键盘协议和尺寸
/// 这些查询，也不会交出一个字节。
#[test]
fn the_view_terminal_never_answers_queries() {
    let (mut session, requests) = capturing_session();
    session.feed(b"\x1b[c\x1b[>c\x1b[5n\x1b[6n\x1b[>q\x1b]10;?\x07\x1b]11;?\x07\x1b]4;1;?\x07\x1b[?u\x1b[18t\x1b[14t\x1b[?2048h\x1b[?1$p");
    session.apply_resized(GridSize { cols: 30, rows: 5, cell_width_px: 8, cell_height_px: 16 });
    assert!(requests.borrow().is_empty(), "{:?}", requests.borrow());
}

#[test]
fn meta_from_the_host_sets_the_title_and_agent() {
    use runode_shared_types::agent::{AgentKind, AgentState};

    let (mut session, _) = capturing_session();
    let agent = Some(Agent { kind: AgentKind::Claude, state: AgentState::Working });
    assert!(session.apply_meta(SessionMeta { title: Some("修 bug".into()), agent, ..SessionMeta::default() }));
    // 别的字段变了不算标题变化。
    assert!(!session.apply_meta(SessionMeta {
        title: Some("修 bug".into()),
        agent,
        cwd: Some("/tmp".into()),
        ..SessionMeta::default()
    }));
    assert_eq!(session.cwd(), Some("/tmp".into()));
    assert_eq!(session.prompt_cwd(), Some("/tmp".into()));
}
