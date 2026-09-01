//! TOML configuration for `rope-ecosystem-discovery`.
//!
//! The daemon loads a single TOML file (default:
//! `/etc/rope-ecosystem-discovery.toml`) that declares:
//!
//! - Where to write the overlay JSONL file.
//! - Which scanners are enabled and their per-scanner knobs.
//! - Global timeouts, poll cadence, allow-list of partner API hosts.
//!
//! All fields have sensible defaults so that a missing file is fatal
//! only if the operator explicitly required one. In the daemon binary
//! we require the config to be present so that ops mistakes surface as
//! startup errors instead of silent no-ops.
//!
//! The config shape is intentionally conservative:
//!
//! - No secrets stored in TOML; API keys are sourced from environment
//!   variables named in the config.
//! - `partner_api.allowed_hosts` is a hard allowlist; a scanner that
//!   discovers a URL outside this list must drop the entry.

use crate::error::{DiscoveryError, DiscoveryResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default overlay output path. Matches
/// `ecosystem_overlay::DEFAULT_OVERLAY_PATH` semantics on the reader
/// side (dc-explorer resolves `ECOSYSTEM_OVERLAY_PATH` env or falls
/// back to this path). Kept in sync in
/// `docs/ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` §5.
pub const DEFAULT_OVERLAY_PATH: &str = "/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl";

/// Default TOML config path.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/rope-ecosystem-discovery.toml";

/// Default poll cadence between full discovery runs.
pub const DEFAULT_RUN_INTERVAL_SECS: u64 = 900; // 15 min

/// Default per-request HTTP timeout.
pub const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryConfig {
    /// Absolute path to the JSONL file the daemon writes.
    #[serde(default = "default_output_path")]
    pub output_path: PathBuf,

    /// Seconds between full discovery runs (must be >= 60).
    #[serde(default = "default_run_interval_secs")]
    pub run_interval_secs: u64,

    /// Per-request HTTP timeout in seconds.
    #[serde(default = "default_http_timeout_secs")]
    pub http_timeout_secs: u64,

    /// Handover scanner config (optional; scanner disabled if absent).
    #[serde(default)]
    pub handover: HandoverScannerConfig,

    /// On-chain scanner config (optional; scanner disabled if absent).
    #[serde(default)]
    pub onchain: OnchainScannerConfig,

    /// Partner API scanner config (optional; scanner disabled if absent).
    #[serde(default)]
    pub partner_api: PartnerApiScannerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HandoverScannerConfig {
    /// Set true to enable the handover-file scanner.
    #[serde(default)]
    pub enabled: bool,
    /// Root directories to scan for `handover-*.mdc` / `handover-*.md`
    /// files. Empty means the scanner is a no-op even if enabled.
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OnchainScannerConfig {
    /// Set true to enable the on-chain scanner.
    #[serde(default)]
    pub enabled: bool,
    /// Base URL for the dcscan API (e.g. `https://dcscan.io`).
    #[serde(default)]
    pub dcscan_base: Option<String>,
    /// Optional block-window override for recent-contract discovery.
    /// Defaults to the last 100k blocks (~3.5 days at 3s knots).
    #[serde(default)]
    pub lookback_blocks: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PartnerApiScannerConfig {
    /// Set true to enable the partner-API scanner.
    #[serde(default)]
    pub enabled: bool,
    /// Hard allowlist of hostnames the scanner is permitted to fetch
    /// from. Any URL discovered outside this list is dropped with a
    /// warning. Example: `["tanastok.io", "dcswap.net"]`.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// URLs to fetch. Each URL MUST resolve to a host in
    /// `allowed_hosts` after DNS-agnostic parsing.
    #[serde(default)]
    pub endpoints: Vec<PartnerApiEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartnerApiEndpoint {
    /// Human-friendly label for logs.
    pub label: String,
    /// Absolute URL to GET.
    pub url: String,
    /// Name of the environment variable holding the bearer token, if
    /// any. Optional; scanner sends no auth when absent.
    #[serde(default)]
    pub bearer_env: Option<String>,
}

fn default_output_path() -> PathBuf {
    PathBuf::from(DEFAULT_OVERLAY_PATH)
}

fn default_run_interval_secs() -> u64 {
    DEFAULT_RUN_INTERVAL_SECS
}

fn default_http_timeout_secs() -> u64 {
    DEFAULT_HTTP_TIMEOUT_SECS
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        DiscoveryConfig {
            output_path: default_output_path(),
            run_interval_secs: default_run_interval_secs(),
            http_timeout_secs: default_http_timeout_secs(),
            handover: HandoverScannerConfig::default(),
            onchain: OnchainScannerConfig::default(),
            partner_api: PartnerApiScannerConfig::default(),
        }
    }
}

impl DiscoveryConfig {
    /// Load a config from a TOML file. Applies defaults for missing
    /// fields. Validates sanity constraints (e.g. `run_interval_secs
    /// >= 60`).
    pub fn from_file(path: &Path) -> DiscoveryResult<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            DiscoveryError::Config(format!("read {}: {}", path.display(), e))
        })?;
        let cfg: DiscoveryConfig = toml::from_str(&raw)
            .map_err(|e| DiscoveryError::Config(format!("parse {}: {}", path.display(), e)))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Enforce structural constraints. Called by `from_file` and
    /// available for direct callers that build a config in code.
    pub fn validate(&self) -> DiscoveryResult<()> {
        if self.run_interval_secs < 60 {
            return Err(DiscoveryError::Config(format!(
                "run_interval_secs must be >= 60, got {}",
                self.run_interval_secs
            )));
        }
        if self.http_timeout_secs == 0 || self.http_timeout_secs > 60 {
            return Err(DiscoveryError::Config(format!(
                "http_timeout_secs must be in [1, 60], got {}",
                self.http_timeout_secs
            )));
        }
        if !self.output_path.is_absolute() {
            return Err(DiscoveryError::Config(format!(
                "output_path must be absolute, got {}",
                self.output_path.display()
            )));
        }
        // Partner-api allow-list: any endpoint URL must contain a host
        // present in allowed_hosts. Empty allowed_hosts + non-empty
        // endpoints is rejected (fail-secure).
        if self.partner_api.enabled {
            if self.partner_api.allowed_hosts.is_empty() && !self.partner_api.endpoints.is_empty()
            {
                return Err(DiscoveryError::Config(
                    "partner_api.enabled with endpoints but empty allowed_hosts".into(),
                ));
            }
            for ep in &self.partner_api.endpoints {
                let parsed = url::Url::parse(&ep.url).map_err(|e| {
                    DiscoveryError::Config(format!(
                        "partner_api endpoint '{}': invalid url '{}': {}",
                        ep.label, ep.url, e
                    ))
                })?;
                let host = parsed.host_str().unwrap_or("");
                if !self
                    .partner_api
                    .allowed_hosts
                    .iter()
                    .any(|h| h.eq_ignore_ascii_case(host))
                {
                    return Err(DiscoveryError::Config(format!(
                        "partner_api endpoint '{}': host '{}' not in allowed_hosts",
                        ep.label, host
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn run_interval(&self) -> Duration {
        Duration::from_secs(self.run_interval_secs)
    }

    pub fn http_timeout(&self) -> Duration {
        Duration::from_secs(self.http_timeout_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn defaults_pass_validation() {
        let cfg = DiscoveryConfig::default();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn rejects_run_interval_below_60() {
        let mut cfg = DiscoveryConfig::default();
        cfg.run_interval_secs = 30;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_zero_or_huge_http_timeout() {
        let mut cfg = DiscoveryConfig::default();
        cfg.http_timeout_secs = 0;
        assert!(cfg.validate().is_err());
        cfg.http_timeout_secs = 61;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_relative_output_path() {
        let mut cfg = DiscoveryConfig::default();
        cfg.output_path = PathBuf::from("relative/path.jsonl");
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_partner_endpoint_outside_allowlist() {
        let mut cfg = DiscoveryConfig::default();
        cfg.partner_api.enabled = true;
        cfg.partner_api.allowed_hosts = vec!["tanastok.io".into()];
        cfg.partner_api.endpoints = vec![PartnerApiEndpoint {
            label: "evil".into(),
            url: "https://evil.example.com/foo".into(),
            bearer_env: None,
        }];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn accepts_partner_endpoint_in_allowlist() {
        let mut cfg = DiscoveryConfig::default();
        cfg.partner_api.enabled = true;
        cfg.partner_api.allowed_hosts = vec!["tanastok.io".into()];
        cfg.partner_api.endpoints = vec![PartnerApiEndpoint {
            label: "tanastok-manifest".into(),
            url: "https://tanastok.io/api/v1/tokenized-assets".into(),
            bearer_env: None,
        }];
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn rejects_partner_enabled_with_empty_allowlist_and_endpoints() {
        let mut cfg = DiscoveryConfig::default();
        cfg.partner_api.enabled = true;
        cfg.partner_api.endpoints = vec![PartnerApiEndpoint {
            label: "x".into(),
            url: "https://x.example.com/".into(),
            bearer_env: None,
        }];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn from_file_loads_minimal_config() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            r#"
output_path = "/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl"
run_interval_secs = 900
http_timeout_secs = 10

[handover]
enabled = true
roots = ["/root/.cursor/rules"]
"#
        )
        .unwrap();
        let cfg = DiscoveryConfig::from_file(tmp.path()).unwrap();
        assert!(cfg.handover.enabled);
        assert_eq!(cfg.handover.roots.len(), 1);
        assert_eq!(cfg.run_interval_secs, 900);
    }
}
