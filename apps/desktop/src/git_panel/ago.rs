//! 提交和分支「多久以前」的文字：git 给的是 Unix 秒，这里按界面语言写成相对现在的时间。

use std::time::{SystemTime, UNIX_EPOCH};

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
}
