//! 限速：同一个来源门禁失败太多次，之后一段时间里它来的连接一律不让进，挡住对着签名和口令硬试的。
//! 来源是对端的地址，经 iroh 隧道来的按对端公钥算（`Source`）。
//! IPv6 在单个地址的上限之外，还给它所在的 /64 前缀记一个合计的上限：换个地址不该换出新的额度，
//! 但局域网里一段 /64 通常是所有设备共用的，所以合计的上限比单个地址高，一台设备锁不住其余的。
//! 最近过过门禁的来源（`RateLimiter::succeeded`）不看前缀的合计：同一段里别人换着地址打满前缀，
//! 挡不住已经连过的手机。只有签名、口令不对才算失败，只开 TCP 再关、设备没登记这些
//! 不算，不然谁都能替别人攒满额度。

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    net::{IpAddr, Ipv6Addr},
    time::{Duration, Instant},
};

use runode_protocol::remote::DeviceId;

/// 一个来源在 `WINDOW` 里失败这么多次后被挡住。
pub(crate) const MAX_FAILURES: usize = 10;
/// 一个 IPv6 /64 前缀里所有地址合起来在 `WINDOW` 里失败这么多次后，整个前缀被挡住。
pub(crate) const MAX_FAILURES_PER_PREFIX: usize = 50;
/// 往回看多久的失败。
pub(crate) const WINDOW: Duration = Duration::from_secs(60);
/// 最多记着这么多个计数。满了时先清掉已经过了 `WINDOW` 的，还是满的就挤掉最久没失败的那个。
const MAX_COUNTERS: usize = 1024;
/// 最多记着这么多个过过门禁的来源，满了挤掉最久没过的那个。
const MAX_TRUSTED: usize = 64;

/// 一条连接从哪儿来。IPv4 映射成的 IPv6 地址按 IPv4 记，同一个地址才认得出是同一个来源，所以地址经
/// `From<IpAddr>` 转成来源。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Source {
    Ip(IpAddr),
    /// 经 iroh 隧道来的，按对端的公钥。
    Tunnel(iroh::EndpointId),
}

/// 入场名额上合计的一组来源。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Group {
    /// 同一个 IPv6 /64 前缀里的地址；IPv4 地址各自一组。
    Prefix(IpAddr),
    /// 所有经隧道来的：换个公钥不费事，所以入场名额上算一组，换着公钥连的合起来也只占一组的名额。失败计数
    /// 仍按各个公钥记、不合计，换着公钥的攻击者才封不住还没过过门禁的公钥（新配对的手机、不存钥的 App）。
    Tunnel,
}

impl Source {
    /// 它在入场名额上算哪一组。
    pub(crate) fn group(self) -> Group {
        match self {
            Source::Ip(ip) => Group::Prefix(prefix(ip)),
            Source::Tunnel(_) => Group::Tunnel,
        }
    }

    /// 凭它认得出是哪台设备。回环地址认不出：本机转进来的连接都从它来，不凭它信任谁，也不当作设备的
    /// 来源记下。
    pub(crate) fn identifies_device(self) -> bool {
        match self {
            Source::Ip(ip) => !ip.to_canonical().is_loopback(),
            Source::Tunnel(_) => true,
        }
    }
}

impl From<IpAddr> for Source {
    fn from(ip: IpAddr) -> Self {
        Source::Ip(ip.to_canonical())
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Ip(ip) => write!(f, "{ip}"),
            Source::Tunnel(id) => write!(f, "iroh {}", id.fmt_short()),
        }
    }
}

/// 一个计数记的是什么：一个来源，或者一个 IPv6 的 /64 前缀。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Key {
    Source(Source),
    Prefix(IpAddr),
}

#[derive(Default)]
pub(crate) struct RateLimiter {
    failures: HashMap<Key, VecDeque<Instant>>,
    /// 过过门禁的来源，带最近一次过的时刻和过的是哪台设备。监听开起来时先填上设备表里记着的各台
    /// 设备上次登录的来源。
    trusted: HashMap<Source, (Instant, DeviceId)>,
}

impl RateLimiter {
    /// `source` 现在是不是被挡住了。过过门禁的来源只看它自己的计数。
    pub(crate) fn limited(&mut self, source: Source, now: Instant) -> bool {
        let trusted = self.trusted(source);
        counters(source).into_iter().filter(|(key, _)| !(trusted && matches!(key, Key::Prefix(_)))).any(|(key, max)| {
            self.failures.get_mut(&key).is_some_and(|times| {
                forget_old(times, now);
                times.len() >= max
            })
        })
    }

    /// `source` 失败了一次。
    pub(crate) fn failed(&mut self, source: Source, now: Instant) {
        for (key, max) in counters(source) {
            self.record(key, max, now);
        }
    }

    /// `source` 过了门禁，是设备 `device`。认不出设备的来源（`Source::identifies_device`）不记。
    pub(crate) fn succeeded(&mut self, source: Source, device: DeviceId, now: Instant) {
        if !source.identifies_device() {
            return;
        }
        if !self.trusted.contains_key(&source)
            && self.trusted.len() >= MAX_TRUSTED
            && let Some(oldest) = self.trusted.iter().min_by_key(|&(_, (at, _))| *at).map(|(&source, _)| source)
        {
            self.trusted.remove(&oldest);
        }
        self.trusted.insert(source, (now, device));
    }

    /// 设备已不在 `known` 里（撤销了）的来源不再算过过门禁。
    pub(crate) fn revoke(&mut self, known: &HashSet<DeviceId>) {
        self.trusted.retain(|_, (_, device)| known.contains(device));
    }

    /// `source` 过过门禁。
    pub(crate) fn trusted(&self, source: Source) -> bool {
        self.trusted.contains_key(&source)
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

/// `source` 的失败记在哪几个计数上，各带自己的上限：它自己，IPv6 地址还有它所在的 /64 前缀。
fn counters(source: Source) -> Vec<(Key, usize)> {
    let mut counters = vec![(Key::Source(source), MAX_FAILURES)];
    if let Source::Ip(ip) = source
        && ip.is_ipv6()
    {
        counters.push((Key::Prefix(prefix(ip)), MAX_FAILURES_PER_PREFIX));
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

    fn ip(text: &str) -> Source {
        text.parse::<IpAddr>().unwrap().into()
    }

    fn tunnel(n: u8) -> Source {
        Source::Tunnel(iroh::SecretKey::from_bytes(&[n; 32]).public())
    }

    fn device(n: u8) -> DeviceId {
        DeviceId([n; 16])
    }

    #[test]
    fn too_many_failures_block_an_address_for_a_while() {
        let mut limiter = RateLimiter::default();
        let start = Instant::now();
        let peer = ip("192.0.2.1");
        let other = ip("192.0.2.2");
        for i in 0..MAX_FAILURES {
            assert!(!limiter.limited(peer, start), "{i}");
            limiter.failed(peer, start + Duration::from_millis(i as u64));
        }
        assert!(limiter.limited(peer, start + Duration::from_secs(1)));
        assert!(!limiter.limited(other, start));
        assert!(!limiter.limited(peer, start + WINDOW + Duration::from_secs(1)));
    }

    #[test]
    fn one_ipv6_address_cannot_lock_out_the_rest_of_its_slash_64() {
        let mut limiter = RateLimiter::default();
        let now = Instant::now();
        let noisy = ip("2001:db8:1:2::1");
        for _ in 0..MAX_FAILURES {
            limiter.failed(noisy, now);
        }
        assert!(limiter.limited(noisy, now));
        assert!(!limiter.limited(ip("2001:db8:1:2::2"), now));
    }

    #[test]
    fn rotating_ipv6_addresses_in_one_slash_64_share_a_limit() {
        let mut limiter = RateLimiter::default();
        let now = Instant::now();
        for i in 0..MAX_FAILURES_PER_PREFIX {
            assert!(!limiter.limited(ip("2001:db8:1:2:ffff::9"), now), "{i}");
            limiter.failed(ip(&format!("2001:db8:1:2::{:x}", i + 1)), now);
        }
        assert!(limiter.limited(ip("2001:db8:1:2:ffff::9"), now));
        assert!(!limiter.limited(ip("2001:db8:1:3::1"), now));
        // IPv4 映射成的 IPv6 地址各自按 IPv4 算，不因为前 64 位相同挤到一起。
        let mapped = ip("::ffff:192.0.2.1");
        for _ in 0..MAX_FAILURES {
            limiter.failed(mapped, now);
        }
        assert!(limiter.limited(ip("192.0.2.1"), now));
        assert!(!limiter.limited(ip("::ffff:192.0.2.2"), now));
    }

    #[test]
    fn an_address_that_got_in_before_is_not_locked_out_by_its_slash_64() {
        let mut limiter = RateLimiter::default();
        let now = Instant::now();
        let phone = ip("2001:db8:1:2::abcd");
        limiter.succeeded(phone, device(1), now);
        for i in 0..MAX_FAILURES_PER_PREFIX {
            limiter.failed(ip(&format!("2001:db8:1:2::{:x}", i + 1)), now);
        }
        assert!(limiter.limited(ip("2001:db8:1:2:ffff::9"), now));
        assert!(!limiter.limited(phone, now));
        // 它自己的计数照样算。
        for _ in 0..MAX_FAILURES {
            limiter.failed(phone, now);
        }
        assert!(limiter.limited(phone, now));
    }

    #[test]
    fn a_tunnel_peer_that_fails_too_often_blocks_only_itself() {
        let mut limiter = RateLimiter::default();
        let now = Instant::now();
        for i in 0..MAX_FAILURES {
            assert!(!limiter.limited(tunnel(1), now), "{i}");
            limiter.failed(tunnel(1), now);
        }
        assert!(limiter.limited(tunnel(1), now));
        assert!(!limiter.limited(tunnel(2), now));
        assert!(!limiter.limited(ip("127.0.0.1"), now));
    }

    #[test]
    fn rotating_tunnel_peers_do_not_share_a_limit() {
        let mut limiter = RateLimiter::default();
        let now = Instant::now();
        let peers = u8::try_from(MAX_FAILURES_PER_PREFIX).unwrap() + 1;
        for n in 0..peers {
            limiter.failed(tunnel(n), now);
        }
        assert!(!limiter.limited(tunnel(peers), now));
    }

    #[test]
    fn trust_is_kept_per_source() {
        let mut limiter = RateLimiter::default();
        limiter.succeeded(tunnel(1), device(1), Instant::now());
        assert!(limiter.trusted(tunnel(1)));
        assert!(!limiter.trusted(tunnel(2)));
        assert!(!limiter.trusted(ip("127.0.0.1")));
    }

    #[test]
    fn loopback_addresses_are_never_trusted() {
        let mut limiter = RateLimiter::default();
        let now = Instant::now();
        for loopback in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
            limiter.succeeded(ip(loopback), device(1), now);
            assert!(!limiter.trusted(ip(loopback)), "{loopback}");
        }
        assert!(limiter.trusted.is_empty());
    }

    #[test]
    fn revoked_devices_lose_their_trust() {
        let mut limiter = RateLimiter::default();
        let now = Instant::now();
        limiter.succeeded(tunnel(1), device(1), now);
        limiter.succeeded(ip("192.0.2.1"), device(1), now);
        limiter.succeeded(tunnel(2), device(2), now);
        limiter.revoke(&HashSet::from([device(2)]));
        assert!(!limiter.trusted(tunnel(1)));
        assert!(!limiter.trusted(ip("192.0.2.1")));
        assert!(limiter.trusted(tunnel(2)));
    }

    #[test]
    fn the_trusted_table_stays_bounded() {
        let mut limiter = RateLimiter::default();
        let start = Instant::now();
        let nth = |i: u32| Source::from(IpAddr::V4((0x0a00_0000 + i).into()));
        for i in 0..=MAX_TRUSTED as u32 {
            limiter.succeeded(nth(i), device(1), start + Duration::from_millis(u64::from(i)));
        }
        assert_eq!(limiter.trusted.len(), MAX_TRUSTED);
        assert!(!limiter.trusted(nth(0)));
        assert!(limiter.trusted(nth(MAX_TRUSTED as u32)));
    }

    #[test]
    fn the_table_stays_bounded_when_every_source_is_recent() {
        let mut limiter = RateLimiter::default();
        let start = Instant::now();
        let nth = |i: u32| Source::from(IpAddr::V4((0x0a00_0000 + i).into()));
        for i in 0..MAX_COUNTERS as u32 {
            limiter.failed(nth(i), start + Duration::from_millis(u64::from(i)));
        }
        assert_eq!(limiter.failures.len(), MAX_COUNTERS);
        let newcomer = ip("192.0.2.1");
        let later = start + Duration::from_secs(2);
        limiter.failed(newcomer, later);
        assert_eq!(limiter.failures.len(), MAX_COUNTERS);
        // 挤掉的是最久没失败的那个，新来的记下了。
        assert!(!limiter.failures.contains_key(&Key::Source(nth(0))));
        assert!(limiter.failures.contains_key(&Key::Source(nth(1))));
        assert_eq!(limiter.failures[&Key::Source(newcomer)].len(), 1);
    }
}
