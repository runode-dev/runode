//! 提交和分支「多久以前」的文字：git 给的是 Unix 秒，这里按界面语言写成相对现在的时间；也写 agent 等回答
//! 等了多久（`waited`），以及把宿主报的 Unix 毫秒时刻折成本机的 `Instant`（`instant_of`）。

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 宿主报的时刻 `since_ms`（Unix 毫秒）按本机的钟折成 `Instant`。
pub(crate) fn instant_of(since_ms: u64) -> Instant {
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis());
    since_instant(since_ms, u64::try_from(now_ms).unwrap_or(u64::MAX), Instant::now())
}

/// `since_ms` 离 `now_ms`（都是 Unix 毫秒）多久，就从 `now` 往前推多久；比现在还晚的（两台机器的钟不齐）
/// 算现在，往前推出了 `Instant` 能表示的范围时也算现在。
fn since_instant(since_ms: u64, now_ms: u64, now: Instant) -> Instant {
    now.checked_sub(Duration::from_millis(now_ms.saturating_sub(since_ms))).unwrap_or(now)
}

/// 等了 `secs` 秒，按界面语言写成整分钟或整小时（向下取整），不到一分钟另有一句（`time.under_minute`）。
pub(crate) fn waited(secs: u64) -> String {
    waited_in(secs, &rust_i18n::locale())
}

fn waited_in(secs: u64, locale: &str) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    match secs {
        s if s < MINUTE => rust_i18n::t!("time.under_minute", locale = locale),
        s if s < HOUR => rust_i18n::t!("time.minutes", locale = locale, n = s / MINUTE),
        s => rust_i18n::t!("time.hours", locale = locale, n = s / HOUR),
    }
    .into_owned()
}

/// `time`（Unix 秒）离现在多久，写成「3 天前」这样；比现在还晚的（两台机器的钟不齐）算刚刚。
pub(crate) fn ago(time: i64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
    ago_at(time, i64::try_from(now).unwrap_or(i64::MAX), &rust_i18n::locale())
}

fn ago_at(time: i64, now: i64, locale: &str) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let secs = now.saturating_sub(time).max(0);
    let (key, n) = match secs {
        s if s < MINUTE => return rust_i18n::t!("git.ago.now", locale = locale).into_owned(),
        s if s < HOUR => ("git.ago.minutes", s / MINUTE),
        s if s < DAY => ("git.ago.hours", s / HOUR),
        s if s < 14 * DAY => ("git.ago.days", s / DAY),
        s if s < 70 * DAY => ("git.ago.weeks", s / (7 * DAY)),
        s if s < 365 * DAY => ("git.ago.months", s / (30 * DAY)),
        s => ("git.ago.years", s / (365 * DAY)),
    };
    rust_i18n::t!(key, locale = locale, n = n).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_largest_unit_that_fits() {
        let now = 1_700_000_000;
        let ago = |secs: i64| ago_at(now - secs, now, "en");
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(-30), "just now");
        assert_eq!(ago(125), "2m ago");
        assert_eq!(ago(3 * 3600), "3h ago");
        assert_eq!(ago(13 * 86_400), "13d ago");
        assert_eq!(ago(15 * 86_400), "2w ago");
        assert_eq!(ago(100 * 86_400), "3mo ago");
        assert_eq!(ago(800 * 86_400), "2y ago");
        assert_eq!(ago_at(now - 3 * 86_400, now, "zh-Hans"), "3 天前");
    }

    #[test]
    fn waited_counts_whole_minutes_then_hours() {
        assert_eq!(waited_in(0, "en"), "under a minute");
        assert_eq!(waited_in(59, "en"), "under a minute");
        assert_eq!(waited_in(60, "en"), "1 min");
        assert_eq!(waited_in(3599, "en"), "59 min");
        assert_eq!(waited_in(3600, "en"), "1 h");
        assert_eq!(waited_in(59, "zh-Hans"), "1 分钟不到");
        assert_eq!(waited_in(7200, "zh-Hant"), "2 小時");
    }

    #[test]
    fn since_instant_counts_back_from_now() {
        let now = Instant::now();
        let now_ms = 1_790_000_000_000;
        assert_eq!(since_instant(now_ms, now_ms, now), now);
        assert_eq!(now - since_instant(now_ms - 90_000, now_ms, now), Duration::from_secs(90));
        // 宿主的钟比本机快：算现在，不算成将来。
        assert_eq!(since_instant(now_ms + 5_000, now_ms, now), now);
    }
}
