//! Engine API client — the piece that was missing from production.
//!
//! Reth (the EVM execution layer) has run in `--dev` mode on every
//! Datachain Rope node since deployment, meaning it invents its own
//! blocks on an internal timer with no external consensus driver. Real
//! post-Merge Ethereum execution clients are driven by a *separate*
//! consensus client over the authenticated JSON-RPC "Engine API"
//! (`engine_forkchoiceUpdatedV2`, `engine_getPayloadV2`,
//! `engine_newPayloadV2`). This module is that consensus-side client.
//!
//! Auth: the Engine API requires a JWT (HS256, shared secret from
//! `jwt.hex`, single claim `iat` = current unix timestamp, per the
//! Ethereum Engine API authentication spec). A fresh token is minted on
//! every call — tokens are cheap to make and the spec only requires
//! `iat` to be within a small clock-skew window of the server's clock.

use anyhow::{bail, Context, Result};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::Serialize;
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
struct EngineClaims {
    iat: u64,
}

/// Authenticated JSON-RPC client to a single Reth node's Engine API
/// (`authrpc`) and plain JSON-RPC (`http`) endpoints.
pub struct EngineClient {
    http: reqwest::Client,
    engine_url: String,
    rpc_url: String,
    /// `None` for read-only "upstream" clients that only ever issue plain
    /// JSON-RPC calls (e.g. a follower reading BLUE's public RPC). Such a
    /// client must never need — and therefore never receives — another
    /// node's private Engine API secret. Calling an `engine_*` method on
    /// a client constructed this way is a programming error, not a
    /// runtime possibility we need to survive gracefully, so it errors
    /// clearly rather than silently signing with a zero key.
    jwt_secret: Option<Vec<u8>>,
}

impl EngineClient {
    /// Full client: plain RPC + authenticated Engine API. Use for the
    /// *local* node this process is supposed to drive.
    pub fn new(engine_url: String, rpc_url: String, jwt_hex: &str) -> Result<Self> {
        let jwt_hex = jwt_hex.trim().trim_start_matches("0x");
        let jwt_secret = hex::decode(jwt_hex).context("jwt secret is not valid hex")?;
        if jwt_secret.len() < 32 {
            bail!("jwt secret must be at least 32 bytes, got {}", jwt_secret.len());
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self {
            http,
            engine_url,
            rpc_url,
            jwt_secret: Some(jwt_secret),
        })
    }

    /// Read-only client: plain RPC only, no Engine API secret required or
    /// accepted. Use for a remote "upstream" node whose private JWT this
    /// process has no business holding.
    pub fn new_readonly(rpc_url: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self {
            http,
            engine_url: String::new(),
            rpc_url,
            jwt_secret: None,
        })
    }

    fn mint_jwt(&self) -> Result<String> {
        let secret = self
            .jwt_secret
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("this client has no Engine API secret (read-only/upstream client) — engine_* calls are not available"))?;
        let iat = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let key = EncodingKey::from_secret(secret);
        let token = encode(&Header::default(), &EngineClaims { iat }, &key)?;
        Ok(token)
    }

    async fn call_engine(&self, method: &str, params: Value) -> Result<Value> {
        let token = self.mint_jwt()?;
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .http
            .post(&self.engine_url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("engine call {method} transport error"))?;
        let parsed: Value = resp.json().await.with_context(|| format!("engine call {method} bad json"))?;
        if let Some(err) = parsed.get("error") {
            bail!("engine call {method} rpc error: {err}");
        }
        Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn call_rpc(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .http
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("rpc call {method} transport error"))?;
        let parsed: Value = resp.json().await.with_context(|| format!("rpc call {method} bad json"))?;
        if let Some(err) = parsed.get("error") {
            bail!("rpc call {method} rpc error: {err}");
        }
        Ok(parsed.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn block_number(&self) -> Result<u64> {
        let r = self.call_rpc("eth_blockNumber", json!([])).await?;
        parse_hex_u64(&r)
    }

    pub async fn get_block_by_number(&self, n: u64, full_txs: bool) -> Result<Option<Value>> {
        let r = self
            .call_rpc(
                "eth_getBlockByNumber",
                json!([format!("0x{:x}", n), full_txs]),
            )
            .await?;
        Ok(if r.is_null() { None } else { Some(r) })
    }

    pub async fn get_raw_transaction(&self, hash: &str) -> Result<String> {
        let r = self
            .call_rpc("debug_getRawTransaction", json!([hash]))
            .await?;
        r.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("debug_getRawTransaction returned non-string for {hash}"))
    }

    /// `engine_forkchoiceUpdatedV2` — sets head/safe/finalized and,
    /// optionally, requests a new payload build (when `payload_attributes`
    /// is `Some`). Returns `(payload_status, payload_id)`.
    pub async fn forkchoice_updated_v2(
        &self,
        head_block_hash: &str,
        safe_block_hash: &str,
        finalized_block_hash: &str,
        payload_attributes: Option<Value>,
    ) -> Result<(Value, Option<String>)> {
        let fcs = json!({
            "headBlockHash": head_block_hash,
            "safeBlockHash": safe_block_hash,
            "finalizedBlockHash": finalized_block_hash,
        });
        let params = json!([fcs, payload_attributes]);
        let r = self.call_engine("engine_forkchoiceUpdatedV2", params).await?;
        let status = r.get("payloadStatus").cloned().unwrap_or(Value::Null);
        let payload_id = r
            .get("payloadId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Ok((status, payload_id))
    }

    pub async fn get_payload_v2(&self, payload_id: &str) -> Result<Value> {
        self.call_engine("engine_getPayloadV2", json!([payload_id])).await
    }

    /// Returns the `status` field ("VALID" | "INVALID" | "SYNCING" | ...).
    pub async fn new_payload_v2(&self, payload: &Value) -> Result<String> {
        let r = self.call_engine("engine_newPayloadV2", json!([payload])).await?;
        Ok(r.get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("UNKNOWN")
            .to_string())
    }
}

pub fn parse_hex_u64(value: &Value) -> Result<u64> {
    let s = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("expected hex string, got {value:?}"))?;
    let s = s.trim_start_matches("0x");
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(s, 16).map_err(|e| anyhow::anyhow!("invalid hex u64 {s}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_u64() {
        assert_eq!(parse_hex_u64(&json!("0x0")).unwrap(), 0);
        assert_eq!(parse_hex_u64(&json!("0x425d4")).unwrap(), 271828);
        assert_eq!(parse_hex_u64(&json!("0x")).unwrap(), 0);
    }

    #[test]
    fn test_engine_client_rejects_short_secret() {
        let short = hex::encode([0u8; 16]);
        let res = EngineClient::new(
            "http://127.0.0.1:8552".into(),
            "http://127.0.0.1:8595".into(),
            &short,
        );
        assert!(res.is_err());
    }

    #[test]
    fn test_engine_client_accepts_valid_secret() {
        let good = hex::encode([1u8; 32]);
        let res = EngineClient::new(
            "http://127.0.0.1:8552".into(),
            "http://127.0.0.1:8595".into(),
            &good,
        );
        assert!(res.is_ok());
    }

    #[test]
    fn test_engine_client_accepts_0x_prefixed_secret() {
        let good = format!("0x{}", hex::encode([2u8; 32]));
        let res = EngineClient::new(
            "http://127.0.0.1:8552".into(),
            "http://127.0.0.1:8595".into(),
            &good,
        );
        assert!(res.is_ok());
    }

    #[test]
    fn test_mint_jwt_produces_three_part_token() {
        let good = hex::encode([3u8; 32]);
        let client = EngineClient::new(
            "http://127.0.0.1:8552".into(),
            "http://127.0.0.1:8595".into(),
            &good,
        )
        .unwrap();
        let token = client.mint_jwt().unwrap();
        assert_eq!(token.split('.').count(), 3);
    }
}
