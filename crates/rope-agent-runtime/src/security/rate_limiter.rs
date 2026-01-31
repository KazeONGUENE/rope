//! Rate Limiting
//!
//! Token bucket rate limiter for API protection

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Token bucket rate limiter
pub struct RateLimiter {
    /// Requests per window
    rate: u32,

    /// Window duration
    window: Duration,

    /// Per-key buckets
    buckets: DashMap<String, TokenBucket>,
}

struct TokenBucket {
    tokens: AtomicU64,
    last_refill: parking_lot::Mutex<Instant>,
    rate: u32,
}

impl RateLimiter {
    /// Create new rate limiter
    pub fn new(rate: u32, window: Duration) -> Self {
        Self {
            rate,
            window,
            buckets: DashMap::new(),
        }
    }

    /// Check if request is allowed
    pub fn check(&self, key: &str) -> bool {
        let rate = self.rate;
        let bucket = self.buckets.entry(key.to_string()).or_insert_with(|| TokenBucket {
            tokens: AtomicU64::new(rate as u64),
            last_refill: parking_lot::Mutex::new(Instant::now()),
            rate,
        });

        // Refill tokens if needed
        let mut last_refill = bucket.last_refill.lock();
        let elapsed = last_refill.elapsed();

        if elapsed >= self.window {
            bucket.tokens.store(bucket.rate as u64, Ordering::SeqCst);
            *last_refill = Instant::now();
        }

        // Try to consume a token
        loop {
            let current = bucket.tokens.load(Ordering::SeqCst);
            if current == 0 {
                return false;
            }

            if bucket
                .tokens
                .compare_exchange(current, current - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Async acquire (waits if rate limited)
    pub async fn acquire(&self, key: &str) {
        while !self.check(key) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Get remaining tokens for a key
    pub fn remaining(&self, key: &str) -> u64 {
        self.buckets
            .get(key)
            .map(|b| b.tokens.load(Ordering::SeqCst))
            .unwrap_or(self.rate as u64)
    }
}

/// Tiered rate limiter with multiple windows
pub struct TieredRateLimiter {
    /// Per-second limit
    per_second: RateLimiter,

    /// Per-minute limit
    per_minute: RateLimiter,

    /// Per-hour limit
    per_hour: RateLimiter,
}

impl TieredRateLimiter {
    /// Create new tiered rate limiter
    pub fn new(per_second: u32, per_minute: u32, per_hour: u32) -> Self {
        Self {
            per_second: RateLimiter::new(per_second, Duration::from_secs(1)),
            per_minute: RateLimiter::new(per_minute, Duration::from_secs(60)),
            per_hour: RateLimiter::new(per_hour, Duration::from_secs(3600)),
        }
    }

    /// Check if request is allowed
    pub fn check(&self, key: &str) -> bool {
        self.per_second.check(key) && self.per_minute.check(key) && self.per_hour.check(key)
    }

    /// Async acquire
    pub async fn acquire(&self, key: &str) {
        self.per_second.acquire(key).await;
        self.per_minute.acquire(key).await;
        self.per_hour.acquire(key).await;
    }

    /// Check which tier is limiting
    pub fn limiting_tier(&self, key: &str) -> Option<&'static str> {
        if !self.per_second.check(key) {
            return Some("per_second");
        }
        if !self.per_minute.check(key) {
            return Some("per_minute");
        }
        if !self.per_hour.check(key) {
            return Some("per_hour");
        }
        None
    }
}

impl Default for TieredRateLimiter {
    fn default() -> Self {
        Self::new(10, 100, 1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter() {
        let limiter = RateLimiter::new(3, Duration::from_secs(1));

        assert!(limiter.check("user1"));
        assert!(limiter.check("user1"));
        assert!(limiter.check("user1"));
        assert!(!limiter.check("user1")); // Should be rate limited

        // Different user should work
        assert!(limiter.check("user2"));
    }

    #[test]
    fn test_remaining() {
        let limiter = RateLimiter::new(5, Duration::from_secs(1));

        assert_eq!(limiter.remaining("user1"), 5);
        limiter.check("user1");
        assert_eq!(limiter.remaining("user1"), 4);
    }

    #[test]
    fn test_tiered_limiter() {
        let limiter = TieredRateLimiter::new(2, 10, 100);

        assert!(limiter.check("user1"));
        assert!(limiter.check("user1"));
        assert!(!limiter.check("user1")); // Per-second limit hit
    }
}
