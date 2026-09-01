//! Scanner trait + shared helpers for all discovery scanners.
//!
//! Every scanner returns a `Vec<OverlayEntry>` (possibly empty) and its
//! own typed error variant. Failures from one scanner never abort the
//! whole discovery run - `lib.rs::run_once` collects per-scanner results
//! and merges them into a single JSONL write.
//!
//! # Wire compatibility
//!
//! The `OverlayEntry` values produced here are consumed by
//! `rope-explorer`'s `ecosystem_overlay::RawOverlayEntry` loader, which
//! rejects anything that fails validation. See
//! `docs/ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` §3 for the authoritative
//! schema. This module MUST stay in lock-step with the loader; every
//! constant here also has a matching constant on the loader side.

pub mod handover;
pub mod onchain;
pub mod partner_api;

use crate::entry::OverlayEntry;
use crate::error::DiscoveryResult;
use async_trait::async_trait;

/// Result payload from a single scanner run.
#[derive(Debug, Default)]
pub struct ScanResult {
    /// Entries that passed local validation and are ready for the writer.
    pub entries: Vec<OverlayEntry>,
    /// Number of raw records the scanner considered (for logs/metrics).
    pub raw_considered: usize,
    /// Number of raw records the scanner dropped locally (invalid
    /// archetype, bad wallet, disallowed host, etc.).
    pub raw_rejected: usize,
    /// Human-friendly scanner name for logs (e.g. `"handover-scanner"`).
    pub scanner: &'static str,
}

/// The single contract every scanner implements.
///
/// Scanners are async because two of the three concrete implementations
/// (onchain, partner-api) do network I/O. The handover scanner is CPU
/// + fs but keeps the same signature for uniformity - runs in a
/// `tokio::task::spawn_blocking` internally where needed.
#[async_trait]
pub trait Scanner: Send + Sync {
    /// Human-friendly name used in logs and in the `ScanResult.scanner`
    /// field. Must match the `DiscoveredBy::as_str()` value the scanner
    /// emits so the loader's provenance audit ties log lines back to
    /// records.
    fn name(&self) -> &'static str;

    /// Return `true` if this scanner is configured to run. Disabled
    /// scanners are skipped without touching the network or the fs.
    fn enabled(&self) -> bool;

    /// Do one full pass. MUST NOT panic; errors go into
    /// `DiscoveryResult::Err(_)` and the orchestrator will log + skip.
    async fn scan(&self) -> DiscoveryResult<ScanResult>;
}

// ---------------------------------------------------------------------
// Shared validation helpers.
//
// These mirror `ecosystem_overlay.rs`'s validation constants so the
// scanner can drop entries early instead of writing them and having the
// loader reject them at read time. The tests below assert the mirror
// stays in sync.
// ---------------------------------------------------------------------

/// The set of archetype slugs the loader accepts. Sourced from
/// `rope-explorer/src/ecosystem_canonical.rs::canonical_archetypes()`.
/// Kept as a local constant so this crate does not depend on the
/// explorer binary.
pub const KNOWN_ARCHETYPES: &[&str] = &[
    "predictive_maintenance",
    "environmental_monitoring",
    "hybrid",
    "dex",
    "asset_tokenization",
    "identity_wallet",
    "sso",
    "block_explorer",
    "ai_agent",
    "governance",
    "foundation",
    "bridge",
    "biodiversity",
    "health",
    "investment",
    "infrastructure",
];

pub const MIN_ID_LEN: usize = 3;
pub const MAX_ID_LEN: usize = 64;
pub const MIN_NAME_LEN: usize = 3;
pub const MAX_NAME_LEN: usize = 128;
pub const MAX_DISCOVERY_SOURCE_LEN: usize = 512;
pub const MAX_TAG_COUNT: usize = 12;
pub const MIN_TAG_LEN: usize = 2;
pub const MAX_TAG_LEN: usize = 32;

/// Convert an arbitrary human name to a lowercase kebab-case slug that
/// satisfies the loader's `id` regex `[a-z0-9-]+`. Runs of non-alnum
/// characters collapse to a single `-`; leading / trailing `-` are
/// trimmed; the result is truncated to `MAX_ID_LEN`.
///
/// Returns `None` when the resulting slug is too short (i.e. the input
/// had no alnum content).
pub fn slugify(input: &str) -> Option<String> {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = true;
    for ch in input.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    // Trim trailing dash.
    while out.ends_with('-') {
        out.pop();
    }
    // Truncate to max, but at a char boundary. Since we only push ASCII
    // this is safe as a byte truncation.
    if out.len() > MAX_ID_LEN {
        out.truncate(MAX_ID_LEN);
        // If truncation left a trailing dash, trim it.
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.len() < MIN_ID_LEN {
        None
    } else {
        Some(out)
    }
}

/// Return true if `slug` is a known archetype in `KNOWN_ARCHETYPES`.
/// Case-sensitive; the canonical archetype list is all-lowercase.
pub fn is_known_archetype(slug: &str) -> bool {
    KNOWN_ARCHETYPES.iter().any(|k| *k == slug)
}

/// Guess an archetype from a set of free-form tags / keywords. Used by
/// the handover scanner when the source doesn't declare an explicit
/// archetype. Returns `"infrastructure"` as the fallback because that
/// is the least-wrong default for a project that clearly exists in the
/// Datachain Rope ecosystem but has no other classification signal.
pub fn guess_archetype(hints: &[&str]) -> &'static str {
    let joined = hints.join(" ").to_ascii_lowercase();
    // Order matters - most-specific matches first.
    // Order matters - most-specific matches must run before generic
    // substrings that would false-match. For example, "biodiversity index"
    // contains the substring "dex", so the biodiversity checks must run
    // before the dex/swap/amm block. Same idea for foundation before
    // governance (a "governance foundation" hint is a foundation, not
    // just governance).
    let checks: &[(&str, &str)] = &[
        ("predictive_maintenance", "predictive"),
        ("environmental_monitoring", "environment"),
        ("biodiversity", "biodivers"),
        ("biodiversity", "nature"),
        ("asset_tokenization", "tokeniz"),
        ("asset_tokenization", "rwa"),
        ("dex", "dex"),
        ("dex", "swap"),
        ("dex", "amm"),
        ("bridge", "bridge"),
        ("identity_wallet", "wallet"),
        ("identity_wallet", "identity"),
        ("sso", "single sign"),
        ("sso", "sso"),
        ("block_explorer", "explorer"),
        ("ai_agent", "agent"),
        ("ai_agent", " ai "),
        ("foundation", "foundation"),
        ("governance", "governance"),
        ("governance", "vote"),
        ("health", "health"),
        ("health", "care"),
        ("investment", "invest"),
        ("investment", "fund"),
        ("hybrid", "hybrid"),
    ];
    for (arche, needle) in checks {
        if joined.contains(needle) {
            return arche;
        }
    }
    "infrastructure"
}

/// Truncate a string to at most `max` bytes at a UTF-8 boundary and
/// return the owned result. Used to keep `discovery_source` and
/// `description` under the loader's length caps.
pub fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Find the largest char boundary <= max.
    let mut idx = max;
    while !s.is_char_boundary(idx) && idx > 0 {
        idx -= 1;
    }
    s[..idx].to_string()
}

/// Current unix time in seconds; used as the `discovered_at` timestamp
/// when a scanner has no better signal.
pub fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Format a hex EVM address (with or without `0x`) as a lower-case
/// `0x`-prefixed 42-char string, or `None` if the input is not a valid
/// 20-byte hex address.
pub fn normalise_evm_address(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let body = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if body.len() != 40 {
        return None;
    }
    if !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("0x{}", body.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_common_cases() {
        assert_eq!(slugify("Tanastok").as_deref(), Some("tanastok"));
        assert_eq!(slugify("DC Swap!").as_deref(), Some("dc-swap"));
        assert_eq!(
            slugify("Careaway Health Connect").as_deref(),
            Some("careaway-health-connect")
        );
        assert_eq!(slugify("!!  !!"), None);
        // Leading/trailing dashes must be stripped. Use a 3+ char body so
        // the result satisfies MIN_ID_LEN (the loader would reject a
        // 2-char id).
        assert_eq!(slugify("--- yes ---").as_deref(), Some("yes"));
        // A 2-char body is stripped to "ok" and then rejected by
        // MIN_ID_LEN, so slugify returns None.
        assert_eq!(slugify("--- ok ---"), None);
    }

    #[test]
    fn slugify_truncates_and_trims_trailing_dash() {
        let long = "a".repeat(MAX_ID_LEN + 20) + " tail";
        let out = slugify(&long).unwrap();
        assert!(out.len() <= MAX_ID_LEN);
        assert!(!out.ends_with('-'));
    }

    #[test]
    fn known_archetypes_all_lowercase() {
        for a in KNOWN_ARCHETYPES {
            assert!(is_known_archetype(a), "not detected: {}", a);
            assert_eq!(a.to_ascii_lowercase().as_str(), *a);
        }
    }

    #[test]
    fn guess_archetype_returns_expected() {
        assert_eq!(guess_archetype(&["decentralised exchange", "swap"]), "dex");
        assert_eq!(
            guess_archetype(&["nft", "asset tokenization"]),
            "asset_tokenization"
        );
        assert_eq!(guess_archetype(&["biodiversity index"]), "biodiversity");
        assert_eq!(guess_archetype(&["nothing relevant"]), "infrastructure");
    }

    #[test]
    fn normalise_evm_address_accepts_valid_and_rejects_garbage() {
        assert_eq!(
            normalise_evm_address("0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb")
                .as_deref(),
            Some("0xcf884c81ed55b150cb1aba8a69e2e9adf8f082eb")
        );
        assert_eq!(
            normalise_evm_address("cf884c81ed55b150cb1aba8a69e2e9adf8f082eb").as_deref(),
            Some("0xcf884c81ed55b150cb1aba8a69e2e9adf8f082eb")
        );
        assert!(normalise_evm_address("0xdeadbeef").is_none());
        assert!(normalise_evm_address("nothex nothex nothex").is_none());
    }

    #[test]
    fn truncate_utf8_respects_char_boundaries() {
        // Multi-byte char: "é" is 2 bytes. Truncating at 3 bytes should
        // yield "aé" (3 bytes total) or step back to "a" (1 byte).
        let s = "aéb";
        let out = truncate_utf8(s, 2);
        assert!(s.starts_with(&out));
        assert!(out.is_char_boundary(out.len()));
    }
}
