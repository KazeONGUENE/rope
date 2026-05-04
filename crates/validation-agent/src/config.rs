//! Typed configuration for the [`crate::agent::ValidationAgent`].
//!
//! Configuration can be built three ways:
//!
//! 1. From the CLI via [`crate::config::ValidationAgentConfig::from_cli`]
//!    (called by `main.rs`).
//! 2. Programmatically via the public fields / `Default` impl.
//! 3. Via `ValidationAgentConfig::for_test()` for unit tests where we
//!    want a deterministic configuration that does NOT touch the network.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Default JSON-RPC URL — the host where rope-node typically listens.
///
/// In production we point at the local node directly to avoid hitting
/// the public proxy and tripping over the 10s read timeout that
/// surfaces 504s for the DCSwap indexer (see the
/// `dcswap-rope-rpc-504-timeout-2026-05-03` rule). When running against
/// a remote node, override this.
pub const DEFAULT_RPC_URL: &str = "http://127.0.0.1:8545";

/// Default poll interval. Five seconds is conservative; one full
/// validation cycle currently takes well under that even when Dilithium
/// verification dominates the budget.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;

/// Default upper bound on knots fetched per poll cycle. Prevents an
/// agent that has been offline from drowning the node with a 100k-knot
/// catch-up burst on its first tick.
pub const DEFAULT_MAX_KNOTS_PER_TICK: u64 = 64;

/// Default HTTP request timeout to the upstream JSON-RPC endpoint.
pub const DEFAULT_RPC_TIMEOUT_SECS: u64 = 8;

/// Validation agent configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationAgentConfig {
    /// JSON-RPC URL of the local rope-node.
    pub rpc_url: String,

    /// Poll interval in seconds for the subscriber loop.
    pub poll_interval: Duration,

    /// Maximum cord anchor knots scanned per poll tick (catch-up cap).
    pub max_knots_per_tick: u64,

    /// HTTP request timeout for individual JSON-RPC calls.
    pub rpc_timeout: Duration,

    /// Filesystem path to the agent's hybrid signing key (BLAKE3 seed,
    /// 32 raw bytes). When `None`, the agent generates an ephemeral
    /// in-memory key (acceptable for tests and dry runs; NOT for
    /// production).
    pub key_path: Option<PathBuf>,

    /// Wallet address used as the testimony submitter on the cord.
    /// Defaults to the canonical [`crate::VALIDATION_AGENT_WALLET`].
    pub wallet_address: String,

    /// When `true` (default), only cord anchor knots are validated.
    /// When `false`, the agent will additionally crawl per-entity
    /// strings via `rope_listStrings` (kind=wallet) — currently a
    /// best-effort, opt-in extension that emits a `not yet implemented`
    /// warning when toggled. See `subscriber.rs`.
    pub anchor_only: bool,

    /// When `true`, the agent runs a single tick and exits — useful for
    /// CI smoke tests.
    pub single_tick: bool,
}

impl Default for ValidationAgentConfig {
    fn default() -> Self {
        Self {
            rpc_url: DEFAULT_RPC_URL.to_string(),
            poll_interval: Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS),
            max_knots_per_tick: DEFAULT_MAX_KNOTS_PER_TICK,
            rpc_timeout: Duration::from_secs(DEFAULT_RPC_TIMEOUT_SECS),
            key_path: None,
            wallet_address: crate::VALIDATION_AGENT_WALLET.to_string(),
            anchor_only: true,
            single_tick: false,
        }
    }
}

impl ValidationAgentConfig {
    /// Build a deterministic configuration suitable for unit tests
    /// (does not touch the network when paired with a mock RPC).
    pub fn for_test() -> Self {
        Self {
            rpc_url: "http://127.0.0.1:0".to_string(),
            poll_interval: Duration::from_millis(10),
            max_knots_per_tick: 8,
            rpc_timeout: Duration::from_millis(50),
            key_path: None,
            wallet_address: crate::VALIDATION_AGENT_WALLET.to_string(),
            anchor_only: true,
            single_tick: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_uses_canonical_wallet() {
        let cfg = ValidationAgentConfig::default();
        assert_eq!(cfg.wallet_address, crate::VALIDATION_AGENT_WALLET);
        assert!(cfg.anchor_only);
        assert!(!cfg.single_tick);
        assert_eq!(cfg.poll_interval, Duration::from_secs(5));
    }

    #[test]
    fn for_test_is_deterministic_and_offline() {
        let cfg = ValidationAgentConfig::for_test();
        assert!(cfg.single_tick);
        assert!(cfg.poll_interval < Duration::from_secs(1));
        assert!(cfg.rpc_timeout < Duration::from_secs(1));
    }
}
