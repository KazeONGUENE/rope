//! JSON-RPC client for the canonical rope-node.
//!
//! Uses two upstream methods:
//!
//! - `rope_listStrings(kind, offset, limit)`: enumerates registered
//!   strings by kind. The shadow witness uses this to discover wallet
//!   strings to observe.
//! - `rope_getStringWithKnots(wallet)`: returns the full knot list
//!   (including tombstones) for one wallet's string. The shadow
//!   witness uses this to find new knots and tombstones since the
//!   last observation.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::debug;

use crate::chain::ObservedKnot;
use crate::error::{ShadowWitnessError, ShadowWitnessResult};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StringListEntry {
    /// `wallet`, `contract`, `asset`, `did`, or `cord`.
    #[serde(default)]
    pub kind: String,
    /// Hex-encoded string identifier.
    #[serde(default)]
    pub string_id: String,
    /// Wallet address (only set for `kind = "wallet"`).
    #[serde(default)]
    pub wallet_address: Option<String>,
    /// Knot count, if exposed.
    #[serde(default)]
    pub knot_count: Option<u64>,
}

/// Lightweight JSON-RPC client.
pub struct RpcClient {
    http: reqwest::Client,
    url: String,
}

impl RpcClient {
    pub fn new(url: impl Into<String>) -> ShadowWitnessResult<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(ShadowWitnessError::Rpc)?;
        Ok(Self {
            http,
            url: url.into(),
        })
    }

    async fn call(&self, method: &str, params: serde_json::Value) -> ShadowWitnessResult<serde_json::Value> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let text = resp.text().await?;
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| ShadowWitnessError::RpcDecode(format!("not json: {} body={}", e, text)))?;

        if let Some(err) = parsed.get("error") {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown")
                .to_string();
            return Err(ShadowWitnessError::RpcRemote { code, message });
        }
        parsed
            .get("result")
            .cloned()
            .ok_or_else(|| ShadowWitnessError::RpcDecode("missing result field".to_string()))
    }

    /// Enumerate registered strings of the given `kind`.
    /// Pagination is via `offset` and `limit`.
    pub async fn list_strings(
        &self,
        kind: &str,
        offset: u64,
        limit: u32,
    ) -> ShadowWitnessResult<Vec<StringListEntry>> {
        let params = json!([{ "kind": kind, "offset": offset, "limit": limit }]);
        let result = self.call("rope_listStrings", params).await?;
        let arr = result
            .get("strings")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(arr.len());
        for v in arr {
            let kind = v
                .get("kind")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let string_id = v
                .get("string_id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let wallet_address = v
                .get("wallet_address")
                .and_then(|x| x.as_str())
                .map(str::to_string);
            let knot_count = v.get("knot_count").and_then(|x| x.as_u64());
            out.push(StringListEntry {
                kind,
                string_id,
                wallet_address,
                knot_count,
            });
        }
        Ok(out)
    }

    /// Fetch the canonical knot list (including tombstones) for one
    /// wallet's string.
    pub async fn get_string_with_knots(&self, wallet: &str) -> ShadowWitnessResult<Vec<ObservedKnot>> {
        let params = json!([wallet]);
        let result = self.call("rope_getStringWithKnots", params).await?;
        let knots = result
            .get("knots")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let string_id = result
            .get("string_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut out = Vec::with_capacity(knots.len());
        for k in knots {
            let knot_index = k.get("knot_index").and_then(|v| v.as_u64()).unwrap_or(0);
            let status = k
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("active");
            let is_tombstone = status == "tombstone";

            let (untied_at, audit_hash, reason) = if is_tombstone {
                let t = k.get("tombstone").cloned().unwrap_or_default();
                let untied = t.get("untied_at").and_then(|v| v.as_i64());
                let audit = t
                    .get("audit_hash")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let reason = t
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                (untied, audit, reason)
            } else {
                (None, None, None)
            };

            out.push(ObservedKnot {
                string_id: string_id.clone(),
                knot_index,
                is_tombstone,
                tombstone_untied_at: untied_at,
                tombstone_audit_hash_hex: audit_hash,
                tombstone_reason: reason,
            });
        }
        debug!(wallet = %wallet, knot_count = out.len(), "rpc: fetched knots");
        Ok(out)
    }
}
