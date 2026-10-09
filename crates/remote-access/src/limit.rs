//! 限速：同一个地址门禁失败太多次，之后一段时间里它来的连接一律回 `RejectReason::RateLimited`，
//! 挡住对着签名和口令硬试的。IPv6 按 /64 前缀算一个来源：一台设备通常就拿到一整个 /64，换个地址
//! 不该换出新的额度。

use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv6Addr},
    time::{Duration, Instant},
};

/// 一个地址在 `WINDOW` 里失败这么多次后被挡住。
pub(crate) const MAX_FAILURES: usize = 10;
/// 往回看多久的失败。
pub(crate) const WINDOW: Duration = Duration::from_secs(60);
/// 最多记着这么多个来源。满了时先清掉已经过了 `WINDOW` 的，还是满的就挤掉最久没失败的那个。
const MAX_SOURCES: usize = 1024;

#[derive(Default)]
pub(crate) struct RateLimiter {
    failures: HashMap<IpAddr, VecDeque<Instant>>,
}

impl RateLimiter {
    /// `ip` 现在是不是被挡住了。
    pub(crate) fn limited(&mut self, ip: IpAddr, now: Instant) -> bool {
        let Some(times) = self.failures.get_mut(&source(ip)) else { return false };
        forget_old(times, now);
        times.len() >= MAX_FAILURES
    }

    /// `ip` 失败了一次。
    pub(crate) fn failed(&mut self, ip: IpAddr, now: Instant) {
        let key = source(ip);
        if !self.failures.contains_key(&key) && self.failures.len() >= MAX_SOURCES {
            self.failures.retain(|_, times| {
                forget_old(times, now);
                !times.is_empty()
            });
            // 都还在窗口里：表不再长，挤掉最久没失败的。
            if self.failures.len() >= MAX_SOURCES
                && let Some(oldest) =
                    self.failures.iter().min_by_key(|(_, times)| times.back().copied()).map(|(&k, _)| k)
            {
                self.failures.remove(&oldest);
            }
        }
        let times = self.failures.entry(key).or_default();
        forget_old(times, now);
        // 已经挡住的地址不再往上记，挡住的时长从最早那次失败起算，过了 `WINDOW` 就放开。
        if times.len() < MAX_FAILURES {
            times.push_back(now);
        }
    }
}

/// 限速和门禁并发按什么算同一个来源：IPv4 是地址本身，IPv6 是它的 /64 前缀（IPv4 映射成的 IPv6
/// 地址按 IPv4 算）。
pub(crate) fn source(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_canonical() {
            IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & !(u128::MAX >> 64))),
            v4 => v4,
        },
        v4 => v4,
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

    #[test]
    fn ipv6_addresses_in_one_slash_64_share_a_limit() {
        let mut limiter = RateLimiter::default();
        let now = Instant::now();
        for i in 0..MAX_FAILURES {
            limiter.failed(format!("2001:db8:1:2::{:x}", i + 1).parse().unwrap(), now);
        }
        assert!(limiter.limited("2001:db8:1:2:ffff::9".parse().unwrap(), now));
        assert!(!limiter.limited("2001:db8:1:3::1".parse().unwrap(), now));
        // IPv4 映射成的 IPv6 地址各自按 IPv4 算，不因为前 64 位相同挤到一起。
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        for _ in 0..MAX_FAILURES {
            limiter.failed(mapped, now);
        }
        assert!(limiter.limited("192.0.2.1".parse().unwrap(), now));
        assert!(!limiter.limited("::ffff:192.0.2.2".parse().unwrap(), now));
    }

    #[test]
    fn the_table_stays_bounded_when_every_source_is_recent() {
        let mut limiter = RateLimiter::default();
        let start = Instant::now();
        for i in 0..MAX_SOURCES as u32 {
            limiter.failed(IpAddr::V4((0x0a00_0000 + i).into()), start + Duration::from_millis(u64::from(i)));
        }
        assert_eq!(limiter.failures.len(), MAX_SOURCES);
        let newcomer: IpAddr = "192.0.2.1".parse().unwrap();
        let later = start + Duration::from_secs(2);
        limiter.failed(newcomer, later);
        assert_eq!(limiter.failures.len(), MAX_SOURCES);
        // 挤掉的是最久没失败的那个，新来的记下了。
        assert!(!limiter.failures.contains_key(&IpAddr::V4(0x0a00_0000.into())));
        assert!(limiter.failures.contains_key(&IpAddr::V4(0x0a00_0001.into())));
        assert_eq!(limiter.failures[&newcomer].len(), 1);
    }
}
