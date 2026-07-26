//! Per-client rate limiting for the public `dc-explorer` (dcscan.io) HTTP
//! API.
//!
//! Added 2026-07-25 (finding H4 of
//! `docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md`). Before this
//! module, `rope-explorer` had NO rate limiting anywhere in its request
//! path — every one of its ~100 public `/api/v1/*` routes (including
//! expensive ones backed by Postgres queries, upstream HTTP calls, or
//! full-manifest JSON serialization) could be hammered by a single
//! client with no server-side throttling at all, only whatever nginx
//! happens to enforce in front of it (which, per the audit, is
//! inconsistent across vhosts — see finding H8).
//!
//! # Trust model (mirrors `rope_auth::effective_client_ip` in `rope-node`)
//!
//! In production every public request arrives via nginx, which
//! terminates the internet-facing connection and opens its own loopback
//! connection to this process. So the raw TCP peer address
//! (`ConnectInfo<SocketAddr>`) is nginx's own address for nearly all
//! traffic — keying a limiter on that alone would collapse every
//! distinct internet client into one shared bucket. Instead:
//!
//! - If the TCP peer is loopback AND `X-Forwarded-For`/`X-Real-IP` is
//!   present, trust the first hop of that header as the real client IP.
//!   An internet-side attacker cannot forge this branch: to reach it
//!   they would have to BE the loopback peer, which they cannot be
//!   without already having a foothold on the box.
//! - Otherwise, key on the raw peer address. A direct (non-proxied)
//!   caller who forges the header gains nothing — their real source IP
//!   is used instead.
//!
//! # Bounding memory
//!
//! The bucket map is capped (see [`MAX_BUCKETS`]) with lazy eviction of
//! stale entries, so an attacker rotating through many claimed source
//! IPs cannot grow the map without bound (finding M1).

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Requests allowed per second, sustained, per effective client IP.
/// Overridable via `DCSCAN_RATE_LIMIT_RPS` for operator tuning (e.g. to
/// raise the budget for a known, high-traffic integration partner IP
/// range at the nginx layer instead — this is a blunt, uniform limit by
/// design, not a per-route policy engine).
const DEFAULT_REQUESTS_PER_SECOND: u32 = 20;
/// Additional burst allowance on top of the sustained rate, so a normal
/// page load (which fires several concurrent `/api/v1/*` calls) doesn't
/// trip the limiter. Overridable via `DCSCAN_RATE_LIMIT_BURST`.
const DEFAULT_BURST: u32 = 60;
/// Hard cap on distinct buckets kept in memory (see module doc, "Bounding
/// memory").
const MAX_BUCKETS: usize = 50_000;

struct Counter {
    count: u32,
    window_start: i64,
}

/// Shared, in-process fixed-window rate limiter. One instance lives on
/// `AppState` for the lifetime of the process.
pub struct RateLimiter {
    requests_per_second: u32,
    burst: u32,
    buckets: RwLock<HashMap<String, Counter>>,
}

impl RateLimiter {
    /// Build the limiter from environment overrides (falling back to the
    /// module defaults), logging the effective configuration once at
    /// boot so operators can confirm what's active without reading
    /// source.
    pub fn from_env() -> Self {
        let requests_per_second = std::env::var("DCSCAN_RATE_LIMIT_RPS")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(DEFAULT_REQUESTS_PER_SECOND);
        let burst = std::env::var("DCSCAN_RATE_LIMIT_BURST")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(DEFAULT_BURST);
        tracing::info!(
            requests_per_second,
            burst,
            "dc-explorer rate limiter initialised (finding H4, SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md)"
        );
        Self {
            requests_per_second,
            burst,
            buckets: RwLock::new(HashMap::new()),
        }
    }

    async fn check(&self, key: &str) -> bool {
        let now = chrono::Utc::now().timestamp();
        let mut buckets = self.buckets.write().await;

        if buckets.len() >= MAX_BUCKETS && !buckets.contains_key(key) {
            buckets.retain(|_, c| now - c.window_start < 2);
        }

        let counter = buckets.entry(key.to_string()).or_insert(Counter {
            count: 0,
            window_start: now,
        });

        if now - counter.window_start >= 1 {
            counter.count = 0;
            counter.window_start = now;
        }

        if counter.count >= self.requests_per_second + self.burst {
            return false;
        }

        counter.count += 1;
        true
    }
}

/// Case-insensitive `Header-Name: value` lookup, first match wins.
fn first_header_value(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
}

/// Resolve the effective client IP for a request, per the trust model
/// documented at the top of this module.
fn effective_client_ip(peer_ip: &str, peer_is_loopback: bool, headers: &axum::http::HeaderMap) -> String {
    if !peer_is_loopback {
        return peer_ip.to_string();
    }
    if let Some(v) = first_header_value(headers, "x-forwarded-for") {
        if let Some(first) = v.split(',').next().map(|s| s.trim()) {
            if !first.is_empty() {
                return first.to_string();
            }
        }
    }
    if let Some(v) = first_header_value(headers, "x-real-ip") {
        if !v.is_empty() {
            return v;
        }
    }
    peer_ip.to_string()
}

/// Axum middleware entry point. Wired in `main.rs` as the outermost
/// `.layer(axum::middleware::from_fn_with_state(...))` on the router, so
/// it runs before every route handler including static-file serving.
pub async fn rate_limit_middleware(
    State(state): State<Arc<crate::AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let peer_addr = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0);
    let Some(peer_addr) = peer_addr else {
        // No connect-info extension present (shouldn't happen given
        // `into_make_service_with_connect_info` in main.rs, but fail
        // open rather than 500 every request if the server is ever
        // wired differently in a future refactor or in a test harness).
        return next.run(request).await;
    };
    let peer_ip = peer_addr.ip().to_string();
    let peer_is_loopback = peer_addr.ip().is_loopback();
    let key = effective_client_ip(&peer_ip, peer_is_loopback, request.headers());

    if !state.rate_limiter.check(&key).await {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", "1")],
            Json(serde_json::json!({
                "success": false,
                "error": "rate_limited",
                "message": "Too many requests — please slow down and retry shortly."
            })),
        )
            .into_response();
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    fn headers_with(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn trusts_xff_only_from_loopback_peer() {
        let h = headers_with(&[("x-forwarded-for", "203.0.113.7, 10.0.0.1")]);
        assert_eq!(effective_client_ip("127.0.0.1", true, &h), "203.0.113.7");

        // Non-loopback peer: never trust client-supplied XFF.
        let h2 = headers_with(&[("x-forwarded-for", "1.2.3.4")]);
        assert_eq!(effective_client_ip("198.51.100.9", false, &h2), "198.51.100.9");
    }

    #[test]
    fn falls_back_to_peer_when_no_proxy_headers() {
        let h = HeaderMap::new();
        assert_eq!(effective_client_ip("127.0.0.1", true, &h), "127.0.0.1");
    }

    #[test]
    fn prefers_xff_over_x_real_ip() {
        let h = headers_with(&[("x-real-ip", "9.9.9.9"), ("x-forwarded-for", "203.0.113.7")]);
        assert_eq!(effective_client_ip("127.0.0.1", true, &h), "203.0.113.7");
    }

    #[tokio::test]
    async fn limiter_allows_burst_then_blocks() {
        let limiter = RateLimiter {
            requests_per_second: 2,
            burst: 1,
            buckets: RwLock::new(HashMap::new()),
        };
        assert!(limiter.check("1.2.3.4").await);
        assert!(limiter.check("1.2.3.4").await);
        assert!(limiter.check("1.2.3.4").await);
        assert!(!limiter.check("1.2.3.4").await);
        // A different key has its own independent budget.
        assert!(limiter.check("5.6.7.8").await);
    }

    #[tokio::test]
    async fn limiter_from_env_uses_defaults_when_unset() {
        std::env::remove_var("DCSCAN_RATE_LIMIT_RPS");
        std::env::remove_var("DCSCAN_RATE_LIMIT_BURST");
        let limiter = RateLimiter::from_env();
        assert_eq!(limiter.requests_per_second, DEFAULT_REQUESTS_PER_SECOND);
        assert_eq!(limiter.burst, DEFAULT_BURST);
    }
}
