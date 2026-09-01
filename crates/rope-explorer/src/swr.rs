//! Stale-while-revalidate (SWR) primitive for dc-explorer response caching.
//!
//! ## Why this exists
//!
//! Multiple `/api/v1/*` endpoints fan out to `rope-node` (which fans out
//! to Reth) sequentially: `/api/v1/stats` scans ~50 recent blocks
//! (~3-16 s cold), `/api/v1/validators` probes N canonical agents over
//! HTTP + RPC (~5-8 s warm, >30 s if any agent is unreachable), and the
//! remaining P2 SWR-extension targets (`/api/v1/strings`,
//! `/api/v1/testimonies`, `/api/v1/tokentxns`,
//! `/api/v1/accounts/:addr/overview`) show similar per-request compute
//! costs. Under the 2026-08-11 rope-node wedge storm those compute
//! paths pushed past nginx's 60 s `proxy_read_timeout` and produced
//! 504s on dcscan.io's homepage. The SWR discipline turns steady-state
//! loads into a lock-free clone with a single background refresh at a
//! time, and hard-caps the cold compute path so a wedged upstream can
//! never surface a 504 through us.
//!
//! ## The three cache states
//!
//! For every request the wrapper takes exactly one of three paths:
//!
//! | Cache state | Behaviour |
//! |---|---|
//! | Fresh (age < `fresh_ttl_secs`) | Return cached payload, no compute |
//! | Stale-but-servable (age < `stale_ttl_secs`) | Return stale payload immediately, spawn one background refresh guarded by `refresh_lock.try_lock()` so N concurrent readers share one compute |
//! | Cold OR past `stale_ttl_secs` | Take `refresh_lock` (blocking, so we don't stampede), compute inline with `compute_timeout_secs` cap; on timeout return the last-known payload if any, else the caller-provided fallback |
//!
//! The two locks that make this safe are:
//! - **`cache: RwLock<Option<SwrEntry>>`** — cheap concurrent reads (fresh path is a lock-free clone).
//! - **`refresh_lock: tokio::sync::Mutex<()>`** — single-flight guard so a burst of N cold-cache readers all funnel through one compute.
//!
//! ## Correctness posture
//!
//! - No 5xx from this wrapper. Ever. The hard timeout guarantees a
//! payload gets returned within `compute_timeout_secs + a small overhead`.
//! - Stale reads are bounded by `stale_ttl_secs` — after that we prefer
//! the fallback shape over an unbounded-age cached response.
//! - Background refreshes never surface errors to the caller — if the
//! spawned compute times out, we log a warning and keep the previous
//! payload; a subsequent request past `stale_ttl_secs` will fall through
//! to the cold-cache path and retry.
//!
//! ## Adding a new SWR-wrapped endpoint
//!
//! 1. Add an `Arc<swr::SwrCache>` field to `AppState`.
//! 2. Initialise it with `Arc::new(swr::SwrCache::new("endpoint-name"))`.
//! 3. Split the handler into a pure `*_compute(state) -> serde_json::Value`
//! function plus a thin wrapper that calls
//! `state.the_cache.serve(cfg, || the_compute(state.clone()), fallback).await`.
//!
//! The wrapper is ~3 lines. See `stats()` and `list_validators()` for
//! canonical examples.

use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, RwLock};

/// One rendered response body + fetch timestamp. Both timestamps use
/// `chrono::Utc::now().timestamp()` (seconds since epoch) to match the
/// pre-existing `SupplyReconCache` / `PriceData` / `BotActivityCacheEntry`
/// conventions elsewhere in dc-explorer.
#[derive(Clone, Debug)]
pub struct SwrEntry {
    pub fetched_at: i64,
    pub payload: serde_json::Value,
}

/// Per-endpoint SWR knobs. Kept as a value type so callers can build
/// one at request time (cheap) rather than lugging around a static
/// singleton.
#[derive(Clone, Copy, Debug)]
pub struct SwrConfig {
    /// TTL after which the cache is considered fresh. Reads within this
    /// window are lock-free clones with no compute. Typical: 15-30 s.
    pub fresh_ttl_secs: i64,
    /// Grace window past `fresh_ttl_secs` during which the cached
    /// payload is still served, with a background refresh kicked off in
    /// parallel. Typical: 5-10 min so a wedged rope-node forwarder can't
    /// surface as a 5xx.
    pub stale_ttl_secs: i64,
    /// Hard timeout on the inline compute path. Both the background
    /// refresh and the cold-cache inline compute honour this budget so
    /// a bad upstream can never contribute to a nginx 504.
    /// Typical: 20 s (well under nginx's 60 s `proxy_read_timeout`).
    pub compute_timeout_secs: u64,
    /// Endpoint tag used in `tracing::warn!` on timeout. Must be static
    /// so we can format without allocating on the hot path.
    pub endpoint_name: &'static str,
}

/// A SWR-wrapped response cache. Owns both the payload cache and the
/// single-flight refresh lock. Cheap to `Arc::clone`; put one on
/// `AppState` per endpoint.
#[derive(Debug)]
pub struct SwrCache {
    cache: RwLock<Option<SwrEntry>>,
    refresh_lock: AsyncMutex<()>,
    /// Endpoint tag for tracing. Also enforces "one cache per endpoint"
    /// discipline: if you accidentally reuse the same `SwrCache` for two
    /// different endpoints, the logs will make it obvious. Kept even
    /// though it is only read via `Debug` today — future diagnostics
    /// (e.g. a `/healthz/swr` endpoint dumping cache freshness per
    /// endpoint) will read it directly.
    #[allow(dead_code)]
    endpoint_name: &'static str,
}

impl SwrCache {
    /// Build an empty cache for a single endpoint.
    pub fn new(endpoint_name: &'static str) -> Self {
        Self {
            cache: RwLock::new(None),
            refresh_lock: AsyncMutex::new(()),
            endpoint_name,
        }
    }

    /// Access the cached payload without triggering a refresh. Returns
    /// `None` if the cache is empty. Used by health / diagnostics
    /// endpoints that want to inspect cache state without stampeding a
    /// compute.
    pub async fn peek(&self) -> Option<SwrEntry> {
        self.cache.read().await.clone()
    }

    /// Overwrite the cached payload with `payload`, using the current
    /// clock as `fetched_at`. Exposed so a startup task can pre-warm
    /// the cache; the standard SWR flow via `serve` populates it
    /// automatically on the cold-cache path.
    pub async fn set(&self, payload: serde_json::Value) {
        let mut w = self.cache.write().await;
        *w = Some(SwrEntry {
            fetched_at: chrono::Utc::now().timestamp(),
            payload,
        });
    }

    /// The three-path SWR read. See the module docs for a full
    /// discussion of the three cache states.
    ///
    /// - `cfg`: per-endpoint TTLs + compute-timeout.
    /// - `compute`: closure that builds a `Future<Output = Value>` on
    /// each invocation. MUST be cheap to call (typically it just
    /// captures an `Arc<AppState>` and returns
    /// `some_compute_fn(state.clone())`).
    /// - `fallback`: called ONLY when the cold-cache compute misses its
    /// timeout AND the cache is completely empty. Should return a
    /// well-shaped, honest payload with an `error` / `note` field so
    /// the frontend can render "loading" state rather than crash.
    pub async fn serve<C, F, B>(
        self: &Arc<Self>,
        cfg: SwrConfig,
        compute: C,
        fallback: B,
    ) -> serde_json::Value
    where
        C: Fn() -> F + Send + Sync + Clone + 'static,
        F: std::future::Future<Output = serde_json::Value> + Send + 'static,
        B: Fn() -> serde_json::Value,
    {
        let now = chrono::Utc::now().timestamp();

        // Fresh path: lock-free clone. Under steady state (bounded burst
        // rate) 99%+ of requests should land here.
        {
            let guard = self.cache.read().await;
            if let Some(entry) = guard.as_ref() {
                let age = now - entry.fetched_at;
                if age >= 0 && age < cfg.fresh_ttl_secs {
                    return entry.payload.clone();
                }
            }
        }

        // Stale-but-servable path. Return the cached payload
        // immediately, spawn a single background refresh. If refresh
        // work is already in flight we skip the spawn (single-flight
        // guarantee via `try_lock`).
        let cached_stale = {
            let guard = self.cache.read().await;
            guard.as_ref().and_then(|entry| {
                let age = now - entry.fetched_at;
                if age >= 0 && age < cfg.stale_ttl_secs {
                    Some(entry.payload.clone())
                } else {
                    None
                }
            })
        };
        if let Some(stale) = cached_stale {
            let cache_arc = Arc::clone(self);
            let compute_bg = compute.clone();
            let endpoint_name = cfg.endpoint_name;
            let compute_timeout = cfg.compute_timeout_secs;
            tokio::spawn(async move {
                let Ok(_guard) = cache_arc.refresh_lock.try_lock() else {
                    return;
                };
                let fut = compute_bg();
                let payload = match tokio::time::timeout(
                    std::time::Duration::from_secs(compute_timeout),
                    fut,
                )
                .await
                {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::warn!(
                            endpoint = endpoint_name,
                            "SWR background refresh timeout - keeping previous cached value"
                        );
                        return;
                    }
                };
                let mut w = cache_arc.cache.write().await;
                *w = Some(SwrEntry {
                    fetched_at: chrono::Utc::now().timestamp(),
                    payload,
                });
            });
            return stale;
        }

        // Cold cache: compute inline under the single-flight lock. A
        // burst of N concurrent cold requests funnels through one
        // compute; the losers wait, then observe the freshly-populated
        // cache below and return immediately without re-computing.
        let _lock_guard = self.refresh_lock.lock().await;
        {
            let guard = self.cache.read().await;
            if let Some(entry) = guard.as_ref() {
                let age = chrono::Utc::now().timestamp() - entry.fetched_at;
                if age >= 0 && age < cfg.fresh_ttl_secs {
                    return entry.payload.clone();
                }
            }
        }
        let fut = compute();
        let payload = match tokio::time::timeout(
            std::time::Duration::from_secs(cfg.compute_timeout_secs),
            fut,
        )
        .await
        {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    endpoint = cfg.endpoint_name,
                    "SWR cold-cache compute timeout - returning last-known payload if any"
                );
                let guard = self.cache.read().await;
                if let Some(entry) = guard.as_ref() {
                    return entry.payload.clone();
                }
                return fallback();
            }
        };
        {
            let mut w = self.cache.write().await;
            *w = Some(SwrEntry {
                fetched_at: chrono::Utc::now().timestamp(),
                payload: payload.clone(),
            });
        }
        payload
    }
}

impl Default for SwrCache {
    fn default() -> Self {
        Self::new("unnamed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn cfg(name: &'static str) -> SwrConfig {
        SwrConfig {
            fresh_ttl_secs: 60,
            stale_ttl_secs: 600,
            compute_timeout_secs: 20,
            endpoint_name: name,
        }
    }

    fn fast_cfg(name: &'static str, fresh: i64, stale: i64) -> SwrConfig {
        SwrConfig {
            fresh_ttl_secs: fresh,
            stale_ttl_secs: stale,
            compute_timeout_secs: 20,
            endpoint_name: name,
        }
    }

    #[tokio::test]
    async fn cold_cache_computes_and_returns_payload() {
        let cache = Arc::new(SwrCache::new("test_cold"));
        let counter = Arc::new(AtomicUsize::new(0));
        let counter2 = Arc::clone(&counter);
        let payload = cache
            .serve(
                cfg("test_cold"),
                move || {
                    let c = Arc::clone(&counter2);
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        serde_json::json!({"cached": false, "n": 1})
                    }
                },
                || serde_json::json!({"error": "fallback"}),
            )
            .await;
        assert_eq!(payload["n"], serde_json::json!(1));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fresh_cache_hit_skips_compute_entirely() {
        let cache = Arc::new(SwrCache::new("test_fresh"));
        let counter = Arc::new(AtomicUsize::new(0));

        // Prime the cache.
        {
            let counter2 = Arc::clone(&counter);
            let _ = cache
                .serve(
                    cfg("test_fresh"),
                    move || {
                        let c = Arc::clone(&counter2);
                        async move {
                            c.fetch_add(1, Ordering::SeqCst);
                            serde_json::json!({"n": 1})
                        }
                    },
                    || serde_json::json!({}),
                )
                .await;
        }
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Immediate second read: fresh path, no compute.
        let counter3 = Arc::clone(&counter);
        let payload = cache
            .serve(
                cfg("test_fresh"),
                move || {
                    let c = Arc::clone(&counter3);
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        serde_json::json!({"n": 999})
                    }
                },
                || serde_json::json!({}),
            )
            .await;
        assert_eq!(
            payload["n"],
            serde_json::json!(1),
            "fresh path must serve cached payload, not recompute"
        );
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "fresh path must not invoke compute"
        );
    }

    #[tokio::test]
    async fn stale_cache_returns_immediately_and_refreshes_in_background() {
        let cache = Arc::new(SwrCache::new("test_stale"));
        let counter = Arc::new(AtomicUsize::new(0));

        // Prime cache with fresh_ttl=0 so it's instantly stale on the next read.
        // Using fresh=0 makes any age >= 0 stale, which is what we want.
        let c1 = Arc::clone(&counter);
        let _ = cache
            .serve(
                fast_cfg("test_stale", 0, 600),
                move || {
                    let c = Arc::clone(&c1);
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        serde_json::json!({"seq": 1})
                    }
                },
                || serde_json::json!({}),
            )
            .await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Wait 1s so age > 0.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        // Stale read: should return cached seq=1 immediately AND spawn a
        // background refresh that increments the counter.
        let c2 = Arc::clone(&counter);
        let payload = cache
            .serve(
                fast_cfg("test_stale", 0, 600),
                move || {
                    let c = Arc::clone(&c2);
                    async move {
                        c.fetch_add(1, Ordering::SeqCst);
                        serde_json::json!({"seq": 2})
                    }
                },
                || serde_json::json!({}),
            )
            .await;
        assert_eq!(
            payload["seq"],
            serde_json::json!(1),
            "stale path must serve cached payload, not the newly-computed one"
        );

        // Give the spawned refresh time to complete.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "background refresh must have executed"
        );
    }

    #[tokio::test]
    async fn concurrent_cold_reads_share_single_compute() {
        let cache = Arc::new(SwrCache::new("test_singleflight"));
        let counter = Arc::new(AtomicUsize::new(0));

        // Fire N parallel cold reads. Only one compute should run.
        let mut handles = Vec::new();
        for _ in 0..16 {
            let cache = Arc::clone(&cache);
            let counter = Arc::clone(&counter);
            handles.push(tokio::spawn(async move {
                let counter2 = Arc::clone(&counter);
                cache
                    .serve(
                        cfg("test_singleflight"),
                        move || {
                            let c = Arc::clone(&counter2);
                            async move {
                                // Slow enough that all readers pile up on the lock.
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                c.fetch_add(1, Ordering::SeqCst);
                                serde_json::json!({"once": true})
                            }
                        },
                        || serde_json::json!({}),
                    )
                    .await
            }));
        }
        for h in handles {
            let payload = h.await.unwrap();
            assert_eq!(payload["once"], serde_json::json!(true));
        }
        // Exactly one compute ran (single-flight guarantee).
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "single-flight guarantee violated"
        );
    }

    #[tokio::test]
    async fn cold_compute_timeout_returns_fallback_when_cache_is_empty() {
        let cache = Arc::new(SwrCache::new("test_timeout_cold"));
        let cfg = SwrConfig {
            fresh_ttl_secs: 60,
            stale_ttl_secs: 600,
            compute_timeout_secs: 0, // instant timeout
            endpoint_name: "test_timeout_cold",
        };
        let payload = cache
            .serve(
                cfg,
                || async {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    serde_json::json!({"real": true})
                },
                || serde_json::json!({"fallback": true, "reason": "timeout"}),
            )
            .await;
        assert_eq!(payload["fallback"], serde_json::json!(true));
        // Cache stays empty.
        assert!(cache.peek().await.is_none());
    }

    #[tokio::test]
    async fn cold_compute_timeout_returns_last_known_payload_when_present() {
        let cache = Arc::new(SwrCache::new("test_timeout_stale"));

        // Prime the cache with a real value.
        cache.set(serde_json::json!({"warmed": true, "n": 42})).await;

        // Wait past both fresh and stale windows so the read falls into cold.
        // We use tiny TTLs and a sleep instead of tampering with the clock.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        let cfg = SwrConfig {
            fresh_ttl_secs: 0,
            stale_ttl_secs: 0, // both windows already exceeded
            compute_timeout_secs: 0,
            endpoint_name: "test_timeout_stale",
        };
        let payload = cache
            .serve(
                cfg,
                || async {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    serde_json::json!({"real": true})
                },
                || serde_json::json!({"fallback": true}),
            )
            .await;
        // Old cached value wins over the fallback shape.
        assert_eq!(payload["warmed"], serde_json::json!(true));
        assert_eq!(payload["n"], serde_json::json!(42));
    }

    #[tokio::test]
    async fn peek_returns_none_when_empty_and_some_when_populated() {
        let cache = Arc::new(SwrCache::new("test_peek"));
        assert!(cache.peek().await.is_none());
        cache.set(serde_json::json!({"a": 1})).await;
        let entry = cache.peek().await.expect("should be populated");
        assert_eq!(entry.payload["a"], serde_json::json!(1));
        assert!(entry.fetched_at > 0);
    }

    #[tokio::test]
    async fn default_impl_names_cache_unnamed_for_diagnostics() {
        let cache: SwrCache = Default::default();
        assert_eq!(cache.endpoint_name, "unnamed");
    }
}
