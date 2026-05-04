// =============================================================================
// ComplianceTestimony — the canonical envelopes that get anchored on
// the ComplianceAgent's wallet string via `rope_appendToLedger`.
// =============================================================================
//
// Three flavours are defined here, mirroring the canonical agent
// description in `rope-explorer::canonical_ai_agents()` (id =
// "compliance"):
//
//   * `GdprArticle17` — emitted once per processed Art. 17 request.
//     Captures the verdict, the per-knot tombstone audit hashes
//     returned by `rope_untieKnot`, and the validator signature.
//   * `MiFidIIDigest` — emitted by the periodic reporter every
//     `reporting_interval`. Aggregates trade events submitted to the
//     agent into per-instrument counts and notional volume buckets.
//   * `DoraIncidentDigest` — emitted alongside the MiFID digest.
//     Aggregates DORA "ICT-related incident" reports into severity
//     buckets.
//
// All three serialise to JSON and are wrapped in a
// `ComplianceTestimonyEnvelope` before being anchored. The envelope
// carries a cheap content commitment (`anchor_hash`, BLAKE3 over the
// serialised payload) so DCScan and any downstream auditor can spot
// tampering at a glance.
// =============================================================================

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::gdpr::{Article17Request, Article17Verdict, JustificationClass};
use crate::orchestrator::{OrchestrationReport, TombstoneOutcome};

/// Top-level testimony content. Tag is `kind` to give DCScan a stable
/// discriminator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComplianceTestimony {
    GdprArticle17(GdprArticle17Testimony),
    MiFidIIDigest(MiFidIIDigest),
    DoraIncidentDigest(DoraIncidentDigest),
}

impl ComplianceTestimony {
    /// Stable short label used in tracing and metrics.
    pub fn label(&self) -> &'static str {
        match self {
            Self::GdprArticle17(_) => "gdpr_article_17",
            Self::MiFidIIDigest(_) => "mifid_ii_digest",
            Self::DoraIncidentDigest(_) => "dora_incident_digest",
        }
    }
}

// ---------------------------------------------------------------------------
// GDPR Article 17 testimony
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprArticle17Testimony {
    pub request_id: String,
    pub subject_wallet: String,
    pub jurisdiction: u16,
    pub justification: JustificationClass,
    pub justification_label: String,
    /// Echo of the verdict (Approved or Rejected) for audit replay.
    pub verdict: Article17Verdict,
    /// Total knots requested in the original payload.
    pub requested_knot_count: usize,
    /// Per-knot tombstone outcomes returned by the orchestrator.
    pub tombstone_outcomes: Vec<TombstoneOutcome>,
    /// Convenience flat list of successful audit hashes.
    pub tombstone_audit_hashes: Vec<String>,
    /// Validator that signed off on the request (the agent itself in
    /// the single-validator case; multiple in a multi-sig deployment).
    pub validator: String,
    /// Trimmed copy of the requestor note so the testimony stays human
    /// readable in DCScan without dumping an arbitrary blob.
    pub note: String,
    pub processed_at: i64,
}

impl GdprArticle17Testimony {
    /// Build a testimony from an approved request and its orchestration
    /// report.
    pub fn from_processed(
        request: &Article17Request,
        verdict: &Article17Verdict,
        report: &OrchestrationReport,
        validator_label: &str,
        processed_at: i64,
    ) -> Self {
        Self {
            request_id: report.request_id.clone(),
            subject_wallet: report.subject_wallet.clone(),
            jurisdiction: request.jurisdiction,
            justification: request.justification.clone(),
            justification_label: request.justification.label().to_string(),
            verdict: verdict.clone(),
            requested_knot_count: request.affected_knots.len(),
            tombstone_outcomes: report.outcomes.clone(),
            tombstone_audit_hashes: report.tombstone_audit_hashes.clone(),
            validator: validator_label.to_string(),
            note: trim_note(&request.note),
            processed_at,
        }
    }

    /// Build a testimony for a request that was rejected before any
    /// orchestration happened. We still emit a knot — the audit trail
    /// "ComplianceAgent saw this request and refused it" is itself
    /// regulatory evidence.
    pub fn from_rejected(
        request: &Article17Request,
        verdict: &Article17Verdict,
        validator_label: &str,
        processed_at: i64,
    ) -> Self {
        Self {
            request_id: verdict.request_id().to_string(),
            subject_wallet: request.subject_wallet.clone(),
            jurisdiction: request.jurisdiction,
            justification: request.justification.clone(),
            justification_label: request.justification.label().to_string(),
            verdict: verdict.clone(),
            requested_knot_count: request.affected_knots.len(),
            tombstone_outcomes: Vec::new(),
            tombstone_audit_hashes: Vec::new(),
            validator: validator_label.to_string(),
            note: trim_note(&request.note),
            processed_at,
        }
    }
}

fn trim_note(s: &str) -> String {
    if s.len() <= 1024 {
        s.to_string()
    } else {
        format!("{}…(truncated)", &s[..1024])
    }
}

// ---------------------------------------------------------------------------
// MiFID II digest
// ---------------------------------------------------------------------------

/// One trade event the agent has observed (or that has been reported
/// to it via `POST /v1/mifid/event`). The fields here are a simplified
/// projection of the MiFID II RTS 22 transaction-report schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiFidIIEvent {
    pub trade_id: String,
    /// ISIN or contract address of the traded instrument.
    pub instrument: String,
    /// Trade venue identifier (MIC for off-chain venues, contract
    /// address for DCSwap pools, etc.).
    pub venue: String,
    pub buyer: String,
    pub seller: String,
    /// Notional value in smallest unit; aggregated and reported as a
    /// single sum per instrument in the digest.
    pub notional: u128,
    pub currency: String,
    pub executed_at: i64,
}

/// Per-instrument bucket inside a MiFID II digest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MiFidInstrumentBucket {
    pub trade_count: u64,
    pub total_notional: u128,
    pub venues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiFidIIDigest {
    pub period_start: i64,
    pub period_end: i64,
    pub total_trades: u64,
    pub total_notional: u128,
    pub by_instrument: BTreeMap<String, MiFidInstrumentBucket>,
    /// Sample trade ids for spot-check auditing (capped at 16 to keep
    /// the testimony small).
    pub sample_trade_ids: Vec<String>,
}

impl MiFidIIDigest {
    /// Aggregate a list of events into a digest. The ordering of
    /// `events` does not affect the resulting digest (modulo the
    /// `sample_trade_ids` order, which preserves arrival order).
    pub fn build(period_start: i64, period_end: i64, events: &[MiFidIIEvent]) -> Self {
        let mut by_instrument: BTreeMap<String, MiFidInstrumentBucket> = BTreeMap::new();
        let mut total_notional: u128 = 0;
        let mut sample_trade_ids = Vec::new();
        for ev in events {
            let bucket = by_instrument
                .entry(ev.instrument.clone())
                .or_default();
            bucket.trade_count += 1;
            bucket.total_notional = bucket.total_notional.saturating_add(ev.notional);
            if !bucket.venues.contains(&ev.venue) {
                bucket.venues.push(ev.venue.clone());
            }
            total_notional = total_notional.saturating_add(ev.notional);
            if sample_trade_ids.len() < 16 {
                sample_trade_ids.push(ev.trade_id.clone());
            }
        }
        Self {
            period_start,
            period_end,
            total_trades: events.len() as u64,
            total_notional,
            by_instrument,
            sample_trade_ids,
        }
    }
}

// ---------------------------------------------------------------------------
// DORA incident digest
// ---------------------------------------------------------------------------

/// DORA Art. 17 ICT-related incident severity classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoraSeverity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoraIncident {
    pub incident_id: String,
    pub severity: DoraSeverity,
    /// Free-form short description (capped to 256 chars in the digest).
    pub description: String,
    pub detected_at: i64,
    /// Optional recovery timestamp; absent if still ongoing.
    pub resolved_at: Option<i64>,
    /// Affected service name (rope-node, dc-explorer, dcswap-router, …).
    pub affected_service: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoraSeverityBucket {
    pub count: u64,
    pub open_count: u64,
    pub closed_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoraIncidentDigest {
    pub period_start: i64,
    pub period_end: i64,
    pub total_incidents: u64,
    pub open_incidents: u64,
    pub closed_incidents: u64,
    pub by_severity: BTreeMap<String, DoraSeverityBucket>,
    pub by_service: BTreeMap<String, u64>,
    /// Sample incident ids for spot-check auditing.
    pub sample_incident_ids: Vec<String>,
}

impl DoraIncidentDigest {
    pub fn build(period_start: i64, period_end: i64, incidents: &[DoraIncident]) -> Self {
        let mut by_severity: BTreeMap<String, DoraSeverityBucket> = BTreeMap::new();
        let mut by_service: BTreeMap<String, u64> = BTreeMap::new();
        let mut open = 0u64;
        let mut closed = 0u64;
        let mut sample = Vec::new();
        for inc in incidents {
            let sev_label = match inc.severity {
                DoraSeverity::Low => "low",
                DoraSeverity::Medium => "medium",
                DoraSeverity::High => "high",
                DoraSeverity::Critical => "critical",
            };
            let bucket = by_severity.entry(sev_label.to_string()).or_default();
            bucket.count += 1;
            if inc.resolved_at.is_some() {
                bucket.closed_count += 1;
                closed += 1;
            } else {
                bucket.open_count += 1;
                open += 1;
            }
            *by_service.entry(inc.affected_service.clone()).or_insert(0) += 1;
            if sample.len() < 16 {
                sample.push(inc.incident_id.clone());
            }
        }
        Self {
            period_start,
            period_end,
            total_incidents: incidents.len() as u64,
            open_incidents: open,
            closed_incidents: closed,
            by_severity,
            by_service,
            sample_incident_ids: sample,
        }
    }
}

// ---------------------------------------------------------------------------
// Testimony envelope (what the anchor ships to rope_appendToLedger)
// ---------------------------------------------------------------------------

/// Envelope wrapping a `ComplianceTestimony` with provenance metadata
/// and an integrity commitment. Serialised to JSON and embedded in the
/// `metadata.payload` field of the `rope_appendToLedger` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceTestimonyEnvelope {
    /// Stable agent id ("compliance" — mirrors `canonical_ai_agents()`).
    pub agent_id: String,
    /// Wallet that anchors the testimony (the agent's canonical wallet).
    pub agent_wallet: String,
    /// Short label of the testimony body (`gdpr_article_17`,
    /// `mifid_ii_digest`, `dora_incident_digest`).
    pub testimony_label: &'static str,
    /// The body itself.
    pub body: ComplianceTestimony,
    /// BLAKE3 commitment over the canonical JSON of `body`.
    pub anchor_hash: String,
    /// Unix seconds when the envelope was sealed.
    pub sealed_at: i64,
    /// PHASE 1 placeholder — empty until ML-DSA-65 wiring lands.
    pub validator_signature: String,
}

impl ComplianceTestimonyEnvelope {
    pub fn seal(
        agent_id: impl Into<String>,
        agent_wallet: impl Into<String>,
        body: ComplianceTestimony,
        sealed_at: i64,
    ) -> Self {
        let label = body.label();
        let bytes = serde_json::to_vec(&body).unwrap_or_default();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&bytes);
        let anchor_hash = format!("0x{}", hasher.finalize().to_hex());
        Self {
            agent_id: agent_id.into(),
            agent_wallet: agent_wallet.into(),
            testimony_label: label,
            body,
            anchor_hash,
            sealed_at,
            validator_signature: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gdpr::RejectionReason;

    fn ev(id: &str, instrument: &str, notional: u128, venue: &str) -> MiFidIIEvent {
        MiFidIIEvent {
            trade_id: id.to_string(),
            instrument: instrument.to_string(),
            venue: venue.to_string(),
            buyer: "0xbuyer".to_string(),
            seller: "0xseller".to_string(),
            notional,
            currency: "USDC".to_string(),
            executed_at: 1700000000,
        }
    }

    fn inc(id: &str, sev: DoraSeverity, resolved: Option<i64>, svc: &str) -> DoraIncident {
        DoraIncident {
            incident_id: id.to_string(),
            severity: sev,
            description: "test".to_string(),
            detected_at: 1700000000,
            resolved_at: resolved,
            affected_service: svc.to_string(),
        }
    }

    #[test]
    fn mifid_digest_aggregates_per_instrument() {
        let events = vec![
            ev("t1", "DC-FAT", 1000, "dcswap"),
            ev("t2", "DC-FAT", 2500, "dcswap"),
            ev("t3", "WFAT-USDC", 500, "dcswap"),
        ];
        let d = MiFidIIDigest::build(0, 100, &events);
        assert_eq!(d.total_trades, 3);
        assert_eq!(d.total_notional, 4000);
        assert_eq!(d.by_instrument.len(), 2);
        let dcfat = d.by_instrument.get("DC-FAT").unwrap();
        assert_eq!(dcfat.trade_count, 2);
        assert_eq!(dcfat.total_notional, 3500);
        assert_eq!(dcfat.venues, vec!["dcswap".to_string()]);
        assert_eq!(d.sample_trade_ids.len(), 3);
    }

    #[test]
    fn mifid_digest_sample_capped_at_16() {
        let events: Vec<MiFidIIEvent> = (0..32)
            .map(|i| ev(&format!("t{}", i), "DC-FAT", 1, "dcswap"))
            .collect();
        let d = MiFidIIDigest::build(0, 100, &events);
        assert_eq!(d.total_trades, 32);
        assert_eq!(d.sample_trade_ids.len(), 16);
        assert_eq!(d.sample_trade_ids[0], "t0");
        assert_eq!(d.sample_trade_ids[15], "t15");
    }

    #[test]
    fn dora_digest_buckets_open_and_closed() {
        let incs = vec![
            inc("i1", DoraSeverity::Low, Some(100), "rope-node"),
            inc("i2", DoraSeverity::High, None, "dcswap-router"),
            inc("i3", DoraSeverity::High, Some(200), "dcswap-router"),
            inc("i4", DoraSeverity::Critical, None, "rope-node"),
        ];
        let d = DoraIncidentDigest::build(0, 1000, &incs);
        assert_eq!(d.total_incidents, 4);
        assert_eq!(d.open_incidents, 2);
        assert_eq!(d.closed_incidents, 2);
        let high = d.by_severity.get("high").unwrap();
        assert_eq!(high.count, 2);
        assert_eq!(high.open_count, 1);
        assert_eq!(high.closed_count, 1);
        assert_eq!(*d.by_service.get("rope-node").unwrap(), 2);
        assert_eq!(*d.by_service.get("dcswap-router").unwrap(), 2);
    }

    #[test]
    fn envelope_anchor_hash_is_deterministic() {
        let body = ComplianceTestimony::MiFidIIDigest(MiFidIIDigest::build(
            0,
            100,
            &[ev("t1", "DC-FAT", 1000, "dcswap")],
        ));
        let env_a = ComplianceTestimonyEnvelope::seal("compliance", "0xC005", body.clone(), 42);
        let env_b = ComplianceTestimonyEnvelope::seal("compliance", "0xC005", body, 999);
        // sealed_at differs but the anchor_hash commits only to body
        assert_eq!(env_a.anchor_hash, env_b.anchor_hash);
        assert!(env_a.anchor_hash.starts_with("0x"));
    }

    #[test]
    fn gdpr_testimony_from_processed_carries_audit_trail() {
        let req = Article17Request {
            request_id: "rid".to_string(),
            subject_wallet: "0xabc".to_string(),
            requestor_proof: "0x".to_string(),
            justification: JustificationClass::ConsentWithdrawn,
            affected_knots: vec!["0xk".to_string()],
            jurisdiction: 250,
            note: "n".to_string(),
            submitted_at: 0,
        };
        let verdict = Article17Verdict::Approved {
            request_id: "rid".to_string(),
            validated_at: 100,
            normalized_subject_wallet: "0xabc".to_string(),
            normalized_knot_ids: vec!["0xk".to_string()],
        };
        let report = OrchestrationReport {
            request_id: "rid".to_string(),
            subject_wallet: "0xabc".to_string(),
            total_knots: 1,
            success_count: 1,
            failure_count: 0,
            outcomes: vec![TombstoneOutcome {
                knot_string_id: "0xk".to_string(),
                success: true,
                tombstone_audit_hash: Some("0xh1".to_string()),
                untied_at: Some(101),
                knots_remaining: Some(5),
                tombstones_total: Some(1),
                error_code: None,
                error_message: None,
            }],
            tombstone_audit_hashes: vec!["0xh1".to_string()],
        };
        let t = GdprArticle17Testimony::from_processed(&req, &verdict, &report, "compliance", 102);
        assert_eq!(t.tombstone_audit_hashes, vec!["0xh1".to_string()]);
        assert_eq!(t.requested_knot_count, 1);
        assert_eq!(t.justification_label, "Art. 17(1)(b) — consent withdrawn");
        assert!(t.verdict.is_approved());
    }

    #[test]
    fn gdpr_testimony_from_rejected_keeps_record() {
        let req = Article17Request {
            request_id: String::new(),
            subject_wallet: "0xabc".to_string(),
            requestor_proof: String::new(),
            justification: JustificationClass::ConsentWithdrawn,
            affected_knots: vec![],
            jurisdiction: 250,
            note: String::new(),
            submitted_at: 0,
        };
        let verdict = Article17Verdict::Rejected {
            request_id: "rid-2".to_string(),
            validated_at: 100,
            reason_code: RejectionReason::ProofMissing,
            message: "no proof".to_string(),
        };
        let t = GdprArticle17Testimony::from_rejected(&req, &verdict, "compliance", 102);
        assert!(!t.verdict.is_approved());
        assert!(t.tombstone_outcomes.is_empty());
        assert_eq!(t.request_id, "rid-2");
    }
}
