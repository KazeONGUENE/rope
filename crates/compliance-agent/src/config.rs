// =============================================================================
// ComplianceAgent — runtime configuration
// =============================================================================
//
// One ComplianceAgent process is a long-running service that:
//
//   * Listens on an HTTP port for GDPR Art. 17 erasure requests, MiFID II
//     trade-event submissions, and DORA incident reports.
//   * Talks to a Datachain Rope node over JSON-RPC (`rope_untieKnot` to
//     execute approved erasures, `rope_appendToLedger` to anchor signed
//     `ComplianceTestimony` knots).
//   * Periodically (default every 15 minutes) drains its MiFID II /
//     DORA event buffers into testimony digests and anchors them on its
//     own canonical wallet.
//
// This module defines the configuration the binary boots with. It can
// be assembled from CLI flags (see `src/main.rs`) or constructed by
// integration tests.
// =============================================================================

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The canonical wallet of the ComplianceAgent on Datachain Rope.
///
/// This is the address whose personal ledger receives `ComplianceTestimony`
/// knots. The value mirrors the `wallet` field of the canonical agent
/// entry returned by `rope-explorer::canonical_ai_agents()` (id =
/// `compliance`, wallet = `0x000000000000000000000000000000000000C005`).
pub const CANONICAL_COMPLIANCE_AGENT_WALLET: &str =
    "0x000000000000000000000000000000000000C005";

/// Default RPC endpoint for the local Datachain Rope node.
///
/// In production this is overridden to the in-cluster URL or to
/// `https://erpc.datachain.network` when the agent is run remotely.
pub const DEFAULT_RPC_URL: &str = "http://127.0.0.1:8545";

/// Default HTTP listen address for inbound regulator / dApp requests.
pub const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:9091";

/// Default reporting cadence for MiFID II / DORA digests.
pub const DEFAULT_REPORTING_INTERVAL_SECS: u64 = 15 * 60;

/// Default per-batch cap so a digest does not balloon unbounded.
pub const DEFAULT_MAX_DIGEST_EVENTS: usize = 1024;

/// Top-level configuration for the running ComplianceAgent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceAgentConfig {
    /// Address the HTTP server binds to. Form: `host:port`.
    pub listen_addr: String,

    /// JSON-RPC endpoint of the local rope-node.
    pub rpc_url: String,

    /// Wallet whose string the agent anchors testimonies on. Defaults to
    /// the canonical ComplianceAgent wallet `0x...C005`.
    pub agent_wallet: String,

    /// Optional path to a key file used to sign outbound testimonies.
    ///
    /// PHASE 1: presence-only (existence is logged; the contents are not
    /// yet wired into a real PQ-signature, mirroring the rope-node
    /// `verify_signatures: false` posture in `consensus_orchestrator.rs`).
    /// PHASE 2 will derive an ML-DSA-65 secret key from this file and
    /// sign every testimony envelope before anchoring.
    pub key_path: Option<PathBuf>,

    /// GDPR validation policy.
    pub gdpr: GdprPolicy,

    /// Periodic reporting cadence (MiFID II + DORA digests).
    pub reporting_interval: Duration,

    /// Maximum events per digest batch (older events are kept and
    /// drained on the next tick).
    pub max_digest_events: usize,
}

impl Default for ComplianceAgentConfig {
    fn default() -> Self {
        Self {
            listen_addr: DEFAULT_LISTEN_ADDR.to_string(),
            rpc_url: DEFAULT_RPC_URL.to_string(),
            agent_wallet: CANONICAL_COMPLIANCE_AGENT_WALLET.to_string(),
            key_path: None,
            gdpr: GdprPolicy::default(),
            reporting_interval: Duration::from_secs(DEFAULT_REPORTING_INTERVAL_SECS),
            max_digest_events: DEFAULT_MAX_DIGEST_EVENTS,
        }
    }
}

/// Structural validation policy for inbound GDPR Art. 17 requests.
///
/// This is intentionally a *schema* policy — it checks that the request
/// is well-formed and that the requested justification + jurisdiction
/// fall inside the configured whitelist. It does **not** attempt to
/// adjudicate the underlying legal question; that responsibility stays
/// with the operator's compliance team and any upstream KYC/AML
/// pipeline. See `gdpr::Article17Validator` for the corresponding code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdprPolicy {
    /// ISO-3166 country codes whose subjects the agent will process.
    /// Empty means "accept any jurisdiction" (the most permissive
    /// configuration; useful for tests and dev).
    pub allowed_jurisdictions: BTreeSet<u16>,

    /// If true, every request must carry a non-empty `requestor_proof`.
    /// PHASE 1 only checks for presence + minimum length — Phase 2 will
    /// verify a real signature against the subject wallet's public key.
    pub require_requestor_proof: bool,

    /// Minimum length of `requestor_proof` (bytes after hex decode).
    /// Defaults to 32 (≈ a 256-bit signature digest).
    pub min_proof_bytes: usize,

    /// Maximum number of knots a single request can target. A defence
    /// against a single request demanding the untying of an entire
    /// chain — operators can still issue many smaller requests.
    pub max_knots_per_request: usize,
}

impl Default for GdprPolicy {
    fn default() -> Self {
        Self {
            allowed_jurisdictions: BTreeSet::new(),
            require_requestor_proof: true,
            min_proof_bytes: 32,
            max_knots_per_request: 256,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_canonical() {
        let cfg = ComplianceAgentConfig::default();
        assert_eq!(cfg.agent_wallet, CANONICAL_COMPLIANCE_AGENT_WALLET);
        assert_eq!(cfg.listen_addr, DEFAULT_LISTEN_ADDR);
        assert_eq!(
            cfg.reporting_interval,
            Duration::from_secs(DEFAULT_REPORTING_INTERVAL_SECS)
        );
        assert!(cfg.gdpr.require_requestor_proof);
        assert_eq!(cfg.gdpr.min_proof_bytes, 32);
        assert_eq!(cfg.gdpr.max_knots_per_request, 256);
    }

    #[test]
    fn config_round_trips_through_json() {
        let cfg = ComplianceAgentConfig::default();
        let s = serde_json::to_string(&cfg).expect("serialize");
        let back: ComplianceAgentConfig = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back.agent_wallet, cfg.agent_wallet);
        assert_eq!(back.listen_addr, cfg.listen_addr);
        assert_eq!(back.reporting_interval, cfg.reporting_interval);
    }

    #[test]
    fn jurisdictions_can_be_extended() {
        let mut cfg = ComplianceAgentConfig::default();
        cfg.gdpr.allowed_jurisdictions.insert(250); // FR
        cfg.gdpr.allowed_jurisdictions.insert(276); // DE
        assert_eq!(cfg.gdpr.allowed_jurisdictions.len(), 2);
        assert!(cfg.gdpr.allowed_jurisdictions.contains(&250));
    }
}
