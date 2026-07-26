//! Fixed-window in-memory rate limiter for credential endpoints.
//!
//! Two dimensions on `/v1/auth/login`: per-IP and per-email, so neither
//! a single source spraying many accounts nor a distributed attack on
//! one account slips through. nginx adds a coarse `limit_req` in front;
//! this is the precise, per-identity layer.

use std::collections::HashMap;

use parking_lot::Mutex;

struct Window {
    start: i64,
    count: u32,
}

pub struct RateLimiter {
    window_secs: i64,
    max_per_window: u32,
    entries: Mutex<HashMap<String, Window>>,
}

impl RateLimiter {
    pub fn new(window_secs: i64, max_per_window: u32) -> Self {
        Self {
            window_secs,
            max_per_window,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Record one attempt for `key`; returns `false` when the caller
    /// exceeded the window budget.
    pub fn allow(&self, key: &str, now: i64) -> bool {
        let mut map = self.entries.lock();
        // Opportunistic cleanup so the map cannot grow unboundedly.
        if map.len() > 10_000 {
            let cutoff = now - self.window_secs;
            map.retain(|_, w| w.start >= cutoff);
        }
        let entry = map.entry(key.to_string()).or_insert(Window {
            start: now,
            count: 0,
        });
        if now - entry.start >= self.window_secs {
            entry.start = now;
            entry.count = 0;
        }
        entry.count += 1;
        entry.count <= self.max_per_window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_budget_then_blocks() {
        let rl = RateLimiter::new(300, 3);
        let now = 1_800_000_000;
        assert!(rl.allow("ip:1.2.3.4", now));
        assert!(rl.allow("ip:1.2.3.4", now + 1));
        assert!(rl.allow("ip:1.2.3.4", now + 2));
        assert!(!rl.allow("ip:1.2.3.4", now + 3));
    }

    #[test]
    fn window_resets() {
        let rl = RateLimiter::new(300, 1);
        let now = 1_800_000_000;
        assert!(rl.allow("k", now));
        assert!(!rl.allow("k", now + 10));
        assert!(rl.allow("k", now + 301));
    }

    #[test]
    fn keys_are_independent() {
        let rl = RateLimiter::new(300, 1);
        let now = 1_800_000_000;
        assert!(rl.allow("a", now));
        assert!(rl.allow("b", now));
    }
}
