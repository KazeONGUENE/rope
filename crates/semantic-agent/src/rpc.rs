//! Thin JSON-RPC client for the rope-node methods the SemanticAgent
//! consumes:
//!
//! - `rope_globalStats`              (Quipu Canon v1.2 totals)
//! - `rope_listStrings`              (paginated string descriptors)
//! - `rope_getStringWithKnots`       (per-string knot list with status)
//! - `rope_repatriatePersonalLedger` (per-string entries with timestamps)
//! - `rope_appendToLedger`           (used by [`crate::anchor`])
//!
//! The client is sync-friendly via tokio; calls return parsed
//! `serde_json::Value` so the caller can pick the fields it cares about
//! without us having to mirror every node-side schema change.

use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON-RPC error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("Malformed JSON-RPC response: {0}")]
    Malformed(String),
    #[error("Could not build HTTP client: {0}")]
    Build(String),
}

/// Thin wrapper around `reqwest::Client` plus a per-process JSON-RPC id
/// counter. Cloneable: every clone shares the same connection pool.
#[derive(Debug, Clone)]
pub struct RpcClient {
    http: Client,
    url: String,
    next_id: std::sync::Arc<AtomicU64>,
}

impl RpcClient {
    pub fn new(url: String, timeout: Duration) -> Result<Self, RpcError> {
        let http = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| RpcError::Build(e.to_string()))?;
        Ok(Self {
            http,
            url,
            next_id: std::sync::Arc::new(AtomicU64::new(1)),
        })
    }

    /// Issue a raw JSON-RPC call. Returns the `result` field on success.
    pub async fn call<P: Serialize>(&self, method: &str, params: P) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });
        let resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        if let Some(err) = resp.get("error") {
            let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
            let message = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown rpc error")
                .to_string();
            return Err(RpcError::Rpc { code, message });
        }
        resp.get("result")
            .cloned()
            .ok_or_else(|| RpcError::Malformed("missing `result` field".into()))
    }

    /// `rope_globalStats` — Quipu Canon v1.2 totals.
    pub async fn global_stats(&self) -> Result<Value, RpcError> {
        self.call("rope_globalStats", json!([])).await
    }

    /// `rope_listStrings` — paginated string descriptors.
    /// `kind` is one of `wallet | contract | asset | did | cord` (or
    /// `None` for "all kinds").
    pub async fn list_strings(
        &self,
        kind: Option<&str>,
        offset: u64,
        limit: u32,
    ) -> Result<Value, RpcError> {
        let mut p = serde_json::Map::new();
        if let Some(k) = kind {
            p.insert("kind".into(), Value::String(k.into()));
        }
        p.insert("offset".into(), json!(offset));
        p.insert("limit".into(), json!(limit));
        self.call("rope_listStrings", json!([Value::Object(p)]))
            .await
    }

    /// `rope_getStringWithKnots` — list of knots on a wallet's string.
    /// (As of the Quipu Canon v1.2 RPC layer, the node's
    /// `rope_getStringWithKnots` is wallet-keyed only; non-wallet kinds
    /// fall back to descriptor-only data via `list_strings`.)
    pub async fn get_string_with_knots(&self, wallet_hex: &str) -> Result<Value, RpcError> {
        self.call("rope_getStringWithKnots", json!([wallet_hex]))
            .await
    }

    /// `rope_repatriatePersonalLedger` — wallet's fragments (knot ids
    /// + timestamps).
    pub async fn repatriate_personal_ledger(&self, wallet_hex: &str) -> Result<Value, RpcError> {
        self.call("rope_repatriatePersonalLedger", json!([wallet_hex]))
            .await
    }

    /// `rope_appendToLedger` — append one knot to `owner`'s string.
    /// Returns the JSON `{ "index": u32, "hash": "0x…" }` shape that
    /// the rope-node returns for personal-ledger appends.
    pub async fn append_to_ledger(
        &self,
        owner: &str,
        interaction: Value,
    ) -> Result<Value, RpcError> {
        self.call("rope_appendToLedger", json!([owner, interaction]))
            .await
    }

    /// Public RPC URL — exposed for diagnostics endpoints.
    pub fn url(&self) -> &str {
        &self.url
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn call_returns_result_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"answer": 42}
            })))
            .mount(&server)
            .await;
        let rpc = RpcClient::new(server.uri(), Duration::from_secs(2)).unwrap();
        let v = rpc.call("test_method", json!([])).await.unwrap();
        assert_eq!(v["answer"], 42);
    }

    #[tokio::test]
    async fn call_propagates_rpc_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32601, "message": "no such method"}
            })))
            .mount(&server)
            .await;
        let rpc = RpcClient::new(server.uri(), Duration::from_secs(2)).unwrap();
        let err = rpc.call("nope", json!([])).await.unwrap_err();
        match err {
            RpcError::Rpc { code, .. } => assert_eq!(code, -32601),
            other => panic!("unexpected err: {other:?}"),
        }
    }
}
