//! Overlay loader for the ecosystem directory.
//!
//! Reads a JSONL file at `ECOSYSTEM_OVERLAY_PATH` (default
//! `/var/lib/rope-explorer/ecosystem-overlay.jsonl`) and returns a
//! vector of card-shaped `serde_json::Value` entries that plug
//! directly into `refresh_ecosystem_directory_cache`'s dedupe pass in
//! `main.rs`.
//!
//! The overlay is the third source in the ecosystem precedence chain
//! (EDC > canonical > overlay); the loader itself does not enforce
//! precedence - the caller does. The loader IS responsible for
//! enforcing visibility precedence: if an overlay entry's `id` matches
//! a canonical `PRIVATE_HIDDEN_IDS` or `PRIVATE_VISIBLE_IDS` entry, the
//! canonical visibility wins regardless of what the overlay file says.
//! This prevents an attacker who writes to the overlay file from
//! un-hiding a project the operator wants hidden.
//!
//! Full contract lives in `docs/ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md`.

use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::ecosystem_canonical::{
    canonical_archetypes, visibility_for as canonical_visibility_for, Visibility,
};

/// Default overlay file location. Overridable via
/// `ECOSYSTEM_OVERLAY_PATH` (must be an absolute path).
pub const DEFAULT_OVERLAY_PATH: &str = "/var/lib/rope-explorer/ecosystem-overlay.jsonl";

/// Env variable that overrides the default path.
pub const OVERLAY_PATH_ENV: &str = "ECOSYSTEM_OVERLAY_PATH";

/// Hard cap on entries per file. Guards against a runaway writer bug.
/// Anything above this and the loader returns empty with an error log.
const MAX_ENTRIES_PER_FILE: usize = 1_000;

/// Hard cap on individual line length in bytes. Larger lines are
/// dropped with a warn.
const MAX_LINE_BYTES: usize = 8 * 1024; // 8 KiB

/// Hard cap on file size in bytes. Larger files cause the loader to
/// return empty with an error (recommend operator rewrite).
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024; // 8 MiB

/// Hard cap on description length in characters. Longer descriptions
/// are truncated with an ellipsis.
const MAX_DESCRIPTION_CHARS: usize = 500;

/// Hard cap on tag length in characters (per tag).
const MAX_TAG_LEN: usize = 32;

/// Minimum tag length in characters (per tag).
const MIN_TAG_LEN: usize = 2;

/// Hard cap on tag count per entry.
const MAX_TAGS: usize = 12;

/// Hard cap on asset/sensor counts. Beyond this the entry is dropped.
const MAX_COUNT: u64 = 10_000_000;

/// Minimum id length in characters.
const MIN_ID_LEN: usize = 3;

/// Maximum id length in characters.
const MAX_ID_LEN: usize = 64;

/// Minimum name length in characters.
const MIN_NAME_LEN: usize = 3;

/// Maximum name length in characters.
const MAX_NAME_LEN: usize = 128;

/// Minimum discovery_source length in characters.
const MIN_DISCOVERY_SOURCE_LEN: usize = 3;

/// Maximum discovery_source length in characters.
const MAX_DISCOVERY_SOURCE_LEN: usize = 512;

/// Wire-format struct that mirrors what the discovery script writes.
/// All optional fields default to `None` so a minimal entry
/// (id/name/archetype/status/discovered_*) parses cleanly.
#[derive(Debug, Clone, Deserialize)]
struct RawOverlayEntry {
    id: String,
    name: String,
    archetype: String,
    status: String,
    discovered_by: String,
    discovery_source: String,
    discovered_at: i64,

    #[serde(default)]
    tags: Vec<String>,

    #[serde(default)]
    region: Option<String>,

    #[serde(default)]
    country: Option<String>,

    #[serde(default)]
    wallet: Option<String>,

    #[serde(default)]
    stakeholder_url: Option<String>,

    #[serde(default)]
    description: Option<String>,

    #[serde(default)]
    asset_count: Option<u64>,

    #[serde(default)]
    sensor_count: Option<u64>,

    #[serde(default)]
    logo_url: Option<String>,

    #[serde(default)]
    created_at: Option<i64>,

    #[serde(default)]
    visibility: Option<String>,
}

/// Reasons a raw entry can be rejected during validation. Not
/// currently exposed to callers, but the loader logs a warn for each
/// non-`Ok` classification.
#[derive(Debug, PartialEq, Eq)]
enum RejectReason {
    IdLengthOutOfRange,
    IdCharset,
    NameLengthOutOfRange,
    UnknownArchetype,
    UnknownStatus,
    UnknownDiscoveredBy,
    DiscoverySourceLengthOutOfRange,
    DiscoveredAtNegative,
    TooManyTags,
    TagLengthOutOfRange,
    TagCharset,
    RegionLengthOutOfRange,
    CountryFormat,
    WalletFormat,
    UrlNotHttps,
    UrlNotAbsolute,
    AssetCountTooLarge,
    SensorCountTooLarge,
    CreatedAtNegative,
    LineTooLong,
    Malformed(String),
}

/// Resolve the overlay path from env with the documented fallback.
fn overlay_path() -> PathBuf {
    std::env::var(OVERLAY_PATH_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_OVERLAY_PATH))
}

/// Public entry point: load the overlay from the env-configured path
/// and return an ordered vec of card-shaped values. On any error
/// (missing file, permission denied, oversized file, unreadable),
/// returns an empty vec and logs at the appropriate `tracing` level.
///
/// This is safe to call on every refresh tick - it re-reads the file
/// each time so operator/script appends are visible on the next tick
/// without a service restart.
pub fn load_overlay_cards() -> Vec<Value> {
    let path = overlay_path();
    load_overlay_cards_from(&path)
}

/// Test-friendly variant that accepts an explicit path.
pub fn load_overlay_cards_from(path: &Path) -> Vec<Value> {
    // Missing file is normal (the discovery script may not have
    // deployed yet). Log once at debug, not warn.
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                "ecosystem overlay: file not present at {} (this is normal)",
                path.display()
            );
            return Vec::new();
        }
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            tracing::warn!(
                "ecosystem overlay: permission denied reading {}: {err}",
                path.display()
            );
            return Vec::new();
        }
        Err(err) => {
            tracing::warn!(
                "ecosystem overlay: stat failed for {}: {err}",
                path.display()
            );
            return Vec::new();
        }
    };

    if metadata.len() > MAX_FILE_BYTES {
        tracing::error!(
            "ecosystem overlay: file at {} is {} bytes (cap {}); refusing to load",
            path.display(),
            metadata.len(),
            MAX_FILE_BYTES
        );
        return Vec::new();
    }

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(err) => {
            tracing::warn!(
                "ecosystem overlay: open failed for {}: {err}",
                path.display()
            );
            return Vec::new();
        }
    };

    let mut reader = BufReader::new(file);
    let mut raw_line = String::new();
    let mut cards: Vec<Value> = Vec::new();
    let mut seen_ids: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut line_no: usize = 0;

    loop {
        raw_line.clear();
        let read = match reader.read_line(&mut raw_line) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) => {
                tracing::warn!(
                    "ecosystem overlay: read error at line {} in {}: {err}",
                    line_no + 1,
                    path.display()
                );
                break;
            }
        };
        line_no += 1;

        // Trim only the trailing newline, keep interior whitespace so
        // the byte-length check reflects on-disk size.
        let line = raw_line.trim_end_matches(&['\n', '\r'][..]);

        if line.is_empty() {
            continue;
        }

        if read > MAX_LINE_BYTES {
            tracing::warn!(
                "ecosystem overlay: line {} in {} is {} bytes (cap {}); dropped",
                line_no,
                path.display(),
                read,
                MAX_LINE_BYTES
            );
            continue;
        }

        let raw: RawOverlayEntry = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(err) => {
                tracing::warn!(
                    "ecosystem overlay: line {} in {} is malformed JSON: {err}",
                    line_no,
                    path.display()
                );
                continue;
            }
        };

        let card = match validate_and_shape(raw) {
            Ok(c) => c,
            Err(reason) => {
                tracing::warn!(
                    "ecosystem overlay: line {} in {} rejected: {:?}",
                    line_no,
                    path.display(),
                    reason
                );
                continue;
            }
        };

        let id = card
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }

        // Last-write-wins for duplicate ids in the same file.
        if let Some(&existing_idx) = seen_ids.get(&id) {
            tracing::debug!(
                "ecosystem overlay: line {} in {} overrides earlier entry for id={}",
                line_no,
                path.display(),
                id
            );
            cards[existing_idx] = card;
        } else {
            seen_ids.insert(id, cards.len());
            cards.push(card);
        }

        if cards.len() >= MAX_ENTRIES_PER_FILE {
            tracing::error!(
                "ecosystem overlay: hit entry cap ({}) at line {} in {}; remaining lines ignored",
                MAX_ENTRIES_PER_FILE,
                line_no,
                path.display()
            );
            break;
        }
    }

    tracing::debug!(
        "ecosystem overlay: loaded {} card(s) from {}",
        cards.len(),
        path.display()
    );

    cards
}

/// Validate a raw entry and shape it into a card-equivalent JSON
/// object matching `ecosystem_canonical::entry_to_card`.
fn validate_and_shape(raw: RawOverlayEntry) -> Result<Value, RejectReason> {
    // id: lowercase slug [a-z0-9-]+, 3-64 chars.
    let id = raw.id.trim().to_lowercase();
    if id.len() < MIN_ID_LEN || id.len() > MAX_ID_LEN {
        return Err(RejectReason::IdLengthOutOfRange);
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(RejectReason::IdCharset);
    }

    // name: 3-128 chars, trimmed.
    let name = raw.name.trim().to_string();
    if name.chars().count() < MIN_NAME_LEN || name.chars().count() > MAX_NAME_LEN {
        return Err(RejectReason::NameLengthOutOfRange);
    }

    // archetype: must be in the canonical archetype list.
    let archetype = raw.archetype.trim().to_lowercase();
    let known = canonical_archetypes();
    if !known.iter().any(|a| *a == archetype.as_str()) {
        return Err(RejectReason::UnknownArchetype);
    }

    // status: one of the four allowed strings.
    let status = raw.status.trim().to_lowercase();
    if !matches!(
        status.as_str(),
        "live" | "development" | "sandbox" | "archived"
    ) {
        return Err(RejectReason::UnknownStatus);
    }

    // discovered_by: one of the documented scanners.
    let discovered_by = raw.discovered_by.trim().to_lowercase();
    if !matches!(
        discovered_by.as_str(),
        "handover-scanner" | "onchain-scanner" | "partner-api-scanner" | "manual"
    ) {
        return Err(RejectReason::UnknownDiscoveredBy);
    }

    // discovery_source: 3-512 chars, kept verbatim (case-preserving,
    // may contain URLs or file paths).
    let discovery_source = raw.discovery_source.trim().to_string();
    if discovery_source.len() < MIN_DISCOVERY_SOURCE_LEN
        || discovery_source.len() > MAX_DISCOVERY_SOURCE_LEN
    {
        return Err(RejectReason::DiscoverySourceLengthOutOfRange);
    }

    if raw.discovered_at < 0 {
        return Err(RejectReason::DiscoveredAtNegative);
    }

    // tags: up to 12, each 2-32 chars, ASCII lowercase + digits + '-'.
    if raw.tags.len() > MAX_TAGS {
        return Err(RejectReason::TooManyTags);
    }
    let mut tags_out: Vec<String> = Vec::with_capacity(raw.tags.len());
    for tag in raw.tags {
        let t = tag.trim().to_lowercase();
        if t.chars().count() < MIN_TAG_LEN || t.chars().count() > MAX_TAG_LEN {
            return Err(RejectReason::TagLengthOutOfRange);
        }
        if !t
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(RejectReason::TagCharset);
        }
        tags_out.push(t);
    }

    // region: 3-64 chars or defaults to "Global".
    let region = match raw.region {
        Some(r) => {
            let r = r.trim().to_string();
            if r.chars().count() < 3 || r.chars().count() > 64 {
                return Err(RejectReason::RegionLengthOutOfRange);
            }
            r
        }
        None => "Global".to_string(),
    };

    // country: ISO 3166-1 alpha-2 (2 chars uppercase A-Z) OR "GLOBAL".
    let country = match raw.country {
        Some(c) => {
            let c = c.trim().to_uppercase();
            if c == "GLOBAL" {
                c
            } else if c.len() == 2 && c.chars().all(|ch| ch.is_ascii_uppercase()) {
                c
            } else {
                return Err(RejectReason::CountryFormat);
            }
        }
        None => "GLOBAL".to_string(),
    };

    // wallet: EVM address (lowercase) OR empty.
    let wallet = match raw.wallet {
        Some(w) => {
            let w = w.trim().to_lowercase();
            if w.is_empty() {
                String::new()
            } else if is_valid_evm_address(&w) {
                w
            } else {
                return Err(RejectReason::WalletFormat);
            }
        }
        None => String::new(),
    };

    // stakeholder_url: absolute https:// OR None.
    let stakeholder_url = match raw.stakeholder_url {
        Some(u) => {
            let u = u.trim().to_string();
            if u.is_empty() {
                None
            } else {
                validate_https_absolute(&u)?;
                Some(u)
            }
        }
        None => None,
    };

    // logo_url: absolute https:// OR None.
    let logo_url = match raw.logo_url {
        Some(u) => {
            let u = u.trim().to_string();
            if u.is_empty() {
                None
            } else {
                validate_https_absolute(&u)?;
                Some(u)
            }
        }
        None => None,
    };

    // description: up to 500 chars (truncated with ellipsis if longer).
    let description = match raw.description {
        Some(d) => {
            let d = d.trim().to_string();
            truncate_chars(&d, MAX_DESCRIPTION_CHARS)
        }
        None => String::new(),
    };

    // asset_count, sensor_count: bounded non-negative integers.
    let asset_count = match raw.asset_count {
        Some(n) if n > MAX_COUNT => return Err(RejectReason::AssetCountTooLarge),
        Some(n) => n,
        None => 0,
    };
    let sensor_count = match raw.sensor_count {
        Some(n) if n > MAX_COUNT => return Err(RejectReason::SensorCountTooLarge),
        Some(n) => n,
        None => 0,
    };

    // created_at: fall back to discovered_at when absent. Reject
    // explicitly negative values.
    let created_at = match raw.created_at {
        Some(n) if n < 0 => return Err(RejectReason::CreatedAtNegative),
        Some(n) => n,
        None => raw.discovered_at,
    };

    // Visibility precedence: canonical wins over overlay. If the id is
    // in PRIVATE_HIDDEN_IDS or PRIVATE_VISIBLE_IDS, use the canonical
    // visibility; otherwise honour what the overlay declares.
    let canonical_v = canonical_visibility_for(&id);
    let effective_visibility = match canonical_v {
        Visibility::Public => match raw.visibility.as_deref().map(str::to_lowercase) {
            Some(ref s) if s == "public" => Visibility::Public,
            Some(ref s) if s == "private_visible" => Visibility::PrivateVisible,
            Some(ref s) if s == "private_hidden" => Visibility::PrivateHidden,
            Some(_) | None => Visibility::Public,
        },
        // Canonical wins - overlay cannot un-hide.
        Visibility::PrivateVisible => Visibility::PrivateVisible,
        Visibility::PrivateHidden => Visibility::PrivateHidden,
    };

    // Build the card. Shape matches ecosystem_canonical::entry_to_card
    // exactly, plus three overlay-only fields (`discovered_at`,
    // `discovered_by`, `discovery_source`) so the frontend or an audit
    // tool can trace where the entry came from.
    let card = json!({
        "id": id,
        "name": name,
        "archetype": archetype,
        "status": status,
        "tags": tags_out,
        "region": region,
        "country": country,
        "wallet": wallet,
        "stakeholder_url": stakeholder_url,
        "description": description,
        "asset_count": asset_count,
        "sensor_count": sensor_count,
        "created_at": created_at,
        // Loader ALWAYS emits "overlay:<discovered_by>" regardless of
        // what the writer set - clean audit trail from source.
        "source": format!("overlay:{}", discovered_by),
        // Overlay entries never come from an EDC instance.
        "edc_base": Value::Null,
        "logo_url": logo_url,
        "visibility": effective_visibility.as_str(),
        // Overlay-only provenance fields (not present on canonical or
        // EDC cards). Consumers that don't care can ignore them.
        "discovered_at": raw.discovered_at,
        "discovered_by": discovered_by,
        "discovery_source": discovery_source,
    });

    Ok(card)
}

/// Validate an https absolute URL. Rejects `http://`, other schemes,
/// and non-absolute (protocol-relative) paths. Deliberately does NOT
/// pull a URL parser dep for this - we want a strict, boring check.
fn validate_https_absolute(url: &str) -> Result<(), RejectReason> {
    if !url.starts_with("https://") {
        // Distinguish "wrong scheme (http)" from "not absolute at all"
        // so operators can diagnose from the log.
        if url.starts_with("http://") {
            return Err(RejectReason::UrlNotHttps);
        }
        return Err(RejectReason::UrlNotAbsolute);
    }
    // Must have something after `https://`.
    if url.len() <= "https://".len() {
        return Err(RejectReason::UrlNotAbsolute);
    }
    Ok(())
}

/// Check for a well-formed EVM address (already lowercased).
fn is_valid_evm_address(addr: &str) -> bool {
    if addr.len() != 42 {
        return false;
    }
    if !addr.starts_with("0x") {
        return false;
    }
    // Enforce lowercase hex to match the "already lowercased" invariant.
    // `validate_and_shape` lowercases the wallet before calling this; the
    // strict check here catches any future call site that forgets to.
    addr[2..]
        .chars()
        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Character-count-bounded truncate (not byte-bounded). Appends
/// ellipsis when truncation happens so operators can spot it in logs.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_overlay(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("tempfile");
        f.write_all(content.as_bytes()).expect("write");
        f.flush().expect("flush");
        f
    }

    fn valid_entry_line(id: &str) -> String {
        // Uses "ai_agent" as the archetype because it's in the
        // canonical archetype list and unambiguous. Timestamps chosen
        // to fall inside sensible i64 range.
        format!(
            r#"{{"id":"{id}","name":"Test {id}","archetype":"ai_agent","status":"development","tags":["ai"],"discovered_at":1786500000,"discovered_by":"handover-scanner","discovery_source":"handover-file:test.mdc"}}
"#
        )
    }

    #[test]
    fn load_missing_file_returns_empty_ok() {
        let cards = load_overlay_cards_from(Path::new("/nonexistent-path-that-does-not-exist"));
        assert_eq!(cards, Vec::<Value>::new());
    }

    #[test]
    fn load_empty_file_returns_empty_ok() {
        let f = make_overlay("");
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards, Vec::<Value>::new());
    }

    #[test]
    fn load_single_valid_entry_returns_one_card() {
        let f = make_overlay(&valid_entry_line("newapp-2026"));
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards.len(), 1);
        let c = &cards[0];
        assert_eq!(c.get("id").and_then(|v| v.as_str()), Some("newapp-2026"));
        assert_eq!(c.get("name").and_then(|v| v.as_str()), Some("Test newapp-2026"));
        assert_eq!(c.get("archetype").and_then(|v| v.as_str()), Some("ai_agent"));
        assert_eq!(c.get("status").and_then(|v| v.as_str()), Some("development"));
    }

    #[test]
    fn load_appends_source_overlay_prefix() {
        let f = make_overlay(&valid_entry_line("app-1"));
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards.len(), 1);
        assert_eq!(
            cards[0].get("source").and_then(|v| v.as_str()),
            Some("overlay:handover-scanner")
        );
    }

    #[test]
    fn load_ignores_malformed_json_lines() {
        let content = format!(
            "not valid json\n{}\nalso not valid\n{}\n",
            valid_entry_line("app-a"),
            valid_entry_line("app-b")
        );
        let f = make_overlay(&content);
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards.len(), 2);
        let ids: Vec<&str> = cards
            .iter()
            .filter_map(|c| c.get("id").and_then(|v| v.as_str()))
            .collect();
        assert!(ids.contains(&"app-a"));
        assert!(ids.contains(&"app-b"));
    }

    #[test]
    fn load_drops_entries_missing_required_fields() {
        // Missing "archetype"
        let f = make_overlay(
            r#"{"id":"broken","name":"broken","status":"live","discovered_at":1,"discovered_by":"manual","discovery_source":"test"}
"#,
        );
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards, Vec::<Value>::new());
    }

    #[test]
    fn load_drops_entries_with_unknown_archetype() {
        let f = make_overlay(
            r#"{"id":"bad","name":"bad","archetype":"quantum-mystery","status":"live","discovered_at":1,"discovered_by":"manual","discovery_source":"test"}
"#,
        );
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards, Vec::<Value>::new());
    }

    #[test]
    fn load_drops_entries_with_unknown_status() {
        let f = make_overlay(
            r#"{"id":"bad","name":"bad","archetype":"ai_agent","status":"maybe","discovered_at":1,"discovered_by":"manual","discovery_source":"test"}
"#,
        );
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards, Vec::<Value>::new());
    }

    #[test]
    fn load_drops_entries_over_max_line_length() {
        let long_desc = "x".repeat(MAX_LINE_BYTES + 100);
        let content = format!(
            r#"{{"id":"toolong","name":"toolong","archetype":"ai_agent","status":"live","discovered_at":1,"discovered_by":"manual","discovery_source":"{long_desc}"}}
"#
        );
        let f = make_overlay(&content);
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards, Vec::<Value>::new());
    }

    #[test]
    fn load_caps_at_max_entries_per_file() {
        // Write MAX_ENTRIES_PER_FILE + 5 valid entries with unique ids.
        let mut content = String::new();
        for i in 0..(MAX_ENTRIES_PER_FILE + 5) {
            content.push_str(&valid_entry_line(&format!("app-{i:04}")));
        }
        let f = make_overlay(&content);
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards.len(), MAX_ENTRIES_PER_FILE);
    }

    #[test]
    fn load_last_write_wins_on_duplicate_id() {
        // Two entries with same id, second one has a different name.
        let content = format!(
            r#"{}{{"id":"dup","name":"Second","archetype":"ai_agent","status":"live","discovered_at":2,"discovered_by":"manual","discovery_source":"test-2"}}
"#,
            valid_entry_line("dup")
        );
        let f = make_overlay(&content);
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].get("name").and_then(|v| v.as_str()), Some("Second"));
    }

    #[test]
    fn load_hidden_id_visibility_is_enforced_from_canonical() {
        // "moneymaker" is in canonical PRIVATE_HIDDEN_IDS. Even if the
        // overlay declares "public", the loader must honour canonical.
        let content = r#"{"id":"moneymaker","name":"Attempted un-hide","archetype":"ai_agent","status":"development","discovered_at":1,"discovered_by":"manual","discovery_source":"attempt","visibility":"public"}
"#;
        let f = make_overlay(content);
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards.len(), 1);
        assert_eq!(
            cards[0].get("visibility").and_then(|v| v.as_str()),
            Some("private_hidden")
        );
    }

    #[test]
    fn load_rejects_http_scheme_in_urls() {
        let content = r#"{"id":"httpapp","name":"httpapp","archetype":"ai_agent","status":"live","discovered_at":1,"discovered_by":"manual","discovery_source":"test","stakeholder_url":"http://example.com"}
"#;
        let f = make_overlay(content);
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards, Vec::<Value>::new());
    }

    #[test]
    fn load_normalizes_wallet_address_lowercase() {
        let content = r#"{"id":"walletapp","name":"walletapp","archetype":"ai_agent","status":"live","discovered_at":1,"discovered_by":"manual","discovery_source":"test","wallet":"0xABCDEF1234567890ABCDEF1234567890ABCDEF12"}
"#;
        let f = make_overlay(content);
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards.len(), 1);
        assert_eq!(
            cards[0].get("wallet").and_then(|v| v.as_str()),
            Some("0xabcdef1234567890abcdef1234567890abcdef12")
        );
    }

    #[test]
    fn load_truncates_long_descriptions() {
        let long_desc = "d".repeat(600);
        let content = format!(
            r#"{{"id":"longdesc","name":"longdesc","archetype":"ai_agent","status":"live","discovered_at":1,"discovered_by":"manual","discovery_source":"test","description":"{long_desc}"}}
"#
        );
        let f = make_overlay(&content);
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards.len(), 1);
        let d = cards[0].get("description").and_then(|v| v.as_str()).unwrap();
        assert!(d.ends_with("..."));
        assert_eq!(d.chars().count(), MAX_DESCRIPTION_CHARS);
    }

    #[test]
    fn load_ignores_entries_over_max_file_size() {
        // A single line that's small but pushes the file over 8 MiB
        // when repeated. Simulate by writing an oversized file.
        let mut content = String::new();
        // Each valid line is ~200 bytes; 50_000 lines ≈ 10 MB.
        for i in 0..50_000 {
            content.push_str(&valid_entry_line(&format!("app-{i:05}")));
        }
        let f = make_overlay(&content);
        let cards = load_overlay_cards_from(f.path());
        // Loader refuses the whole file when it's over the size cap.
        assert_eq!(cards, Vec::<Value>::new());
    }

    #[test]
    fn card_shape_matches_canonical_entry_to_card_keys() {
        // Cross-check: overlay cards should carry every key that
        // canonical cards carry, so downstream frontend renderers
        // don't need special-case handling.
        let f = make_overlay(&valid_entry_line("shape-check"));
        let cards = load_overlay_cards_from(f.path());
        assert_eq!(cards.len(), 1);
        let card = cards[0].as_object().unwrap();
        // Every canonical key must be present.
        for k in [
            "id",
            "name",
            "archetype",
            "status",
            "tags",
            "region",
            "country",
            "wallet",
            "stakeholder_url",
            "description",
            "asset_count",
            "sensor_count",
            "created_at",
            "source",
            "edc_base",
            "logo_url",
            "visibility",
        ] {
            assert!(card.contains_key(k), "canonical key {k} missing on overlay card");
        }
        // Overlay-only provenance keys must ALSO be present.
        for k in ["discovered_at", "discovered_by", "discovery_source"] {
            assert!(card.contains_key(k), "overlay provenance key {k} missing");
        }
    }

    #[test]
    fn is_valid_evm_address_smoke() {
        assert!(is_valid_evm_address("0x0000000000000000000000000000000000000000"));
        assert!(is_valid_evm_address("0xabcdef1234567890abcdef1234567890abcdef12"));
        assert!(!is_valid_evm_address("0xABCDEF1234567890ABCDEF1234567890ABCDEF12")); // must be lowercase
        assert!(!is_valid_evm_address("0x123")); // too short
        assert!(!is_valid_evm_address("abcdef1234567890abcdef1234567890abcdef12")); // no 0x
        assert!(!is_valid_evm_address("0xzzzz1234567890abcdef1234567890abcdef1234")); // non-hex
    }

    #[test]
    fn validate_https_absolute_smoke() {
        assert!(validate_https_absolute("https://example.com").is_ok());
        assert!(validate_https_absolute("https://sub.example.com/a/b").is_ok());
        assert_eq!(
            validate_https_absolute("http://example.com"),
            Err(RejectReason::UrlNotHttps)
        );
        assert_eq!(
            validate_https_absolute("//example.com"),
            Err(RejectReason::UrlNotAbsolute)
        );
        assert_eq!(
            validate_https_absolute("https://"),
            Err(RejectReason::UrlNotAbsolute)
        );
    }

    #[test]
    fn truncate_chars_smoke() {
        assert_eq!(truncate_chars("hello", 100), "hello");
        assert_eq!(truncate_chars("hello", 5), "hello");
        assert_eq!(truncate_chars("hellothere", 5), "he...");
    }

    #[test]
    fn overlay_path_defaults_when_env_unset() {
        // Force-unset the env for this test.
        // SAFETY: unit tests don't share mutable env in cargo default;
        // the env-var read is a snapshot at call time.
        let key = OVERLAY_PATH_ENV;
        let saved = std::env::var(key).ok();
        std::env::remove_var(key);
        let p = overlay_path();
        assert_eq!(p, PathBuf::from(DEFAULT_OVERLAY_PATH));
        if let Some(v) = saved {
            std::env::set_var(key, v);
        }
    }

    #[test]
    fn overlay_path_honors_env_override() {
        let key = OVERLAY_PATH_ENV;
        let saved = std::env::var(key).ok();
        std::env::set_var(key, "/custom/overlay.jsonl");
        let p = overlay_path();
        assert_eq!(p, PathBuf::from("/custom/overlay.jsonl"));
        // Restore
        match saved {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
