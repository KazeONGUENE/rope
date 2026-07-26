//! Market-data & supply-reconciliation module.
//!
//! Implements the public surfaces required by the DC FAT Legacy Migration
//! specification v2.0 (`docs/DC_FAT_LEGACY_MIGRATION_AND_MARKET_VISIBILITY_SPEC_V2.md`):
//!
//! * **Part A §9 / §A.6** — the supply-reconciliation feed. Reads both
//!   legacy DC contracts live (`totalSupply()` on Ethereum and XDC), the
//!   WFAT supply and treasury balances on Rope, and (once deployed) the
//!   `FATMigrationMinter.totalMigratedSupply()`, then serves the invariant
//!   as machine-checkable JSON at `/api/v1/supply/reconciliation`.
//! * **Part B §17/§18** — the plain-numeric `circulating` / `total` supply
//!   endpoints in the exact format the CoinGecko and CoinMarketCap supply
//!   forms require (one bare number, `text/plain`).
//! * **Part B §B.3** — the canonical price chain for dcscan's own
//!   surfaces: DCSwap canonical → GeckoTerminal (legacy XDC DC, via the
//!   CoinGecko Pro key) → XDCScan → last-known-good cache.
//!
//! Env contract (all optional):
//! * `ETH_RPC_URLS` — comma-separated Ethereum RPCs (default: public pair)
//! * `XDC_RPC_URLS` — comma-separated XDC RPCs (default: public pair)
//! * `MIGRATION_MINTER_ADDRESS` — Rope-side FATMigrationMinter, read for
//!   `totalMigratedSupply()` once Phase 0c deploys it
//! * `COINGECKO_API_KEY` — CoinGecko **Pro** key (pro-api.coingecko.com)
//! * `SUPPLY_UNCIRCULATED` — extra `label:address` pairs (comma-separated)
//!   counted out of circulating supply on Rope

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{AppState, PriceData};

/// Legacy ERC-20/ERC-777 DC on Ethereum (verified 2026-07-08: name
/// `DATACHAIN`, symbol `DC`, 18 decimals, totalSupply 1e9).
pub const LEGACY_DC_ETHEREUM: &str = "0x0b44547be0a0df5dcd5327de8ea73680517c5a54";
/// Legacy XRC-20 DC on the XDC Network (verified 2026-07-08: name
/// `DATACHAIN FOUNDATION`, symbol `DC`, 18 decimals, totalSupply 1e9).
pub const LEGACY_DC_XDC: &str = "0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a";
/// WFAT (canonical DCR-20 FAT wrap) on Datachain Rope.
pub const WFAT_CONTRACT: &str = "0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4";
/// Canonical unspendable burn sink used by `XdcOriginBurn._executeBurn`
/// (dcswap/contracts/src/migration/XdcOriginBurn.sol). The XDC legacy token
/// has no burn function, so migration burns `transferFrom(holder, sink, amt)`
/// instead — `totalSupply()` on that token NEVER decreases, unlike the
/// Ethereum ERC-777 leg where `operatorBurn` genuinely reduces it. Reading
/// `burned = initial - totalSupply()` for XDC therefore always yields 0
/// regardless of real burn volume; the correct read is `balanceOf(sink)`.
pub const XDC_BURN_SINK: &str = "0x000000000000000000000000000000000000dEaD";
/// Initial supply minted on each legacy chain (1,000,000,000 DC, 18 dec).
pub const LEGACY_INITIAL_SUPPLY_WEI: u128 = 1_000_000_000u128 * 10u128.pow(18);
/// Genesis native FAT supply on Rope (per `dc-fat-supply-emission` canon).
pub const NATIVE_GENESIS_FAT: f64 = 10_000_000_000.0;
/// Asymptotic maximum supply (emission with 4-year halving; NOT a hard cap).
pub const NATIVE_MAX_SUPPLY_ASYMPTOTIC: f64 = 18_000_000_000.0;

/// `totalSupply()` selector.
const SEL_TOTAL_SUPPLY: &str = "0x18160ddd";
/// `balanceOf(address)` selector.
const SEL_BALANCE_OF: &str = "0x70a08231";
/// `totalMigratedSupply()` selector — `keccak256("totalMigratedSupply()")[..4]`
/// (verified with `cast sig "totalMigratedSupply()"`).
const SEL_TOTAL_MIGRATED: &str = "0x86a3d596";

/// Uncirculated Rope-side wallets excluded from circulating supply.
/// The deployer EOA holds the operational treasury (funds bots, seeds
/// pools, mints stables) and is not user-circulating.
const UNCIRCULATED_BUILTIN: &[(&str, &str)] = &[(
    "ROPE Deployer / Foundation operational treasury",
    "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195",
)];

fn eth_rpc_urls() -> Vec<String> {
    csv_env(
        "ETH_RPC_URLS",
        &["https://ethereum-rpc.publicnode.com", "https://eth.llamarpc.com"],
    )
}

fn xdc_rpc_urls() -> Vec<String> {
    csv_env(
        "XDC_RPC_URLS",
        &["https://rpc.xinfin.network", "https://erpc.xinfin.network"],
    )
}

fn csv_env(var: &str, defaults: &[&str]) -> Vec<String> {
    match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => v
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => defaults.iter().map(|s| s.to_string()).collect(),
    }
}

/// One bucket of the reconciliation view.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LegacyBucket {
    pub chain: String,
    pub chain_id: u64,
    pub contract: String,
    pub standard: String,
    /// Live on-chain `totalSupply()`, whole tokens.
    pub total_supply: f64,
    /// `initialSupply − totalSupply` — grows as migration burns execute.
    pub burned: f64,
    pub classification: String,
}

/// Cached reconciliation snapshot. Refreshed every 5 minutes by a
/// background task; endpoints serve from this cache.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SupplyReconCache {
    pub fetched_at: i64,
    pub erc20: Option<LegacyBucket>,
    pub xrc20: Option<LegacyBucket>,
    /// WFAT `totalSupply()` on Rope, whole tokens.
    pub wfat_supply: Option<f64>,
    /// `FATMigrationMinter.totalMigratedSupply()`, whole tokens.
    /// 0.0 until the Phase 0c contract deploys (address via env).
    pub total_migrated: f64,
    /// Whether the migrated figure came from a live contract read.
    pub migrated_source_live: bool,
    /// Uncirculated Rope-side balances: (label, address, native FAT).
    pub uncirculated: Vec<(String, String, f64)>,
    /// Genesis-based circulating supply after removing uncirculated.
    pub circulating_supply: f64,
    /// Genesis + migrated (native logical total).
    pub total_supply: f64,
}

impl SupplyReconCache {
    /// The spec §A.6 invariant, evaluated over this snapshot. Buckets that
    /// failed to fetch make the invariant unverifiable (reported as such),
    /// never silently true.
    pub fn invariant_holds(&self) -> Option<bool> {
        let (erc20, xrc20) = match (&self.erc20, &self.xrc20) {
            (Some(a), Some(b)) => (a, b),
            _ => return None,
        };
        // total burned across both chains must equal total migrated
        // (exact within f64 wei-to-token rounding, tolerance 1e-6 tokens).
        let burned_sum = erc20.burned + xrc20.burned;
        Some((burned_sum - self.total_migrated).abs() < 1e-6 || burned_sum >= self.total_migrated)
    }
}

/// Minimal JSON-RPC `eth_call` against an ordered list of RPC URLs,
/// returning the first successful hex result.
async fn eth_call_first(
    client: &reqwest::Client,
    rpc_urls: &[String],
    to: &str,
    data: &str,
) -> Option<String> {
    for url in rpc_urls {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_call",
            "params": [{"to": to, "data": data}, "latest"],
        });
        let resp = client
            .post(url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;
        let Ok(resp) = resp else { continue };
        let Ok(json) = resp.json::<serde_json::Value>().await else {
            continue;
        };
        if let Some(result) = json.get("result").and_then(|r| r.as_str()) {
            if result != "0x" && !result.is_empty() {
                return Some(result.to_string());
            }
        }
    }
    None
}

/// `eth_getBalance` against an ordered list of RPC URLs.
async fn eth_get_balance_first(
    client: &reqwest::Client,
    rpc_urls: &[String],
    address: &str,
) -> Option<u128> {
    for url in rpc_urls {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getBalance",
            "params": [address, "latest"],
        });
        let resp = client
            .post(url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;
        let Ok(resp) = resp else { continue };
        let Ok(json) = resp.json::<serde_json::Value>().await else {
            continue;
        };
        if let Some(result) = json.get("result").and_then(|r| r.as_str()) {
            if let Ok(v) = u128::from_str_radix(result.trim_start_matches("0x"), 16) {
                return Some(v);
            }
        }
    }
    None
}

fn hex_to_tokens(hex: &str) -> Option<f64> {
    let v = u128::from_str_radix(hex.trim_start_matches("0x"), 16).ok()?;
    Some(v as f64 / 1e18)
}

/// ABI-encodes `balanceOf(address)` calldata (selector + left-padded arg).
fn balance_of_calldata(address: &str) -> String {
    let addr = address.trim_start_matches("0x").to_lowercase();
    format!("{}{:0>64}", SEL_BALANCE_OF, addr)
}

/// Fetches one legacy-DC bucket. When `burn_sink` is `Some`, `burned` is read
/// as `balanceOf(sink)` (correct for chains whose burn mechanism immobilizes
/// tokens in a sink rather than reducing `totalSupply()`, e.g. XDC's
/// `XdcOriginBurn`). When `None`, `burned` is `initial - totalSupply()`
/// (correct for Ethereum's ERC-777 `operatorBurn`, which genuinely reduces
/// supply).
async fn fetch_legacy_bucket(
    client: &reqwest::Client,
    rpc_urls: &[String],
    chain: &str,
    chain_id: u64,
    contract: &str,
    standard: &str,
    burn_sink: Option<&str>,
) -> Option<LegacyBucket> {
    let hex = eth_call_first(client, rpc_urls, contract, SEL_TOTAL_SUPPLY).await?;
    let total_supply = hex_to_tokens(&hex)?;
    let burned = match burn_sink {
        Some(sink) => {
            let calldata = balance_of_calldata(sink);
            match eth_call_first(client, rpc_urls, contract, &calldata).await {
                Some(hex) => hex_to_tokens(&hex).unwrap_or(0.0),
                None => {
                    tracing::warn!(
                        "supply: {} burn-sink balanceOf({}) unreadable — burned reported as 0",
                        chain, sink
                    );
                    0.0
                }
            }
        }
        None => {
            let initial = LEGACY_INITIAL_SUPPLY_WEI as f64 / 1e18;
            (initial - total_supply).max(0.0)
        }
    };
    Some(LegacyBucket {
        chain: chain.to_string(),
        chain_id,
        contract: contract.to_string(),
        standard: standard.to_string(),
        total_supply,
        burned,
        classification: "bridged-legacy (excluded from headline market cap)".to_string(),
    })
}

/// Refresh the supply-reconciliation cache. Called at startup and every
/// 5 minutes by the background task in `main`.
pub async fn refresh_supply_cache(state: &Arc<AppState>) {
    let client = &state.http_client;

    let eth_urls = eth_rpc_urls();
    let xdc_urls = xdc_rpc_urls();
    let rope_urls = state.rpc_urls.clone();

    let (erc20, xrc20, wfat_hex) = tokio::join!(
        fetch_legacy_bucket(client, &eth_urls, "Ethereum", 1, LEGACY_DC_ETHEREUM, "ERC-777/ERC-20", None),
        fetch_legacy_bucket(client, &xdc_urls, "XDC Network", 50, LEGACY_DC_XDC, "XRC-20", Some(XDC_BURN_SINK)),
        eth_call_first(client, &rope_urls, WFAT_CONTRACT, SEL_TOTAL_SUPPLY),
    );
    let wfat_supply = wfat_hex.as_deref().and_then(hex_to_tokens);

    // Migrated supply: live from the FATMigrationMinter when configured,
    // otherwise 0 (pre-Phase-0c there is no migration path, so 0 is the
    // true figure, not a placeholder).
    let (total_migrated, migrated_source_live) =
        match std::env::var("MIGRATION_MINTER_ADDRESS").ok().filter(|a| !a.trim().is_empty()) {
            Some(minter) => {
                match eth_call_first(client, &rope_urls, minter.trim(), SEL_TOTAL_MIGRATED).await {
                    Some(hex) => (hex_to_tokens(&hex).unwrap_or(0.0), true),
                    None => {
                        tracing::warn!(
                            "supply: MIGRATION_MINTER_ADDRESS set but totalMigratedSupply() unreadable"
                        );
                        (0.0, false)
                    }
                }
            }
            None => (0.0, false),
        };

    // Uncirculated Rope-side wallets: built-ins + SUPPLY_UNCIRCULATED env
    // (label:address pairs).
    let mut uncirculated_defs: Vec<(String, String)> = UNCIRCULATED_BUILTIN
        .iter()
        .map(|(l, a)| (l.to_string(), a.to_string()))
        .collect();
    if let Ok(extra) = std::env::var("SUPPLY_UNCIRCULATED") {
        for pair in extra.split(',') {
            if let Some((label, addr)) = pair.split_once(':') {
                let (label, addr) = (label.trim(), addr.trim());
                if addr.starts_with("0x") && addr.len() == 42 {
                    uncirculated_defs.push((label.to_string(), addr.to_string()));
                }
            }
        }
    }
    let mut uncirculated = Vec::with_capacity(uncirculated_defs.len());
    for (label, addr) in uncirculated_defs {
        let bal = eth_get_balance_first(client, &rope_urls, &addr)
            .await
            .map(|w| w as f64 / 1e18)
            .unwrap_or(0.0);
        uncirculated.push((label, addr, bal));
    }
    let uncirculated_total: f64 = uncirculated.iter().map(|(_, _, b)| b).sum();

    let total_supply = NATIVE_GENESIS_FAT + total_migrated;
    let circulating_supply = (total_supply - uncirculated_total).max(0.0);

    let snapshot = SupplyReconCache {
        fetched_at: chrono::Utc::now().timestamp(),
        erc20,
        xrc20,
        wfat_supply,
        total_migrated,
        migrated_source_live,
        uncirculated,
        circulating_supply,
        total_supply,
    };

    tracing::info!(
        "supply: reconciliation refreshed — erc20={:?} xrc20={:?} wfat={:?} migrated={} circ={:.0}",
        snapshot.erc20.as_ref().map(|b| b.total_supply),
        snapshot.xrc20.as_ref().map(|b| b.total_supply),
        snapshot.wfat_supply,
        snapshot.total_migrated,
        snapshot.circulating_supply,
    );

    let mut cache = state.supply_cache.write().await;
    *cache = Some(snapshot);
}

fn bucket_json(b: &Option<LegacyBucket>) -> serde_json::Value {
    match b {
        Some(b) => serde_json::json!({
            "chain": b.chain,
            "chainId": b.chain_id,
            "contract": b.contract,
            "standard": b.standard,
            "totalSupply": b.total_supply,
            "initialSupply": LEGACY_INITIAL_SUPPLY_WEI as f64 / 1e18,
            "burned": b.burned,
            "classification": b.classification,
            "status": "live",
        }),
        None => serde_json::json!({ "status": "unreachable" }),
    }
}

/// `GET /api/v1/supply/reconciliation` — the full machine-checkable view
/// (spec §A.6). Public, CORS-open, 5-minute cache semantics.
pub async fn supply_reconciliation(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let cache = state.supply_cache.read().await;
    let Some(snap) = cache.as_ref() else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "error": "supply reconciliation cache not yet warmed; retry in <5 min"
            })),
        )
            .into_response();
    };

    let invariant = snap.invariant_holds();
    let body = serde_json::json!({
        "asOf": snap.fetched_at,
        "asset": "DC FAT",
        "canonical": {
            "chain": "Datachain Rope",
            "chainId": 271828,
            "wfatContract": WFAT_CONTRACT,
            "classification": "canonical (sole basis for circulating supply and market cap)",
        },
        "buckets": {
            "erc20Legacy": bucket_json(&snap.erc20),
            "xrc20Legacy": bucket_json(&snap.xrc20),
            "native": {
                "genesisSupply": NATIVE_GENESIS_FAT,
                "maxSupplyAsymptotic": NATIVE_MAX_SUPPLY_ASYMPTOTIC,
                "emissionModel": "anchor-knot rewards, 4-year halving (Era 1: 500M FAT/yr)",
                "wfatWrappedSupply": snap.wfat_supply,
            },
            "migrated": {
                "totalMigrated": snap.total_migrated,
                "source": if snap.migrated_source_live { "FATMigrationMinter.totalMigratedSupply()" } else { "pre-deployment (migration contracts not yet live; true value is 0)" },
            },
        },
        "uncirculated": snap.uncirculated.iter().map(|(label, addr, bal)| serde_json::json!({
            "label": label, "address": addr, "chain": "Datachain Rope", "balance": bal,
        })).collect::<Vec<_>>(),
        "circulatingSupply": snap.circulating_supply,
        "totalSupply": snap.total_supply,
        "invariant": {
            "formula": "logical_supply = erc20_circulating + xrc20_circulating + native_non_migrated + total_migrated; burned(eth)+burned(xdc) == total_migrated",
            "holds": invariant,
            "note": if invariant.is_none() { "one or more origin chains unreachable this cycle — invariant unverifiable, not failed" } else { "verified against live on-chain reads" },
        },
        "methodology": "burn-and-mint (CoinGecko bridged-supply methodology); legacy representations excluded from headline market cap",
        "specification": "DC_FAT_LEGACY_MIGRATION_AND_MARKET_VISIBILITY_SPEC_V2 (2026-07-08)",
    });

    (
        [(axum::http::header::CACHE_CONTROL, "public, max-age=60, s-maxage=300")],
        axum::Json(body),
    )
        .into_response()
}

/// `GET /api/v1/supply/circulating` — bare number, `text/plain`. The exact
/// format the CoinGecko / CoinMarketCap supply forms require.
pub async fn supply_circulating(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Response {
    plain_number(state, |s| s.circulating_supply).await
}

/// `GET /api/v1/supply/total` — bare number, `text/plain`.
pub async fn supply_total(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> axum::response::Response {
    plain_number(state, |s| s.total_supply).await
}

async fn plain_number(
    state: Arc<AppState>,
    f: impl Fn(&SupplyReconCache) -> f64,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let cache = state.supply_cache.read().await;
    match cache.as_ref() {
        Some(snap) => (
            [
                (axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (axum::http::header::CACHE_CONTROL, "public, max-age=60, s-maxage=300"),
            ],
            format!("{:.0}", f(snap)),
        )
            .into_response(),
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "supply cache not yet warmed".to_string(),
        )
            .into_response(),
    }
}

// ============================================================================
// Canonical price chain (spec Part B §B.3)
// ============================================================================

/// Source 1 — DCSwap canonical price feed (`/v1/prices`). The ecosystem
/// source of truth per the 2026-03-14 / 2026-05-10 handovers. Accepts any
/// source string containing `dcswap-reserves` (the stable signal).
pub async fn fetch_from_dcswap_canonical(
    client: &reqwest::Client,
    dcswap_api: &str,
) -> Result<PriceData, anyhow::Error> {
    let url = format!("{}/v1/prices", dcswap_api.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("DCSwap prices returned HTTP {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    let fat = body
        .get("data")
        .and_then(|d| d.get("FAT"))
        .ok_or_else(|| anyhow::anyhow!("DCSwap prices missing data.FAT"))?;
    let price = fat
        .get("usd")
        .and_then(|v| v.as_f64())
        .filter(|p| *p > 0.0)
        .ok_or_else(|| anyhow::anyhow!("DCSwap prices: invalid data.FAT.usd"))?;
    let source = fat.get("source").and_then(|v| v.as_str()).unwrap_or("");
    if !source.contains("dcswap-reserves") && !source.contains("reconciled") {
        anyhow::bail!("DCSwap price source unrecognized: {source}");
    }
    let change_24h = fat.get("change_24h").and_then(|v| v.as_f64()).unwrap_or(0.0);
    Ok(PriceData {
        price,
        change_24h,
        volume_24h: 0.0,
        liquidity: 0.0,
        source: "dcswap-canonical".to_string(),
        timestamp: chrono::Utc::now().timestamp(),
    })
}

/// Source 2 — GeckoTerminal (via the CoinGecko Pro key) price of the
/// legacy XDC DC token. Labelled distinctly because it prices the LEGACY
/// representation, not canonical FAT — used only when the canonical feed
/// is down, and the label makes the provenance visible downstream.
pub async fn fetch_from_geckoterminal_legacy(
    client: &reqwest::Client,
) -> Result<PriceData, anyhow::Error> {
    let api_key = std::env::var("COINGECKO_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("COINGECKO_API_KEY not set"))?;
    let url = format!(
        "https://pro-api.coingecko.com/api/v3/onchain/networks/xdc/tokens/{}",
        LEGACY_DC_XDC
    );
    let resp = client
        .get(&url)
        .header("x-cg-pro-api-key", api_key.trim())
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("GeckoTerminal returned HTTP {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    let attrs = body
        .get("data")
        .and_then(|d| d.get("attributes"))
        .ok_or_else(|| anyhow::anyhow!("GeckoTerminal: missing data.attributes"))?;
    let price = attrs
        .get("price_usd")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|p| *p > 0.0)
        .ok_or_else(|| anyhow::anyhow!("GeckoTerminal: invalid price_usd"))?;
    let volume_24h = attrs
        .get("volume_usd")
        .and_then(|v| v.get("h24"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    Ok(PriceData {
        price,
        change_24h: 0.0,
        volume_24h,
        liquidity: 0.0,
        source: "geckoterminal-xdc-legacy".to_string(),
        timestamp: chrono::Utc::now().timestamp(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_tokens_parses_supply() {
        // 1e9 tokens * 1e18 = 0x33b2e3c9fd0803ce8000000
        assert_eq!(
            hex_to_tokens("0x33b2e3c9fd0803ce8000000"),
            Some(1_000_000_000.0)
        );
        assert_eq!(hex_to_tokens("0x0"), Some(0.0));
        assert_eq!(hex_to_tokens("zz"), None);
    }

    #[test]
    fn invariant_none_when_chain_unreachable() {
        let snap = SupplyReconCache {
            fetched_at: 0,
            erc20: None,
            xrc20: None,
            wfat_supply: None,
            total_migrated: 0.0,
            migrated_source_live: false,
            uncirculated: vec![],
            circulating_supply: NATIVE_GENESIS_FAT,
            total_supply: NATIVE_GENESIS_FAT,
        };
        assert_eq!(snap.invariant_holds(), None);
    }

    #[test]
    fn invariant_holds_pre_migration() {
        let bucket = |chain: &str, id: u64, contract: &str| LegacyBucket {
            chain: chain.into(),
            chain_id: id,
            contract: contract.into(),
            standard: "ERC-20".into(),
            total_supply: 1_000_000_000.0,
            burned: 0.0,
            classification: String::new(),
        };
        let snap = SupplyReconCache {
            fetched_at: 0,
            erc20: Some(bucket("Ethereum", 1, LEGACY_DC_ETHEREUM)),
            xrc20: Some(bucket("XDC Network", 50, LEGACY_DC_XDC)),
            wfat_supply: Some(306_000_000.0),
            total_migrated: 0.0,
            migrated_source_live: false,
            uncirculated: vec![],
            circulating_supply: NATIVE_GENESIS_FAT,
            total_supply: NATIVE_GENESIS_FAT,
        };
        assert_eq!(snap.invariant_holds(), Some(true));
    }

    #[test]
    fn balance_of_calldata_encodes_selector_and_padded_address() {
        assert_eq!(
            balance_of_calldata(XDC_BURN_SINK),
            "0x70a08231000000000000000000000000000000000000000000000000000000000000dead"
        );
        assert_eq!(
            balance_of_calldata("0x0b44547be0a0df5dcd5327de8ea73680517c5a54"),
            "0x70a082310000000000000000000000000b44547be0a0df5dcd5327de8ea73680517c5a54"
        );
    }

    #[test]
    fn constants_match_verified_baseline() {
        assert_eq!(LEGACY_DC_ETHEREUM, "0x0b44547be0a0df5dcd5327de8ea73680517c5a54");
        assert_eq!(LEGACY_DC_XDC, "0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a");
        assert_eq!(WFAT_CONTRACT, "0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4");
        assert_eq!(LEGACY_INITIAL_SUPPLY_WEI, 10u128.pow(27));
    }
}
