//! 限速：同一个地址门禁失败太多次，之后一段时间里它来的连接一律回 `RejectReason::RateLimited`，
//! 挡住对着签名和口令硬试的。配对口令另外有自己的上限（连错几次作废），见 `pairing`。

use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    time::{Duration, Instant},
};

/// 一个地址在 `WINDOW` 里失败这么多次后被挡住。
pub(crate) const MAX_FAILURES: usize = 10;
/// 往回看多久的失败。
pub(crate) const WINDOW: Duration = Duration::from_secs(60);
/// 记着的地址超过这么多时，先清掉已经过了 `WINDOW` 的。
const PRUNE_AT: usize = 1024;

#[derive(Default)]
pub(crate) struct RateLimiter {
    failures: HashMap<IpAddr, VecDeque<Instant>>,
}

impl RateLimiter {
    /// `ip` 现在是不是被挡住了。
    pub(crate) fn limited(&mut self, ip: IpAddr, now: Instant) -> bool {
        let Some(times) = self.failures.get_mut(&ip) else { return false };
        forget_old(times, now);
        times.len() >= MAX_FAILURES
    }

    /// `ip` 失败了一次。
    pub(crate) fn failed(&mut self, ip: IpAddr, now: Instant) {
        if self.failures.len() >= PRUNE_AT {
            self.failures.retain(|_, times| {
                forget_old(times, now);
                !times.is_empty()
            });
        }
        let times = self.failures.entry(ip).or_default();
        forget_old(times, now);
        // 已经挡住的地址不再往上记，挡住的时长从最早那次失败起算，过了 `WINDOW` 就放开。
        if times.len() < MAX_FAILURES {
            times.push_back(now);
        }
    }
}

fn forget_old(times: &mut VecDeque<Instant>, now: Instant) {
    while times.front().is_some_and(|&at| now.duration_since(at) >= WINDOW) {
        times.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_many_failures_block_an_address_for_a_while() {
        let mut limiter = RateLimiter::default();
        let start = Instant::now();
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let other: IpAddr = "192.0.2.2".parse().unwrap();
        for i in 0..MAX_FAILURES {
            assert!(!limiter.limited(ip, start), "{i}");
            limiter.failed(ip, start + Duration::from_millis(i as u64));
        }
        assert!(limiter.limited(ip, start + Duration::from_secs(1)));
        assert!(!limiter.limited(other, start));
        assert!(!limiter.limited(ip, start + WINDOW + Duration::from_secs(1)));
    }
}
