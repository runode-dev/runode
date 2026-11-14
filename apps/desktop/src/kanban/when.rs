//! 看板上时间的写法：现在的 Unix 时间、卡片和定时上的短时间、定时规则的说法。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use runode_protocol::todo::{Todo, When};
use runode_todo::LocalTime;

use super::dialog;

/// 现在的 Unix 时间，秒。
pub(super) fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

/// 定时的规则给人看的写法：「每天 09:30 · 周一、周三」「每 15 分钟」「一次 · 10-13 14:00」。
pub(super) fn describe_when(when: &When) -> String {
    match *when {
        When::Once { at } => rust_i18n::t!("kanban.when.once", time = dialog::full_time(at)).into_owned(),
        When::Daily { hour, minute, days } => {
            let time = format!("{hour:02}:{minute:02}");
            if days == 0 {
                rust_i18n::t!("kanban.when.daily", time = time).into_owned()
            } else {
                let names: Vec<String> =
                    dialog::week_order().filter(|day| days & (1 << day) != 0).map(dialog::weekday).collect();
                rust_i18n::t!("kanban.when.weekly", time = time, days = names.join(" ")).into_owned()
            }
        }
        When::Every { minutes } if minutes % 60 == 0 => {
            rust_i18n::t!("kanban.when.every_hours", count = minutes / 60).into_owned()
        }
        When::Every { minutes } => rust_i18n::t!("kanban.when.every_minutes", count = minutes).into_owned(),
        When::Cron { ref expr } => rust_i18n::t!("kanban.when.cron", expr = expr).into_owned(),
        When::Unknown => rust_i18n::t!("kanban.when.unknown").into_owned(),
    }
}

/// 定时页里一次运行的标签：所在的列和开始的时间；还没开始（`when` 为空）时只有列名。
pub(super) fn run_label(column: String, when: &str) -> String {
    if when.is_empty() { column } else { format!("{column} · {when}") }
}

/// `now` 那一天本地零点的 Unix 时间；年份装不下时为空。
fn local_midnight(now: u64) -> Option<u64> {
    LocalTime { hour: 0, minute: 0, ..LocalTime::from_unix(now)? }.unix()
}

/// `todos` 里本地今天（`now` 那一天的零点起）完成的个数。
pub(super) fn done_today(todos: &[Todo], now: u64) -> usize {
    let Some(midnight) = local_midnight(now) else { return 0 };
    todos.iter().filter(|todo| todo.done_at.is_some_and(|at| at >= midnight)).count()
}

/// 时限 `minutes` 分钟的写法：整小时的写小时，别的照写分钟，不取整。
pub(super) fn limit_text(minutes: u32) -> String {
    limit_text_in(minutes, &rust_i18n::locale())
}

pub(super) fn limit_text_in(minutes: u32, locale: &str) -> String {
    if minutes > 0 && minutes.is_multiple_of(60) {
        rust_i18n::t!("time.hours", locale = locale, n = minutes / 60).into_owned()
    } else {
        rust_i18n::t!("time.minutes", locale = locale, n = minutes).into_owned()
    }
}

/// 跑了 `elapsed` 秒时用掉了时限 `limit` 分钟的几成，0 到 1；时限为 0 时算用完。
pub(super) fn limit_used(elapsed: u64, limit: u32) -> f32 {
    if limit == 0 {
        return 1.;
    }
    (elapsed as f64 / (f64::from(limit) * 60.)).min(1.) as f32
}

/// 从某一刻起过了 `elapsed`，离下一个满整分钟还有多久；正好满整分钟时是整整一分钟。
pub(super) fn until_next_minute(elapsed: Duration) -> Duration {
    const MINUTE: Duration = Duration::from_secs(60);
    let into = Duration::from_nanos((elapsed.as_nanos() % MINUTE.as_nanos()) as u64);
    MINUTE - into
}

/// 今天的写 `HH:MM`，别的日子写 `MM-DD HH:MM`，按本地时间。
pub(super) fn short_time(at: u64, now: u64) -> String {
    let (Some(at), Some(today)) = (LocalTime::from_unix(at), LocalTime::from_unix(now)) else { return String::new() };
    if (at.year, at.month, at.day) == (today.year, today.month, today.day) {
        format!("{:02}:{:02}", at.hour, at.minute)
    } else {
        format!("{:02}-{:02} {:02}:{:02}", at.month, at.day, at.hour, at.minute)
    }
}

/// 定时往后到点的时刻 `at` 的写法：今天、明天的写成「今天 09:00」「明天 09:00」，再往后写 `MM-DD HH:MM`，
/// 按本地时间。
pub(super) fn fire_text(at: u64, now: u64) -> String {
    fire_text_in(at, now, &rust_i18n::locale())
}

fn fire_text_in(at: u64, now: u64, locale: &str) -> String {
    let (Some(local), Some(day), Some(today)) = (LocalTime::from_unix(at), local_midnight(at), local_midnight(now))
    else {
        return String::new();
    };
    let time = format!("{:02}:{:02}", local.hour, local.minute);
    // 夏令时那天只有 23 或 25 个钟头，零点之间差几天按四舍五入算。
    match day.checked_sub(today).map(|gap| (gap + 3600) / 86_400) {
        Some(0) => rust_i18n::t!("kanban.dialog.fire_today", locale = locale, time = time).into_owned(),
        Some(1) => rust_i18n::t!("kanban.dialog.fire_tomorrow", locale = locale, time = time).into_owned(),
        _ => format!("{:02}-{:02} {time}", local.month, local.day),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kanban::tests::todo;

    /// 还没开始的运行没有时间，标签里不留多余的分隔点。
    #[test]
    fn run_labels_skip_a_missing_time() {
        assert_eq!(run_label("Todo".into(), ""), "Todo");
        assert_eq!(run_label("Done".into(), "09:30"), "Done · 09:30");
    }

    /// 整小时的时限写小时，别的照写分钟，九十分钟不写成一小时。
    #[test]
    fn limits_keep_their_minutes() {
        assert_eq!(limit_text_in(30, "en"), "30 min");
        assert_eq!(limit_text_in(60, "en"), "1 h");
        assert_eq!(limit_text_in(90, "en"), "90 min");
        assert_eq!(limit_text_in(120, "zh-Hans"), "2 小时");
    }

    /// 用掉的比例按秒算，过了时限停在 1，时限为 0 时算用完。
    #[test]
    fn limit_used_is_capped() {
        assert_eq!(limit_used(0, 30), 0.);
        assert_eq!(limit_used(12 * 60, 30), 0.4);
        assert_eq!(limit_used(45 * 60, 30), 1.);
        assert_eq!(limit_used(10, 0), 1.);
    }

    #[test]
    fn next_minute_is_never_now() {
        assert_eq!(until_next_minute(Duration::ZERO), Duration::from_secs(60));
        assert_eq!(until_next_minute(Duration::from_secs(59)), Duration::from_secs(1));
        assert_eq!(until_next_minute(Duration::from_millis(125_500)), Duration::from_millis(54_500));
    }

    // 不改 `TZ`：它是整个进程共用的，测试并行跑时会互相干扰。时刻都从本地时间换算，断言在哪个时区跑都成立。
    fn local(text: &str) -> u64 {
        LocalTime::parse(text).and_then(LocalTime::unix).unwrap()
    }

    /// 今天完成的从本地零点数起：零点前一分钟完成的算昨天，过了午夜再看，昨晚完成的不算今天。
    #[test]
    fn done_today_counts_from_local_midnight() {
        let midnight = local("2026-06-10 00:00");
        let todos = [
            todo("yesterday", Some(0), Some(midnight - 60)),
            todo("midnight", Some(0), Some(midnight)),
            todo("morning", Some(0), Some(local("2026-06-10 09:15"))),
            todo("open", Some(0), None),
        ];
        assert_eq!(done_today(&todos, local("2026-06-10 10:00")), 2);
        assert_eq!(done_today(&todos, local("2026-06-10 23:59")), 2);
        // 午夜前半分钟看，前一分钟完成的那张还算今天。
        assert_eq!(done_today(&todos[..1], midnight - 30), 1);
        assert_eq!(done_today(&todos, local("2026-06-11 00:01")), 0);
    }

    /// 到点的时刻按本地的日子分今天、明天和再往后，跨过午夜就算明天。
    #[test]
    fn fire_times_name_today_and_tomorrow() {
        let now = local("2026-06-10 23:30");
        assert_eq!(fire_text_in(local("2026-06-10 23:45"), now, "en"), "today at 23:45");
        assert_eq!(fire_text_in(local("2026-06-11 00:15"), now, "en"), "tomorrow at 00:15");
        assert_eq!(fire_text_in(local("2026-06-11 09:00"), now, "zh-Hans"), "明天 09:00");
        assert_eq!(fire_text_in(local("2026-06-12 09:00"), now, "en"), "06-12 09:00");
    }

    /// 零点按本地时间算：换算回去正好是同一天的 00:00，和 UTC 差几个钟头都一样。
    #[test]
    fn local_midnight_is_the_start_of_the_local_day() {
        for text in ["2026-01-15 00:00", "2026-01-15 00:30", "2026-01-15 12:00", "2026-01-15 23:59"] {
            let now = local(text);
            let midnight = local_midnight(now).unwrap();
            assert!(midnight <= now && now - midnight < 24 * 3600, "{text}");
            let at = LocalTime::from_unix(midnight).unwrap();
            assert_eq!(at.to_string(), "2026-01-15 00:00", "{text}");
        }
    }
}
