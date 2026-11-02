//! 光标闪烁：亮、灭各持续多久，空闲多久后停在亮的那一半（`blink_step`），以及一个终端闪到哪了
//! （`CursorBlink`）。

use std::time::{Duration, Instant};

use gpui::Task;

/// 光标闪烁时亮、灭各持续的时长。
pub(super) const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(600);

/// 闪烁计时器到点时该做什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlinkStep {
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
fn blink_step(phase: Duration, idle: Duration, timeout: Option<Duration>) -> BlinkStep {
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

/// 一个终端的光标闪到哪了，和让它闪的计时器。计时器只在有焦点、光标在闪时跑，光标不闪时没有任何
/// 计时器在跑，空闲的窗口就不再重画。
pub(super) struct CursorBlink {
    /// 光标当前处于亮的一半周期。
    visible: bool,
    /// 当前这半个闪烁周期从何时开始。
    since: Instant,
    /// 最近一次活动的时刻，空闲期限从这里算。
    active_at: Instant,
    /// 因空闲到期停了，光标常亮；有了活动（`reset`）才恢复。
    stopped: bool,
    timer: Option<Task<()>>,
}

impl CursorBlink {
    pub fn new() -> Self {
        let now = Instant::now();
        Self { visible: true, since: now, active_at: now, stopped: false, timer: None }
    }

    /// 光标这一刻亮着。
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// 有了活动：光标立即亮起，从头计闪烁周期和空闲期限，空闲停了的也恢复。不碰计时器，只改时刻；
    /// 返回要不要重画：正灭着的要重画才亮得起来，停了的要重画一次，绘制时才会重新起计时器。
    pub fn reset(&mut self) -> bool {
        let repaint = !self.visible || self.stopped;
        self.visible = true;
        self.stopped = false;
        let now = Instant::now();
        self.since = now;
        self.active_at = now;
        repaint
    }

    /// 这一帧光标不闪：停掉计时器，光标算亮着。
    pub fn halt(&mut self) {
        if self.timer.take().is_some() {
            self.visible = true;
        }
    }

    /// 失去焦点：停掉计时器，下次获得焦点时 `reset` 再让它亮。
    pub fn pause(&mut self) {
        self.timer = None;
    }

    /// 光标要闪时该起计时器了：没在跑，也不是空闲到期停的（那样每次绘制都会把它拉起来）。
    pub fn wants_timer(&self) -> bool {
        self.timer.is_none() && !self.stopped
    }

    /// 起了计时器，开始闪也算一次活动，从现在起计空闲期限。
    pub fn start(&mut self, timer: Task<()>) {
        let now = Instant::now();
        self.visible = true;
        self.since = now;
        self.active_at = now;
        self.timer = Some(timer);
    }

    /// 计时器到点，`timeout` 是空闲期限：切换亮灭，或者空闲到期后停在亮的那一半。返回下次再等多久
    /// （停了时为空）和要不要重画。
    pub fn tick(&mut self, timeout: Option<Duration>) -> (Option<Duration>, bool) {
        match blink_step(self.since.elapsed(), self.active_at.elapsed(), timeout) {
            BlinkStep::Wait(wait) => (Some(wait), false),
            BlinkStep::Toggle { next } => {
                self.visible = !self.visible;
                self.since = Instant::now();
                (Some(next), true)
            }
            BlinkStep::Stop => {
                let repaint = !self.visible;
                self.visible = true;
                self.stopped = true;
                // 这就是正在跑的那个计时器：丢掉句柄后它这一轮返回就结束，`wants_timer` 才会在恢复时
                // 让绘制重新起一个。
                self.timer = None;
                (None, repaint)
            }
        }
    }
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
