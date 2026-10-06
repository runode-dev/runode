//! 光标闪烁的节奏：亮、灭各持续多久，空闲多久后停在亮的那一半。计时器怎么起停见
//! `TerminalView::sync_cursor_blink`。

use std::time::Duration;

/// 光标闪烁时亮、灭各持续的时长。
pub(super) const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(600);

/// 闪烁计时器到点时该做什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BlinkStep {
    /// 还没到切换亮灭的时候（等的时候重新计了周期），再等这么久。
    Wait(Duration),
    /// 切换亮灭，然后等 `next` 再来。
    Toggle { next: Duration },
    /// 空闲到了期限：光标停在亮的那一半，计时器退出。
    Stop,
}

/// 计时器到点时，按当前这半个周期过了多久（`phase`）、离最近一次活动（键盘输入、终端输出、
/// 重新获得焦点）过了多久（`idle`）和空闲期限 `timeout`（`None` 表示一直闪）决定下一步。
/// 等待的时长不超过离期限还剩的时间，到期限时正好停下。
pub(super) fn blink_step(phase: Duration, idle: Duration, timeout: Option<Duration>) -> BlinkStep {
    let left = match timeout {
        Some(timeout) if idle >= timeout => return BlinkStep::Stop,
        Some(timeout) => timeout - idle,
        None => Duration::MAX,
    };
    if phase < CURSOR_BLINK_INTERVAL {
        return BlinkStep::Wait((CURSOR_BLINK_INTERVAL - phase).min(left));
    }
    BlinkStep::Toggle { next: CURSOR_BLINK_INTERVAL.min(left) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn blinks_on_the_interval_without_a_timeout() {
        assert_eq!(blink_step(MS(600), MS(60_000), None), BlinkStep::Toggle { next: CURSOR_BLINK_INTERVAL });
        assert_eq!(blink_step(MS(900), MS(0), None), BlinkStep::Toggle { next: CURSOR_BLINK_INTERVAL });
        // 等的时候重新计了周期：等到这个周期结束。
        assert_eq!(blink_step(MS(200), MS(200), None), BlinkStep::Wait(MS(400)));
    }

    #[test]
    fn stops_once_idle_for_the_timeout() {
        let timeout = Some(Duration::from_secs(5));
        assert_eq!(blink_step(MS(600), MS(1_000), timeout), BlinkStep::Toggle { next: CURSOR_BLINK_INTERVAL });
        assert_eq!(blink_step(MS(600), MS(5_000), timeout), BlinkStep::Stop);
        assert_eq!(blink_step(MS(100), MS(7_000), timeout), BlinkStep::Stop);
    }

    #[test]
    fn waits_no_longer_than_the_time_left() {
        let timeout = Some(Duration::from_secs(5));
        // 离期限只剩 200 毫秒：切换后只等 200 毫秒，到点正好停下。
        assert_eq!(blink_step(MS(600), MS(4_800), timeout), BlinkStep::Toggle { next: MS(200) });
        assert_eq!(blink_step(MS(100), MS(4_900), timeout), BlinkStep::Wait(MS(100)));
        assert_eq!(blink_step(MS(100), MS(1_000), timeout), BlinkStep::Wait(MS(500)));
    }
}
