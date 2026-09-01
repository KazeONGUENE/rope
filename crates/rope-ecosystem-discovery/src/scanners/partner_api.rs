//! Partner-API scanner.
//!
//! Step 1 (Minimal buildable): STUB. Returns an empty `ScanResult`
//! so the crate compiles and the orchestrator loop is exercisable
//! end-to-end with a real on-chain scanner.
//!
//! Step 2 (deferred): the real implementation walks
//! `PartnerApiScannerConfig::endpoints`, GETs each URL (with the
//! bearer token from `bearer_env` if present), parses the JSON
//! response using a per-endpoint adapter (Tanastok returns
//! `{ontologies: [...]}`, DCSwap returns `{pairs: [...]}`, etc.),
//! and emits one `OverlayEntry` per project found. Every URL is
//! validated against `allowed_hosts` before the fetch fires, so a
//! misconfigured endpoint pointing at localhost or an internal
//! address cannot cause an SSRF.
//!
//! The stub logs `enabled=<bool> endpoints=<n>` on every call so
//! operators can confirm the scanner is wired up without waiting for
//! Step 2.

use crate::config::PartnerApiScannerConfig;
use crate::entry::DiscoveredBy;
use crate::error::DiscoveryResult;
use crate::scanners::{ScanResult, Scanner};
use async_trait::async_trait;
use std::time::Duration;
use tracing::info;

/// Partner-API scanner. Constructed from the daemon config +
/// http-timeout inherited from the top-level config so all scanners
/// share one budget.
pub struct PartnerApiScanner {
    config: PartnerApiScannerConfig,
    #[allow(dead_code)] // Step 2 uses this when building the reqwest client.
    http_timeout: Duration,
}

impl PartnerApiScanner {
    pub fn new(config: PartnerApiScannerConfig, http_timeout: Duration) -> Self {
        Self {
            config,
            http_timeout,
        }
    }
}

#[async_trait]
impl Scanner for PartnerApiScanner {
    fn name(&self) -> &'static str {
        DiscoveredBy::PartnerApiScanner.as_str()
    }

    fn enabled(&self) -> bool {
        // Enabled only when the operator opts in AND at least one
        // endpoint is configured. Empty endpoints with `enabled=true`
        // is a config mistake, not a request to guess partner URLs.
        self.config.enabled && !self.config.endpoints.is_empty()
    }

    async fn scan(&self) -> DiscoveryResult<ScanResult> {
        info!(
            "partner-api scanner (stub): enabled={} endpoints={}",
            self.enabled(),
            self.config.endpoints.len()
        );
        Ok(ScanResult {
            entries: Vec::new(),
            raw_considered: 0,
            raw_rejected: 0,
            scanner: self.name(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PartnerApiEndpoint;

    fn one_endpoint() -> PartnerApiEndpoint {
        PartnerApiEndpoint {
            label: "test".to_string(),
            url: "https://example.com/api".to_string(),
            bearer_env: None,
        }
    }

    #[tokio::test]
    async fn stub_returns_empty_result_when_enabled() {
        let mut cfg = PartnerApiScannerConfig::default();
        cfg.enabled = true;
        cfg.endpoints = vec![one_endpoint()];
        let s = PartnerApiScanner::new(cfg, Duration::from_secs(5));
        assert!(s.enabled());
        let r = s.scan().await.unwrap();
        assert!(r.entries.is_empty());
        assert_eq!(r.raw_considered, 0);
        assert_eq!(r.raw_rejected, 0);
        assert_eq!(r.scanner, "partner-api-scanner");
    }

    #[tokio::test]
    async fn stub_reports_disabled_when_endpoints_empty() {
        let mut cfg = PartnerApiScannerConfig::default();
        cfg.enabled = true;
        cfg.endpoints = Vec::new();
        let s = PartnerApiScanner::new(cfg, Duration::from_secs(5));
        assert!(!s.enabled(), "empty endpoints must disable the scanner");
    }

    #[tokio::test]
    async fn stub_reports_disabled_when_flag_off() {
        let mut cfg = PartnerApiScannerConfig::default();
        cfg.enabled = false;
        cfg.endpoints = vec![one_endpoint()];
        let s = PartnerApiScanner::new(cfg, Duration::from_secs(5));
        assert!(!s.enabled(), "enabled=false must disable the scanner");
    }
}
