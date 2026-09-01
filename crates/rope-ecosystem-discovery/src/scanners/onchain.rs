//! On-chain scanner - discovers ecosystem projects from `dcscan.io`'s
//! curated label registry.
//!
//! The `GET /api/v1/labels` endpoint returns a JSON object
//! `{ count, labels: { "<addr>": { label, icon, category, hidden }, .. } }`.
//! This scanner treats every labelled address as a candidate `OverlayEntry`,
//! but filters aggressively:
//!
//! - `hidden: true` entries are skipped (operator marked them non-public).
//! - Category allowlist: only categories that plausibly map to a standalone
//!   ecosystem project are emitted. Token contracts, T-REX system infra,
//!   AMM pools, and network primitives are intentionally dropped because
//!   they are components of larger projects that are already tracked in
//!   the canonical registry.
//!
//! The precedence rule (`EDC > canonical > overlay`) means that even if
//! this scanner emits a duplicate of a canonical project (e.g. Tanastok),
//! the canonical entry wins at load time - so the on-chain scanner is
//! safe to be generous about emission.
//!
//! # Wire shape
//!
//! Each emitted record satisfies `ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` §3:
//!
//! - `id`: slug of the label name (`slugify(label)`)
//! - `name`: the label string, trimmed
//! - `archetype`: derived from `category` via `category_to_archetype`
//! - `status`: always `Live` (the contract exists on-chain)
//! - `discovered_by`: `OnchainScanner`
//! - `discovery_source`: `<dcscan_base>/api/v1/labels#<address>`
//! - `discovered_at`: current unix time
//! - `wallet`: lowercase `0x`-prefixed address
//! - `tags`: `[category]` for filtering / debugging

use crate::config::OnchainScannerConfig;
use crate::entry::{DiscoveredBy, OverlayEntry, Status};
use crate::error::{DiscoveryError, DiscoveryResult};
use crate::scanners::{
    normalise_evm_address, now_unix_secs, slugify, truncate_utf8, ScanResult, Scanner,
    MAX_DISCOVERY_SOURCE_LEN, MAX_NAME_LEN, MIN_NAME_LEN,
};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Default dcscan API base if the operator does not set one explicitly.
/// This is the canonical public endpoint.
const DEFAULT_DCSCAN_BASE: &str = "https://dcscan.io";

/// Raw label record returned by `GET /api/v1/labels`.
#[derive(Debug, Clone, Deserialize)]
struct RawLabel {
    label: String,
    #[serde(default)]
    #[allow(dead_code)]
    icon: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    hidden: bool,
}

/// Top-level response shape from `/api/v1/labels`.
#[derive(Debug, Deserialize)]
struct LabelsResponse {
    #[serde(default)]
    #[allow(dead_code)]
    count: u64,
    #[serde(default)]
    labels: HashMap<String, RawLabel>,
}

pub struct OnchainScanner {
    config: OnchainScannerConfig,
    http: reqwest::Client,
    dcscan_base: String,
}

impl OnchainScanner {
    /// Build a new scanner. Fails if the caller-provided `dcscan_base`
    /// is not a valid URL (validated via `url::Url::parse`).
    pub fn new(
        config: OnchainScannerConfig,
        http_timeout: Duration,
    ) -> DiscoveryResult<Self> {
        let dcscan_base = config
            .dcscan_base
            .clone()
            .unwrap_or_else(|| DEFAULT_DCSCAN_BASE.to_string())
            .trim_end_matches('/')
            .to_string();
        // Sanity-check the URL early so we fail on start-up rather than
        // on the first request.
        url::Url::parse(&dcscan_base).map_err(|e| {
            DiscoveryError::Config(format!(
                "onchain.dcscan_base is not a valid URL: {}: {}",
                dcscan_base, e
            ))
        })?;
        let http = reqwest::Client::builder()
            .timeout(http_timeout)
            .user_agent("rope-ecosystem-discovery/0.1 (+https://dcscan.io)")
            .build()
            .map_err(|e| {
                DiscoveryError::OnchainScan(format!("build reqwest client: {}", e))
            })?;
        Ok(Self {
            config,
            http,
            dcscan_base,
        })
    }

    /// Map a dcscan.io `category` string to a canonical archetype from
    /// `KNOWN_ARCHETYPES`, or `None` if the category is a component of
    /// a larger project (tokens, T-REX infra, AMM pools, network
    /// primitives) that should NOT surface as its own ecosystem card.
    fn category_to_archetype(category: &str) -> Option<&'static str> {
        match category {
            "bridge" => Some("bridge"),
            "governance" => Some("governance"),
            "infrastructure" => Some("infrastructure"),
            // Mapstore is an investment / RWA project; its operator +
            // guardian addresses both get labelled `mapstore` on dcscan.
            "mapstore" => Some("investment"),
            // Treasuries are typically part of a governance / project
            // stack; classify as governance so they render distinctly
            // from tokens or infra.
            "treasury" => Some("governance"),
            // Explicit drops - these are components, not projects:
            "defi" => None,       // AMM pools + factory (part of DCSwap)
            "system" => None,     // Genesis, protocol primitives
            "token" => None,      // Fungible tokens (WFAT/USDT/USDC/EUROD)
            "trex" => None,       // ONCHAINID / ERC-3643 T-REX infra
            _ => None,            // Unknown -> drop (fail-closed)
        }
    }

    async fn fetch_labels(&self) -> DiscoveryResult<LabelsResponse> {
        let url = format!("{}/api/v1/labels", self.dcscan_base);
        debug!("onchain: fetching {}", url);
        let resp = self.http.get(&url).send().await.map_err(|e| {
            DiscoveryError::OnchainScan(format!("GET {}: {}", url, e))
        })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(DiscoveryError::OnchainScan(format!(
                "GET {} -> HTTP {}",
                url, status
            )));
        }
        let body: LabelsResponse = resp.json().await.map_err(|e| {
            DiscoveryError::OnchainScan(format!(
                "parse JSON from {}: {}",
                url, e
            ))
        })?;
        Ok(body)
    }
}

#[async_trait]
impl Scanner for OnchainScanner {
    fn name(&self) -> &'static str {
        DiscoveredBy::OnchainScanner.as_str()
    }

    fn enabled(&self) -> bool {
        self.config.enabled
    }

    async fn scan(&self) -> DiscoveryResult<ScanResult> {
        info!("onchain scanner: fetching labels from {}", self.dcscan_base);
        let body = self.fetch_labels().await?;

        let now = now_unix_secs();
        let mut entries: Vec<OverlayEntry> = Vec::new();
        let mut considered = 0usize;
        let mut rejected = 0usize;

        for (addr, raw) in body.labels.into_iter() {
            considered += 1;

            // Skip hidden labels - operator marked them non-public.
            if raw.hidden {
                rejected += 1;
                debug!(
                    "onchain: skip hidden label addr={} name={:?}",
                    addr, raw.label
                );
                continue;
            }

            // Category must map to a known archetype.
            let category = raw.category.as_deref().unwrap_or("");
            let archetype = match Self::category_to_archetype(category) {
                Some(a) => a,
                None => {
                    rejected += 1;
                    debug!(
                        "onchain: skip category={:?} for addr={} name={:?}",
                        category, addr, raw.label
                    );
                    continue;
                }
            };

            // Normalise the wallet address (also validates hex + length).
            let wallet = match normalise_evm_address(&addr) {
                Some(w) => w,
                None => {
                    rejected += 1;
                    warn!("onchain: reject invalid address key: {}", addr);
                    continue;
                }
            };

            // Slugify the label to get an id.
            let name = raw.label.trim();
            let id = match slugify(name) {
                Some(s) => s,
                None => {
                    rejected += 1;
                    warn!(
                        "onchain: reject unslugifiable label {:?} for addr={}",
                        name, wallet
                    );
                    continue;
                }
            };

            // Name length check (loader will reject otherwise).
            if name.len() < MIN_NAME_LEN {
                rejected += 1;
                debug!(
                    "onchain: skip too-short name {:?} for addr={}",
                    name, wallet
                );
                continue;
            }
            let name_capped = truncate_utf8(name, MAX_NAME_LEN);

            let discovery_source = truncate_utf8(
                &format!("{}/api/v1/labels#{}", self.dcscan_base, wallet),
                MAX_DISCOVERY_SOURCE_LEN,
            );

            entries.push(OverlayEntry {
                id,
                name: name_capped,
                archetype: archetype.to_string(),
                status: Status::Live,
                discovered_by: DiscoveredBy::OnchainScanner,
                discovery_source,
                discovered_at: now,
                tags: vec![category.to_string()],
                region: None,
                country: None,
                wallet: Some(wallet),
                stakeholder_url: None,
                description: None,
                asset_count: None,
                sensor_count: None,
                logo_url: None,
                created_at: None,
                visibility: None,
            });
        }

        info!(
            "onchain scanner: considered={} emitted={} rejected={}",
            considered,
            entries.len(),
            rejected
        );

        Ok(ScanResult {
            entries,
            raw_considered: considered,
            raw_rejected: rejected,
            scanner: self.name(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_to_archetype_maps_expected() {
        assert_eq!(
            OnchainScanner::category_to_archetype("bridge"),
            Some("bridge")
        );
        assert_eq!(
            OnchainScanner::category_to_archetype("governance"),
            Some("governance")
        );
        assert_eq!(
            OnchainScanner::category_to_archetype("mapstore"),
            Some("investment")
        );
        assert_eq!(
            OnchainScanner::category_to_archetype("treasury"),
            Some("governance")
        );
        assert_eq!(
            OnchainScanner::category_to_archetype("infrastructure"),
            Some("infrastructure")
        );
    }

    #[test]
    fn category_to_archetype_drops_components() {
        assert_eq!(OnchainScanner::category_to_archetype("defi"), None);
        assert_eq!(OnchainScanner::category_to_archetype("token"), None);
        assert_eq!(OnchainScanner::category_to_archetype("trex"), None);
        assert_eq!(OnchainScanner::category_to_archetype("system"), None);
        assert_eq!(OnchainScanner::category_to_archetype("unknown"), None);
        assert_eq!(OnchainScanner::category_to_archetype(""), None);
    }

    #[test]
    fn all_mapped_archetypes_are_in_known_set() {
        use crate::scanners::is_known_archetype;
        let categories = [
            "bridge",
            "governance",
            "infrastructure",
            "mapstore",
            "treasury",
        ];
        for cat in categories {
            let arche = OnchainScanner::category_to_archetype(cat)
                .expect("mapped category has archetype");
            assert!(
                is_known_archetype(arche),
                "category {} -> {} is not in KNOWN_ARCHETYPES",
                cat,
                arche
            );
        }
    }

    #[test]
    fn scanner_disabled_when_config_disabled() {
        let cfg = OnchainScannerConfig {
            enabled: false,
            dcscan_base: None,
            lookback_blocks: None,
        };
        let scanner = OnchainScanner::new(cfg, Duration::from_secs(10))
            .expect("construct scanner");
        assert!(!scanner.enabled());
    }

    #[test]
    fn scanner_rejects_invalid_dcscan_base() {
        let cfg = OnchainScannerConfig {
            enabled: true,
            dcscan_base: Some("not a url".to_string()),
            lookback_blocks: None,
        };
        let res = OnchainScanner::new(cfg, Duration::from_secs(10));
        assert!(res.is_err(), "invalid URL should be rejected on construction");
    }

    #[test]
    fn scanner_uses_default_base_when_none() {
        let cfg = OnchainScannerConfig {
            enabled: true,
            dcscan_base: None,
            lookback_blocks: None,
        };
        let scanner = OnchainScanner::new(cfg, Duration::from_secs(10))
            .expect("construct scanner");
        assert_eq!(scanner.dcscan_base, "https://dcscan.io");
    }

    #[test]
    fn scanner_trims_trailing_slash_from_base() {
        let cfg = OnchainScannerConfig {
            enabled: true,
            dcscan_base: Some("https://dcscan.io/".to_string()),
            lookback_blocks: None,
        };
        let scanner = OnchainScanner::new(cfg, Duration::from_secs(10))
            .expect("construct scanner");
        assert_eq!(scanner.dcscan_base, "https://dcscan.io");
    }

    #[test]
    fn parse_labels_response_shape() {
        let body = r#"{
            "count": 2,
            "labels": {
                "0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb": {
                    "label": "Datachain Foundation Operator",
                    "icon": "fa-landmark",
                    "category": "treasury",
                    "hidden": false
                },
                "0xdabf1af728223041c82d11755b114e25d9c05030": {
                    "label": "Mapstore Operator (Disputes)",
                    "icon": "fa-gavel",
                    "category": "mapstore",
                    "hidden": false
                }
            }
        }"#;
        let parsed: LabelsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.count, 2);
        assert_eq!(parsed.labels.len(), 2);
        assert!(parsed
            .labels
            .contains_key("0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb"));
    }

    #[test]
    fn parse_labels_response_tolerates_missing_optional_fields() {
        // No `icon`, no `category`, no `hidden` - all should default.
        let body = r#"{
            "count": 1,
            "labels": {
                "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef": {
                    "label": "Bare Label"
                }
            }
        }"#;
        let parsed: LabelsResponse = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.labels.len(), 1);
        let (_, raw) = parsed.labels.iter().next().unwrap();
        assert_eq!(raw.label, "Bare Label");
        assert!(raw.category.is_none());
        assert!(raw.icon.is_none());
        assert!(!raw.hidden);
    }
}
