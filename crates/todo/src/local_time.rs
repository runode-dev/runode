//! 本机的本地时间和 Unix 时间互换。时区按 C 库的规矩（Unix 上是 `TZ`、`/etc/localtime`，Windows 上是系统的
//! 时区设置）。

use std::fmt;

use crate::schedule::{local_tm, mktime};

/// 本地的一个钟点，精确到分钟。写成文字是 `YYYY-MM-DD HH:MM`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

impl LocalTime {
    /// 读 `YYYY-MM-DD HH:MM`，日期和钟点之间是空白，钟点照 `parse_clock`。日子是否真有（2 月 30 日）到
    /// `unix` 时才知道。
    pub fn parse(text: &str) -> Option<Self> {
        let (date, time) = text.trim().split_once(char::is_whitespace)?;
        let mut date = date.split('-');
        let (year, month, day) = (date.next()?.parse().ok()?, date.next()?.parse().ok()?, date.next()?.parse().ok()?);
        let (hour, minute) = parse_clock(time)?;
        (date.next().is_none() && (1..=12).contains(&month) && (1..=31).contains(&day)).then_some(Self {
            year,
            month,
            day,
            hour,
            minute,
        })
    }

    /// 换成 Unix 时间的秒数；没有这一天时为空。春天跳过的钟点由 `mktime` 挪到前后一小时，照收。
    pub fn unix(self) -> Option<u64> {
        // SAFETY: `tm` 是纯数据，全零是合法的值。
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = self.year - 1900;
        tm.tm_mon = i32::from(self.month) - 1;
        tm.tm_mday = i32::from(self.day);
        tm.tm_hour = i32::from(self.hour);
        tm.tm_min = i32::from(self.minute);
        tm.tm_isdst = -1;
        let wanted = (tm.tm_year, tm.tm_mon, tm.tm_mday);
        let at = mktime(&mut tm);
        // `mktime` 把 2 月 30 日规范成 3 月 2 日，日期变了就是没有这一天。
        ((tm.tm_year, tm.tm_mon, tm.tm_mday) == wanted).then(|| u64::try_from(at).ok()).flatten()
    }

    /// Unix 时间的秒数在本地是几点；年份装不下时为空。
    pub fn from_unix(at: u64) -> Option<Self> {
        let tm = local_tm(libc::time_t::try_from(at).ok()?)?;
        Some(Self {
            year: tm.tm_year.checked_add(1900)?,
            month: u8::try_from(tm.tm_mon + 1).ok()?,
            day: u8::try_from(tm.tm_mday).ok()?,
            hour: u8::try_from(tm.tm_hour).ok()?,
            minute: u8::try_from(tm.tm_min).ok()?,
        })
    }
}

impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { year, month, day, hour, minute } = self;
        write!(f, "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
    }
}

/// 读 24 小时制的 `HH:MM`，时和分都是一到两位数，`9:05`、` 09:05 ` 都认。
pub fn parse_clock(text: &str) -> Option<(u8, u8)> {
    let (hour, minute) = text.trim().split_once(':')?;
    let digits = |part: &str| (1..=2).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit());
    let (hour, minute) = (hour.trim(), minute.trim());
    if !digits(hour) || !digits(minute) {
        return None;
    }
    let (hour, minute): (u8, u8) = (hour.parse().ok()?, minute.parse().ok()?);
    (hour < 24 && minute < 60).then_some((hour, minute))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 不改 `TZ`：本地时间来回一趟原样回来，和时区无关。
    #[test]
    fn local_times_round_trip_and_bad_dates_are_refused() {
        let at = LocalTime::parse("2026-10-13 14:05").unwrap();
        assert_eq!(at, LocalTime { year: 2026, month: 10, day: 13, hour: 14, minute: 5 });
        assert_eq!(LocalTime::from_unix(at.unix().unwrap()), Some(at));
        assert_eq!(at.to_string(), "2026-10-13 14:05");
        assert_eq!(LocalTime::parse("  2026-10-13   9:05 "), Some(LocalTime { hour: 9, ..at }));
        assert_eq!(LocalTime::parse("2026-02-30 09:00").unwrap().unix(), None);
        for bad in ["2026-10-13", "2026-13-01 09:00", "2026-10-13 24:00", "2026-10-13T09:00", "13:00"] {
            assert_eq!(LocalTime::parse(bad), None, "{bad}");
        }
        // 宿主只查一次性的定时在将来，任意大的时刻都可能来：年份装不下时为空，不溢出。
        assert_eq!(LocalTime::from_unix(2_147_482_677 * 31_556_952), None);
    }

    #[test]
    fn clocks_take_one_or_two_digits() {
        assert_eq!(parse_clock("9:30"), Some((9, 30)));
        assert_eq!(parse_clock(" 23:59 "), Some((23, 59)));
        assert_eq!(parse_clock("09:60"), None);
        assert_eq!(parse_clock("24:00"), None);
        assert_eq!(parse_clock("9:5x"), None);
        assert_eq!(parse_clock("905"), None);
        assert_eq!(parse_clock("09:005"), None);
        assert_eq!(parse_clock("+9:05"), None);
    }
}
