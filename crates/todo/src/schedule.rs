//! 定时按本地时间算下一次到点（`next_fire`）。

use runode_types::todo::When;

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
pub fn local_tm(at: libc::time_t) -> Option<libc::tm> {
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
pub fn mktime(tm: &mut libc::tm) -> libc::time_t {
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
