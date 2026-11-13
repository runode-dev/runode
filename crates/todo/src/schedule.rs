//! 定时按本地时间算下一次到点（`next_fire`）。

use runode_types::todo::When;

/// `when` 从 `base`（上一次到点或者建好的时刻，Unix 时间的秒数）算起的下一次到点；不会再到点时为空。
pub fn next_fire(when: &When, base: u64) -> Option<u64> {
    match *when {
        When::Once { at } => (at > base).then_some(at),
        When::Every { minutes } => (minutes > 0).then(|| base.saturating_add(u64::from(minutes) * 60)),
        When::Daily { hour, minute, days } => daily(base, hour, minute, days),
        When::Cron { ref expr } => cron(base, &Cron::parse(expr)?),
        When::Unknown => None,
    }
}

/// `expr` 是 `When::Cron` 认得的写法。
pub fn valid_cron(expr: &str) -> bool {
    Cron::parse(expr).is_some()
}

/// 解开的 `When::Cron`：各段允许的值，第 n 位是 n。
struct Cron {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    /// 第 0 位是星期日。
    weekdays: u8,
    /// 日、星期两段以 `*` 开头（`*`、`*/2`）。
    any_day: bool,
    any_weekday: bool,
}

impl Cron {
    fn parse(expr: &str) -> Option<Self> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields.as_slice() else { return None };
        let weekdays = cron_field(weekday, 0, 7)?;
        Some(Self {
            minutes: cron_field(minute, 0, 59)?,
            hours: u32::try_from(cron_field(hour, 0, 23)?).ok()?,
            days: u32::try_from(cron_field(day, 1, 31)?).ok()?,
            months: u16::try_from(cron_field(month, 1, 12)?).ok()?,
            // 7 也是星期日。
            weekdays: u8::try_from((weekdays | weekdays >> 7) & 0x7f).ok()?,
            any_day: day.starts_with('*'),
            any_weekday: weekday.starts_with('*'),
        })
    }

    /// 本地日期（`day` 从 1 起，`month` 从 1 起，`weekday` 0 是星期日）对不对得上：日和星期都限定了时对上一个就算。
    fn day_matches(&self, day: i32, month: i32, weekday: i32) -> bool {
        let bit = |bits: u64, n: i32| u32::try_from(n).is_ok_and(|n| n < 64 && bits & (1 << n) != 0);
        let by_day = bit(self.days.into(), day);
        let by_weekday = bit(self.weekdays.into(), weekday);
        // 写成 `*` 的那段所有位都是 1，取「且」等于只看另一段。
        let day_ok = if self.any_day || self.any_weekday { by_day && by_weekday } else { by_day || by_weekday };
        day_ok && bit(self.months.into(), month)
    }
}

/// crontab 的一段：逗号连起来的 `*`、`n`、`a-b`，各自可以带 `/步长`（`n/步长` 是从 n 到 `max`）。超出
/// `min..=max` 的、写错的为空。
fn cron_field(text: &str, min: u32, max: u32) -> Option<u64> {
    let mut bits = 0u64;
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => (range, step.parse::<u32>().ok().filter(|&step| step > 0)?),
            None => (part, 1),
        };
        let (from, to) = match range {
            "*" => (min, max),
            _ => match range.split_once('-') {
                Some((from, to)) => (from.parse().ok()?, to.parse().ok()?),
                None => {
                    let from: u32 = range.parse().ok()?;
                    (from, if part.contains('/') { max } else { from })
                }
            },
        };
        if from < min || to > max || from > to {
            return None;
        }
        bits |= (from..=to).step_by(step as usize).fold(0, |bits, n| bits | 1 << n);
    }
    Some(bits)
}

/// `cron` 在 `base` 之后的第一次，按本地的墙上时间：`base` 那天只找墙上钟点比它晚的，秋天回拨的那天同一个钟点
/// 不会到两次。最多往后找五年（2 月 29 日这样的日子要找过闰年），对不上的日子（2 月 30 日）为空。
fn cron(base: u64, cron: &Cron) -> Option<u64> {
    let today = local_tm(libc::time_t::try_from(base).ok()?)?;
    for offset in 0..5 * 366 {
        let mut day = today;
        day.tm_mday += offset;
        // 按中午规范化日期，不受夏令时挪动的钟点影响。
        (day.tm_hour, day.tm_min, day.tm_sec, day.tm_isdst) = (12, 0, 0, -1);
        mktime(&mut day);
        if !cron.day_matches(day.tm_mday, day.tm_mon + 1, day.tm_wday) {
            continue;
        }
        for hour in (0..24).filter(|hour| cron.hours & (1 << hour) != 0) {
            for minute in (0..60).filter(|minute| cron.minutes & (1 << minute) != 0) {
                if offset == 0 && (hour, minute) <= (today.tm_hour, today.tm_min) {
                    continue;
                }
                let mut at = day;
                (at.tm_hour, at.tm_min, at.tm_isdst) = (hour, minute, -1);
                let at = u64::try_from(mktime(&mut at)).ok()?;
                if at > base {
                    return Some(at);
                }
            }
        }
    }
    None
}

/// 每天 `hour:minute`（`days` 不为 0 时只在这几天，第 0 位是星期日）在 `base` 之后的第一次。按本地的墙上
/// 时间算：`base` 那天已经过了这个钟点（含正好到点）就从第二天找起，秋天回拨的那天同一个钟点不会到两次；
/// 春天跳过的钟点由 `mktime` 挪到前后一小时。
fn daily(base: u64, hour: u8, minute: u8, days: u8) -> Option<u64> {
    if hour >= 24 || minute >= 60 {
        return None;
    }
    let today = local_tm(libc::time_t::try_from(base).ok()?)?;
    let passed = (today.tm_hour, today.tm_min) >= (i32::from(hour), i32::from(minute));
    // 第一天加上整整一周，星期几总有一天对得上。
    (i32::from(passed)..i32::from(passed) + 8).find_map(|offset| {
        let mut day = today;
        day.tm_mday += offset;
        day.tm_hour = i32::from(hour);
        day.tm_min = i32::from(minute);
        day.tm_sec = 0;
        day.tm_isdst = -1;
        let at = u64::try_from(mktime(&mut day)).ok()?;
        let weekday = u32::try_from(day.tm_wday).ok()?;
        (at > base && (days == 0 || days & (1 << weekday) != 0)).then_some(at)
    })
}

/// `at`（Unix 时间）在本地是几点（C 库的 `localtime_r`，Windows 上是 `localtime_s`）。
pub(crate) fn local_tm(at: libc::time_t) -> Option<libc::tm> {
    // SAFETY: `tm` 是纯数据，全零是合法的值。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: 两个指针都指向这里的局部变量，调用期间有效；函数只写 `tm`。
    #[cfg(unix)]
    let ok = !unsafe { libc::localtime_r(&raw const at, &raw mut tm) }.is_null();
    // SAFETY: 同上。
    #[cfg(windows)]
    let ok = unsafe { libc::localtime_s(&raw mut tm, &raw const at) } == 0;
    ok.then_some(tm)
}

/// 规范化 `tm` 并填好星期几，返回它的 Unix 时间（C 库的 `mktime`）。
pub(crate) fn mktime(tm: &mut libc::tm) -> libc::time_t {
    #[cfg(windows)]
    unsafe extern "C" {
        // UCRT 只导出带位宽的名字，C 头文件里的 `mktime` 是它的内联别名。
        #[link_name = "_mktime64"]
        safe fn mktime(tm: &mut libc::tm) -> libc::time_t;
    }
    #[cfg(windows)]
    return mktime(tm);
    // SAFETY: 指针来自可变引用，调用期间有效。
    #[cfg(unix)]
    unsafe {
        libc::mktime(tm)
    }
}
