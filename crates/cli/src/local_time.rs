//! 本机的本地时间：`schedule add --at` 的钟点换成 Unix 时间，`schedule list` 把 Unix 时间写成钟点。
//! 时区按 C 库的规矩（`TZ`、`/etc/localtime`），和宿主算定时的下一次到点用的是同一套。

use std::fmt;

/// 本地的一个钟点，精确到分钟。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalTime {
    pub(crate) year: i32,
    pub(crate) month: u8,
    pub(crate) day: u8,
    pub(crate) hour: u8,
    pub(crate) minute: u8,
}

impl LocalTime {
    /// 读 `YYYY-MM-DD HH:MM`。日子是否真有（2 月 30 日）到 `unix` 时才知道。
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let (date, time) = text.trim().split_once(' ')?;
        let mut date = date.split('-');
        let (year, month, day) = (date.next()?.parse().ok()?, date.next()?.parse().ok()?, date.next()?.parse().ok()?);
        let (hour, minute) = clock(time.trim())?;
        (date.next().is_none() && (1..=12).contains(&month) && (1..=31).contains(&day)).then_some(Self {
            year,
            month,
            day,
            hour,
            minute,
        })
    }

    /// 换成 Unix 时间的秒数；没有这一天时为空。春天跳过的钟点挪到前后一小时，同宿主一样。
    pub(crate) fn unix(self) -> Option<u64> {
        // SAFETY: `tm` 是纯数据，全零是合法的值。
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = self.year - 1900;
        tm.tm_mon = i32::from(self.month) - 1;
        tm.tm_mday = i32::from(self.day);
        tm.tm_hour = i32::from(self.hour);
        tm.tm_min = i32::from(self.minute);
        tm.tm_isdst = -1;
        let wanted = (tm.tm_year, tm.tm_mon, tm.tm_mday);
        // SAFETY: 指针指向局部变量；`mktime` 只规范化它。
        let at = unsafe { libc::mktime(&raw mut tm) };
        // `mktime` 把 2 月 30 日规范成 3 月 2 日，日期变了就是没有这一天。
        ((tm.tm_year, tm.tm_mon, tm.tm_mday) == wanted).then(|| u64::try_from(at).ok()).flatten()
    }

    /// Unix 时间的秒数在本地是几点。
    pub(crate) fn from_unix(at: u64) -> Option<Self> {
        let at = libc::time_t::try_from(at).ok()?;
        // SAFETY: `tm` 是纯数据，全零是合法的值。
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: 两个指针都指向局部变量，调用期间有效；`localtime_r` 只写 `tm`。
        if unsafe { libc::localtime_r(&raw const at, &raw mut tm) }.is_null() {
            return None;
        }
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

/// 读 `HH:MM`（24 小时制）。
pub(crate) fn clock(text: &str) -> Option<(u8, u8)> {
    let (hour, minute) = text.split_once(':')?;
    let (hour, minute) = (hour.parse().ok()?, minute.parse().ok()?);
    (hour < 24 && minute < 60 && text.len() <= 5).then_some((hour, minute))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_times_round_trip_and_bad_dates_are_refused() {
        let at = LocalTime::parse("2026-10-13 14:05").unwrap();
        assert_eq!(at, LocalTime { year: 2026, month: 10, day: 13, hour: 14, minute: 5 });
        assert_eq!(LocalTime::from_unix(at.unix().unwrap()), Some(at));
        assert_eq!(at.to_string(), "2026-10-13 14:05");
        assert_eq!(LocalTime::parse("2026-02-30 09:00").unwrap().unix(), None);
        for bad in ["2026-10-13", "2026-13-01 09:00", "2026-10-13 24:00", "2026-10-13T09:00", "13:00"] {
            assert_eq!(LocalTime::parse(bad), None, "{bad}");
        }
        // 宿主只查一次性的定时在将来，任意大的时刻都可能来：年份装不下时为空，不溢出。
        assert_eq!(LocalTime::from_unix(2_147_482_677 * 31_556_952), None);
        assert_eq!(clock("9:30"), Some((9, 30)));
        assert_eq!(clock("09:60"), None);
    }
}
