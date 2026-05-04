//! SemanticAgent configuration and CLI surface.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Identity used when anchoring [`crate::IndexCheckpointTestimony`] knots.
///
/// The wallet defaults to the canonical agent wallet listed in
/// `crates/rope-explorer/src/main.rs::canonical_ai_agents()`. Operators
/// can override either field for sandbox/staging environments.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub agent_id: String,
    pub wallet: String,
}

impl Default for AgentIdentity {
    fn default() -> Self {
        Self {
            agent_id: crate::CANONICAL_AGENT_ID.to_string(),
            wallet: crate::CANONICAL_AGENT_WALLET.to_string(),
        }
    }
}

/// Top-level runtime configuration. All fields have sane defaults so a
/// fresh `AgentConfig::default()` is enough to spin up a local agent
/// against `http://localhost:8545`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentConfig {
    /// JSON-RPC endpoint of the rope-node we observe.
    pub rpc_url: String,
    /// Per-RPC-call timeout.
    pub rpc_timeout: Duration,
    /// Where the tantivy index lives on disk.
    pub index_path: PathBuf,
    /// HTTP listen address for the search API.
    pub listen_addr: String,
    /// Indexer poll interval.
    pub poll_interval: Duration,
    /// Checkpoint cadence — every N seconds emit a signed
    /// [`crate::IndexCheckpointTestimony`].
    pub checkpoint_interval: Duration,
    /// Page size for `rope_listStrings` calls.
    pub list_strings_limit: u32,
    /// Hard cap on knots fetched per poll (safety against runaway
    /// scans). Set high in production.
    pub max_knots_per_poll: usize,
    /// Identity used when anchoring checkpoints.
    pub identity: AgentIdentity,
    /// When `true`, the agent only reads (no anchor RPCs are issued).
    /// Useful for read-only replicas or pre-flight smoke tests.
    pub read_only: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            rpc_url: "http://127.0.0.1:8545".to_string(),
            rpc_timeout: Duration::from_secs(10),
            index_path: PathBuf::from("./semantic-agent-index"),
            listen_addr: "0.0.0.0:9092".to_string(),
            poll_interval: Duration::from_secs(30),
            checkpoint_interval: Duration::from_secs(600),
            list_strings_limit: 200,
            max_knots_per_poll: 5_000,
            identity: AgentIdentity::default(),
            read_only: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_canonical_constants() {
        let c = AgentConfig::default();
        assert_eq!(c.identity.agent_id, crate::CANONICAL_AGENT_ID);
        assert_eq!(c.identity.wallet, crate::CANONICAL_AGENT_WALLET);
        assert_eq!(c.poll_interval, Duration::from_secs(30));
        assert_eq!(c.checkpoint_interval, Duration::from_secs(600));
    }
}
