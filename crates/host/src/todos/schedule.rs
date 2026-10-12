//! 定时：按本地时间算下一次到点（`next_fire`），和到点开任务的线程（`run`）。
//!
//! 错过的（电脑睡着、宿主没在跑）补一次：上一次到点后的下一次已经过了就算到点，开一张任务，把到点的
//! 时刻记成现在，下一次从现在算起，所以睡了三天醒来也只补一次。同一个定时上次开出的任务还在干活时
//! 这次跳过，免得两个 agent 改同一处。

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use runode_protocol::todo::{Column, Schedule, When, column};

use super::{Todos, store, unix_now};
use crate::Shared;

/// 定时的线程最多睡这么久就再看一次：电脑睡眠时单调时钟停着，醒来后按墙上时间补上。
const MAX_SLEEP: Duration = Duration::from_secs(30);

/// `when` 从 `base`（上一次到点或者建好的时刻，Unix 时间的秒数）算起的下一次到点；不会再到点时为空。
pub(crate) fn next_fire(when: &When, base: u64) -> Option<u64> {
    match *when {
        When::Once { at } => (at > base).then_some(at),
        When::Every { minutes } => (minutes > 0).then(|| base.saturating_add(u64::from(minutes) * 60)),
        When::Daily { hour, minute, days } => daily(base, hour, minute, days),
        When::Unknown => None,
    }
}

/// 每天 `hour:minute`（`days` 不为 0 时只在这几天，第 0 位是星期日）在 `base` 之后的第一次。按本地的墙上
/// 时间算：`base` 那天已经过了这个钟点（含正好到点）就从第二天找起，秋天回拨的那天同一个钟点不会到两次；
/// 春天跳过的钟点由 `mktime` 挪到前后一小时。
fn daily(base: u64, hour: u8, minute: u8, days: u8) -> Option<u64> {
    if hour >= 24 || minute >= 60 {
        return None;
    }
    let base_t = libc::time_t::try_from(base).ok()?;
    // SAFETY: `tm` 是纯数据，全零是合法的值；`localtime_r` 只写它。
    let mut today: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: 两个指针都指向这里的局部变量，调用期间有效。
    if unsafe { libc::localtime_r(&raw const base_t, &raw mut today) }.is_null() {
        return None;
    }
    let passed = (today.tm_hour, today.tm_min) >= (i32::from(hour), i32::from(minute));
    // 第一天加上整整一周，星期几总有一天对得上。
    (i32::from(passed)..i32::from(passed) + 8).find_map(|offset| {
        let mut day = today;
        day.tm_mday += offset;
        day.tm_hour = i32::from(hour);
        day.tm_min = i32::from(minute);
        day.tm_sec = 0;
        day.tm_isdst = -1;
        // SAFETY: 指针指向局部变量；`mktime` 规范化它并填好星期几。
        let at = u64::try_from(unsafe { libc::mktime(&raw mut day) }).ok()?;
        let weekday = u32::try_from(day.tm_wday).ok()?;
        (at > base && (days == 0 || days & (1 << weekday) != 0)).then_some(at)
    })
}

/// 定时的线程：清完成了的任务的工作树，到点的定时开任务，再睡到下一次到点、最多 `MAX_SLEEP`，任务板变了
/// 时提前醒。宿主没了就结束。
pub(super) fn run(todos: &Arc<Todos>, shared: &Weak<Shared>) {
    loop {
        let seen = todos.generation();
        let Some(strong) = shared.upgrade() else { return };
        strong.sweep_worktrees();
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
        let schedules = match self.todos.with_db(|conn| store::list_schedules(conn)) {
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

    /// 定时到点：记下到点的时刻（一次性的删掉），开一张新任务并开始它；上次开出的那张还在干活时跳过。
    fn fire(&self, schedule: &Schedule, now: u64) {
        let previous = self.todos.with_db(|conn| store::latest_open_todo_of(conn, &schedule.id));
        if let Ok(Some(todo)) = &previous
            && let Some(session) = todo.session
            && column(todo, self.session_info(session).as_ref(), now) == Column::InProgress
        {
            tracing::info!("schedule {} skips this run: its last todo {} is still in progress", schedule.id, todo.id);
            if let Err(err) = self.todos.with_db(|conn| store::set_fired(conn, &schedule.id, now)) {
                tracing::warn!("cannot record that schedule {} fired: {err}", schedule.id);
            }
            return;
        }
        let added = self.todos.with_db(|conn| {
            store::set_fired(conn, &schedule.id, now)?;
            if matches!(schedule.when, When::Once { .. }) {
                store::remove_schedule(conn, &schedule.id)?;
            }
            store::add_todo(conn, &schedule.spec, now, Some(&schedule.id))
        });
        self.todos_changed();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `at` 的本地时间：时、分、星期几。
    fn local(at: u64) -> (i32, i32, u32) {
        let at = libc::time_t::try_from(at).unwrap();
        // SAFETY: 同 `daily`。
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: 同 `daily`。
        assert!(!unsafe { libc::localtime_r(&raw const at, &raw mut tm) }.is_null());
        (tm.tm_hour, tm.tm_min, u32::try_from(tm.tm_wday).unwrap())
    }

    // 不改 `TZ`：它是整个进程共用的，测试并行跑时会互相干扰。只断言和时区无关的性质。
    #[test]
    fn daily_lands_on_the_wall_clock_time_after_base() {
        let base = 1_760_000_000; // 2025 年 10 月
        for (hour, minute) in [(0, 0), (9, 30), (23, 59)] {
            let at = next_fire(&When::Daily { hour, minute, days: 0 }, base).unwrap();
            assert!(at > base && at <= base + 24 * 3600 + 3600, "{hour}:{minute} -> {at}");
            let (h, m, _) = local(at);
            assert_eq!((h, m), (i32::from(hour), i32::from(minute)));
            // 正好到点时下一次是第二天，不会马上再到一次。
            let again = next_fire(&When::Daily { hour, minute, days: 0 }, at).unwrap();
            assert!(again >= at + 23 * 3600 && again <= at + 25 * 3600, "{at} -> {again}");
        }
    }

    #[test]
    fn daily_only_on_the_chosen_weekdays() {
        let base = 1_760_000_000;
        for days in [0b000_0001u8, 0b010_0000, 0b011_1110] {
            let at = next_fire(&When::Daily { hour: 9, minute: 0, days }, base).unwrap();
            assert!(at > base && at <= base + 8 * 24 * 3600);
            assert_ne!(days & (1 << local(at).2), 0, "{days:#b} -> weekday {}", local(at).2);
        }
        assert_eq!(next_fire(&When::Daily { hour: 24, minute: 0, days: 0 }, base), None);
    }

    #[test]
    fn once_every_and_unknown() {
        assert_eq!(next_fire(&When::Once { at: 500 }, 100), Some(500));
        assert_eq!(next_fire(&When::Once { at: 500 }, 500), None);
        assert_eq!(next_fire(&When::Every { minutes: 15 }, 100), Some(100 + 15 * 60));
        assert_eq!(next_fire(&When::Every { minutes: 0 }, 100), None);
        assert_eq!(next_fire(&When::Unknown, 100), None);
    }
}
