//! Live entity-manifest loader (Quipu Canon v1.2 — Phase 5 of
//! `SPEC_TANASTOK_ENTITY_INTEGRATION_v1.md`).
//!
//! ## What this does
//!
//! The Datachain Rope node periodically pulls
//! `https://tanastok.io/api/v1/tanastok-entity-manifest` (and any other
//! ecosystem endpoint registered via [`ManifestSource`]) and merges the
//! 1,626+ entities Tanastok publishes into the in-memory label
//! registry.  After this module ships, every `kind=asset` /
//! `kind=contract` / `kind=did` Tanastok string returned by the live
//! RPC resolves to a human-readable label without a code change to
//! `entity_labels.rs`.
//!
//! ## Cache discipline
//!
//! - The fetcher records the `X-Tanastok-Manifest-Version` header and
//!   the `generated_at` field on every response. If both are unchanged
//!   from the previous successful fetch, no rebuild happens — only the
//!   "last seen" timestamp moves forward.
//! - When either changes, [`apply_response`] builds a brand-new
//!   `LabelRegistry` (built-ins + every manifest entry) and atomically
//!   swaps it into [`crate::entity_labels::install_current`].
//! - String memory is reused across refreshes through
//!   [`crate::entity_labels::intern`]: the same display name leaked
//!   once on a cold start is reused on every subsequent refresh.
//!
//! ## Spawn site
//!
//! The refresh task is spawned by `RopeNode::run` immediately after the
//! personal-ledger subsystem comes up.  It honours
//! `ROPE_DISABLE_ENTITY_MANIFEST=1` for offline / unit-test runs.

use std::env;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::entity_labels::{
    fresh_with_builtins, install_current, intern, intern_opt, EntityLabel, LabelKind,
};

/// Default endpoint as of 2026-05-21 (per Tanastok handover).
pub const DEFAULT_TANASTOK_MANIFEST_URL: &str =
    "https://tanastok.io/api/v1/tanastok-entity-manifest";

/// Default refresh cadence — matches the endpoint's `s-maxage=300`.
pub const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// HTTP request timeout. Tanastok's full payload is ~750 KB and serves
/// in ~1 s warm; keep a generous ceiling for cold caches and TLS RTT.
pub const DEFAULT_FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// One ecosystem manifest source. Phase 5 ships with one source
/// (Tanastok). Future ecosystems (DCSwap, NaturaProof, Datawallet+,
/// Careaway) plug in by adding more entries to the spawn loop.
#[derive(Clone, Debug)]
pub struct ManifestSource {
    /// Human-friendly identifier — `"tanastok"`, `"dcswap"`, ...
    pub name: &'static str,
    /// Public manifest URL.
    pub url: String,
    /// Refresh cadence. Tanastok recommends 5 min.
    pub interval: Duration,
}

impl ManifestSource {
    pub fn tanastok_default() -> Self {
        Self {
            name: "tanastok",
            url: env::var("TANASTOK_MANIFEST_URL")
                .unwrap_or_else(|_| DEFAULT_TANASTOK_MANIFEST_URL.to_string()),
            interval: DEFAULT_REFRESH_INTERVAL,
        }
    }
}

/// Cache discipline state — one entry per source.
#[derive(Default, Debug, Clone)]
struct LastSeen {
    version: Option<String>,
    generated_at: Option<u64>,
}

/// In-memory cache of the most recent successful manifest fetches per
/// source. Used to gate registry rebuilds.
#[derive(Default)]
struct ManifestCache {
    last_seen: std::collections::HashMap<&'static str, LastSeen>,
    /// Owned copy of the most recent payload per source so a refresh
    /// from a different source can re-include it when rebuilding the
    /// merged registry.
    last_response: std::collections::HashMap<&'static str, Arc<ManifestResponse>>,
}

fn cache() -> &'static Mutex<ManifestCache> {
    use std::sync::OnceLock;
    static C: OnceLock<Mutex<ManifestCache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(ManifestCache::default()))
}

// ============================================================================
// Wire format — mirrors
// `tanastok-app/src/app/api/v1/tanastok-entity-manifest/builders.ts`.
// ============================================================================

/// Public manifest response shape.
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestResponse {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub generated_at: u64,
    #[serde(default)]
    pub counts: serde_json::Value,
    #[serde(default)]
    pub entities: Vec<ManifestEntity>,
}

/// One entity in the manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestEntity {
    pub kind: String,
    /// 32-byte canonical Quipu string id (preferred lookup key).
    #[serde(default)]
    pub string_id: Option<String>,
    /// Lower-level identity bytes when not a synthetic string id
    /// (typically equal to `string_id` for v1.0.0 of the manifest).
    #[serde(default)]
    pub id_bytes: Option<String>,
    pub label: ManifestLabel,
    #[serde(default)]
    pub parent_string_id: Option<String>,
    #[serde(default)]
    pub ecosystem_id: Option<String>,
}

/// Label sub-block — free-form to accommodate role taxonomy growth.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ManifestLabel {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub short_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub verified: Option<bool>,
    #[serde(default)]
    pub verifier: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub asset_type: Option<String>,
    /// Anything else the manifest emits — kept verbatim for forward
    /// compatibility with future taxonomy additions (e.g. yields,
    /// listing status).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ============================================================================
// Conversion: ManifestEntity -> EntityLabel
// ============================================================================

/// Normalise any incoming hex id to the registry's preferred form:
/// lowercase, no `0x` prefix.
fn normalise_id(id_hex: &str) -> String {
    id_hex.trim().trim_start_matches("0x").to_ascii_lowercase()
}

/// Map the manifest's `kind` string to the canonical [`LabelKind`].
/// Falls back to `LabelKind::Contract` for unknown kinds with a 20-byte
/// id and `LabelKind::Did` otherwise — never panics.
fn kind_for(entity: &ManifestEntity) -> LabelKind {
    if let Some(k) = LabelKind::parse(&entity.kind) {
        return k;
    }
    // Fallback heuristic for forward-compat.
    match entity
        .string_id
        .as_deref()
        .or(entity.id_bytes.as_deref())
        .map(normalise_id)
    {
        Some(id) if id.len() == 64 && id.starts_with("000000000000000000000000") => {
            LabelKind::Contract
        }
        _ => LabelKind::Did,
    }
}

/// Tanastok manifest → "tanastok" platform; future sources override.
fn platform_for(_entity: &ManifestEntity, source: &ManifestSource) -> &'static str {
    intern(source.name)
}

/// Build an [`EntityLabel`] from a [`ManifestEntity`]. Returns `None`
/// when the entity has no string id (impossible per the spec but
/// defensive).
fn to_entity_label(
    entity: &ManifestEntity,
    source: &ManifestSource,
) -> Option<EntityLabel> {
    let raw_id = entity
        .string_id
        .as_deref()
        .or(entity.id_bytes.as_deref())?;
    let id_lower = normalise_id(raw_id);
    let id_hex = intern(&id_lower);

    let display_name = entity
        .label
        .display_name
        .as_deref()
        .unwrap_or(&id_lower)
        .to_string();
    let short_name = entity
        .label
        .short_name
        .clone()
        .unwrap_or_else(|| short_from(&display_name));
    let description = entity
        .label
        .description
        .clone()
        .unwrap_or_default();

    // The asset_type extra carries useful colour for the frontend palette,
    // but isn't a separate `EntityLabel` field — fold it into the role
    // when the role itself is generic.
    let role_owned = match (
        entity.label.role.as_deref(),
        entity.label.asset_type.as_deref(),
    ) {
        (Some(r), Some(at)) if r == "physical_asset" => format!("{}:{}", r, at.to_lowercase()),
        (Some(r), _) => r.to_string(),
        (None, Some(at)) => format!("asset:{}", at.to_lowercase()),
        (None, None) => "unknown".to_string(),
    };

    let icon_owned = entity
        .label
        .icon
        .clone()
        .unwrap_or_else(|| icon_for(&entity.kind, &role_owned));

    Some(EntityLabel {
        id_hex,
        kind: kind_for(entity),
        display_name: intern(&display_name),
        short_name: intern(&short_name),
        description: intern(&description),
        platform: platform_for(entity, source),
        role: intern(&role_owned),
        parent: intern_opt(
            entity
                .parent_string_id
                .as_deref()
                .map(normalise_id)
                .as_deref(),
        ),
        ecosystem: intern_opt(
            entity
                .ecosystem_id
                .as_deref()
                .map(normalise_id)
                .as_deref(),
        ),
        verified: entity.label.verified.unwrap_or(false),
        verifier: intern_opt(entity.label.verifier.as_deref()),
        icon: intern(&icon_owned),
        hidden: false,
    })
}

fn short_from(display: &str) -> String {
    display
        .split_whitespace()
        .next()
        .unwrap_or(display)
        .chars()
        .take(24)
        .collect()
}

/// Default FontAwesome icon per kind / role.
fn icon_for(kind: &str, role: &str) -> String {
    let lower = role.to_ascii_lowercase();
    if lower.contains("dcnft") || lower.contains("title_deed") {
        return "fa-certificate".to_string();
    }
    if lower.contains("security_token") || lower.contains("erc3643") {
        return "fa-coins".to_string();
    }
    if lower.contains("issuance") {
        return "fa-rocket".to_string();
    }
    if lower.contains("marketplace") {
        return "fa-shop".to_string();
    }
    if lower.contains("compliance") {
        return "fa-shield-halved".to_string();
    }
    if lower.contains("partner") {
        return "fa-handshake".to_string();
    }
    match kind {
        "ecosystem" => "fa-globe",
        "application" => "fa-cubes",
        "asset" => "fa-gem",
        "contract" => "fa-file-contract",
        "did" => "fa-id-card",
        _ => "fa-circle",
    }
    .to_string()
}

// ============================================================================
// Apply-to-registry path
// ============================================================================

/// Returns `true` when the registry was rebuilt; `false` when the
/// payload was unchanged and the cache hit skipped work.
pub fn apply_response(source: &ManifestSource, resp: ManifestResponse) -> bool {
    let prev = cache()
        .lock()
        .last_seen
        .get(source.name)
        .cloned()
        .unwrap_or_default();

    let same_version = prev.version.as_deref() == Some(resp.version.as_str());
    let same_generated = prev.generated_at == Some(resp.generated_at);
    if same_version && same_generated {
        debug!(
            source = source.name,
            version = %resp.version,
            entities = resp.entities.len(),
            "manifest unchanged — skipping rebuild",
        );
        return false;
    }

    // Update the per-source cache slot up-front so a panic mid-rebuild
    // doesn't loop us into the same response.
    let resp_arc = Arc::new(resp);
    {
        let mut c = cache().lock();
        c.last_seen.insert(
            source.name,
            LastSeen {
                version: Some(resp_arc.version.clone()),
                generated_at: Some(resp_arc.generated_at),
            },
        );
        c.last_response
            .insert(source.name, Arc::clone(&resp_arc));
    }

    // Build a fresh registry: built-ins, then every cached source.
    let mut reg = fresh_with_builtins();
    let snapshots: Vec<(&'static str, Arc<ManifestResponse>)> = cache()
        .lock()
        .last_response
        .iter()
        .map(|(k, v)| (*k, Arc::clone(v)))
        .collect();

    let mut accepted = 0usize;
    let mut skipped = 0usize;
    for (sname, snap) in &snapshots {
        let s = if *sname == source.name {
            source.clone()
        } else {
            // Reconstruct a minimal source for the conversion path.
            ManifestSource {
                name: sname,
                url: String::new(),
                interval: DEFAULT_REFRESH_INTERVAL,
            }
        };
        for ent in &snap.entities {
            match to_entity_label(ent, &s) {
                Some(label) => {
                    reg.insert(label);
                    accepted += 1;
                }
                None => skipped += 1,
            }
        }
    }

    install_current(reg);

    info!(
        source = source.name,
        version = %resp_arc.version,
        accepted,
        skipped,
        "entity manifest applied",
    );
    true
}

// ============================================================================
// HTTP fetch
// ============================================================================

/// Pull the manifest and parse it. Honours `DEFAULT_FETCH_TIMEOUT`.
pub async fn fetch_manifest(url: &str) -> anyhow::Result<ManifestResponse> {
    let client = reqwest::Client::builder()
        .timeout(DEFAULT_FETCH_TIMEOUT)
        .user_agent("rope-node/entity-manifest")
        .build()?;
    let resp = client
        .get(url)
        .header("accept", "application/json")
        .send()
        .await?
        .error_for_status()?;
    let body = resp.bytes().await?;
    let parsed: ManifestResponse = serde_json::from_slice(&body)?;
    Ok(parsed)
}

// ============================================================================
// Refresh task
// ============================================================================

/// Spawn an infinite background task that polls every
/// [`ManifestSource`] at its configured interval.
///
/// Honours `ROPE_DISABLE_ENTITY_MANIFEST=1` for unit tests / offline
/// development. Failures (network, parse) are logged at WARN and the
/// next interval continues — the registry never reverts to an older
/// view on a transient failure.
pub fn spawn_refresh_task(sources: Vec<ManifestSource>) {
    if env::var("ROPE_DISABLE_ENTITY_MANIFEST")
        .ok()
        .as_deref()
        == Some("1")
    {
        info!("entity-manifest refresh disabled by ROPE_DISABLE_ENTITY_MANIFEST=1");
        return;
    }
    if sources.is_empty() {
        return;
    }

    for src in sources {
        tokio::spawn(async move {
            // First fetch on a short timer so the registry warms up
            // before the first user RPC.
            let mut tick = tokio::time::interval(src.interval);
            tick.tick().await; // immediate fire
            loop {
                match fetch_manifest(&src.url).await {
                    Ok(resp) => {
                        // Rebuild touches parking_lot locks and walks 1,600+
                        // entities — keep it off the Tokio RPC worker pool so
                        // loopback health probes (eth_blockNumber) cannot be
                        // starved during the 5-min refresh tick.
                        let src_for_apply = src.clone();
                        match tokio::task::spawn_blocking(move || {
                            apply_response(&src_for_apply, resp)
                        })
                        .await
                        {
                            Ok(applied) => {
                                if applied {
                                    debug!(source = src.name, "manifest registry rebuilt");
                                }
                            }
                            Err(join_err) => {
                                warn!(
                                    source = src.name,
                                    error = %join_err,
                                    "entity-manifest apply task panicked",
                                );
                            }
                        }
                    }
                    Err(err) => {
                        warn!(
                            source = src.name,
                            url = %src.url,
                            error = %err,
                            "entity-manifest fetch failed; will retry on next tick",
                        );
                    }
                }
                tick.tick().await;
            }
        });
    }
}

// ============================================================================
// Tests — offline only, never hit network.
// ============================================================================

/// **Test-only** helper: clear the cached `last_seen` / `last_response`
/// state. Used by the unit tests to reset the singletons between cases
/// when they share the same process-wide global registry.
#[cfg(test)]
pub(crate) fn _test_reset_cache() {
    let mut c = cache().lock();
    c.last_seen.clear();
    c.last_response.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_labels;
    use std::sync::{Mutex, OnceLock};

    /// Serialise the tests so they don't fight over the global
    /// `current_slot` / `cache()` singletons.
    fn test_lock() -> &'static Mutex<()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        L.get_or_init(|| Mutex::new(()))
    }

    /// Acquire the test lock, recovering from poisoning so that a
    /// failing test doesn't cascade-poison every subsequent test.
    fn lock_for_test() -> std::sync::MutexGuard<'static, ()> {
        match test_lock().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    fn fixture_response() -> ManifestResponse {
        ManifestResponse {
            version: "1.0.0".to_string(),
            generated_at: 1779365453,
            counts: serde_json::Value::Null,
            entities: vec![
                ManifestEntity {
                    kind: "ecosystem".to_string(),
                    string_id: Some(
                        "0x5f5a4b62b1f904df0a6a9c30f813fb8c3ebfa616f0416a59a89d0053f218d5b0"
                            .to_string(),
                    ),
                    id_bytes: None,
                    parent_string_id: None,
                    ecosystem_id: None,
                    label: ManifestLabel {
                        display_name: Some("Tanastok".to_string()),
                        role: Some("ecosystem".to_string()),
                        verified: Some(true),
                        ..ManifestLabel::default()
                    },
                },
                ManifestEntity {
                    kind: "asset".to_string(),
                    string_id: Some(
                        "0x613c2b3a2a66e5340b756585b7e0e78e2156162a03ed2d3bfab4b6d8d318d44f"
                            .to_string(),
                    ),
                    id_bytes: None,
                    parent_string_id: Some(
                        "0xa1b27b82a2561f4bfe66090f4004399a17d44c54802b4adae999e6b6e9693070"
                            .to_string(),
                    ),
                    ecosystem_id: Some(
                        "0x5f5a4b62b1f904df0a6a9c30f813fb8c3ebfa616f0416a59a89d0053f218d5b0"
                            .to_string(),
                    ),
                    label: ManifestLabel {
                        display_name: Some("Asset 173 - Luxury Watch".to_string()),
                        role: Some("physical_asset".to_string()),
                        verified: Some(false),
                        asset_type: Some("PRIVATE_JETS".to_string()),
                        ..ManifestLabel::default()
                    },
                },
                // T2 — DCNFT contract whose parent_string_id matches
                // the asset above. Mirrors the live manifest shape:
                // each asset has exactly one DCNFT (ERC-721 title deed)
                // and exactly one ERC-3643 security token; both
                // reference the asset's `string_id` as their parent.
                ManifestEntity {
                    kind: "contract".to_string(),
                    string_id: Some(
                        "0x000000000000000000000000a7497d9bb741d1a734da7c4603a79b217c6e7920"
                            .to_string(),
                    ),
                    id_bytes: None,
                    parent_string_id: Some(
                        "0x613c2b3a2a66e5340b756585b7e0e78e2156162a03ed2d3bfab4b6d8d318d44f"
                            .to_string(),
                    ),
                    ecosystem_id: Some(
                        "0x5f5a4b62b1f904df0a6a9c30f813fb8c3ebfa616f0416a59a89d0053f218d5b0"
                            .to_string(),
                    ),
                    label: ManifestLabel {
                        display_name: Some("Asset 173 - Luxury Watch (DCNFT)".to_string()),
                        role: Some("asset_title_deed_nft".to_string()),
                        verified: Some(false),
                        ..ManifestLabel::default()
                    },
                },
                // T2 — paired ERC-3643 security token, also pointing at
                // the asset's string_id.
                ManifestEntity {
                    kind: "contract".to_string(),
                    string_id: Some(
                        "0x000000000000000000000000e0629d3c3afd4e74b59a1372370154d17183bb5c"
                            .to_string(),
                    ),
                    id_bytes: None,
                    parent_string_id: Some(
                        "0x613c2b3a2a66e5340b756585b7e0e78e2156162a03ed2d3bfab4b6d8d318d44f"
                            .to_string(),
                    ),
                    ecosystem_id: Some(
                        "0x5f5a4b62b1f904df0a6a9c30f813fb8c3ebfa616f0416a59a89d0053f218d5b0"
                            .to_string(),
                    ),
                    label: ManifestLabel {
                        display_name: Some("Asset 173 - Luxury Watch (A1LW)".to_string()),
                        role: Some("asset_security_token".to_string()),
                        verified: Some(false),
                        ..ManifestLabel::default()
                    },
                },
                ManifestEntity {
                    kind: "did".to_string(),
                    string_id: Some(
                        "0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                            .to_string(),
                    ),
                    id_bytes: None,
                    parent_string_id: Some(
                        "0x5f5a4b62b1f904df0a6a9c30f813fb8c3ebfa616f0416a59a89d0053f218d5b0"
                            .to_string(),
                    ),
                    ecosystem_id: Some(
                        "0x5f5a4b62b1f904df0a6a9c30f813fb8c3ebfa616f0416a59a89d0053f218d5b0"
                            .to_string(),
                    ),
                    label: ManifestLabel {
                        display_name: Some("Acme Holdings GmbH".to_string()),
                        role: Some("issuing_org".to_string()),
                        verified: Some(true),
                        ..ManifestLabel::default()
                    },
                },
            ],
        }
    }

    fn fixture_source() -> ManifestSource {
        ManifestSource {
            name: "tanastok",
            url: "https://tanastok.io/api/v1/tanastok-entity-manifest".to_string(),
            interval: DEFAULT_REFRESH_INTERVAL,
        }
    }

    #[test]
    fn t1_apply_response_resolves_kind_asset_and_contract_strings() {
        let _g = lock_for_test();
        _test_reset_cache();
        let _ = apply_response(&fixture_source(), fixture_response());
        let reg = entity_labels::current();
        // T1: kind=asset string id resolves to the Tanastok display_name.
        let asset = reg
            .get("613c2b3a2a66e5340b756585b7e0e78e2156162a03ed2d3bfab4b6d8d318d44f")
            .expect("asset must be in current registry");
        assert!(asset.display_name.contains("Asset 173"));
        assert_eq!(asset.kind, LabelKind::Asset);
        assert_eq!(asset.platform, "tanastok");

        // T1 for contracts: kind=contract DCNFT resolves and the role is
        // intact verbatim from the manifest.
        let contract = reg
            .get("000000000000000000000000a7497d9bb741d1a734da7c4603a79b217c6e7920")
            .expect("DCNFT contract must be in current registry");
        assert_eq!(contract.kind, LabelKind::Contract);
        assert_eq!(contract.role, "asset_title_deed_nft");
    }

    #[test]
    fn t2_asset_links_to_application_and_ecosystem() {
        let _g = lock_for_test();
        _test_reset_cache();
        let _ = apply_response(&fixture_source(), fixture_response());
        let reg = entity_labels::current();
        let asset_id =
            "613c2b3a2a66e5340b756585b7e0e78e2156162a03ed2d3bfab4b6d8d318d44f";
        let asset = reg.get(asset_id).expect("asset present");
        assert_eq!(
            asset.parent.unwrap(),
            "a1b27b82a2561f4bfe66090f4004399a17d44c54802b4adae999e6b6e9693070"
        );
        assert_eq!(
            asset.ecosystem.unwrap(),
            "5f5a4b62b1f904df0a6a9c30f813fb8c3ebfa616f0416a59a89d0053f218d5b0"
        );

        // T2 (the bigger half): the DCNFT and ERC-3643 contracts link
        // back to the asset via `parent` → asset.string_id.
        let kids = reg.children_of(asset_id);
        let roles: std::collections::HashSet<&str> =
            kids.iter().map(|k| k.role).collect();
        assert!(
            roles.contains("asset_title_deed_nft"),
            "DCNFT contract must point at asset",
        );
        assert!(
            roles.contains("asset_security_token"),
            "ERC-3643 security-token contract must point at asset",
        );
        let kinds: std::collections::HashSet<_> =
            kids.iter().map(|k| k.kind).collect();
        assert!(kinds.contains(&LabelKind::Contract));
    }

    #[test]
    fn t3_did_kind_renders_org_label_with_verifier_when_present() {
        let _g = lock_for_test();
        _test_reset_cache();
        let mut resp = fixture_response();
        // Add a verifier to the DID entry to exercise the optional field.
        if let Some(d) = resp.entities.iter_mut().find(|e| e.kind == "did") {
            d.label.verifier = Some(
                "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195".to_string(),
            );
        }
        let _ = apply_response(&fixture_source(), resp);
        let reg = entity_labels::current();
        let did = reg
            .get("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .expect("did present");
        assert_eq!(did.kind, LabelKind::Did);
        assert_eq!(did.display_name, "Acme Holdings GmbH");
        assert!(did.verified);
        assert!(did.verifier.is_some());
    }

    #[test]
    fn t5_relations_link_children_to_parents_via_label_topology() {
        let _g = lock_for_test();
        _test_reset_cache();
        let _ = apply_response(&fixture_source(), fixture_response());
        let reg = entity_labels::current();
        // Asset says parent=app; the synthetic "anchors" relation in the
        // RPC layer is built from these parent links. Verify we have the
        // expected upward chain.
        let app_id = "a1b27b82a2561f4bfe66090f4004399a17d44c54802b4adae999e6b6e9693070";
        let descendants = reg.children_of(app_id);
        assert!(
            descendants.iter().any(|l| l.kind == LabelKind::Asset),
            "asset should be listed as a child of its application id",
        );
    }

    #[test]
    fn t6_kind_filter_by_platform_yields_only_tanastok() {
        let _g = lock_for_test();
        _test_reset_cache();
        let _ = apply_response(&fixture_source(), fixture_response());
        let reg = entity_labels::current();
        let tan = reg.list_by_platform("tanastok");
        let kinds: std::collections::HashSet<_> = tan.iter().map(|l| l.kind).collect();
        // We expect at least one of each Tanastok-relevant kind seeded
        // by the fixture.
        assert!(kinds.contains(&LabelKind::Ecosystem));
        assert!(kinds.contains(&LabelKind::Asset));
        assert!(kinds.contains(&LabelKind::Contract));
        assert!(kinds.contains(&LabelKind::Did));
    }

    #[test]
    fn cache_skips_rebuild_when_version_and_generated_at_unchanged() {
        let _g = lock_for_test();
        _test_reset_cache();
        let src = fixture_source();
        let resp = fixture_response();
        // First apply must rebuild.
        assert!(apply_response(&src, resp.clone()));
        // Second apply with identical (version, generated_at) must skip.
        assert!(!apply_response(&src, resp));
    }

    #[test]
    fn intern_dedup_round_trip() {
        let a = entity_labels::intern("kibali-gold-mine");
        let b = entity_labels::intern("kibali-gold-mine");
        // Same byte content AND same address — proves dedup.
        assert_eq!(a, b);
        assert_eq!(a.as_ptr(), b.as_ptr());
    }

    #[test]
    fn icon_default_for_known_roles() {
        assert_eq!(icon_for("contract", "asset_title_deed_nft"), "fa-certificate");
        assert_eq!(icon_for("contract", "asset_security_token"), "fa-coins");
        assert_eq!(icon_for("application", "issuance"), "fa-rocket");
    }

    #[test]
    fn t6_verified_flag_and_verifier_render_through_to_label() {
        let _g = lock_for_test();
        _test_reset_cache();
        let mut resp = fixture_response();
        // Mark the asset verified with a verifier address (the spec's
        // T6 rendering: a "verified" badge backed by a verifier wallet).
        if let Some(a) = resp.entities.iter_mut().find(|e| e.kind == "asset") {
            a.label.verified = Some(true);
            a.label.verifier = Some(
                "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195".to_string(),
            );
        }
        let _ = apply_response(&fixture_source(), resp);
        let reg = entity_labels::current();
        let asset = reg
            .get("613c2b3a2a66e5340b756585b7e0e78e2156162a03ed2d3bfab4b6d8d318d44f")
            .expect("asset present");
        assert!(
            asset.verified,
            "T6: asset.verified must propagate from manifest -> label",
        );
        assert!(
            asset.verifier.is_some(),
            "T6: verifier address must be exposed for badge rendering",
        );
    }

    /// Live-network acceptance: pulls the real Tanastok manifest from
    /// https://tanastok.io/api/v1/tanastok-entity-manifest and asserts
    /// that the live shape matches every assumption we encoded above
    /// (including ≥1,000-entity SLO, T1 asset/contract resolution, and
    /// T2 asset->contract parent linkage). Disabled by default — set
    /// `ROPE_LIVE_TANASTOK_MANIFEST=1` to run it.
    #[test]
    fn live_smoke_tanastok_manifest_accepts_production_shape() {
        if env::var("ROPE_LIVE_TANASTOK_MANIFEST")
            .ok()
            .as_deref()
            != Some("1")
        {
            eprintln!(
                "live_smoke_tanastok_manifest skipped: set ROPE_LIVE_TANASTOK_MANIFEST=1 to enable",
            );
            return;
        }
        let _g = lock_for_test();
        _test_reset_cache();
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let resp = rt
            .block_on(fetch_manifest(DEFAULT_TANASTOK_MANIFEST_URL))
            .expect("manifest fetch must succeed");
        assert!(!resp.version.is_empty(), "manifest must carry a version");
        assert!(
            resp.entities.len() >= 1000,
            "Tanastok SLO requires >=1000 entities, got {}",
            resp.entities.len(),
        );
        let _ = apply_response(&ManifestSource::tanastok_default(), resp);
        let reg = entity_labels::current();
        // T1: at least 100 kind=asset and 100 kind=contract live entries
        // in the merged registry.
        assert!(reg.list_by_kind(LabelKind::Asset).len() >= 100);
        assert!(reg.list_by_kind(LabelKind::Contract).len() >= 100);
        // T2: at least one asset with two contract children (DCNFT +
        // ERC-3643).
        let mut asset_with_pair = 0usize;
        for asset in reg.list_by_kind(LabelKind::Asset) {
            let kids = reg.children_of(asset.id_hex);
            let has_dcnft = kids.iter().any(|k| k.role.contains("title_deed"));
            let has_3643 = kids.iter().any(|k| k.role.contains("security_token"));
            if has_dcnft && has_3643 {
                asset_with_pair += 1;
            }
        }
        assert!(
            asset_with_pair >= 50,
            "expected many asset⇄(DCNFT+ERC-3643) pairs, found {}",
            asset_with_pair,
        );
    }

    #[test]
    fn malformed_entity_without_id_is_dropped_not_panicking() {
        let _g = lock_for_test();
        _test_reset_cache();
        let mut resp = fixture_response();
        resp.entities.push(ManifestEntity {
            kind: "asset".to_string(),
            string_id: None,
            id_bytes: None,
            parent_string_id: None,
            ecosystem_id: None,
            label: ManifestLabel::default(),
        });
        // Bump generated_at so the cache treats it as a new payload.
        resp.generated_at += 1;
        let _ = apply_response(&fixture_source(), resp);
        // Registry rebuild should have completed without panic and the
        // good entries should still resolve.
        assert!(entity_labels::current()
            .get("613c2b3a2a66e5340b756585b7e0e78e2156162a03ed2d3bfab4b6d8d318d44f")
            .is_some());
    }
}
