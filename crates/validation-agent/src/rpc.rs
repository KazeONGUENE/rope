//! JSON-RPC client trait + a `reqwest`-backed HTTP implementation.
//!
//! Splitting the trait out lets us drive the entire poll → verify →
//! witness pipeline from a single tokio test against a stubbed
//! [`MockRpcClient`] without touching the network. Production code
//! uses [`HttpRpcClient`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use thiserror::Error;

/// Strongly-typed JSON-RPC error.
#[derive(Debug, Error)]
pub enum JsonRpcError {
    /// Transport-level failure (connection refused, timeout, DNS, …).
    #[error("rpc transport error: {0}")]
    Transport(String),

    /// HTTP non-2xx status code from the upstream node.
    #[error("rpc http status {status}: {body}")]
    HttpStatus {
        /// HTTP status code.
        status: u16,
        /// Response body fragment.
        body: String,
    },

    /// JSON-RPC server returned an `error` object.
    #[error("rpc method `{method}` returned server error code {code}: {message}")]
    Server {
        /// JSON-RPC method that was invoked.
        method: String,
        /// Server-side error code.
        code: i64,
        /// Server-side error message.
        message: String,
    },

    /// Server returned a malformed body or unexpected shape.
    #[error("malformed rpc response from `{method}`: {detail}")]
    Malformed {
        /// JSON-RPC method that was invoked.
        method: String,
        /// What we found wrong with the response.
        detail: String,
    },
}

/// Result alias for JSON-RPC calls.
pub type RpcResult<T> = Result<T, JsonRpcError>;

/// Minimal JSON-RPC client surface used by the validation agent.
///
/// The trait is `async_trait` to keep the implementation surface small
/// — neither `Send + Sync` nor lifetime-genericism is required by the
/// internal control loop.
#[async_trait]
pub trait RopeRpcClient: Send + Sync + std::fmt::Debug {
    /// Issue an arbitrary JSON-RPC `method` with positional `params`.
    /// Returns the deserialized `result` object on success.
    async fn call(&self, method: &str, params: Value) -> RpcResult<Value>;

    /// Helper: read the current cord anchor knot index via
    /// `rope_knotIndex` (canonical) with a fall-through to
    /// `eth_blockNumber` (EVM-compat alias) — both return a hex
    /// string.
    async fn knot_index(&self) -> RpcResult<u64> {
        // Prefer the canonical name. Fall through to the alias if the
        // server doesn't recognize it (older Reth-only deployments).
        let result = match self.call("rope_knotIndex", json!([])).await {
            Ok(v) => v,
            Err(JsonRpcError::Server { code, .. }) if code == -32601 || code == -32600 => {
                self.call("eth_blockNumber", json!([])).await?
            }
            Err(other) => return Err(other),
        };
        let s = result.as_str().ok_or_else(|| JsonRpcError::Malformed {
            method: "rope_knotIndex".to_string(),
            detail: "expected hex string".to_string(),
        })?;
        u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| JsonRpcError::Malformed {
            method: "rope_knotIndex".to_string(),
            detail: format!("not a valid hex u64: {e}"),
        })
    }

    /// Helper: fetch a cord anchor knot by index via
    /// `rope_getKnotByIndex` with EVM-compat fallthrough to
    /// `eth_getBlockByNumber`. Returns the raw knot JSON body.
    ///
    /// `full_txs = false` is sufficient for signature-only validation
    /// since we only need the knot hash + miner address; transaction
    /// details would just inflate the response.
    async fn get_knot_by_index(&self, index: u64) -> RpcResult<Value> {
        let hex_index = format!("0x{:x}", index);
        let params = json!([hex_index, false]);
        match self.call("rope_getKnotByIndex", params.clone()).await {
            Ok(v) => Ok(v),
            Err(JsonRpcError::Server { code, .. }) if code == -32601 || code == -32600 => {
                self.call("eth_getBlockByNumber", params).await
            }
            Err(other) => Err(other),
        }
    }

    /// Helper: submit a testimony interaction onto the agent's wallet
    /// string via `rope_appendToLedger`.
    async fn append_to_ledger(&self, owner_wallet: &str, interaction: Value) -> RpcResult<Value> {
        self.call("rope_appendToLedger", json!([owner_wallet, interaction]))
            .await
    }
}

/// Production HTTP JSON-RPC client backed by `reqwest`.
#[derive(Debug, Clone)]
pub struct HttpRpcClient {
    inner: reqwest::Client,
    url: String,
    next_id: Arc<AtomicU64>,
}

impl HttpRpcClient {
    /// Construct an HTTP client with a per-request timeout.
    pub fn new(url: impl Into<String>, timeout: Duration) -> Result<Self, JsonRpcError> {
        let inner = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| JsonRpcError::Transport(format!("client build failed: {e}")))?;
        Ok(Self {
            inner,
            url: url.into(),
            next_id: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Construct from an already-built `reqwest::Client` (useful for
    /// tests that want to inject custom middleware).
    pub fn with_client(url: impl Into<String>, inner: reqwest::Client) -> Self {
        Self {
            inner,
            url: url.into(),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

impl HttpRpcClient {
    /// Inner per-attempt request. Returns a `JsonRpcError` so the
    /// caller can decide whether to retry transport failures.
    async fn call_once(&self, method: &str, body: &Value) -> RpcResult<Value> {
        let resp = self
            .inner
            .post(&self.url)
            .json(body)
            .send()
            .await
            .map_err(|e| JsonRpcError::Transport(format!("{method} send failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(JsonRpcError::HttpStatus {
                status: status.as_u16(),
                body: body.chars().take(512).collect(),
            });
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| JsonRpcError::Transport(format!("{method} body parse failed: {e}")))?;

        if let Some(err) = json.get("error") {
            let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-32603);
            let message = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            return Err(JsonRpcError::Server {
                method: method.to_string(),
                code,
                message,
            });
        }

        Ok(json.get("result").cloned().unwrap_or(Value::Null))
    }
}

#[async_trait]
impl RopeRpcClient for HttpRpcClient {
    /// Retries once on `JsonRpcError::Transport` only.
    ///
    /// Rationale: `rope-node`'s HTTP server occasionally drops idle
    /// keep-alive connections under load, which surfaces here as
    /// `error sending request: connection closed before message
    /// completed` or `connection reset by peer`. Both are textbook
    /// transient failures — the next request opens a fresh socket and
    /// succeeds. A single short-backoff retry collapses the previous
    /// 3–5 warn/sec into ~zero noise without masking real protocol
    /// errors (`HttpStatus` and `Server` are NOT retried — they are
    /// authoritative responses from the node).
    async fn call(&self, method: &str, params: Value) -> RpcResult<Value> {
        let id = self.next_id();
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        match self.call_once(method, &body).await {
            Ok(v) => Ok(v),
            Err(JsonRpcError::Transport(first)) => {
                tokio::time::sleep(Duration::from_millis(150)).await;
                match self.call_once(method, &body).await {
                    Ok(v) => Ok(v),
                    Err(JsonRpcError::Transport(second)) => {
                        Err(JsonRpcError::Transport(format!(
                            "{method} retry failed: first={first}; second={second}"
                        )))
                    }
                    Err(other) => Err(other),
                }
            }
            Err(other) => Err(other),
        }
    }
}

// ---------------------------------------------------------------------
// Mock client for unit tests.
// ---------------------------------------------------------------------
//
// Lives behind `#[cfg(test)]` of the crate (it's `pub(crate)` so the
// witness/subscriber test modules can also use it). It's a simple
// FIFO of canned responses keyed by method name with a request log
// for assertion. We deliberately avoid `mockall` to keep the
// dev-dep set minimal.

#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::VecDeque;

    /// Recorded RPC call (method + params).
    #[derive(Debug, Clone)]
    pub struct RecordedCall {
        pub method: String,
        pub params: Value,
    }

    /// Behaviour to apply for the next call to a given method.
    #[derive(Debug)]
    pub enum Reply {
        Ok(Value),
        Err(JsonRpcError),
    }

    /// In-memory mock RPC client.
    #[derive(Debug, Default, Clone)]
    pub struct MockRpcClient {
        inner: Arc<MockInner>,
    }

    #[derive(Debug, Default)]
    struct MockInner {
        // Per-method FIFO of canned replies. When a method runs out of
        // canned replies we fall back to `default_reply` (Ok(null)).
        canned: Mutex<std::collections::HashMap<String, VecDeque<Reply>>>,
        log: Mutex<Vec<RecordedCall>>,
    }

    impl MockRpcClient {
        pub fn new() -> Self {
            Self::default()
        }

        /// Queue a successful reply for the next call to `method`.
        pub fn enqueue_ok(&self, method: &str, value: Value) -> &Self {
            self.inner
                .canned
                .lock()
                .entry(method.to_string())
                .or_default()
                .push_back(Reply::Ok(value));
            self
        }

        /// Queue an error reply for the next call to `method`.
        #[allow(dead_code)]
        pub fn enqueue_err(&self, method: &str, err: JsonRpcError) -> &Self {
            self.inner
                .canned
                .lock()
                .entry(method.to_string())
                .or_default()
                .push_back(Reply::Err(err));
            self
        }

        /// Return all RPC calls observed so far in chronological order.
        pub fn calls(&self) -> Vec<RecordedCall> {
            self.inner.log.lock().clone()
        }

        /// Count the number of calls to a specific method.
        pub fn count(&self, method: &str) -> usize {
            self.inner
                .log
                .lock()
                .iter()
                .filter(|c| c.method == method)
                .count()
        }
    }

    #[async_trait]
    impl RopeRpcClient for MockRpcClient {
        async fn call(&self, method: &str, params: Value) -> RpcResult<Value> {
            self.inner.log.lock().push(RecordedCall {
                method: method.to_string(),
                params: params.clone(),
            });
            let mut canned = self.inner.canned.lock();
            if let Some(q) = canned.get_mut(method) {
                if let Some(reply) = q.pop_front() {
                    return match reply {
                        Reply::Ok(v) => Ok(v),
                        Reply::Err(e) => Err(e),
                    };
                }
            }
            Ok(Value::Null)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::MockRpcClient;
    use super::*;

    #[tokio::test]
    async fn knot_index_decodes_canonical_hex() {
        let mock = MockRpcClient::new();
        mock.enqueue_ok("rope_knotIndex", json!("0x10"));
        let n = mock.knot_index().await.unwrap();
        assert_eq!(n, 16);
        assert_eq!(mock.count("rope_knotIndex"), 1);
    }

    #[tokio::test]
    async fn knot_index_falls_back_to_eth_blocknumber() {
        let mock = MockRpcClient::new();
        mock.enqueue_err(
            "rope_knotIndex",
            JsonRpcError::Server {
                method: "rope_knotIndex".to_string(),
                code: -32601,
                message: "method not found".to_string(),
            },
        );
        mock.enqueue_ok("eth_blockNumber", json!("0xff"));
        let n = mock.knot_index().await.unwrap();
        assert_eq!(n, 255);
        assert_eq!(mock.count("rope_knotIndex"), 1);
        assert_eq!(mock.count("eth_blockNumber"), 1);
    }

    #[tokio::test]
    async fn knot_index_propagates_unrecognized_errors() {
        let mock = MockRpcClient::new();
        mock.enqueue_err(
            "rope_knotIndex",
            JsonRpcError::Transport("connection refused".to_string()),
        );
        let res = mock.knot_index().await;
        assert!(matches!(res, Err(JsonRpcError::Transport(_))));
    }
}
