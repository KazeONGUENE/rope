// =============================================================================
// GDPR Article 17 — request types + structural validator
// =============================================================================
//
// Article 17 of the EU GDPR ("Right to erasure / Right to be forgotten")
// is the regulatory primitive that the Datachain Rope `rope_untieKnot`
// RPC was designed to satisfy. The flow is:
//
//   1. A subject (or their authorised data-protection officer) submits
//      an erasure request to the ComplianceAgent over HTTP.
//   2. The agent runs the structural validator in this module.
//   3. If approved, the orchestrator (`crate::orchestrator`) calls
//      `rope_untieKnot` once per affected knot and collects the
//      tombstone audit hashes.
//   4. The agent emits a signed `ComplianceTestimony::GdprArticle17`
//      knot via `rope_appendToLedger`, capturing the audit trail.
//   5. The verdict and resulting receipt are returned to the caller in
//      the HTTP response.
//
// HONEST SCOPE STATEMENT
// ----------------------
// The validator implemented here is a SCHEMA + WHITELIST validator. It
// rejects requests that are obviously malformed or that fall outside
// the configured policy envelope. It does NOT decide the underlying
// legal question of whether erasure is required (that decision lives
// with the operator's compliance team and any upstream KYC pipeline).
// Production deployments should treat an `Approved` verdict as
// "structurally well-formed and inside the configured policy" rather
// than as a court-grade legal ruling.
// =============================================================================

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::GdprPolicy;

/// The legal basis the requestor invokes to demand erasure.
///
/// Mirrors the six lawful grounds enumerated in GDPR Art. 17(1)(a-f).
/// We intentionally keep the enum small and human-readable so it can
/// be embedded verbatim in the testimony knot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JustificationClass {
    /// (1)(a) — data no longer necessary for the original purpose.
    NoLongerNecessary,
    /// (1)(b) — consent withdrawn.
    ConsentWithdrawn,
    /// (1)(c) — subject objects under Art. 21.
    ObjectionToProcessing,
    /// (1)(d) — data was unlawfully processed.
    UnlawfulProcessing,
    /// (1)(e) — erasure required by EU or member-state law.
    LegalObligation,
    /// (1)(f) — processed in the context of information-society
    /// services offered to a child.
    ChildProtection,
}

impl JustificationClass {
    /// Human-readable label used in tracing logs and testimony payloads.
    pub fn label(&self) -> &'static str {
        match self {
            Self::NoLongerNecessary => "Art. 17(1)(a) — no longer necessary",
            Self::ConsentWithdrawn => "Art. 17(1)(b) — consent withdrawn",
            Self::ObjectionToProcessing => "Art. 17(1)(c) — objection to processing",
            Self::UnlawfulProcessing => "Art. 17(1)(d) — unlawful processing",
            Self::LegalObligation => "Art. 17(1)(e) — legal obligation",
            Self::ChildProtection => "Art. 17(1)(f) — child protection",
        }
    }
}

/// A GDPR Art. 17 erasure request as it arrives over HTTP.
///
/// `request_id` is generated server-side if omitted (so dApps can be
/// lazy). `submitted_at` is overwritten on receipt to the agent clock
/// to avoid clock-skew attacks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Article17Request {
    /// Unique id of the request; auto-assigned if missing in the wire form.
    #[serde(default = "default_request_id")]
    pub request_id: String,

    /// Wallet whose knots are to be untied (0x-prefixed hex address).
    pub subject_wallet: String,

    /// Hex-encoded proof-of-control over `subject_wallet` (or a DPO
    /// authorisation token in the multi-party case). PHASE 1 checks
    /// presence + length; Phase 2 will verify the signature.
    pub requestor_proof: String,

    /// One of the lawful grounds in `JustificationClass`.
    pub justification: JustificationClass,

    /// 0x-prefixed hex `knot_string_id`s to untie. The orchestrator
    /// will iterate this list and call `rope_untieKnot` once per id.
    pub affected_knots: Vec<String>,

    /// ISO-3166 numeric country code of the subject.
    pub jurisdiction: u16,

    /// Free-form note kept verbatim in the testimony knot (max 1KB).
    #[serde(default)]
    pub note: String,

    /// Unix timestamp of receipt. Overwritten by the validator.
    #[serde(default)]
    pub submitted_at: i64,
}

fn default_request_id() -> String {
    Uuid::new_v4().to_string()
}

/// Outcome of the structural validator. Either the request is approved
/// for orchestration, or it is rejected with a stable, machine-readable
/// reason code (so callers and the testimony anchor can branch on it).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Article17Verdict {
    Approved {
        request_id: String,
        validated_at: i64,
        normalized_subject_wallet: String,
        normalized_knot_ids: Vec<String>,
    },
    Rejected {
        request_id: String,
        validated_at: i64,
        reason_code: RejectionReason,
        message: String,
    },
}

impl Article17Verdict {
    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved { .. })
    }

    pub fn request_id(&self) -> &str {
        match self {
            Self::Approved { request_id, .. } | Self::Rejected { request_id, .. } => request_id,
        }
    }
}

/// Stable reason codes for rejection. Designed for downstream
/// dashboards that want to chart rejection causes without parsing
/// human-readable strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    SchemaInvalid,
    ProofMissing,
    ProofTooShort,
    ProofMalformed,
    JurisdictionNotAllowed,
    NoKnotsRequested,
    TooManyKnots,
    KnotIdMalformed,
    SubjectWalletMalformed,
}

/// Structural / policy validator for `Article17Request`.
///
/// This struct is cheap to clone and is held by the HTTP handler. It
/// is stateless — every call is pure — which keeps the test surface
/// small.
#[derive(Debug, Clone)]
pub struct Article17Validator {
    policy: GdprPolicy,
}

impl Article17Validator {
    pub fn new(policy: GdprPolicy) -> Self {
        Self { policy }
    }

    pub fn policy(&self) -> &GdprPolicy {
        &self.policy
    }

    /// Run the validator. Mutates `req.submitted_at` to the validator
    /// clock and `req.request_id` to a UUID if missing.
    pub fn validate(&self, req: &mut Article17Request) -> Article17Verdict {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        req.submitted_at = now;
        if req.request_id.is_empty() {
            req.request_id = default_request_id();
        }

        // ---- subject_wallet ----
        let normalised_wallet = match normalise_address(&req.subject_wallet) {
            Some(w) => w,
            None => {
                return self.reject(
                    req,
                    now,
                    RejectionReason::SubjectWalletMalformed,
                    "subject_wallet must be 0x-prefixed 20-byte hex",
                );
            }
        };

        // ---- requestor_proof ----
        let proof_bytes = if req.requestor_proof.is_empty() {
            if self.policy.require_requestor_proof {
                return self.reject(
                    req,
                    now,
                    RejectionReason::ProofMissing,
                    "requestor_proof is required by policy",
                );
            }
            Vec::new()
        } else {
            match decode_hex(&req.requestor_proof) {
                Some(b) => b,
                None => {
                    return self.reject(
                        req,
                        now,
                        RejectionReason::ProofMalformed,
                        "requestor_proof is not valid hex",
                    );
                }
            }
        };
        if self.policy.require_requestor_proof && proof_bytes.len() < self.policy.min_proof_bytes {
            return self.reject(
                req,
                now,
                RejectionReason::ProofTooShort,
                &format!(
                    "requestor_proof shorter than policy minimum ({} < {} bytes)",
                    proof_bytes.len(),
                    self.policy.min_proof_bytes
                ),
            );
        }

        // ---- jurisdiction ----
        if !self.policy.allowed_jurisdictions.is_empty()
            && !self.policy.allowed_jurisdictions.contains(&req.jurisdiction)
        {
            return self.reject(
                req,
                now,
                RejectionReason::JurisdictionNotAllowed,
                &format!(
                    "jurisdiction {} not in configured allowlist",
                    req.jurisdiction
                ),
            );
        }

        // ---- affected_knots ----
        if req.affected_knots.is_empty() {
            return self.reject(
                req,
                now,
                RejectionReason::NoKnotsRequested,
                "affected_knots must contain at least one knot id",
            );
        }
        if req.affected_knots.len() > self.policy.max_knots_per_request {
            return self.reject(
                req,
                now,
                RejectionReason::TooManyKnots,
                &format!(
                    "affected_knots length {} exceeds policy max {}",
                    req.affected_knots.len(),
                    self.policy.max_knots_per_request
                ),
            );
        }

        let mut normalised_knots = Vec::with_capacity(req.affected_knots.len());
        for raw in &req.affected_knots {
            match normalise_knot_id(raw) {
                Some(id) => normalised_knots.push(id),
                None => {
                    return self.reject(
                        req,
                        now,
                        RejectionReason::KnotIdMalformed,
                        &format!("affected_knots entry {:?} is not 32-byte hex", raw),
                    );
                }
            }
        }

        Article17Verdict::Approved {
            request_id: req.request_id.clone(),
            validated_at: now,
            normalized_subject_wallet: normalised_wallet,
            normalized_knot_ids: normalised_knots,
        }
    }

    fn reject(
        &self,
        req: &Article17Request,
        now: i64,
        reason_code: RejectionReason,
        message: &str,
    ) -> Article17Verdict {
        Article17Verdict::Rejected {
            request_id: req.request_id.clone(),
            validated_at: now,
            reason_code,
            message: message.to_string(),
        }
    }
}

/// Lower-case 0x-prefixed normaliser for an EVM address. Returns
/// `None` if the value is not a 20-byte hex string.
pub fn normalise_address(raw: &str) -> Option<String> {
    let stripped = raw.trim().trim_start_matches("0x").trim_start_matches("0X");
    let bytes = decode_hex(stripped)?;
    if bytes.len() != 20 {
        return None;
    }
    Some(format!("0x{}", hex::encode(bytes)))
}

/// Lower-case 0x-prefixed normaliser for a 32-byte knot StringId.
pub fn normalise_knot_id(raw: &str) -> Option<String> {
    let stripped = raw.trim().trim_start_matches("0x").trim_start_matches("0X");
    let bytes = decode_hex(stripped)?;
    if bytes.len() != 32 {
        return None;
    }
    Some(format!("0x{}", hex::encode(bytes)))
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let stripped = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    if stripped.is_empty() {
        return Some(Vec::new());
    }
    hex::decode(stripped).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approving_policy() -> GdprPolicy {
        GdprPolicy {
            allowed_jurisdictions: Default::default(), // empty = accept any
            require_requestor_proof: true,
            min_proof_bytes: 32,
            max_knots_per_request: 16,
        }
    }

    fn good_request() -> Article17Request {
        Article17Request {
            request_id: "req-1".to_string(),
            subject_wallet: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            requestor_proof: format!("0x{}", "ab".repeat(32)),
            justification: JustificationClass::ConsentWithdrawn,
            affected_knots: vec![
                format!("0x{}", "11".repeat(32)),
                format!("0x{}", "22".repeat(32)),
            ],
            jurisdiction: 250,
            note: "test".to_string(),
            submitted_at: 0,
        }
    }

    #[test]
    fn approves_well_formed_request() {
        let v = Article17Validator::new(approving_policy());
        let mut req = good_request();
        let verdict = v.validate(&mut req);
        assert!(verdict.is_approved(), "verdict = {:?}", verdict);
        if let Article17Verdict::Approved {
            normalized_subject_wallet,
            normalized_knot_ids,
            ..
        } = verdict
        {
            assert!(normalized_subject_wallet.starts_with("0x"));
            assert_eq!(normalized_knot_ids.len(), 2);
        }
    }

    #[test]
    fn rejects_short_proof() {
        let v = Article17Validator::new(approving_policy());
        let mut req = good_request();
        req.requestor_proof = "0xabab".to_string();
        let verdict = v.validate(&mut req);
        match verdict {
            Article17Verdict::Rejected { reason_code, .. } => {
                assert_eq!(reason_code, RejectionReason::ProofTooShort);
            }
            other => panic!("expected Rejected ProofTooShort, got {:?}", other),
        }
    }

    #[test]
    fn rejects_missing_proof_when_required() {
        let v = Article17Validator::new(approving_policy());
        let mut req = good_request();
        req.requestor_proof.clear();
        let verdict = v.validate(&mut req);
        match verdict {
            Article17Verdict::Rejected { reason_code, .. } => {
                assert_eq!(reason_code, RejectionReason::ProofMissing);
            }
            other => panic!("expected ProofMissing, got {:?}", other),
        }
    }

    #[test]
    fn allows_missing_proof_when_policy_permits() {
        let mut policy = approving_policy();
        policy.require_requestor_proof = false;
        let v = Article17Validator::new(policy);
        let mut req = good_request();
        req.requestor_proof.clear();
        let verdict = v.validate(&mut req);
        assert!(verdict.is_approved());
    }

    #[test]
    fn rejects_disallowed_jurisdiction() {
        let mut policy = approving_policy();
        policy.allowed_jurisdictions.insert(250);
        let v = Article17Validator::new(policy);
        let mut req = good_request();
        req.jurisdiction = 408; // DPRK
        let verdict = v.validate(&mut req);
        match verdict {
            Article17Verdict::Rejected { reason_code, .. } => {
                assert_eq!(reason_code, RejectionReason::JurisdictionNotAllowed);
            }
            other => panic!("expected JurisdictionNotAllowed, got {:?}", other),
        }
    }

    #[test]
    fn rejects_empty_knots_list() {
        let v = Article17Validator::new(approving_policy());
        let mut req = good_request();
        req.affected_knots.clear();
        let verdict = v.validate(&mut req);
        match verdict {
            Article17Verdict::Rejected { reason_code, .. } => {
                assert_eq!(reason_code, RejectionReason::NoKnotsRequested);
            }
            other => panic!("expected NoKnotsRequested, got {:?}", other),
        }
    }

    #[test]
    fn rejects_too_many_knots() {
        let mut policy = approving_policy();
        policy.max_knots_per_request = 1;
        let v = Article17Validator::new(policy);
        let mut req = good_request();
        let verdict = v.validate(&mut req);
        match verdict {
            Article17Verdict::Rejected { reason_code, .. } => {
                assert_eq!(reason_code, RejectionReason::TooManyKnots);
            }
            other => panic!("expected TooManyKnots, got {:?}", other),
        }
    }

    #[test]
    fn rejects_malformed_subject_wallet() {
        let v = Article17Validator::new(approving_policy());
        let mut req = good_request();
        req.subject_wallet = "0x1234".to_string();
        let verdict = v.validate(&mut req);
        match verdict {
            Article17Verdict::Rejected { reason_code, .. } => {
                assert_eq!(reason_code, RejectionReason::SubjectWalletMalformed);
            }
            other => panic!("expected SubjectWalletMalformed, got {:?}", other),
        }
    }

    #[test]
    fn rejects_malformed_knot_id() {
        let v = Article17Validator::new(approving_policy());
        let mut req = good_request();
        req.affected_knots.push("not-hex".to_string());
        let verdict = v.validate(&mut req);
        match verdict {
            Article17Verdict::Rejected { reason_code, .. } => {
                assert_eq!(reason_code, RejectionReason::KnotIdMalformed);
            }
            other => panic!("expected KnotIdMalformed, got {:?}", other),
        }
    }

    #[test]
    fn assigns_request_id_when_blank() {
        let v = Article17Validator::new(approving_policy());
        let mut req = good_request();
        req.request_id.clear();
        let verdict = v.validate(&mut req);
        // request_id should now be a UUID (36 chars, with dashes)
        assert_eq!(req.request_id.len(), 36);
        assert!(verdict.is_approved());
    }

    #[test]
    fn stamps_submitted_at_with_validator_clock() {
        let v = Article17Validator::new(approving_policy());
        let mut req = good_request();
        req.submitted_at = 1; // anything; will be overwritten
        let _ = v.validate(&mut req);
        assert!(req.submitted_at > 1_000_000_000);
    }

    #[test]
    fn justification_class_label_is_stable() {
        assert_eq!(
            JustificationClass::ConsentWithdrawn.label(),
            "Art. 17(1)(b) — consent withdrawn"
        );
    }
}
