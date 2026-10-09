//! 限速：同一个地址门禁失败太多次，之后一段时间里它来的连接一律不让进，挡住对着签名和口令硬试的。
//! IPv6 在单个地址的上限之外，还给它所在的 /64 前缀记一个合计的上限：换个地址不该换出新的额度，
//! 但局域网里一段 /64 通常是所有设备共用的，所以合计的上限比单个地址高，一台设备锁不住其余的。

use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, Ipv6Addr},
    time::{Duration, Instant},
};

/// 一个地址在 `WINDOW` 里失败这么多次后被挡住。
pub(crate) const MAX_FAILURES: usize = 10;
/// 一个 IPv6 /64 前缀里所有地址合起来在 `WINDOW` 里失败这么多次后，整个前缀被挡住。
pub(crate) const MAX_FAILURES_PER_PREFIX: usize = 50;
/// 往回看多久的失败。
pub(crate) const WINDOW: Duration = Duration::from_secs(60);
/// 最多记着这么多个计数。满了时先清掉已经过了 `WINDOW` 的，还是满的就挤掉最久没失败的那个。
const MAX_COUNTERS: usize = 1024;

/// 一个计数记的是什么：一个地址，或者（第二项为 `true`）一个 IPv6 的 /64 前缀。
type Key = (IpAddr, bool);

#[derive(Default)]
pub(crate) struct RateLimiter {
    failures: HashMap<Key, VecDeque<Instant>>,
}

impl RateLimiter {
    /// `ip` 现在是不是被挡住了。
    pub(crate) fn limited(&mut self, ip: IpAddr, now: Instant) -> bool {
        counters(ip).into_iter().any(|(key, max)| {
            self.failures.get_mut(&key).is_some_and(|times| {
                forget_old(times, now);
                times.len() >= max
            })
        })
    }

    /// `ip` 失败了一次。
    pub(crate) fn failed(&mut self, ip: IpAddr, now: Instant) {
        for (key, max) in counters(ip) {
            self.record(key, max, now);
        }
    }

    fn record(&mut self, key: Key, max: usize, now: Instant) {
        if !self.failures.contains_key(&key) && self.failures.len() >= MAX_COUNTERS {
            self.failures.retain(|_, times| {
                forget_old(times, now);
                !times.is_empty()
            });
            // 都还在窗口里：表不再长，挤掉最久没失败的。
            if self.failures.len() >= MAX_COUNTERS
                && let Some(oldest) =
                    self.failures.iter().min_by_key(|(_, times)| times.back().copied()).map(|(&k, _)| k)
            {
                self.failures.remove(&oldest);
            }
        }
        let times = self.failures.entry(key).or_default();
        forget_old(times, now);
        // 已经挡住的不再往上记，挡住的时长从最早那次失败起算，过了 `WINDOW` 就放开。
        if times.len() < max {
            times.push_back(now);
        }
    }
}

/// `ip` 的失败记在哪几个计数上，各带自己的上限：它自己，IPv6 还有它所在的 /64 前缀。
fn counters(ip: IpAddr) -> Vec<(Key, usize)> {
    let ip = ip.to_canonical();
    let mut counters = vec![((ip, false), MAX_FAILURES)];
    if ip.is_ipv6() {
        counters.push(((prefix(ip), true), MAX_FAILURES_PER_PREFIX));
    }
    counters
}

/// IPv6 地址所在的 /64 前缀，IPv4 是地址本身；IPv4 映射成的 IPv6 地址按 IPv4 算。
pub(crate) fn prefix(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & !(u128::MAX >> 64))),
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
    fn one_ipv6_address_cannot_lock_out_the_rest_of_its_slash_64() {
        let mut limiter = RateLimiter::default();
        let now = Instant::now();
        let noisy: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        for _ in 0..MAX_FAILURES {
            limiter.failed(noisy, now);
        }
        assert!(limiter.limited(noisy, now));
        assert!(!limiter.limited("2001:db8:1:2::2".parse().unwrap(), now));
    }

    #[test]
    fn rotating_ipv6_addresses_in_one_slash_64_share_a_limit() {
        let mut limiter = RateLimiter::default();
        let now = Instant::now();
        for i in 0..MAX_FAILURES_PER_PREFIX {
            assert!(!limiter.limited("2001:db8:1:2:ffff::9".parse().unwrap(), now), "{i}");
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
        for i in 0..MAX_COUNTERS as u32 {
            limiter.failed(IpAddr::V4((0x0a00_0000 + i).into()), start + Duration::from_millis(u64::from(i)));
        }
        assert_eq!(limiter.failures.len(), MAX_COUNTERS);
        let newcomer: IpAddr = "192.0.2.1".parse().unwrap();
        let later = start + Duration::from_secs(2);
        limiter.failed(newcomer, later);
        assert_eq!(limiter.failures.len(), MAX_COUNTERS);
        // 挤掉的是最久没失败的那个，新来的记下了。
        assert!(!limiter.failures.contains_key(&(IpAddr::V4(0x0a00_0000.into()), false)));
        assert!(limiter.failures.contains_key(&(IpAddr::V4(0x0a00_0001.into()), false)));
        assert_eq!(limiter.failures[&(newcomer, false)].len(), 1);
    }
}
