//! 定时到点开任务的线程（`run`），下一次到点由 `runode_todo::next_fire` 按本地时间算。
//!
//! 错过的（电脑睡着、宿主没在跑）补一次：上一次到点后的下一次已经过了就算到点，开一张任务，把到点的
//! 时刻记成现在，下一次从现在算起，所以睡了三天醒来也只补一次。同一个定时上次开出的任务还在干活、或者
//! agent 停在权限询问上时这次跳过，免得两个 agent 改同一处。带 `Schedule::run` 的定时到点先另起线程跑命令
//! （`Shared::start_run`），按它的结果开任务；上一次的命令还没跑完时这次也跳过。定时开出的任务和用户排进队的
//! 一样，由 `start_queued` 按看板的并发上限开始。手动立即触发（`Shared::fire_now`）走同样的让路判断，要让路时
//! 回绝，但不记到点的时刻。

use std::{
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

use runode_protocol::todo::{Column, Outcome, Schedule, Todo, column, holds_schedule};
use runode_todo::{FiredNow, next_fire};

use super::{Todos, unix_now};
use crate::Shared;

/// 定时的线程最多睡这么久就再看一次：电脑睡眠时单调时钟停着，醒来后按墙上时间补上。
pub(super) const MAX_SLEEP: Duration = Duration::from_secs(30);

/// 排着队的任务没开始成时，第几次失败以后隔多久再试；都试完还开始不了就记成没做成，不再挡着定时。
const START_RETRY_AFTER: [Duration; 2] = [Duration::from_secs(60), Duration::from_secs(5 * 60)];

/// 定时的线程：标完成做完了的自动完成任务，停下卡住的任务，清完成了的任务的工作树，到点的定时开任务，开始排着队的
/// 任务，再睡到下一次该看的时候、最多 `MAX_SLEEP`，看板变了时提前醒。宿主没了就结束。
pub(super) fn run(todos: &Arc<Todos>, shared: &Weak<Shared>) {
    loop {
        let seen = todos.generation();
        let Some(strong) = shared.upgrade() else { return };
        let settle = strong.finish_idle_todos();
        strong.fail_stuck_todos();
        strong.sweep_worktrees();
        strong.end_finished_sessions();
        let (next, scheduled) = strong.fire_due();
        // 定时开出的任务排着队，这里接着开始。
        let (queued, retry) = strong.start_queued();
        if let (Some(scheduled), Some(queued)) = (scheduled, queued) {
            strong.set_scheduled(scheduled || queued);
        }
        drop(strong);
        let mut sleep = next.map_or(MAX_SLEEP, |at| Duration::from_secs(at.saturating_sub(unix_now())).min(MAX_SLEEP));
        for at in settle.into_iter().chain(retry) {
            sleep = sleep.min(at.saturating_duration_since(Instant::now()));
        }
        todos.wait_for_change(seen, sleep);
    }
}

impl Shared {
    /// 开到了点的定时，返回最早的下一次到点和有没有在用的定时。不归这个宿主办（正在交出会话、要退出了）或者
    /// 看板没开时什么都不做，两样都为空。
    fn fire_due(&self) -> (Option<u64>, Option<bool>) {
        if self.todos_writable().is_err() {
            return (None, None);
        }
        let schedules = match self.todos.with_db(|db| db.list_schedules()) {
            Ok(schedules) => schedules,
            Err(err) => {
                tracing::warn!("cannot read the schedules: {err}");
                return (None, None);
            }
        };
        let now = unix_now();
        let mut soonest: Option<u64> = None;
        for schedule in schedules.iter().filter(|schedule| !schedule.paused) {
            let Some(at) = next_fire(&schedule.when, schedule.last_fired) else { continue };
            let next = if at <= now {
                self.fire(schedule, now);
                next_fire(&schedule.when, now)
            } else {
                Some(at)
            };
            soonest = soonest.into_iter().chain(next).min();
        }
        // 命令还在跑的也算：一次性的定时到点时已经删掉，命令跑完才开任务。
        (soonest, Some(schedules.iter().any(|schedule| !schedule.paused) || self.todos.runs_pending()))
    }

    /// 开始排着队的任务：按先后，等的任务做完了（删掉了也算）、看板上进行中的少于并发上限时开始；等的任务没做成
    /// 时记成没做成。开始不了的留在队里，隔 `START_RETRY_AFTER` 再试，试满还不成就记成没做成，结果是最后一次的
    /// 错误。返回还有没有排着队的（不归这个宿主办、读不了库时为空）和最早该再试的时刻。
    pub(super) fn start_queued(&self) -> (Option<bool>, Option<Instant>) {
        if self.todos_writable().is_err() {
            return (None, None);
        }
        let now = unix_now();
        let (todos, limit) = match self.todos.with_db(|db| Ok((db.list_todos(now)?, db.max_running()?))) {
            Ok(read) => read,
            Err(err) => {
                tracing::warn!("cannot read the todos: {err}");
                return (None, None);
            }
        };
        let waiting: Vec<&Todo> =
            todos.iter().filter(|todo| todo.queued && todo.started.is_none() && todo.done_at.is_none()).collect();
        // 不再排着队的不再替它重试。
        self.todos.watched().retries.retain(|id, _| waiting.iter().any(|todo| &todo.id == id));
        if waiting.is_empty() {
            return (Some(false), None);
        }
        let mut running = todos
            .iter()
            .filter(|todo| {
                todo.session
                    .is_some_and(|session| column(todo, self.session_info(session).as_ref(), now) == Column::InProgress)
            })
            .count();
        for todo in waiting {
            if let Some(after) = &todo.after {
                // 早就完成了的不在 `list_todos` 里，另读。
                let found = match todos.iter().find(|other| &other.id == after) {
                    Some(other) => Ok(Some(other.clone())),
                    None => self.todos.with_db(|db| db.todo_by_id(after)),
                };
                match found {
                    Ok(Some(other)) if other.outcome == Some(Outcome::Failed) => {
                        self.fail_queued(todo, &format!("Runode did not start it: #{} failed.", other.number));
                        continue;
                    }
                    Ok(Some(other)) if other.done_at.is_none() => continue,
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!("cannot read todo {after}: {err}");
                        continue;
                    }
                }
            }
            let tries = match self.todos.watched().retries.get(&todo.id) {
                Some(&(_, at)) if at > Instant::now() => continue,
                Some(&(tries, _)) => tries,
                None => 0,
            };
            if limit > 0 && running >= limit as usize {
                // 按先后排，前面的没轮上，后面的也等着；等别的任务的可以先跳过，上面已经跳过了。
                break;
            }
            // 用户正在改、正在开始的，和宿主正在交出会话时开始不了的，都不算一次失败，下一轮再看。
            let Some(claim) = self.todos.claim(&todo.id) else { continue };
            // 读出列表以后用户可能刚取消排队、改了要等的任务：占住以后照库里现在的样子再看一眼。
            let still = self.todos.with_db(|db| db.todo_by_id(&todo.id)).ok().flatten().is_some_and(|now| {
                now.queued && now.started.is_none() && now.done_at.is_none() && now.after == todo.after
            });
            if !still {
                continue;
            }
            let started = self.start_claimed(&todo.id, &claim);
            drop(claim);
            let Err(err) = started else {
                running += 1;
                self.todos.watched().retries.remove(&todo.id);
                continue;
            };
            if self.todos_writable().is_err() {
                break;
            }
            tracing::warn!("queued todo {} could not start: {err}", todo.id);
            match START_RETRY_AFTER.get(tries as usize) {
                Some(&after) => {
                    self.todos.watched().retries.insert(todo.id.clone(), (tries + 1, Instant::now() + after));
                }
                None => self.fail_queued(todo, &format!("Runode could not start it: {err}")),
            }
        }
        // 到了时候却因为上限没轮上的不算，不然这一轮会马上再来；上限空出来最晚睡满 `MAX_SLEEP` 也看得到。
        let now = Instant::now();
        (Some(true), self.todos.watched().retries.values().map(|&(_, at)| at).filter(|&at| at > now).min())
    }

    /// 排着队的任务开始不了：不再排队，记成没做成，结果是 `why`。
    fn fail_queued(&self, todo: &Todo, why: &str) {
        let Some(claim) = self.todos.claim(&todo.id) else { return };
        let done = self.todos.with_db(|db| {
            db.set_queued(&todo.id, false, None)?;
            db.set_outcome(&todo.id, Some(Outcome::Failed), Some(why))
        });
        drop(claim);
        match done {
            Ok(_) => self.todos_changed(),
            Err(err) => tracing::warn!("cannot record that todo {} cannot start: {err}", todo.id),
        }
    }

    /// 定时 `id` 这次要不要让路：上次的命令还在跑，或者上次开出的那张还在排队、没开始成、还在干活、停在询问上时
    /// 为让路的原因，不用让路时为空。读不了库时出错：库只是一时打不开的话，开出来可能和还在干活的上一个 agent
    /// 撞在一起，调用方不开。
    fn held_back(&self, id: &str, now: u64) -> Result<Option<HeldBack>, String> {
        if self.todos.run_pending(id) {
            return Ok(Some(HeldBack::Running));
        }
        let Some(todo) = self.todos.with_db(|db| db.latest_open_todo_of(id))? else { return Ok(None) };
        let holds = match todo.session.map(|session| (session, self.session_info(session))) {
            // 会话还在却没及时回话：看不出它在不在干活，当它在干，免得两个 agent 一起跑。
            Some((session, None)) => self.registry().sessions.contains_key(&session),
            found => holds_schedule(&todo, found.and_then(|(_, info)| info).as_ref(), now),
        };
        Ok(holds.then_some(HeldBack::Todo(todo.id)))
    }

    /// 定时到点：记下到点的时刻（一次性的删掉），加一张排着队的新任务，带 `Schedule::run` 的先跑命令、按结果开；
    /// 要让路（`held_back`）时跳过，读不了库时下一轮再看。
    fn fire(&self, schedule: &Schedule, now: u64) {
        let _firing = self.todos.firing();
        match self.held_back(&schedule.id, now) {
            Ok(None) => {}
            Ok(Some(held)) => {
                let blocker = match held {
                    HeldBack::Running => {
                        tracing::info!("schedule {} skips this run: its last command is still running", schedule.id);
                        None
                    }
                    HeldBack::Todo(todo) => {
                        tracing::info!(
                            "schedule {} skips this run: its last todo {todo} has not started, is still in progress \
                             or is blocked",
                            schedule.id
                        );
                        Some(todo)
                    }
                };
                self.skip(schedule, now, blocker.as_deref());
                return;
            }
            Err(err) => {
                tracing::warn!("schedule {} cannot read its last todo: {err}", schedule.id);
                return;
            }
        }
        if schedule.run.is_some() {
            self.fire_run(schedule, now);
            return;
        }
        // `schedule` 是这一轮开头读的，之后可能被删、被改过：`fire_schedule` 照库里现在的样子办。
        match self.todos.with_db(|db| db.fire_schedule(&schedule.id, now)) {
            Ok(Some(id)) => {
                self.todos_changed();
                tracing::info!("schedule {} fired: todo {id}", schedule.id);
            }
            Ok(None) => tracing::info!("schedule {} was removed or changed before it fired", schedule.id),
            // 不通知：通知会叫醒定时的线程马上再来一轮，记不进库时就成了空转；下一轮（最多 `MAX_SLEEP`）再试。
            Err(err) => tracing::warn!("schedule {} could not add its todo: {err}", schedule.id),
        }
    }

    /// 定时这次到点跳过、不开任务：记下到点的时刻。一次性的到过就没了，跳过也删掉：只记到点的话它再也不会
    /// 到点，却一直留着让宿主不空闲。`blocker` 是挡住它的那张任务，上次的命令还在跑时为空。
    fn skip(&self, schedule: &Schedule, now: u64, blocker: Option<&str>) {
        // `schedule` 是这一轮开头读的，决定跳过的这一会儿可能被改过：`skip_schedule` 照库里现在的样子办。
        match self.todos.with_db(|db| db.skip_schedule(&schedule.id, now, blocker)) {
            // 下一次到点变了，看板要重读。
            Ok(true) => self.todos_changed(),
            Ok(false) => tracing::info!("schedule {} was removed or changed before it was skipped", schedule.id),
            Err(err) => tracing::warn!("cannot record that schedule {} fired: {err}", schedule.id),
        }
    }

    /// 带 `Schedule::run` 的定时到点：记下到点的时刻（一次性的删掉），另起线程跑命令（`start_run`）。
    fn fire_run(&self, schedule: &Schedule, now: u64) {
        // 照库里现在的样子办：这一轮开头读到以后可能被删、被停、去掉了命令。
        match self.todos.with_db(|db| db.fire_schedule_run(&schedule.id, now)) {
            Ok(Some(current)) => {
                tracing::info!("schedule {} fired: running its command", schedule.id);
                self.start_run(current, false);
                // 下一次到点变了、命令开跑了，看板要重读。
                self.todos_changed();
            }
            Ok(None) => tracing::info!("schedule {} was removed or changed before it fired", schedule.id),
            // 不通知：记不进库时叫醒定时的线程只会空转，下一轮再试。
            Err(err) => tracing::warn!("schedule {} could not record that it fired: {err}", schedule.id),
        }
    }

    /// 手动立即触发定时 `id` 一次（`Store::fire_schedule_now`）：没有 `run` 的加一张排着队的任务，由定时的线程
    /// 照常开始；有的另起线程跑命令。要让路（`held_back`）、读不了库、定时不存在时出错。
    pub(super) fn fire_now(&self, id: &str) -> Result<(), String> {
        let _firing = self.todos.firing();
        let now = unix_now();
        match self.held_back(id, now)? {
            None => {}
            Some(HeldBack::Running) => return Err(format!("schedule {id} is still running its last command")),
            Some(HeldBack::Todo(_)) => return Err(format!("schedule {id} still has an unfinished todo")),
        }
        match self.todos.with_db(|db| db.fire_schedule_now(id, now))? {
            None => Err(format!("no schedule {id}")),
            Some(FiredNow::Todo(todo)) => {
                tracing::info!("schedule {id} fired by hand: todo {todo}");
                self.todos_changed();
                Ok(())
            }
            Some(FiredNow::Run(current)) => {
                if !self.start_run(*current, true) {
                    return Err(format!("schedule {id} is still running its last command"));
                }
                tracing::info!("schedule {id} fired by hand: running its command");
                self.todos_changed();
                Ok(())
            }
        }
    }
}

/// 定时这次要让路的原因。
enum HeldBack {
    /// 上次的命令还在跑。
    Running,
    /// 上次开出的这张任务还挡着。
    Todo(String),
}
