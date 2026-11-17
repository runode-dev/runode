//! 按键限频的令牌桶：桶满时能连着来 `capacity` 次，之后每过 `period / capacity` 补回一次。

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

/// 最多记这么多个键，免得被大量的键撑爆内存。
const MAX_KEYS: usize = 100_000;

pub struct Limiter {
    capacity: f64,
    per_second: f64,
    max_keys: usize,
    buckets: Mutex<HashMap<String, Bucket>>,
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Limiter {
    pub fn new(capacity: u32, period: Duration) -> Self {
        Self::with_max_keys(capacity, period, MAX_KEYS)
    }

    fn with_max_keys(capacity: u32, period: Duration, max_keys: usize) -> Self {
        let capacity = f64::from(capacity);
        Self { capacity, per_second: capacity / period.as_secs_f64(), max_keys, buckets: Mutex::new(HashMap::new()) }
    }

    /// 记 `key` 的一次请求；超了时不记，返回至少还要等多久。
    pub fn take(&self, key: &str, now: Instant) -> Result<(), Duration> {
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if buckets.len() >= self.max_keys && !buckets.contains_key(key) {
            self.evict(&mut buckets, now);
        }
        let bucket = buckets.entry(key.to_owned()).or_insert(Bucket { tokens: self.capacity, at: now });
        bucket.tokens = self.level(bucket, now);
        bucket.at = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - bucket.tokens) / self.per_second))
        }
    }

    /// 表满时腾地方：先丢已补满的桶，还多就丢最接近补满的一批，一次腾出不止一个位置。被丢的键下次回到
    /// 满额，丢最满的放过的额度最少。
    fn evict(&self, buckets: &mut HashMap<String, Bucket>, now: Instant) {
        buckets.retain(|_, bucket| self.level(bucket, now) < self.capacity);
        let keep = self.max_keys.saturating_sub((self.max_keys / 10).max(1));
        if buckets.len() <= keep {
            return;
        }
        let excess = buckets.len() - keep;
        let mut levels: Vec<(f64, &String)> =
            buckets.iter().map(|(key, bucket)| (self.level(bucket, now), key)).collect();
        levels.select_nth_unstable_by(excess - 1, |a, b| b.0.total_cmp(&a.0));
        let fullest: Vec<String> = levels[..excess].iter().map(|(_, key)| (*key).clone()).collect();
        for key in fullest {
            buckets.remove(&key);
        }
    }

    fn level(&self, bucket: &Bucket, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(bucket.at).as_secs_f64();
        (bucket.tokens + elapsed * self.per_second).min(self.capacity)
    }
}

/// 按地址限频时用的键：IPv6 按 /64 算，一台机器通常分到整个 /64，换个地址绕不过去。
pub fn ip_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_drain_then_refill() {
        let limiter = Limiter::new(2, Duration::from_secs(10));
        let start = Instant::now();
        assert!(limiter.take("a", start).is_ok());
        assert!(limiter.take("a", start).is_ok());
        let wait = limiter.take("a", start).unwrap_err();
        assert!(wait > Duration::from_secs(4) && wait <= Duration::from_secs(5), "{wait:?}");
        // 别的键不受影响。
        assert!(limiter.take("b", start).is_ok());
        assert!(limiter.take("a", start + Duration::from_secs(5)).is_ok());
        assert!(limiter.take("a", start + Duration::from_secs(5)).is_err());
    }

    /// 表满时取空了的桶留着，不然换一批新键就能把它的额度刷回满额。
    #[test]
    fn a_full_table_keeps_drained_buckets() {
        let limiter = Limiter::with_max_keys(10, Duration::from_secs(60), 100);
        let now = Instant::now();
        for _ in 0..10 {
            assert!(limiter.take("victim", now).is_ok());
        }
        assert!(limiter.take("victim", now).is_err());
        for i in 0..200 {
            assert!(limiter.take(&format!("key{i}"), now).is_ok(), "key{i}");
        }
        assert!(limiter.take("victim", now).is_err());
    }

    #[test]
    fn a_full_table_stays_bounded() {
        let limiter = Limiter::with_max_keys(10, Duration::from_secs(60), 100);
        let now = Instant::now();
        for i in 0..1000 {
            assert!(limiter.take(&format!("key{i}"), now).is_ok(), "key{i}");
        }
        assert!(limiter.buckets.lock().unwrap().len() <= 100);
    }

    /// 还多时丢的是最接近补满的桶，余额少的留着。
    #[test]
    fn a_full_table_drops_the_fullest_buckets() {
        let limiter = Limiter::with_max_keys(10, Duration::from_secs(60), 100);
        let now = Instant::now();
        for i in 0..100 {
            let key = format!("key{i}");
            // 前 50 个取到只剩 1 次，后 50 个只取 1 次。
            let takes = if i < 50 { 9 } else { 1 };
            for _ in 0..takes {
                assert!(limiter.take(&key, now).is_ok());
            }
        }
        assert!(limiter.take("new", now).is_ok());
        let buckets = limiter.buckets.lock().unwrap();
        assert!(buckets.len() <= 100);
        assert!((0..50).all(|i| buckets.contains_key(&format!("key{i}"))));
        assert!(buckets.contains_key("new"));
    }

    #[test]
    fn ipv6_addresses_share_their_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
        assert_eq!(ip_key(a), ip_key(b));
        assert_ne!(ip_key(a), ip_key("2001:db8:1:3::1".parse().unwrap()));
        assert_eq!(ip_key("::ffff:10.0.0.1".parse().unwrap()), "10.0.0.1");
    }
}
