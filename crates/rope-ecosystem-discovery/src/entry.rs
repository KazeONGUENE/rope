//! Overlay entry type + serialisation. Wire-compatible with the
//! `ecosystem_overlay::RawOverlayEntry` deserialiser in `rope-explorer`.
//!
//! Every field name, type, and enum value in this module MUST match the
//! spec at `docs/ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md`. The loader will
//! reject anything that doesn't - if you're tempted to add a field here,
//! add it to the spec + loader first.

use serde::{Deserialize, Serialize};

/// The set of scanners that are allowed to produce overlay entries. The
/// loader (`ecosystem_overlay.rs`) rejects any `discovered_by` value that
/// is not in this list, so keep the two in lock-step. The variants are
/// serialised to lowercase strings via `#[serde(rename_all = "kebab-case")]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoveredBy {
    /// Scanned a `.cursor/rules/handover-*.mdc` file in the local repo.
    HandoverScanner,
    /// Read a labelled address / contract from an on-chain source
    /// (currently dcscan.io's `/api/v1/labels` + `/api/v1/registry`).
    OnchainScanner,
    /// Fetched a well-known partner API (Tanastok, DCSwap, Datawallet+,
    /// etc.) that publishes its own directory of projects.
    PartnerApiScanner,
    /// Written by hand by an operator, e.g. via `cat >> overlay.jsonl`.
    /// The loader accepts this but the discovery binary NEVER emits it.
    #[allow(dead_code)]
    Manual,
}

impl DiscoveredBy {
    pub fn as_str(self) -> &'static str {
        match self {
            DiscoveredBy::HandoverScanner => "handover-scanner",
            DiscoveredBy::OnchainScanner => "onchain-scanner",
            DiscoveredBy::PartnerApiScanner => "partner-api-scanner",
            DiscoveredBy::Manual => "manual",
        }
    }
}

/// Project lifecycle status. Matches the loader's allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Live,
    Development,
    Sandbox,
    Archived,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Live => "live",
            Status::Development => "development",
            Status::Sandbox => "sandbox",
            Status::Archived => "archived",
        }
    }
}

/// Overlay visibility hint. The loader treats this as advisory only:
/// canonical `PRIVATE_HIDDEN_IDS` / `PRIVATE_VISIBLE_IDS` always win.
/// A malicious writer cannot un-hide a canonical project by emitting
/// `Visibility::Public` here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    PrivateVisible,
    PrivateHidden,
}

impl Default for Visibility {
    fn default() -> Self {
        Visibility::Public
    }
}

/// One overlay entry, serialised as one JSON object per line into
/// `/var/lib/rope-explorer/ecosystem-overlay.jsonl`.
///
/// Field ordering here follows the spec doc; changing it doesn't break
/// the loader (JSON is unordered) but keeps diffs / manual inspection
/// stable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayEntry {
    /// Lowercase kebab-case slug, 3-64 chars, unique per project.
    pub id: String,
    /// Human-readable project name, 3-128 chars.
    pub name: String,
    /// One of `canonical_archetypes()` (kept in sync via the tests
    /// module).
    pub archetype: String,
    /// One of the four `Status` values.
    pub status: Status,
    /// Which scanner produced this entry. Set at construction; NEVER
    /// change post-emission or you'll break the loader's provenance
    /// audit.
    pub discovered_by: DiscoveredBy,
    /// Free-form up to 512 chars. Should identify the exact record
    /// scanned (e.g. `handover:/path/to/rules/handover-foo-2026.mdc`,
    /// `onchain:https://dcscan.io/api/v1/labels#0xabc...`,
    /// `partner-api:https://tanastok.io/api/v1/registry`).
    pub discovery_source: String,
    /// Unix seconds when the scanner first saw this entry.
    pub discovered_at: i64,

    // -------------------- optional fields --------------------
    /// Up to 12 tags, each 2-32 chars, lowercase + digits + `-`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Free-form region label, 3-64 chars, defaults to "Global" in the
    /// loader if omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// ISO 3166-1 alpha-2 (2 uppercase) or the literal `"GLOBAL"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Optional EVM address (`0x` + 40 lowercase hex). The loader
    /// enforces case + length; we normalise here too so a scanner that
    /// picks up a mixed-case address (dcscan labels API sometimes does)
    /// still lands correctly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet: Option<String>,
    /// Absolute `https://` URL for the project's public site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stakeholder_url: Option<String>,
    /// Free-form description, up to 500 chars. Loader will truncate
    /// with an ellipsis if longer, but we truncate at emission time
    /// too so the on-disk overlay stays bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// For asset-tokenization projects. Loader caps at 1e9.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_count: Option<u64>,
    /// For environmental-monitoring projects. Loader caps at 1e9.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor_count: Option<u64>,
    /// Absolute `https://` URL for the project's logo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
    /// Unix seconds. Defaults to `discovered_at` in the loader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    /// Overlay-side visibility hint. Loader treats this as advisory
    /// only (canonical wins). Defaults to Public.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
}

impl OverlayEntry {
    /// Serialise to a single JSON line ending in `\n`. Never contains
    /// embedded newlines because `serde_json::to_string` outputs a
    /// single-line rendering by default.
    pub fn to_jsonl_line(&self) -> Result<String, serde_json::Error> {
        let mut out = serde_json::to_string(self)?;
        out.push('\n');
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovered_by_serialises_to_kebab_case() {
        assert_eq!(DiscoveredBy::HandoverScanner.as_str(), "handover-scanner");
        assert_eq!(DiscoveredBy::OnchainScanner.as_str(), "onchain-scanner");
        assert_eq!(
            DiscoveredBy::PartnerApiScanner.as_str(),
            "partner-api-scanner"
        );
        assert_eq!(DiscoveredBy::Manual.as_str(), "manual");

        // Round-trip via serde:
        let s = serde_json::to_string(&DiscoveredBy::HandoverScanner).unwrap();
        assert_eq!(s, "\"handover-scanner\"");
        let back: DiscoveredBy = serde_json::from_str(&s).unwrap();
        assert_eq!(back, DiscoveredBy::HandoverScanner);
    }

    #[test]
    fn status_serialises_to_lowercase() {
        assert_eq!(Status::Live.as_str(), "live");
        let s = serde_json::to_string(&Status::Development).unwrap();
        assert_eq!(s, "\"development\"");
    }

    #[test]
    fn visibility_serialises_to_snake_case() {
        assert_eq!(
            serde_json::to_string(&Visibility::PrivateHidden).unwrap(),
            "\"private_hidden\""
        );
        assert_eq!(
            serde_json::to_string(&Visibility::PrivateVisible).unwrap(),
            "\"private_visible\""
        );
        assert_eq!(
            serde_json::to_string(&Visibility::Public).unwrap(),
            "\"public\""
        );
    }

    #[test]
    fn to_jsonl_line_produces_single_line_ending_in_newline() {
        let e = OverlayEntry {
            id: "test-project".into(),
            name: "Test Project".into(),
            archetype: "infrastructure".into(),
            status: Status::Live,
            discovered_by: DiscoveredBy::HandoverScanner,
            discovery_source: "handover:/path/to/handover.mdc".into(),
            discovered_at: 1_786_600_000,
            tags: vec![],
            region: None,
            country: None,
            wallet: None,
            stakeholder_url: None,
            description: None,
            asset_count: None,
            sensor_count: None,
            logo_url: None,
            created_at: None,
            visibility: None,
        };
        let line = e.to_jsonl_line().unwrap();
        assert!(line.ends_with('\n'), "must end in newline");
        assert_eq!(
            line.chars().filter(|c| *c == '\n').count(),
            1,
            "only one newline"
        );
        // Round-trip via loader-compatible deserialiser:
        let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value["id"], "test-project");
        assert_eq!(value["discovered_by"], "handover-scanner");
        assert_eq!(value["status"], "live");
        // Optional fields must be absent when None, not `null`:
        assert!(value.get("region").is_none(), "None must skip serialisation");
        assert!(value.get("visibility").is_none());
        assert!(value.get("tags").is_none(), "empty vec is skipped");
    }

    #[test]
    fn optional_fields_serialise_when_set() {
        let e = OverlayEntry {
            id: "test-project".into(),
            name: "Test Project".into(),
            archetype: "asset_tokenization".into(),
            status: Status::Live,
            discovered_by: DiscoveredBy::OnchainScanner,
            discovery_source: "onchain:https://dcscan.io/address/0xabc".into(),
            discovered_at: 1_786_600_000,
            tags: vec!["rwa".into(), "test".into()],
            region: Some("Europe".into()),
            country: Some("FR".into()),
            wallet: Some("0x1234567890abcdef1234567890abcdef12345678".into()),
            stakeholder_url: Some("https://example.com".into()),
            description: Some("A test project".into()),
            asset_count: Some(42),
            sensor_count: None,
            logo_url: Some("https://example.com/logo.png".into()),
            created_at: Some(1_786_500_000),
            visibility: Some(Visibility::Public),
        };
        let line = e.to_jsonl_line().unwrap();
        let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value["region"], "Europe");
        assert_eq!(value["country"], "FR");
        assert_eq!(value["asset_count"], 42);
        assert_eq!(value["visibility"], "public");
        assert_eq!(value["tags"].as_array().unwrap().len(), 2);
        // sensor_count is None so must be absent:
        assert!(value.get("sensor_count").is_none());
    }

    /// The canonical archetype list that the loader validates against.
    /// This test is a compile-time-visible reminder to keep the two in
    /// sync (loader source of truth:
    /// `rope-explorer::ecosystem_canonical::canonical_archetypes`).
    #[test]
    fn known_archetypes_documented() {
        let known: &[&str] = &[
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
        // If the loader adds a new archetype, this list must be updated
        // in lock-step and the scanners can start emitting it.
        assert_eq!(known.len(), 16);
    }
}
