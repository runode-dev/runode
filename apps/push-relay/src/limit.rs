//! 按键限频的令牌桶：桶满时能连着来 `capacity` 次，之后每过 `period / capacity` 补回一次。

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

/// 最多记这么多个键；满了先丢掉已经补满的桶，还满就全清掉，免得被大量的键撑爆内存。
const MAX_KEYS: usize = 100_000;

pub struct Limiter {
    capacity: f64,
    per_second: f64,
    buckets: Mutex<HashMap<String, Bucket>>,
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Limiter {
    pub fn new(capacity: u32, period: Duration) -> Self {
        let capacity = f64::from(capacity);
        Self { capacity, per_second: capacity / period.as_secs_f64(), buckets: Mutex::new(HashMap::new()) }
    }

    /// 记 `key` 的一次请求；超了时不记，返回至少还要等多久。
    pub fn take(&self, key: &str, now: Instant) -> Result<(), Duration> {
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if buckets.len() >= MAX_KEYS && !buckets.contains_key(key) {
            buckets.retain(|_, bucket| self.level(bucket, now) < self.capacity);
            if buckets.len() >= MAX_KEYS {
                buckets.clear();
            }
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

    #[test]
    fn ipv6_addresses_share_their_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
        assert_eq!(ip_key(a), ip_key(b));
        assert_ne!(ip_key(a), ip_key("2001:db8:1:3::1".parse().unwrap()));
        assert_eq!(ip_key("::ffff:10.0.0.1".parse().unwrap()), "10.0.0.1");
    }
}
