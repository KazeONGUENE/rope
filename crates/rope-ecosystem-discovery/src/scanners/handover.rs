//! Handover-file scanner.
//!
//! Step 1 (Minimal buildable): STUB. Returns an empty `ScanResult`
//! so the crate compiles and the orchestrator loop is exercisable
//! end-to-end with a real on-chain scanner.
//!
//! Step 2 (deferred): the real parser walks every root directory in
//! `HandoverScannerConfig::roots`, opens `handover-*.mdc` /
//! `handover-*.md` files, and extracts project references using
//! deterministic regex patterns (workspace path -> project id,
//! `wallet:` lines -> EVM address, `tags:` lines -> archetype hint,
//! `datachain.network/<project>` links -> stakeholder URL). Every
//! extracted entry runs through `is_known_archetype` (or
//! `guess_archetype` when the source has no explicit tag) and
//! `normalise_evm_address` before being emitted, so anything the
//! loader would reject is dropped locally with an incremented
//! `raw_rejected` counter.
//!
//! The stub logs `enabled=<bool> roots=<n>` on every call so
//! operators can confirm the scanner is wired up without waiting for
//! Step 2.

use crate::config::HandoverScannerConfig;
use crate::entry::DiscoveredBy;
use crate::error::DiscoveryResult;
use crate::scanners::{ScanResult, Scanner};
use async_trait::async_trait;
use tracing::info;

/// Handover-file scanner. Constructed from the daemon config; owns
/// only its configuration (no external state to keep across scans).
pub struct HandoverScanner {
    config: HandoverScannerConfig,
}

impl HandoverScanner {
    pub fn new(config: HandoverScannerConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Scanner for HandoverScanner {
    fn name(&self) -> &'static str {
        DiscoveredBy::HandoverScanner.as_str()
    }

    fn enabled(&self) -> bool {
        // The scanner is enabled only when the operator opts in AND
        // at least one root is configured. An empty root list with
        // `enabled=true` is a config mistake, not a request to scan
        // the whole filesystem.
        self.config.enabled && !self.config.roots.is_empty()
    }

    async fn scan(&self) -> DiscoveryResult<ScanResult> {
        info!(
            "handover scanner (stub): enabled={} roots={}",
            self.enabled(),
            self.config.roots.len()
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
    use std::path::PathBuf;

    #[tokio::test]
    async fn stub_returns_empty_result_when_enabled() {
        let mut cfg = HandoverScannerConfig::default();
        cfg.enabled = true;
        cfg.roots = vec![PathBuf::from("/tmp")];
        let s = HandoverScanner::new(cfg);
        assert!(s.enabled());
        let r = s.scan().await.unwrap();
        assert!(r.entries.is_empty());
        assert_eq!(r.raw_considered, 0);
        assert_eq!(r.raw_rejected, 0);
        assert_eq!(r.scanner, "handover-scanner");
    }

    #[tokio::test]
    async fn stub_reports_disabled_when_roots_empty() {
        let mut cfg = HandoverScannerConfig::default();
        cfg.enabled = true;
        cfg.roots = Vec::new();
        let s = HandoverScanner::new(cfg);
        assert!(!s.enabled(), "empty roots must disable the scanner");
    }

    #[tokio::test]
    async fn stub_reports_disabled_when_flag_off() {
        let mut cfg = HandoverScannerConfig::default();
        cfg.enabled = false;
        cfg.roots = vec![PathBuf::from("/tmp")];
        let s = HandoverScanner::new(cfg);
        assert!(!s.enabled(), "enabled=false must disable the scanner");
    }
}
