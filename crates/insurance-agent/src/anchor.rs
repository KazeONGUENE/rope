//! Submit signed attestations as testimony knots via `rope_appendToLedger`.
//!
//! Wire format (per `crates/rope-node/src/rpc_server.rs::rope_appendToLedger`):
//!
//! ```json
//! {
//!   "jsonrpc": "2.0",
//!   "method": "rope_appendToLedger",
//!   "params": [
//!     "<owner-address>",
//!     {
//!       "interaction_type": "ParametricInsuranceAttestation",
//!       "description": "<short summary>",
//!       "metadata": { "<canonical attestation JSON, flattened to strings>" }
//!     }
//!   ],
//!   "id": 1
//! }
//! ```
//!
//! The response contains `{ index, hash }` where `hash` is the canonical
//! `knot_string_id` per the v1.1 canon. We surface that as
//! [`AnchorReceipt::knot_string_id`].
//!
//! ## Signing
//!
//! On the live network, `rope_appendToLedger` is signed implicitly by the
//! node operating the wallet — the canonical InsuranceAgent wallet
//! `0x...C003` is owned by the federation operator running this CLI. Any
//! standalone signer is layered on top of this contract; we keep the trait
//! simple so a future signer-aware implementation can swap in.

use crate::attestation::{AttestationDigest, ParametricInsuranceAttestation};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Outcome of a successful `rope_appendToLedger` call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnchorReceipt {
    /// `knot_string_id` returned by the node — canonical 32-byte hex.
    pub knot_string_id: String,

    /// Sequence number on the agent's string.
    pub piece_count: u64,

    /// Local digest of the attestation payload (not what the node returned;
    /// useful for client-side de-dup).
    pub attestation_digest: AttestationDigest,
}

#[derive(Debug, Error)]
pub enum AnchorError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("RPC error {code}: {message}")]
    Rpc { code: i64, message: String },

    #[error("invalid RPC response: {0}")]
    InvalidResponse(String),

    #[error("attestation serialisation failed: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Plug-in contract for anchoring attestations.
#[async_trait]
pub trait Anchor: Send + Sync {
    /// Anchor one attestation and return a receipt.
    async fn anchor(
        &self,
        attestation: &ParametricInsuranceAttestation,
    ) -> Result<AnchorReceipt, AnchorError>;
}

/// Default anchor: JSON-RPC client against `rope_appendToLedger`.
pub struct JsonRpcAnchor {
    rpc_url: String,
    owner: String,
    client: Client,
    next_id: Arc<AtomicU64>,
}

impl JsonRpcAnchor {
    pub fn new(
        rpc_url: impl Into<String>,
        owner: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, AnchorError> {
        let client = Client::builder()
            .timeout(timeout)
            .user_agent(format!("insurance-agent/{}", crate::VERSION))
            .build()?;
        Ok(Self {
            rpc_url: rpc_url.into(),
            owner: owner.into(),
            client,
            next_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub fn with_client(
        rpc_url: impl Into<String>,
        owner: impl Into<String>,
        client: Client,
    ) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            owner: owner.into(),
            client,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }
}

#[async_trait]
impl Anchor for JsonRpcAnchor {
    async fn anchor(
        &self,
        attestation: &ParametricInsuranceAttestation,
    ) -> Result<AnchorReceipt, AnchorError> {
        let req_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = build_request(req_id, &self.owner, attestation)?;

        tracing::debug!(
            target: "insurance_agent::anchor",
            asset_id = %attestation.asset_id,
            owner = %self.owner,
            digest = %attestation.digest().to_hex(),
            "submitting rope_appendToLedger"
        );

        let resp = self
            .client
            .post(&self.rpc_url)
            .json(&request)
            .send()
            .await?
            .text()
            .await?;

        parse_response(&resp, attestation.digest())
    }
}

/// Build the JSON-RPC request payload. Pulled out so tests can assert on
/// the wire format without needing a server.
pub(crate) fn build_request(
    id: u64,
    owner: &str,
    attestation: &ParametricInsuranceAttestation,
) -> Result<serde_json::Value, AnchorError> {
    let canonical = serde_json::to_value(attestation)?;
    let metadata = flatten_metadata(&canonical);

    let description = format!(
        "ParametricInsuranceAttestation asset={} type={} premium_bps={} coverage_usd={}",
        attestation.asset_id,
        attestation.asset_type,
        attestation.premium_bps,
        attestation.coverage_usd,
    );

    Ok(serde_json::json!({
        "jsonrpc": "2.0",
        "method": "rope_appendToLedger",
        "params": [
            owner,
            {
                "interaction_type": "ParametricInsuranceAttestation",
                "description": description,
                "metadata": metadata,
            }
        ],
        "id": id,
    }))
}

/// `rope_appendToLedger` accepts a `metadata` map of `string -> string`.
/// Flatten the canonical attestation JSON into that shape so we can pass
/// every field through.
fn flatten_metadata(value: &serde_json::Value) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if let Some(obj) = value.as_object() {
        for (k, v) in obj {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            out.insert(k.clone(), serde_json::Value::String(s));
        }
    }
    serde_json::Value::Object(out)
}

pub(crate) fn parse_response(
    body: &str,
    digest: AttestationDigest,
) -> Result<AnchorReceipt, AnchorError> {
    let parsed: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AnchorError::InvalidResponse(format!("decoding body: {e}: {body}")))?;

    if let Some(err) = parsed.get("error") {
        let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-32603);
        let message = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown RPC error")
            .to_string();
        return Err(AnchorError::Rpc { code, message });
    }

    let result = parsed
        .get("result")
        .ok_or_else(|| AnchorError::InvalidResponse("missing 'result' field".into()))?;

    let knot_string_id = result
        .get("hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AnchorError::InvalidResponse("missing 'result.hash'".into()))?
        .to_string();
    let piece_count = result
        .get("index")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| AnchorError::InvalidResponse("missing 'result.index'".into()))?;

    Ok(AnchorReceipt {
        knot_string_id,
        piece_count,
        attestation_digest: digest,
    })
}

/// Test-only mock anchor that records every attestation and returns
/// canned receipts. Counts as part of the public API of the crate at
/// `cfg(test)` so integration tests inside this crate can drive the agent
/// loop without touching the network.
#[cfg(test)]
pub mod mock {
    use super::*;
    use std::sync::Mutex;

    pub struct MockAnchor {
        pub anchored: Mutex<Vec<ParametricInsuranceAttestation>>,
        pub next_seq: AtomicU64,
        pub fail_after: Option<usize>,
    }

    impl Default for MockAnchor {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MockAnchor {
        pub fn new() -> Self {
            Self {
                anchored: Mutex::new(Vec::new()),
                next_seq: AtomicU64::new(1),
                fail_after: None,
            }
        }

        pub fn fail_after(n: usize) -> Self {
            Self {
                anchored: Mutex::new(Vec::new()),
                next_seq: AtomicU64::new(1),
                fail_after: Some(n),
            }
        }

        pub fn count(&self) -> usize {
            self.anchored.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl Anchor for MockAnchor {
        async fn anchor(
            &self,
            attestation: &ParametricInsuranceAttestation,
        ) -> Result<AnchorReceipt, AnchorError> {
            let mut guard = self.anchored.lock().unwrap();
            if let Some(n) = self.fail_after {
                if guard.len() >= n {
                    return Err(AnchorError::Rpc {
                        code: -32603,
                        message: "mock failure".into(),
                    });
                }
            }
            guard.push(attestation.clone());
            let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
            Ok(AnchorReceipt {
                knot_string_id: format!("0x{:064x}", seq),
                piece_count: seq,
                attestation_digest: attestation.digest(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::{AssetSource, TokenizedAsset};
    use crate::risk::RiskModel;

    fn sample_attestation() -> ParametricInsuranceAttestation {
        let asset = TokenizedAsset {
            asset_id: "featured-kibali-gold-mine".into(),
            name: "Kibali".into(),
            asset_type: "GOLD_MINE".into(),
            location: Some("Democratic Republic of Congo".into()),
            valuation_usd: 1_000_000.0,
            is_verified: true,
            chain_id: Some(271828),
            dcnft_addr: Some("0xdcnft".into()),
            erc3643_addr: Some("0xerc3643".into()),
            source: AssetSource::Tanastok,
        };
        let profile = RiskModel::default().evaluate(&asset);
        ParametricInsuranceAttestation::build(&asset, &profile, "InsuranceAgent", 1, 1_000_000)
            .unwrap()
    }

    #[test]
    fn request_payload_has_expected_shape() {
        let att = sample_attestation();
        let req = build_request(7, "0xC003", &att).unwrap();
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["method"], "rope_appendToLedger");
        assert_eq!(req["id"], 7);
        let params = req["params"].as_array().unwrap();
        assert_eq!(params[0], "0xC003");
        assert_eq!(
            params[1]["interaction_type"],
            "ParametricInsuranceAttestation"
        );
        // The metadata map must contain the asset_id.
        let metadata = params[1]["metadata"].as_object().unwrap();
        assert_eq!(metadata["asset_id"], "featured-kibali-gold-mine");
        assert!(metadata.contains_key("premium_bps"));
        assert!(metadata.contains_key("coverage_usd"));
        assert!(metadata.contains_key("triggers"));
    }

    #[test]
    fn parses_success_response() {
        let body = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "index": 42, "hash": "0xdeadbeef" }
        }"#;
        let receipt = parse_response(body, AttestationDigest([0u8; 32])).unwrap();
        assert_eq!(receipt.piece_count, 42);
        assert_eq!(receipt.knot_string_id, "0xdeadbeef");
    }

    #[test]
    fn parses_rpc_error() {
        let body = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": 2002, "message": "No ledger found for this address" }
        }"#;
        match parse_response(body, AttestationDigest([0u8; 32])) {
            Err(AnchorError::Rpc { code: 2002, .. }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_invalid_response() {
        let body = r#"{"jsonrpc":"2.0","id":1}"#;
        match parse_response(body, AttestationDigest([0u8; 32])) {
            Err(AnchorError::InvalidResponse(_)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn mock_anchor_records_attestations() {
        let mock = mock::MockAnchor::new();
        let att = sample_attestation();
        let receipt = mock.anchor(&att).await.unwrap();
        assert_eq!(receipt.piece_count, 1);
        assert_eq!(mock.count(), 1);
        assert_eq!(receipt.attestation_digest, att.digest());
    }

    #[tokio::test]
    async fn mock_anchor_can_fail_after_n() {
        let mock = mock::MockAnchor::fail_after(2);
        let att = sample_attestation();
        assert!(mock.anchor(&att).await.is_ok());
        assert!(mock.anchor(&att).await.is_ok());
        assert!(matches!(
            mock.anchor(&att).await,
            Err(AnchorError::Rpc { .. })
        ));
    }
}
