//! 定时的下一次到点（`next_fire`）：按本地的墙上时间算，只在选中的星期几。

use runode_todo::next_fire;
use runode_types::todo::When;

/// `at` 的本地时间：时、分、星期几。
fn local(at: u64) -> (i32, i32, u32) {
    let at = libc::time_t::try_from(at).unwrap();
    // SAFETY: `tm` 是纯数据，全零是合法的值；`localtime_r` 和 `localtime_s` 只写它。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: 两个指针都指向这里的局部变量，调用期间有效。
    #[cfg(unix)]
    assert!(!unsafe { libc::localtime_r(&raw const at, &raw mut tm) }.is_null());
    // SAFETY: 同上。
    #[cfg(windows)]
    assert_eq!(unsafe { libc::localtime_s(&raw mut tm, &raw const at) }, 0);
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

/// `at` 是本地的几号。
fn local_day(at: u64) -> i32 {
    let at = libc::time_t::try_from(at).unwrap();
    // SAFETY: 同 `local`。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: 同 `local`。
    #[cfg(unix)]
    assert!(!unsafe { libc::localtime_r(&raw const at, &raw mut tm) }.is_null());
    // SAFETY: 同 `local`。
    #[cfg(windows)]
    assert_eq!(unsafe { libc::localtime_s(&raw mut tm, &raw const at) }, 0);
    tm.tm_mday
}

fn cron(expr: &str) -> When {
    When::Cron { expr: expr.into() }
}

#[test]
fn cron_lands_on_matching_wall_clock_times() {
    let base = 1_760_000_000;
    // 工作日 9:30。
    let at = next_fire(&cron("30 9 * * 1-5"), base).unwrap();
    assert!(at > base && at <= base + 4 * 24 * 3600);
    let (h, m, weekday) = local(at);
    assert_eq!((h, m), (9, 30));
    assert!((1..=5).contains(&weekday), "weekday {weekday}");
    // 每 15 分钟：落在整刻上，不晚于一刻钟。
    let at = next_fire(&cron("*/15 * * * *"), base).unwrap();
    assert!(at > base && at <= base + 15 * 60);
    assert_eq!(local(at).1 % 15, 0);
    // 日那段以 `*` 开头时两段都要对上：单号日里的星期一。
    let at = next_fire(&cron("0 9 */2 * 1"), base).unwrap();
    assert_eq!((local(at).2, local_day(at) % 2), (1, 1));
    // 隔天：只落在单号日。
    let mut at = base;
    for _ in 0..5 {
        at = next_fire(&cron("0 9 */2 * *"), at).unwrap();
        assert_eq!(local_day(at) % 2, 1, "{at}");
    }
    // 星期日写成 7 也认。
    assert_eq!(local(next_fire(&cron("0 12 * * 7"), base).unwrap()).2, 0);
    // 列出的几个钟点挨个到，正好到点时下一次是下一个。
    let first = next_fire(&cron("0 8,20 * * *"), base).unwrap();
    let second = next_fire(&cron("0 8,20 * * *"), first).unwrap();
    assert_eq!(second - first, 12 * 3600, "unless the clocks changed in between");
    // 每分钟：正好到点和到点后半分钟，下一次都是下一分钟。
    let minute = next_fire(&cron("* * * * *"), base).unwrap();
    assert_eq!(next_fire(&cron("* * * * *"), minute), Some(minute + 60));
    assert_eq!(next_fire(&cron("* * * * *"), minute + 30), Some(minute + 60));
}

#[test]
fn cron_rejects_what_it_cannot_read() {
    for expr in ["", "* * * *", "* * * * * *", "60 * * * *", "* 24 * * *", "* * 0 * *", "* * * 13 *", "* * * * 8"] {
        assert!(!runode_todo::valid_cron(expr), "{expr:?}");
        assert_eq!(next_fire(&cron(expr), 100), None, "{expr:?}");
    }
    for expr in ["5-1 * * * *", "*/0 * * * *", "a * * * *", "1,,2 * * * *"] {
        assert!(!runode_todo::valid_cron(expr), "{expr:?}");
    }
    assert!(runode_todo::valid_cron("0,30 9-17/2 1,15 */3 0"));
    // 写得出来却永远对不上的日子不会到点。
    assert_eq!(next_fire(&cron("0 0 30 2 *"), 1_760_000_000), None);
}
