//! Typed configuration for the OracleAgent binary.
//!
//! The configuration is layered: CLI flags > environment variables > defaults.
//! The CLI binary in `main.rs` constructs an [`AgentConfig`] from the parsed
//! [`clap::Parser`] args, and the agent loop in [`crate::OracleAgent`] consumes
//! it.
//!
//! ## Defaults
//!
//! * Feed URL: `https://dcswap.net/v1/prices` — the canonical Datachain Rope
//!   price feed (see workspace rule `handover-canonical-fat-price-2026-03-14`).
//! * RPC URL: `http://127.0.0.1:9001` — the local rope-node JSON-RPC HTTP
//!   endpoint as specified in the OracleAgent build brief.
//! * Interval: 60s.
//! * Wallet hex: `0x000000000000000000000000000000000000C002` — the
//!   canonical OracleAgent wallet listed in the dc-explorer's
//!   `canonical_ai_agents()` (see `crates/rope-explorer/src/main.rs`).

use std::path::PathBuf;
use std::time::Duration;

/// Default canonical feed URL.
pub const DEFAULT_FEED_URL: &str = "https://dcswap.net/v1/prices";

/// Default local node RPC URL.
pub const DEFAULT_RPC_URL: &str = "http://127.0.0.1:9001";

/// Canonical OracleAgent wallet (per `canonical_ai_agents()` in dc-explorer).
pub const DEFAULT_WALLET_HEX: &str = "0x000000000000000000000000000000000000C002";

/// Default polling interval (seconds).
pub const DEFAULT_INTERVAL_SECS: u64 = 60;

/// Default request timeout for the price feed fetch.
pub const DEFAULT_FEED_TIMEOUT_SECS: u64 = 15;

/// Default request timeout for the JSON-RPC anchor call.
pub const DEFAULT_RPC_TIMEOUT_SECS: u64 = 10;

/// Default user agent string sent to dcswap.net so the operator can identify
/// the traffic in logs.
pub const DEFAULT_USER_AGENT: &str = concat!("oracle-agent/", env!("CARGO_PKG_VERSION"));

/// Signing mode for the testimony payload.
///
/// The Datachain Rope production preference is hybrid (Ed25519 + Dilithium3,
/// per `rope-crypto::HybridSigner`). The Ed25519-only mode exists only as a
/// debugging aid for local development and *must not* be used in production.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SigningMode {
    /// Ed25519 + CRYSTALS-Dilithium3, as produced by `rope_crypto::KeyPair`.
    #[default]
    Hybrid,
    /// Ed25519 only — the Dilithium part of the testimony is left empty.
    /// This mode is for local development / faster CI only.
    Ed25519Only,
}

impl std::fmt::Display for SigningMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SigningMode::Hybrid => write!(f, "hybrid"),
            SigningMode::Ed25519Only => write!(f, "ed25519-only"),
        }
    }
}

impl std::str::FromStr for SigningMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "hybrid" | "h" => Ok(SigningMode::Hybrid),
            "ed25519" | "ed25519-only" | "ed" => Ok(SigningMode::Ed25519Only),
            other => Err(format!(
                "unknown signing mode: {other:?} (valid: hybrid, ed25519-only)"
            )),
        }
    }
}

/// Resolved configuration for a running [`crate::OracleAgent`].
#[derive(Clone, Debug)]
pub struct AgentConfig {
    /// URL of the price feed (e.g. `https://dcswap.net/v1/prices`).
    pub feed_url: String,
    /// URL of the local rope-node JSON-RPC endpoint.
    pub rpc_url: String,
    /// Wallet hex (0x-prefixed) the OracleAgent anchors testimonies on.
    pub wallet_hex: String,
    /// Polling interval between testimonies.
    pub interval: Duration,
    /// Optional path to a 32-byte raw seed file used to derive a deterministic
    /// keypair. When `None`, an ephemeral keypair is generated at startup.
    pub key_path: Option<PathBuf>,
    /// Whether to call `rope_createPersonalLedger` once at startup if the
    /// ledger does not yet exist.
    pub auto_create_ledger: bool,
    /// Signing mode (hybrid by default).
    pub signing_mode: SigningMode,
    /// Timeout for the HTTP feed fetch.
    pub feed_timeout: Duration,
    /// Timeout for the JSON-RPC anchor call.
    pub rpc_timeout: Duration,
    /// Maximum retry attempts (per cycle) for the feed and the anchor call.
    pub max_retries: u32,
    /// Initial backoff between retries (doubles each attempt, capped).
    pub backoff_initial: Duration,
    /// Hard cap on the exponential backoff between retries.
    pub backoff_max: Duration,
    /// User-Agent header for the feed fetch.
    pub user_agent: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            feed_url: DEFAULT_FEED_URL.to_string(),
            rpc_url: DEFAULT_RPC_URL.to_string(),
            wallet_hex: DEFAULT_WALLET_HEX.to_string(),
            interval: Duration::from_secs(DEFAULT_INTERVAL_SECS),
            key_path: None,
            auto_create_ledger: true,
            signing_mode: SigningMode::Hybrid,
            feed_timeout: Duration::from_secs(DEFAULT_FEED_TIMEOUT_SECS),
            rpc_timeout: Duration::from_secs(DEFAULT_RPC_TIMEOUT_SECS),
            max_retries: 4,
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
            user_agent: DEFAULT_USER_AGENT.to_string(),
        }
    }
}

impl AgentConfig {
    /// Validate the parsed configuration.
    ///
    /// This catches obvious user errors (zero interval, malformed URLs, bad
    /// wallet hex) early so the agent loop can assume the values are sane.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.interval.is_zero() {
            return Err(ConfigError::Invalid("interval must be > 0".into()));
        }
        if self.feed_timeout.is_zero() {
            return Err(ConfigError::Invalid("feed_timeout must be > 0".into()));
        }
        if self.rpc_timeout.is_zero() {
            return Err(ConfigError::Invalid("rpc_timeout must be > 0".into()));
        }
        if !(self.feed_url.starts_with("http://") || self.feed_url.starts_with("https://")) {
            return Err(ConfigError::Invalid(format!(
                "feed_url must start with http(s)://, got {:?}",
                self.feed_url
            )));
        }
        if !(self.rpc_url.starts_with("http://") || self.rpc_url.starts_with("https://")) {
            return Err(ConfigError::Invalid(format!(
                "rpc_url must start with http(s)://, got {:?}",
                self.rpc_url
            )));
        }
        validate_wallet_hex(&self.wallet_hex)?;
        Ok(())
    }
}

/// Errors raised while validating an [`AgentConfig`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Strict 0x-prefixed 20-byte address validator. Same shape as the
/// `WalletAddress::from_hex` accepted format in `rope-crypto::ledger_encryption`.
pub fn validate_wallet_hex(s: &str) -> Result<(), ConfigError> {
    let stripped = s.strip_prefix("0x").unwrap_or(s);
    if stripped.len() != 40 {
        return Err(ConfigError::Invalid(format!(
            "wallet hex must be 20 bytes (40 hex chars), got {} chars",
            stripped.len()
        )));
    }
    if !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ConfigError::Invalid(
            "wallet hex contains non-hex characters".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        AgentConfig::default()
            .validate()
            .expect("default config must validate");
    }

    #[test]
    fn rejects_zero_interval() {
        let cfg = AgentConfig {
            interval: Duration::from_secs(0),
            ..AgentConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_bad_url() {
        let cfg = AgentConfig {
            feed_url: "ftp://nope".into(),
            ..AgentConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_bad_wallet_hex() {
        for bad in ["", "0xZZ", "deadbeef", "0x1234"] {
            let cfg = AgentConfig {
                wallet_hex: bad.into(),
                ..AgentConfig::default()
            };
            assert!(cfg.validate().is_err(), "expected {:?} to be rejected", bad);
        }
    }

    #[test]
    fn signing_mode_parses_round_trip() {
        for mode in [SigningMode::Hybrid, SigningMode::Ed25519Only] {
            let s = mode.to_string();
            let parsed: SigningMode = s.parse().unwrap();
            assert_eq!(parsed, mode);
        }
    }

    #[test]
    fn signing_mode_accepts_aliases() {
        assert_eq!(
            "ed25519".parse::<SigningMode>().unwrap(),
            SigningMode::Ed25519Only
        );
        assert_eq!("h".parse::<SigningMode>().unwrap(), SigningMode::Hybrid);
        assert!("frobnicate".parse::<SigningMode>().is_err());
    }
}
