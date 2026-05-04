//! Price feed fetcher.
//!
//! Pulls the canonical Datachain Rope price feed published by the DCSwap
//! indexer at `https://dcswap.net/v1/prices` (see workspace rule
//! `handover-canonical-fat-price-2026-03-14`). The response shape is documented
//! in that handover; the snapshot we extract is a typed [`PriceSnapshot`].
//!
//! ## Resilience
//!
//! [`PriceFeed::fetch_with_retry`] applies an exponential backoff retry policy
//! around `fetch_once`. The agent loop in [`crate::OracleAgent`] then chooses
//! whether to skip the cycle or to anchor a "feed unavailable" testimony — the
//! decision is **not** made here; the feed module just reports the result.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::AgentConfig;

/// One token entry inside the canonical `/v1/prices` response.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct TokenPrice {
    pub usd: f64,
    #[serde(default)]
    pub change_24h: f64,
    #[serde(default)]
    pub source: String,
}

/// One contributing source inside `priceMechanism.sources`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PriceSource {
    pub source: String,
    pub price: f64,
    pub weight: f64,
}

/// The `priceMechanism` section of the canonical feed response.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PriceMechanism {
    pub version: String,
    pub phase: String,
    pub price: f64,
    #[serde(default)]
    pub p_ref: f64,
    #[serde(default)]
    pub p_floor: f64,
    #[serde(default)]
    pub sources: Vec<PriceSource>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Top-level data section.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PriceData {
    #[serde(rename = "FAT", default)]
    pub fat: TokenPrice,
    #[serde(rename = "USDC", default)]
    pub usdc: TokenPrice,
    #[serde(rename = "USDT", default)]
    pub usdt: TokenPrice,
    #[serde(rename = "EUROD", default)]
    pub eurod: TokenPrice,
}

/// Full canonical response envelope.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PriceFeedResponse {
    pub success: bool,
    pub data: PriceData,
    pub timestamp: i64,
    #[serde(rename = "priceMechanism", default)]
    pub price_mechanism: PriceMechanism,
}

/// A typed snapshot the agent uses to build a [`crate::OraclePriceTestimony`].
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct PriceSnapshot {
    pub fat: TokenPrice,
    pub usdc: TokenPrice,
    pub usdt: TokenPrice,
    pub eurod: TokenPrice,
    pub mechanism: PriceMechanism,
    /// Server-side feed timestamp (epoch seconds).
    pub feed_timestamp: i64,
    /// Local timestamp at which the agent received the response.
    pub fetched_at: i64,
    /// URL the snapshot was sourced from.
    pub source_url: String,
}

impl PriceSnapshot {
    /// Build a snapshot from a parsed feed response and the URL it came from.
    pub fn from_response(resp: PriceFeedResponse, source_url: &str) -> Self {
        Self {
            fat: resp.data.fat,
            usdc: resp.data.usdc,
            usdt: resp.data.usdt,
            eurod: resp.data.eurod,
            mechanism: resp.price_mechanism,
            feed_timestamp: resp.timestamp,
            fetched_at: chrono::Utc::now().timestamp(),
            source_url: source_url.to_string(),
        }
    }

    /// Lightweight sanity check: the feed must have returned a positive FAT
    /// price; otherwise the snapshot is unusable for an oracle testimony.
    pub fn is_usable(&self) -> bool {
        self.fat.usd.is_finite() && self.fat.usd > 0.0
    }
}

/// Errors emitted by the feed fetcher.
#[derive(Debug, thiserror::Error)]
pub enum FeedError {
    #[error("HTTP error fetching {url}: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("non-success HTTP status {status} from {url}: {body}")]
    Status {
        url: String,
        status: u16,
        body: String,
    },
    #[error("malformed feed response from {url}: {source}")]
    Decode {
        url: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("feed reported success=false at {url}")]
    Unhealthy { url: String },
    #[error("feed snapshot rejected as unusable at {url}: {reason}")]
    Unusable { url: String, reason: String },
}

/// HTTP price feed client.
pub struct PriceFeed {
    http: reqwest::Client,
    feed_url: String,
    user_agent: String,
    max_retries: u32,
    backoff_initial: Duration,
    backoff_max: Duration,
}

impl PriceFeed {
    /// Build a new client from an [`AgentConfig`]. The underlying
    /// [`reqwest::Client`] inherits the configured request timeout and User-
    /// Agent.
    pub fn from_config(cfg: &AgentConfig) -> Result<Self, FeedError> {
        let http = reqwest::Client::builder()
            .timeout(cfg.feed_timeout)
            .user_agent(cfg.user_agent.clone())
            .build()
            .map_err(|e| FeedError::Http {
                url: cfg.feed_url.clone(),
                source: e,
            })?;
        Ok(Self {
            http,
            feed_url: cfg.feed_url.clone(),
            user_agent: cfg.user_agent.clone(),
            max_retries: cfg.max_retries,
            backoff_initial: cfg.backoff_initial,
            backoff_max: cfg.backoff_max,
        })
    }

    /// Build a client with an explicit URL — used in unit tests against a
    /// mock server. The retry policy is conservative (single attempt) so the
    /// tests don't have to wait for the backoff to expire.
    pub fn for_url(url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client must build with default settings"),
            feed_url: url.into(),
            user_agent: crate::config::DEFAULT_USER_AGENT.to_string(),
            max_retries: 0,
            backoff_initial: Duration::from_millis(50),
            backoff_max: Duration::from_millis(100),
        }
    }

    /// Read-only view of the feed URL the client is bound to.
    pub fn feed_url(&self) -> &str {
        &self.feed_url
    }

    /// Read-only view of the User-Agent string the client sends.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// Single HTTP fetch — no retry, no validation beyond JSON decoding.
    pub async fn fetch_once(&self) -> Result<PriceSnapshot, FeedError> {
        let resp = self
            .http
            .get(&self.feed_url)
            .send()
            .await
            .map_err(|source| FeedError::Http {
                url: self.feed_url.clone(),
                source,
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(FeedError::Status {
                url: self.feed_url.clone(),
                status: status.as_u16(),
                body: truncate(&body, 256),
            });
        }
        let bytes = resp.bytes().await.map_err(|source| FeedError::Http {
            url: self.feed_url.clone(),
            source,
        })?;
        let parsed: PriceFeedResponse =
            serde_json::from_slice(&bytes).map_err(|source| FeedError::Decode {
                url: self.feed_url.clone(),
                source,
            })?;
        if !parsed.success {
            return Err(FeedError::Unhealthy {
                url: self.feed_url.clone(),
            });
        }
        let snap = PriceSnapshot::from_response(parsed, &self.feed_url);
        if !snap.is_usable() {
            return Err(FeedError::Unusable {
                url: self.feed_url.clone(),
                reason: format!("FAT price not positive: {}", snap.fat.usd),
            });
        }
        Ok(snap)
    }

    /// Fetch with exponential backoff. Logs every failed attempt at WARN.
    /// Returns the last error if every retry is exhausted.
    pub async fn fetch_with_retry(&self) -> Result<PriceSnapshot, FeedError> {
        let mut attempt: u32 = 0;
        let mut delay = self.backoff_initial;
        loop {
            match self.fetch_once().await {
                Ok(snap) => {
                    if attempt > 0 {
                        tracing::info!(
                            target: "oracle_agent::feeds",
                            url = self.feed_url.as_str(),
                            attempt,
                            "feed fetched after {} retr{}",
                            attempt,
                            if attempt == 1 { "y" } else { "ies" }
                        );
                    } else {
                        tracing::debug!(
                            target: "oracle_agent::feeds",
                            url = self.feed_url.as_str(),
                            "feed fetched"
                        );
                    }
                    return Ok(snap);
                }
                Err(e) if attempt < self.max_retries => {
                    tracing::warn!(
                        target: "oracle_agent::feeds",
                        url = self.feed_url.as_str(),
                        attempt,
                        retry_in_ms = delay.as_millis() as u64,
                        error = %e,
                        "feed fetch failed; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    delay = (delay.saturating_mul(2)).min(self.backoff_max);
                }
                Err(e) => {
                    tracing::error!(
                        target: "oracle_agent::feeds",
                        url = self.feed_url.as_str(),
                        attempts = attempt + 1,
                        error = %e,
                        "feed fetch exhausted retries"
                    );
                    return Err(e);
                }
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn canonical_body() -> serde_json::Value {
        serde_json::json!({
            "success": true,
            "data": {
                "USDC": { "usd": 0.999967, "change_24h": 0.006, "source": "coingecko" },
                "USDT": { "usd": 1.0, "change_24h": -0.002, "source": "coingecko" },
                "EUROD": { "usd": 1.1447, "change_24h": 0, "source": "exchangerate-api" },
                "FAT": {
                    "usd": 0.007408,
                    "change_24h": -3.199,
                    "source": "reconciled(dcswap-reserves+geckoterminal-xdc)"
                }
            },
            "timestamp": 1773507597_i64,
            "priceMechanism": {
                "version": "2.1",
                "phase": "market",
                "price": 0.007408,
                "p_ref": 0.0025,
                "p_floor": 0.0,
                "sources": [
                    { "source": "dcswap-reserves", "price": 0.010297, "weight": 0.7 },
                    { "source": "geckoterminal-xdc", "price": 0.000667, "weight": 0.3 }
                ],
                "note": "price = VWAP(sources). Pure market price — no artificial floor."
            }
        })
    }

    #[tokio::test]
    async fn fetch_once_parses_canonical_payload() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/prices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(canonical_body()))
            .mount(&server)
            .await;

        let feed = PriceFeed::for_url(format!("{}/v1/prices", server.uri()));
        let snap = feed.fetch_once().await.expect("fetch must succeed");

        assert!(snap.is_usable());
        assert!((snap.fat.usd - 0.007408).abs() < 1e-9);
        assert_eq!(snap.mechanism.version, "2.1");
        assert_eq!(snap.mechanism.phase, "market");
        assert_eq!(snap.mechanism.sources.len(), 2);
        assert!((snap.usdc.usd - 0.999967).abs() < 1e-9);
        assert_eq!(snap.feed_timestamp, 1_773_507_597);
        assert!(snap.source_url.ends_with("/v1/prices"));
    }

    #[tokio::test]
    async fn fetch_once_rejects_non_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/prices"))
            .respond_with(ResponseTemplate::new(503).set_body_string("Service unavailable"))
            .mount(&server)
            .await;

        let feed = PriceFeed::for_url(format!("{}/v1/prices", server.uri()));
        let err = feed.fetch_once().await.expect_err("503 must error");
        assert!(matches!(err, FeedError::Status { status: 503, .. }));
    }

    #[tokio::test]
    async fn fetch_once_rejects_malformed_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/prices"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let feed = PriceFeed::for_url(format!("{}/v1/prices", server.uri()));
        let err = feed
            .fetch_once()
            .await
            .expect_err("non-JSON body must error");
        assert!(matches!(err, FeedError::Decode { .. }));
    }

    #[tokio::test]
    async fn fetch_once_rejects_success_false() {
        let server = MockServer::start().await;
        let mut body = canonical_body();
        body["success"] = serde_json::Value::Bool(false);
        Mock::given(method("GET"))
            .and(path("/v1/prices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let feed = PriceFeed::for_url(format!("{}/v1/prices", server.uri()));
        let err = feed
            .fetch_once()
            .await
            .expect_err("success=false must error");
        assert!(matches!(err, FeedError::Unhealthy { .. }));
    }

    #[tokio::test]
    async fn fetch_once_rejects_zero_fat_price() {
        let server = MockServer::start().await;
        let mut body = canonical_body();
        body["data"]["FAT"]["usd"] = serde_json::Value::from(0.0_f64);
        Mock::given(method("GET"))
            .and(path("/v1/prices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let feed = PriceFeed::for_url(format!("{}/v1/prices", server.uri()));
        let err = feed
            .fetch_once()
            .await
            .expect_err("zero FAT price must be rejected as unusable");
        assert!(matches!(err, FeedError::Unusable { .. }));
    }

    #[tokio::test]
    async fn fetch_with_retry_recovers_after_transient_failure() {
        let server = MockServer::start().await;
        // First the server will 500, then 200. Wiremock matches in priority
        // order; we register the failure with a low expected hit count.
        Mock::given(method("GET"))
            .and(path("/v1/prices"))
            .respond_with(ResponseTemplate::new(500).set_body_string("oops"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/prices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(canonical_body()))
            .with_priority(2)
            .mount(&server)
            .await;

        let mut feed = PriceFeed::for_url(format!("{}/v1/prices", server.uri()));
        feed.max_retries = 3;

        let snap = feed.fetch_with_retry().await.expect("retry must succeed");
        assert!(snap.is_usable());
    }

    #[tokio::test]
    async fn fetch_with_retry_gives_up_after_max_retries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/prices"))
            .respond_with(ResponseTemplate::new(500).set_body_string("permanent failure"))
            .mount(&server)
            .await;

        let mut feed = PriceFeed::for_url(format!("{}/v1/prices", server.uri()));
        feed.max_retries = 2;

        let err = feed
            .fetch_with_retry()
            .await
            .expect_err("permanent failure must surface");
        assert!(matches!(err, FeedError::Status { status: 500, .. }));
    }
}
