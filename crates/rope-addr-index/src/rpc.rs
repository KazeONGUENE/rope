//! Minimal JSON-RPC client used by the indexer to talk to Reth
//! (BLUE / GREEN / DO-rpc-*). Deliberately narrow: only the methods
//! the writer needs (`eth_blockNumber`, `eth_getBlockByNumber`,
//! `eth_getLogs`, `eth_getTransactionReceipt`), with per-URL failover.
//!
//! The indexer runs on rope-vps and talks to BLUE loopback in prod,
//! so timeouts are aggressive by default. The reader (dc-explorer)
//! keeps using its own `rpc_call` in `rope-explorer/src/main.rs` and
//! is unaffected.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("all RPC endpoints failed; last: {0}")]
    AllFailed(String),
    #[error("json-rpc protocol error: {0}")]
    Protocol(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type RpcResult<T> = Result<T, RpcError>;

/// JSON-RPC client with per-request failover across a list of URLs.
/// The last-known-good URL is remembered in an atomic so subsequent
/// calls stick to it until it fails.
#[derive(Clone)]
pub struct RpcClient {
    http: reqwest::Client,
    urls: Vec<String>,
    active: Arc<AtomicUsize>,
}

impl RpcClient {
    /// Build a client with the given endpoint list. `timeout` is the
    /// per-request budget (total across connect + body); production
    /// default is 10s.
    pub fn new(urls: Vec<String>, timeout: Duration) -> RpcResult<Self> {
        if urls.is_empty() {
            return Err(RpcError::AllFailed("no RPC URLs provided".to_string()));
        }
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent("rope-addr-indexer/0.1")
            .build()?;
        Ok(Self {
            http,
            urls,
            active: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Fire a single JSON-RPC call. Retries across every configured
    /// URL exactly once each. Returns the `result` field on success,
    /// or `RpcError` on total failure / RPC-level error.
    pub async fn call(&self, method: &str, params: serde_json::Value) -> RpcResult<serde_json::Value> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let n = self.urls.len();
        let start = self.active.load(Ordering::Relaxed);
        let mut last_err = String::new();
        for offset in 0..n {
            let idx = (start + offset) % n;
            let url = &self.urls[idx];
            match self.http.post(url).json(&body).send().await {
                Ok(res) => {
                    let status = res.status();
                    if !status.is_success() {
                        last_err = format!("{} -> HTTP {}", url, status);
                        continue;
                    }
                    match res.json::<JsonRpcResponse>().await {
                        Ok(rpc) => {
                            if let Some(err) = rpc.error {
                                // Deterministic RPC error - don't retry, try next URL.
                                last_err = format!("{} -> code {} {}", url, err.code, err.message);
                                continue;
                            }
                            if offset > 0 {
                                self.active.store(idx, Ordering::Relaxed);
                                tracing::warn!(target: "rope_addr_index::rpc", from = %self.urls[start], to = %url, "RPC failover");
                            }
                            return rpc
                                .result
                                .ok_or_else(|| RpcError::Protocol("missing result".to_string()));
                        }
                        Err(e) => {
                            last_err = format!("{} (parse): {}", url, e);
                        }
                    }
                }
                Err(e) => {
                    last_err = format!("{} (connect): {}", url, e);
                }
            }
        }
        Err(RpcError::AllFailed(last_err))
    }

    /// Convenience: `eth_blockNumber` returning a plain u64.
    pub async fn eth_block_number(&self) -> RpcResult<u64> {
        let v = self.call("eth_blockNumber", serde_json::json!([])).await?;
        parse_hex_u64(&v)
    }

    /// Convenience: `eth_chainId` returning a plain u64.
    pub async fn eth_chain_id(&self) -> RpcResult<u64> {
        let v = self.call("eth_chainId", serde_json::json!([])).await?;
        parse_hex_u64(&v)
    }

    /// `eth_getBlockByNumber(number, true)` - full transactions.
    /// Returns `None` if the node reports the block does not exist
    /// (past-the-tip or reorged out).
    pub async fn eth_get_block_by_number_full(
        &self,
        number: u64,
    ) -> RpcResult<Option<serde_json::Value>> {
        let hex = format!("0x{:x}", number);
        let v = self
            .call(
                "eth_getBlockByNumber",
                serde_json::json!([hex, true]),
            )
            .await?;
        if v.is_null() {
            return Ok(None);
        }
        Ok(Some(v))
    }

    /// `eth_getLogs({fromBlock, toBlock})`. Range is inclusive on both
    /// ends. Caller must keep the span small enough to fit Reth's
    /// server-side cap (default 10k per request in Reth as of 1.11.x).
    pub async fn eth_get_logs(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> RpcResult<Vec<serde_json::Value>> {
        let filter = serde_json::json!({
            "fromBlock": format!("0x{:x}", from_block),
            "toBlock": format!("0x{:x}", to_block),
        });
        let v = self.call("eth_getLogs", serde_json::json!([filter])).await?;
        match v {
            serde_json::Value::Array(arr) => Ok(arr),
            other => Err(RpcError::Protocol(format!(
                "eth_getLogs expected array, got {}",
                other
            ))),
        }
    }

    /// `eth_getTransactionReceipt(hash)`. Returns `None` if the node
    /// hasn't indexed the receipt yet (rare on a follower node after
    /// the block is fully synced).
    pub async fn eth_get_transaction_receipt(
        &self,
        tx_hash: &str,
    ) -> RpcResult<Option<serde_json::Value>> {
        let v = self
            .call(
                "eth_getTransactionReceipt",
                serde_json::json!([tx_hash]),
            )
            .await?;
        if v.is_null() {
            return Ok(None);
        }
        Ok(Some(v))
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

/// Parse `"0x..."` or a numeric JSON value into a u64. Accepts both
/// because different Reth versions have serialised block numbers both
/// ways in the past.
pub fn parse_hex_u64(v: &serde_json::Value) -> RpcResult<u64> {
    match v {
        serde_json::Value::String(s) => {
            let stripped = s.strip_prefix("0x").unwrap_or(s);
            u64::from_str_radix(stripped, 16).map_err(|e| {
                RpcError::Protocol(format!("bad hex u64 {:?}: {}", s, e))
            })
        }
        serde_json::Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| RpcError::Protocol(format!("non-u64 number {}", n))),
        other => Err(RpcError::Protocol(format!(
            "expected hex-string or number, got {}",
            other
        ))),
    }
}

/// Parse a hex `0x`-prefixed string into a `u128`. Used for `value`
/// (native FAT amount) which can exceed u64 for wide-carriage transfers.
pub fn parse_hex_u128(v: &serde_json::Value) -> RpcResult<u128> {
    match v {
        serde_json::Value::String(s) => {
            let stripped = s.strip_prefix("0x").unwrap_or(s);
            if stripped.is_empty() {
                return Ok(0);
            }
            u128::from_str_radix(stripped, 16).map_err(|e| {
                RpcError::Protocol(format!("bad hex u128 {:?}: {}", s, e))
            })
        }
        serde_json::Value::Number(n) => n
            .as_u64()
            .map(u128::from)
            .ok_or_else(|| RpcError::Protocol(format!("non-u128 number {}", n))),
        other => Err(RpcError::Protocol(format!(
            "expected hex-string, got {}",
            other
        ))),
    }
}

/// Parse a `0x...` hex string into raw bytes. Length is not enforced;
/// callers should validate the result matches their expected shape.
pub fn parse_hex_bytes(v: &serde_json::Value) -> RpcResult<Vec<u8>> {
    let s = v
        .as_str()
        .ok_or_else(|| RpcError::Protocol(format!("expected hex-string, got {}", v)))?;
    let stripped = s.strip_prefix("0x").unwrap_or(s);
    hex::decode(stripped).map_err(|e| RpcError::Protocol(format!("bad hex {:?}: {}", s, e)))
}

/// Parse a `0x...` hex string into a fixed 32-byte array (block/tx hash).
pub fn parse_hex_h256(v: &serde_json::Value) -> RpcResult<[u8; 32]> {
    let bytes = parse_hex_bytes(v)?;
    if bytes.len() != 32 {
        return Err(RpcError::Protocol(format!(
            "expected 32-byte hash, got {} bytes",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Parse a `0x...` hex string into a fixed 20-byte address.
pub fn parse_hex_h160(v: &serde_json::Value) -> RpcResult<[u8; 20]> {
    let bytes = parse_hex_bytes(v)?;
    if bytes.len() != 20 {
        return Err(RpcError::Protocol(format!(
            "expected 20-byte address, got {} bytes",
            bytes.len()
        )));
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Ok(out)
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_u64_accepts_0x_and_bare_and_number() {
        assert_eq!(parse_hex_u64(&serde_json::json!("0x10")).unwrap(), 16);
        assert_eq!(parse_hex_u64(&serde_json::json!("ff")).unwrap(), 255);
        assert_eq!(parse_hex_u64(&serde_json::json!(42)).unwrap(), 42);
        assert!(parse_hex_u64(&serde_json::json!("0xZZ")).is_err());
    }

    #[test]
    fn parse_hex_u128_handles_empty_and_large() {
        assert_eq!(parse_hex_u128(&serde_json::json!("0x")).unwrap(), 0);
        assert_eq!(parse_hex_u128(&serde_json::json!("0x0")).unwrap(), 0);
        assert_eq!(
            parse_hex_u128(&serde_json::json!("0xffffffffffffffffffffffffffffffff")).unwrap(),
            u128::MAX
        );
    }

    #[test]
    fn parse_hex_h160_and_h256_length_check() {
        let a20 = "0x0102030405060708090a0b0c0d0e0f1011121314";
        assert_eq!(parse_hex_h160(&serde_json::json!(a20)).unwrap()[0], 0x01);
        assert!(parse_hex_h160(&serde_json::json!("0x1234")).is_err());
        let h = format!("0x{}", "ab".repeat(32));
        assert_eq!(parse_hex_h256(&serde_json::json!(h)).unwrap()[0], 0xab);
    }
}
