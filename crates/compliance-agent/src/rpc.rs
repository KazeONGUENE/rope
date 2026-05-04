// =============================================================================
// JSON-RPC client trait + reqwest-backed implementation
// =============================================================================
//
// Both `orchestrator` (which calls `rope_untieKnot`) and `anchor` (which
// calls `rope_appendToLedger`) need to talk to the same rope-node
// JSON-RPC endpoint. We model this with a small trait so:
//
//   * Tests can swap in a `MockRopeRpcClient` that records what was
//     requested and returns deterministic canned responses.
//   * The real binary uses `HttpRopeRpcClient`, a thin wrapper around
//     `reqwest::Client` configured with the agent's RPC URL.
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use thiserror::Error;

/// Errors surfaced from the JSON-RPC transport. Domain-level rejections
/// (e.g. "knot does not belong to wallet") are returned as `RpcError`
/// with the rope-node error code preserved so the orchestrator can
/// branch on it.
#[derive(Debug, Clone, Error)]
pub enum RpcClientError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("invalid JSON-RPC response: {0}")]
    InvalidResponse(String),
    #[error("rope-node returned error {code}: {message}")]
    RpcError { code: i64, message: String },
}

/// Result of a JSON-RPC call. The successful payload is the `result`
/// member of the JSON-RPC envelope, parsed as a generic `Value`.
pub type RpcResult = std::result::Result<Value, RpcClientError>;

/// Async trait implemented by anything that can satisfy a JSON-RPC
/// call against a rope-node. Cheap to clone (Arc + reqwest::Client
/// internally), so it can be handed to the HTTP request handler and
/// the periodic reporter alike.
#[async_trait]
pub trait RopeRpcClient: Send + Sync + 'static {
    async fn call(&self, method: &str, params: Value) -> RpcResult;
}

/// `reqwest`-backed implementation. Thread-safe; a single instance is
/// expected to be shared across the whole agent process.
#[derive(Clone)]
pub struct HttpRopeRpcClient {
    inner: Arc<HttpRopeRpcInner>,
}

struct HttpRopeRpcInner {
    url: String,
    client: reqwest::Client,
}

impl HttpRopeRpcClient {
    pub fn new(url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client init");
        Self {
            inner: Arc::new(HttpRopeRpcInner {
                url: url.into(),
                client,
            }),
        }
    }

    pub fn url(&self) -> &str {
        &self.inner.url
    }
}

#[async_trait]
impl RopeRpcClient for HttpRopeRpcClient {
    async fn call(&self, method: &str, params: Value) -> RpcResult {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .inner
            .client
            .post(&self.inner.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| RpcClientError::Transport(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| RpcClientError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(RpcClientError::Transport(format!(
                "HTTP {}: {}",
                status, text
            )));
        }
        let envelope: Value = serde_json::from_str(&text)
            .map_err(|e| RpcClientError::InvalidResponse(format!("{}: body={}", e, text)))?;
        if let Some(err) = envelope.get("error") {
            let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-32603);
            let message = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("(no message)")
                .to_string();
            return Err(RpcClientError::RpcError { code, message });
        }
        Ok(envelope
            .get("result")
            .cloned()
            .unwrap_or(Value::Null))
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use parking_lot::Mutex;

    /// Ordered record of a single observed RPC call.
    #[derive(Debug, Clone)]
    pub struct RecordedCall {
        pub method: String,
        pub params: Value,
    }

    /// Result the mock should return for the next call to `method`.
    #[derive(Clone)]
    pub enum MockResponse {
        Ok(Value),
        Err(RpcClientError),
    }

    /// Mock `RopeRpcClient` for unit tests. Stores every call and
    /// returns canned responses keyed by method name in arrival
    /// order. Falls back to `Value::Null` if no canned response was
    /// queued.
    #[derive(Default)]
    pub struct MockRopeRpcClient {
        calls: Mutex<Vec<RecordedCall>>,
        responses: Mutex<std::collections::HashMap<String, std::collections::VecDeque<MockResponse>>>,
    }

    impl MockRopeRpcClient {
        pub fn new() -> Self {
            Self::default()
        }

        /// Queue an `Ok(value)` response for the next call to `method`.
        pub fn enqueue_ok(&self, method: &str, value: Value) {
            self.responses
                .lock()
                .entry(method.to_string())
                .or_default()
                .push_back(MockResponse::Ok(value));
        }

        /// Queue an `Err` response for the next call to `method`.
        pub fn enqueue_err(&self, method: &str, err: RpcClientError) {
            self.responses
                .lock()
                .entry(method.to_string())
                .or_default()
                .push_back(MockResponse::Err(err));
        }

        pub fn calls(&self) -> Vec<RecordedCall> {
            self.calls.lock().clone()
        }

        pub fn calls_for(&self, method: &str) -> Vec<RecordedCall> {
            self.calls
                .lock()
                .iter()
                .filter(|c| c.method == method)
                .cloned()
                .collect()
        }
    }

    impl Clone for MockRopeRpcClient {
        fn clone(&self) -> Self {
            Self {
                calls: Mutex::new(self.calls.lock().clone()),
                responses: Mutex::new(self.responses.lock().clone()),
            }
        }
    }

    #[async_trait]
    impl RopeRpcClient for MockRopeRpcClient {
        async fn call(&self, method: &str, params: Value) -> RpcResult {
            self.calls.lock().push(RecordedCall {
                method: method.to_string(),
                params: params.clone(),
            });
            let mut all = self.responses.lock();
            if let Some(queue) = all.get_mut(method) {
                if let Some(resp) = queue.pop_front() {
                    return match resp {
                        MockResponse::Ok(v) => Ok(v),
                        MockResponse::Err(e) => Err(e),
                    };
                }
            }
            Ok(Value::Null)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[tokio::test]
    async fn mock_records_calls_and_returns_canned_response() {
        let mock = MockRopeRpcClient::new();
        mock.enqueue_ok("rope_untieKnot", json!({"tombstone_audit_hash": "0xab"}));

        let v = mock
            .call("rope_untieKnot", json!(["0xWALLET", "0xKNOT", "GdprArticle17"]))
            .await
            .expect("ok");
        assert_eq!(v.get("tombstone_audit_hash").unwrap(), "0xab");

        let calls = mock.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].method, "rope_untieKnot");
    }

    #[tokio::test]
    async fn mock_returns_queued_error() {
        let mock = MockRopeRpcClient::new();
        mock.enqueue_err(
            "rope_untieKnot",
            RpcClientError::RpcError {
                code: 2010,
                message: "genesis knot".into(),
            },
        );
        let err = mock
            .call("rope_untieKnot", json!([]))
            .await
            .expect_err("error");
        match err {
            RpcClientError::RpcError { code, .. } => assert_eq!(code, 2010),
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[tokio::test]
    async fn mock_returns_null_when_no_response_queued() {
        let mock = MockRopeRpcClient::new();
        let v = mock.call("anything", json!([])).await.expect("ok");
        assert!(v.is_null());
    }
}
