//! 定时按本地时间算下一次到点（`next_fire`）。

use runode_protocol::todo::When;

/// `when` 从 `base`（上一次到点或者建好的时刻，Unix 时间的秒数）算起的下一次到点；不会再到点时为空。
pub fn next_fire(when: &When, base: u64) -> Option<u64> {
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
