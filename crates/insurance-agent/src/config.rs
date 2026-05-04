//! Runtime configuration for `InsuranceAgent`.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// All knobs that control a running `InsuranceAgent`.
///
/// The CLI builds one of these from flags and environment variables and hands
/// it to [`crate::InsuranceAgent::new`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InsuranceAgentConfig {
    /// Datachain Rope JSON-RPC endpoint used by the [`crate::JsonRpcAnchor`]
    /// to submit attestations via `rope_appendToLedger`.
    pub rpc_url: String,

    /// Tanastok tokenized-assets endpoint. Defaults to the public production
    /// URL from the workspace handover rule.
    pub tanastok_url: String,

    /// Wallet address that owns the on-chain agent string. Every anchor call
    /// is `rope_appendToLedger(<this address>, <payload>)`.
    pub agent_wallet: String,

    /// Logical agent identifier embedded in every attestation. Defaults to
    /// the canonical name `"InsuranceAgent"`.
    pub agent_id: String,

    /// Refresh cadence for the asset list and attestation pass.
    /// Default: 1 hour.
    pub interval: Duration,

    /// Re-attestation threshold. An asset whose most recent attestation is
    /// younger than this is skipped this round. Default: 24 hours.
    pub reattest_after: Duration,

    /// HTTP timeout for both feed fetches and anchor calls. Default: 30 s.
    pub http_timeout: Duration,

    /// Validity window written into each attestation's `valid_until`.
    /// Default: 7 days. The `valid_from` is always the issuance timestamp.
    pub attestation_validity: Duration,

    /// If `true`, run a single pass and exit (used by the `--once` CLI flag
    /// and by tests).
    pub run_once: bool,
}

impl Default for InsuranceAgentConfig {
    fn default() -> Self {
        Self {
            rpc_url: crate::DEFAULT_RPC_URL.to_string(),
            tanastok_url: crate::DEFAULT_TANASTOK_URL.to_string(),
            agent_wallet: crate::CANONICAL_AGENT_WALLET.to_string(),
            agent_id: crate::CANONICAL_AGENT_ID.to_string(),
            interval: Duration::from_secs(3600),
            reattest_after: Duration::from_secs(86_400),
            http_timeout: Duration::from_secs(30),
            attestation_validity: Duration::from_secs(7 * 86_400),
            run_once: false,
        }
    }
}

impl InsuranceAgentConfig {
    /// Build a config from explicit parts. All other fields take their
    /// [`Default`] values.
    pub fn new(rpc_url: impl Into<String>, tanastok_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
            tanastok_url: tanastok_url.into(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = InsuranceAgentConfig::default();
        assert_eq!(cfg.interval.as_secs(), 3600);
        assert_eq!(cfg.reattest_after.as_secs(), 86_400);
        assert_eq!(cfg.attestation_validity.as_secs(), 7 * 86_400);
        assert!(cfg.rpc_url.starts_with("https://"));
        assert!(cfg.tanastok_url.starts_with("https://"));
        assert_eq!(cfg.agent_wallet, crate::CANONICAL_AGENT_WALLET);
    }
}
