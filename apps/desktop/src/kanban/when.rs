//! 看板上时间的写法：现在的 Unix 时间、卡片和定时上的短时间、定时规则的说法。

use std::time::{SystemTime, UNIX_EPOCH};

use runode_protocol::todo::When;
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
                let names: Vec<String> = (0..7).filter(|day| days & (1 << day) != 0).map(dialog::weekday).collect();
                rust_i18n::t!("kanban.when.weekly", time = time, days = names.join(" ")).into_owned()
            }
        }
        When::Every { minutes } if minutes % 60 == 0 => {
            rust_i18n::t!("kanban.when.every_hours", count = minutes / 60).into_owned()
        }
        When::Every { minutes } => rust_i18n::t!("kanban.when.every_minutes", count = minutes).into_owned(),
        When::Unknown => rust_i18n::t!("kanban.when.unknown").into_owned(),
    }
}

/// 定时页里一次运行的标签：所在的列和开始的时间；还没开始（`when` 为空）时只有列名。
pub(super) fn run_label(column: String, when: &str) -> String {
    if when.is_empty() { column } else { format!("{column} · {when}") }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 还没开始的运行没有时间，标签里不留多余的分隔点。
    #[test]
    fn run_labels_skip_a_missing_time() {
        assert_eq!(run_label("Todo".into(), ""), "Todo");
        assert_eq!(run_label("Done".into(), "09:30"), "Done · 09:30");
    }
}
