// =============================================================================
// Orchestrator — coordinates `rope_untieKnot` calls for an approved
// GDPR Art. 17 erasure request and captures the per-knot tombstone
// audit hashes for downstream testimony anchoring.
// =============================================================================
//
// Wire contract:
//
//   rope_untieKnot(wallet_address, knot_string_id, reason)
//       -> {
//           wallet_address: "0x…",
//           knot_string_id: "0x…",
//           tombstone_audit_hash: "0x…",
//           untied_at: i64 (unix seconds),
//           reason: "GdprArticle17",
//           knots_remaining: u64,
//           tombstones_total: u64,
//           gdpr_article: "Article 17 — …",
//           canon: "v1.1 §4.2 — …",
//           scope: "single_knot",
//           auth_method: "phase-1-trusted-proxy"
//       }
//
// Source-of-truth: `crates/rope-node/src/rpc_server.rs` (handler block
// at `"rope_untieKnot" =>`) and `LedgerManager::untie_knot`.
//
// Behaviour:
//
//   * Iterate the request's knot list and call `rope_untieKnot` once
//     per knot. We deliberately do NOT batch — the rope-node RPC has
//     no batched form, and serial calls let us record an individual
//     outcome (and tombstone hash) per knot.
//   * A failure on one knot does not abort the entire request — the
//     remaining knots are still attempted, and the caller receives a
//     per-knot outcome list. This matches the expected "best-effort
//     erasure with full audit" posture of GDPR.
//   * The per-call reason is the lower-camel-case justification class
//     of the request, prefixed with `GdprArticle17/`. The rope-node
//     stores this verbatim in the tombstone metadata.
// =============================================================================

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::gdpr::{Article17Request, Article17Verdict, JustificationClass};
use crate::rpc::{RopeRpcClient, RpcClientError};

/// Outcome of one `rope_untieKnot` invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TombstoneOutcome {
    pub knot_string_id: String,
    pub success: bool,
    /// Present iff `success`. The hex-encoded BLAKE3 commitment over
    /// (string_id || untied_at || reason) returned by the rope-node.
    pub tombstone_audit_hash: Option<String>,
    /// Present iff `success`. Unix seconds when the rope-node recorded
    /// the tombstone.
    pub untied_at: Option<i64>,
    /// Present iff `success`. After-untying counters from the node.
    pub knots_remaining: Option<u64>,
    pub tombstones_total: Option<u64>,
    /// Present iff `!success`. Stable error code from the rope-node
    /// JSON-RPC layer (e.g. 2010 = genesis-knot rejection, 2011 =
    /// knot does not belong to wallet, 2012 = already untied).
    pub error_code: Option<i64>,
    pub error_message: Option<String>,
}

/// Aggregated result over all knots in a single Art. 17 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationReport {
    pub request_id: String,
    pub subject_wallet: String,
    pub total_knots: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub outcomes: Vec<TombstoneOutcome>,
    /// All `tombstone_audit_hash` values from successful untyings, in
    /// arrival order. Convenience field for the testimony builder.
    pub tombstone_audit_hashes: Vec<String>,
}

/// Coordinator that turns one approved verdict into N `rope_untieKnot`
/// calls.
pub struct UntieOrchestrator {
    rpc: Arc<dyn RopeRpcClient>,
}

impl UntieOrchestrator {
    pub fn new(rpc: Arc<dyn RopeRpcClient>) -> Self {
        Self { rpc }
    }

    /// Execute the verdict against the rope-node. The verdict MUST be
    /// `Approved` — Rejected verdicts are rejected at the type level
    /// by the caller (we still defensive-check at runtime).
    pub async fn execute(
        &self,
        request: &Article17Request,
        verdict: &Article17Verdict,
    ) -> Result<OrchestrationReport, OrchestrationError> {
        let (subject_wallet, knot_ids, request_id) = match verdict {
            Article17Verdict::Approved {
                request_id,
                normalized_subject_wallet,
                normalized_knot_ids,
                ..
            } => (
                normalized_subject_wallet.clone(),
                normalized_knot_ids.clone(),
                request_id.clone(),
            ),
            Article17Verdict::Rejected { .. } => {
                return Err(OrchestrationError::VerdictNotApproved);
            }
        };

        let reason = canonical_reason(&request.justification);
        let mut outcomes = Vec::with_capacity(knot_ids.len());
        let mut audit_hashes = Vec::new();
        let mut success_count = 0usize;
        let mut failure_count = 0usize;

        for knot_id in &knot_ids {
            let params = json!([subject_wallet, knot_id, reason]);
            let result = self.rpc.call("rope_untieKnot", params).await;
            let outcome = build_outcome(knot_id, result);
            if outcome.success {
                success_count += 1;
                if let Some(h) = &outcome.tombstone_audit_hash {
                    audit_hashes.push(h.clone());
                }
                tracing::info!(
                    target: "compliance::orchestrator",
                    request_id = %request_id,
                    knot_id = %knot_id,
                    audit_hash = ?outcome.tombstone_audit_hash,
                    "rope_untieKnot succeeded"
                );
            } else {
                failure_count += 1;
                tracing::warn!(
                    target: "compliance::orchestrator",
                    request_id = %request_id,
                    knot_id = %knot_id,
                    error_code = ?outcome.error_code,
                    error = ?outcome.error_message,
                    "rope_untieKnot failed"
                );
            }
            outcomes.push(outcome);
        }

        Ok(OrchestrationReport {
            request_id,
            subject_wallet,
            total_knots: knot_ids.len(),
            success_count,
            failure_count,
            outcomes,
            tombstone_audit_hashes: audit_hashes,
        })
    }
}

/// Errors at the orchestration level (i.e. before any RPC is made).
#[derive(Debug, thiserror::Error)]
pub enum OrchestrationError {
    #[error("verdict is not Approved; orchestration refused")]
    VerdictNotApproved,
}

fn canonical_reason(j: &JustificationClass) -> String {
    let suffix = match j {
        JustificationClass::NoLongerNecessary => "NoLongerNecessary",
        JustificationClass::ConsentWithdrawn => "ConsentWithdrawn",
        JustificationClass::ObjectionToProcessing => "ObjectionToProcessing",
        JustificationClass::UnlawfulProcessing => "UnlawfulProcessing",
        JustificationClass::LegalObligation => "LegalObligation",
        JustificationClass::ChildProtection => "ChildProtection",
    };
    format!("GdprArticle17/{}", suffix)
}

fn build_outcome(
    knot_id: &str,
    result: Result<serde_json::Value, RpcClientError>,
) -> TombstoneOutcome {
    match result {
        Ok(v) => {
            let audit_hash = v
                .get("tombstone_audit_hash")
                .and_then(|h| h.as_str())
                .map(|s| s.to_string());
            // We only consider it a success if we actually got a
            // tombstone hash back. Some rope-node code paths return a
            // null `result` on partial errors that did not surface
            // through the JSON-RPC `error` field.
            if audit_hash.is_some() {
                TombstoneOutcome {
                    knot_string_id: knot_id.to_string(),
                    success: true,
                    tombstone_audit_hash: audit_hash,
                    untied_at: v.get("untied_at").and_then(|t| t.as_i64()),
                    knots_remaining: v.get("knots_remaining").and_then(|t| t.as_u64()),
                    tombstones_total: v.get("tombstones_total").and_then(|t| t.as_u64()),
                    error_code: None,
                    error_message: None,
                }
            } else {
                TombstoneOutcome {
                    knot_string_id: knot_id.to_string(),
                    success: false,
                    tombstone_audit_hash: None,
                    untied_at: None,
                    knots_remaining: None,
                    tombstones_total: None,
                    error_code: None,
                    error_message: Some(
                        "rope-node returned null result without tombstone audit hash".to_string(),
                    ),
                }
            }
        }
        Err(RpcClientError::RpcError { code, message }) => TombstoneOutcome {
            knot_string_id: knot_id.to_string(),
            success: false,
            tombstone_audit_hash: None,
            untied_at: None,
            knots_remaining: None,
            tombstones_total: None,
            error_code: Some(code),
            error_message: Some(message),
        },
        Err(other) => TombstoneOutcome {
            knot_string_id: knot_id.to_string(),
            success: false,
            tombstone_audit_hash: None,
            untied_at: None,
            knots_remaining: None,
            tombstones_total: None,
            error_code: None,
            error_message: Some(other.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GdprPolicy;
    use crate::gdpr::Article17Validator;
    use crate::rpc::testing::MockRopeRpcClient;

    fn approving_policy() -> GdprPolicy {
        GdprPolicy {
            allowed_jurisdictions: Default::default(),
            require_requestor_proof: true,
            min_proof_bytes: 32,
            max_knots_per_request: 16,
        }
    }

    fn good_request() -> Article17Request {
        Article17Request {
            request_id: "req-orch-1".to_string(),
            subject_wallet: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            requestor_proof: format!("0x{}", "ab".repeat(32)),
            justification: JustificationClass::ConsentWithdrawn,
            affected_knots: vec![
                format!("0x{}", "11".repeat(32)),
                format!("0x{}", "22".repeat(32)),
            ],
            jurisdiction: 250,
            note: String::new(),
            submitted_at: 0,
        }
    }

    #[tokio::test]
    async fn happy_path_collects_two_audit_hashes() {
        let mock = Arc::new(MockRopeRpcClient::new());
        mock.enqueue_ok(
            "rope_untieKnot",
            json!({
                "tombstone_audit_hash": "0xaaaa",
                "untied_at": 1700000000i64,
                "knots_remaining": 9u64,
                "tombstones_total": 1u64,
            }),
        );
        mock.enqueue_ok(
            "rope_untieKnot",
            json!({
                "tombstone_audit_hash": "0xbbbb",
                "untied_at": 1700000001i64,
                "knots_remaining": 8u64,
                "tombstones_total": 2u64,
            }),
        );

        let v = Article17Validator::new(approving_policy());
        let mut req = good_request();
        let verdict = v.validate(&mut req);
        assert!(verdict.is_approved());

        let orchestrator = UntieOrchestrator::new(mock.clone() as Arc<dyn RopeRpcClient>);
        let report = orchestrator.execute(&req, &verdict).await.unwrap();

        assert_eq!(report.total_knots, 2);
        assert_eq!(report.success_count, 2);
        assert_eq!(report.failure_count, 0);
        assert_eq!(report.tombstone_audit_hashes.len(), 2);
        assert_eq!(report.tombstone_audit_hashes[0], "0xaaaa");
        assert_eq!(report.tombstone_audit_hashes[1], "0xbbbb");

        // Verify the wire shape: subject wallet, knot id, reason "GdprArticle17/ConsentWithdrawn".
        let calls = mock.calls_for("rope_untieKnot");
        assert_eq!(calls.len(), 2);
        for c in &calls {
            let arr = c.params.as_array().expect("array params");
            assert!(arr[0]
                .as_str()
                .unwrap()
                .starts_with("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
            assert_eq!(arr[2].as_str().unwrap(), "GdprArticle17/ConsentWithdrawn");
        }
    }

    #[tokio::test]
    async fn failure_on_one_knot_does_not_abort_others() {
        let mock = Arc::new(MockRopeRpcClient::new());
        // First knot — rope-node rejects (knot does not belong to wallet, code 2011)
        mock.enqueue_err(
            "rope_untieKnot",
            RpcClientError::RpcError {
                code: 2011,
                message: "Knot 0x… does not belong to wallet".to_string(),
            },
        );
        // Second knot — succeeds.
        mock.enqueue_ok(
            "rope_untieKnot",
            json!({
                "tombstone_audit_hash": "0xbbbb",
                "untied_at": 1700000001i64,
                "knots_remaining": 8u64,
                "tombstones_total": 1u64,
            }),
        );

        let v = Article17Validator::new(approving_policy());
        let mut req = good_request();
        let verdict = v.validate(&mut req);
        let orchestrator = UntieOrchestrator::new(mock as Arc<dyn RopeRpcClient>);
        let report = orchestrator.execute(&req, &verdict).await.unwrap();

        assert_eq!(report.success_count, 1);
        assert_eq!(report.failure_count, 1);
        assert_eq!(report.tombstone_audit_hashes, vec!["0xbbbb".to_string()]);
        assert_eq!(report.outcomes[0].error_code, Some(2011));
        assert!(!report.outcomes[0].success);
        assert!(report.outcomes[1].success);
    }

    #[tokio::test]
    async fn refuses_to_execute_a_rejected_verdict() {
        let mock = Arc::new(MockRopeRpcClient::new());
        let req = good_request();
        let verdict = Article17Verdict::Rejected {
            request_id: "x".to_string(),
            validated_at: 0,
            reason_code: crate::gdpr::RejectionReason::ProofMissing,
            message: "no proof".into(),
        };
        let orch = UntieOrchestrator::new(mock as Arc<dyn RopeRpcClient>);
        let err = orch.execute(&req, &verdict).await.expect_err("must fail");
        assert!(matches!(err, OrchestrationError::VerdictNotApproved));
    }

    #[tokio::test]
    async fn null_result_treated_as_failure() {
        let mock = Arc::new(MockRopeRpcClient::new());
        // Nothing enqueued -> mock returns Value::Null.
        let v = Article17Validator::new(approving_policy());
        let mut req = good_request();
        req.affected_knots = vec![format!("0x{}", "11".repeat(32))];
        let verdict = v.validate(&mut req);
        let orchestrator = UntieOrchestrator::new(mock as Arc<dyn RopeRpcClient>);
        let report = orchestrator.execute(&req, &verdict).await.unwrap();
        assert_eq!(report.success_count, 0);
        assert_eq!(report.failure_count, 1);
        assert!(report.outcomes[0]
            .error_message
            .as_ref()
            .unwrap()
            .contains("null result"));
    }

    #[test]
    fn canonical_reason_format() {
        assert_eq!(
            canonical_reason(&JustificationClass::ConsentWithdrawn),
            "GdprArticle17/ConsentWithdrawn"
        );
        assert_eq!(
            canonical_reason(&JustificationClass::LegalObligation),
            "GdprArticle17/LegalObligation"
        );
    }
}
