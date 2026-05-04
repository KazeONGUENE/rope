// =============================================================================
// Anchor — submits sealed `ComplianceTestimonyEnvelope`s to a rope-node
// over `rope_appendToLedger`.
// =============================================================================
//
// Wire contract (mirrors `rope_appendToLedger` in
// `crates/rope-node/src/rpc_server.rs`):
//
//   rope_appendToLedger(
//       wallet_address,
//       {
//           interaction_type: "TestimonySubmission",
//           description: "<short label>",
//           metadata: { "envelope": <json string of envelope>,
//                       "anchor_hash": "0x…",
//                       "testimony_label": "…",
//                       "agent_id": "compliance" }
//       }
//   ) -> { index: u32, hash: "0x…" /* knot_string_id */ }
//
// The returned `hash` IS the canonical `knot_string_id` (canon v1.1
// §6 — stable identifier guarantee). It is the same value that would
// later be passed to `rope_untieKnot` if the testimony itself ever
// needed to be tombstoned (e.g. for a meta-erasure request).
// =============================================================================

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::rpc::{RopeRpcClient, RpcClientError};
use crate::testimony::ComplianceTestimonyEnvelope;

/// Receipt returned by the rope-node for a successful append.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorReceipt {
    pub knot_string_id: String,
    pub piece_count: u32,
    pub anchored_at: i64,
    pub agent_wallet: String,
    pub testimony_label: String,
    pub anchor_hash: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AnchorError {
    #[error("rope-node RPC error {code}: {message}")]
    RpcError { code: i64, message: String },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("invalid append response: {0}")]
    InvalidResponse(String),
    #[error("envelope serialisation failed: {0}")]
    Serialisation(String),
}

impl From<RpcClientError> for AnchorError {
    fn from(e: RpcClientError) -> Self {
        match e {
            RpcClientError::RpcError { code, message } => Self::RpcError { code, message },
            RpcClientError::Transport(s) => Self::Transport(s),
            RpcClientError::InvalidResponse(s) => Self::InvalidResponse(s),
        }
    }
}

/// Thin client over `RopeRpcClient` that knows how to format a
/// `ComplianceTestimonyEnvelope` for `rope_appendToLedger`.
#[derive(Clone)]
pub struct AnchorClient {
    rpc: Arc<dyn RopeRpcClient>,
    agent_wallet: String,
}

impl AnchorClient {
    pub fn new(rpc: Arc<dyn RopeRpcClient>, agent_wallet: impl Into<String>) -> Self {
        Self {
            rpc,
            agent_wallet: agent_wallet.into(),
        }
    }

    pub fn agent_wallet(&self) -> &str {
        &self.agent_wallet
    }

    /// Anchor one envelope. Returns the `AnchorReceipt` on success.
    pub async fn anchor(
        &self,
        envelope: &ComplianceTestimonyEnvelope,
    ) -> Result<AnchorReceipt, AnchorError> {
        let envelope_json = serde_json::to_string(envelope)
            .map_err(|e| AnchorError::Serialisation(e.to_string()))?;
        let metadata = json!({
            "envelope": envelope_json,
            "anchor_hash": envelope.anchor_hash,
            "testimony_label": envelope.testimony_label,
            "agent_id": envelope.agent_id,
        });
        let interaction = json!({
            "interaction_type": "TestimonySubmission",
            "description": envelope.testimony_label,
            "metadata": metadata,
        });
        let params = json!([self.agent_wallet, interaction]);
        let res = self.rpc.call("rope_appendToLedger", params).await?;

        let knot_string_id = res
            .get("hash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AnchorError::InvalidResponse(
                    "rope_appendToLedger response missing `hash` field".to_string(),
                )
            })?
            .to_string();
        let piece_count = res
            .get("index")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(0);

        let now = chrono::Utc::now().timestamp();
        Ok(AnchorReceipt {
            knot_string_id,
            piece_count,
            anchored_at: now,
            agent_wallet: self.agent_wallet.clone(),
            testimony_label: envelope.testimony_label.to_string(),
            anchor_hash: envelope.anchor_hash.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::testing::MockRopeRpcClient;
    use crate::testimony::{ComplianceTestimony, MiFidIIDigest};

    fn envelope() -> ComplianceTestimonyEnvelope {
        let body = ComplianceTestimony::MiFidIIDigest(MiFidIIDigest::build(0, 100, &[]));
        ComplianceTestimonyEnvelope::seal("compliance", "0xC005", body, 1)
    }

    #[tokio::test]
    async fn happy_path_returns_receipt_with_knot_string_id() {
        let mock = Arc::new(MockRopeRpcClient::new());
        mock.enqueue_ok(
            "rope_appendToLedger",
            json!({"index": 1, "hash": "0xdeadbeef".to_string()}),
        );
        let anchor = AnchorClient::new(mock.clone() as Arc<dyn RopeRpcClient>, "0xC005");
        let receipt = anchor.anchor(&envelope()).await.unwrap();
        assert_eq!(receipt.knot_string_id, "0xdeadbeef");
        assert_eq!(receipt.piece_count, 1);
        assert_eq!(receipt.testimony_label, "mifid_ii_digest");

        let calls = mock.calls_for("rope_appendToLedger");
        assert_eq!(calls.len(), 1);
        let arr = calls[0].params.as_array().unwrap();
        assert_eq!(arr[0].as_str().unwrap(), "0xC005");
        let interaction = &arr[1];
        assert_eq!(
            interaction.get("interaction_type").unwrap().as_str().unwrap(),
            "TestimonySubmission"
        );
        let metadata = interaction.get("metadata").unwrap();
        assert!(metadata.get("envelope").unwrap().as_str().unwrap().len() > 0);
        assert_eq!(
            metadata.get("agent_id").unwrap().as_str().unwrap(),
            "compliance"
        );
    }

    #[tokio::test]
    async fn rpc_error_is_propagated() {
        let mock = Arc::new(MockRopeRpcClient::new());
        mock.enqueue_err(
            "rope_appendToLedger",
            RpcClientError::RpcError {
                code: 2002,
                message: "No ledger found for this address".to_string(),
            },
        );
        let anchor = AnchorClient::new(mock as Arc<dyn RopeRpcClient>, "0xC005");
        let err = anchor.anchor(&envelope()).await.expect_err("must fail");
        match err {
            AnchorError::RpcError { code, .. } => assert_eq!(code, 2002),
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[tokio::test]
    async fn missing_hash_field_is_invalid_response() {
        let mock = Arc::new(MockRopeRpcClient::new());
        mock.enqueue_ok("rope_appendToLedger", json!({"index": 1}));
        let anchor = AnchorClient::new(mock as Arc<dyn RopeRpcClient>, "0xC005");
        let err = anchor.anchor(&envelope()).await.expect_err("must fail");
        assert!(matches!(err, AnchorError::InvalidResponse(_)));
    }
}
