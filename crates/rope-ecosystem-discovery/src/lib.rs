//! `rope-ecosystem-discovery` - autonomous discovery scanner for the
//! Datachain Rope ecosystem directory.
//!
//! Runs on a schedule (default 15 min) and scans multiple sources:
//!
//! - **HandoverScanner** - walks `.cursor/rules/handover-*.mdc` files in
//!   the local repo and extracts referenced projects (Step 2 - stub in
//!   this drop).
//! - **OnchainScanner** - reads labelled contract addresses from
//!   `https://dcscan.io/api/v1/labels` and emits one card per non-noise
//!   category.
//! - **PartnerApiScanner** - fetches well-known partner APIs from a
//!   hard host allowlist (Tanastok, DCSwap, Datawallet+, ...) (Step 2 -
//!   stub in this drop).
//!
//! Every scanner produces `OverlayEntry` values that are validated
//! locally, then merged and written atomically to a JSONL file that
//! `rope-explorer::ecosystem_overlay` consumes. Precedence in the
//! merged directory stays **EDC-registered > canonical > overlay**, so
//! this crate can never override a canonical operator decision.
//!
//! # Module layout
//!
//! ```text
//! config    - TOML config loading + validation
//! entry     - `OverlayEntry` type + wire-compatible enums
//! error     - `DiscoveryError` + `DiscoveryResult<T>`
//! scanners  - `Scanner` trait + shared helpers
//!   ::handover     - handover markdown scanner
//!   ::onchain      - dcscan `/api/v1/labels` scanner
//!   ::partner_api  - partner directory API scanner
//! writer    - atomic JSONL writer (tmp + fsync + rename)
//! ```
//!
//! # Wire contract
//!
//! Every emitted entry MUST validate against the loader in
//! `rope-explorer::ecosystem_overlay::RawOverlayEntry`. The spec at
//! `docs/ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` is the authoritative
//! source of truth; the constants in `scanners::mod` mirror the
//! loader's constants and are covered by unit tests that keep the two
//! in lock-step.

pub mod config;
pub mod entry;
pub mod error;
pub mod scanners;
pub mod writer;

pub use config::{DiscoveryConfig, DEFAULT_CONFIG_PATH, DEFAULT_OVERLAY_PATH};
pub use entry::{DiscoveredBy, OverlayEntry, Status, Visibility};
pub use error::{DiscoveryError, DiscoveryResult};
pub use scanners::{ScanResult, Scanner};
pub use writer::{write_overlay_atomic, WriteSummary};

use tracing::{info, warn};

/// Run one full discovery pass across every configured scanner and
/// atomically rewrite the overlay JSONL file.
///
/// Design:
///
/// - Every scanner is polled independently; a failing scanner is
///   logged and skipped, it never aborts the whole pass.
/// - Results are concatenated in scanner order (handover → onchain →
///   partner-api). The writer dedupes by lowercase `id` with
///   first-seen-wins, so ordering matters for equal-id collisions.
/// - The atomic writer only rewrites the final file after every entry
///   has been serialised successfully, so a partial failure cannot
///   leave a truncated overlay on disk.
pub async fn run_once(config: &DiscoveryConfig) -> DiscoveryResult<WriteSummary> {
    let http_timeout = config.http_timeout();
    let mut all_entries: Vec<OverlayEntry> = Vec::new();
    let mut scanners_run = 0usize;
    let mut scanners_ok = 0usize;

    // Handover scanner (Step 1: stub returns empty).
    let handover = scanners::handover::HandoverScanner::new(config.handover.clone());
    if handover.enabled() {
        scanners_run += 1;
        match handover.scan().await {
            Ok(r) => {
                scanners_ok += 1;
                info!(
                    "scanner={} considered={} emitted={} rejected={}",
                    r.scanner,
                    r.raw_considered,
                    r.entries.len(),
                    r.raw_rejected
                );
                all_entries.extend(r.entries);
            }
            Err(e) => warn!("scanner=handover-scanner failed: {}", e),
        }
    }

    // On-chain scanner (real implementation).
    if config.onchain.enabled {
        let onchain =
            scanners::onchain::OnchainScanner::new(config.onchain.clone(), http_timeout)?;
        if onchain.enabled() {
            scanners_run += 1;
            match onchain.scan().await {
                Ok(r) => {
                    scanners_ok += 1;
                    info!(
                        "scanner={} considered={} emitted={} rejected={}",
                        r.scanner,
                        r.raw_considered,
                        r.entries.len(),
                        r.raw_rejected
                    );
                    all_entries.extend(r.entries);
                }
                Err(e) => warn!("scanner=onchain-scanner failed: {}", e),
            }
        }
    }

    // Partner-API scanner (Step 1: stub returns empty).
    let partner =
        scanners::partner_api::PartnerApiScanner::new(config.partner_api.clone(), http_timeout);
    if partner.enabled() {
        scanners_run += 1;
        match partner.scan().await {
            Ok(r) => {
                scanners_ok += 1;
                info!(
                    "scanner={} considered={} emitted={} rejected={}",
                    r.scanner,
                    r.raw_considered,
                    r.entries.len(),
                    r.raw_rejected
                );
                all_entries.extend(r.entries);
            }
            Err(e) => warn!("scanner=partner-api-scanner failed: {}", e),
        }
    }

    info!(
        "discovery pass: scanners_run={} scanners_ok={} entries_total={}",
        scanners_run,
        scanners_ok,
        all_entries.len()
    );

    let summary = write_overlay_atomic(&all_entries, &config.output_path)?;
    info!(
        "overlay written: path={} input={} written={} deduped={} bytes={}",
        config.output_path.display(),
        summary.input_count,
        summary.written_count,
        summary.deduped_count,
        summary.bytes_written
    );
    Ok(summary)
}
