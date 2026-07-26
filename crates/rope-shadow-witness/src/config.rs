//! TOML configuration schema for the shadow witness.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::error::{ShadowWitnessError, ShadowWitnessResult};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShadowWitnessConfig {
    /// Upstream canonical rope-node JSON-RPC endpoint to observe.
    /// Example: `http://127.0.0.1:8545`
    pub upstream_rpc_url: String,

    /// Local RocksDB directory for shadow chain persistence.
    /// Will be created if it does not exist.
    pub data_dir: PathBuf,

    /// HTTP bind address for the shadow JSON-RPC server.
    /// Example: `127.0.0.1:8556`. Bind to `127.0.0.1` for local-only;
    /// front with nginx if remote exposure is required.
    pub bind_addr: String,

    /// Polling interval, in seconds, between consecutive observation
    /// rounds. Smaller values give lower observation latency at the
    /// cost of more upstream RPC traffic. Default 5 s.
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,

    /// Maximum number of strings to enumerate per round via
    /// `rope_listStrings`. Default 200.
    #[serde(default = "default_strings_per_round")]
    pub strings_per_round: u32,

    /// Operational tag attached to log lines and prometheus metrics.
    /// Example: `"datachain-rpc-1-canary"`.
    #[serde(default = "default_witness_tag")]
    pub witness_tag: String,
}

fn default_poll_interval_secs() -> u64 {
    5
}

fn default_strings_per_round() -> u32 {
    200
}

fn default_witness_tag() -> String {
    "shadow-witness".to_string()
}

impl ShadowWitnessConfig {
    pub fn from_path(path: &std::path::Path) -> ShadowWitnessResult<Self> {
        let s = std::fs::read_to_string(path)?;
        toml::from_str(&s)
            .map_err(|e| ShadowWitnessError::Config(format!("parse {}: {}", path.display(), e)))
    }
}
