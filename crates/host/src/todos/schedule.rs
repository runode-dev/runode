//! 定时到点开任务的线程（`run`），下一次到点由 `runode_todo::next_fire` 按本地时间算。
//!
//! 错过的（电脑睡着、宿主没在跑）补一次：上一次到点后的下一次已经过了就算到点，开一张任务，把到点的
//! 时刻记成现在，下一次从现在算起，所以睡了三天醒来也只补一次。同一个定时上次开出的任务还在干活时
//! 这次跳过，免得两个 agent 改同一处。

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use runode_protocol::todo::{Column, Schedule, When, column};
use runode_todo::next_fire;

use super::{Todos, unix_now};
use crate::Shared;

/// 定时的线程最多睡这么久就再看一次：电脑睡眠时单调时钟停着，醒来后按墙上时间补上。
const MAX_SLEEP: Duration = Duration::from_secs(30);

/// 定时的线程：清完成了的任务的工作树，到点的定时开任务，再睡到下一次到点、最多 `MAX_SLEEP`，任务板变了
/// 时提前醒。宿主没了就结束。
pub(super) fn run(todos: &Arc<Todos>, shared: &Weak<Shared>) {
    loop {
        let seen = todos.generation();
        let Some(strong) = shared.upgrade() else { return };
        strong.sweep_worktrees();
        strong.end_finished_sessions();
        let next = strong.fire_due();
        drop(strong);
        let sleep = next.map_or(MAX_SLEEP, |at| Duration::from_secs(at.saturating_sub(unix_now())).min(MAX_SLEEP));
        todos.wait_for_change(seen, sleep);
    }
}

impl Shared {
    /// 开到了点的定时，返回最早的下一次到点。不归这个宿主办（正在交出会话、要退出了）或者任务板没开时
    /// 什么都不做。
    fn fire_due(&self) -> Option<u64> {
        if self.todos_writable().is_err() {
            return None;
        }
        let schedules = match self.todos.with_db(|db| db.list_schedules()) {
            Ok(schedules) => schedules,
            Err(err) => {
                tracing::warn!("cannot read the schedules: {err}");
                return None;
            }
        };
        self.set_scheduled(!schedules.is_empty());
        let now = unix_now();
        let mut soonest: Option<u64> = None;
        for schedule in &schedules {
            let Some(at) = next_fire(&schedule.when, schedule.last_fired) else { continue };
            let next = if at <= now {
                self.fire(schedule, now);
                next_fire(&schedule.when, now)
            } else {
                Some(at)
            };
            soonest = soonest.into_iter().chain(next).min();
        }
        soonest
    }

    /// 定时到点：记下到点的时刻（一次性的删掉），开一张新任务并开始它；上次开出的那张没开始成或者还在干活时
    /// 跳过。
    fn fire(&self, schedule: &Schedule, now: u64) {
        let previous = self.todos.with_db(|db| db.latest_open_todo_of(&schedule.id));
        // 上次开出的那张没开始成（比如目录没了）时也跳过，免得每到点就多一张开不起来的待办。
        if let Ok(Some(todo)) = &previous
            && match todo.session {
                None => true,
                Some(session) => column(todo, self.session_info(session).as_ref(), now) == Column::InProgress,
            }
        {
            tracing::info!(
                "schedule {} skips this run: its last todo {} has not started or is still in progress",
                schedule.id,
                todo.id
            );
            if let Err(err) = self.todos.with_db(|db| db.set_fired(&schedule.id, now)) {
                tracing::warn!("cannot record that schedule {} fired: {err}", schedule.id);
            }
            return;
        }
        // 先记到点、再加任务、最后删一次性的定时：加任务失败时定时还在（记过到点，不会马上又触发），不会
        // 连任务带定时一起丢掉。
        let added = self.todos.with_db(|db| {
            db.set_fired(&schedule.id, now)?;
            let id = db.add_todo(&schedule.spec, now, Some(&schedule.id))?;
            if matches!(schedule.when, When::Once { .. })
                && let Err(err) = db.remove_schedule(&schedule.id)
            {
                tracing::warn!("cannot remove the one-time schedule {}: {err}", schedule.id);
            }
            Ok(id)
        });
        // 失败时不通知：通知会叫醒定时的线程马上再来一轮，记不进库时就成了空转。
        if added.is_ok() {
            self.todos_changed();
        }
        match added {
            Ok(id) => {
                tracing::info!("schedule {} fired: todo {id}", schedule.id);
                if let Err(err) = self.start_todo(&id) {
                    tracing::warn!("schedule {} could not start todo {id}: {err}", schedule.id);
                }
            }
            Err(err) => tracing::warn!("schedule {} could not add its todo: {err}", schedule.id),
        }
    }
}
