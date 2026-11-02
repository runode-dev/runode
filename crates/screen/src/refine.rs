//! 画面静止以后把最后一张补清楚：动着的时候码率摊到每一帧上，停下时最后一帧往往是糊的，而屏幕不变就不再出新帧。
//! 停了 `STILL_DELAY` 以后把最后一张按 `REFINE_BOOST` 倍的码率再编 `REFINE_FRAMES` 次，每次只补和上一帧的差别，
//! 几帧下来文字就清楚了；屏幕一变就停下、回到原来的码率。

use std::time::{Duration, Instant};

/// 屏幕不变这么久算停下。
const STILL_DELAY: Duration = Duration::from_millis(300);
/// 停下以后补这么多帧。
const REFINE_FRAMES: u32 = 4;
/// 补的两帧之间隔这么久，编码器和网络来得及一帧帧送走。
const REFINE_GAP: Duration = Duration::from_millis(50);
/// 轮询的采集循环等新画面最多这么久就回来看一眼要不要停、要不要补、要不要重编。
#[cfg(any(windows, target_os = "linux", test))]
const FRAME_WAIT: Duration = Duration::from_millis(100);
/// 要了关键帧这么久还没有新画面把它带出来，就重编最近一张：屏幕在变时用不着重编，接连要关键帧时也不至于
/// 一直重编。
#[cfg(any(target_os = "macos", windows))]
pub(crate) const REPEAT_DELAY: Duration = Duration::from_millis(100);

/// 什么时候该补一帧。
#[derive(Debug, Default)]
pub(crate) struct Refiner {
    /// 上次编新画面的时刻；还没有画面时为空。
    changed: Option<Instant>,
    /// 这次停下以后补了几帧、上次补的时刻。
    refined: u32,
    last: Option<Instant>,
}

impl Refiner {
    /// 刚编了一张新画面。
    pub(crate) fn changed(&mut self, now: Instant) {
        self.changed = Some(now);
        self.refined = 0;
        self.last = None;
    }

    /// 刚补了一帧。
    pub(crate) fn refined(&mut self, now: Instant) {
        self.refined += 1;
        self.last = Some(now);
    }

    /// 离该补下一帧还有多久，到了时是零；这次停下已经补够了、还没有画面时为空。
    pub(crate) fn wait(&self, now: Instant) -> Option<Duration> {
        let changed = self.changed?;
        if self.refined >= REFINE_FRAMES {
            return None;
        }
        let at = match self.last {
            Some(last) => last + REFINE_GAP,
            None => changed + STILL_DELAY,
        };
        Some(at.saturating_duration_since(now))
    }

    /// 轮询的采集循环这一轮最多等新画面多久：不超过 `FRAME_WAIT`，该补下一帧时提前回来。
    #[cfg(any(windows, target_os = "linux", test))]
    pub(crate) fn timeout(&self, now: Instant) -> Duration {
        self.wait(now).map_or(FRAME_WAIT, |wait| wait.min(FRAME_WAIT))
    }

    /// 编出了一帧：`refine` 时是补清楚的那帧，否则是新画面或按原码率重编的关键帧，停下以后从头补。
    #[cfg(any(windows, target_os = "linux", test))]
    pub(crate) fn encoded(&mut self, now: Instant, refine: bool) {
        if refine { self.refined(now) } else { self.changed(now) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refines_a_few_frames_after_the_screen_stops() {
        let start = Instant::now();
        let mut refiner = Refiner::default();
        assert_eq!(refiner.wait(start), None, "nothing to refine before the first frame");
        refiner.changed(start);
        assert_eq!(refiner.wait(start), Some(STILL_DELAY));
        let mut now = start + STILL_DELAY;
        for _ in 0..REFINE_FRAMES {
            assert_eq!(refiner.wait(now), Some(Duration::ZERO));
            refiner.refined(now);
            now += REFINE_GAP;
        }
        assert_eq!(refiner.wait(now), None, "enough refining for this pause");
        // 屏幕又变了，停下以后再补。
        refiner.changed(now);
        assert_eq!(refiner.wait(now + STILL_DELAY), Some(Duration::ZERO));
    }

    /// 轮询等新画面不超过 `FRAME_WAIT`，快该补时提前回来；没有画面时照 `FRAME_WAIT` 等。
    #[test]
    fn polls_no_longer_than_the_next_refine() {
        let start = Instant::now();
        let mut refiner = Refiner::default();
        assert_eq!(refiner.timeout(start), FRAME_WAIT);
        refiner.encoded(start, false);
        assert_eq!(refiner.timeout(start), FRAME_WAIT.min(STILL_DELAY));
        let now = start + STILL_DELAY;
        assert_eq!(refiner.timeout(now), Duration::ZERO);
        refiner.encoded(now, true);
        assert_eq!(refiner.timeout(now), REFINE_GAP.min(FRAME_WAIT));
    }
}
