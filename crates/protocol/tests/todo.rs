//! 任务板的数据：卡片在哪一列、拖拽放下时做什么，以及 `When` 的写法。

use std::path::PathBuf;

use runode_protocol::{
    SessionId, SessionInfo,
    todo::{Column, DropAction, START_GRACE, Todo, TodoSpec, When, column, drop_action},
};
use runode_shared_types::{
    agent::{Agent, AgentKind, AgentState},
    grid::GridSize,
    session::SessionMeta,
};

const STARTED: u64 = 1_000_000;
const SESSION: SessionId = SessionId(7);

fn todo() -> Todo {
    Todo {
        id: "0000abcd".into(),
        spec: TodoSpec {
            title: "Fix login".into(),
            prompt: String::new(),
            dir: PathBuf::from("/src/app"),
            agent: "claude".into(),
            worktree: false,
        },
        created: STARTED - 60,
        started: None,
        session: None,
        worktree_dir: None,
        branch: None,
        done_at: None,
        schedule: None,
        rank: 0,
    }
}

fn started() -> Todo {
    Todo { started: Some(STARTED), session: Some(SESSION), ..todo() }
}

fn session(agent: Option<AgentState>, foreground_is_shell: bool) -> SessionInfo {
    SessionInfo {
        id: SESSION,
        size: GridSize { cols: 80, rows: 24, cell_width_px: 0, cell_height_px: 0 },
        meta: SessionMeta {
            agent: agent.map(|state| Agent { kind: AgentKind::Claude, state }),
            foreground_is_shell,
            ..SessionMeta::default()
        },
        clients: 0,
        claimed: false,
        exited: false,
        size_owner: None,
    }
}

const FRESH: u64 = STARTED + 1;
const LATER: u64 = STARTED + START_GRACE;

#[test]
fn columns_follow_the_task_and_its_session() {
    let working = session(Some(AgentState::Working), false);
    let blocked = session(Some(AgentState::Blocked), false);
    let idle = session(Some(AgentState::Idle), false);
    let shell = session(None, true);
    let unknown_program = session(None, false);

    assert_eq!(column(&todo(), None, LATER), Column::Todo);
    let done = Todo { done_at: Some(LATER), ..started() };
    assert_eq!(column(&done, Some(&working), LATER), Column::Done);

    // 会话没了、对不上、shell 退出了：要用户看一眼。
    assert_eq!(column(&started(), None, FRESH), Column::NeedsYou);
    let other = SessionInfo { id: SessionId(8), ..working.clone() };
    assert_eq!(column(&started(), Some(&other), FRESH), Column::NeedsYou);
    let exited = SessionInfo { exited: true, ..working.clone() };
    assert_eq!(column(&started(), Some(&exited), FRESH), Column::NeedsYou);

    for now in [FRESH, LATER] {
        assert_eq!(column(&started(), Some(&working), now), Column::InProgress);
        assert_eq!(column(&started(), Some(&blocked), now), Column::NeedsYou);
        assert_eq!(column(&started(), Some(&unknown_program), now), Column::InProgress);
    }
    // 刚开始时 agent 还没起来或还没干活，算进行中；过了宽限期就是停下来了。
    assert_eq!(column(&started(), Some(&idle), FRESH), Column::InProgress);
    assert_eq!(column(&started(), Some(&idle), LATER), Column::NeedsYou);
    assert_eq!(column(&started(), Some(&shell), FRESH), Column::InProgress);
    assert_eq!(column(&started(), Some(&shell), LATER), Column::NeedsYou);
    // 时钟往回调过，开始时间比现在晚：也算刚开始。
    assert_eq!(column(&started(), Some(&shell), STARTED - 100), Column::InProgress);
}

#[test]
fn a_drop_does_what_the_target_column_means_or_bounces() {
    let working = session(Some(AgentState::Working), false);
    let blocked = session(Some(AgentState::Blocked), false);

    // 同一列里只调先后。
    assert_eq!(drop_action(&todo(), None, Column::Todo, LATER), Some(DropAction::Reorder));
    // 待办拖进进行中就开始，拖进完成就标完成；拖进需要你不接。
    assert_eq!(drop_action(&todo(), None, Column::InProgress, LATER), Some(DropAction::Start));
    assert_eq!(drop_action(&todo(), None, Column::Done, LATER), Some(DropAction::Finish));
    assert_eq!(drop_action(&todo(), None, Column::NeedsYou, LATER), None);

    // 开始过的拖不回待办；需要你和进行中之间由 agent 定，不能拖。
    assert_eq!(drop_action(&started(), Some(&working), Column::Todo, LATER), None);
    assert_eq!(drop_action(&started(), Some(&working), Column::NeedsYou, LATER), None);
    assert_eq!(drop_action(&started(), Some(&blocked), Column::InProgress, LATER), None);
    assert_eq!(drop_action(&started(), Some(&blocked), Column::Done, LATER), Some(DropAction::Finish));

    // 从完成拖出去：取消完成后落在哪列，就只能拖到哪列。
    let done_unstarted = Todo { done_at: Some(LATER), ..todo() };
    assert_eq!(drop_action(&done_unstarted, None, Column::Todo, LATER), Some(DropAction::Reopen));
    assert_eq!(drop_action(&done_unstarted, None, Column::InProgress, LATER), None);
    let done_working = Todo { done_at: Some(LATER), ..started() };
    assert_eq!(drop_action(&done_working, Some(&working), Column::InProgress, LATER), Some(DropAction::Reopen));
    assert_eq!(drop_action(&done_working, Some(&working), Column::NeedsYou, LATER), None);
    assert_eq!(drop_action(&done_working, None, Column::NeedsYou, LATER), Some(DropAction::Reopen));
}

#[test]
fn spec_prompt_falls_back_to_the_title() {
    let mut spec = todo().spec;
    assert_eq!(spec.prompt(), "Fix login");
    spec.prompt = "Fix the login redirect loop".into();
    assert_eq!(spec.prompt(), "Fix the login redirect loop");
}

#[test]
fn spec_fills_in_the_default_agent() {
    let spec: TodoSpec = serde_json::from_str(r#"{"title":"t","dir":"/src"}"#).unwrap();
    assert_eq!((spec.agent.as_str(), spec.worktree, spec.prompt.as_str()), ("claude", false, ""));
}

#[test]
fn when_is_tagged_by_kind_and_unknown_kinds_never_fire() {
    let daily = When::Daily { hour: 9, minute: 30, days: 0b0011_1110 };
    let json = serde_json::to_string(&daily).unwrap();
    assert_eq!(json, r#"{"kind":"daily","hour":9,"minute":30,"days":62}"#);
    assert_eq!(serde_json::from_str::<When>(&json).unwrap(), daily);
    assert_eq!(
        serde_json::from_str::<When>(r#"{"kind":"daily","hour":9,"minute":0}"#).unwrap(),
        When::Daily { hour: 9, minute: 0, days: 0 }
    );
    assert_eq!(serde_json::from_str::<When>(r#"{"kind":"every","minutes":15}"#).unwrap(), When::Every { minutes: 15 });
    assert_eq!(serde_json::from_str::<When>(r#"{"kind":"once","at":5}"#).unwrap(), When::Once { at: 5 });
    assert_eq!(serde_json::from_str::<When>(r#"{"kind":"cron","expr":"* * * * *"}"#).unwrap(), When::Unknown);
}
