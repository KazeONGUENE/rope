//! JSON-RPC client for anchoring testimonies on a local rope-node.
//!
//! Wraps two methods exposed by `crates/rope-node/src/rpc_server.rs`:
//!
//! * `rope_createPersonalLedger(wallet)` — idempotently registers the
//!   OracleAgent's wallet string. Used once at startup.
//! * `rope_appendToLedger(wallet, interaction)` — anchors one testimony as a
//!   knot on the wallet's string. Returns `{ index, hash }` where `hash` is
//!   the canonical 32-byte StringId of the new knot (per the canon §6 stable-
//!   identifier guarantee documented above the RPC handler).
//!
//! The `interaction` shape accepted by the node is:
//!
//! ```json
//! { "interaction_type": "TestimonySubmission",
//!   "description": "<short human-readable label>",
//!   "metadata": { "key": "value", ... } }
//! ```
//!
//! All metadata values are coerced to strings on the node side, so this
//! module already serialises them as strings.
//!
//! ## Resilience
//!
//! [`AnchorClient::append_with_retry`] wraps `append_once` in an exponential
//! backoff loop. The agent loop in [`crate::OracleAgent`] uses the retrying
//! variant; standalone tests use `append_once` so they don't have to wait for
//! the backoff.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::AgentConfig;

/// Response of `rope_appendToLedger` decoded into a typed struct.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AppendResult {
    /// The piece count of the encrypted envelope (sliced for distribution).
    /// The node returns this as `index` for backward-compat with older
    /// fragment-based clients.
    pub piece_count: u32,
    /// The canonical StringId of the new knot, hex-encoded with no `0x`
    /// prefix. This is the `knot_string_id` referenced by `rope_untieKnot`
    /// and DCScan's per-knot views.
    pub knot_string_id: String,
}

/// Errors emitted by [`AnchorClient`].
#[derive(Debug, thiserror::Error)]
pub enum AnchorError {
    #[error("transport error talking to {url}: {source}")]
    Transport {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("non-success HTTP {status} from {url}: {body}")]
    Status {
        url: String,
        status: u16,
        body: String,
    },
    #[error("malformed JSON-RPC response from {url}: {source}: {body}")]
    Decode {
        url: String,
        body: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("JSON-RPC error from {url} (code={code}): {message}")]
    Rpc {
        url: String,
        code: i64,
        message: String,
    },
    #[error("JSON-RPC response missing required field {field}")]
    MissingField { field: &'static str },
}

impl AnchorError {
    /// Whether this error is potentially recoverable (worth retrying). Code
    /// 2002 ("No ledger") and 2001 ("Already exists") are not transient and
    /// must be handled by the caller. Network errors and 5xx are retried.
    pub fn is_transient(&self) -> bool {
        match self {
            AnchorError::Transport { .. } => true,
            AnchorError::Status { status, .. } => *status >= 500,
            AnchorError::Decode { .. } => false,
            AnchorError::Rpc { code, .. } => {
                // -32603 internal error, 5xx-ish RPC codes — retry. Domain
                // codes (2001 already exists, 2002 missing ledger,
                // 2003 deleted, -32602 invalid params) are NOT transient.
                matches!(*code, -32603 | -32099..=-32000)
            }
            AnchorError::MissingField { .. } => false,
        }
    }
}

/// Minimal JSON-RPC client tailored to the rope-node `rope_*` methods used
/// by the OracleAgent. Holds a [`reqwest::Client`] internally so connections
/// can be reused across cycles.
pub struct AnchorClient {
    http: reqwest::Client,
    rpc_url: String,
    max_retries: u32,
    backoff_initial: Duration,
    backoff_max: Duration,
    /// Monotonic id counter for JSON-RPC requests. Per the JSON-RPC 2.0 spec
    /// the id is opaque; the rope-node echoes it back.
    next_id: parking_lot::Mutex<u64>,
}

impl AnchorClient {
    /// Build from an [`AgentConfig`]. The HTTP client picks up the configured
    /// timeout; retries reuse the same backoff window as the feed client.
    pub fn from_config(cfg: &AgentConfig) -> Result<Self, AnchorError> {
        let http = reqwest::Client::builder()
            .timeout(cfg.rpc_timeout)
            .user_agent(cfg.user_agent.clone())
            .build()
            .map_err(|e| AnchorError::Transport {
                url: cfg.rpc_url.clone(),
                source: e,
            })?;
        Ok(Self {
            http,
            rpc_url: cfg.rpc_url.clone(),
            max_retries: cfg.max_retries,
            backoff_initial: cfg.backoff_initial,
            backoff_max: cfg.backoff_max,
            next_id: parking_lot::Mutex::new(1),
        })
    }

    /// Build a test-only client with a fixed URL and no retry pacing.
    pub fn for_url(url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("reqwest client must build"),
            rpc_url: url.into(),
            max_retries: 0,
            backoff_initial: Duration::from_millis(50),
            backoff_max: Duration::from_millis(100),
            next_id: parking_lot::Mutex::new(1),
        }
    }

    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    fn next_id(&self) -> u64 {
        let mut guard = self.next_id.lock();
        let id = *guard;
        *guard = guard.wrapping_add(1);
        id
    }

    /// Issue a raw JSON-RPC POST. The caller passes the method and params;
    /// this function returns the parsed `result` field on success or an
    /// [`AnchorError`] on any kind of failure.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, AnchorError> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": method,
            "params": params,
        });
        let resp = self
            .http
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|source| AnchorError::Transport {
                url: self.rpc_url.clone(),
                source,
            })?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|source| AnchorError::Transport {
                url: self.rpc_url.clone(),
                source,
            })?;
        if !status.is_success() {
            let body_text = String::from_utf8_lossy(&bytes).to_string();
            return Err(AnchorError::Status {
                url: self.rpc_url.clone(),
                status: status.as_u16(),
                body: truncate(&body_text, 256),
            });
        }
        let v: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|source| AnchorError::Decode {
                url: self.rpc_url.clone(),
                body: truncate(&String::from_utf8_lossy(&bytes), 256),
                source,
            })?;
        if let Some(err) = v.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("(no message)")
                .to_string();
            return Err(AnchorError::Rpc {
                url: self.rpc_url.clone(),
                code,
                message,
            });
        }
        Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null))
    }

    /// Idempotently ensure a personal ledger exists for `wallet_hex`.
    /// Returns `Ok(true)` if the ledger was created in this call,
    /// `Ok(false)` if it already existed (RPC error code 2001).
    pub async fn ensure_ledger(&self, wallet_hex: &str) -> Result<bool, AnchorError> {
        let params =
            serde_json::Value::Array(vec![serde_json::Value::String(wallet_hex.to_string())]);
        match self.call("rope_createPersonalLedger", params).await {
            Ok(_) => {
                tracing::info!(
                    target: "oracle_agent::anchor",
                    wallet = wallet_hex,
                    "personal ledger created on local node"
                );
                Ok(true)
            }
            Err(AnchorError::Rpc { code: 2001, .. }) => {
                tracing::debug!(
                    target: "oracle_agent::anchor",
                    wallet = wallet_hex,
                    "personal ledger already exists"
                );
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// Append one testimony interaction. Single attempt — no retry. Used by
    /// tests; production callers use `append_with_retry`.
    pub async fn append_once(
        &self,
        wallet_hex: &str,
        interaction: &Interaction,
    ) -> Result<AppendResult, AnchorError> {
        let params = serde_json::json!([
            wallet_hex,
            {
                "interaction_type": interaction.interaction_type,
                "description": interaction.description,
                "metadata": interaction.metadata_as_json(),
            }
        ]);
        let result = self.call("rope_appendToLedger", params).await?;
        let piece_count = result
            .get("index")
            .and_then(|v| v.as_u64())
            .ok_or(AnchorError::MissingField { field: "index" })? as u32;
        let knot_string_id = result
            .get("hash")
            .and_then(|v| v.as_str())
            .ok_or(AnchorError::MissingField { field: "hash" })?
            .to_string();
        Ok(AppendResult {
            piece_count,
            knot_string_id,
        })
    }

    /// Append with exponential backoff. Aborts immediately on non-transient
    /// errors (e.g. "No ledger" returned by the node).
    pub async fn append_with_retry(
        &self,
        wallet_hex: &str,
        interaction: &Interaction,
    ) -> Result<AppendResult, AnchorError> {
        let mut attempt: u32 = 0;
        let mut delay = self.backoff_initial;
        loop {
            match self.append_once(wallet_hex, interaction).await {
                Ok(out) => {
                    if attempt > 0 {
                        tracing::info!(
                            target: "oracle_agent::anchor",
                            wallet = wallet_hex,
                            attempts = attempt + 1,
                            knot_string_id = %out.knot_string_id,
                            "testimony anchored after {} retr{}",
                            attempt,
                            if attempt == 1 { "y" } else { "ies" }
                        );
                    } else {
                        tracing::info!(
                            target: "oracle_agent::anchor",
                            wallet = wallet_hex,
                            knot_string_id = %out.knot_string_id,
                            "testimony anchored"
                        );
                    }
                    return Ok(out);
                }
                Err(e) if e.is_transient() && attempt < self.max_retries => {
                    tracing::warn!(
                        target: "oracle_agent::anchor",
                        wallet = wallet_hex,
                        attempt,
                        retry_in_ms = delay.as_millis() as u64,
                        error = %e,
                        "anchor failed transiently; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    delay = (delay.saturating_mul(2)).min(self.backoff_max);
                }
                Err(e) => {
                    tracing::error!(
                        target: "oracle_agent::anchor",
                        wallet = wallet_hex,
                        attempts = attempt + 1,
                        transient = e.is_transient(),
                        error = %e,
                        "anchor failed permanently"
                    );
                    return Err(e);
                }
            }
        }
    }
}

/// One testimony interaction as it gets serialised onto the wire.
///
/// Mirrors the shape `rope_appendToLedger` accepts but stays in the
/// oracle-agent crate so we don't take a workspace dep on `rope-core`.
#[derive(Clone, Debug, PartialEq)]
pub struct Interaction {
    pub interaction_type: String,
    pub description: String,
    pub metadata: BTreeMap<String, String>,
}

impl Interaction {
    /// Build a `TestimonySubmission` interaction — the canonical shape used
    /// by AI testimony agents.
    pub fn testimony(description: impl Into<String>) -> Self {
        Self {
            interaction_type: "TestimonySubmission".to_string(),
            description: description.into(),
            metadata: BTreeMap::new(),
        }
    }

    /// Insert a metadata key. Values are stringified on the node side, so
    /// callers can keep their data in raw string form.
    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    fn metadata_as_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::with_capacity(self.metadata.len());
        for (k, v) in &self.metadata {
            map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        serde_json::Value::Object(map)
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
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ok_append_response(id: i64, knot_id: &str, piece_count: u64) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "index": piece_count,
                "hash": knot_id
            }
        })
    }

    fn rpc_error(id: i64, code: i64, message: &str) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        })
    }

    #[tokio::test]
    async fn append_once_parses_canonical_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(header("content-type", "application/json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ok_append_response(1, "0xabc123", 7)),
            )
            .mount(&server)
            .await;

        let client = AnchorClient::for_url(server.uri());
        let interaction = Interaction::testimony("hello").with_meta("k", "v");
        let out = client
            .append_once("0x0000000000000000000000000000000000000C002", &interaction)
            .await
            .expect("append must succeed");

        assert_eq!(out.knot_string_id, "0xabc123");
        assert_eq!(out.piece_count, 7);
    }

    #[tokio::test]
    async fn append_once_reports_rpc_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_error(
                1,
                2002,
                "No ledger found for this address",
            )))
            .mount(&server)
            .await;

        let client = AnchorClient::for_url(server.uri());
        let err = client
            .append_once(
                "0x0000000000000000000000000000000000000C002",
                &Interaction::testimony("x"),
            )
            .await
            .expect_err("missing-ledger error must surface");

        match err {
            AnchorError::Rpc { code: 2002, .. } => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn append_once_reports_missing_hash_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "index": 1 }
            })))
            .mount(&server)
            .await;

        let client = AnchorClient::for_url(server.uri());
        let err = client
            .append_once(
                "0x0000000000000000000000000000000000000C002",
                &Interaction::testimony("x"),
            )
            .await
            .expect_err("missing field must error");
        assert!(matches!(err, AnchorError::MissingField { field: "hash" }));
    }

    #[tokio::test]
    async fn append_with_retry_recovers_after_transient_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(503).set_body_string("temporarily down"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ok_append_response(1, "0xdead", 3)),
            )
            .with_priority(2)
            .mount(&server)
            .await;

        let mut client = AnchorClient::for_url(server.uri());
        client.max_retries = 3;

        let out = client
            .append_with_retry(
                "0x0000000000000000000000000000000000000C002",
                &Interaction::testimony("x"),
            )
            .await
            .expect("retry must recover");
        assert_eq!(out.knot_string_id, "0xdead");
    }

    #[tokio::test]
    async fn append_with_retry_aborts_on_non_transient_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_error(
                1,
                2002,
                "missing ledger",
            )))
            .mount(&server)
            .await;

        let mut client = AnchorClient::for_url(server.uri());
        client.max_retries = 5;

        let err = client
            .append_with_retry(
                "0x0000000000000000000000000000000000000C002",
                &Interaction::testimony("x"),
            )
            .await
            .expect_err("must NOT retry non-transient errors");
        match err {
            AnchorError::Rpc { code: 2002, .. } => {}
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn ensure_ledger_treats_already_exists_as_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(rpc_error(
                1,
                2001,
                "Ledger already exists for this address",
            )))
            .mount(&server)
            .await;

        let client = AnchorClient::for_url(server.uri());
        let created = client
            .ensure_ledger("0x0000000000000000000000000000000000000C002")
            .await
            .expect("2001 must be mapped to Ok(false)");
        assert!(!created);
    }

    #[tokio::test]
    async fn ensure_ledger_returns_true_on_first_creation() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": { "owner": "0x...", "created_at": 1_700_000_000_i64 }
            })))
            .mount(&server)
            .await;

        let client = AnchorClient::for_url(server.uri());
        let created = client
            .ensure_ledger("0x0000000000000000000000000000000000000C002")
            .await
            .expect("first-time creation must return Ok(true)");
        assert!(created);
    }

    #[test]
    fn transient_classification_matches_node_error_codes() {
        // Domain errors that the agent must NOT retry
        for code in [-32602_i64, 2001, 2002, 2003] {
            let err = AnchorError::Rpc {
                url: "http://example".into(),
                code,
                message: "x".into(),
            };
            assert!(!err.is_transient(), "code {code} should be non-transient");
        }
        // -32603 internal error should retry (the rope-node returns this for
        // any anyhow error bubbled out of the handler).
        let err = AnchorError::Rpc {
            url: "http://example".into(),
            code: -32603,
            message: "x".into(),
        };
        assert!(err.is_transient());
        // 5xx should retry; 4xx should not
        let err5 = AnchorError::Status {
            url: "http://x".into(),
            status: 503,
            body: String::new(),
        };
        assert!(err5.is_transient());
        let err4 = AnchorError::Status {
            url: "http://x".into(),
            status: 400,
            body: String::new(),
        };
        assert!(!err4.is_transient());
    }
}
