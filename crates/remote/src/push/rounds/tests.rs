use runode_protocol::{
    SessionInfo,
    todo::{Todo, TodoSpec},
};
use runode_types::{
    agent::{Agent, AgentKind},
    grid::GridSize,
};

use super::*;

const DELAY: Duration = Duration::from_secs(10);
const A: SessionId = SessionId(0xa);
const B: SessionId = SessionId(0xb);

fn session(id: SessionId, state: Option<AgentState>) -> SessionInfo {
    SessionInfo {
        id,
        size: GridSize { cols: 80, rows: 24, cell_width_px: 0, cell_height_px: 0 },
        meta: SessionMeta {
            title: Some(format!("会话 {}", id.0)),
            agent: state.map(|state| Agent { kind: AgentKind::Claude, state }),
            ..SessionMeta::default()
        },
        clients: 0,
        claimed: false,
        exited: false,
        size_owner: None,
    }
}

fn blocked(id: SessionId) -> SessionInfo {
    session(id, Some(AgentState::Blocked))
}

fn working(id: SessionId) -> SessionInfo {
    session(id, Some(AgentState::Working))
}

fn notify(id: SessionId, lines: &[&str]) -> Action {
    Action::Notify(id, Box::new(blocked(id).meta), lines.iter().map(|line| (*line).to_owned()).collect())
}

fn secs(start: Instant, n: u64) -> Instant {
    start + Duration::from_secs(n)
}

/// 等过宽限期才读屏幕、推；一次等回答只推一次，问题变了也不再推；答了再等是新的一次。
#[test]
fn one_push_per_wait() {
    let t = Instant::now();
    let mut rounds = Rounds::new(DELAY, true);
    assert_eq!(rounds.observe(&[working(A)], t), []);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 2)), []);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 10)), []);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 12)), [Action::ReadScreen(A)]);
    // 读屏幕的请求还没回，不再要。
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 14)), []);
    assert_eq!(
        rounds.screen(A, Some("Do you want to proceed?\n❯ 1. Yes\n  2. No\n")),
        [notify(A, &["Do you want to proceed?"])]
    );
    // 还在等、问题变了：不再读屏幕，也不再推。
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 30)), []);
    assert_eq!(rounds.screen(A, Some("Allow edits?\n❯ 1. Yes")), []);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 600)), []);
    // 答了：还没发出去的通知别发了；再等是新的一次，又从宽限期算起。
    assert_eq!(rounds.observe(&[working(A)], secs(t, 602)), [Action::Cancel(A)]);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 604)), []);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 614)), [Action::ReadScreen(A)]);
}

/// 宽限期里答了的不推。
#[test]
fn quick_answers_are_not_pushed() {
    let t = Instant::now();
    let mut rounds = Rounds::new(DELAY, true);
    rounds.observe(&[working(A)], t);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 2)), []);
    assert_eq!(rounds.observe(&[working(A)], secs(t, 8)), [Action::Cancel(A)]);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 14)), []);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 22)), []);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 24)), [Action::ReadScreen(A)]);
}

/// 读屏幕时已经答了：读回来也不推。
#[test]
fn answered_while_reading_is_not_pushed() {
    let t = Instant::now();
    let mut rounds = Rounds::new(Duration::ZERO, true);
    rounds.observe(&[working(A)], t);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 2)), [Action::ReadScreen(A)]);
    rounds.observe(&[working(A)], secs(t, 4));
    assert_eq!(rounds.screen(A, Some("Proceed?")), []);
}

/// 连上后第一份列表里已经在等的不推；之后新等的照常。
#[test]
fn the_first_list_only_records() {
    let t = Instant::now();
    let mut rounds = Rounds::new(DELAY, true);
    assert_eq!(rounds.observe(&[blocked(A), working(B)], t), []);
    assert_eq!(rounds.observe(&[blocked(A), blocked(B)], secs(t, 2)), []);
    assert_eq!(rounds.observe(&[blocked(A), blocked(B)], secs(t, 60)), [Action::ReadScreen(B)]);
    // 断了重连也一样，没回的读屏幕请求不等了。
    rounds.reconnected();
    assert_eq!(rounds.screen(B, Some("x")), []);
    rounds.observe(&[working(A), working(B)], secs(t, 62));
    assert!(rounds.observe(&[blocked(A)], secs(t, 64)).is_empty());
    rounds.reconnected();
    assert!(rounds.observe(&[blocked(A)], secs(t, 66)).is_empty());
    assert!(rounds.observe(&[blocked(A)], secs(t, 100)).is_empty());
}

/// 会话没了、shell 退出了：读回来的屏幕不推，还没发出去的通知也别发了。
#[test]
fn gone_and_exited_sessions_are_not_pushed() {
    let t = Instant::now();
    let mut rounds = Rounds::new(DELAY, true);
    rounds.observe(&[], t);
    rounds.observe(&[blocked(A), blocked(B)], secs(t, 1));
    assert_eq!(rounds.observe(&[blocked(A), blocked(B)], secs(t, 11)), [Action::ReadScreen(A), Action::ReadScreen(B)]);
    let exited = SessionInfo { exited: true, ..blocked(A) };
    assert_eq!(rounds.observe(&[exited], secs(t, 13)), [Action::Cancel(B), Action::Cancel(A)]);
    assert_eq!(rounds.screen(A, Some("x")), []);
    assert_eq!(rounds.screen(B, Some("x")), []);
}

/// 不带屏幕文字时不读屏幕，直接推；读回来的文字（开着文字时发出的请求）也不带。
#[test]
fn without_text_the_screen_is_not_read() {
    let t = Instant::now();
    let mut rounds = Rounds::new(Duration::ZERO, false);
    rounds.observe(&[], t);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 1)), [notify(A, &[])]);
    assert_eq!(rounds.observe(&[blocked(A)], secs(t, 30)), []);
    rounds.set_options(Duration::ZERO, true);
    rounds.observe(&[working(B)], secs(t, 31));
    assert_eq!(rounds.observe(&[blocked(B)], secs(t, 32)), [Action::ReadScreen(B)]);
    rounds.set_options(Duration::ZERO, false);
    assert_eq!(rounds.screen(B, Some("secret")), [notify(B, &[])]);
}

/// 看板上的任务用的 Unix 秒：任务早过了开始后的宽限期。
const UNIX: u64 = 1_760_000_000;

fn todo(id: &str, session: SessionId) -> Todo {
    Todo {
        id: id.into(),
        number: 0,
        spec: TodoSpec {
            title: format!("任务 {id}"),
            prompt: String::new(),
            dir: "/tmp".into(),
            agent: "claude".into(),
            worktree: false,
        },
        created: UNIX - 200,
        started: Some(UNIX - 100),
        session: Some(session),
        worktree_dir: None,
        branch: None,
        done_at: None,
        schedule: None,
        rank: 0,
    }
}

fn idle(id: SessionId) -> SessionInfo {
    session(id, Some(AgentState::Idle))
}

fn done(id: SessionId) -> Action {
    Action::TodoDone(id, "任务 1".into())
}

/// 同一轮的任务表和会话列表，会话列表先到。
fn round(rounds: &mut Rounds, todos: &[Todo], sessions: &[SessionInfo], at: Instant) -> Vec<Action> {
    let mut actions = rounds.todo_sessions(sessions, at, UNIX);
    actions.extend(rounds.set_todos(todos.to_vec(), at, UNIX));
    actions
}

/// 从进行中变成做完，过了宽限期还是做完才推，一次做完只推一次；又干活、又做完是新的一次。
#[test]
fn finished_todos_are_pushed_once() {
    let t = Instant::now();
    let mut rounds = Rounds::new(DELAY, true);
    let todos = [todo("1", A)];
    assert_eq!(round(&mut rounds, &todos, &[working(A)], t), []);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 2)), []);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 10)), []);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 12)), [done(A)]);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 30)), []);
    // agent 退出、前台回到 shell 也还是同一次做完。
    let shell =
        [SessionInfo { meta: SessionMeta { foreground_is_shell: true, ..session(A, None).meta }, ..session(A, None) }];
    assert_eq!(round(&mut rounds, &todos, &shell, secs(t, 32)), []);
    assert_eq!(round(&mut rounds, &todos, &[working(A)], secs(t, 34)), [Action::Cancel(A)]);
    assert_eq!(round(&mut rounds, &todos, &shell, secs(t, 36)), []);
    assert_eq!(round(&mut rounds, &todos, &shell, secs(t, 46)), [done(A)]);
}

/// 等回答也是需要你，但那个有等回答的通知：不推做完；等完答了停下来也不算刚做完。宽限期里变成等
/// 回答、回去干活的也不推。
#[test]
fn waiting_and_quick_resumes_are_not_finished() {
    let t = Instant::now();
    let mut rounds = Rounds::new(DELAY, true);
    let todos = [todo("1", A)];
    round(&mut rounds, &todos, &[working(A)], t);
    assert_eq!(round(&mut rounds, &todos, &[blocked(A)], secs(t, 2)), []);
    assert_eq!(round(&mut rounds, &todos, &[blocked(A)], secs(t, 60)), []);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 62)), []);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 120)), []);

    round(&mut rounds, &todos, &[working(A)], secs(t, 200));
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 202)), []);
    assert_eq!(round(&mut rounds, &todos, &[blocked(A)], secs(t, 206)), []);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 220)), []);
    round(&mut rounds, &todos, &[working(A)], secs(t, 300));
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 302)), []);
    assert_eq!(round(&mut rounds, &todos, &[working(A)], secs(t, 306)), []);
    assert_eq!(round(&mut rounds, &todos, &[working(A)], secs(t, 330)), []);
}

/// 做完了又回去干活：还在退避、没发出去的做完的通知别发了。
#[test]
fn resuming_cancels_the_done_push() {
    let t = Instant::now();
    let mut rounds = Rounds::new(Duration::ZERO, true);
    let todos = [todo("1", A)];
    round(&mut rounds, &todos, &[working(A)], t);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 2)), [done(A)]);
    assert_eq!(round(&mut rounds, &todos, &[working(A)], secs(t, 4)), [Action::Cancel(A)]);
    assert_eq!(round(&mut rounds, &todos, &[working(A)], secs(t, 6)), []);
    // 没推过的不撤。
    assert_eq!(round(&mut rounds, &todos, &[blocked(A)], secs(t, 8)), []);
    assert_eq!(round(&mut rounds, &todos, &[working(A)], secs(t, 10)), []);
}

/// 第一次看到的任务只记基线；重连后记着的都清掉，做完了一半的也不推。会话没了、shell 退出了不推。
#[test]
fn todos_start_from_a_baseline() {
    let t = Instant::now();
    let mut rounds = Rounds::new(Duration::ZERO, true);
    let both = [todo("1", A), todo("2", B)];
    assert_eq!(round(&mut rounds, &both, &[idle(A), working(B)], t), []);
    assert_eq!(round(&mut rounds, &both, &[idle(A), idle(B)], secs(t, 2)), [Action::TodoDone(B, "任务 2".into())]);

    let mut rounds = Rounds::new(DELAY, true);
    let todos = [todo("1", A)];
    round(&mut rounds, &todos, &[working(A)], t);
    round(&mut rounds, &todos, &[idle(A)], secs(t, 2));
    rounds.reconnected();
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 22)), []);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 60)), []);

    round(&mut rounds, &todos, &[working(A)], secs(t, 62));
    assert_eq!(round(&mut rounds, &todos, &[], secs(t, 64)), []);
    assert_eq!(round(&mut rounds, &todos, &[], secs(t, 100)), []);
    round(&mut rounds, &todos, &[working(A)], secs(t, 102));
    let exited = [SessionInfo { exited: true, ..idle(A) }];
    assert_eq!(round(&mut rounds, &todos, &exited, secs(t, 104)), []);
    assert_eq!(round(&mut rounds, &todos, &exited, secs(t, 140)), []);
}

/// 推过做完以后标成完成、删掉了、会话没了：还没发出去的那条撤掉，只撤一次。会话在等回答时不撤，等回答
/// 的那条要留着。
#[test]
fn done_pushes_are_cancelled_once_the_todo_moves_on() {
    let t = Instant::now();
    let todos = [todo("1", A)];
    let pushed = || {
        let mut rounds = Rounds::new(Duration::ZERO, true);
        round(&mut rounds, &todos, &[working(A)], t);
        assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 2)), [done(A)]);
        rounds
    };

    let mut rounds = pushed();
    let marked = [Todo { done_at: Some(UNIX), ..todo("1", A) }];
    assert_eq!(round(&mut rounds, &marked, &[idle(A)], secs(t, 4)), [Action::Cancel(A)]);
    assert_eq!(round(&mut rounds, &marked, &[idle(A)], secs(t, 6)), []);

    let mut rounds = pushed();
    assert_eq!(round(&mut rounds, &[], &[idle(A)], secs(t, 4)), [Action::Cancel(A)]);
    assert_eq!(round(&mut rounds, &[], &[idle(A)], secs(t, 6)), []);

    let mut rounds = pushed();
    assert_eq!(round(&mut rounds, &todos, &[], secs(t, 4)), [Action::Cancel(A)]);
    assert_eq!(round(&mut rounds, &todos, &[], secs(t, 6)), []);

    let mut rounds = pushed();
    assert_eq!(round(&mut rounds, &[], &[blocked(A)], secs(t, 4)), []);
}

/// 上一轮的任务表到了、会话列表还没到就又要了一轮：那份旧任务表不和这一轮的会话列表配对。
#[test]
fn a_new_round_drops_the_unpaired_snapshot() {
    let t = Instant::now();
    let mut rounds = Rounds::new(DELAY, true);
    let todos = [todo("1", A)];
    round(&mut rounds, &todos, &[working(A)], t);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 2)), []);
    assert_eq!(rounds.set_todos(todos.to_vec(), secs(t, 12), UNIX), []);
    rounds.new_round();
    assert_eq!(rounds.todo_sessions(&[idle(A)], secs(t, 14), UNIX), []);
    let finished = Todo { done_at: Some(UNIX), ..todo("1", A) };
    assert_eq!(rounds.set_todos(vec![finished], secs(t, 14), UNIX), []);
}

/// 同一轮的任务表和会话列表都到了才看，谁先到都一样：宽限期最后一刻标成完成、删掉的任务不推，不拿
/// 上一轮的任务表看。
#[test]
fn todos_wait_for_the_same_round() {
    let t = Instant::now();
    let mut rounds = Rounds::new(DELAY, true);
    let todos = [todo("1", A)];
    round(&mut rounds, &todos, &[working(A)], t);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 2)), []);
    // 会话列表先到：任务表没来之前不看。
    assert_eq!(rounds.todo_sessions(&[idle(A)], secs(t, 12), UNIX), []);
    let finished = Todo { done_at: Some(UNIX), ..todo("1", A) };
    assert_eq!(rounds.set_todos(vec![finished], secs(t, 12), UNIX), []);

    let mut rounds = Rounds::new(DELAY, true);
    round(&mut rounds, &todos, &[working(A)], t);
    assert_eq!(round(&mut rounds, &todos, &[idle(A)], secs(t, 2)), []);
    // 任务表先到，已经删掉了。
    assert_eq!(rounds.set_todos(Vec::new(), secs(t, 12), UNIX), []);
    assert_eq!(rounds.todo_sessions(&[idle(A)], secs(t, 12), UNIX), []);
    // 只有会话列表来的一轮（任务表没回）不看，下一轮照常。
    assert_eq!(rounds.todo_sessions(&[idle(A)], secs(t, 14), UNIX), []);
    assert_eq!(rounds.todo_sessions(&[working(A)], secs(t, 16), UNIX), []);
    assert_eq!(rounds.set_todos(todos.to_vec(), secs(t, 16), UNIX), []);
}
