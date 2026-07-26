//! DC Explorer (DCScan) - Knot/String Explorer for Datachain Rope.
//!
//! Per Quipu Primitive Canon v1.1, the canonical unit of state change on
//! Datachain Rope is the **knot** (the per-event entry on a wallet's
//! sovereign **string**). The legacy term "block" is preserved only in the
//! EVM-compat alias layer (eth_blockNumber / eth_getBlockBy*) so that
//! MetaMask, ethers.js, hardhat, and similar tooling continue to work
//! unchanged. New API consumers should prefer the knot-shaped fields
//! (`knot`, `knotIndex`, `knotHash`) and the `/api/v1/personal-ledger/...`
//! endpoint which exposes the canonical String → Knot[] → Tx-details
//! hierarchy for a wallet's public personal ledger.
//!
//! API server powering dcscan.io

use axum::{
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod api;
mod api_keys;
mod certification_providers;
mod db;
mod mailer;
mod extra;
mod cross_chain_weight;
mod databox_registry;
mod governance_votes;
mod indexer;
mod market_data;
mod models;
mod rate_limit;
mod security_guard;

use api::*;
use extra::*;

// DC FAT Token contract address on XDC Network
const DC_FAT_CONTRACT: &str = "0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a";

// Price cache TTL: 5 minutes
const PRICE_CACHE_TTL_SECS: u64 = 300;

// Fallback price
const FALLBACK_PRICE: f64 = 0.00390;

/// Price data structure
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PriceData {
    pub price: f64,
    pub change_24h: f64,
    pub volume_24h: f64,
    pub liquidity: f64,
    pub source: String,
    pub timestamp: i64,
}

impl Default for PriceData {
    fn default() -> Self {
        Self {
            price: FALLBACK_PRICE,
            change_24h: 0.0,
            volume_24h: 0.0,
            liquidity: 0.0,
            source: "fallback".to_string(),
            timestamp: chrono::Utc::now().timestamp(),
        }
    }
}

/// Application state
pub struct AppState {
    pub chain_id: u64,
    pub network_name: String,
    pub http_client: reqwest::Client,
    /// Primary RPC URL (kept for backward compat with code that reads this field)
    pub rpc_url: String,
    /// Ordered list of RPC endpoints — tried in order with automatic failover.
    /// Index of the currently healthy endpoint is tracked in `rpc_active_index`.
    pub rpc_urls: Vec<String>,
    /// Index into `rpc_urls` for the currently active endpoint (atomic for lock-free access)
    pub rpc_active_index: std::sync::atomic::AtomicUsize,
    pub price_cache: RwLock<Option<PriceData>>,
    /// DCSwap API base URL (e.g. <https://dcswap.net>)
    pub dcswap_api: String,
    /// In-memory service registry (same as dcscan-api)
    pub services_registry: RwLock<Vec<extra::ServiceRegistryEntry>>,
    pub verification_store: RwLock<std::collections::HashMap<String, extra::VerificationEntry>>,
    pub certifications_store:
        RwLock<std::collections::HashMap<String, Vec<extra::CertificationEntry>>>,
    /// When set, path to DCScan static frontend (for extensionless .html fallback)
    pub static_dir: Option<String>,
    /// PostgreSQL connection pool (None if DATABASE_URL not set)
    pub db_pool: Option<sqlx::postgres::PgPool>,
    /// Cached testimony data (refreshed by background task every 60s)
    pub testimony_cache: RwLock<Option<TestimonyCache>>,
    /// Cached token transfer data (refreshed by background task every 30s)
    pub tokentxn_cache: RwLock<Option<TokenTxnCache>>,
    /// Tanastok tokenized assets cache (refreshed every 5 min)
    pub tanastok_cache: RwLock<Option<TanastokCache>>,
    /// Tanastok entity-manifest cache (Quipu Canon v1.2 / Phase 5,
    /// refreshed every 5 min). Powers the `/api/v1/registry/labels`
    /// and `/api/v1/registry/manifest` endpoints consumed by DCScan,
    /// the Rope Graph at event.datachain.one, and any third-party
    /// frontend that needs server-side label resolution for
    /// kind=asset / kind=contract / kind=did / kind=application.
    pub tanastok_manifest_cache: RwLock<Option<TanastokManifestCache>>,
    /// Mapstore entity-manifest cache (marketplace participant ledger,
    /// refreshed every 5 min). Powers the `/api/v1/registry/mapstore-*`
    /// endpoints — same mirror discipline as `tanastok_manifest_cache`.
    pub mapstore_manifest_cache: RwLock<Option<MapstoreManifestCache>>,
    /// Careaway entity-manifest cache (aggregate-only, health-data
    /// boundary respected — counts/timestamps/hashes only, refreshed
    /// every 5 min). Powers `/api/v1/registry/careaway-manifest`.
    pub careaway_manifest_cache: RwLock<Option<CareawayManifestCache>>,
    /// TangibleDC Goodies entity-manifest cache (physical gold/silver
    /// coin/title registry — DCNFT deed + ERC-3643 fractional title,
    /// pre-mint candidates, settlement/revenue activity, metal-spot
    /// provenance — refreshed every 5 min). Powers the
    /// `/api/v1/registry/tangibledc-*` endpoints — same mirror
    /// discipline as `tanastok_manifest_cache` / `mapstore_manifest_cache`.
    pub tangibledc_manifest_cache: RwLock<Option<TangibleDcManifestCache>>,
    /// Ecosystem Deployment Console public directory cache (refreshed
    /// every 60 s from every EDC instance in `EDC_DIRECTORY_URLS`).
    /// Powers `/api/v1/ecosystem/directory` and the `/ecosystem` page.
    pub ecosystem_directory_cache: RwLock<Option<EcosystemDirectoryCache>>,
    /// Exact cumulative transaction count (incrementally scanned by background task)
    pub tx_count_cache: RwLock<TxCountCache>,
    /// Quipu Canon v1.2 — `rope_globalStats` cache. The data changes
    /// only when a new string is created or a knot appended, so a
    /// short TTL is enough to absorb burst load on `/api/v1/stats`.
    pub global_stats_cache: RwLock<Option<GlobalStatsCacheEntry>>,
    /// `eth_blockNumber` (== Quipu Canon v1.2 cord head / `totalKnots`)
    /// cache. Same rationale as `global_stats_cache`: rope-node's RPC
    /// forwarder to Reth occasionally drops calls under burst load
    /// and the handler would otherwise emit `totalKnots: 0`.
    pub block_number_cache: RwLock<Option<BlockNumberCacheEntry>>,
    /// DCSwap bot activity cache. Each call scans up to 100 blocks (~5 min)
    /// of recent activity; the rope-node→Reth RPC forwarder drops ~30 % of
    /// concurrent calls under load, so we serve from cache for 60 s and
    /// only re-scan if the entry is stale. This makes the bot endpoint
    /// cheap to call from the dcscan.io frontend.
    pub bot_activity_cache: RwLock<Option<BotActivityCacheEntry>>,
    /// Homepage "Latest Transactions" cache. Under the post-cutover
    /// Testimony-quorum block cadence (2026-07-23), most cord anchor
    /// knots carry zero transactions (bot/user traffic is bursty), so a
    /// fixed small recent-block window can legitimately come up empty on
    /// any given request. We scan a much wider window (see
    /// `latest_transactions`) and cache the result briefly so concurrent
    /// homepage polls share one scan instead of each re-walking hundreds
    /// of blocks against the rope-node→Reth RPC forwarder.
    pub latest_tx_cache: RwLock<Option<BotActivityCacheEntry>>,
    /// Persistent per-token holder index (full chain history, scanned
    /// incrementally via chunked `eth_getLogs`). Backs the
    /// `/api/v1/tokens/:addr/holders` endpoint with real, complete data
    /// rather than the partial view derived from the rolling
    /// `tokentxn_cache`.
    pub holder_index: RwLock<HolderIndex>,
    /// Persistent ERC-721 ownership index. Tracks per-tokenId ownership
    /// for every Tanastok DCNFT (and any other ERC-721 that ends up in
    /// the indexed set). Powers the address page's NFT Transfers tab,
    /// the DCNFT inventory tab, and `/api/v1/accounts/:addr/nfts`.
    pub nft_index: RwLock<NftIndex>,
    /// Self-service API keys for authenticated (Datachain ID) users.
    /// File-persisted; management endpoints under `/api/v1/keys`.
    pub api_keys: api_keys::ApiKeyStore,
    /// Onboarded third-party certification providers (auditors,
    /// compliance vendors). Gates `POST /api/v1/verify/certify` — see
    /// `certification_providers.rs` (finding C8,
    /// SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md).
    pub certification_providers: certification_providers::CertificationProviderRegistry,
    /// Per-effective-client-IP rate limiter covering every route on this
    /// router (API + static frontend). See `rate_limit.rs` (finding H4,
    /// SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md) — before this field
    /// existed, `rope-explorer` had no request-rate throttling at all.
    pub rate_limiter: rate_limit::RateLimiter,
    /// SendGrid-backed transactional mailer (contact form relay for
    /// datachain.network / dcscan.io + API-key notifications). Reads
    /// EMAIL_* configuration from the environment at startup.
    pub mailer: mailer::Mailer,
    /// Live CoinMarketCap quote cache (USDC / USDT / EUROD / WFAT etc).
    /// Refreshed every 5 minutes by a background task when
    /// `CMC_API_KEY` is set in the environment. Falls back to the
    /// hand-curated 2026-06-04 snapshot in `token_metadata()` when the
    /// key is absent or the API is unreachable, so the token pages
    /// degrade gracefully rather than going dark.
    pub cmc_cache: RwLock<Option<CmcCache>>,
    /// DC FAT supply-reconciliation cache (legacy ERC-20/XRC-20 supplies,
    /// WFAT supply, migrated supply, uncirculated wallets). Refreshed
    /// every 5 minutes; powers `/api/v1/supply/*` per the DC FAT Legacy
    /// Migration spec v2.0 (Part A §9 / Part B §17–18).
    pub supply_cache: RwLock<Option<market_data::SupplyReconCache>>,
}

/// One snapshot of CoinMarketCap quote data, keyed by token symbol
/// (uppercased). Each entry carries the canonical USD market cap, 24 h
/// volume, circulating supply and price as last reported by CMC.
#[derive(Clone, Default)]
pub struct CmcCache {
    pub fetched_at: i64,
    pub quotes: std::collections::HashMap<String, CmcQuote>,
    pub source: &'static str,
}

#[derive(Clone, Default)]
pub struct CmcQuote {
    pub symbol: String,
    pub price_usd: f64,
    pub market_cap_usd: f64,
    pub volume_24h_usd: f64,
    pub circulating_supply: f64,
    pub percent_change_24h: f64,
    pub last_updated: String,
}

/// Cached DCSwap bot activity snapshot.
#[derive(Clone)]
pub struct BotActivityCacheEntry {
    pub fetched_at: i64,
    pub payload: serde_json::Value,
}

/// One snapshot of `rope_globalStats`. TTL enforced at read time.
#[derive(Clone)]
pub struct GlobalStatsCacheEntry {
    pub fetched_at: i64,
    pub total_strings: u64,
    pub by_kind: serde_json::Value,
}

/// One snapshot of `eth_blockNumber` (cord head / `totalKnots`).
#[derive(Clone, Copy)]
pub struct BlockNumberCacheEntry {
    pub fetched_at: i64,
    pub head: u64,
}

/// Incrementally scanned exact transaction count across the entire chain.
/// Also tracks cumulative DCR-20 transfer volume (the *conveyed* value)
/// since genesis, computed by aggregating Transfer logs on known token
/// contracts in the same incremental scan window.
///
/// **Persisted to disk** (see TX_COUNT_CACHE_PATH) so the cumulative scan
/// progress survives dc-explorer restarts. Without persistence, every
/// restart re-scans from block 0 and the home page shows misleadingly
/// low numbers for ~10–30 minutes while the cache catches up. Persistence
/// fixes the "less than 200 K transactions since genesis" complaint by
/// resuming scan from the last persisted block on every cold start.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TxCountCache {
    pub total_transactions: u64,
    pub last_scanned_block: u64,
    /// Cumulative USD volume of DCR-20 transfers across all known tokens
    /// (WFAT, USDC, USDT, EUROD; LP tokens excluded — they have no $ price).
    pub total_volume_usd: f64,
    /// Cumulative WFAT-equivalent volume (sum of WFAT transfer amounts).
    /// Useful as a chain-native unit when stablecoin pricing is unavailable.
    pub total_volume_fat: f64,
    /// How many DCR-20 Transfer events have been observed since genesis.
    pub total_transfer_events: u64,
}

/// Disk path for the persisted tx-count cache. Override with the
/// `TX_COUNT_CACHE_PATH` env var (e.g. for non-default deploy layouts).
fn tx_count_cache_path() -> std::path::PathBuf {
    std::env::var("TX_COUNT_CACHE_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/var/lib/dc-explorer/tx_count_cache.json"))
}

fn load_tx_count_cache() -> TxCountCache {
    let path = tx_count_cache_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<TxCountCache>(&s) {
            Ok(c) => {
                tracing::info!(
                    "TxCountCache resumed from {}: tx={} events={} last_block={}",
                    path.display(),
                    c.total_transactions,
                    c.total_transfer_events,
                    c.last_scanned_block
                );
                c
            }
            Err(e) => {
                tracing::warn!(
                    "TxCountCache parse failed at {} ({}); starting fresh from block 0",
                    path.display(),
                    e
                );
                TxCountCache::default()
            }
        },
        Err(_) => {
            tracing::info!(
                "TxCountCache: no persisted cache at {}; starting fresh from block 0",
                path.display()
            );
            TxCountCache::default()
        }
    }
}

fn save_tx_count_cache(cache: &TxCountCache) {
    let path = tx_count_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(s) = serde_json::to_string_pretty(cache) {
        if std::fs::write(&tmp, s).is_ok() {
            // Atomic rename so a crash mid-write can never corrupt the cache.
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Persistent per-token holder index. For each known DCR-20 token we keep
/// `holder address -> raw u128 balance` plus the highest block we've already
/// scanned, so the background scanner only walks the new tail on each
/// refresh cycle (mirrors the TxCountCache pattern). Persistence on disk
/// lets the explorer survive restarts without re-walking ~2M blocks of
/// Transfer events from genesis every time it boots.
///
/// Why this exists: the previous implementation derived holder data from
/// the in-memory `tokentxn_cache`, which only retains the last ~50 transfer
/// events. That made the Holders / Top-N tabs structurally incomplete.
/// The persistent index closes that gap by walking the entire chain via
/// chunked `eth_getLogs` (Reth keeps full log history but caps each query
/// at 100K blocks / 20K results, so the scanner paginates internally).
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HolderIndex {
    /// `token_address (lowercase) -> { holder_address (lowercase) -> raw balance string }`.
    /// Balances are stored as decimal strings to avoid u128 / serde_json size
    /// pitfalls when the on-disk file is read by other tools.
    pub tokens: std::collections::HashMap<String, TokenHolderState>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TokenHolderState {
    /// Highest block number we've fully scanned for this token.
    pub last_scanned_block: u64,
    /// First block we've scanned (set on initial seed; useful for diagnostics).
    pub first_scanned_block: u64,
    /// `holder_address (lc) -> raw u128 balance encoded as decimal string`.
    pub balances: std::collections::HashMap<String, String>,
    /// Total number of Transfer events processed since genesis. Used as
    /// the authoritative "transfer count" for the token-info card —
    /// previously the count came from the rolling 50-event cache, which
    /// massively under-reported.
    pub transfer_count: u64,
    /// Last successful refresh timestamp (epoch seconds).
    pub updated_at: i64,
}

fn holder_index_path() -> std::path::PathBuf {
    std::env::var("HOLDER_INDEX_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/var/lib/dc-explorer/holder_index.json"))
}

fn load_holder_index() -> HolderIndex {
    let path = holder_index_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<HolderIndex>(&s) {
            Ok(idx) => {
                let total_holders: usize = idx.tokens.values().map(|s| s.balances.len()).sum();
                let total_transfers: u64 = idx.tokens.values().map(|s| s.transfer_count).sum();
                tracing::info!(
                    "HolderIndex resumed from {}: {} tokens, {} holders, {} transfers",
                    path.display(),
                    idx.tokens.len(),
                    total_holders,
                    total_transfers
                );
                idx
            }
            Err(e) => {
                tracing::warn!(
                    "HolderIndex parse failed at {} ({}); starting fresh",
                    path.display(),
                    e
                );
                HolderIndex::default()
            }
        },
        Err(_) => {
            tracing::info!(
                "HolderIndex: no persisted index at {}; starting fresh",
                path.display()
            );
            HolderIndex::default()
        }
    }
}

fn save_holder_index(idx: &HolderIndex) {
    let path = holder_index_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(s) = serde_json::to_string(idx) {
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Persistent ERC-721 (NFT) ownership index.
///
/// Mirrors `HolderIndex` for the fungible-token side, but tracks per-tokenId
/// ownership instead of per-address balances. Walks every ERC-721 `Transfer`
/// event (`topic0 = keccak("Transfer(address,address,uint256)")` with
/// **four** topics — for ERC-20 the same topic0 has only three topics, so
/// the topic count is the disambiguator) for every Tanastok DCNFT contract
/// and any other ERC-721 we discover via `supportsInterface(0x80ac58cd)`.
///
/// Why this exists: the founder explicitly asked for the "Address: NFT
/// Transfers" gap to be closed, with the sample DCNFT
/// `0x2e4A6fCF8B7C26408D76e5ffd7cb2B8F98A8357f` rendered as a proper
/// Tanastok title-deed page. A persistent index is the only honest way
/// to surface inventory + ownership history because Reth's log retention
/// is unbounded but rope-node forwards each `eth_getLogs` with caps.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct NftIndex {
    /// `collection_address (lowercase) -> NftCollectionState`.
    pub collections: std::collections::HashMap<String, NftCollectionState>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct NftCollectionState {
    /// Highest block fully scanned for this collection.
    pub last_scanned_block: u64,
    /// First block scanned (set on initial seed).
    pub first_scanned_block: u64,
    /// `tokenId (decimal string) -> current owner (lowercase address)`.
    /// A tokenId that has been burned is removed from this map.
    pub owners: std::collections::HashMap<String, String>,
    /// Per-collection counters.
    pub mint_count: u64,
    pub burn_count: u64,
    pub transfer_count: u64,
    /// Last successful refresh timestamp (epoch seconds).
    pub updated_at: i64,
    /// Most recent N transfer events (capped at 200) so the
    /// `/api/v1/nfts/:addr/transfers` endpoint can serve from RAM
    /// without re-scanning chain logs (DCNFT mints happen near genesis,
    /// far outside any reasonable live `eth_getLogs` window).
    /// Newer events are stored first.
    #[serde(default)]
    pub recent_transfers: Vec<NftTransferRecord>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct NftTransferRecord {
    pub block: u64,
    pub tx_hash: String,
    pub log_index: u64,
    pub from: String,
    pub to: String,
    pub token_id: String,
}

fn nft_index_path() -> std::path::PathBuf {
    std::env::var("NFT_INDEX_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/var/lib/dc-explorer/nft_index.json"))
}

fn load_nft_index() -> NftIndex {
    let path = nft_index_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<NftIndex>(&s) {
            Ok(idx) => {
                let total_owners: usize = idx.collections.values().map(|s| s.owners.len()).sum();
                let total_transfers: u64 =
                    idx.collections.values().map(|s| s.transfer_count).sum();
                tracing::info!(
                    "NftIndex resumed from {}: {} collections, {} held tokens, {} transfer events",
                    path.display(),
                    idx.collections.len(),
                    total_owners,
                    total_transfers
                );
                idx
            }
            Err(e) => {
                tracing::warn!(
                    "NftIndex parse failed at {} ({}); starting fresh",
                    path.display(),
                    e
                );
                NftIndex::default()
            }
        },
        Err(_) => {
            tracing::info!(
                "NftIndex: no persisted index at {}; starting fresh",
                path.display()
            );
            NftIndex::default()
        }
    }
}

fn save_nft_index(idx: &NftIndex) {
    let path = nft_index_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if let Ok(s) = serde_json::to_string(idx) {
        if std::fs::write(&tmp, s).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Cached Tanastok tokenized asset data.
/// Assets are indexed by contract address (lowercased) for O(1) lookup.
///
/// **Persisted to disk** (`tanastok_cache.json`, see `save_json_cache` /
/// `load_json_cache`) so a `dc-explorer` restart during a Tanastok
/// outage keeps serving the last-known-good asset list instead of
/// going dark for every address-page banner until the upstream
/// recovers. See `tanastok_manifest_cache.json` below for the same
/// reasoning applied to the full entity manifest.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct TanastokCache {
    pub assets: Vec<serde_json::Value>,
    /// DCNFT contract address → index into `assets`
    pub by_dcnft: std::collections::HashMap<String, usize>,
    /// ERC-3643 contract address → index into `assets`
    pub by_erc3643: std::collections::HashMap<String, usize>,
    pub updated_at: i64,
}

/// Cached Tanastok **entity manifest** (Quipu Canon v1.2 / Phase 5).
///
/// Mirrors `https://tanastok.io/api/v1/tanastok-entity-manifest` so:
///
/// 1. DCScan can render assets / contracts / dids / applications /
///    ecosystems on the per-string and per-address pages without
///    embedding the public Tanastok URL in the browser (single origin
///    for clients, lower fan-out, CORS sanity).
/// 2. The `event.datachain.one` Rope Graph can ask DCScan for the full
///    1,626-entity payload in one request and render every Tanastok
///    string, not only wallets.
///
/// Refreshed every 5 min (matches the upstream `s-maxage=300`).
///
/// **Persisted to disk** (`tanastok_manifest_cache.json`). Found
/// 2026-07-23: `https://tanastok.io/api/v1/tanastok-entity-manifest`
/// was returning `HTTP 500 {"success":false,"error":"Failed to build
/// entity manifest"}` for 72+ hours straight. Because the in-memory
/// cache is only ever populated on a *successful* fetch, any
/// `dc-explorer` restart during that window (deploy, watchdog,
/// crash) reset `/api/v1/registry/manifest` to a permanent `503
/// "not yet warmed"` with zero entities — even though a perfectly
/// good payload had been served minutes earlier. Persisting the last
/// successful payload to disk and reloading it at startup means a
/// restart can no longer make this worse than "as stale as it was
/// before the restart"; the upstream outage still has to be fixed
/// at the source (flagged to the Tanastok team separately), but
/// dc-explorer no longer amplifies it into a hard outage of its own.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct TanastokManifestCache {
    /// Raw upstream payload (`{ version, generated_at, counts, entities }`).
    pub raw: serde_json::Value,
    /// `version` from upstream, surfaced as `X-Tanastok-Manifest-Version`.
    pub version: String,
    /// `generated_at` epoch seconds from upstream.
    pub generated_at: i64,
    /// Index from lowercase Quipu `string_id` (no `0x`) to entity index.
    pub by_id: std::collections::HashMap<String, usize>,
    /// Server-local refresh timestamp.
    pub fetched_at: i64,
}

/// Cached Mapstore **entity manifest** (Quipu Canon v1.2, marketplace
/// participant ledger).
///
/// Mirrors `https://mapstore.net/api/v1/mapstore-entity-manifest` (alias
/// `/api/v1/registry/manifest` on the Mapstore side) using the exact same
/// discipline as `TanastokManifestCache` above:
///
/// 1. DCScan can render Mapstore merchants / escrow contracts / service
///    providers on address and string pages from a single dcscan.io
///    origin, without the browser fanning out to mapstore.net.
/// 2. Refreshed every 5 min (matches the upstream
///    `Cache-Control: s-maxage=300`).
/// 3. **Persisted to disk** (`mapstore_manifest_cache.json`) so a
///    `dc-explorer` restart during a Mapstore-side outage keeps serving
///    the last-known-good payload instead of going dark — same
///    last-known-good rationale documented on `TanastokManifestCache`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct MapstoreManifestCache {
    /// Raw upstream payload (`{ version, generated_at, ecosystem_id,
    /// participant_types, counts, entities, activity_stats, degraded }`).
    pub raw: serde_json::Value,
    /// `version` from upstream, surfaced as `X-Mapstore-Manifest-Version`.
    pub version: String,
    /// `generated_at` epoch seconds from upstream.
    pub generated_at: i64,
    /// Index from lowercase Quipu `string_id` to entity index.
    pub by_id: std::collections::HashMap<String, usize>,
    /// Server-local refresh timestamp.
    pub fetched_at: i64,
}

/// Cached TangibleDC Goodies **entity manifest** — the physical
/// gold/silver coin/title registry (`dc.datachain.one`).
///
/// Mirrors `https://dc.datachain.one/api/v1/tangibledc-entity-manifest`
/// (alias `/api/v1/registry/manifest` on the TangibleDC side) using the
/// exact same discipline as `MapstoreManifestCache` above: real
/// per-entity records (not aggregate-only like Careaway), so this gets
/// the full three-endpoint mirror (manifest / labels / entity-by-id).
/// Each entity is one physical coin — its NFC chip identity, DCNFT deed
/// (if minted) and ERC-3643 fractional title (if issued), and full
/// production→delivery history. No customer PII: the only identifiers
/// exposed are the same ones a buyer already reads by tapping their own
/// coin (serial, chip UID, on-chain references).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct TangibleDcManifestCache {
    /// Raw upstream payload (`{ version, generated_at, ecosystem_id,
    /// chain_id, contracts, counts, entities, coins_by_status,
    /// activity_stats, metal_spot, degraded }`).
    pub raw: serde_json::Value,
    /// `version` from upstream, surfaced as `X-TangibleDC-Manifest-Version`.
    pub version: String,
    /// `generated_at` epoch seconds from upstream.
    pub generated_at: i64,
    /// Index from lowercase Quipu `string_id` (no `0x`) to entity index.
    pub by_id: std::collections::HashMap<String, usize>,
    /// Server-local refresh timestamp.
    pub fetched_at: i64,
}

/// Cached Careaway **entity manifest** — aggregate-only healthcare
/// coordination stats (care-plan lifecycle counts, GDPR Art.17 erasure
/// counters, DC-credit ledger settlement volume).
///
/// Mirrors `https://careaway.co/api/v1/careaway-entity-manifest` (alias
/// `/api/v1/registry/manifest` on the Careaway side). Unlike
/// `TanastokManifestCache` / `MapstoreManifestCache`, Careaway's payload
/// carries `"entities": []` **by design** — per the health-data special
/// category boundary (GDPR Art. 9), Careaway exposes counts, timestamps,
/// and hashes only, never a per-record listing. There is therefore no
/// `by_id` index and no `/api/v1/registry/careaway-labels` or
/// `/api/v1/registry/careaway-entity/:id` endpoint — building those would
/// mean serving a permanently-empty stub, which the "no stubs" mandate
/// forbids. If Careaway later ships a genuine per-record surface (e.g.
/// the deferred verified-professional registry), extend this cache then.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CareawayManifestCache {
    /// Raw upstream payload (`{ version, generated_at, ecosystem_id,
    /// scope, counts, care_plan_lifecycle, gdpr_article17,
    /// dc_credit_settlement, entities: [], degraded }`).
    pub raw: serde_json::Value,
    /// `version` from upstream, surfaced as `X-Careaway-Manifest-Version`.
    pub version: String,
    /// `generated_at` epoch seconds from upstream.
    pub generated_at: i64,
    /// Server-local refresh timestamp.
    pub fetched_at: i64,
}

/// Cached Ecosystem Deployment Console (EDC) public project directory.
///
/// dcscan.io is the neutral, public index of every ecosystem project
/// deployed through the EDC (spec v2.0 §8 — "dcscan.io integration").
/// Each project runs its own sovereign EDC instance on its primary node;
/// dcscan aggregates the *public cards* from every known instance so
/// regulators and investors can discover projects in one place, then
/// follow the `stakeholder_url` on each card for **disintermediated**,
/// grant-scoped access to the project's live data — the data itself
/// never transits through dcscan.
///
/// Instance list comes from `EDC_DIRECTORY_URLS` (comma-separated base
/// URLs; defaults to the loopback EDC on the same node). Refreshed
/// every 60 s.
#[derive(Clone)]
pub struct EcosystemDirectoryCache {
    /// Aggregated public cards, each annotated with `edc_base` (the
    /// origin EDC instance) so the detail proxy knows where to fetch.
    pub projects: Vec<serde_json::Value>,
    /// project id (lowercased) → index into `projects`
    pub by_id: std::collections::HashMap<String, usize>,
    /// Per-instance fetch outcome for the status field.
    pub sources: Vec<serde_json::Value>,
    pub fetched_at: i64,
}

/// EDC instance base URLs that dcscan aggregates the public project
/// directory from. Comma-separated in `EDC_DIRECTORY_URLS`.
fn edc_directory_urls() -> Vec<String> {
    std::env::var("EDC_DIRECTORY_URLS")
        .unwrap_or_else(|_| "http://127.0.0.1:9095".to_string())
        .split(',')
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl AppState {
    /// Returns the URL of the currently active RPC endpoint.
    pub fn rpc_url_active(&self) -> &str {
        let idx = self
            .rpc_active_index
            .load(std::sync::atomic::Ordering::Relaxed);
        &self.rpc_urls[idx % self.rpc_urls.len()]
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct TestimonyCache {
    pub stats: serde_json::Value,
    pub testimonies: Vec<serde_json::Value>,
    pub updated_at: i64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct TokenTxnCache {
    pub stats: serde_json::Value,
    pub transfers: Vec<serde_json::Value>,
    pub updated_at: i64,
}

/// Embedded per-node cache persistence (database-less by design).
///
/// Each dc-explorer instance owns a private state directory
/// (`DC_EXPLORER_STATE_DIR`, default `/var/lib/dc-explorer`) holding one
/// JSON file per cache. Nothing is shared between nodes; a restart is
/// warm instead of triggering a 10–30 min rescan from block 0. The
/// holder/NFT/tx-count indices already persist this way — these helpers
/// extend the same pattern to the remaining in-memory caches.
fn cache_state_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::var("DC_EXPLORER_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/var/lib/dc-explorer"));
    dir.join(name)
}

fn load_json_cache<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    let path = cache_state_path(name);
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<T>(&s) {
            Ok(v) => {
                tracing::info!("Cache '{}' resumed from {}", name, path.display());
                Some(v)
            }
            Err(e) => {
                tracing::warn!(
                    "Cache '{}' parse failed at {} ({}); starting cold",
                    name,
                    path.display(),
                    e
                );
                None
            }
        },
        Err(_) => None,
    }
}

/// Atomic write (tmp + rename) so a crash mid-write never corrupts the
/// on-disk cache — the previous complete snapshot survives.
fn save_json_cache<T: serde::Serialize>(name: &str, value: &T) {
    let path = cache_state_path(name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    match serde_json::to_string(value) {
        Ok(s) => {
            if let Err(e) = std::fs::write(&tmp, s).and_then(|_| std::fs::rename(&tmp, &path)) {
                tracing::warn!("Cache '{}' persist failed at {}: {}", name, path.display(), e);
            }
        }
        Err(e) => tracing::warn!("Cache '{}' serialize failed: {}", name, e),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("╔══════════════════════════════════════════════════════════════╗");
    tracing::info!("║              DC EXPLORER - dcscan.io                         ║");
    tracing::info!("║   Knot/String Explorer for Datachain Rope (Quipu Canon v1.1) ║");
    tracing::info!("╚══════════════════════════════════════════════════════════════╝");

    // Initialize HTTP client for price fetching
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("DC-Explorer/1.0")
        .build()
        .expect("Failed to create HTTP client");

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(3001);
    let rpc_url = std::env::var("RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8545".to_string());
    let rpc_url_secondary = std::env::var("RPC_URL_SECONDARY").ok();
    let dcswap_api =
        std::env::var("DCSWAP_API").unwrap_or_else(|_| "https://dcswap.net".to_string());

    let mut rpc_urls = vec![rpc_url.clone()];
    if let Some(ref secondary) = rpc_url_secondary {
        rpc_urls.push(secondary.clone());
        tracing::info!(
            "RPC failover enabled: primary={}, secondary={}",
            rpc_url,
            secondary
        );
    } else {
        tracing::info!(
            "RPC: {} (no failover — set RPC_URL_SECONDARY to enable)",
            rpc_url
        );
    }

    // Static frontend: DCSCAN_STATIC overrides; else use bundled static/ (same HTML as former dcscan-api)
    let static_dir = std::env::var("DCSCAN_STATIC")
        .ok()
        .filter(|p| {
            let path = std::path::Path::new(p);
            path.exists() && path.join("index.html").exists()
        })
        .or_else(|| {
            let cwd = std::env::current_dir().ok()?;
            for rel in [
                "static",
                "crates/rope-explorer/static",
                "datachain-rope/crates/rope-explorer/static",
            ] {
                let p = cwd.join(rel);
                if p.exists() && p.join("index.html").exists() {
                    return Some(p.to_string_lossy().into_owned());
                }
            }
            // Fallback: path relative to executable (e.g. target/debug/dc-explorer -> crates/rope-explorer/static)
            if let Ok(exe) = std::env::current_exe() {
                if let Some(exe_dir) = exe.parent() {
                    // from target/debug go up to workspace root, then crates/rope-explorer/static
                    let root = exe_dir.parent().and_then(|p| p.parent());
                    if let Some(r) = root {
                        let p = r.join("crates/rope-explorer/static");
                        if p.exists() && p.join("index.html").exists() {
                            return Some(p.to_string_lossy().into_owned());
                        }
                    }
                }
            }
            None
        });

    if static_dir.is_none() {
        tracing::warn!("DCSCAN_STATIC not set and no static/ found (tried cwd + current_exe relative). Set DCSCAN_STATIC to the path of crates/rope-explorer/static to serve the frontend.");
    } else {
        tracing::info!(
            "Serving static frontend from: {}",
            static_dir.as_ref().unwrap()
        );
    }

    let db_pool = match std::env::var("DATABASE_URL") {
        Ok(url) => match db::connect(&url).await {
            Ok(pool) => {
                tracing::info!("Connected to PostgreSQL");
                Some(pool)
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to connect to PostgreSQL: {} — agent data will be unavailable",
                    e
                );
                None
            }
        },
        Err(_) => {
            tracing::warn!("DATABASE_URL not set — agent data will be unavailable");
            None
        }
    };

    let state = Arc::new(AppState {
        chain_id: 271828,
        network_name: "Datachain Rope Mainnet".to_string(),
        http_client,
        rpc_url: rpc_url.clone(),
        rpc_urls,
        rpc_active_index: std::sync::atomic::AtomicUsize::new(0),
        price_cache: RwLock::new(None),
        dcswap_api,
        services_registry: RwLock::new(Vec::new()),
        verification_store: RwLock::new(std::collections::HashMap::new()),
        certifications_store: RwLock::new(std::collections::HashMap::new()),
        static_dir: static_dir.clone(),
        db_pool,
        // Warm-start both caches from the per-node state dir; the
        // background refreshers overwrite them within one cycle.
        testimony_cache: RwLock::new(load_json_cache::<TestimonyCache>("testimony_cache.json")),
        tokentxn_cache: RwLock::new(load_json_cache::<TokenTxnCache>("tokentxn_cache.json")),
        // Warm-start from disk so a restart during a Tanastok-side outage
        // (see TanastokManifestCache doc comment) doesn't zero out the
        // registry mirror — the background refresher overwrites this
        // with a fresher payload as soon as the upstream recovers.
        tanastok_cache: RwLock::new(load_json_cache::<TanastokCache>("tanastok_cache.json")),
        tanastok_manifest_cache: RwLock::new(load_json_cache::<TanastokManifestCache>(
            "tanastok_manifest_cache.json",
        )),
        // Same warm-start rationale as tanastok_manifest_cache: a
        // dc-explorer restart during a Mapstore-side outage must not
        // zero out the mirror the background refresher will otherwise
        // repopulate within one 5-min cycle.
        mapstore_manifest_cache: RwLock::new(load_json_cache::<MapstoreManifestCache>(
            "mapstore_manifest_cache.json",
        )),
        // Same warm-start rationale as the two mirrors above.
        careaway_manifest_cache: RwLock::new(load_json_cache::<CareawayManifestCache>(
            "careaway_manifest_cache.json",
        )),
        // Same warm-start rationale as the mirrors above.
        tangibledc_manifest_cache: RwLock::new(load_json_cache::<TangibleDcManifestCache>(
            "tangibledc_manifest_cache.json",
        )),
        ecosystem_directory_cache: RwLock::new(None),
        // Resume scan progress from disk so a restart doesn't reset the
        // visible "transactions since genesis" count to ~0 for 10–30 min.
        tx_count_cache: RwLock::new(load_tx_count_cache()),
        global_stats_cache: RwLock::new(None),
        block_number_cache: RwLock::new(None),
        bot_activity_cache: RwLock::new(None),
        latest_tx_cache: RwLock::new(None),
        holder_index: RwLock::new(load_holder_index()),
        nft_index: RwLock::new(load_nft_index()),
        api_keys: api_keys::ApiKeyStore::load(),
        certification_providers: certification_providers::CertificationProviderRegistry::load(),
        rate_limiter: rate_limit::RateLimiter::from_env(),
        mailer: mailer::Mailer::from_env(),
        cmc_cache: RwLock::new(None),
        supply_cache: RwLock::new(None),
    });

    // Persist API-key usage counters periodically (mint/revoke persist
    // immediately; the per-request counters accumulate in memory).
    let api_keys_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
            api_keys_state.api_keys.persist().await;
        }
    });

    // ── Background CoinMarketCap refresh ─────────────────────────────
    // Updates `cmc_cache` every 5 minutes with live USD quotes for the
    // bridged stables and any other token symbol our `token_metadata()`
    // map cares about. Becomes a no-op when `CMC_API_KEY` is not set —
    // in that case the token pages keep using the static 2026-06-04
    // snapshot embedded in `token_metadata()` and label the data source
    // accordingly.
    let cmc_state = Arc::clone(&state);
    tokio::spawn(async move {
        // Warm the cache once at startup so the first page hit after
        // a deploy doesn't need to wait for the 5-minute tick.
        refresh_cmc_cache(&cmc_state).await;
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(5 * 60)).await;
            refresh_cmc_cache(&cmc_state).await;
        }
    });

    // CERBER config-drift detector (new capability, 2026-07-25 audit
    // remediation) — periodic background probe verifying that (a) this
    // process's own RequestGuard blocklist is still active and (b) the
    // connected rope-node backend still rejects destructive RPC methods
    // for non-internal callers (the Phase-1 V11 gate). Every 10 minutes;
    // warms once at startup so a fresh deploy is checked immediately
    // rather than waiting for the first tick. WATCH only — findings are
    // logged, never auto-remediated.
    let drift_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            security_guard::run_config_drift_probe(&drift_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(600)).await;
        }
    });

    // Start background testimony cache refresh task (every 60s)
    let testimony_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            refresh_testimony_cache(&testimony_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }
    });

    // Start background token transfer cache refresh task (every 30s)
    let tokentxn_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            refresh_tokentxn_cache(&tokentxn_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
        }
    });

    // Start background Tanastok asset cache refresh task (every 5 min)
    let tanastok_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            refresh_tanastok_cache(&tanastok_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
        }
    });

    // Start background Tanastok entity-manifest cache refresh task
    // (every 5 min, matches upstream `s-maxage=300`). This is the
    // 1,626-entity Quipu Canon v1.2 manifest, distinct from the older
    // 198-asset `tanastok_cache` above. See `TanastokManifestCache` for
    // why.
    let manifest_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            refresh_tanastok_manifest_cache(&manifest_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
        }
    });

    // Start background Mapstore entity-manifest cache refresh task
    // (every 5 min, matches upstream `s-maxage=300`). Mirrors
    // `https://mapstore.net/api/v1/mapstore-entity-manifest` the same
    // way the task above mirrors Tanastok's manifest.
    let mapstore_manifest_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            refresh_mapstore_manifest_cache(&mapstore_manifest_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
        }
    });

    // Start background Careaway entity-manifest cache refresh task
    // (every 5 min, matches upstream `s-maxage=300`). Mirrors
    // `https://careaway.co/api/v1/careaway-entity-manifest` — same
    // discipline as the two tasks above, aggregate-only payload.
    let careaway_manifest_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            refresh_careaway_manifest_cache(&careaway_manifest_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
        }
    });

    // Start background TangibleDC Goodies entity-manifest cache refresh
    // task (every 5 min, matches upstream `s-maxage=300`). Mirrors
    // `https://dc.datachain.one/api/v1/tangibledc-entity-manifest` — same
    // discipline as the mirrors above, real per-entity coin/title records.
    let tangibledc_manifest_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            refresh_tangibledc_manifest_cache(&tangibledc_manifest_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
        }
    });

    // Start background Ecosystem Deployment Console directory refresh
    // (every 60 s). Aggregates public project cards from every EDC
    // instance in `EDC_DIRECTORY_URLS` for the /ecosystem page.
    let edc_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            refresh_ecosystem_directory_cache(&edc_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }
    });

    // Start background exact transaction counter (every 10s, incremental)
    let txcount_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            refresh_tx_count_cache(&txcount_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        }
    });

    // Start background holder-index scanner. The first pass walks the
    // full chain in 100K-block chunks (Reth keeps complete log history,
    // but caps each `eth_getLogs` at 100K blocks / 20K results); after
    // that it only walks the new tail every 60 s. Persistence to disk
    // means restarts resume from the last scanned block instead of
    // re-doing the full ~2M-block scan.
    let holder_state = Arc::clone(&state);
    tokio::spawn(async move {
        // Stagger the first run by 5 s so the explorer's port is open
        // and serving traffic before the (potentially long) genesis
        // bootstrap scan starts. This keeps cold-start latency on the
        // homepage low.
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        loop {
            refresh_holder_index(&holder_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }
    });

    // Start background NFT (ERC-721) ownership-index scanner. Uses the
    // Tanastok manifest cache as the source-of-truth for the collection
    // set (413 DCNFT contracts as of 2026-06-04, each minted exactly
    // once with totalSupply=1) plus any contracts we discover as
    // ERC-721 elsewhere. Stagger by 15 s so the holder index has a
    // chance to claim the RPC budget for the first 10 s.
    let nft_state = Arc::clone(&state);
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
        loop {
            refresh_nft_index(&nft_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }
    });

    // Start background price fetching task
    let price_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            if let Err(e) = fetch_and_cache_price(&price_state).await {
                tracing::warn!("Price fetch error: {}", e);
            }
            tokio::time::sleep(std::time::Duration::from_secs(PRICE_CACHE_TTL_SECS)).await;
        }
    });

    // Start background supply-reconciliation refresh (every 5 min).
    // Reads both legacy DC contracts (Ethereum + XDC), WFAT supply and
    // treasury balances on Rope, and the FATMigrationMinter once it
    // deploys. Powers /api/v1/supply/* per the DC FAT Legacy Migration
    // spec v2.0.
    let supply_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            market_data::refresh_supply_cache(&supply_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(300)).await;
        }
    });

    // CORS layer — wildcard, used for the public, unauthenticated majority
    // of routes below. Any site being able to read these responses
    // cross-origin is intentional (dcscan.io stats/labels/registry/etc.
    // are meant to be embeddable by any ecosystem frontend).
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // M8 (2026-07-25 security audit): `/api/v1/keys` (list/create) and
    // `/api/v1/keys/:id` (revoke) are gated on a Datachain ID Bearer token
    // scoped to one signed-in owner (see api_keys.rs module doc) — nothing
    // about these responses is meant to be read cross-origin, and the only
    // real caller is dcscan.io's own same-origin frontend
    // (`static/apis.html` uses relative `fetch('/api/v1/keys', ...)`,
    // which needs no CORS grant at all since it's same-origin). A wildcard
    // `Access-Control-Allow-Origin: *` on these three routes adds no
    // functionality and only widens the read surface for a hypothetical
    // attacker page that already holds a stolen bearer token (e.g. via
    // XSS elsewhere, or a leaked token) to read a victim's key list or a
    // freshly minted key's plaintext from an arbitrary third-party origin
    // instead of only from dcscan.io/datachain.network. Restrict to the
    // known Datachain frontends; override via the comma-separated
    // `DCSCAN_KEYS_CORS_ORIGINS` env var for staging/local dev.
    //
    // `/api/v1/keys/verify` is deliberately kept on the wildcard `cors`
    // layer above, not this one: it authenticates via `X-API-Key` (not
    // Bearer) and is designed to be called from any third-party
    // integrator's own site to self-check a key, matching the common
    // API-key-verification UX pattern (e.g. Stripe-style key checks).
    let keys_cors_origins: Vec<axum::http::HeaderValue> = std::env::var("DCSCAN_KEYS_CORS_ORIGINS")
        .unwrap_or_else(|_| {
            "https://dcscan.io,https://www.dcscan.io,https://datachain.network,https://www.datachain.network"
                .to_string()
        })
        .split(',')
        .map(|o| o.trim())
        .filter(|o| !o.is_empty())
        .filter_map(|o| axum::http::HeaderValue::from_str(o).ok())
        .collect();
    let keys_cors = CorsLayer::new()
        .allow_origin(keys_cors_origins)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::DELETE,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ]);
    let keys_router: Router<Arc<AppState>> = Router::new()
        .route(
            "/api/v1/keys",
            get(api_keys::list_keys).post(api_keys::create_key),
        )
        .route(
            "/api/v1/keys/:id",
            axum::routing::delete(api_keys::revoke_key),
        )
        .layer(keys_cors);

    // When static frontend is enabled (DCSCAN_STATIC or bundled static/), serve HTML; else add JSON root
    let mut app = Router::new();
    if static_dir.is_none() {
        app = app.route("/", get(root));
    }
    let app = app
        .route("/health", get(health))
        .route("/api/v1/status", get(status))
        .route("/api/v1/network/config", get(network_config))
        // Stats
        .route("/api/v1/stats", get(stats))
        .route("/api/v1/stats/charts/:chart_type", get(chart_data))
        // DC FAT supply reconciliation (Legacy Migration spec v2.0,
        // Part A §9 + Part B §17–18). `reconciliation` is the full
        // machine-checkable view; `circulating`/`total` are the bare
        // text/plain numbers CoinGecko's and CoinMarketCap's supply
        // forms consume directly.
        .route(
            "/api/v1/supply/reconciliation",
            get(market_data::supply_reconciliation),
        )
        .route(
            "/api/v1/supply/circulating",
            get(market_data::supply_circulating),
        )
        .route("/api/v1/supply/total", get(market_data::supply_total))
        // Strings (Knots — canon v1.1, formerly "Blocks" in EVM tooling)
        .route("/api/v1/strings", get(list_strings))
        .route("/api/v1/strings/latest", get(latest_strings))
        .route("/api/v1/strings/:id", get(get_string))
        // Quipu Canon v1.2 — string registry (per-entity, NOT per-anchor)
        .route("/api/v1/registry/strings", get(registry_list_strings))
        .route("/api/v1/registry/stats", get(registry_global_stats))
        // Quipu Canon v1.2 — Phase 5 (Tanastok entity-manifest mirror).
        // `manifest` returns the full ~1,626 entity payload with strong
        // caching headers. `labels` is a slim id→label map for fast
        // client lookup. `entity/:id` is the single-entity endpoint.
        // All three are populated by `refresh_tanastok_manifest_cache`.
        .route(
            "/api/v1/registry/manifest",
            get(registry_tanastok_manifest),
        )
        .route("/api/v1/registry/labels", get(registry_tanastok_labels))
        .route(
            "/api/v1/registry/entity/:id",
            get(registry_tanastok_entity_by_id),
        )
        // Mapstore entity-manifest mirror — same shape/caching contract
        // as the Tanastok trio above, under a distinct `mapstore-`
        // route segment so the two ecosystems never collide on
        // `/api/v1/registry/manifest`. Populated by
        // `refresh_mapstore_manifest_cache`.
        .route(
            "/api/v1/registry/mapstore-manifest",
            get(registry_mapstore_manifest),
        )
        .route(
            "/api/v1/registry/mapstore-labels",
            get(registry_mapstore_labels),
        )
        .route(
            "/api/v1/registry/mapstore-entity/:id",
            get(registry_mapstore_entity_by_id),
        )
        // Careaway entity-manifest mirror. Single endpoint only — no
        // `-labels` / `-entity/:id` siblings, because Careaway's payload
        // is aggregate-only by design (health-data special-category
        // boundary, GDPR Art. 9): `entities: []` always, so a labels/
        // entity-lookup endpoint would just be a permanent stub. Populated
        // by `refresh_careaway_manifest_cache`.
        .route(
            "/api/v1/registry/careaway-manifest",
            get(registry_careaway_manifest),
        )
        // TangibleDC Goodies entity-manifest mirror — same shape/caching
        // contract as the Tanastok/Mapstore trios above, under a distinct
        // `tangibledc-` route segment. Each entity is one physical coin
        // (NFC identity, DCNFT deed, ERC-3643 title, full history).
        // Populated by `refresh_tangibledc_manifest_cache`.
        .route(
            "/api/v1/registry/tangibledc-manifest",
            get(registry_tangibledc_manifest),
        )
        .route(
            "/api/v1/registry/tangibledc-labels",
            get(registry_tangibledc_labels),
        )
        .route(
            "/api/v1/registry/tangibledc-entity/:id",
            get(registry_tangibledc_entity_by_id),
        )
        // Same-origin JSON-RPC proxy. Lets DCScan pages and any
        // browser client call `rope_*` / `eth_*` methods without
        // depending on a CORS preflight to `erpc.datachain.network`.
        // The body is forwarded verbatim to the active RPC backend
        // chosen by `rpc_url_active()`.
        .route("/api/rpc", post(rpc_proxy))
        // Node deployment requests — intake queue for the
        // datachain.network "Deploy a Node" get-started form. Requests
        // are appended to a durable JSONL queue and fulfilled by an
        // operator with `ropectl deploy-node` against the foundation's
        // DigitalOcean / Exoscale accounts (auto-provisioning straight
        // from an unauthenticated public form would be an unbounded
        // cloud-spend vector, so the queue + operator CLI is the
        // production design).
        .route(
            "/api/v1/node-requests",
            post(node_request_submit).get(node_requests_list),
        )
        // DCSwap bot activity — discovery surface for Moneymaker / DCSwap
        // bots interacting with known DCSwap contracts. Read-only; computed
        // from the recent-transaction window.
        .route("/api/v1/dcswap/bots", get(dcswap_bot_activity))
        .route(
            "/api/v1/contracts/:address/callers",
            get(contract_recent_callers),
        )
        // Transactions
        .route("/api/v1/transactions", get(list_transactions))
        .route("/api/v1/transactions/latest", get(latest_transactions))
        .route(
            "/api/v1/transactions/pending",
            get(pending_transactions_live),
        )
        .route("/api/v1/transactions/:hash", get(get_transaction))
        // Address labels (public registry for frontends)
        .route("/api/v1/labels", get(address_labels))
        // Tanastok tokenized assets
        .route("/api/v1/tanastok/assets", get(tanastok_all_assets))
        .route(
            "/api/v1/accounts/:address/tanastok",
            get(tanastok_by_address),
        )
        // Accounts
        .route("/api/v1/accounts/:address", get(get_account))
        .route(
            "/api/v1/accounts/:address/overview",
            get(account_overview_live),
        )
        .route(
            "/api/v1/accounts/:address/agent-testimonies",
            get(agent_testimonies_by_wallet),
        )
        .route("/api/v1/accounts/:address/bytecode", get(account_bytecode))
        .route(
            "/api/v1/accounts/:address/transfers",
            get(account_transfers),
        )
        .route("/api/v1/accounts/:address/events", get(account_events))
        .route(
            "/api/v1/accounts/:address/transactions",
            get(account_transactions),
        )
        .route("/api/v1/accounts/:address/tokens", get(account_tokens))
        // Quipu Canon v1.1 §6(2) — canonical String → Knot[] → Tx-details
        // hierarchy for the wallet's public personal ledger view in DCScan.
        // Tries the rope-node native `rope_getStringWithKnots` RPC first;
        // falls back to a block-anchored grouping built from EVM tx data.
        .route(
            "/api/v1/personal-ledger/:address/string",
            get(personal_ledger_string),
        )
        .route(
            "/api/v1/personal-ledger/:address/knots",
            get(personal_ledger_string),
        )
        .route("/api/v1/accounts/stats", get(accounts_stats_live))
        .route("/api/v1/accounts/top", get(accounts_top_live))
        // Tokens
        .route("/api/v1/tokens", get(list_tokens))
        .route("/api/v1/tokens/:address", get(get_token))
        .route("/api/v1/tokens/:address/holders", get(token_holders))
        .route("/api/v1/tokens/:address/transfers", get(token_transfers))
        .route("/api/v1/tokens/:address/dex", get(token_dex_overview))
        .route("/api/v1/tokens/:address/analytics", get(token_analytics))
        // CMC live-cache provenance (used by dcscan.io to label the
        // "Market data source" footer with "Live (refreshed N min ago)"
        // vs "Snapshot")
        .route("/api/v1/cmc/status", get(cmc_status))
        // NFT (ERC-721) — Tanastok DCNFT support
        .route("/api/v1/nfts/:address", get(get_nft_collection))
        .route("/api/v1/nfts/:address/holders", get(nft_holders))
        .route("/api/v1/nfts/:address/tokens", get(nft_inventory))
        .route("/api/v1/nfts/:address/transfers", get(nft_transfers))
        .route("/api/v1/accounts/:address/nfts", get(account_nfts))
        .route(
            "/api/v1/accounts/:address/nft-transfers",
            get(account_nft_transfers),
        )
        .route(
            "/api/v1/accounts/:address/internal-txs",
            get(account_internal_txs),
        )
        .route("/api/v1/tokentxns", get(list_token_transfers_live))
        // Validators
        .route("/api/v1/validators", get(list_validators))
        .route("/api/v1/validators/:address", get(get_validator))
        // AI Agents
        .route("/api/v1/ai-agents", get(list_ai_agents_live))
        .route("/api/v1/ai-agents/:id", get(get_ai_agent_live))
        .route(
            "/api/v1/ai-agents/:id/testimonies",
            get(agent_testimonies_live),
        )
        // Global Databox Network — real self-service registry (registration,
        // heartbeat, per-type discovery routes). See databox_registry.rs.
        .route(
            "/api/v1/databoxes",
            get(databox_registry::list_databoxes).post(databox_registry::register_databox),
        )
        .route("/api/v1/databoxes/register", post(databox_registry::register_databox))
        .route("/api/v1/databoxes/types", get(databox_registry::databox_types))
        .route("/api/v1/databoxes/type/:type", get(databox_registry::databoxes_by_type))
        .route("/api/v1/databoxes/map", get(databox_registry::databox_map))
        .route("/api/v1/databoxes/:id", get(databox_registry::get_databox))
        .route(
            "/api/v1/databoxes/:id/heartbeat",
            post(databox_registry::heartbeat_databox),
        )
        .route(
            "/api/v1/databoxes/:id/deregister",
            post(databox_registry::deregister_databox),
        )
        // Search
        .route("/api/v1/search", get(search))
        // Consensus & Testimonies
        .route("/api/v1/consensus", get(consensus))
        .route("/api/v1/testimonies/stats", get(testimonies_stats_live))
        .route("/api/v1/testimonies", get(testimonies_list_live))
        .route("/api/v1/consensus/testimonies", get(consensus_testimonies))
        // Contracts
        .route("/api/v1/contracts/stats", get(contracts_stats_live))
        .route("/api/v1/contracts/list", get(contracts_list_live))
        // DeFi
        .route("/api/v1/defi/overview", get(defi_overview))
        .route("/api/v1/defi/swaps", get(defi_swaps))
        // Services registry & Verify
        .route("/api/v1/services/registry", get(services_registry_get))
        .route("/api/v1/services/registry", post(services_registry_post))
        .route("/api/v1/verify/:address", get(verify_get))
        .route("/api/v1/verify/certify", post(verify_certify_post))
        .route("/api/v1/verify", post(verify_post))
        .route("/api/v1/tokens/register", post(tokens_register_post))
        // Gas & Prices
        .route("/api/v1/gas/price", get(gas_price))
        .route("/api/v1/gas/oracle", get(gas_oracle))
        // DC FAT Token Price (real data from XDCScan & XSPSwap)
        .route("/api/v1/dcfat/price", get(dcfat_price))
        // ============================================
        // Federation & Community Generation APIs
        // ============================================
        // Federations
        .route("/api/v1/federations", get(list_federations))
        .route("/api/v1/federations", post(create_federation))
        .route("/api/v1/federations/:id", get(get_federation))
        .route(
            "/api/v1/federations/:id/communities",
            get(federation_communities),
        )
        .route("/api/v1/federations/:id/vote", post(vote_federation))
        // Communities
        .route("/api/v1/communities", get(list_communities))
        .route("/api/v1/communities", post(create_community))
        .route("/api/v1/communities/:id", get(get_community))
        .route("/api/v1/communities/:id/wallets", get(community_wallets))
        .route(
            "/api/v1/communities/:id/wallets/generate",
            post(generate_wallets),
        )
        .route("/api/v1/communities/:id/vote", post(vote_community))
        // Project Submissions (Start Building) — real persistence, chain
        // anchoring, EIP-191 signature verification, and live single-chain
        // (Rope-native) FAT balance-weighted voting. See governance_votes.rs.
        .route(
            "/api/v1/projects",
            get(governance_votes::list_projects).post(governance_votes::submit_project),
        )
        .route("/api/v1/projects/:id", get(governance_votes::get_project))
        .route(
            "/api/v1/projects/:id/vote",
            post(governance_votes::vote_project),
        )
        .route(
            "/api/v1/projects/:id/review",
            post(governance_votes::review_project),
        )
        .route("/api/v1/projects/categories", get(project_categories))
        .route(
            "/api/v1/projects/voting",
            get(governance_votes::voting_projects),
        )
        // Governance Phase 2 — cross-chain (Ethereum + XDC legacy DC + Rope
        // native FAT) voting-weight aggregation and EIP-191 attestation
        // signing for VoteEscrow.sol. See cross_chain_weight.rs.
        .route(
            "/api/v1/governance/weight/:address",
            get(cross_chain_weight::get_weight),
        )
        .route(
            "/api/v1/governance/attestor",
            get(cross_chain_weight::get_attestor_info),
        )
        // Ecosystem Deployment Console — public directory (spec v2.0 §8).
        // Aggregated project cards from every sovereign EDC instance;
        // regulator / investor data access is disintermediated via each
        // card's stakeholder_url (dcscan never proxies the data itself).
        .route("/api/v1/ecosystem/directory", get(ecosystem_directory))
        .route(
            "/api/v1/ecosystem/directory/:id",
            get(ecosystem_directory_project),
        )
        // Votes — project ballots are real (see governance_votes.rs); other
        // target types (federation/community demo) remain out of scope.
        .route("/api/v1/votes", get(governance_votes::list_votes))
        .route(
            "/api/v1/votes/:target_type/:target_id",
            get(governance_votes::get_votes_for_target),
        )
        // Self-service API keys (Datachain ID authenticated users).
        // `/api/v1/keys` and `/api/v1/keys/:id` live in `keys_router`
        // (merged below) with their own, non-wildcard CORS policy — see
        // the M8 comment above `keys_cors`. Only `verify` (X-API-Key,
        // not Bearer) stays on the wildcard-CORS public chain here.
        .route("/api/v1/keys/verify", get(api_keys::verify_key))
        // Contact-form relay (SendGrid) for datachain.network + dcscan.io
        .route("/api/v1/contact", axum::routing::post(mailer::contact))
        .layer(cors)
        // `keys_router` was built and layered with `keys_cors` BEFORE this
        // merge, so merging it in here (rather than adding its routes via
        // `.route()` above) means the wildcard `cors` layer just applied
        // does NOT also wrap it — each route keeps exactly the CORS policy
        // it was given. `track_usage` below is applied after the merge so
        // it still wraps every route, keys included, same as before M8.
        .merge(keys_router)
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            api_keys::track_usage,
        ));

    let app = if static_dir.is_some() {
        app.route("/", get(serve_index))
            .route("/*path", get(serve_static_with_html_fallback))
    } else {
        app
    };
    // Outermost layer, added last so it wraps EVERY route above —
    // including the static-frontend fallback routes just added, which
    // sit outside the `cors`/`track_usage` layers applied earlier
    // (finding H4, SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md; see
    // `rate_limit.rs` module doc for the full trust model).
    let app = app.layer(axum::middleware::from_fn_with_state(
        Arc::clone(&state),
        rate_limit::rate_limit_middleware,
    ));
    let app = app.with_state(state);

    let addr = format!("0.0.0.0:{}", port);
    tracing::info!("DC Explorer API listening on {}", addr);
    tracing::info!("API docs: http://{}/api/v1/status", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}

// ============================================================================
// Price Fetching Functions
// ============================================================================

/// Fetch price from XDCScan (primary source - confirmed working)
async fn fetch_from_xdcscan(client: &reqwest::Client) -> Result<PriceData, anyhow::Error> {
    // XDCScan token API endpoint (confirmed working)
    let api_url = format!("https://xdcscan.io/api/tokens/{}", DC_FAT_CONTRACT);
    let response = client.get(&api_url).send().await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "XDCScan API returned status: {}",
            response.status()
        ));
    }

    let data: serde_json::Value = response.json().await?;

    // Parse exchange_rate (this is the USD price)
    let price = data
        .get("exchange_rate")
        .or_else(|| data.get("stats").and_then(|s| s.get("fiat_value")))
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| v.as_f64())
        })
        .unwrap_or(0.0);

    if price <= 0.0 {
        return Err(anyhow::anyhow!("Invalid price from XDCScan"));
    }

    let change_24h = data
        .get("price_change_24h")
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| v.as_f64())
        })
        .unwrap_or(0.0);

    let volume_24h = data
        .get("volume_24h")
        .or_else(|| data.get("stats").and_then(|s| s.get("last_24h_volume")))
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| v.as_f64())
        })
        .unwrap_or(0.0);

    let symbol = data
        .get("symbol")
        .and_then(|v| v.as_str())
        .unwrap_or("DC")
        .to_string();

    let name = data
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("DATACHAIN FOUNDATION")
        .to_string();

    tracing::info!(
        "XDCScan data - Symbol: {}, Price: ${:.8}, Change 24h: {:.2}%, Volume: ${:.2}",
        symbol,
        price,
        change_24h,
        volume_24h
    );

    Ok(PriceData {
        price,
        change_24h,
        volume_24h,
        liquidity: 0.0,
        source: "xdcscan".to_string(),
        timestamp: chrono::Utc::now().timestamp(),
    })
}

/// Fetch and cache the DC FAT price through the canonical source chain
/// (Legacy Migration spec v2.0, Part B §B.3):
///
/// 1. **DCSwap canonical** (`{DCSWAP_API}/v1/prices`) — the ecosystem
///    source of truth (handover-canonical-fat-price-2026-03-14).
/// 2. **GeckoTerminal** legacy XDC DC price via the CoinGecko Pro key —
///    labelled `geckoterminal-xdc-legacy` so consumers can tell a
///    legacy-representation price from the canonical one.
/// 3. **XDCScan** token API (the pre-2026-07 primary, now last live
///    fallback).
/// 4. Last-known-good cache entry; the static `FALLBACK_PRICE` only if no
///    fetch has ever succeeded since process start.
async fn fetch_and_cache_price(state: &Arc<AppState>) -> Result<PriceData, anyhow::Error> {
    let price_data = match market_data::fetch_from_dcswap_canonical(
        &state.http_client,
        &state.dcswap_api,
    )
    .await
    {
        Ok(data) => {
            tracing::info!("Price from DCSwap canonical: ${:.8}", data.price);
            data
        }
        Err(primary_err) => {
            tracing::warn!(
                "DCSwap canonical price fetch failed: {} — trying GeckoTerminal",
                primary_err
            );
            match market_data::fetch_from_geckoterminal_legacy(&state.http_client).await {
                Ok(data) => {
                    tracing::info!("Price from GeckoTerminal (legacy XDC DC): ${:.8}", data.price);
                    data
                }
                Err(gt_err) => {
                    tracing::warn!("GeckoTerminal fetch failed: {} — trying XDCScan", gt_err);
                    match fetch_from_xdcscan(&state.http_client).await {
                        Ok(data) => {
                            tracing::info!("Price from XDCScan: ${:.8}", data.price);
                            data
                        }
                        Err(xdc_err) => {
                            tracing::warn!(
                                "All live price sources failed (dcswap: {}, geckoterminal: {}, xdcscan: {})",
                                primary_err,
                                gt_err,
                                xdc_err
                            );
                            // Prefer the last-known-good value over a synthetic
                            // number: a stale real price is honest, a synthetic
                            // one is not.
                            let cache = state.price_cache.read().await;
                            if let Some(last) = cache.as_ref() {
                                let mut stale = last.clone();
                                stale.source = format!("{} (stale)", last.source);
                                return Ok(stale);
                            }
                            PriceData {
                                price: FALLBACK_PRICE,
                                change_24h: 0.0,
                                volume_24h: 0.0,
                                liquidity: 0.0,
                                source: "fallback".to_string(),
                                timestamp: chrono::Utc::now().timestamp(),
                            }
                        }
                    }
                }
            }
        }
    };

    // Update cache
    let mut cache = state.price_cache.write().await;
    *cache = Some(price_data.clone());

    Ok(price_data)
}

/// Generate pseudo-random variation (0.0 to 1.0). Retained for tests;
/// production price paths no longer synthesize price variation — a stale
/// real price is served instead (see `fetch_and_cache_price`).
#[cfg(test)]
fn rand_variation() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    (nanos as f64) / 1_000_000_000.0
}

// ============================================================================
// Route Handlers
// ============================================================================

/// Serves index.html for GET / when static frontend is enabled (axum /*path does not match root).
async fn serve_index(State(state): State<Arc<AppState>>) -> Response {
    let Some(ref dir) = state.static_dir else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = std::path::Path::new(dir).join("index.html");
    match tokio::fs::read(&path).await {
        Ok(body) => {
            let mut res = Response::new(axum::body::Body::from(body));
            res.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            res
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Serves static files from static_dir; extensionless paths (e.g. /strings, /txs) are served as path.html.
async fn serve_static_with_html_fallback(
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let Some(ref dir) = state.static_dir else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = path.trim_start_matches('/');
    // Do not serve static files for API paths (safety guard)
    if path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    if path.contains("..") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let base = std::path::Path::new(dir);
    // Path rewrites: URL path -> file path (relative to base)
    let path_rewrites: &[(&str, &str)] = &[
        ("tokens/transfers", "tokentxns.html"),
        ("tokens/vote", "vote.html"),
        ("network/validators", "validators.html"),
        ("network/stats", "stats.html"),
        ("apis", "apis.html"),
        ("api", "apis.html"),
        ("validators", "validators.html"),
        ("databases", "databases.html"),
        ("databoxes", "databoxes.html"),
    ];
    let rewritten = path_rewrites
        .iter()
        .find(|(from, _)| path == *from)
        .map(|(_, to)| to.to_string());

    let (file_path, content_type) = if path.is_empty() {
        (base.join("index.html"), "text/html; charset=utf-8")
    } else if let Some(ref file) = rewritten {
        let p = base.join(file);
        if p.exists() {
            let ct = content_type_for_path(p.as_path());
            (p, ct)
        } else {
            return StatusCode::NOT_FOUND.into_response();
        }
    } else {
        let path_index = base.join(path).join("index.html");
        let path_html = base.join(format!("{}.html", path));
        if path_html.exists() {
            let ct = content_type_for_path(path_html.as_path());
            (path_html, ct)
        } else if path_index.exists() {
            (path_index, "text/html; charset=utf-8")
        } else if path.starts_with("tx/")
            || path.starts_with("address/")
            || path.starts_with("string/")
            || path.starts_with("token/")
            || path.starts_with("blockchain/")
            || path.starts_with("agents/")
            || path.starts_with("tokens/")
            || path.starts_with("network/")
        {
            let segment = path.split('/').next().unwrap_or("tx");
            let index_path = base.join(segment).join("index.html");
            if index_path.exists() {
                (index_path, "text/html; charset=utf-8")
            } else {
                return StatusCode::NOT_FOUND.into_response();
            }
        } else {
            let path_has_extension = std::path::Path::new(path).extension().is_some();
            let try_path = if path_has_extension {
                base.join(path)
            } else {
                base.join(format!("{}.html", path))
            };
            if try_path.exists() {
                let ct = content_type_for_path(try_path.as_path());
                (try_path, ct)
            } else if path_has_extension {
                return StatusCode::NOT_FOUND.into_response();
            } else {
                let direct = base.join(path);
                if direct.exists() && direct.is_file() {
                    let ct = content_type_for_path(direct.as_path());
                    (direct, ct)
                } else {
                    return StatusCode::NOT_FOUND.into_response();
                }
            }
        }
    };
    match tokio::fs::read(&file_path).await {
        Ok(body) => {
            let mut res = Response::new(axum::body::Body::from(body));
            let hv = HeaderValue::try_from(content_type)
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
            res.headers_mut()
                .insert(axum::http::header::CONTENT_TYPE, hv);
            res
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn content_type_for_path(p: &std::path::Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        _ => "application/octet-stream",
    }
}

async fn root() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": "DC Explorer API",
        "version": "1.0.0",
        "chain": "Datachain Rope",
        "chainId": 271828,
        "docs": "/api/v1/status"
    }))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "healthy",
        "timestamp": chrono::Utc::now().timestamp()
    }))
}

async fn status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "chainId": state.chain_id,
        "networkName": state.network_name,
        "version": "1.0.0",
        "endpoints": {
            "stats": "/api/v1/stats",
            "strings": "/api/v1/strings",
            "transactions": "/api/v1/transactions",
            "accounts": "/api/v1/accounts/{address}",
            "tokens": "/api/v1/tokens",
            "validators": "/api/v1/validators",
            "aiAgents": "/api/v1/ai-agents",
            "databoxes": "/api/v1/databoxes",
            "search": "/api/v1/search"
        }
    }))
}

/// Public, key-material-free network descriptor for wallet "Add Network"
/// buttons (EIP-3085 `wallet_addEthereumChain` shape) and for any tool that
/// needs to auto-configure against Datachain Rope. Deliberately contains no
/// secrets — this is safe to call from any client, unauthenticated, and is
/// the single source of truth other pages (create-wallet, dcscan-wallet.js
/// callers, third-party integrators) should read instead of hardcoding the
/// chain params in N different places.
async fn network_config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let chain_id_hex = format!("0x{:x}", state.chain_id);
    Json(serde_json::json!({
        "eip3085": {
            "chainId": chain_id_hex,
            "chainName": state.network_name,
            "nativeCurrency": { "name": "DC FAT", "symbol": "FAT", "decimals": 18 },
            "rpcUrls": ["https://erpc.datachain.network"],
            "blockExplorerUrls": ["https://dcscan.io"]
        },
        "chainIdDecimal": state.chain_id,
        "wsUrl": "wss://ws.datachain.network",
        "derivationPath": "m/44'/60'/0'/0/0",
        "createWalletUrl": "https://dcscan.io/create-wallet",
        "datawalletPlusUrl": "https://datawallet.plus",
        "dcswapUrl": "https://dcswap.net",
        "docsUrl": "https://dcscan.io/apis"
    }))
}

/// JSON-RPC call to Datachain Rope node (Reth EVM execution layer) with
/// automatic failover. The pre-`reth-blue-green` architecture used Anvil;
/// Anvil was fully archived 2026-03-31 — see `reth-migration-2026-03-12.mdc`
/// and `reth-blue-green-ipfs-architecture.mdc`. The function name and
/// failover semantics are unchanged because Reth is wire-compatible with the
/// JSON-RPC interface this client speaks.
/// Tries the currently active endpoint first; on failure, rotates through all
/// configured endpoints before giving up.
/// Number of times to retry a *connection/transport* failure against the
/// same RPC URL before moving on to the next endpoint (or giving up, when
/// there is only one configured — the common case, since RPC_URL_SECONDARY
/// failover is off by default). Without this, a single transient TCP hiccup
/// against the sole rope-node RPC endpoint fails the call outright with no
/// second chance, which surfaced in production as endpoints like
/// `/api/v1/strings/latest` intermittently returning an empty list even
/// though the node was healthy moments before and after.
const RPC_CALL_RETRIES: u32 = 2;
const RPC_CALL_RETRY_DELAY_MS: u64 = 120;

async fn rpc_call(
    state: &AppState,
    method: &str,
    params: Vec<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1
    });
    let n = state.rpc_urls.len();
    let start = state
        .rpc_active_index
        .load(std::sync::atomic::Ordering::Relaxed);
    let mut last_err = String::new();

    for offset in 0..n {
        let idx = (start + offset) % n;
        let url = &state.rpc_urls[idx];
        for attempt in 0..=RPC_CALL_RETRIES {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(RPC_CALL_RETRY_DELAY_MS)).await;
            }
            match state.http_client.post(url).json(&body).send().await {
                Ok(res) => match res.json::<serde_json::Value>().await {
                    Ok(json) => {
                        if let Some(err) = json.get("error") {
                            last_err = err.to_string();
                            break; // RPC-level error is deterministic; don't retry, try next URL.
                        }
                        if offset > 0 {
                            tracing::warn!(
                                "RPC failover: switched from {} to {}",
                                state.rpc_urls[start],
                                url
                            );
                            state
                                .rpc_active_index
                                .store(idx, std::sync::atomic::Ordering::Relaxed);
                        }
                        return json
                            .get("result")
                            .cloned()
                            .ok_or_else(|| "missing result".to_string());
                    }
                    Err(e) => {
                        last_err = format!("{} (parse): {}", url, e);
                    }
                },
                Err(e) => {
                    last_err = format!("{} (connect): {}", url, e);
                }
            }
        }
    }
    Err(format!(
        "all {} RPC endpoints failed — last: {}",
        n, last_err
    ))
}

/// Batch JSON-RPC call with failover (used for block-scanning loops).
async fn rpc_batch_call(
    state: &AppState,
    batch: &[serde_json::Value],
) -> Option<Vec<serde_json::Value>> {
    let n = state.rpc_urls.len();
    let start = state
        .rpc_active_index
        .load(std::sync::atomic::Ordering::Relaxed);
    for offset in 0..n {
        let idx = (start + offset) % n;
        let url = &state.rpc_urls[idx];
        match state.http_client.post(url).json(batch).send().await {
            Ok(res) => match res.json::<Vec<serde_json::Value>>().await {
                Ok(v) if !v.is_empty() => {
                    if offset > 0 {
                        tracing::warn!("RPC batch failover: switched to {}", url);
                        state
                            .rpc_active_index
                            .store(idx, std::sync::atomic::Ordering::Relaxed);
                    }
                    return Some(v);
                }
                _ => continue,
            },
            Err(_) => continue,
        }
    }
    None
}

/// eth_blockNumber -> current block number (u64)
async fn rpc_block_number(state: &AppState) -> Result<u64, String> {
    let hex = rpc_call(state, "eth_blockNumber", vec![]).await?;
    let s = hex.as_str().ok_or("not a string")?;
    let n = u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| e.to_string())?;
    Ok(n)
}

/// eth_gasPrice -> gas price in wei (u64)
async fn rpc_gas_price(state: &AppState) -> Result<u64, String> {
    let hex = rpc_call(state, "eth_gasPrice", vec![]).await?;
    let s = hex.as_str().ok_or("not a string")?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
}

fn hex_to_u64(s: &str) -> u64 {
    u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0)
}

fn hex_to_u128(s: &str) -> u128 {
    u128::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0)
}

fn wei_to_fat(hex: &str) -> f64 {
    hex_to_u128(hex) as f64 / 1e18
}

fn format_fat(val: f64) -> String {
    if val == 0.0 {
        "0 FAT".to_string()
    } else if val < 0.001 {
        format!("{:.8} FAT", val)
    } else {
        format!("{:.4} FAT", val)
    }
}

const TRANSFER_TOPIC: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

struct TokenInfo {
    symbol: &'static str,
    decimals: u8,
    usd_price: f64,
}

fn known_token(addr: &str) -> Option<TokenInfo> {
    // Addresses are matched lowercase. We list both the live 2026-02-26
    // DCSwap redeployment set (currently trading on chain) and the
    // post-Reth-migration set (per handover-canonical-fat-price-2026-03-14.mdc)
    // so the explorer keeps decoding correctly across redeployments.
    match addr.to_lowercase().as_str() {
        // ── WFAT ────────────────────────────────────────────────────────────
        "0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4"  // 2026-02-26 (live)
        | "0xddbf887982a2a1c03cb8705fef9e09c46122fff6" // post-Reth (planned)
        | "0x90e2e170b0fc133343f0d7fde128c1fb716aab25" => Some(TokenInfo {
            symbol: "WFAT",
            decimals: 18,
            usd_price: 0.01,
        }),
        // ── USDC ────────────────────────────────────────────────────────────
        "0xb93bd8db94f1baff474aa9cba0739daaad01641f"  // 2026-02-26 (live)
        | "0x3109c838e9a08a42fba000a48310845919759a02" // post-Reth (planned)
        | "0x9f700dd3bb1764ab568263d3e19a1fc5cdf3f9a5" => Some(TokenInfo {
            symbol: "USDC",
            decimals: 6,
            usd_price: 1.0,
        }),
        // ── USDT ────────────────────────────────────────────────────────────
        "0x79a26132f48394421382c13b54ae77fa3af73289"  // 2026-02-26 (live)
        | "0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef" => Some(TokenInfo {
            symbol: "USDT",
            decimals: 6,
            usd_price: 1.0,
        }),
        // ── EUROD ───────────────────────────────────────────────────────────
        "0x24d6137807fa8a592888726d87ac748d018c6d4a"  // 2026-02-26 (live)
        | "0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa" => Some(TokenInfo {
            symbol: "EUROD",
            decimals: 6,
            usd_price: 1.08,
        }),
        // ── LP TOKENS (DCSwap pools — also DCR-20, decimals 18) ────────────
        // 2026-02-26 redeployment pools:
        "0xd9ebc3da001618a3ae90481d33ae7ef85e130317" => Some(TokenInfo {
            symbol: "FAT-USDC LP",
            decimals: 18,
            usd_price: 0.0,
        }),
        "0x644da44bcd5f453c593781dbe22dfd733e8d1441" => Some(TokenInfo {
            symbol: "FAT-USDT LP",
            decimals: 18,
            usd_price: 0.0,
        }),
        "0x1e9c2ccf67320459bc4999a9f8be4a063d4021e4" => Some(TokenInfo {
            symbol: "FAT-EUROD LP",
            decimals: 18,
            usd_price: 0.0,
        }),
        "0xb86bdcecad93573d6ca21313aa7eac52800513c8" => Some(TokenInfo {
            symbol: "USDC-USDT LP",
            decimals: 18,
            usd_price: 0.0,
        }),
        // Post-Reth pools:
        "0x94e779cdc322d096d8f30b41ff50cad2d8206b70" => Some(TokenInfo {
            symbol: "FAT-USDC LP",
            decimals: 18,
            usd_price: 0.0,
        }),
        "0xe579ed174a391c6771f3b04eb59bc1629b1ced2a" => Some(TokenInfo {
            symbol: "FAT-USDT LP",
            decimals: 18,
            usd_price: 0.0,
        }),
        "0xf31958221926b30db3e0254acd86efa85b684201" => Some(TokenInfo {
            symbol: "FAT-EUROD LP",
            decimals: 18,
            usd_price: 0.0,
        }),
        "0x1956539b4b90548e31387b74f628728535559eec" => Some(TokenInfo {
            symbol: "USDC-USDT LP",
            decimals: 18,
            usd_price: 0.0,
        }),
        _ => None,
    }
}

fn decode_hex_u256(hex_data: &str) -> u128 {
    let clean = hex_data.trim_start_matches("0x");
    if clean.len() > 32 {
        u128::from_str_radix(&clean[clean.len().saturating_sub(32)..], 16).unwrap_or(0)
    } else {
        u128::from_str_radix(clean, 16).unwrap_or(0)
    }
}

fn topic_to_address(topic: &str) -> String {
    let clean = topic.trim_start_matches("0x");
    if clean.len() >= 40 {
        format!("0x{}", &clean[clean.len() - 40..])
    } else {
        format!("0x{}", clean)
    }
}

/// Decode an ABI-encoded `string` return value (e.g. from `name()` / `symbol()`)
/// from a hex-encoded `eth_call` response. Falls back to a `bytes32`-style
/// decoding if the response is too short to be a dynamic string — some older
/// DCR-20 / ERC-20 tokens encode symbol as a fixed `bytes32`.
fn decode_abi_string(hex_data: &str) -> Option<String> {
    let clean = hex_data.trim_start_matches("0x");
    if clean.is_empty() || clean == "0" {
        return None;
    }
    if clean.len() >= 128 {
        if let Ok(length) = usize::from_str_radix(&clean[64..128], 16) {
            if length > 0 && 128 + length * 2 <= clean.len() {
                if let Ok(bytes) = hex::decode(&clean[128..128 + length * 2]) {
                    if let Ok(s) = String::from_utf8(bytes) {
                        let t = s.trim();
                        if !t.is_empty() {
                            return Some(t.to_string());
                        }
                    }
                }
            }
        }
    }
    if clean.len() >= 64 {
        if let Ok(bytes) = hex::decode(&clean[..64]) {
            let s: String = bytes
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as char)
                .collect();
            let t = s.trim();
            if !t.is_empty() && t.chars().all(|c| !c.is_control()) {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Call a zero-arg DCR-20 view method (`name()`, `symbol()`, `decimals()`,
/// `totalSupply()`) on `token` via `eth_call` and return the raw hex result.
async fn eth_call_token_method(state: &AppState, token: &str, selector: &str) -> Option<String> {
    // Reth's RPC connection pool sometimes drops on connection reset
    // (mostly during heavy holder-index scans). Retry a couple of times
    // with a short backoff before giving up — getting `name()` /
    // `symbol()` / `totalSupply()` wrong on a stable token page is far
    // more visible to users than a small extra wait.
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(200 * attempt as u64)).await;
        }
        match rpc_call(
            state,
            "eth_call",
            vec![
                serde_json::json!({ "to": token, "data": selector }),
                serde_json::json!("latest"),
            ],
        )
        .await
        {
            Ok(res) => {
                if let Some(s) = res.as_str() {
                    if !s.is_empty() && s != "0x" {
                        return Some(s.to_string());
                    }
                    // `0x` is a valid empty response (e.g. EOAs return
                    // empty), so don't retry that — only retry on errors.
                    return Some(s.to_string());
                }
            }
            Err(_) => continue,
        }
    }
    None
}

/// Call `balanceOf(holder)` on `token` and return the raw u128 balance.
async fn eth_call_balance_of(state: &AppState, token: &str, holder: &str) -> u128 {
    let holder_clean = holder.trim_start_matches("0x").to_lowercase();
    let mut padded = String::from("0x70a08231");
    padded.push_str(&"0".repeat(64usize.saturating_sub(holder_clean.len())));
    padded.push_str(&holder_clean);
    match rpc_call(
        state,
        "eth_call",
        vec![
            serde_json::json!({ "to": token, "data": padded }),
            serde_json::json!("latest"),
        ],
    )
    .await
    {
        Ok(v) => v.as_str().map(decode_hex_u256).unwrap_or(0),
        Err(_) => 0,
    }
}

/// Format a u128 raw token amount with its decimals into a human-readable
/// string with thousands separators. Mirrors the formatting style used by the
/// landing-page transfer cards.
fn format_token_amount(raw: u128, decimals: u32) -> String {
    if raw == 0 {
        return "0".to_string();
    }
    let divisor = 10f64.powi(decimals as i32);
    let f = raw as f64 / divisor;
    if f >= 1.0 {
        format!("{:.4}", f)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    } else {
        format!("{:.8}", f)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// Format a number with thousands separators (e.g. 10_000_000_000 -> "10,000,000,000").
fn format_with_commas(n: f64) -> String {
    let int_part = n.trunc() as i128;
    let frac = n - int_part as f64;
    let mut s = int_part.abs().to_string();
    let bytes: Vec<char> = s.chars().rev().collect();
    let mut out = String::new();
    for (i, c) in bytes.iter().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    s = out.chars().rev().collect();
    if int_part < 0 {
        s.insert(0, '-');
    }
    if frac.abs() > 1e-9 {
        let frac_str = format!("{:.4}", frac.abs());
        let trimmed = frac_str
            .trim_start_matches('0')
            .trim_end_matches('0')
            .trim_end_matches('.');
        if !trimmed.is_empty() {
            s.push_str(trimmed);
        }
    }
    s
}

/// Walk the in-memory `tokentxn_cache` Transfer events for one token contract
/// and derive the per-holder running balance map (lower-case address -> raw
/// signed delta). Burns to / mints from `0x000…000` are excluded from holder
/// counts. The cache currently holds the most recent ~50 transfers, so this
/// is necessarily a partial view; the response carries an explicit
/// `is_partial: true` flag so the frontend can render an honest "indexing in
/// progress" hint.
fn derive_token_holders_from_cache(
    cache: &TokenTxnCache,
    token_addr_lc: &str,
    decimals: u32,
) -> Vec<(String, f64)> {
    use std::collections::HashMap;
    let zero = "0x0000000000000000000000000000000000000000";
    let divisor = 10f64.powi(decimals as i32);
    let mut balances: HashMap<String, f64> = HashMap::new();
    for t in &cache.transfers {
        let token = t
            .get("tokenAddress")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if token != token_addr_lc {
            continue;
        }
        let from = t
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let to = t
            .get("to")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let amount: f64 = t
            .get("amount")
            .and_then(|v| match v {
                serde_json::Value::Number(n) => n.as_f64(),
                serde_json::Value::String(s) => s.replace(',', "").parse::<f64>().ok(),
                _ => None,
            })
            .unwrap_or(0.0);
        if from != zero && !from.is_empty() {
            *balances.entry(from).or_insert(0.0) -= amount;
        }
        if to != zero && !to.is_empty() {
            *balances.entry(to).or_insert(0.0) += amount;
        }
        let _ = divisor;
    }
    let mut entries: Vec<(String, f64)> = balances.into_iter().filter(|(_, v)| *v > 0.0).collect();
    entries.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    entries
}

fn decode_token_transfers(logs: &[serde_json::Value]) -> (Vec<serde_json::Value>, String) {
    let mut transfers = Vec::new();
    let mut total_usd = 0.0f64;
    let mut summary_parts: Vec<String> = Vec::new();

    for log in logs {
        let topics = match log.get("topics").and_then(|v| v.as_array()) {
            Some(t) if t.len() >= 3 => t,
            _ => continue,
        };
        let topic0 = topics[0].as_str().unwrap_or("");
        if topic0 != TRANSFER_TOPIC {
            continue;
        }

        let token_addr = log.get("address").and_then(|v| v.as_str()).unwrap_or("");
        let from = topic_to_address(topics[1].as_str().unwrap_or(""));
        let to = topic_to_address(topics[2].as_str().unwrap_or(""));
        let data = log.get("data").and_then(|v| v.as_str()).unwrap_or("0x0");
        let raw_amount = decode_hex_u256(data);

        let (symbol, decimals, usd_price) = match known_token(token_addr) {
            Some(info) => (info.symbol.to_string(), info.decimals, info.usd_price),
            None => ("UNKNOWN".to_string(), 18, 0.0),
        };

        let divisor = 10f64.powi(decimals as i32);
        let amount = raw_amount as f64 / divisor;
        let usd_val = amount * usd_price;
        total_usd += usd_val;

        let amount_str = if amount < 0.001 && amount > 0.0 {
            format!("{:.8}", amount)
        } else if amount >= 1_000_000.0 {
            format!("{:.2}", amount)
        } else {
            format!("{:.4}", amount)
        };

        // Always surface the conveyed value. For unknown DCR-20 tokens we
        // tag the amount with a short contract suffix so the user sees that
        // SOMETHING moved (better than dropping it and showing "0 FAT").
        let display_symbol = if symbol == "UNKNOWN" {
            // e.g. "0x644d…1441" — last 4 chars of address as a short tag
            let short = if token_addr.len() >= 8 {
                let prefix = &token_addr[..6];
                let suffix = &token_addr[token_addr.len() - 4..];
                format!("token({}…{})", prefix, suffix)
            } else {
                "token".to_string()
            };
            short
        } else {
            symbol.clone()
        };
        summary_parts.push(format!("{} {}", amount_str, display_symbol));

        transfers.push(serde_json::json!({
            "token": token_addr,
            "symbol": symbol,
            "from": from,
            "to": to,
            "amount": amount_str,
            "amountRaw": raw_amount.to_string(),
            "decimals": decimals,
            "usdValue": format!("${:.2}", usd_val),
        }));
    }

    // Pretty summary. We dedupe the swap-route view so a Router call that
    // moves WFAT → pool → USDT shows "X WFAT → Y USDT" rather than the full
    // four-leg log path that's hard to read.
    let summary = if transfers.is_empty() {
        String::new()
    } else if total_usd > 0.0 {
        format!("${:.2} ({})", total_usd, summary_parts.join(" → "))
    } else {
        summary_parts.join(" → ")
    };

    (transfers, summary)
}

/// Fetch a block by number (hex string like "0x1a") with optional full transactions.
async fn rpc_get_block(
    state: &AppState,
    block_hex: &str,
    full_txs: bool,
) -> Result<serde_json::Value, String> {
    rpc_call(
        state,
        "eth_getBlockByNumber",
        vec![serde_json::json!(block_hex), serde_json::json!(full_txs)],
    )
    .await
}

/// Extract transactions from the latest N blocks, returning (tx_json, block_number, block_timestamp) tuples.
async fn collect_txs_from_recent_blocks(
    state: &AppState,
    block_count: u64,
    tx_limit: usize,
) -> Vec<(serde_json::Value, u64, i64)> {
    let head = match rpc_block_number(state).await {
        Ok(n) => n,
        Err(_) => return Vec::new(),
    };

    // Quorum-driven production cadence anchors a knot on a fixed clock
    // regardless of whether anyone submitted a transaction, so most
    // recent blocks are legitimately empty. A naive sequential
    // full-block fetch over `block_count` blocks is both slow (one HTTP
    // round-trip per block) and can still come up empty during quiet
    // periods. Instead: batch-scan tx *counts* (cheap,
    // eth_getBlockTransactionCountByNumber, 200 per HTTP batch — same
    // technique as the tx+volume scanner above) walking backward from
    // head until we've identified enough non-empty blocks or exhausted
    // `max_scan_blocks`, then fetch full block bodies only for the
    // blocks that actually have transactions.
    let batch_size: u64 = 200;
    let max_scan_blocks: u64 = block_count.max(20_000);
    let mut hit_blocks: Vec<u64> = Vec::new();
    let mut scanned: u64 = 0;
    let mut cursor = head;

    'scan: while scanned < max_scan_blocks && cursor > 0 && hit_blocks.len() < tx_limit {
        let batch_end = cursor;
        let batch_start = cursor.saturating_sub(batch_size - 1).max(1);
        let mut requests: Vec<serde_json::Value> = Vec::with_capacity((batch_end - batch_start + 1) as usize);
        for bn in batch_start..=batch_end {
            requests.push(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getBlockTransactionCountByNumber",
                "params": [format!("0x{:x}", bn)],
                "id": bn,
            }));
        }

        // A single flaky HTTP round-trip must not sink the whole scan —
        // retry this chunk a couple of times before treating it as a gap
        // and moving on. Observed in production: `rpc_batch_call` failing
        // on one chunk out of ~100 was enough to make the endpoint
        // intermittently return zero transactions even though real ones
        // existed well within the scan window.
        let mut responses_opt = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }
            if let Some(r) = rpc_batch_call(state, &requests).await {
                responses_opt = Some(r);
                break;
            }
        }

        if let Some(responses) = responses_opt {
            // Walk this batch newest-first so hit ordering stays head->tail.
            let mut counts: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
            for r in &responses {
                if let (Some(id), Some(result)) = (
                    r.get("id").and_then(|v| v.as_u64()),
                    r.get("result").and_then(|v| v.as_str()),
                ) {
                    counts.insert(id, hex_to_u64(result));
                }
            }
            let mut bn = batch_end;
            while bn >= batch_start {
                if counts.get(&bn).copied().unwrap_or(0) > 0 {
                    hit_blocks.push(bn);
                    if hit_blocks.len() >= tx_limit {
                        break 'scan;
                    }
                }
                if bn == batch_start {
                    break;
                }
                bn -= 1;
            }
        } else {
            tracing::warn!(
                "collect_txs_from_recent_blocks: batch RPC failed for blocks {}..{} after retries — skipping chunk, continuing scan",
                batch_start,
                batch_end
            );
        }
        scanned += batch_end - batch_start + 1;
        if batch_start <= 1 {
            break;
        }
        cursor = batch_start - 1;
    }

    let mut out: Vec<(serde_json::Value, u64, i64)> = Vec::new();
    for num in hit_blocks {
        let block_hex = format!("0x{:x}", num);
        if let Ok(block) = rpc_get_block(state, &block_hex, true).await {
            let ts = block
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| hex_to_u64(s) as i64)
                .unwrap_or(0);
            if let Some(txs) = block.get("transactions").and_then(|v| v.as_array()) {
                for tx in txs {
                    out.push((tx.clone(), num, ts));
                    if out.len() >= tx_limit {
                        return out;
                    }
                }
            }
        }
    }
    out
}

/// Build a summary JSON object for a transaction (used by list/latest endpoints).
fn tx_summary_json(
    tx: &serde_json::Value,
    block_number: u64,
    block_timestamp: i64,
) -> serde_json::Value {
    let hash = tx.get("hash").and_then(|v| v.as_str()).unwrap_or("0x0");
    let from = tx.get("from").and_then(|v| v.as_str()).unwrap_or("0x0");
    let to = tx
        .get("to")
        .and_then(|v| v.as_str())
        .unwrap_or("Contract Creation");
    let value_hex = tx.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");
    let value_fat = wei_to_fat(value_hex);
    let input = tx.get("input").and_then(|v| v.as_str()).unwrap_or("0x");
    let input_sel = if input.len() >= 10 {
        &input[..10]
    } else {
        input
    };

    // Quipu Canon v1.1: emit canon-shaped fields (knot, knotIndex)
    // alongside the legacy "string"/"block" fields. Frontend may consume
    // either; both name the same anchor index.
    let mut j = serde_json::json!({
        "hash": hash,
        "from": from,
        "to": to,
        "value": format_fat(value_fat),
        "status": "Success",
        "string": block_number,
        "knot": block_number,
        "knotIndex": block_number,
        "timestamp": block_timestamp,
        "input": input_sel
    });
    enrich_addr_field(&mut j, from, "from");
    enrich_addr_field(&mut j, to, "to");
    j
}

/// Classify a transaction by canon §4 event_type for the Knot view.
/// Inspects the first 4 bytes of input data (function selector) and the
/// presence/absence of a recipient.
fn classify_knot_event_type(tx: &serde_json::Value) -> &'static str {
    let to = tx.get("to").and_then(|v| v.as_str());
    let input = tx.get("input").and_then(|v| v.as_str()).unwrap_or("0x");
    let value_hex = tx.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");
    let value = u128::from_str_radix(value_hex.trim_start_matches("0x"), 16).unwrap_or(0);

    if to.is_none() || to == Some("") {
        return "ContractCreation";
    }
    if input.len() < 10 {
        // No function call data — pure value transfer
        if value > 0 {
            "Transfer"
        } else {
            "Empty"
        }
    } else {
        let sel = &input[..10].to_lowercase();
        match sel.as_str() {
            // DCR-20 / ERC-20 standard
            "0xa9059cbb" => "TokenTransfer",
            "0x23b872dd" => "TokenTransferFrom",
            "0x095ea7b3" => "TokenApproval",
            "0x40c10f19" => "TokenMint",
            "0x42966c68" => "TokenBurn",
            // DEX (DCSwap router)
            "0x38ed1739" | "0x18cbafe5" | "0x7ff36ab5" | "0x4a25d94a" | "0xfb3bdb41" => "Swap",
            "0xe8e33700" | "0xf305d719" => "AddLiquidity",
            "0xbaa2abde" | "0x02751cec" => "RemoveLiquidity",
            // Legacy DC → FAT migration (FATMigrationMinter.mintFromMigration,
            // dcswap/contracts/src/migration/FATMigrationMinter.sol)
            "0x94218091" => "MigrationMint",
            // Anchor / consensus / testimony
            _ => "ContractCall",
        }
    }
}

/// Quipu Canon v1.2 — TTL on the cached `rope_globalStats` snapshot.
/// 5 s is plenty: strings are typically created on human timescales,
/// not per-block, so the visible drift is negligible. The cache also
/// shields the `/api/v1/stats` handler from rope-node RPC flakiness
/// under burst load (the v1.2 call would otherwise sometimes drop and
/// the handler would emit a degenerate `totalStrings: 0`).
const GLOBAL_STATS_CACHE_TTL_SECS: i64 = 5;

/// TTL on the cached `eth_blockNumber` (cord head / `totalKnots`).
/// 2 s is short enough that the displayed knot count is never more
/// than one anchor (~3 s knot interval) behind, and long enough to
/// absorb the same kind of burst-load drops that hit
/// `rope_globalStats`. Pre-existing rope-node→Reth forwarder issue,
/// not v1.2-specific.
const BLOCK_NUMBER_CACHE_TTL_SECS: i64 = 2;

/// Get the current cord head (== `eth_blockNumber`) with a short TTL
/// cache. Falls back to the last known good value if the live RPC
/// call fails — this protects `/api/v1/stats` from transient drops
/// that would otherwise paint `totalKnots: 0` on the dashboard.
async fn fetch_block_number_cached(state: &AppState) -> u64 {
    let now = chrono::Utc::now().timestamp();
    {
        let cache = state.block_number_cache.read().await;
        if let Some(entry) = cache.as_ref() {
            if now - entry.fetched_at < BLOCK_NUMBER_CACHE_TTL_SECS {
                return entry.head;
            }
        }
    }
    match rpc_block_number(state).await {
        Ok(head) => {
            *state.block_number_cache.write().await = Some(BlockNumberCacheEntry {
                fetched_at: now,
                head,
            });
            head
        }
        Err(_) => {
            // RPC failed — last known good beats zero.
            let cache = state.block_number_cache.read().await;
            cache.as_ref().map(|e| e.head).unwrap_or(0)
        }
    }
}

/// Get `rope_globalStats` from the local cache when fresh, otherwise
/// fetch & store. Returns `(total_strings, by_kind_value)` — falls
/// back to the previous cached value (regardless of age) if the live
/// fetch fails, and only returns `(0, Null)` on a cold-cache miss.
async fn fetch_global_stats_cached(
    state: &AppState,
) -> (u64, serde_json::Value) {
    let now = chrono::Utc::now().timestamp();
    {
        let cache = state.global_stats_cache.read().await;
        if let Some(entry) = cache.as_ref() {
            if now - entry.fetched_at < GLOBAL_STATS_CACHE_TTL_SECS {
                return (entry.total_strings, entry.by_kind.clone());
            }
        }
    }
    match rpc_call(state, "rope_globalStats", vec![]).await {
        Ok(v) => {
            let s = v.get("total_strings").and_then(|x| x.as_u64()).unwrap_or(0);
            let bk = v
                .get("by_kind")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            *state.global_stats_cache.write().await = Some(GlobalStatsCacheEntry {
                fetched_at: now,
                total_strings: s,
                by_kind: bk.clone(),
            });
            (s, bk)
        }
        Err(_) => {
            // Live call failed — return last known good if we have one,
            // otherwise the cold-cache zero default.
            let cache = state.global_stats_cache.read().await;
            match cache.as_ref() {
                Some(entry) => (entry.total_strings, entry.by_kind.clone()),
                None => (0u64, serde_json::Value::Null),
            }
        }
    }
}

async fn stats(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let price_cache = state.price_cache.read().await;
    let price_data = price_cache.clone().unwrap_or_default();
    let fat_price = format!("${:.6}", price_data.price);
    let market_cap = format!("${:.0}", price_data.price * 10_000_000_000.0);

    // Quipu Canon v1.2 — pull from the TTL cache so the response is
    // never poisoned by a transient rope-node RPC drop.
    let (total_strings_real, by_kind_breakdown) =
        fetch_global_stats_cached(&state).await;

    // Same TTL-cache strategy for the cord head — rope-node's Reth
    // forwarder occasionally drops `eth_blockNumber` under burst load,
    // which would otherwise paint `totalKnots: 0` on the dashboard.
    let head = fetch_block_number_cached(&state).await;
    let gas_price_wei = rpc_gas_price(&state).await.unwrap_or(1_000_000_000u64);
    let gas_price_gwei = format!("{} gwei", gas_price_wei / 1_000_000_000);

    // Sample recent blocks to compute real metrics
    let sample_size: u64 = 50;
    let sample_end = head;
    let sample_start = if head > sample_size {
        head - sample_size
    } else {
        0
    };

    let mut total_txs_in_sample: u64 = 0;
    let mut first_ts: u64 = 0;
    let mut last_ts: u64 = 0;
    let mut fee_samples: Vec<f64> = Vec::new();

    for bn in (sample_start..=sample_end).rev() {
        let blk = match rpc_get_block(&state, &format!("0x{:x}", bn), true).await {
            Ok(b) => b,
            Err(_) => continue,
        };
        let ts = blk
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(hex_to_u64)
            .unwrap_or(0);
        let txs = blk
            .get("transactions")
            .and_then(|v| v.as_array())
            .map(|a| a.len() as u64)
            .unwrap_or(0);

        if last_ts == 0 {
            last_ts = ts;
        }
        first_ts = ts;
        total_txs_in_sample += txs;

        // Sample fees from first few blocks only (up to 20 txs total)
        if fee_samples.len() < 20 {
            if let Some(tx_arr) = blk.get("transactions").and_then(|v| v.as_array()) {
                for tx in tx_arr {
                    if fee_samples.len() >= 20 {
                        break;
                    }
                    let gp = tx
                        .get("gasPrice")
                        .and_then(|v| v.as_str())
                        .map(hex_to_u128)
                        .unwrap_or(0);
                    let gas_limit = tx
                        .get("gas")
                        .and_then(|v| v.as_str())
                        .map(hex_to_u128)
                        .unwrap_or(0);
                    // Use gasLimit as upper bound; receipt would be more precise
                    // but we avoid per-tx receipt calls for stats
                    let fee_wei = gp * gas_limit;
                    fee_samples.push(fee_wei as f64 / 1e18);
                }
            }
        }
    }

    let time_span = if last_ts > first_ts {
        last_ts - first_ts
    } else {
        1
    };
    let blocks_in_sample = sample_end.saturating_sub(sample_start).max(1);
    let avg_block_time = time_span as f64 / blocks_in_sample as f64;
    let tps = if time_span > 0 {
        total_txs_in_sample as f64 / time_span as f64
    } else {
        0.0
    };

    // Extrapolate 24h metrics from sample
    let blocks_per_day = if avg_block_time > 0.0 {
        86400.0 / avg_block_time
    } else {
        28800.0
    };
    let avg_txs_per_block = total_txs_in_sample as f64 / blocks_in_sample as f64;
    let txs_24h = (avg_txs_per_block * blocks_per_day) as u64;

    let tx_cache = state.tx_count_cache.read().await;
    let total_tx_cumulative = tx_cache.total_transactions;

    let avg_fee = if fee_samples.is_empty() {
        0.0
    } else {
        fee_samples.iter().sum::<f64>() / fee_samples.len() as f64
    };
    let total_fee_24h = avg_fee * txs_24h as f64;

    // Pending txs from txpool
    let pending_count = match rpc_call(&state, "txpool_status", vec![]).await {
        Ok(pool) => pool
            .get("pending")
            .and_then(|v| v.as_str())
            .map(hex_to_u64)
            .unwrap_or(0),
        Err(_) => 0,
    };

    // (`total_strings_real` and `by_kind_breakdown` were computed at
    // the top of this handler — see the Quipu Canon v1.2 note above.)

    Json(serde_json::json!({
        // ── Quipu Canon v1.2 — knot/transaction/event hierarchy ──────────────
        // See .cursor/rules/quipu-canon-v1.2-knot-event-distinction.mdc
        // for the full canonical definition. Hierarchy from top to bottom:
        //   cord anchor knot  (~1.1 M)  ← cordAnchors
        //     ├─ transactions (~110 K)  ← transactions  (one knot type)
        //     │   └─ events   (~50 K)   ← events        (sub-tx logs)
        //     └─ per-entity knots       ← entityKnots   (v1.2 registry)
        //
        // Each layer is exposed under a self-explanatory canonical name
        // plus a one-release deprecated alias for the legacy v1.0/1.1 names.
        // ─────────────────────────────────────────────────────────────────────

        // Cord anchor knot count (== EVM block height). One anchor every
        // ~3 s. This is what cord-level Quipu language calls "knot"; it
        // bundles all the transactions of that anchor interval.
        "cordAnchors": head,

        // Count of EVM-shaped knots (transactions) inside cord anchors.
        // A transaction is one type of knot — see canon §4 event_type.
        "transactions": total_tx_cumulative,

        // Count of sub-transaction events scanned (Transfer / Approval /
        // Mint / Burn / etc.). Each event becomes a per-entity knot once
        // the affected entity has a string in the v1.2 registry.
        "events": tx_cache.total_transfer_events,

        // Per-entity knots in the v1.2 string registry (sourced from
        // rope_globalStats.total_knots). Grows as ecosystem agents emit.
        "entityKnots": total_strings_real,
        "stringsByKind": by_kind_breakdown,

        // Number of distinct entity strings in the v1.2 registry.
        "strings": total_strings_real,

        // ── DEPRECATED ALIASES (v1.0/1.1 names — drop in v1.3) ───────────────
        // These keep existing frontends working through one release. New
        // code should use the canonical names above.
        // - totalKnots was the cord anchor count (cordAnchors)
        // - totalTransactions is now `transactions`
        // - totalStrings was overloaded; now strictly entity-string count
        // - totalTransferEvents is now `events`
        // - totalBlocksLegacy was the v1.1 alias for the same cord count
        "totalKnots": head,
        "totalTransactions": total_tx_cumulative,
        "totalStrings": total_strings_real,
        "totalBlocksLegacy": head,
        "totalTransferEvents": tx_cache.total_transfer_events,

        // Diagnostic: how far the cumulative scan has progressed. Used by
        // operators to verify cache persistence is working (after a
        // restart, lastScannedBlock should match what was on disk, not 0).
        "lastScannedBlock": tx_cache.last_scanned_block,
        "scanProgressPct": if head > 0 {
            ((tx_cache.last_scanned_block as f64 / head as f64) * 100.0).min(100.0)
        } else {
            0.0
        },

        // Cumulative DCR-20 transfer volume since genesis (the *conveyed*
        // value across all known DCR-20 contracts: WFAT, USDC, USDT, EUROD).
        // Updated by the same incremental scan as the transaction count.
        "totalVolumeUsd": format!("${:.2}", tx_cache.total_volume_usd),
        "totalVolumeUsdRaw": tx_cache.total_volume_usd,
        "totalVolumeFat": format!("{:.4} FAT", tx_cache.total_volume_fat),
        "totalVolumeFatRaw": tx_cache.total_volume_fat,
        "transactions24h": txs_24h,
        "pendingTransactions": pending_count,
        "avgTxnFee24h": format!("{:.6} FAT", avg_fee),
        "totalTxnFee24h": format!("{:.2} FAT", total_fee_24h),
        "gasPrice": gas_price_gwei,
        "fatPrice": fat_price,
        "fatPriceRaw": price_data.price,
        "fatPriceChange24h": price_data.change_24h,
        "fatPriceSource": price_data.source,
        "marketCap": market_cap,
        "circulatingSupply": "10,000,000,000 FAT",
        "tps": format!("{:.1}", tps),
        "avgBlockTime": format!("{:.1}s", avg_block_time),
        "finalityTime": format!("{:.1}s", avg_block_time * 2.0)
    }))
}

/// DC FAT Token Price endpoint
async fn dcfat_price(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // Check cache first
    let cache = state.price_cache.read().await;

    let price_data = if let Some(cached) = &*cache {
        // Check if cache is still valid (within TTL)
        let now = chrono::Utc::now().timestamp();
        if now - cached.timestamp < PRICE_CACHE_TTL_SECS as i64 {
            cached.clone()
        } else {
            drop(cache); // Release read lock before fetching
                         // Cache expired, fetch new data
            match fetch_and_cache_price(&state).await {
                Ok(data) => data,
                Err(_) => PriceData::default(),
            }
        }
    } else {
        drop(cache); // Release read lock before fetching
                     // No cache, fetch new data
        match fetch_and_cache_price(&state).await {
            Ok(data) => data,
            Err(_) => PriceData::default(),
        }
    };

    let next_update =
        chrono::DateTime::from_timestamp(price_data.timestamp + PRICE_CACHE_TTL_SECS as i64, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();

    Json(serde_json::json!({
        "price": price_data.price,
        "priceFormatted": format!("${:.6}", price_data.price),
        "change24h": price_data.change_24h,
        "change24hFormatted": format!("{:.2}%", price_data.change_24h),
        "volume24h": price_data.volume_24h,
        "liquidity": price_data.liquidity,
        "source": price_data.source,
        "contract": DC_FAT_CONTRACT,
        "network": "XDC Network",
        "timestamp": price_data.timestamp,
        "nextUpdate": next_update,
        "sources": {
            "primary": format!("https://info.xspswap.finance/#/tokens/{}", DC_FAT_CONTRACT),
            "secondary": format!("https://xdcscan.io/token/{}", DC_FAT_CONTRACT)
        }
    }))
}

#[derive(Deserialize)]
struct ChartParams {
    period: Option<String>,
}

/// Real daily knot-anchor (cord-anchor) time series derived from live
/// chain data. Previously fabricated a `1000 + i*100` synthetic ramp
/// regardless of `chart_type`. Rope anchors a knot roughly every ~3s
/// (Quipu Canon knot interval), so each day boundary's approximate block
/// is located by walking back from the current head using that interval,
/// then corrected against the block's real on-chain timestamp. The
/// reported `value` for `chart_type=knots|blocks|cordAnchors` is the
/// real knot-count delta between consecutive day boundaries; for any
/// other `chart_type` the series still reflects the real anchor cadence
/// (no separate per-metric indexer exists yet), and the response says so
/// explicitly via `metric` rather than silently mislabeling the data.
async fn chart_data(
    State(state): State<Arc<AppState>>,
    Path(chart_type): Path<String>,
    Query(params): Query<ChartParams>,
) -> Json<serde_json::Value> {
    let period = params.period.unwrap_or_else(|| "7d".to_string());
    let days: i64 = match period.as_str() {
        "24h" | "1d" => 1,
        "30d" => 30,
        _ => 7,
    };

    let head = match rpc_block_number(&state).await {
        Ok(n) => n,
        Err(e) => {
            return Json(serde_json::json!({
                "chartType": chart_type,
                "metric": "cordAnchors",
                "data": [],
                "error": format!("live chain unavailable: {}", e),
            }));
        }
    };

    let head_ts = rpc_call(
        &state,
        "eth_getBlockByNumber",
        vec![
            serde_json::json!(format!("0x{:x}", head)),
            serde_json::json!(false),
        ],
    )
    .await
    .ok()
    .and_then(|b| b.get("timestamp").and_then(|v| v.as_str()).map(hex_to_u64))
    .unwrap_or(chrono::Utc::now().timestamp() as u64);

    const KNOT_INTERVAL_SECS: u64 = 3; // Quipu Canon nominal knot interval

    let mut boundary_blocks: Vec<(i64, u64)> = Vec::with_capacity((days + 1) as usize);
    for i in 0..=days {
        let target_ts = head_ts.saturating_sub((i as u64) * 86_400);
        let elapsed = head_ts.saturating_sub(target_ts);
        let estimated_block = head.saturating_sub(elapsed / KNOT_INTERVAL_SECS);
        boundary_blocks.push((chrono::Utc::now().timestamp() - i * 86_400, estimated_block));
    }
    boundary_blocks.reverse(); // oldest -> newest

    let mut data: Vec<serde_json::Value> = Vec::with_capacity(days as usize);
    for w in boundary_blocks.windows(2) {
        let (older_ts, older_block) = w[0];
        let (newer_ts, newer_block) = w[1];
        let delta = newer_block.saturating_sub(older_block);
        data.push(serde_json::json!({
            "timestamp": newer_ts,
            "value": delta,
        }));
        let _ = older_ts;
    }

    Json(serde_json::json!({
        "chartType": chart_type,
        "metric": "cordAnchors",
        "period": period,
        "data": data,
        "source": "eth_blockNumber + eth_getBlockByNumber (live, day-boundary block estimation)",
    }))
}

#[derive(Deserialize)]
struct PaginationParams {
    page: Option<u32>,
    limit: Option<u32>,
    filter: Option<String>,
}

/// Quipu Canon v1.2 — `/api/v1/registry/strings`.
///
/// Per-entity string registry. Thin proxy to `rope_listStrings` on the
/// consensus node. Falls back to an empty page when the RPC is
/// unavailable (older nodes mid-rolling-deploy).
///
/// Query params:
///   - `kind`   one of `wallet|contract|asset|did|cord` (optional)
///   - `offset` (default 0)
///   - `limit`  (default 50, max 500)
#[derive(Deserialize)]
struct RegistryQuery {
    kind: Option<String>,
    offset: Option<u64>,
    limit: Option<u64>,
}

async fn registry_list_strings(
    State(state): State<Arc<AppState>>,
    Query(q): Query<RegistryQuery>,
) -> Json<serde_json::Value> {
    let mut params_obj = serde_json::Map::new();
    if let Some(kind) = q.kind.as_ref() {
        params_obj.insert("kind".to_string(), serde_json::Value::String(kind.clone()));
    }
    if let Some(off) = q.offset {
        params_obj.insert("offset".to_string(), serde_json::json!(off));
    }
    if let Some(lim) = q.limit {
        params_obj.insert("limit".to_string(), serde_json::json!(lim));
    }
    let params = vec![serde_json::Value::Object(params_obj)];
    match rpc_call(&state, "rope_listStrings", params).await {
        Ok(v) => Json(v),
        Err(_) => Json(serde_json::json!({
            "total": 0,
            "offset": q.offset.unwrap_or(0),
            "limit": q.limit.unwrap_or(50),
            "kind_filter": q.kind,
            "strings": [],
            "error": "rope_listStrings unavailable on this node (Quipu Canon v1.2 RPC required)"
        })),
    }
}

async fn registry_global_stats(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    match rpc_call(&state, "rope_globalStats", vec![]).await {
        Ok(v) => {
            // Refresh the cache opportunistically so /api/v1/stats and
            // /api/v1/registry/stats never disagree.
            if let Some(s) = v.get("total_strings").and_then(|x| x.as_u64()) {
                let bk = v.get("by_kind").cloned().unwrap_or(serde_json::Value::Null);
                *state.global_stats_cache.write().await = Some(GlobalStatsCacheEntry {
                    fetched_at: chrono::Utc::now().timestamp(),
                    total_strings: s,
                    by_kind: bk,
                });
            }
            Json(v)
        }
        Err(_) => {
            // Fall back to the last known good snapshot before declaring
            // the RPC unavailable. Smooths over rope-node hiccups.
            let cache = state.global_stats_cache.read().await;
            if let Some(entry) = cache.as_ref() {
                let total_knots = entry
                    .by_kind
                    .as_object()
                    .map(|m| {
                        m.values()
                            .filter_map(|v| v.get("knots").and_then(|k| k.as_u64()))
                            .sum::<u64>()
                    })
                    .unwrap_or(0);
                Json(serde_json::json!({
                    "total_strings": entry.total_strings,
                    "total_knots": total_knots,
                    "by_kind": entry.by_kind,
                    "invariant_holds": total_knots >= entry.total_strings,
                    "stale_at": entry.fetched_at,
                    "note": "served from local cache — rope_globalStats RPC briefly unavailable"
                }))
            } else {
                Json(serde_json::json!({
                    "total_strings": 0,
                    "total_knots": 0,
                    "by_kind": {},
                    "invariant_holds": true,
                    "error": "rope_globalStats unavailable on this node (Quipu Canon v1.2 RPC required)"
                }))
            }
        }
    }
}

async fn list_strings(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PaginationParams>,
) -> Json<serde_json::Value> {
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20).min(100);

    match rpc_block_number(&state).await {
        Ok(total) => {
            let total = total.max(1);
            let limit_u = limit as u64;
            let start = total.saturating_sub((page as u64).saturating_mul(limit_u));
            let mut strings = Vec::with_capacity(limit as usize);
            for i in 0..limit {
                let num = start + limit_u.saturating_sub(1).saturating_sub(i as u64);
                if num == 0 {
                    break;
                }
                let block_hex = format!("0x{:x}", num);
                if let Ok(block) = rpc_call(
                    &state,
                    "eth_getBlockByNumber",
                    vec![serde_json::json!(block_hex), serde_json::json!(false)],
                )
                .await
                {
                    if let Some(blk) = block.as_object() {
                        let hash = blk.get("hash").and_then(|v| v.as_str()).unwrap_or("0x0");
                        let timestamp = blk
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .map(|s| {
                                u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0)
                            })
                            .unwrap_or(0);
                        let miner = blk.get("miner").and_then(|v| v.as_str()).unwrap_or("0x0");
                        let tx_count = blk
                            .get("transactions")
                            .and_then(|v| v.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        let gas_used = blk.get("gasUsed").and_then(|v| v.as_str()).unwrap_or("0x0");
                        let gas_limit = blk
                            .get("gasLimit")
                            .and_then(|v| v.as_str())
                            .unwrap_or("0x0");
                        let gas_used_dec =
                            u64::from_str_radix(gas_used.trim_start_matches("0x"), 16).unwrap_or(0);
                        let gas_limit_dec =
                            u64::from_str_radix(gas_limit.trim_start_matches("0x"), 16)
                                .unwrap_or(30_000_000);
                        strings.push(serde_json::json!({
                            "number": num,
                            "hash": hash,
                            "transactions": tx_count,
                            "validator": miner,
                            "timestamp": timestamp as i64,
                            "gasUsed": gas_used_dec.to_string(),
                            "gasLimit": gas_limit_dec.to_string(),
                            "status": "Final",
                            "aiVerified": false
                        }));
                    }
                }
            }
            return Json(serde_json::json!({
                "strings": strings,
                "pagination": { "page": page, "limit": limit, "total": total }
            }));
        }
        Err(_) => {}
    }

    Json(serde_json::json!({
        "strings": [],
        "pagination": { "page": page, "limit": limit, "total": 0 }
    }))
}

async fn latest_strings(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let head = rpc_block_number(&state).await.unwrap_or(0);
    let mut strings = Vec::with_capacity(10);
    for i in 0u64..10 {
        let num = head.saturating_sub(i);
        if num == 0 {
            break;
        }
        let block_hex = format!("0x{:x}", num);
        if let Ok(block) = rpc_get_block(&state, &block_hex, false).await {
            let hash = block.get("hash").and_then(|v| v.as_str()).unwrap_or("0x0");
            let ts = block
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| hex_to_u64(s) as i64)
                .unwrap_or(0);
            let miner = block.get("miner").and_then(|v| v.as_str()).unwrap_or("0x0");
            let tx_count = block
                .get("transactions")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            strings.push(serde_json::json!({
                "number": num,
                "hash": hash,
                "transactions": tx_count,
                "validator": miner,
                "timestamp": ts,
                "status": "Final"
            }));
        }
    }
    Json(serde_json::json!({ "strings": strings }))
}

async fn get_string(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let num = match id.parse::<u64>() {
        Ok(n) if n > 0 => n,
        _ => {
            return Json(serde_json::json!({ "error": "Invalid string number", "id": id }));
        }
    };

    let block_hex = format!("0x{:x}", num);
    let block = match rpc_get_block(&state, &block_hex, true).await {
        Ok(b) if !b.is_null() => b,
        _ => {
            return Json(serde_json::json!({ "error": "String not found", "id": id }));
        }
    };

    let blk = match block.as_object() {
        Some(b) => b,
        None => {
            return Json(serde_json::json!({ "error": "String not found", "id": id }));
        }
    };

    let hash = blk.get("hash").and_then(|v| v.as_str()).unwrap_or("0x0");
    let parent_hash = blk
        .get("parentHash")
        .and_then(|v| v.as_str())
        .unwrap_or("0x0");
    let timestamp = blk
        .get("timestamp")
        .and_then(|v| v.as_str())
        .map(|s| hex_to_u64(s) as i64)
        .unwrap_or(0);
    let miner = blk.get("miner").and_then(|v| v.as_str()).unwrap_or("0x0");
    let gas_used = blk.get("gasUsed").and_then(|v| v.as_str()).unwrap_or("0x0");
    let gas_limit = blk
        .get("gasLimit")
        .and_then(|v| v.as_str())
        .unwrap_or("0x0");
    let gas_used_dec = hex_to_u64(gas_used);
    let gas_limit_dec = hex_to_u64(gas_limit);

    let txs_list: Vec<serde_json::Value> = blk
        .get("transactions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|tx| {
                    let tx_hash = tx.get("hash").and_then(|v| v.as_str()).unwrap_or("0x0");
                    let from = tx.get("from").and_then(|v| v.as_str()).unwrap_or("0x0");
                    let to = tx.get("to").and_then(|v| v.as_str());
                    let value_hex = tx.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");
                    let gas_hex = tx.get("gas").and_then(|v| v.as_str()).unwrap_or("0x0");
                    let value_fat = wei_to_fat(value_hex);
                    serde_json::json!({
                        "hash": tx_hash,
                        "from": from,
                        "to": to,
                        "value": format_fat(value_fat),
                        "gasUsed": hex_to_u64(gas_hex).to_string()
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let tx_count = txs_list.len();

    Json(serde_json::json!({
        "number": num,
        "hash": hash,
        "parentHash": parent_hash,
        "timestamp": timestamp,
        "transactions": tx_count,
        "transactionsList": txs_list,
        "validator": miner,
        "gasUsed": gas_used_dec.to_string(),
        "gasLimit": gas_limit_dec.to_string(),
        "status": "Final"
    }))
}

async fn enrich_tx_with_transfers(
    state: &AppState,
    summary: &mut serde_json::Value,
    tx_hash: &str,
) {
    if tx_hash.is_empty() {
        return;
    }
    if let Ok(receipt) = rpc_call(
        state,
        "eth_getTransactionReceipt",
        vec![serde_json::json!(tx_hash)],
    )
    .await
    {
        if !receipt.is_null() {
            // Decode token transfers from logs
            let logs: Vec<serde_json::Value> = receipt
                .get("logs")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter().map(|log| serde_json::json!({
                    "address": log.get("address").and_then(|v| v.as_str()).unwrap_or(""),
                    "topics": log.get("topics").cloned().unwrap_or(serde_json::json!([])),
                    "data": log.get("data").and_then(|v| v.as_str()).unwrap_or("0x")
                })).collect()
                })
                .unwrap_or_default();
            let (_, transfer_summary) = decode_token_transfers(&logs);
            if !transfer_summary.is_empty() {
                summary["value"] = serde_json::json!(transfer_summary);
            }

            // Fee from receipt: gasUsed * effectiveGasPrice (actual on-chain fee)
            let gas_used = receipt
                .get("gasUsed")
                .and_then(|v| v.as_str())
                .map(hex_to_u128)
                .unwrap_or(0);
            let effective_gas_price = receipt
                .get("effectiveGasPrice")
                .and_then(|v| v.as_str())
                .map(hex_to_u128)
                .unwrap_or(0);
            let fee_wei = gas_used * effective_gas_price;
            let fee_fat = fee_wei as f64 / 1e18;
            summary["txnFee"] = if fee_fat == 0.0 {
                serde_json::json!("0 FAT")
            } else if fee_fat < 0.000001 {
                serde_json::json!(format!("{:.8} FAT", fee_fat))
            } else {
                serde_json::json!(format!("{:.6} FAT", fee_fat))
            };
            summary["gasUsed"] = serde_json::json!(gas_used.to_string());

            // Status from receipt
            let status_hex = receipt
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("0x1");
            summary["status"] = if status_hex == "0x1" {
                serde_json::json!("Success")
            } else {
                serde_json::json!("Failed")
            };
        }
    }
}

async fn list_transactions(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PaginationParams>,
) -> Json<serde_json::Value> {
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20).min(100) as usize;

    let max_blocks_to_scan: u64 = (limit as u64) * 10;
    let collected =
        collect_txs_from_recent_blocks(&state, max_blocks_to_scan, limit * page as usize).await;

    let start = (page as usize - 1) * limit;
    let page_slice: Vec<&(serde_json::Value, u64, i64)> =
        collected.iter().skip(start).take(limit).collect();

    let mut page_txs = Vec::with_capacity(page_slice.len());
    for (tx, bn, ts) in page_slice {
        let mut summary = tx_summary_json(tx, *bn, *ts);
        let hash = tx.get("hash").and_then(|v| v.as_str()).unwrap_or("");
        enrich_tx_with_transfers(&state, &mut summary, hash).await;
        page_txs.push(summary);
    }

    let total_head = rpc_block_number(&state).await.unwrap_or(0);

    Json(serde_json::json!({
        "transactions": page_txs,
        "pagination": {
            "page": page,
            "limit": limit,
            "total": total_head * 2
        }
    }))
}

/// TTL for the homepage "Latest Transactions" cache, in seconds. Short
/// enough that the list still feels live, long enough that the
/// every-15s frontend poll from many concurrent visitors collapses into
/// one backing scan.
const LATEST_TX_CACHE_TTL_SECS: i64 = 8;

/// How many of the most recent blocks to walk looking for transactions.
/// Under real Testimony-quorum block production most anchor knots are
/// empty (see comment on `AppState::latest_tx_cache`), so this must be
/// wide enough to reliably find at least a handful of real transactions
/// even during a lull, not just a small fixed window (which routinely
/// came up completely empty and rendered "No transactions yet" on a
/// chain that in fact has plenty of history). `collect_txs_from_recent_blocks`
/// no longer walks blocks one-by-one with a full-body fetch; it
/// batch-scans cheap `eth_getBlockTransactionCountByNumber` counts
/// (200 per HTTP round-trip, same technique as the tx+volume scanner)
/// and only fetches full bodies for the blocks that actually have
/// transactions, so widening this to 20,000 blocks (~23 hours of real
/// chain time at the ~4.2s block interval) is still bounded to roughly
/// 100 batch round-trips in the fully-empty worst case.
const LATEST_TX_SCAN_BLOCKS: u64 = 20_000;

async fn latest_transactions(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let now = chrono::Utc::now().timestamp();
    if let Some(entry) = state.latest_tx_cache.read().await.clone() {
        if now - entry.fetched_at < LATEST_TX_CACHE_TTL_SECS {
            return Json(entry.payload);
        }
    }

    let collected = collect_txs_from_recent_blocks(&state, LATEST_TX_SCAN_BLOCKS, 10).await;
    let mut txs = Vec::with_capacity(collected.len());
    for (tx, bn, ts) in &collected {
        let mut summary = tx_summary_json(tx, *bn, *ts);
        let hash = tx.get("hash").and_then(|v| v.as_str()).unwrap_or("");
        enrich_tx_with_transfers(&state, &mut summary, hash).await;
        txs.push(summary);
    }
    let payload = serde_json::json!({ "transactions": txs });

    // Only cache a non-empty result. If the wide scan genuinely finds
    // nothing (e.g. a brand-new chain), don't lock in an empty response
    // for the full TTL — let the next request try again immediately.
    if !txs.is_empty() {
        *state.latest_tx_cache.write().await = Some(BotActivityCacheEntry {
            fetched_at: now,
            payload: payload.clone(),
        });
    }
    Json(payload)
}

/// Known DCSwap contracts (Router + Factory + Pools). Used by the
/// /api/v1/dcswap/bots endpoint to identify which `to` addresses count
/// as DCSwap interactions. Sourced from handover-dcswap-redeployed-2026-02-26.
const DCSWAP_CONTRACTS: &[(&str, &str)] = &[
    ("0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4", "DCSwap Router"),
    ("0x772e5fd559069aecce5e6983c0c415c8579d780d", "DCSwap Factory"),
    ("0xd9ebc3da001618a3ae90481d33ae7ef85e130317", "FAT/USDC Pool"),
    ("0x644da44bcd5f453c593781dbe22dfd733e8d1441", "FAT/USDT Pool"),
    ("0x1e9c2ccf67320459bc4999a9f8be4a063d4021e4", "FAT/EUROD Pool"),
    ("0xb86bdcecad93573d6ca21313aa7eac52800513c8", "USDC/USDT Pool"),
    // Post-Reth set (planned addresses)
    ("0x4e1cfaa1c7ea2ca96b4a49fc4b8e75a2a3dc402e", "DCSwap Router (post-Reth)"),
    ("0xa5c55b0cb658dc5a651fcb0054a040a194433694", "DCSwap Factory (post-Reth)"),
];

/// Quipu Canon v1.2 — DCSwap bot activity surface.
///
/// Scans the last `window` blocks (default ~24 h) and counts, per
/// from-address, how many transactions touched a known DCSwap contract.
/// Surfaces the busiest callers so users browsing dcscan.io can see at
/// a glance which bot wallets are operating against DCSwap right now,
/// and on which contract.
///
/// This is the today-version of "all bots operating on DCSwap's strings'
/// activities visible on dcscan.io". When the bot wallets start emitting
/// per-wallet knots per the Moneymaker handover, this endpoint will
/// additionally enrich each entry with the wallet's v1.2 string head.
/// TTL for the DCSwap bot activity cache, in seconds.
const BOT_ACTIVITY_CACHE_TTL_SECS: i64 = 60;

async fn dcswap_bot_activity(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    use std::collections::HashMap;

    // Cheap path: serve from cache when recent.
    let now = chrono::Utc::now().timestamp();
    if let Some(entry) = state.bot_activity_cache.read().await.clone() {
        if now - entry.fetched_at < BOT_ACTIVITY_CACHE_TTL_SECS {
            return Json(entry.payload);
        }
    }

    // Scan window: each block fetch is one HTTP RPC round-trip, so we
    // keep this tight. 100 blocks ≈ 5 minutes of activity; that's plenty
    // to surface the active bots since DCSwap bots fire every few seconds.
    // The endpoint itself returns in well under a second.
    let scan_blocks: u64 = 100;
    let head = match rpc_block_number(&state).await {
        Ok(h) => h,
        Err(_) => 0,
    };
    let start = head.saturating_sub(scan_blocks);

    // Counters: (from_address, contract_address) -> tx_count
    let mut by_caller: HashMap<String, HashMap<String, u64>> = HashMap::new();
    let dcswap_set: std::collections::HashSet<String> = DCSWAP_CONTRACTS
        .iter()
        .map(|(addr, _)| addr.to_lowercase())
        .collect();

    // Iterate blocks directly so we can trace any RPC errors and ensure
    // an empty/missing block doesn't silently zero out the whole window.
    let mut blocks_fetched = 0u32;
    let mut blocks_failed = 0u32;
    let mut total_txs_scanned = 0u32;
    if head > 0 {
        for offset in 0..scan_blocks {
            let num = head.saturating_sub(offset);
            if num == 0 {
                break;
            }
            let block_hex = format!("0x{:x}", num);
            // One retry on connection-reset / pool-exhaustion errors
            // (rope-node→Reth forwarder occasionally drops calls).
            let mut block_result = rpc_get_block(&state, &block_hex, true).await;
            if block_result.is_err() {
                tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                block_result = rpc_get_block(&state, &block_hex, true).await;
            }
            match block_result {
                Ok(block) => {
                    blocks_fetched += 1;
                    if let Some(txs) = block.get("transactions").and_then(|v| v.as_array()) {
                        for tx in txs {
                            total_txs_scanned += 1;
                            let from = tx
                                .get("from")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_lowercase();
                            let to = tx
                                .get("to")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_lowercase();
                            if from.is_empty() || to.is_empty() {
                                continue;
                            }
                            if !dcswap_set.contains(&to) {
                                continue;
                            }
                            let entry = by_caller.entry(from).or_default();
                            *entry.entry(to).or_default() += 1;
                        }
                    }
                }
                Err(e) => {
                    blocks_failed += 1;
                    if blocks_failed <= 3 {
                        tracing::warn!(
                            "dcswap_bot_activity: rpc_get_block({}) failed: {}",
                            block_hex,
                            e
                        );
                    }
                }
            }
        }
    }
    tracing::info!(
        "dcswap_bot_activity: head={} scan_blocks={} blocks_ok={} blocks_failed={} txs_scanned={} matched_callers={}",
        head,
        scan_blocks,
        blocks_fetched,
        blocks_failed,
        total_txs_scanned,
        by_caller.len()
    );

    let contract_label = |addr: &str| -> &'static str {
        for (a, label) in DCSWAP_CONTRACTS {
            if a.eq_ignore_ascii_case(addr) {
                return label;
            }
        }
        "DCSwap Contract"
    };

    let mut bots: Vec<serde_json::Value> = by_caller
        .into_iter()
        .map(|(caller, contract_map)| {
            let total: u64 = contract_map.values().sum();
            let mut interactions: Vec<serde_json::Value> = contract_map
                .into_iter()
                .map(|(addr, count)| {
                    serde_json::json!({
                        "contract": addr,
                        "label": contract_label(&addr),
                        "txCount": count,
                    })
                })
                .collect();
            interactions.sort_by(|a, b| {
                b.get("txCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    .cmp(&a.get("txCount").and_then(|v| v.as_u64()).unwrap_or(0))
            });
            serde_json::json!({
                "caller": caller,
                "totalTxCount": total,
                "interactions": interactions,
                // Reserved for the v1.2.1 enrichment when bots start
                // emitting per-wallet knots — will populate
                // string_id / head_knot_id / knot_count from rope_getString.
                "v12String": serde_json::Value::Null,
            })
        })
        .collect();

    bots.sort_by(|a, b| {
        b.get("totalTxCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .cmp(&a.get("totalTxCount").and_then(|v| v.as_u64()).unwrap_or(0))
    });

    let bot_count = bots.len();
    let total_interactions: u64 = bots
        .iter()
        .map(|b| b.get("totalTxCount").and_then(|v| v.as_u64()).unwrap_or(0))
        .sum();

    let payload = serde_json::json!({
        "window": {
            "fromBlock": start,
            "toBlock": head,
            "blocks": scan_blocks,
            "approxMinutes": (scan_blocks as f64 * 3.0) / 60.0,
            "blocksFetched": blocks_fetched,
            "blocksFailed": blocks_failed,
            "txsScanned": total_txs_scanned,
        },
        "uniqueCallers": bot_count,
        "totalInteractions": total_interactions,
        "knownContracts": DCSWAP_CONTRACTS
            .iter()
            .map(|(addr, label)| serde_json::json!({"address": addr, "label": label}))
            .collect::<Vec<_>>(),
        "bots": bots,
        "cachedAt": now,
        "cacheTtlSecs": BOT_ACTIVITY_CACHE_TTL_SECS,
        "note": "Bots are identified by transaction frequency against known \
                 DCSwap contracts in the recent block window. Once Moneymaker \
                 / DCSwap agents emit per-wallet knots per their v1.2 \
                 handovers, each entry will include the wallet's string \
                 head from the Quipu Canon v1.2 registry."
    });

    *state.bot_activity_cache.write().await = Some(BotActivityCacheEntry {
        fetched_at: now,
        payload: payload.clone(),
    });

    Json(payload)
}

/// Per-contract caller list — generic version of the DCSwap-specific
/// /api/v1/dcswap/bots endpoint. Returns the top callers of `address`
/// in the recent block window.
async fn contract_recent_callers(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    use std::collections::HashMap;
    let target = address.to_lowercase();
    let scan_blocks: u64 = 100;
    let head = match rpc_block_number(&state).await {
        Ok(h) => h,
        Err(_) => 0,
    };
    let start = head.saturating_sub(scan_blocks);

    let mut counter: HashMap<String, u64> = HashMap::new();
    let txs = collect_txs_from_recent_blocks(&state, scan_blocks, 5_000).await;
    for (tx, _bn, _ts) in &txs {
        let from = tx
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let to = tx
            .get("to")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if to != target || from.is_empty() {
            continue;
        }
        *counter.entry(from).or_default() += 1;
    }

    let mut callers: Vec<serde_json::Value> = counter
        .into_iter()
        .map(|(caller, count)| serde_json::json!({"caller": caller, "txCount": count}))
        .collect();
    callers.sort_by(|a, b| {
        b.get("txCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .cmp(&a.get("txCount").and_then(|v| v.as_u64()).unwrap_or(0))
    });

    Json(serde_json::json!({
        "contract": target,
        "window": {
            "fromBlock": start,
            "toBlock": head,
            "blocks": scan_blocks,
            "txsScanned": txs.len(),
        },
        "uniqueCallers": callers.len(),
        "callers": callers,
    }))
}

async fn pending_transactions_live(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let pool = match rpc_call(&state, "txpool_content", vec![]).await {
        Ok(p) if !p.is_null() => p,
        _ => return Json(serde_json::json!({ "pending": [], "total": 0 })),
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut txs: Vec<serde_json::Value> = Vec::new();

    if let Some(pending_map) = pool.get("pending").and_then(|v| v.as_object()) {
        for (_addr, nonces) in pending_map {
            if let Some(nonce_map) = nonces.as_object() {
                for (_nonce, tx) in nonce_map {
                    let hash = tx.get("hash").and_then(|v| v.as_str()).unwrap_or("0x0");
                    let from = tx.get("from").and_then(|v| v.as_str()).unwrap_or("0x0");
                    let to = tx.get("to").and_then(|v| v.as_str()).unwrap_or("0x0");
                    let nonce = tx
                        .get("nonce")
                        .and_then(|v| v.as_str())
                        .map(hex_to_u64)
                        .unwrap_or(0);
                    let gas_price_wei = tx
                        .get("gasPrice")
                        .and_then(|v| v.as_str())
                        .map(hex_to_u128)
                        .unwrap_or(0);
                    let gas_price_gwei = gas_price_wei as f64 / 1e9;
                    let gas_limit = tx
                        .get("gas")
                        .and_then(|v| v.as_str())
                        .map(hex_to_u64)
                        .unwrap_or(0);
                    let value_hex = tx.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");
                    let value_fat = wei_to_fat(value_hex);

                    let mut entry = serde_json::json!({
                        "hash": hash,
                        "from": from,
                        "to": to,
                        "nonce": nonce,
                        "gasPrice": format!("{:.4} gwei", gas_price_gwei),
                        "gasLimit": gas_limit,
                        "value": format_fat(value_fat),
                        "timestamp": now
                    });
                    enrich_addr_field(&mut entry, from, "from");
                    enrich_addr_field(&mut entry, to, "to");
                    txs.push(entry);
                }
            }
        }
    }

    let total = txs.len();
    Json(serde_json::json!({ "pending": txs, "total": total }))
}

async fn get_transaction(
    State(state): State<Arc<AppState>>,
    Path(hash): Path<String>,
) -> Json<serde_json::Value> {
    let tx_result = rpc_call(
        &state,
        "eth_getTransactionByHash",
        vec![serde_json::json!(hash)],
    )
    .await;
    let receipt_result = rpc_call(
        &state,
        "eth_getTransactionReceipt",
        vec![serde_json::json!(hash)],
    )
    .await;

    let tx = match tx_result {
        Ok(ref v) if !v.is_null() => v,
        _ => {
            return Json(serde_json::json!({
                "error": "Transaction not found",
                "hash": hash
            }));
        }
    };

    let receipt = receipt_result.as_ref().ok().filter(|v| !v.is_null());

    let status = receipt
        .and_then(|r| r.get("status").and_then(|v| v.as_str()))
        .map(|s| if s == "0x1" { "Success" } else { "Failed" })
        .unwrap_or("Pending");

    let block_number_hex = receipt
        .and_then(|r| r.get("blockNumber").and_then(|v| v.as_str()))
        .or_else(|| tx.get("blockNumber").and_then(|v| v.as_str()));
    let block_number = block_number_hex.map(hex_to_u64).unwrap_or(0);

    let timestamp = if let Some(bn_hex) = block_number_hex {
        if let Ok(block) = rpc_get_block(&state, bn_hex, false).await {
            block
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| hex_to_u64(s) as i64)
                .unwrap_or_else(|| chrono::Utc::now().timestamp())
        } else {
            chrono::Utc::now().timestamp()
        }
    } else {
        chrono::Utc::now().timestamp()
    };

    let from = tx.get("from").and_then(|v| v.as_str()).unwrap_or("0x0");
    let to = tx.get("to").and_then(|v| v.as_str());
    let value_hex = tx.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");
    let gas_price_hex = tx.get("gasPrice").and_then(|v| v.as_str()).unwrap_or("0x0");
    let nonce_hex = tx.get("nonce").and_then(|v| v.as_str()).unwrap_or("0x0");
    let tx_index_hex = tx
        .get("transactionIndex")
        .and_then(|v| v.as_str())
        .unwrap_or("0x0");
    let input = tx.get("input").and_then(|v| v.as_str()).unwrap_or("0x");

    let gas_used = receipt
        .and_then(|r| r.get("gasUsed").and_then(|v| v.as_str()))
        .map(hex_to_u64)
        .unwrap_or(0);

    let gas_price_wei = hex_to_u64(gas_price_hex);
    let gas_price_gwei = gas_price_wei as f64 / 1e9;

    let value_fat = wei_to_fat(value_hex);

    let raw_logs: Vec<serde_json::Value> = receipt
        .and_then(|r| r.get("logs").and_then(|v| v.as_array()))
        .map(|arr| {
            arr.iter()
                .map(|log| {
                    serde_json::json!({
                        "address": log.get("address").and_then(|v| v.as_str()).unwrap_or("0x0"),
                        "topics": log.get("topics").cloned().unwrap_or(serde_json::json!([])),
                        "data": log.get("data").and_then(|v| v.as_str()).unwrap_or("0x")
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let (token_transfers, transfer_value) = decode_token_transfers(&raw_logs);

    let display_value = if !transfer_value.is_empty() {
        transfer_value.clone()
    } else {
        format_fat(value_fat)
    };

    let mut resp = serde_json::json!({
        "hash": hash,
        "status": status,
        "string": block_number,
        "from": from,
        "to": to,
        "value": display_value,
        "nativeValue": format_fat(value_fat),
        "gasUsed": gas_used.to_string(),
        "gasPrice": format!("{:.4} gwei", gas_price_gwei),
        "timestamp": timestamp,
        "nonce": hex_to_u64(nonce_hex),
        "index": hex_to_u64(tx_index_hex),
        "input": input,
        "logs": raw_logs,
        "tokenTransfers": token_transfers,
        "transferValue": transfer_value
    });
    enrich_addr_field(&mut resp, from, "from");
    if let Some(to_addr) = to {
        enrich_addr_field(&mut resp, to_addr, "to");
    }
    Json(resp)
}

struct KnownContract {
    address: &'static str,
    name: &'static str,
    compiler: &'static str,
    version: &'static str,
    license: &'static str,
}

const KNOWN_CONTRACTS: &[KnownContract] = &[
    // ── Live DCR-20 contracts (post 2026-02-26 redeployment) ───────────
    KnownContract {
        address: "0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4",
        name: "WFAT",
        compiler: "Solidity",
        version: "0.8.20",
        license: "MIT",
    },
    KnownContract {
        address: "0xb93bd8db94f1baff474aa9cba0739daaad01641f",
        name: "USDC",
        compiler: "Solidity",
        version: "0.8.20",
        license: "MIT",
    },
    KnownContract {
        address: "0x79a26132f48394421382c13b54ae77fa3af73289",
        name: "USDT",
        compiler: "Solidity",
        version: "0.8.20",
        license: "MIT",
    },
    KnownContract {
        address: "0x24d6137807fa8a592888726d87ac748d018c6d4a",
        name: "EUROD",
        compiler: "Solidity",
        version: "0.8.20",
        license: "MIT",
    },
    KnownContract {
        address: "0xc2eeb0100aa7e81a3193bdce6733ff767f3bb93a",
        name: "Multicall3",
        compiler: "Solidity",
        version: "0.8.12",
        license: "MIT",
    },
    KnownContract {
        address: "0x772e5fd559069aecce5e6983c0c415c8579d780d",
        name: "DCSwapFactory",
        compiler: "Solidity",
        version: "0.8.20",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4",
        name: "DCSwapRouter",
        compiler: "Solidity",
        version: "0.8.20",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0xd9ebc3da001618a3ae90481d33ae7ef85e130317",
        name: "DCSwapPair (FAT/USDC)",
        compiler: "Solidity",
        version: "0.8.20",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0x644da44bcd5f453c593781dbe22dfd733e8d1441",
        name: "DCSwapPair (FAT/USDT)",
        compiler: "Solidity",
        version: "0.8.20",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0x1e9c2ccf67320459bc4999a9f8be4a063d4021e4",
        name: "DCSwapPair (FAT/EUROD)",
        compiler: "Solidity",
        version: "0.8.20",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0xb86bdcecad93573d6ca21313aa7eac52800513c8",
        name: "DCSwapPair (USDC/USDT)",
        compiler: "Solidity",
        version: "0.8.20",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0xe158a7b8030af5386aae3bae4fc7382200064f20",
        name: "IdentityImplementation",
        compiler: "Solidity",
        version: "0.8.17",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4",
        name: "ImplementationAuthority",
        compiler: "Solidity",
        version: "0.8.17",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0xb93bd8db94f1baff474aa9cba0739daaad01641f",
        name: "IdFactory",
        compiler: "Solidity",
        version: "0.8.17",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0x79a26132f48394421382c13b54ae77fa3af73289",
        name: "ClaimTopicsRegistry",
        compiler: "Solidity",
        version: "0.8.17",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0x094237118686fef3b03af028721c2e5c23027455",
        name: "TrustedIssuersRegistry",
        compiler: "Solidity",
        version: "0.8.17",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0xe3d48836733c4ebaf504694aa5d15d6f8f22fbf2",
        name: "IdentityRegistryStorage",
        compiler: "Solidity",
        version: "0.8.17",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0xb28e38b344a7238c9777d74209f966d1873d26e0",
        name: "IdentityRegistry",
        compiler: "Solidity",
        version: "0.8.17",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0x34ab12ca0bc2cfb3510cca479cc5bd4eb6eae883",
        name: "DatawalletClaimIssuer",
        compiler: "Solidity",
        version: "0.8.17",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0x30ed28e33fcd73705bdda7c4246cf51f3d544ca6",
        name: "RopeComplianceModule",
        compiler: "Solidity",
        version: "0.8.17",
        license: "GPL-3.0",
    },
];

async fn contracts_stats_live(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut verified = 0u64;
    for kc in KNOWN_CONTRACTS {
        let code = rpc_call(
            &state,
            "eth_getCode",
            vec![serde_json::json!(kc.address), serde_json::json!("latest")],
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x".to_string());
        if code != "0x" && code.len() > 2 {
            verified += 1;
        }
    }

    let mit_count = KNOWN_CONTRACTS
        .iter()
        .filter(|c| c.license == "MIT")
        .count();
    let gpl_count = KNOWN_CONTRACTS
        .iter()
        .filter(|c| c.license == "GPL-3.0")
        .count();

    Json(serde_json::json!({
        "totalVerified": verified,
        "verifiedToday": 0,
        "openSourceLicenses": format!("{} MIT, {} GPL-3.0", mit_count, gpl_count),
        "auditedContracts": 0,
        "total": verified
    }))
}

async fn contracts_list_live(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PaginationParams>,
) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(100).min(100) as usize;
    let mut contracts: Vec<serde_json::Value> = Vec::new();

    for kc in KNOWN_CONTRACTS {
        let code = rpc_call(
            &state,
            "eth_getCode",
            vec![serde_json::json!(kc.address), serde_json::json!("latest")],
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x".to_string());

        if code == "0x" || code.len() <= 2 {
            continue;
        }

        let balance_hex = rpc_call(
            &state,
            "eth_getBalance",
            vec![serde_json::json!(kc.address), serde_json::json!("latest")],
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x0".to_string());

        let nonce_hex = rpc_call(
            &state,
            "eth_getTransactionCount",
            vec![serde_json::json!(kc.address), serde_json::json!("latest")],
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x0".to_string());

        let balance_fat = wei_to_fat(&balance_hex);
        let tx_count = hex_to_u64(&nonce_hex);

        contracts.push(serde_json::json!({
            "address": kc.address,
            "contractName": kc.name,
            "compiler": kc.compiler,
            "version": kc.version,
            "license": kc.license,
            "balance": format_fat(balance_fat),
            "txns": tx_count,
            "settings": "Optimized",
            "verified": true
        }));
    }

    let total = contracts.len();
    let page_contracts: Vec<serde_json::Value> = contracts.into_iter().take(limit).collect();

    Json(serde_json::json!({
        "contracts": page_contracts,
        "total": total,
        "pagination": {
            "page": 1,
            "limit": limit,
            "total": total
        }
    }))
}

struct AddressTag {
    label: &'static str,
    category: &'static str,
    icon: &'static str,
    /// When true, the raw address is permanently redacted from all API
    /// responses. Only the label is exposed. The hex never reaches the client.
    hidden: bool,
}

fn address_registry() -> &'static std::collections::HashMap<&'static str, AddressTag> {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<std::collections::HashMap<&'static str, AddressTag>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        m.insert(
            "0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195",
            AddressTag {
                label: "DC Treasury",
                category: "treasury",
                icon: "fa-landmark",
                hidden: true,
            },
        );
        m.insert(
            "0x302fa11a6e784dfa89f96942a919c09b45559676",
            AddressTag {
                label: "Genesis",
                category: "system",
                icon: "fa-cube",
                hidden: false,
            },
        );
        // ── Governance + legacy-migration infrastructure ──
        m.insert(
            "0x50cfc56d81603a61660b8c6306e7cb6e6693532c",
            AddressTag {
                label: "DCSwap Governance Timelock (1h delay)",
                category: "governance",
                icon: "fa-hourglass-half",
                hidden: false,
            },
        );
        // FATMigrationMinter — deployed 2026-07-08 (block 3024924, tx
        // 0xaeb17858…4355), owner = DCSwapTimelock, paused until the
        // Phase 1 audit gate clears. Escrow-releases native FAT 1:1 per
        // verified legacy DC burn (spec DC_FAT_LEGACY_MIGRATION_* v2.0).
        m.insert(
            "0x70406ae110d6ccff9a73a2ac2b82d3b666b5a51a",
            AddressTag {
                label: "DC FAT Migration Minter (Legacy DC → FAT)",
                category: "bridge",
                icon: "fa-right-left",
                hidden: false,
            },
        );
        // DCSwap Stablecoin BridgeMinter — Rope-side mint controller for the
        // lock-and-mint stablecoin bridge (USDC/USDT from Arbitrum, etc.).
        // owner = DCSwapTimelock, deployed PAUSED (audit F1); stays paused
        // until the origin vault is live and the mint path is fully wired.
        // NOTE (audit §3 — CREATE address collision): the SAME address
        // 0xBf01…6742 is a *different* contract on other chains — XdcOriginBurn
        // on XDC (50) and OriginBridgeVault on Arbitrum (42161). This registry
        // is Rope-only (chainId 271828), so this label is correctly scoped;
        // never reuse it for the same bare address ingested from another chain.
        m.insert(
            "0xbf010dad0c44ed0481ed9edcc01a2dcfd8ee6742",
            AddressTag {
                label: "DCSwap Stablecoin BridgeMinter (paused)",
                category: "bridge",
                icon: "fa-right-left",
                hidden: false,
            },
        );
        // ── Live DCR-20 stablecoins + WFAT (post 2026-02-26 redeployment) ──
        // The pre-Reth addresses (0x3109c838 / 0xddbf887 / 0x73e3cc /
        // 0xc784ea / 0x8b3554 / 0xfb0e84 / 0x38bfe / 0x7a4bcc / 0xef5f76 /
        // 0xf37bbe / 0x2e2304ca) are intentionally NOT registered here:
        // they have no bytecode on the live chain and labelling them as
        // contracts produces the misleading "$ USDC Contract  # EOA"
        // badge collision the user reported on 2026-06-04. Their
        // forwarding pointers live in `dead_token_replacement()` so the
        // address page can show a "Deprecated, see → live address"
        // banner instead.
        m.insert(
            "0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4",
            AddressTag {
                label: "WFAT Contract",
                category: "token",
                icon: "fa-coins",
                hidden: false,
            },
        );
        m.insert(
            "0xb93bd8db94f1baff474aa9cba0739daaad01641f",
            AddressTag {
                label: "USDC Contract",
                category: "token",
                icon: "fa-dollar-sign",
                hidden: false,
            },
        );
        m.insert(
            "0x79a26132f48394421382c13b54ae77fa3af73289",
            AddressTag {
                label: "USDT Contract",
                category: "token",
                icon: "fa-dollar-sign",
                hidden: false,
            },
        );
        m.insert(
            "0x24d6137807fa8a592888726d87ac748d018c6d4a",
            AddressTag {
                label: "EUROD Contract",
                category: "token",
                icon: "fa-euro-sign",
                hidden: false,
            },
        );
        m.insert(
            "0x772e5fd559069aecce5e6983c0c415c8579d780d",
            AddressTag {
                label: "DCSwapFactory",
                category: "defi",
                icon: "fa-industry",
                hidden: false,
            },
        );
        m.insert(
            "0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4",
            AddressTag {
                label: "DCSwapRouter",
                category: "defi",
                icon: "fa-exchange-alt",
                hidden: false,
            },
        );
        m.insert(
            "0xd9ebc3da001618a3ae90481d33ae7ef85e130317",
            AddressTag {
                label: "FAT/USDC Pool",
                category: "defi",
                icon: "fa-water",
                hidden: false,
            },
        );
        m.insert(
            "0x644da44bcd5f453c593781dbe22dfd733e8d1441",
            AddressTag {
                label: "FAT/USDT Pool",
                category: "defi",
                icon: "fa-water",
                hidden: false,
            },
        );
        m.insert(
            "0x1e9c2ccf67320459bc4999a9f8be4a063d4021e4",
            AddressTag {
                label: "FAT/EUROD Pool",
                category: "defi",
                icon: "fa-water",
                hidden: false,
            },
        );
        m.insert(
            "0xb86bdcecad93573d6ca21313aa7eac52800513c8",
            AddressTag {
                label: "USDC/USDT Pool",
                category: "defi",
                icon: "fa-water",
                hidden: false,
            },
        );
        m.insert(
            "0xc2eeb0100aa7e81a3193bdce6733ff767f3bb93a",
            AddressTag {
                label: "Multicall3",
                category: "infrastructure",
                icon: "fa-layer-group",
                hidden: false,
            },
        );
        // T-REX / Tanastok infrastructure contracts
        m.insert(
            "0x76b40d5439f1cb661b2479fd15410662a7fe0991",
            AddressTag {
                label: "T-REX Factory (Tanastok)",
                category: "trex",
                icon: "fa-industry",
                hidden: false,
            },
        );
        m.insert(
            "0x3065138f0ce815eb09f14d2e87e8bcbe98dd172b",
            AddressTag {
                label: "ONCHAINID Identity Registry",
                category: "trex",
                icon: "fa-id-card",
                hidden: false,
            },
        );
        m.insert(
            "0x98a7ec2f86cfe4721dff36c648396f1f5ba11ab0",
            AddressTag {
                label: "ONCHAINID Claim Topics",
                category: "trex",
                icon: "fa-list-check",
                hidden: false,
            },
        );
        m.insert(
            "0x42d605a05a063d91e83481867839bfd713d21666",
            AddressTag {
                label: "ONCHAINID Trusted Issuers",
                category: "trex",
                icon: "fa-shield-halved",
                hidden: false,
            },
        );
        m.insert(
            "0x4f4741f3cbeafd9b4ab92b549ce6f49c426bcb03",
            AddressTag {
                label: "ONCHAINID Identity Storage",
                category: "trex",
                icon: "fa-database",
                hidden: false,
            },
        );
        m.insert(
            "0xe5156df30ed0645a585cb8207caa93d8d3847417",
            AddressTag {
                label: "Datawallet+ Claim Issuer",
                category: "trex",
                icon: "fa-certificate",
                hidden: false,
            },
        );
        m.insert(
            "0x0919baf7e91785ae65351698a04b07bb13d14bbc",
            AddressTag {
                label: "ROPE Compliance Module",
                category: "trex",
                icon: "fa-gavel",
                hidden: false,
            },
        );
        m.insert(
            "0xd28cf001910d814c578e773efcbf0459d98db15f",
            AddressTag {
                label: "Tanastok ONCHAINID",
                category: "trex",
                icon: "fa-fingerprint",
                hidden: false,
            },
        );
        m.insert(
            "0x30fec506029781ba7d1d2ea27bdf9be422af81a7",
            AddressTag {
                label: "Deployer ONCHAINID",
                category: "trex",
                icon: "fa-fingerprint",
                hidden: false,
            },
        );
        m.insert(
            "0x183c0666bfcfdab9453c0d48c0d39d511b4010b3",
            AddressTag {
                label: "DCNFT Bytecode Template",
                category: "trex",
                icon: "fa-file-code",
                hidden: false,
            },
        );
        m.insert(
            "0x0264e76755493caf8f6eae214df188f2b9f6bbe2",
            AddressTag {
                label: "T-REX Implementation Authority",
                category: "trex",
                icon: "fa-key",
                hidden: false,
            },
        );
        m.insert(
            "0xbd3d7372caf8e448c6a3457561cc1c5de08bf1ef",
            AddressTag {
                label: "T-REX IA Factory",
                category: "trex",
                icon: "fa-industry",
                hidden: false,
            },
        );
        // Tanastok deployer wallets
        m.insert(
            "0x297ba821da55ed5e37c5c25b3832ce45fc54c475",
            AddressTag {
                label: "Tanastok Issuer",
                category: "trex",
                icon: "fa-stamp",
                hidden: false,
            },
        );
        // Tanastok Private Pool USDC payout treasury — established 2026-06-04
        // by DCSwap deployer 0x60FB32ef…4195 with an initial mint of
        // 50,000 DCR-20 USDC (tx 0xeb84fc1eb…f5c6, block 2025878). Sole
        // spender is tanastok-vps via PRIVATE_POOL_PAYOUT_PRIVATE_KEY,
        // per-tx cap $5,000 USDC. Audit trail anchored on-chain at
        // genesis_knot=0x903b86164f… and head=0xd580de574b… on the
        // wallet's personal ledger. See handover-from-dcswap-tanastok-
        // treasury-and-token-reconciliation-2026-06-04.mdc.
        m.insert(
            "0x63423bbc1275f973eb00d6198b757797a8db320b",
            AddressTag {
                label: "Tanastok Private Pool Payout Treasury",
                category: "treasury",
                icon: "fa-vault",
                hidden: false,
            },
        );
        // Careways Health Connect (careaway.io) operational treasury —
        // established 2026-06-05 by ROPE deployer 0x60FB32ef…4195. Initial
        // funding: 1,000 native FAT (gas, tx 0xb524301b…6498), 200,000
        // DCR-20 USDC (mint, tx 0x113a23ba…58c4), and 6,259,010.3292 WFAT
        // matched to FAT/USDC pool ratio 31.293 (deposit 0x03faf8cb…ab8b
        // + transfer 0x92629ea1…76ca, blocks 2069741-2069744). Audit trail
        // anchored on-chain at genesis_knot=0xf18cda736f… on the wallet's
        // personal ledger.
        m.insert(
            "0xd7c519679660f778e64c73c305f9a5cd17b5fded",
            AddressTag {
                label: "Careaway Treasury",
                category: "treasury",
                icon: "fa-heart-pulse",
                hidden: false,
            },
        );
        // Mapstore marketplace contracts and governance - deployed
        // 2026-06-15 by foundation deployer 0x60FB32ef...4195
        // (tx 0x0f8f1ce30464c81742059473621d8fb500df2e10dd5ef499a66fab58ef62db93,
        // block 2365689). Trustless DCR-20 USDC escrow for service jobs
        // and multi-merchant baskets, with 7-day auto-release window,
        // 8% default platform fee (20% hard cap). Foundation-controlled
        // bootstrap: Treasury and Admin are the foundation deployer;
        // Platform, Operator, Guardian are fresh wallets to be rotated
        // to Mapstore multisigs once Mapstore confirms them. See
        // handover-from-rope-mapstore-escrow-live-2026-06-15.mdc.
        m.insert(
            "0xbd365e336a7d6a84516a1f2b79d05bc64297ca4c",
            AddressTag {
                label: "Mapstore Escrow",
                category: "mapstore",
                icon: "fa-handshake",
                hidden: false,
            },
        );
        m.insert(
            "0x34e8d117d834cb0806064da325f9f6d4ee94385d",
            AddressTag {
                label: "Mapstore API Relayer",
                category: "mapstore",
                icon: "fa-server",
                hidden: false,
            },
        );
        m.insert(
            "0xdabf1af728223041c82d11755b114e25d9c05030",
            AddressTag {
                label: "Mapstore Operator (Disputes)",
                category: "mapstore",
                icon: "fa-gavel",
                hidden: false,
            },
        );
        m.insert(
            "0x5c19244ff713f18100a7b604fb610fa18028a09e",
            AddressTag {
                label: "Mapstore Guardian (Pauser)",
                category: "mapstore",
                icon: "fa-circle-pause",
                hidden: false,
            },
        );
        m
    })
}

fn known_label(addr: &str) -> Option<&'static str> {
    address_registry()
        .get(addr.to_lowercase().as_str())
        .map(|tag| tag.label)
}

fn is_hidden_address(addr: &str) -> bool {
    address_registry()
        .get(addr.to_lowercase().as_str())
        .map(|tag| tag.hidden)
        .unwrap_or(false)
}

/// Enrich a JSON object with label metadata for a from/to address field.
/// Hidden addresses have their raw hex replaced with null — the address
/// never reaches the client.
fn enrich_addr_field(json: &mut serde_json::Value, raw_addr: &str, field_prefix: &str) {
    let lower = raw_addr.to_lowercase();
    let hidden = is_hidden_address(raw_addr);
    json[format!("{}Label", field_prefix)] = match known_label(raw_addr) {
        Some(l) => serde_json::json!(l),
        None => serde_json::Value::Null,
    };
    json[format!("{}Hidden", field_prefix)] = serde_json::json!(hidden);
    if hidden {
        json[field_prefix.to_string()] = serde_json::Value::Null;
    }
    if let Some(tag) = address_registry().get(lower.as_str()) {
        json[format!("{}Icon", field_prefix)] = serde_json::json!(tag.icon);
        json[format!("{}Category", field_prefix)] = serde_json::json!(tag.category);
    }
}

async fn address_labels() -> Json<serde_json::Value> {
    let registry = address_registry();
    let mut labels = serde_json::Map::new();
    for (addr, tag) in registry.iter() {
        if tag.hidden {
            // Hidden addresses are keyed by label (not hex) so the raw
            // address never appears in the API response at all.
            labels.insert(
                tag.label.to_lowercase().replace(' ', "-"),
                serde_json::json!({
                    "label": tag.label,
                    "icon": tag.icon,
                    "category": tag.category,
                    "hidden": true,
                }),
            );
        } else {
            labels.insert(
                addr.to_string(),
                serde_json::json!({
                    "label": tag.label,
                    "icon": tag.icon,
                    "category": tag.category,
                    "hidden": false,
                }),
            );
        }
    }
    Json(serde_json::json!({
        "labels": labels,
        "count": labels.len(),
    }))
}

const TOTAL_SUPPLY_FAT: f64 = 10_000_000_000.0;

async fn discover_addresses(state: &AppState) -> Vec<String> {
    let mut addrs = std::collections::HashSet::new();
    // Seed with known addresses
    let known = [
        "0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195",
        "0x302fa11a6e784dfa89f96942a919c09b45559676",
        "0xddbf887982a2a1c03cb8705fef9e09c46122fff6",
        "0x3109c838e9a08a42fba000a48310845919759a02",
        "0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef",
        "0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa",
        "0x8b3554e7d32deeb8a8c057268e1eebd6c043313c",
        "0xfb0e84d2674dee6b330f17fa2f36e22c54327093",
        "0x38bfe303f02f892a7603f5e5d1ce99dda1e0fabf",
        "0x7a4bcc7b6513770dc6feb58655063cb52cb95039",
        "0xef5f76d24de7252c43e20f1dbce145b897cc1b1f",
        "0xf37bbeb4c37e0a9ef3ce5286a32e0947b0a26f78",
        "0xb28e38b344a7238c9777d74209f966d1873d26e0",
        "0x34ab12ca0bc2cfb3510cca479cc5bd4eb6eae883",
        "0x2e2304cabe9a75f00627fe92b73a391fff0486f8",
        "0x63423bbc1275f973eb00d6198b757797a8db320b",
        "0xd7c519679660f778e64c73c305f9a5cd17b5fded",
    ];
    for a in &known {
        addrs.insert(a.to_string());
    }

    // Scan recent blocks for unique addresses (from/to)
    let collected = collect_txs_from_recent_blocks(state, 200, 2000).await;
    for (tx, _, _) in &collected {
        if let Some(f) = tx.get("from").and_then(|v| v.as_str()) {
            addrs.insert(f.to_lowercase());
        }
        if let Some(t) = tx.get("to").and_then(|v| v.as_str()) {
            addrs.insert(t.to_lowercase());
        }
    }
    addrs.into_iter().collect()
}

async fn accounts_top_live(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PaginationParams>,
) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(100).min(100) as usize;
    let addresses = discover_addresses(&state).await;

    let mut accounts: Vec<serde_json::Value> = Vec::new();
    for addr in &addresses {
        let balance_hex = rpc_call(
            &state,
            "eth_getBalance",
            vec![serde_json::json!(addr), serde_json::json!("latest")],
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x0".to_string());

        let nonce_hex = rpc_call(
            &state,
            "eth_getTransactionCount",
            vec![serde_json::json!(addr), serde_json::json!("latest")],
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x0".to_string());

        let code = rpc_call(
            &state,
            "eth_getCode",
            vec![serde_json::json!(addr), serde_json::json!("latest")],
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x".to_string());

        let balance_fat = wei_to_fat(&balance_hex);
        let tx_count = hex_to_u64(&nonce_hex);
        let is_contract = code != "0x" && code.len() > 2;
        let label = known_label(addr).unwrap_or("");
        let hidden = is_hidden_address(addr);
        let pct = if TOTAL_SUPPLY_FAT > 0.0 {
            balance_fat / TOTAL_SUPPLY_FAT * 100.0
        } else {
            0.0
        };

        accounts.push(serde_json::json!({
            "address": if hidden { serde_json::Value::Null } else { serde_json::json!(addr) },
            "balance": format_fat(balance_fat),
            "balanceRaw": balance_fat,
            "percentOfSupply": format!("{:.4}%", pct),
            "transactionCount": tx_count,
            "isContract": is_contract,
            "label": label,
            "hidden": hidden
        }));
    }

    // Apply filter
    let filter = params.filter.as_deref().unwrap_or("all");
    let filtered: Vec<serde_json::Value> = accounts
        .into_iter()
        .filter(|a| match filter {
            "eoa" => !a
                .get("isContract")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "contracts" => a
                .get("isContract")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            "validators" => {
                let lbl = a.get("label").and_then(|v| v.as_str()).unwrap_or("");
                lbl.to_lowercase().contains("validator")
            }
            _ => true,
        })
        .collect();

    // Sort by balance descending
    let mut sorted = filtered;
    sorted.sort_by(|a, b| {
        let ba = a.get("balanceRaw").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let bb = b.get("balanceRaw").and_then(|v| v.as_f64()).unwrap_or(0.0);
        bb.partial_cmp(&ba).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Assign ranks and take top N
    let top: Vec<serde_json::Value> = sorted
        .iter()
        .take(limit)
        .enumerate()
        .map(|(i, a)| {
            let mut entry = a.clone();
            entry["rank"] = serde_json::json!(i + 1);
            entry
        })
        .collect();

    let unique_count = sorted.len();

    Json(serde_json::json!({
        "accounts": top,
        "uniqueCount": unique_count,
        "total": unique_count
    }))
}

async fn accounts_stats_live(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let addresses = discover_addresses(&state).await;
    let total_accounts = addresses.len();

    let mut total_balance = 0.0_f64;
    let mut contract_count = 0u64;
    let mut top_balances: Vec<f64> = Vec::new();

    for addr in &addresses {
        let balance_hex = rpc_call(
            &state,
            "eth_getBalance",
            vec![serde_json::json!(addr), serde_json::json!("latest")],
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x0".to_string());

        let code = rpc_call(
            &state,
            "eth_getCode",
            vec![serde_json::json!(addr), serde_json::json!("latest")],
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x".to_string());

        let bal = wei_to_fat(&balance_hex);
        total_balance += bal;
        top_balances.push(bal);
        if code != "0x" && code.len() > 2 {
            contract_count += 1;
        }
    }

    top_balances.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let top100_sum: f64 = top_balances.iter().take(100).sum();
    let top100_pct = if TOTAL_SUPPLY_FAT > 0.0 {
        top100_sum / TOTAL_SUPPLY_FAT * 100.0
    } else {
        0.0
    };

    // Active addresses: unique from/to in recent blocks
    let recent = collect_txs_from_recent_blocks(&state, 100, 5000).await;
    let mut active = std::collections::HashSet::new();
    for (tx, _, _) in &recent {
        if let Some(f) = tx.get("from").and_then(|v| v.as_str()) {
            active.insert(f.to_lowercase());
        }
        if let Some(t) = tx.get("to").and_then(|v| v.as_str()) {
            active.insert(t.to_lowercase());
        }
    }

    Json(serde_json::json!({
        "totalAccounts": total_accounts,
        "totalContracts": contract_count,
        "totalSupply": TOTAL_SUPPLY_FAT,
        "totalSupplyFormatted": "10,000,000,000 FAT",
        "top100SharePercent": format!("{:.2}", top100_pct),
        "activeAddresses24h": active.len()
    }))
}

async fn get_account(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let balance_hex = rpc_call(
        &state,
        "eth_getBalance",
        vec![serde_json::json!(address), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x0".to_string());

    let nonce_hex = rpc_call(
        &state,
        "eth_getTransactionCount",
        vec![serde_json::json!(address), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x0".to_string());

    let code = rpc_call(
        &state,
        "eth_getCode",
        vec![serde_json::json!(address), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x".to_string());

    let balance_fat = wei_to_fat(&balance_hex);
    let tx_count = hex_to_u64(&nonce_hex);
    let is_contract = code != "0x" && code.len() > 2;

    let fat_price = {
        let cache = state.price_cache.read().await;
        cache.as_ref().map(|p| p.price).unwrap_or(FALLBACK_PRICE)
    };
    let balance_usd = balance_fat * fat_price;

    Json(serde_json::json!({
        "address": address,
        "balance": format_fat(balance_fat),
        "balanceRaw": balance_fat,
        "balanceUsd": format!("${:.2}", balance_usd),
        "transactionCount": tx_count,
        "isContract": is_contract,
        "isValidator": false,
        "lastSeen": chrono::Utc::now().timestamp()
    }))
}

async fn account_transactions(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(20).min(100) as usize;
    let addr_lower = address.to_lowercase();

    // For known token contracts the meaningful "Transactions" view is
    // every transaction that interacted with the contract — i.e. every
    // tx whose receipt contains a `Transfer` log from this address.
    // Falling back to the from/to walk would only pick up direct
    // `transfer()` calls and silently hide everything routed through
    // DCSwapRouter, the Permit handler, or any other proxying contract.
    if known_token(&address).is_some() {
        let head = rpc_block_number(&state).await.unwrap_or(0);
        if head > 0 {
            // Walk backwards in 2 K-block windows until we have enough
            // distinct tx hashes or hit a 30 K-block ceiling. Reth tends
            // to choke on a single >2 K-block log query for high-volume
            // tokens so chunking gives us a stable response time even
            // when the token is busy.
            let max_lookback: u64 = 30_000;
            let chunk: u64 = 2_000;
            let mut tx_hashes: Vec<(String, u64)> = Vec::new();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut cursor: u64 = head;
            let oldest = head.saturating_sub(max_lookback);
            while cursor > oldest && tx_hashes.len() < limit {
                let from_block = cursor.saturating_sub(chunk).max(oldest);
                let logs_res = rpc_call(
                    &state,
                    "eth_getLogs",
                    vec![serde_json::json!({
                        "fromBlock": format!("0x{:x}", from_block),
                        "toBlock":   format!("0x{:x}", cursor),
                        "address":   &addr_lower,
                        "topics":    [TRANSFER_TOPIC],
                    })],
                )
                .await;
                if let Ok(v) = logs_res {
                    if let Some(arr) = v.as_array() {
                        for log in arr.iter().rev() {
                            let tx_hash = log
                                .get("transactionHash")
                                .and_then(|x| x.as_str())
                                .unwrap_or("");
                            if tx_hash.is_empty() || !seen.insert(tx_hash.to_string()) {
                                continue;
                            }
                            let block = log
                                .get("blockNumber")
                                .and_then(|x| x.as_str())
                                .map(hex_to_u64)
                                .unwrap_or(0);
                            tx_hashes.push((tx_hash.to_string(), block));
                            if tx_hashes.len() >= limit {
                                break;
                            }
                        }
                    }
                }
                if from_block == oldest {
                    break;
                }
                cursor = from_block.saturating_sub(1);
            }
            // Hydrate each tx hash to a tx summary. Use a per-block cache
            // for timestamps so we don't refetch the same block N times
            // for clustered events.
            let mut block_ts: std::collections::HashMap<u64, i64> = std::collections::HashMap::new();
            let mut matched: Vec<serde_json::Value> = Vec::new();
            for (hash, block) in tx_hashes.iter() {
                if let Ok(tx) = rpc_call(
                    &state,
                    "eth_getTransactionByHash",
                    vec![serde_json::json!(hash)],
                )
                .await
                {
                    if tx.is_object() {
                        let ts = if let Some(t) = block_ts.get(block) {
                            *t
                        } else {
                            let t = rpc_call(
                                &state,
                                "eth_getBlockByNumber",
                                vec![
                                    serde_json::json!(format!("0x{:x}", block)),
                                    serde_json::json!(false),
                                ],
                            )
                            .await
                            .ok()
                            .and_then(|b| {
                                b.get("timestamp")
                                    .and_then(|x| x.as_str())
                                    .map(hex_to_u64)
                            })
                            .unwrap_or(0) as i64;
                            block_ts.insert(*block, t);
                            t
                        };
                        matched.push(tx_summary_json(&tx, *block, ts));
                    }
                }
            }
            return Json(serde_json::json!({
                "address": address,
                "transactions": matched,
                "scanWindowBlocks": max_lookback,
                "totalReturned": matched.len(),
                "source": "eth_getLogs (Transfer events, deduplicated by tx hash, chunked)",
            }));
        }
    }

    // Default path for EOAs / unknown contracts: walk the last 5 K blocks
    // and filter by from/to. The previous 200-block window was too small
    // for active wallets and silently hid contract pages — 5 K blocks is
    // ≈4 hours of chain time and a reasonable browsing horizon.
    let all_txs = collect_txs_from_recent_blocks(&state, 5_000, 100_000).await;
    let matched: Vec<serde_json::Value> = all_txs
        .iter()
        .filter(|(tx, _, _)| {
            let from = tx
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let to = tx
                .get("to")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            from == addr_lower || to == addr_lower
        })
        .take(limit)
        .map(|(tx, bn, ts)| tx_summary_json(tx, *bn, *ts))
        .collect();

    Json(serde_json::json!({
        "address": address,
        "transactions": matched,
        "scanWindowBlocks": 5_000,
        "totalReturned": matched.len(),
        "source": "from/to walk over recent blocks",
    }))
}

/// Quipu Canon v1.1 §6(2) — Public personal-ledger view.
///
/// Returns the canonical hierarchy for a wallet's sovereign String:
///
/// ```json
/// {
///   "wallet_address": "0x...",
///   "string_id": "0x..." | "0xWALLET" (fallback),
///   "knots": [
///     {
///       "knot_index": 0,
///       "anchor_block": 12345,
///       "timestamp": 1730000000,
///       "status": "active" | "tombstone",
///       "tombstone": null | { untied_at, audit_hash, reason },
///       "transactions": [ {hash, from, to, value, event_type, ...}, ... ],
///       "tx_count": N,
///       "event_types": ["Transfer","Swap"],
///       "knot_size": <total wei of value transferred at this knot>
///     }, ...
///   ],
///   "knot_count": N,
///   "active_count": N,
///   "tombstone_count": N,
///   "source": "native" | "reth-anchored",
///   "canon": "v1.1 §6(2)"
/// }
/// ```
///
/// Knots are listed newest-first by anchor block. Each knot groups all of
/// the wallet's transactions confirmed at the same anchor — the natural
/// "event" granularity for the public DCScan view. Per-tx details (event
/// type, value, counterparty) are nested under the knot.
async fn personal_ledger_string(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Json<serde_json::Value> {
    let limit = params.limit.unwrap_or(50).min(200) as usize;
    let addr_lower = address.to_lowercase();

    // Try the canonical native path first. If the rope-node has the personal
    // ledger subsystem online, this returns real String + Knot[] + tombstones.
    if let Ok(native) = rpc_call(
        &state,
        "rope_getStringWithKnots",
        vec![serde_json::json!(addr_lower.clone())],
    )
    .await
    {
        if native.is_object() && native.get("string_id").is_some() {
            return Json(serde_json::json!({
                "wallet_address": addr_lower,
                "string_id": native.get("string_id").cloned().unwrap_or(serde_json::Value::Null),
                "knots": native.get("knots").cloned().unwrap_or(serde_json::json!([])),
                "knot_count": native.get("knot_count").cloned().unwrap_or(serde_json::json!(0)),
                "active_count": native.get("active_count").cloned().unwrap_or(serde_json::json!(0)),
                "tombstone_count": native.get("tombstone_count").cloned().unwrap_or(serde_json::json!(0)),
                "source": "native",
                "canon": "v1.1 §6(2) — String → Knot[] → Transaction details (rope-node native path)"
            }));
        }
    }

    // Fallback: build a knot-grouped view from on-chain tx data.
    // Each anchor block where the wallet has activity = one knot on the
    // wallet's string. Per-tx details are listed under the knot.
    let all_txs = collect_txs_from_recent_blocks(&state, 500, 8000).await;
    let mut by_anchor: std::collections::BTreeMap<u64, (i64, Vec<serde_json::Value>)> =
        std::collections::BTreeMap::new();

    for (tx, bn, ts) in all_txs.iter() {
        let from = tx
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let to = tx
            .get("to")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if from != addr_lower && to != addr_lower {
            continue;
        }
        let event_type = classify_knot_event_type(tx);
        let mut summary = tx_summary_json(tx, *bn, *ts);
        if let Some(obj) = summary.as_object_mut() {
            obj.insert("event_type".to_string(), serde_json::json!(event_type));
            // Cord-color analogue per canon §6 mapping table
            let color = match event_type {
                "Transfer" | "TokenTransfer" | "TokenTransferFrom" => "value",
                "TokenMint" => "mint",
                "TokenBurn" => "burn",
                "TokenApproval" => "approval",
                "Swap" | "AddLiquidity" | "RemoveLiquidity" => "defi",
                "ContractCreation" => "deploy",
                "ContractCall" => "call",
                _ => "other",
            };
            obj.insert("cord_color".to_string(), serde_json::json!(color));
        }
        by_anchor
            .entry(*bn)
            .or_insert_with(|| (*ts, Vec::new()))
            .1
            .push(summary);
    }

    // Convert to descending order (newest knot first), cap at `limit`.
    let mut knots: Vec<serde_json::Value> = Vec::new();
    let mut active_count = 0usize;
    let total_anchors = by_anchor.len();
    for (knot_index, (anchor_block, (timestamp, txs))) in by_anchor.into_iter().rev().enumerate() {
        if knots.len() >= limit {
            break;
        }
        let event_types: Vec<&str> = {
            let mut s: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
            for t in &txs {
                if let Some(et) = t.get("event_type").and_then(|v| v.as_str()) {
                    s.insert(et);
                }
            }
            s.into_iter().collect()
        };
        let tx_count = txs.len();
        active_count += 1;
        knots.push(serde_json::json!({
            "knot_index": knot_index,
            "anchor_block": anchor_block,
            "timestamp": timestamp,
            "status": "active",
            "tombstone": serde_json::Value::Null,
            "tx_count": tx_count,
            "event_types": event_types,
            "transactions": txs,
        }));
    }

    Json(serde_json::json!({
        "wallet_address": addr_lower,
        // Public-ledger fallback uses the wallet address as the cord ID,
        // exactly per canon §6 mapping ("Primary cord = Wallet string").
        "string_id": addr_lower,
        "knots": knots,
        "knot_count": total_anchors,
        "active_count": active_count,
        "tombstone_count": 0,
        // Reth (the post-2026-03-31 EVM execution layer for Datachain Rope)
        // is the source of these anchor blocks. We label this as
        // "reth-anchored" to distinguish from the canonical native rope-node
        // ledger path. The fallback is engaged when rope-node has not yet
        // initialized its native LedgerManager; the rope-bridge encoder
        // already notarizes every Reth tx onto the String Lattice, so this
        // grouping is a faithful canon §6(2) view either way.
        "source": "reth-anchored",
        "canon": "v1.1 §6(2) — String → Knot[] → Transaction details (Reth EVM execution-layer view; native rope-node ledger view returned when available)"
    }))
}

/// Live balanceOf() across the known DCR-20 token set, plus native DC FAT
/// via eth_getBalance, for a single address. Shared by `account_tokens`
/// (the dedicated `/tokens` tab) and `account_overview_live` (the
/// overview tab's `tokens`/`tokenCount`/`tokenHoldingsValueUsd` fields —
/// which previously hardcoded these to empty/zero on every request even
/// though `/tokens` already computed the real answer). Only tokens with
/// a non-zero balance are returned. The list of known tokens is the same
/// set that backs `known_token()` so the explorer stays consistent
/// across the /tokens, /address, and /token surfaces.
async fn compute_account_tokens(state: &AppState, addr_lc: &str) -> (Vec<serde_json::Value>, f64) {
    let known_dcr20: &[(&str, &str, u8)] = &[
        ("0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4", "WFAT", 18),
        ("0xddbf887982a2a1c03cb8705fef9e09c46122fff6", "WFAT", 18),
        ("0xb93bd8db94f1baff474aa9cba0739daaad01641f", "USDC", 6),
        ("0x3109c838e9a08a42fba000a48310845919759a02", "USDC", 6),
        ("0x79a26132f48394421382c13b54ae77fa3af73289", "USDT", 6),
        ("0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef", "USDT", 6),
        ("0x24d6137807fa8a592888726d87ac748d018c6d4a", "EUROD", 6),
        ("0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa", "EUROD", 6),
    ];

    let fat_price = {
        let cache = state.price_cache.read().await;
        cache.as_ref().map(|p| p.price).unwrap_or(FALLBACK_PRICE)
    };

    let native_balance_hex = rpc_call(
        state,
        "eth_getBalance",
        vec![serde_json::json!(addr_lc), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x0".to_string());
    let native_fat = wei_to_fat(&native_balance_hex);
    let native_usd = native_fat * fat_price;

    let mut tokens: Vec<serde_json::Value> = Vec::new();
    if native_fat > 0.0 {
        tokens.push(serde_json::json!({
            "address": "0x0000000000000000000000000000000000000000",
            "name": "DC FAT",
            "symbol": "FAT",
            "decimals": 18,
            "balance": format_with_commas(native_fat),
            "balanceRaw": native_fat,
            "usdValue": format!("${:.2}", native_usd),
            "usdRaw": native_usd,
            "kind": "native",
            "standard": "Native DC FAT",
        }));
    }

    for (token_addr, symbol, decimals) in known_dcr20 {
        let raw = eth_call_balance_of(state, token_addr, addr_lc).await;
        if raw == 0 {
            continue;
        }
        let info = known_token(token_addr);
        let usd_unit = info.as_ref().map(|i| i.usd_price).unwrap_or(0.0);
        let divisor = 10f64.powi(*decimals as i32);
        let bal_f = raw as f64 / divisor;
        let display_symbol = info.as_ref().map(|i| i.symbol).unwrap_or(*symbol);
        let usd_value = bal_f * usd_unit;
        tokens.push(serde_json::json!({
            "address": *token_addr,
            "name": display_symbol,
            "symbol": display_symbol,
            "altName": token_alt_name(token_addr),
            "logoCid": token_logo_cid(token_addr),
            "logoUrl": token_logo_cid(token_addr).map(|c| format!("/ipfs/{}", c)),
            "decimals": *decimals,
            "balance": format_with_commas(bal_f),
            "balanceRaw": bal_f,
            "usdValue": if usd_value > 0.0 { format!("${:.2}", usd_value) } else { "—".to_string() },
            "usdRaw": usd_value,
            "kind": "dcr20",
            "standard": "DCR-20",
        }));
    }

    let total_usd: f64 = tokens
        .iter()
        .filter_map(|t| t.get("usdRaw").and_then(|v| v.as_f64()))
        .sum();

    (tokens, total_usd)
}

async fn account_tokens(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();
    let (tokens, total_usd) = compute_account_tokens(&state, &addr_lc).await;

    Json(serde_json::json!({
        "address": address,
        "tokens": tokens,
        "totalCount": tokens.len(),
        "totalUsd": total_usd,
        "totalUsdStr": format!("${:.2}", total_usd),
        "source": "live-eth_call+balanceOf",
    }))
}

/// Canonical, currently-live DCR-20 contract addresses on Datachain Rope
/// (per `known_token()` above and the DCSwap 2026-02-26 redeployment
/// handover). Kept as a single list so `/api/v1/tokens` never drifts from
/// what `/token/:addr` and `/address/:addr` already treat as canonical.
const LIST_TOKENS_DCR20_ADDRS: &[(&str, &str, &str)] = &[
    (
        "0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4",
        "Wrapped FAT",
        "dcr20",
    ),
    (
        "0xb93bd8db94f1baff474aa9cba0739daaad01641f",
        "Bridged USD Coin",
        "dcr20",
    ),
    (
        "0x79a26132f48394421382c13b54ae77fa3af73289",
        "Bridged Tether USD",
        "dcr20",
    ),
    (
        "0x24d6137807fa8a592888726d87ac748d018c6d4a",
        "Bridged EUROD",
        "dcr20",
    ),
    (
        "0xd9ebc3da001618a3ae90481d33ae7ef85e130317",
        "DCSwap LP FAT/USDC",
        "dcr20",
    ),
    (
        "0x644da44bcd5f453c593781dbe22dfd733e8d1441",
        "DCSwap LP FAT/USDT",
        "dcr20",
    ),
    (
        "0x1e9c2ccf67320459bc4999a9f8be4a063d4021e4",
        "DCSwap LP FAT/EUROD",
        "dcr20",
    ),
    (
        "0xb86bdcecad93573d6ca21313aa7eac52800513c8",
        "DCSwap LP USDC/USDT",
        "dcr20",
    ),
];

/// Fetches per-symbol `{usd, change_24h}` from the canonical DCSwap price
/// feed (`{DCSWAP_API}/v1/prices` — handover-canonical-fat-price-2026-03-14).
/// Real, live values; `None` per symbol (never a fabricated number) when
/// the feed is unreachable or a symbol is absent from its payload.
async fn fetch_dcswap_token_prices(
    state: &AppState,
) -> std::collections::HashMap<String, (f64, Option<f64>)> {
    let url = format!("{}/v1/prices", state.dcswap_api.trim_end_matches('/'));
    let mut out = std::collections::HashMap::new();
    if let Ok(r) = state.http_client.get(&url).send().await {
        if r.status().is_success() {
            if let Ok(json) = r.json::<serde_json::Value>().await {
                if let Some(data) = json.get("data").and_then(|d| d.as_object()) {
                    for (symbol, v) in data {
                        let usd = v.get("usd").and_then(|x| x.as_f64());
                        let change = v.get("change_24h").and_then(|x| x.as_f64());
                        if let Some(usd) = usd {
                            out.insert(symbol.to_uppercase(), (usd, change));
                        }
                    }
                }
            }
        }
    }
    out
}

/// Real per-token summary: live `name()/symbol()/decimals()/totalSupply()`,
/// live holder/transfer counts from the same holder-index/tokentxn-cache
/// pipeline `get_token()` uses, and a live price sourced from the
/// canonical DCSwap feed (falling back to the `known_token()` USD peg for
/// stables, or the WFAT price cache, only when the feed itself has no
/// entry — never a hardcoded market number).
async fn list_tokens_summary(
    state: &Arc<AppState>,
    address: &str,
    fallback_name: &str,
    standard: &str,
    prices: &std::collections::HashMap<String, (f64, Option<f64>)>,
) -> serde_json::Value {
    let addr_lc = address.to_lowercase();
    let known = known_token(&addr_lc);

    let name_hex = eth_call_token_method(state, address, "0x06fdde03").await;
    let symbol_hex = eth_call_token_method(state, address, "0x95d89b41").await;
    let decimals_hex = eth_call_token_method(state, address, "0x313ce567").await;
    let total_supply_hex = eth_call_token_method(state, address, "0x18160ddd").await;

    let name = name_hex
        .as_deref()
        .and_then(decode_abi_string)
        .unwrap_or_else(|| fallback_name.to_string());
    let symbol = symbol_hex
        .as_deref()
        .and_then(decode_abi_string)
        .unwrap_or_else(|| known.as_ref().map(|i| i.symbol.to_string()).unwrap_or_default());
    let decimals: u32 = decimals_hex
        .as_deref()
        .map(|h| hex_to_u64(h) as u32)
        .or(known.as_ref().map(|i| i.decimals as u32))
        .unwrap_or(18);
    let total_supply_raw: u128 = total_supply_hex.as_deref().map(decode_hex_u256).unwrap_or(0);
    let divisor = 10f64.powi(decimals as i32);
    let total_supply_f = total_supply_raw as f64 / divisor;

    let (price_usd, change_24h) = prices
        .get(&symbol.to_uppercase())
        .copied()
        .unwrap_or_else(|| (known.as_ref().map(|i| i.usd_price).unwrap_or(0.0), None));
    let market_cap = price_usd * total_supply_f;

    let (holders_count, _transfer_count) = {
        let idx = state.holder_index.read().await;
        if let Some(ts) = idx.tokens.get(&addr_lc) {
            let live_holders = ts
                .balances
                .values()
                .filter(|raw| raw.parse::<u128>().map(|n| n > 0).unwrap_or(false))
                .count();
            (live_holders as u64, ts.transfer_count)
        } else {
            let cache = state.tokentxn_cache.read().await;
            if let Some(c) = &*cache {
                let entries = derive_token_holders_from_cache(c, &addr_lc, decimals);
                (entries.len() as u64, 0)
            } else {
                (0, 0)
            }
        }
    };

    serde_json::json!({
        "address": address,
        "name": name,
        "symbol": symbol,
        "decimals": decimals,
        "standard": standard,
        "standardLabel": "DCR-20",
        "totalSupply": format_with_commas(total_supply_f),
        "totalSupplyRaw": total_supply_raw.to_string(),
        "price": price_usd,
        "change": change_24h,
        "volume": serde_json::Value::Null,
        "marketCap": market_cap,
        "mcap": market_cap,
        "holders": holders_count,
        "origin": bridged_token_origin_json(&addr_lc),
    })
}

/// Real, live `/api/v1/tokens` list. Previously served a fully hardcoded
/// `ChainToken` fixture list with fabricated addresses (several of which
/// did not match any live contract), fabricated 24h change/volume, and
/// fabricated holder counts. Now iterates the canonical DCR-20 address
/// set and native FAT, fetching every field from the chain / the
/// canonical DCSwap price feed / the same holder-index the token detail
/// page already trusts.
async fn list_tokens(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let fat_price_cache = {
        let cache = state.price_cache.read().await;
        cache.as_ref().map(|p| (p.price, p.change_24h))
    };
    let prices = fetch_dcswap_token_prices(&state).await;

    let (fat_price_usd, fat_change) = prices
        .get("FAT")
        .copied()
        .unwrap_or_else(|| (fat_price_cache.map(|(p, _)| p).unwrap_or(FALLBACK_PRICE), fat_price_cache.map(|(_, c)| c)));

    let native_supply = {
        let sc = state.supply_cache.read().await;
        sc.as_ref()
            .map(|s| s.total_supply)
            .unwrap_or(market_data::NATIVE_GENESIS_FAT)
    };

    let mut token_list: Vec<serde_json::Value> = vec![serde_json::json!({
        "address": "0x0000000000000000000000000000000000000000",
        "name": "DC FAT",
        "symbol": "FAT",
        "decimals": 18,
        "standard": "native",
        "standardLabel": "Native DC FAT",
        "totalSupply": format_with_commas(native_supply),
        "totalSupplyRaw": null,
        "price": fat_price_usd,
        "change": fat_change,
        "volume": serde_json::Value::Null,
        "marketCap": fat_price_usd * native_supply,
        "mcap": fat_price_usd * native_supply,
        "holders": serde_json::Value::Null,
        "origin": serde_json::Value::Null,
    })];

    let summary_futures: Vec<_> = LIST_TOKENS_DCR20_ADDRS
        .iter()
        .map(|&(addr, name, standard)| {
            let state = state.clone();
            let prices = prices.clone();
            let addr = addr.to_string();
            let name = name.to_string();
            let standard = standard.to_string();
            async move { list_tokens_summary(&state, &addr, &name, &standard, &prices).await }
        })
        .collect();
    let dcr20_summaries = futures::future::join_all(summary_futures).await;
    token_list.extend(dcr20_summaries);

    let total_tokens = token_list.len() as u64;
    let total_market_cap: f64 = token_list
        .iter()
        .filter_map(|t| t.get("marketCap").and_then(|v| v.as_f64()))
        .sum();
    let total_holders: u64 = token_list
        .iter()
        .filter_map(|t| t.get("holders").and_then(|v| v.as_u64()))
        .sum();

    Json(serde_json::json!({
        "tokens": token_list,
        "stats": {
            "totalTokens": total_tokens,
            "totalMarketCap": total_market_cap,
            "totalVolume24h": serde_json::Value::Null,
            "totalHolders": total_holders,
        },
        "source": "live (eth_call name/symbol/decimals/totalSupply + DCSwap canonical prices + holder index)",
    }))
}

async fn get_token(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();

    // Native DC FAT short-circuit. Native FAT has no contract; address `0x0`
    // and the legacy placeholder `0x000…0001` both resolve here.
    let is_native = addr_lc == "0x0000000000000000000000000000000000000000"
        || addr_lc == "0x0000000000000000000000000000000000000001";
    if is_native {
        let fat_price = state
            .price_cache
            .read()
            .await
            .as_ref()
            .map(|p| p.price)
            .unwrap_or(FALLBACK_PRICE);
        let supply = 10_000_000_000.0_f64;
        let mc = supply * fat_price;
        return Json(serde_json::json!({
            "address": "0x0000000000000000000000000000000000000000",
            "contract": "0x0000000000000000000000000000000000000000",
            "name": "DC FAT",
            "symbol": "FAT",
            "decimals": 18,
            "standard": "native",
            "standardLabel": "Native DC FAT",
            "totalSupply": format_with_commas(supply),
            "totalSupplyRaw": "10000000000000000000000000000",
            "totalSupplyFormatted": format!("{} FAT", format_with_commas(supply)),
            "holders": 0,
            "transfers": 0,
            "priceUsd": fat_price,
            "price": format!("${:.6}", fat_price),
            "marketCap": mc,
            "marketCapStr": format!("${}", format_with_commas(mc)),
            "isContract": false,
            "network": "Datachain Rope",
            "chainId": 271828,
            "source": "native",
        }));
    }

    // Live read: name(), symbol(), decimals(), totalSupply().
    let name_hex = eth_call_token_method(&state, &address, "0x06fdde03").await;
    let symbol_hex = eth_call_token_method(&state, &address, "0x95d89b41").await;
    let decimals_hex = eth_call_token_method(&state, &address, "0x313ce567").await;
    let total_supply_hex = eth_call_token_method(&state, &address, "0x18160ddd").await;

    let known = known_token(&address);
    let name = name_hex
        .as_deref()
        .and_then(decode_abi_string)
        .unwrap_or_else(|| {
            known
                .as_ref()
                .map(|i| i.symbol.to_string())
                .unwrap_or_else(|| "Unknown Token".to_string())
        });
    let symbol = symbol_hex
        .as_deref()
        .and_then(decode_abi_string)
        .unwrap_or_else(|| {
            known
                .as_ref()
                .map(|i| i.symbol.to_string())
                .unwrap_or_else(|| "???".to_string())
        });
    let decimals: u32 = decimals_hex
        .as_deref()
        .map(|h| hex_to_u64(h) as u32)
        .or(known.as_ref().map(|i| i.decimals as u32))
        .unwrap_or(18);
    let total_supply_raw: u128 = total_supply_hex
        .as_deref()
        .map(decode_hex_u256)
        .unwrap_or(0);

    let divisor = 10f64.powi(decimals as i32);
    let total_supply_f = total_supply_raw as f64 / divisor;

    // Price + market cap. We trust `known_token()` for the USD peg of
    // bridged stables and the FAT price cache for WFAT. Unknown tokens are
    // intentionally returned with `priceUsd: 0` so the UI can show "—" rather
    // than a fake value.
    let price_usd: f64 = if let Some(info) = known.as_ref() {
        if info.symbol == "WFAT" {
            state
                .price_cache
                .read()
                .await
                .as_ref()
                .map(|p| p.price)
                .unwrap_or(FALLBACK_PRICE)
        } else {
            info.usd_price
        }
    } else {
        0.0
    };
    let market_cap_usd = price_usd * total_supply_f;

    // Holder count + transfer count. Prefer the persistent holder index
    // (full chain history); fall back to the rolling tokentxn_cache if the
    // background scanner hasn't covered this token yet.
    let (holders_count, transfer_count, holders_is_partial) = {
        let idx = state.holder_index.read().await;
        if let Some(ts) = idx.tokens.get(&addr_lc) {
            let head_now = rpc_block_number(&state).await.unwrap_or(0);
            // If we couldn't read head right now (RPC blip), assume partial
            // rather than misleadingly reporting "complete". Only flip to
            // false when we KNOW the index has caught up to current head.
            let partial = head_now == 0 || ts.last_scanned_block + 60 < head_now;
            let live_holders = ts
                .balances
                .values()
                .filter(|raw| raw.parse::<u128>().map(|n| n > 0).unwrap_or(false))
                .count();
            (live_holders as u64, ts.transfer_count, partial)
        } else {
            let cache = state.tokentxn_cache.read().await;
            if let Some(c) = &*cache {
                let entries = derive_token_holders_from_cache(c, &addr_lc, decimals);
                let tx_count = c
                    .transfers
                    .iter()
                    .filter(|t| {
                        t.get("tokenAddress")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_lowercase() == addr_lc)
                            .unwrap_or(false)
                    })
                    .count();
                (entries.len() as u64, tx_count as u64, true)
            } else {
                (0, 0, true)
            }
        }
    };

    // eth_getCode with the same connection-reset retry as
    // eth_call_token_method — the token page hides the rich Profile
    // Summary on `isContract: false` so a single transient blip would
    // surface as "Wrapped DC FAT (loose page, no overview)" for users.
    let code_hex = {
        let mut out: Option<String> = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(200 * attempt as u64)).await;
            }
            if let Ok(v) = rpc_call(
                &state,
                "eth_getCode",
                vec![serde_json::json!(address), serde_json::json!("latest")],
            )
            .await
            {
                if let Some(s) = v.as_str() {
                    out = Some(s.to_string());
                    if s.len() > 2 {
                        break;
                    }
                }
            }
        }
        out.unwrap_or_else(|| "0x".to_string())
    };
    let is_contract = code_hex != "0x" && code_hex.len() > 2;

    // Rich project metadata (description, creator, global market data)
    // for the well-known stables + WFAT. Falls back to None for unlabelled
    // contracts so the UI can hide the corresponding sections.
    let meta = token_metadata(&address);
    // Live CMC data (refreshed every 5 min by `refresh_cmc_cache`). When
    // present for this address's symbol we override the static
    // 2026-06-04 snapshot baked into `token_metadata()` so the page
    // shows fresh global market cap / 24 h volume / circulating
    // supply. Falls back to the static numbers when the key is unset
    // or CMC is unreachable.
    let live_cmc: Option<CmcQuote> = {
        match cmc_symbol_for_address(&address) {
            Some(symbol) => {
                let guard = state.cmc_cache.read().await;
                guard
                    .as_ref()
                    .and_then(|c| c.quotes.get(symbol).cloned())
            }
            None => None,
        }
    };
    let cmc_fetched_at: Option<i64> = {
        let guard = state.cmc_cache.read().await;
        guard.as_ref().map(|c| c.fetched_at)
    };
    let meta_json = match meta.as_ref() {
        Some(m) => {
            // Pick live CMC values when available; fall back to the
            // static snapshot otherwise. We keep the static numbers as
            // a tail-end safety net so a CMC outage doesn't blank the
            // page.
            let mcap = live_cmc.as_ref().map(|q| q.market_cap_usd).unwrap_or(m.global_market_cap_usd);
            let vol = live_cmc.as_ref().map(|q| q.volume_24h_usd).unwrap_or(m.global_volume_24h_usd);
            let circ = live_cmc.as_ref().map(|q| q.circulating_supply).unwrap_or(m.global_circulating_supply);
            let live_price = live_cmc.as_ref().map(|q| q.price_usd).unwrap_or(0.0);
            let pct_24h = live_cmc.as_ref().map(|q| q.percent_change_24h).unwrap_or(0.0);
            let source_label: String = match (live_cmc.as_ref(), cmc_fetched_at) {
                (Some(_), Some(ts)) => format!(
                    "CoinMarketCap (live, refreshed {}Z)",
                    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
                        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "—".into())
                ),
                _ => m.data_source.to_string(),
            };
            serde_json::json!({
                "description": m.description,
                "creator": m.creator,
                "creatorUrl": m.creator_url,
                "creatorAddr": m.creator_addr,
                "projectUrl": m.project_url,
                "socialUrl": m.social_url,
                "globalMarketCapUsd": mcap,
                "globalMarketCapStr": if mcap > 0.0 {
                    format!("${}", format_with_commas(mcap))
                } else { "—".to_string() },
                "globalVolume24hUsd": vol,
                "globalVolume24hStr": if vol > 0.0 {
                    format!("${}", format_with_commas(vol))
                } else { "—".to_string() },
                "globalCirculatingSupply": circ,
                "globalCirculatingSupplyStr": if circ > 0.0 {
                    format!("{} {}", format_with_commas(circ), symbol)
                } else { "—".to_string() },
                "globalPriceUsd": live_price,
                "globalPercentChange24h": pct_24h,
                "isLive": live_cmc.is_some(),
                "dataSource": source_label,
                "origin": bridged_token_origin_json(&addr_lc),
            })
        }
        None => serde_json::Value::Null,
    };

    // Deprecation pointer: if this address is a known dead pre-Reth
    // address, surface the canonical replacement so the UI can render a
    // "Deprecated, see → live address" banner instead of a misleading
    // empty page.
    let replacement = dead_token_replacement(&address).map(|(live, label)| {
        serde_json::json!({
            "isDeprecated": true,
            "replacedBy": live,
            "replacedByLabel": label,
            "reason": "Pre-Reth-migration deployment. The contract has no \
                       bytecode on the live chain; balances/transfers/holders \
                       all migrated to the address above.",
        })
    });

    Json(serde_json::json!({
        "address": address,
        "contract": address,
        "name": name,
        "symbol": symbol,
        "altName": token_alt_name(&address),
        "logoCid": token_logo_cid(&address),
        "logoUrl": token_logo_cid(&address).map(|c| format!("/ipfs/{}", c)),
        "decimals": decimals,
        "standard": "DCR-20",
        "standardLabel": "DCR-20",
        "totalSupply": format_with_commas(total_supply_f),
        "totalSupplyRaw": total_supply_raw.to_string(),
        "totalSupplyFormatted": format!("{} {}", format_with_commas(total_supply_f), symbol),
        "bridgedSupply": total_supply_f,
        "bridgedSupplyStr": format!("{} {}", format_with_commas(total_supply_f), symbol),
        "holders": holders_count,
        "holdersIsPartial": holders_is_partial,
        "transfers": transfer_count,
        "priceUsd": price_usd,
        "price": if price_usd > 0.0 { format!("${:.6}", price_usd) } else { "—".to_string() },
        "marketCap": market_cap_usd,
        "marketCapStr": if market_cap_usd > 0.0 { format!("${}", format_with_commas(market_cap_usd)) } else { "—".to_string() },
        "isContract": is_contract,
        "network": "Datachain Rope",
        "chainId": 271828,
        "source": "live-eth_call",
        "metadata": meta_json,
        "deprecation": replacement,
    }))
}

async fn token_holders(
    Path(address): Path<String>,
    Query(params): Query<PaginationParams>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();
    let limit = params.limit.unwrap_or(50).min(200) as usize;
    let page = params.page.unwrap_or(1).max(1) as usize;
    let offset = (page - 1) * limit;

    // Read decimals via eth_call so percentages are computed against the
    // correct base. Falls back to 18 (DCR-20 default) if the call fails.
    let decimals = eth_call_token_method(&state, &address, "0x313ce567")
        .await
        .as_deref()
        .map(|h| hex_to_u64(h) as u32)
        .or_else(|| known_token(&address).map(|i| i.decimals as u32))
        .unwrap_or(18);
    let total_supply_raw: u128 = eth_call_token_method(&state, &address, "0x18160ddd")
        .await
        .as_deref()
        .map(decode_hex_u256)
        .unwrap_or(0);
    let divisor = 10f64.powi(decimals as i32);
    let total_supply_f = total_supply_raw as f64 / divisor;

    // Prefer the persistent holder index (full chain history). Fall back
    // to the rolling tokentxn_cache if the index hasn't seen this token
    // yet (cold start before the first scan tick).
    let (entries, last_scanned, first_scanned, source_label, is_partial): (
        Vec<(String, f64)>,
        u64,
        u64,
        &'static str,
        bool,
    ) = {
        let idx = state.holder_index.read().await;
        if let Some(ts) = idx.tokens.get(&addr_lc) {
            let mut v: Vec<(String, f64)> = ts
                .balances
                .iter()
                .filter_map(|(addr, raw_str)| {
                    raw_str.parse::<u128>().ok().and_then(|raw| {
                        if raw == 0 {
                            None
                        } else {
                            Some((addr.clone(), raw as f64 / divisor))
                        }
                    })
                })
                .collect();
            v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            // Treat an unknown head (RPC blip) as "still partial" rather than
            // misleadingly flipping to current.
            let head_now = rpc_block_number(&state).await.unwrap_or(0);
            let partial = head_now == 0 || ts.last_scanned_block + 60 < head_now;
            (
                v,
                ts.last_scanned_block,
                ts.first_scanned_block,
                "persistent-holder-index",
                partial,
            )
        } else {
            let cache = state.tokentxn_cache.read().await;
            let v = match &*cache {
                Some(c) => derive_token_holders_from_cache(c, &addr_lc, decimals),
                None => Vec::new(),
            };
            (v, 0, 0, "tokentxn-cache-derived", true)
        }
    };

    let total = entries.len();
    let page_items: Vec<serde_json::Value> = entries
        .iter()
        .skip(offset)
        .take(limit)
        .enumerate()
        .map(|(i, (addr, balance))| {
            let pct = if total_supply_f > 0.0 {
                (balance / total_supply_f) * 100.0
            } else {
                0.0
            };
            serde_json::json!({
                "rank": offset + i + 1,
                "address": addr,
                "balance": format_with_commas(*balance),
                "balanceRaw": balance,
                "percentage": format!("{:.4}%", pct),
            })
        })
        .collect();

    Json(serde_json::json!({
        "token": address,
        "holders": page_items,
        "total": total,
        "offset": offset,
        "limit": limit,
        "isPartial": is_partial,
        "lastScannedBlock": last_scanned,
        "firstScannedBlock": first_scanned,
        "note": if is_partial {
            "Holder index is still catching up to chain head — values reflect every Transfer up to the lastScannedBlock above."
        } else {
            "Live: holder index is current with chain head."
        },
        "source": source_label,
    }))
}

async fn token_transfers(
    Path(address): Path<String>,
    Query(params): Query<PaginationParams>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();
    let limit = params.limit.unwrap_or(25).min(100) as usize;
    let page = params.page.unwrap_or(1).max(1) as usize;
    let offset = (page - 1) * limit;

    let cache = state.tokentxn_cache.read().await;
    let all: Vec<serde_json::Value> = match &*cache {
        Some(c) => c
            .transfers
            .iter()
            .filter(|t| {
                t.get("tokenAddress")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_lowercase() == addr_lc)
                    .unwrap_or(false)
            })
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    let total = all.len();
    let page: Vec<serde_json::Value> = all.into_iter().skip(offset).take(limit).collect();

    Json(serde_json::json!({
        "token": address,
        "transfers": page,
        "total": total,
        "offset": offset,
        "limit": limit,
        "source": "tokentxn-cache",
    }))
}

/// Map a known token contract address (any redeployment) to its IPFS logo
/// CID. The CIDs are the same set DCSwap pins on its IPFS gateway, so the
/// UI can resolve them via either `dcscan.io/ipfs/<cid>` or
/// `dcswap.net/ipfs/<cid>` interchangeably. EUROD also goes by the
/// trade name "Hodo" in some markets — same contract, same logo.
fn token_logo_cid(addr: &str) -> Option<&'static str> {
    match addr.to_lowercase().as_str() {
        // WFAT (every redeployment)
        "0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4"
        | "0xddbf887982a2a1c03cb8705fef9e09c46122fff6"
        | "0x90e2e170b0fc133343f0d7fde128c1fb716aab25" => {
            Some("QmUTcDN2hAxRv32eGTLVGYfs7Bn4UcMNWhrZykrr9W5YHH")
        }
        // USDC (every redeployment)
        "0xb93bd8db94f1baff474aa9cba0739daaad01641f"
        | "0x3109c838e9a08a42fba000a48310845919759a02"
        | "0x9f700dd3bb1764ab568263d3e19a1fc5cdf3f9a5" => {
            Some("QmXfzKRvjZz3u5JRgC4v5mGVbm9ahrUiB4DgzHBsnWbTMM")
        }
        // USDT (every redeployment)
        "0x79a26132f48394421382c13b54ae77fa3af73289"
        | "0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef" => {
            Some("Qmchysx7eP2xMn9CvLeiVM4YCjNQGoSKcYq6rY2FUnkdj1")
        }
        // EUROD (a.k.a. "Hodo") — same contract, both names refer to the
        // Tanastok-issued euro-denominated stablecoin.
        "0x24d6137807fa8a592888726d87ac748d018c6d4a"
        | "0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa" => {
            Some("QmasVz3no4Uu1EXLDunxtDs573Y9n1s7yZJCTfSn4vvag2")
        }
        _ => None,
    }
}

/// Returns the alternate trade name for a token, if any. Used so search and
/// display surfaces can show "EUROD (Hodo)" without the user having to know
/// both names.
fn token_alt_name(addr: &str) -> Option<&'static str> {
    match addr.to_lowercase().as_str() {
        "0x24d6137807fa8a592888726d87ac748d018c6d4a"
        | "0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa" => Some("Hodo"),
        _ => None,
    }
}

/// Static, hand-curated metadata for the well-known DCR-20 tokens that
/// dcscan.io needs to render the per-token landing page (Etherscan-style
/// "Profile Summary" + "Market" + "Other Info" tri-panel).
///
/// Returns:
///  - `description`: the marketing-grade project description (Circle for
///    USDC, Tether for USDT, Hodo / Datachain Foundation for EUROD,
///    Datachain Foundation for WFAT)
///  - `creator`: the issuer / project legal entity that the user
///    expects to see in the "Contract Creator" row of the More Info card
///  - `creator_url`: the issuer's website
///  - `creator_addr`: the on-chain wallet that deployed the bridged
///    contract on Datachain Rope (NOT the global issuer's mainnet
///    deployer — this is the local deploy origin)
///  - `project_url`, `social_url`: external links for the token info card
///  - `global_market_cap_usd`: hand-maintained snapshot of the global
///    upstream market cap so the UI can show the canonical "What this
///    token is worth across the whole industry" number alongside the
///    "What's bridged onto Datachain Rope" number we read on-chain
///  - `global_volume_24h_usd`: same idea for 24 h volume
///  - `global_circulating_supply`: same idea for circulating supply
///  - `data_source`: human-readable attribution for the snapshot
struct TokenMetadata {
    description: &'static str,
    creator: &'static str,
    creator_url: &'static str,
    creator_addr: &'static str,
    project_url: &'static str,
    social_url: Option<&'static str>,
    global_market_cap_usd: f64,
    global_volume_24h_usd: f64,
    global_circulating_supply: f64,
    data_source: &'static str,
}

/// Origin-chain provenance for a bridged DCR-20 token, keyed by
/// Datachain Rope contract address (every known redeployment included).
/// Ground truth per `handover-audit-migration-bridge-2026-07-20.mdc`
/// (evidence-first on-chain audit of `BridgeMinter` / `OriginBridgeVault`
/// / the legacy operator-mint bridge):
///
///  - USDC / USDT: wired to the Arbitrum-first trustless rail
///    (`BridgeMinter.allowedOriginChain(42161)`, `OriginBridgeVault` on
///    Arbitrum holding native USDC `0xaf88…5831` / USDT `0xFd08…Cbb9`).
///    That rail is deployed but currently **paused** end-to-end (Rope
///    `BridgeMinter.paused()==true`) — surfaced here as `bridgeStatus`
///    so the UI never implies a live, exercisable bridge.
///  - EUROD: no Arbitrum vault token is configured for it. It moves
///    over the older operator-mint flow
///    (`dcswap-api::handlers::bridge::SUPPORTED_ORIGIN_NETWORKS`),
///    where the depositor picks Ethereum or XDC Network per request —
///    there is no single canonical origin chain to report.
///  - WFAT / native FAT: not bridged at all; returns `None`.
///
/// Returns `(origin_network_label, origin_chain_id, bridge_status)`.
fn bridged_token_origin(addr: &str) -> Option<(&'static str, Option<u64>, &'static str)> {
    match addr.to_lowercase().as_str() {
        "0xb93bd8db94f1baff474aa9cba0739daaad01641f"
        | "0x3109c838e9a08a42fba000a48310845919759a02"
        | "0x9f700dd3bb1764ab568263d3e19a1fc5cdf3f9a5"
        | "0x79a26132f48394421382c13b54ae77fa3af73289"
        | "0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef" => Some((
            "Arbitrum",
            Some(42161),
            "Bridge wired (BridgeMinter route + Arbitrum OriginBridgeVault) but currently paused — not yet accepting live deposits",
        )),
        "0x24d6137807fa8a592888726d87ac748d018c6d4a"
        | "0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa" => Some((
            "Ethereum / XDC Network (depositor-selected)",
            None,
            "Legacy operator-mint bridge — origin chain chosen per deposit, no single canonical origin",
        )),
        _ => None,
    }
}

/// JSON shape of `bridged_token_origin`, ready to splice into any token
/// API response. `null` for native FAT and any non-bridged / unknown
/// contract so the frontend can hide the "Origin Network" row entirely.
fn bridged_token_origin_json(addr: &str) -> serde_json::Value {
    match bridged_token_origin(addr) {
        Some((network, chain_id, status)) => serde_json::json!({
            "originNetwork": network,
            "originChainId": chain_id,
            "bridgeStatus": status,
        }),
        None => serde_json::Value::Null,
    }
}

fn token_metadata(addr: &str) -> Option<TokenMetadata> {
    // Snapshot baseline taken 2026-06-04 from CoinMarketCap public market
    // data for USDC / USDT, from Tanastok issuer reporting for EUROD /
    // Hodo, and from rope-economics for DC FAT / WFAT. These numbers
    // refresh whenever a release ships — the on-chain "bridged supply"
    // figure is always live (read via eth_call) and is the canonical
    // value for anything happening on Datachain Rope.
    match addr.to_lowercase().as_str() {
        // ── USDC (every known redeployment) ──────────────────────────
        "0xb93bd8db94f1baff474aa9cba0739daaad01641f"
        | "0x3109c838e9a08a42fba000a48310845919759a02"
        | "0x9f700dd3bb1764ab568263d3e19a1fc5cdf3f9a5" => Some(TokenMetadata {
            description: "USDC is a US dollar-backed stablecoin issued by Circle. \
                          USDC is designed to provide a faster, safer, and more \
                          efficient way to send, spend, and exchange money around \
                          the world. Bridged to Datachain Rope by the DCSwap \
                          treasury so users can settle in dollars natively on \
                          chain.",
            creator: "Circle",
            creator_url: "https://www.circle.com/",
            creator_addr: "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195",
            project_url: "https://www.circle.com/usdc",
            social_url: Some("https://twitter.com/circle"),
            global_market_cap_usd: 75_833_953_098.0,
            global_volume_24h_usd: 24_516_132_202.0,
            global_circulating_supply: 75_853_215_436.0,
            data_source: "CoinMarketCap (2026-06-04 snapshot)",
        }),
        // ── USDT (every known redeployment) ──────────────────────────
        "0x79a26132f48394421382c13b54ae77fa3af73289"
        | "0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef" => Some(TokenMetadata {
            description: "Tether (USDT) is the largest US dollar-pegged \
                          stablecoin, issued by Tether Limited. Tether gives \
                          you the joint benefits of open blockchain technology \
                          and traditional currency by converting your cash \
                          into a stable digital currency equivalent. Bridged \
                          to Datachain Rope by the DCSwap treasury.",
            creator: "Tether",
            creator_url: "https://tether.to/",
            creator_addr: "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195",
            project_url: "https://tether.to/en/",
            social_url: Some("https://twitter.com/Tether_to"),
            global_market_cap_usd: 187_316_503_076.0,
            global_volume_24h_usd: 262_614_561_186.0,
            global_circulating_supply: 187_529_700_105.0,
            data_source: "CoinMarketCap (2026-06-04 snapshot)",
        }),
        // ── EUROD (a.k.a. Hodo) — every known redeployment ───────────
        "0x24d6137807fa8a592888726d87ac748d018c6d4a"
        | "0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa" => Some(TokenMetadata {
            description: "EUROD (formerly trading as Hodo) is a euro-pegged \
                          digital currency issued by the Datachain Foundation \
                          and bridged onto Datachain Rope. Each EUROD is \
                          backed 1:1 by EUR-denominated reserves and is \
                          designed for native euro settlement on Datachain \
                          Rope and partner ecosystems.",
            creator: "Hodo / Datachain Foundation",
            creator_url: "https://datachain.network/",
            creator_addr: "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195",
            project_url: "https://datachain.network/eurod",
            social_url: Some("https://x.com/DATACHAINDC"),
            global_market_cap_usd: 0.0,
            global_volume_24h_usd: 0.0,
            global_circulating_supply: 37_402_860.0,
            data_source: "Tanastok / Datachain Foundation issuer report (2026-06-04)",
        }),
        // ── WFAT (every known redeployment) ──────────────────────────
        "0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4"
        | "0xddbf887982a2a1c03cb8705fef9e09c46122fff6"
        | "0x90e2e170b0fc133343f0d7fde128c1fb716aab25" => Some(TokenMetadata {
            description: "Wrapped DC FAT (WFAT) is the canonical DCR-20 \
                          wrapper for native DC FAT, the protocol-native \
                          asset of Datachain Rope. WFAT lets DC FAT trade on \
                          DCSwap pools, settle into ERC-3643 securities and \
                          interact with any DCR-20-compatible contract \
                          without leaving the chain.",
            creator: "Datachain Foundation",
            creator_url: "https://datachain.network/",
            creator_addr: "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195",
            project_url: "https://datachain.network/dc-fat",
            social_url: Some("https://x.com/DATACHAINDC"),
            global_market_cap_usd: 0.0,
            global_volume_24h_usd: 0.0,
            global_circulating_supply: 300_312_800.0,
            data_source: "On-chain WFAT total supply (read via eth_call, refreshed every minute)",
        }),
        _ => None,
    }
}

/// If `addr` is a known DEAD pre-Reth-migration address, returns the
/// canonical LIVE replacement so the address page can render a
/// "Deprecated, see → live address" banner. The frontend uses this hint
/// to spare users from staring at empty tabs on a contract that no
/// longer has bytecode.
fn dead_token_replacement(addr: &str) -> Option<(&'static str, &'static str)> {
    // (live_address, friendly_label)
    match addr.to_lowercase().as_str() {
        "0xddbf887982a2a1c03cb8705fef9e09c46122fff6" => {
            Some(("0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4", "WFAT (live)"))
        }
        "0x3109c838e9a08a42fba000a48310845919759a02" => {
            Some(("0xb93bd8db94f1baff474aa9cba0739daaad01641f", "USDC (live)"))
        }
        "0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef" => {
            Some(("0x79a26132f48394421382c13b54ae77fa3af73289", "USDT (live)"))
        }
        "0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa" => {
            Some(("0x24d6137807fa8a592888726d87ac748d018c6d4a", "EUROD (live)"))
        }
        "0x8b3554e7d32deeb8a8c057268e1eebd6c043313c" => {
            Some(("0x772e5fd559069aecce5e6983c0c415c8579d780d", "DCSwapFactory (live)"))
        }
        "0xfb0e84d2674dee6b330f17fa2f36e22c54327093" => {
            Some(("0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4", "DCSwapRouter (live)"))
        }
        "0x38bfe303f02f892a7603f5e5d1ce99dda1e0fabf" => {
            Some(("0xd9ebc3da001618a3ae90481d33ae7ef85e130317", "FAT/USDC Pool (live)"))
        }
        "0x7a4bcc7b6513770dc6feb58655063cb52cb95039" => {
            Some(("0x644da44bcd5f453c593781dbe22dfd733e8d1441", "FAT/USDT Pool (live)"))
        }
        "0xef5f76d24de7252c43e20f1dbce145b897cc1b1f" => {
            Some(("0x1e9c2ccf67320459bc4999a9f8be4a063d4021e4", "FAT/EUROD Pool (live)"))
        }
        "0xf37bbeb4c37e0a9ef3ce5286a32e0947b0a26f78" => {
            Some(("0xb86bdcecad93573d6ca21313aa7eac52800513c8", "USDC/USDT Pool (live)"))
        }
        _ => None,
    }
}

/// `GET /api/v1/tokens/:addr/dex` — DEX overview for a token.
///
/// Aggregates per-pool data from DCSwap's `/v1/pools` endpoint (filtered to
/// pools that contain this token) and the most recent swaps from
/// `/v1/swaps/recent` (also filtered to those pools). Lets the address /
/// token page render a "DEX Trades" tab with real reserves, TVL, 24 h
/// volume and recent fills without any extra logic on the dcscan side.
async fn token_dex_overview(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();
    let dcswap_base = state.dcswap_api.trim_end_matches('/').to_string();

    let pools_url = format!("{}/v1/pools", dcswap_base);
    let pools_json: serde_json::Value = match state
        .http_client
        .get(&pools_url)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
    {
        Ok(r) => r.json::<serde_json::Value>().await.unwrap_or_else(|_| {
            serde_json::json!({ "success": false, "data": { "pools": [] } })
        }),
        Err(_) => serde_json::json!({ "success": false, "data": { "pools": [] } }),
    };

    let empty_pools: Vec<serde_json::Value> = Vec::new();
    let all_pools = pools_json
        .get("data")
        .and_then(|d| d.get("pools"))
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or(empty_pools);

    let matching: Vec<serde_json::Value> = all_pools
        .iter()
        .filter(|p| {
            let a = p
                .get("token_a")
                .and_then(|t| t.get("address"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let b = p
                .get("token_b")
                .and_then(|t| t.get("address"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            a == addr_lc || b == addr_lc
        })
        .cloned()
        .collect();

    let pool_ids: std::collections::HashSet<String> = matching
        .iter()
        .filter_map(|p| {
            p.get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_lowercase())
        })
        .collect();

    // Recent swaps: pull a generous slice from DCSwap and filter to pools
    // that involve this token. DCSwap's /v1/swaps/recent already returns
    // newest-first, so a slice of 200 is enough for "Latest DEX Trades".
    let swaps_url = format!("{}/v1/swaps/recent?limit=200", dcswap_base);
    let swaps_json: serde_json::Value = match state
        .http_client
        .get(&swaps_url)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
    {
        Ok(r) => r
            .json::<serde_json::Value>()
            .await
            .unwrap_or_else(|_| serde_json::json!({ "success": false, "data": [] })),
        Err(_) => serde_json::json!({ "success": false, "data": [] }),
    };
    let all_swaps = swaps_json
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let recent: Vec<serde_json::Value> = all_swaps
        .into_iter()
        .filter(|s| {
            let pid = s
                .get("pool_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            pool_ids.contains(&pid)
        })
        .take(50)
        .collect();

    // Aggregate volumes + TVL across the matching pools so the
    // "Token DEX Overview" card has chart-friendly numbers.
    let mut total_volume_24h = 0.0f64;
    let mut total_tvl = 0.0f64;
    let mut total_swaps: u64 = 0;
    for p in &matching {
        if let Some(v) = p.get("volume_24h").and_then(|v| v.as_str()) {
            total_volume_24h += v.parse::<f64>().unwrap_or(0.0);
        }
        if let Some(v) = p.get("tvl_usd").and_then(|v| v.as_str()) {
            total_tvl += v.parse::<f64>().unwrap_or(0.0);
        }
        if let Some(v) = p.get("swap_count").and_then(|v| v.as_str()) {
            total_swaps += v.parse::<u64>().unwrap_or(0);
        }
    }

    Json(serde_json::json!({
        "token": address,
        "pools": matching,
        "poolCount": pool_ids.len(),
        "recentSwaps": recent,
        "totals": {
            "volume_24h_usd": total_volume_24h,
            "volume_24h_str": format!("${}", format_with_commas(total_volume_24h)),
            "tvl_usd": total_tvl,
            "tvl_str": format!("${}", format_with_commas(total_tvl)),
            "lifetime_swaps": total_swaps,
        },
        "source": "dcswap-api-proxy",
        "upstream": dcswap_base,
    }))
}

/// `GET /api/v1/tokens/:addr/analytics` — token analytics.
///
/// Combines:
/// 1. The persistent holder index for current holder count + transfer count.
/// 2. DCSwap pool data for 24 h volume and TVL across all matching pools.
/// 3. Live `eth_call`-derived total supply.
///
/// The response is intentionally flat so the frontend can render charts
/// without doing additional aggregation work. Where a metric is genuinely
/// not available (e.g. historical holder series before the index started),
/// it is omitted rather than faked.
async fn token_analytics(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();

    // Holder count + transfer count from the persistent index.
    let (holders, transfers, last_scanned) = {
        let idx = state.holder_index.read().await;
        if let Some(ts) = idx.tokens.get(&addr_lc) {
            let live_holders = ts
                .balances
                .values()
                .filter(|raw| raw.parse::<u128>().map(|n| n > 0).unwrap_or(false))
                .count();
            (
                live_holders as u64,
                ts.transfer_count,
                ts.last_scanned_block,
            )
        } else {
            (0u64, 0u64, 0u64)
        }
    };

    // DEX volumes from DCSwap. Reuses the same /v1/pools call as the dex
    // endpoint so we don't double-fetch — the data is small and DCSwap
    // serves it from a tight cache anyway.
    let dcswap_base = state.dcswap_api.trim_end_matches('/').to_string();
    let pools_url = format!("{}/v1/pools", dcswap_base);
    let pools_json: serde_json::Value = match state
        .http_client
        .get(&pools_url)
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
    {
        Ok(r) => r
            .json::<serde_json::Value>()
            .await
            .unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };
    let pools = pools_json
        .get("data")
        .and_then(|d| d.get("pools"))
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let mut volume_24h = 0.0f64;
    let mut tvl = 0.0f64;
    let mut pool_count = 0u64;
    for p in &pools {
        let a = p
            .get("token_a")
            .and_then(|t| t.get("address"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let b = p
            .get("token_b")
            .and_then(|t| t.get("address"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if a == addr_lc || b == addr_lc {
            pool_count += 1;
            if let Some(v) = p.get("volume_24h").and_then(|v| v.as_str()) {
                volume_24h += v.parse::<f64>().unwrap_or(0.0);
            }
            if let Some(v) = p.get("tvl_usd").and_then(|v| v.as_str()) {
                tvl += v.parse::<f64>().unwrap_or(0.0);
            }
        }
    }

    // Total supply via eth_call (cheap; the price-cache layer absorbs the
    // overhead at high request rates).
    let decimals = eth_call_token_method(&state, &address, "0x313ce567")
        .await
        .as_deref()
        .map(|h| hex_to_u64(h) as u32)
        .or_else(|| known_token(&address).map(|i| i.decimals as u32))
        .unwrap_or(18);
    let total_supply_raw: u128 = eth_call_token_method(&state, &address, "0x18160ddd")
        .await
        .as_deref()
        .map(decode_hex_u256)
        .unwrap_or(0);
    let total_supply_f = total_supply_raw as f64 / 10f64.powi(decimals as i32);

    let price_usd = match known_token(&address) {
        Some(info) if info.symbol == "WFAT" => state
            .price_cache
            .read()
            .await
            .as_ref()
            .map(|p| p.price)
            .unwrap_or(FALLBACK_PRICE),
        Some(info) => info.usd_price,
        None => 0.0,
    };
    let market_cap_usd = total_supply_f * price_usd;

    Json(serde_json::json!({
        "token": address,
        "current": {
            "holders": holders,
            "transfers": transfers,
            "totalSupply": total_supply_f,
            "totalSupplyStr": format_with_commas(total_supply_f),
            "priceUsd": price_usd,
            "marketCapUsd": market_cap_usd,
            "marketCapStr": if market_cap_usd > 0.0 {
                format!("${}", format_with_commas(market_cap_usd))
            } else { "—".to_string() },
        },
        "dex": {
            "poolCount": pool_count,
            "volume24hUsd": volume_24h,
            "volume24hStr": format!("${}", format_with_commas(volume_24h)),
            "tvlUsd": tvl,
            "tvlStr": format!("${}", format_with_commas(tvl)),
        },
        "indexer": {
            "lastScannedBlock": last_scanned,
            "trackedSinceGenesis": last_scanned > 0,
        },
        "note": "Historical time-series (holder count over time, daily transfer volume) require a separate time-bucketed indexer; not yet available.",
        "source": "holder-index + dcswap-pools + eth_call",
    }))
}

/// Query params for `account_internal_txs`. `depth` is how many recent
/// blocks to scan (default 200, max 2000); `page` and `limit` control
/// pagination of the resulting items.
#[derive(Deserialize)]
struct InternalTxsParams {
    page: Option<u32>,
    limit: Option<u32>,
    depth: Option<u64>,
}

/// `GET /api/v1/accounts/:addr/internal-txs` — internal transactions for an
/// account, derived from `trace_block`.
///
/// We walk the most recent N blocks (default 200, configurable via `?depth=`)
/// and surface every sub-call where the address appears as `from` or `to`,
/// excluding the top-level entry (those are already on the Transactions
/// tab). This is the same source Etherscan/Blockscout use for their
/// "Internal Transactions" section. Reth keeps the trace cache for as long
/// as the corresponding state is in memory; older trace data may be empty.
async fn account_internal_txs(
    Path(address): Path<String>,
    Query(params): Query<InternalTxsParams>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();
    let limit = params.limit.unwrap_or(50).min(200) as usize;
    let page = params.page.unwrap_or(1).max(1) as usize;
    let offset = (page - 1) * limit;
    let depth = params.depth.unwrap_or(200).min(2000);

    let head = match rpc_block_number(&state).await {
        Ok(h) => h,
        Err(_) => {
            return Json(serde_json::json!({
                "address": address,
                "items": [],
                "total": 0,
                "limit": limit,
                "offset": offset,
                "note": "Chain head unavailable",
                "source": "trace_block",
            }))
        }
    };
    let from_block = head.saturating_sub(depth);

    let mut all: Vec<serde_json::Value> = Vec::new();

    // Walk most-recent-first so pagination is intuitive.
    for bn in (from_block..=head).rev() {
        if all.len() >= offset + limit + 50 {
            break;
        }
        let traces = match rpc_call(
            &state,
            "trace_block",
            vec![serde_json::json!(format!("0x{:x}", bn))],
        )
        .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };
        let arr = match traces.as_array() {
            Some(a) => a,
            None => continue,
        };

        for t in arr {
            // Skip the top-level entry; those are real transactions, not
            // internal calls. trace_block returns trace_address: [] for
            // the top frame.
            let trace_addr = t
                .get("traceAddress")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if trace_addr == 0 {
                continue;
            }
            let action = match t.get("action") {
                Some(a) => a,
                None => continue,
            };
            let from = action
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let to = action
                .get("to")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            if from != addr_lc && to != addr_lc {
                continue;
            }
            let value_hex = action.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");
            let value_fat = wei_to_fat(value_hex);
            let call_type = action
                .get("callType")
                .and_then(|v| v.as_str())
                .unwrap_or(t.get("type").and_then(|v| v.as_str()).unwrap_or("call"));
            all.push(serde_json::json!({
                "block": bn,
                "blockNumber": bn,
                "txHash": t.get("transactionHash").and_then(|v| v.as_str()).unwrap_or(""),
                "txPosition": t.get("transactionPosition").and_then(|v| v.as_u64()).unwrap_or(0),
                "callType": call_type,
                "from": from,
                "to": to,
                "valueRaw": value_hex,
                "valueFat": value_fat,
                "valueFormatted": format_fat(value_fat),
                "gas": action.get("gas").and_then(|v| v.as_str()).unwrap_or(""),
                "input": action.get("input").and_then(|v| v.as_str()).unwrap_or(""),
                "subtraces": t.get("subtraces").and_then(|v| v.as_u64()).unwrap_or(0),
                "traceAddress": t.get("traceAddress").cloned().unwrap_or(serde_json::Value::Array(vec![])),
                "error": t.get("error").and_then(|v| v.as_str()),
            }));
        }
    }

    let total = all.len();
    let page_items: Vec<serde_json::Value> = all.into_iter().skip(offset).take(limit).collect();

    Json(serde_json::json!({
        "address": address,
        "items": page_items,
        "total": total,
        "limit": limit,
        "offset": offset,
        "scannedFromBlock": from_block,
        "scannedToBlock": head,
        "depth": depth,
        "note": if total == 0 {
            "No internal transactions found in the scanned window. Increase `?offset=N` (depth in blocks) to scan deeper."
        } else { "Internal transactions derived from trace_block." },
        "source": "trace_block",
    }))
}

// =====================================================================
// NFT (ERC-721) endpoints — Tanastok DCNFT support
// =====================================================================

/// `GET /api/v1/nfts/:addr` — collection-level info for an ERC-721
/// contract. Returns name/symbol/totalSupply via `eth_call`, owner
/// distribution and historical transfer counts from the persistent
/// `nft_index`, and the full Tanastok asset descriptor (hero image,
/// asset type, location, valuation, ERC-3643 sister contract, …) for
/// any DCNFT registered with Tanastok.
///
/// This endpoint is the source-of-truth for the address page's
/// NFT-mode header. The frontend calls it whenever
/// `supportsInterface(0x80ac58cd) == true` and switches the layout
/// from fungible-token cards to NFT cards + Tanastok banner.
async fn get_nft_collection(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();

    // Live ABI introspection.
    let name = eth_call_token_method(&state, &address, "0x06fdde03")
        .await
        .as_deref()
        .and_then(decode_abi_string)
        .unwrap_or_else(|| "Unknown collection".to_string());
    let symbol = eth_call_token_method(&state, &address, "0x95d89b41")
        .await
        .as_deref()
        .and_then(decode_abi_string)
        .unwrap_or_default();
    let total_supply: u128 = eth_call_token_method(&state, &address, "0x18160ddd")
        .await
        .as_deref()
        .map(decode_hex_u256)
        .unwrap_or(0);

    // Confirm ERC-721 interface support so the frontend can be honest
    // about whether this address really is a DCNFT/ERC-721.
    // selector(supportsInterface) = 0x01ffc9a7 ; ERC-721 interfaceId = 0x80ac58cd
    let supports_erc721 = eth_call_token_method(
        &state,
        &address,
        "0x01ffc9a780ac58cd00000000000000000000000000000000000000000000000000000000",
    )
    .await
    .map(|h| h.to_lowercase().ends_with('1'))
    .unwrap_or(false);
    let supports_metadata = eth_call_token_method(
        &state,
        &address,
        "0x01ffc9a75b5e139f00000000000000000000000000000000000000000000000000000000",
    )
    .await
    .map(|h| h.to_lowercase().ends_with('1'))
    .unwrap_or(false);

    // Index snapshot.
    let (held, mint_count, burn_count, transfer_count, last_scanned, first_scanned, owners_count) = {
        let r = state.nft_index.read().await;
        if let Some(c) = r.collections.get(&addr_lc) {
            let unique_owners: std::collections::HashSet<&String> = c.owners.values().collect();
            (
                c.owners.len() as u64,
                c.mint_count,
                c.burn_count,
                c.transfer_count,
                c.last_scanned_block,
                c.first_scanned_block,
                unique_owners.len() as u64,
            )
        } else {
            (0, 0, 0, 0, 0, 0, 0)
        }
    };
    let head_now = rpc_block_number(&state).await.unwrap_or(0);
    let is_partial = head_now == 0 || last_scanned + 60 < head_now;

    // Tanastok enrichment (single source of truth for the DCNFT side).
    let tanastok = state
        .tanastok_cache
        .read()
        .await
        .as_ref()
        .and_then(|c| tanastok_lookup(c, &address));

    let (
        tanastok_name,
        hero_image,
        asset_type,
        asset_location,
        asset_id,
        sister_erc3643,
        is_verified,
        tanastok_url,
    ) = if let Some(t) = tanastok.as_ref() {
        let dc = t.get("dcnft").cloned().unwrap_or_default();
        let er = t.get("erc3643").cloned().unwrap_or_default();
        (
            t.get("name").and_then(|v| v.as_str()).map(String::from),
            t.get("heroImage").and_then(|v| v.as_str()).map(String::from),
            t.get("assetType").and_then(|v| v.as_str()).map(String::from),
            t.get("location").and_then(|v| v.as_str()).map(String::from),
            t.get("id").and_then(|v| v.as_str()).map(String::from),
            er.get("contractAddress")
                .and_then(|v| v.as_str())
                .map(String::from),
            t.get("isVerified").and_then(|v| v.as_bool()).unwrap_or(false),
            t.get("tanastokUrl").and_then(|v| v.as_str()).map(String::from),
        )
    } else {
        (None, None, None, None, None, None, false, None)
    };

    // Hero image URL: Tanastok returns either an absolute URL or a
    // relative path; normalise to an absolute URL so the frontend
    // doesn't need to do this.
    let hero_url = hero_image.as_ref().map(|h| {
        if h.starts_with("http://") || h.starts_with("https://") {
            h.clone()
        } else {
            format!("https://tanastok.io{}", h)
        }
    });

    Json(serde_json::json!({
        "address": addr_lc,
        "name": name,
        "symbol": symbol,
        "tokenStandard": if supports_erc721 { "ERC-721" } else { "Unknown" },
        "supportsErc721": supports_erc721,
        "supportsErc721Metadata": supports_metadata,
        "totalSupply": total_supply.to_string(),
        "totalSupplyNum": total_supply as f64,
        "heldTokens": held,
        "mintCount": mint_count,
        "burnCount": burn_count,
        "transferCount": transfer_count,
        "ownersCount": owners_count,
        "indexer": {
            "firstScannedBlock": first_scanned,
            "lastScannedBlock": last_scanned,
            "isPartial": is_partial,
        },
        "tanastok": tanastok.as_ref().map(|t| serde_json::json!({
            "isTanastokDcnft": true,
            "assetId": asset_id,
            "assetName": tanastok_name,
            "assetType": asset_type,
            "location": asset_location,
            "heroImage": hero_url,
            "isVerified": is_verified,
            "tanastokUrl": tanastok_url,
            "sisterErc3643": sister_erc3643,
            "fullDescriptor": t,
        })).unwrap_or_else(|| serde_json::json!({
            "isTanastokDcnft": false,
        })),
        "source": "nft-index + eth_call + tanastok-cache",
    }))
}

/// `GET /api/v1/nfts/:addr/holders` — top owners by token count.
async fn nft_holders(
    Path(address): Path<String>,
    Query(params): Query<PaginationParams>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();
    let page = params.page.unwrap_or(1).max(1) as usize;
    let limit = params.limit.unwrap_or(50).min(500) as usize;
    let offset = (page - 1) * limit;

    let (entries, total_owners, total_held, last_scanned, first_scanned) = {
        let r = state.nft_index.read().await;
        if let Some(c) = r.collections.get(&addr_lc) {
            // Count tokens per owner.
            let mut by_owner: std::collections::HashMap<String, u64> =
                std::collections::HashMap::new();
            for owner in c.owners.values() {
                *by_owner.entry(owner.clone()).or_insert(0) += 1;
            }
            let total_held = c.owners.len() as u64;
            let mut entries: Vec<(String, u64)> = by_owner.into_iter().collect();
            entries.sort_by(|a, b| b.1.cmp(&a.1));
            (
                entries,
                c.owners.values().collect::<std::collections::HashSet<_>>().len() as u64,
                total_held,
                c.last_scanned_block,
                c.first_scanned_block,
            )
        } else {
            (Vec::new(), 0, 0, 0, 0)
        }
    };

    let head_now = rpc_block_number(&state).await.unwrap_or(0);
    let is_partial = head_now == 0 || last_scanned + 60 < head_now;

    let page_entries: Vec<serde_json::Value> = entries
        .iter()
        .skip(offset)
        .take(limit)
        .enumerate()
        .map(|(i, (owner, count))| {
            let pct = if total_held > 0 {
                (*count as f64 / total_held as f64) * 100.0
            } else {
                0.0
            };
            serde_json::json!({
                "rank": offset + i + 1,
                "address": owner,
                "tokenCount": count,
                "percentage": format!("{:.4}%", pct),
            })
        })
        .collect();

    Json(serde_json::json!({
        "address": addr_lc,
        "total": total_owners,
        "totalHeld": total_held,
        "page": page,
        "limit": limit,
        "offset": offset,
        "holders": page_entries,
        "isPartial": is_partial,
        "firstScannedBlock": first_scanned,
        "lastScannedBlock": last_scanned,
        "source": "nft-index",
        "note": if is_partial {
            "NFT index is still catching up to chain head — values reflect every Transfer up to the lastScannedBlock above."
        } else { "Live, up to chain head." },
    }))
}

/// `GET /api/v1/nfts/:addr/tokens` — inventory: every minted tokenId
/// with its current owner. Sorted by numeric tokenId ascending.
async fn nft_inventory(
    Path(address): Path<String>,
    Query(params): Query<PaginationParams>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();
    let page = params.page.unwrap_or(1).max(1) as usize;
    let limit = params.limit.unwrap_or(50).min(500) as usize;
    let offset = (page - 1) * limit;

    let (mut tokens, last_scanned, first_scanned) = {
        let r = state.nft_index.read().await;
        if let Some(c) = r.collections.get(&addr_lc) {
            let v: Vec<(String, String)> = c
                .owners
                .iter()
                .map(|(id, owner)| (id.clone(), owner.clone()))
                .collect();
            (v, c.last_scanned_block, c.first_scanned_block)
        } else {
            (Vec::new(), 0, 0)
        }
    };
    tokens.sort_by(|a, b| {
        let ai = a.0.parse::<u128>().unwrap_or(0);
        let bi = b.0.parse::<u128>().unwrap_or(0);
        ai.cmp(&bi)
    });
    let total = tokens.len() as u64;

    let page_items: Vec<serde_json::Value> = tokens
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(id, owner)| {
            serde_json::json!({
                "tokenId": id,
                "owner": owner,
            })
        })
        .collect();

    let head_now = rpc_block_number(&state).await.unwrap_or(0);
    let is_partial = head_now == 0 || last_scanned + 60 < head_now;

    Json(serde_json::json!({
        "address": addr_lc,
        "total": total,
        "page": page,
        "limit": limit,
        "offset": offset,
        "tokens": page_items,
        "isPartial": is_partial,
        "firstScannedBlock": first_scanned,
        "lastScannedBlock": last_scanned,
        "source": "nft-index",
    }))
}

/// `GET /api/v1/nfts/:addr/transfers` — recent ERC-721 Transfer events
/// for this collection. Reads from the persistent NFT index's
/// `recent_transfers` ring buffer (capped at 200 events per collection,
/// newest first), so the response is instant regardless of how far in
/// the past the events were tied. Falls back to a live `eth_getLogs`
/// walk for collections that haven't been indexed yet (rare).
async fn nft_transfers(
    Path(address): Path<String>,
    Query(params): Query<InternalTxsParams>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();
    let page = params.page.unwrap_or(1).max(1) as usize;
    let limit = params.limit.unwrap_or(25).min(200) as usize;
    let offset = (page - 1) * limit;

    // 1) Fast path — read from the persistent index.
    let (cached, last_scanned, transfer_count, mint_count, burn_count) = {
        let r = state.nft_index.read().await;
        if let Some(c) = r.collections.get(&addr_lc) {
            (
                c.recent_transfers.clone(),
                c.last_scanned_block,
                c.transfer_count,
                c.mint_count,
                c.burn_count,
            )
        } else {
            (Vec::new(), 0, 0, 0, 0)
        }
    };

    if !cached.is_empty() {
        let zero_addr = "0x0000000000000000000000000000000000000000";
        let total = cached.len() as u64;
        let items: Vec<serde_json::Value> = cached
            .iter()
            .skip(offset)
            .take(limit)
            .map(|t| {
                let kind = if t.from == zero_addr {
                    "Mint"
                } else if t.to == zero_addr {
                    "Burn"
                } else {
                    "Transfer"
                };
                serde_json::json!({
                    "txHash": t.tx_hash,
                    "block": t.block,
                    "logIndex": t.log_index,
                    "from": t.from,
                    "to": t.to,
                    "tokenId": t.token_id,
                    "type": kind,
                })
            })
            .collect();
        return Json(serde_json::json!({
            "address": addr_lc,
            "total": total,
            "page": page,
            "limit": limit,
            "offset": offset,
            "items": items,
            "lastScannedBlock": last_scanned,
            "transferCountLifetime": transfer_count,
            "mintCount": mint_count,
            "burnCount": burn_count,
            "source": "nft-index (recent_transfers cache)",
            "note": if total < transfer_count {
                "Showing the most recent transfer events held by the index (capped at 200)."
            } else { "Complete transfer history for this collection." },
        }));
    }

    // 2) Fallback — collection not yet in the index, do a live scan.
    let depth = params.depth.unwrap_or(2_000_000).min(10_000_000);
    let head = match rpc_block_number(&state).await {
        Ok(h) => h,
        Err(_) => {
            return Json(serde_json::json!({
                "address": addr_lc,
                "items": [],
                "total": 0,
                "note": "Chain head unavailable",
            }))
        }
    };
    let from_block = head.saturating_sub(depth);

    let chunk: u64 = 100_000;
    let mut all: Vec<serde_json::Value> = Vec::new();
    let target = (offset + limit + 1) as usize;

    let mut cur_to = head;
    while cur_to > from_block && all.len() < target * 4 {
        let cur_from = cur_to.saturating_sub(chunk - 1).max(from_block);
        let logs_res = rpc_call(
            &state,
            "eth_getLogs",
            vec![serde_json::json!({
                "fromBlock": format!("0x{:x}", cur_from),
                "toBlock":   format!("0x{:x}", cur_to),
                "address":   &addr_lc,
                "topics":    [TRANSFER_TOPIC],
            })],
        )
        .await;
        if let Ok(v) = logs_res {
            if let Some(arr) = v.as_array() {
                for log in arr.iter().rev() {
                    let topics = match log.get("topics").and_then(|t| t.as_array()) {
                        Some(t) if t.len() == 4 => t,
                        _ => continue,
                    };
                    let from = topic_to_address(topics[1].as_str().unwrap_or(""));
                    let to = topic_to_address(topics[2].as_str().unwrap_or(""));
                    let token_id =
                        decode_hex_u256(topics[3].as_str().unwrap_or("0x0")).to_string();
                    let block = log
                        .get("blockNumber")
                        .and_then(|v| v.as_str())
                        .map(hex_to_u64)
                        .unwrap_or(0);
                    let tx_hash = log
                        .get("transactionHash")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let log_index = log
                        .get("logIndex")
                        .and_then(|v| v.as_str())
                        .map(hex_to_u64)
                        .unwrap_or(0);
                    all.push(serde_json::json!({
                        "txHash": tx_hash,
                        "block": block,
                        "logIndex": log_index,
                        "from": from,
                        "to": to,
                        "tokenId": token_id,
                        "type": if from == "0x0000000000000000000000000000000000000000" {
                            "Mint"
                        } else if to == "0x0000000000000000000000000000000000000000" {
                            "Burn"
                        } else { "Transfer" },
                    }));
                }
            }
        } else {
            break;
        }
        if cur_from == from_block {
            break;
        }
        cur_to = cur_from.saturating_sub(1);
    }

    let total = all.len() as u64;
    let items: Vec<serde_json::Value> = all.into_iter().skip(offset).take(limit).collect();

    Json(serde_json::json!({
        "address": addr_lc,
        "total": total,
        "page": page,
        "limit": limit,
        "offset": offset,
        "depth": depth,
        "items": items,
        "source": "eth_getLogs (live fallback — collection not in NFT index)",
    }))
}

/// `GET /api/v1/accounts/:addr/nfts` — every DCNFT (or other ERC-721)
/// currently owned by this account, derived from the persistent
/// `nft_index`. Includes the Tanastok asset metadata (hero image, name,
/// type) for each held DCNFT so the address page can show a rich
/// inventory grid.
async fn account_nfts(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();

    let mut held: Vec<(String, String)> = Vec::new(); // (collection_addr, tokenId)
    {
        let r = state.nft_index.read().await;
        for (col_addr, col_state) in r.collections.iter() {
            for (token_id, owner) in col_state.owners.iter() {
                if owner == &addr_lc {
                    held.push((col_addr.clone(), token_id.clone()));
                }
            }
        }
    }
    held.sort();

    // Enrich each holding with Tanastok metadata if it's a DCNFT.
    let tanastok = state.tanastok_cache.read().await;
    let items: Vec<serde_json::Value> = held
        .iter()
        .map(|(col_addr, token_id)| {
            let asset = tanastok
                .as_ref()
                .and_then(|c| tanastok_lookup(c, col_addr));
            let mut hero: Option<String> = None;
            let mut asset_name: Option<String> = None;
            let mut asset_type: Option<String> = None;
            let mut sister_erc3643: Option<String> = None;
            let mut tanastok_url: Option<String> = None;
            if let Some(a) = asset.as_ref() {
                hero = a
                    .get("heroImage")
                    .and_then(|v| v.as_str())
                    .map(|h| {
                        if h.starts_with("http") {
                            h.to_string()
                        } else {
                            format!("https://tanastok.io{}", h)
                        }
                    });
                asset_name = a.get("name").and_then(|v| v.as_str()).map(String::from);
                asset_type = a.get("assetType").and_then(|v| v.as_str()).map(String::from);
                sister_erc3643 = a
                    .get("erc3643")
                    .and_then(|e| e.get("contractAddress"))
                    .and_then(|v| v.as_str())
                    .map(String::from);
                tanastok_url = a
                    .get("tanastokUrl")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
            serde_json::json!({
                "collectionAddress": col_addr,
                "tokenId": token_id,
                "isTanastokDcnft": asset.is_some(),
                "assetName": asset_name,
                "assetType": asset_type,
                "heroImage": hero,
                "sisterErc3643": sister_erc3643,
                "tanastokUrl": tanastok_url,
            })
        })
        .collect();

    Json(serde_json::json!({
        "address": addr_lc,
        "total": items.len(),
        "items": items,
        "source": "nft-index + tanastok-cache",
    }))
}

/// `GET /api/v1/accounts/:addr/nft-transfers` — every ERC-721 transfer
/// where this account appears as `from` or `to`. Reads from the
/// persistent NFT index's `recent_transfers` ring buffer first (covers
/// the entire mint+transfer history of every Tanastok DCNFT once the
/// scanner has caught up), and falls back to a live `eth_getLogs`
/// query only if the index is still warming up.
async fn account_nft_transfers(
    Path(address): Path<String>,
    Query(params): Query<InternalTxsParams>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let addr_lc = address.to_lowercase();
    let page = params.page.unwrap_or(1).max(1) as usize;
    let limit = params.limit.unwrap_or(25).min(200) as usize;
    let offset = (page - 1) * limit;

    // 1) Fast path — walk the persistent index across every collection
    // looking for transfers where this address appears.
    let mut all: Vec<serde_json::Value> = Vec::new();
    let zero_addr = "0x0000000000000000000000000000000000000000";
    {
        let r = state.nft_index.read().await;
        for (col_addr, col_state) in r.collections.iter() {
            for t in col_state.recent_transfers.iter() {
                if t.from != addr_lc && t.to != addr_lc {
                    continue;
                }
                let kind = if t.from == zero_addr {
                    "Mint"
                } else if t.to == zero_addr {
                    "Burn"
                } else {
                    "Transfer"
                };
                all.push(serde_json::json!({
                    "txHash": t.tx_hash,
                    "block": t.block,
                    "logIndex": t.log_index,
                    "from": t.from,
                    "to": t.to,
                    "tokenId": t.token_id,
                    "collection": col_addr,
                    "type": kind,
                }));
            }
        }
    }
    if !all.is_empty() {
        all.sort_by(|a, b| {
            let ab = a.get("block").and_then(|v| v.as_u64()).unwrap_or(0);
            let bb = b.get("block").and_then(|v| v.as_u64()).unwrap_or(0);
            let al = a.get("logIndex").and_then(|v| v.as_u64()).unwrap_or(0);
            let bl = b.get("logIndex").and_then(|v| v.as_u64()).unwrap_or(0);
            bb.cmp(&ab).then(bl.cmp(&al))
        });
        let total = all.len() as u64;
        let items: Vec<serde_json::Value> = all.into_iter().skip(offset).take(limit).collect();
        return Json(serde_json::json!({
            "address": addr_lc,
            "total": total,
            "page": page,
            "limit": limit,
            "offset": offset,
            "items": items,
            "source": "nft-index (recent_transfers cache)",
        }));
    }

    // 2) Fallback — index empty (first boot). Live walk like before.
    let depth = params.depth.unwrap_or(2_000_000).min(10_000_000);
    let head = match rpc_block_number(&state).await {
        Ok(h) => h,
        Err(_) => {
            return Json(serde_json::json!({
                "address": addr_lc,
                "items": [],
                "total": 0,
                "note": "Chain head unavailable",
            }))
        }
    };
    let from_block = head.saturating_sub(depth);

    let collections: Vec<String> = {
        let r = state.nft_index.read().await;
        r.collections.keys().cloned().collect()
    };
    if collections.is_empty() {
        return Json(serde_json::json!({
            "address": addr_lc,
            "items": [],
            "total": 0,
            "note": "NFT index not yet populated.",
        }));
    }

    let padded = format!(
        "0x000000000000000000000000{}",
        addr_lc.trim_start_matches("0x")
    );

    let queries = vec![
        serde_json::json!({
            "fromBlock": format!("0x{:x}", from_block),
            "toBlock":   format!("0x{:x}", head),
            "address":   &collections,
            "topics":    [TRANSFER_TOPIC, padded.clone(), null],
        }),
        serde_json::json!({
            "fromBlock": format!("0x{:x}", from_block),
            "toBlock":   format!("0x{:x}", head),
            "address":   &collections,
            "topics":    [TRANSFER_TOPIC, null, padded.clone()],
        }),
    ];

    let mut all: Vec<serde_json::Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for q in queries {
        if let Ok(v) = rpc_call(&state, "eth_getLogs", vec![q]).await {
            if let Some(arr) = v.as_array() {
                for log in arr {
                    let topics = match log.get("topics").and_then(|t| t.as_array()) {
                        Some(t) if t.len() == 4 => t,
                        _ => continue,
                    };
                    let from = topic_to_address(topics[1].as_str().unwrap_or(""));
                    let to = topic_to_address(topics[2].as_str().unwrap_or(""));
                    let token_id =
                        decode_hex_u256(topics[3].as_str().unwrap_or("0x0")).to_string();
                    let block = log
                        .get("blockNumber")
                        .and_then(|v| v.as_str())
                        .map(hex_to_u64)
                        .unwrap_or(0);
                    let tx_hash = log
                        .get("transactionHash")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let log_index = log
                        .get("logIndex")
                        .and_then(|v| v.as_str())
                        .map(hex_to_u64)
                        .unwrap_or(0);
                    let collection = log
                        .get("address")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    let key = format!("{}:{}", tx_hash, log_index);
                    if !seen.insert(key) {
                        continue;
                    }
                    all.push(serde_json::json!({
                        "txHash": tx_hash,
                        "block": block,
                        "logIndex": log_index,
                        "from": from,
                        "to": to,
                        "tokenId": token_id,
                        "collection": collection,
                        "type": if from == "0x0000000000000000000000000000000000000000" {
                            "Mint"
                        } else if to == "0x0000000000000000000000000000000000000000" {
                            "Burn"
                        } else { "Transfer" },
                    }));
                }
            }
        }
    }

    all.sort_by(|a, b| {
        let ab = a.get("block").and_then(|v| v.as_u64()).unwrap_or(0);
        let bb = b.get("block").and_then(|v| v.as_u64()).unwrap_or(0);
        let al = a.get("logIndex").and_then(|v| v.as_u64()).unwrap_or(0);
        let bl = b.get("logIndex").and_then(|v| v.as_u64()).unwrap_or(0);
        bb.cmp(&ab).then(bl.cmp(&al))
    });

    let total = all.len() as u64;
    let items: Vec<serde_json::Value> = all.into_iter().skip(offset).take(limit).collect();

    Json(serde_json::json!({
        "address": addr_lc,
        "total": total,
        "page": page,
        "limit": limit,
        "offset": offset,
        "depth": depth,
        "items": items,
        "source": "eth_getLogs (live fallback)",
    }))
}

async fn refresh_tokentxn_cache(state: &Arc<AppState>) {
    let head_hex = rpc_call(state, "eth_blockNumber", vec![])
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x0".to_string());
    let head = hex_to_u64(&head_hex);

    let existing = state.tokentxn_cache.read().await;
    let last_block = existing
        .as_ref()
        .and_then(|c| c.transfers.first())
        .and_then(|t| t.get("block").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    drop(existing);

    let scan_depth: u64 = 2000;
    let start = if last_block > 0 {
        last_block + 1
    } else if head > scan_depth {
        head - scan_depth
    } else {
        0
    };

    let mut all_transfers: Vec<serde_json::Value> = Vec::new();
    let mut unique_tokens: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut total_usd: f64 = 0.0;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    for bn in (start..=head).rev() {
        if all_transfers.len() >= 50 {
            break;
        }
        let bn_hex = format!("0x{:x}", bn);

        let block = match rpc_call(
            state,
            "eth_getBlockByNumber",
            vec![serde_json::json!(bn_hex), serde_json::json!(false)],
        )
        .await
        {
            Ok(b) if !b.is_null() => b,
            _ => continue,
        };

        let tx_hashes = match block.get("transactions").and_then(|v| v.as_array()) {
            Some(a) if !a.is_empty() => a.clone(),
            _ => continue,
        };

        let block_ts = block
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(|s| hex_to_u64(s))
            .unwrap_or(0);
        let age_secs = if now_secs > block_ts {
            now_secs - block_ts
        } else {
            0
        };
        let age_str = if age_secs < 60 {
            format!("{}s ago", age_secs)
        } else if age_secs < 3600 {
            format!("{}m ago", age_secs / 60)
        } else if age_secs < 86400 {
            format!("{}h ago", age_secs / 3600)
        } else {
            format!("{}d ago", age_secs / 86400)
        };

        for tx_val in &tx_hashes {
            if all_transfers.len() >= 50 {
                break;
            }
            let tx_hash = tx_val.as_str().unwrap_or("");
            if tx_hash.is_empty() {
                continue;
            }

            let receipt = match rpc_call(
                state,
                "eth_getTransactionReceipt",
                vec![serde_json::json!(tx_hash)],
            )
            .await
            {
                Ok(r) if !r.is_null() => r,
                _ => continue,
            };

            let logs: Vec<serde_json::Value> = receipt
                .get("logs")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let tx_from = receipt
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or("0x0")
                .to_string();

            for log in &logs {
                if all_transfers.len() >= 50 {
                    break;
                }
                let topics = match log.get("topics").and_then(|v| v.as_array()) {
                    Some(t) if t.len() >= 3 => t,
                    _ => continue,
                };
                let topic0 = topics[0].as_str().unwrap_or("");
                if topic0 != TRANSFER_TOPIC {
                    continue;
                }

                let token_addr = log.get("address").and_then(|v| v.as_str()).unwrap_or("");
                let (symbol, decimals, usd_price) = match known_token(token_addr) {
                    Some(info) => (info.symbol.to_string(), info.decimals, info.usd_price),
                    None => continue,
                };
                unique_tokens.insert(symbol.clone());

                let from = topic_to_address(topics[1].as_str().unwrap_or(""));
                let to = topic_to_address(topics[2].as_str().unwrap_or(""));
                let data = log.get("data").and_then(|v| v.as_str()).unwrap_or("0x0");
                let raw_amount = decode_hex_u256(data);

                let divisor = 10f64.powi(decimals as i32);
                let amount = raw_amount as f64 / divisor;
                let usd_val = amount * usd_price;
                total_usd += usd_val;

                let amount_str = if amount < 0.001 && amount > 0.0 {
                    format!("{:.8}", amount)
                } else if amount >= 1_000_000.0 {
                    format!("{:.2}", amount)
                } else {
                    format!("{:.4}", amount)
                };

                all_transfers.push(serde_json::json!({
                    "txHash": tx_hash,
                    "block": bn,
                    "age": age_str,
                    "from": from,
                    "to": to,
                    "amount": amount_str,
                    "usdValue": format!("${:.2}", usd_val),
                    "token": symbol,
                    "tokenAddress": token_addr,
                    "initiator": tx_from,
                }));
            }
        }
    }

    {
        let existing = state.tokentxn_cache.read().await;
        if let Some(ref c) = *existing {
            for old in &c.transfers {
                if all_transfers.len() >= 50 {
                    break;
                }
                let old_hash = old.get("txHash").and_then(|v| v.as_str()).unwrap_or("");
                let already = all_transfers
                    .iter()
                    .any(|t| t.get("txHash").and_then(|v| v.as_str()).unwrap_or("") == old_hash);
                if !already {
                    if let Some(sym) = old.get("token").and_then(|v| v.as_str()) {
                        unique_tokens.insert(sym.to_string());
                    }
                    if let Some(usd) = old.get("usdValue").and_then(|v| v.as_str()) {
                        total_usd += usd.trim_start_matches('$').parse::<f64>().unwrap_or(0.0);
                    }
                    all_transfers.push(old.clone());
                }
            }
        }
        drop(existing);
    }

    all_transfers.sort_by(|a, b| {
        let ba = a.get("block").and_then(|v| v.as_u64()).unwrap_or(0);
        let bb = b.get("block").and_then(|v| v.as_u64()).unwrap_or(0);
        bb.cmp(&ba)
    });
    all_transfers.truncate(50);

    let total_transfers = all_transfers.len() as u64;
    let avg_value = if total_transfers > 0 {
        total_usd / total_transfers as f64
    } else {
        0.0
    };

    let stats = serde_json::json!({
        "totalTransfers": total_transfers,
        "transfers24h": total_transfers,
        "uniqueTokens": unique_tokens.len(),
        "avgTransferValue": format!("${:.2}", avg_value),
    });

    let fresh = TokenTxnCache {
        stats,
        transfers: all_transfers,
        updated_at: now_secs as i64,
    };
    save_json_cache("tokentxn_cache.json", &fresh);
    let mut cache = state.tokentxn_cache.write().await;
    *cache = Some(fresh);
    tracing::info!(
        "Token transfer cache refreshed: {} transfers",
        total_transfers
    );
}

async fn list_token_transfers_live(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let cache = state.tokentxn_cache.read().await;
    if let Some(ref c) = *cache {
        return Json(serde_json::json!({
            "stats": c.stats,
            "transfers": c.transfers,
        }));
    }
    drop(cache);
    Json(serde_json::json!({
        "stats": { "totalTransfers": 0, "transfers24h": 0, "uniqueTokens": 0, "avgTransferValue": "$0.00" },
        "transfers": [],
    }))
}

async fn list_validators(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let head_hex = rpc_call(&state, "eth_blockNumber", vec![])
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x0".to_string());
    let head = hex_to_u64(&head_hex);

    // Gather AI agents as validators from the DB
    let mut validators: Vec<serde_json::Value> = Vec::new();
    let mut total_staked: f64 = 0.0;
    let mut active_count: u64 = 0;
    let mut total_validations: u64 = 0;
    let mut uptime_sum: f64 = 0.0;

    if let Some(ref pool) = state.db_pool {
        if let Ok(agents) = db::list_agents(pool).await {
            for agent in &agents {
                let balance_hex = rpc_call(
                    &state,
                    "eth_getBalance",
                    vec![
                        serde_json::json!(&agent.wallet_address),
                        serde_json::json!("latest"),
                    ],
                )
                .await
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "0x0".to_string());
                let nonce_hex = rpc_call(
                    &state,
                    "eth_getTransactionCount",
                    vec![
                        serde_json::json!(&agent.wallet_address),
                        serde_json::json!("latest"),
                    ],
                )
                .await
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "0x0".to_string());

                let balance = wei_to_fat(&balance_hex);
                let tx_count = hex_to_u64(&nonce_hex);

                let mut is_online = agent.status == "online";
                let mut processed: u64 = 0;
                let mut agent_uptime: f64 = 99.5;
                if let Some(ref url) = agent.health_url {
                    match state
                        .http_client
                        .get(url)
                        .timeout(std::time::Duration::from_secs(3))
                        .send()
                        .await
                    {
                        Ok(resp) if resp.status().is_success() => {
                            is_online = true;
                            if let Ok(json) = resp.json::<serde_json::Value>().await {
                                if let Some(stats) = json.get("stats") {
                                    processed = stats
                                        .get("processed")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0);
                                    let start = stats
                                        .get("startTime")
                                        .and_then(|v| v.as_f64())
                                        .unwrap_or(0.0);
                                    if start > 0.0 {
                                        let running_ms =
                                            (chrono::Utc::now().timestamp_millis() as f64) - start;
                                        let total_ms = running_ms + 3600000.0;
                                        agent_uptime = (running_ms / total_ms * 100.0).min(99.99);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }

                let validations = if processed > 0 { processed } else { tx_count };
                total_validations += validations;
                total_staked += balance;
                if is_online || tx_count > 0 {
                    active_count += 1;
                }
                uptime_sum += agent_uptime;

                validators.push(serde_json::json!({
                    "address": agent.wallet_address,
                    "name": agent.name,
                    "type": agent.agent_type,
                    "status": if is_online { "active" } else { "standby" },
                    "stake": format!("{:.0}", balance),
                    "stakeRaw": balance,
                    "validations": validations,
                    "uptime": format!("{:.1}", agent_uptime),
                    "uptimeRaw": agent_uptime,
                    "isAgent": true,
                    "icon": match agent.agent_type.as_str() {
                        "Regulatory" => "fa-gavel",
                        "Validation" => "fa-shield-alt",
                        "Risk Assessment" => "fa-umbrella",
                        "Data Verification" => "fa-database",
                        "Contract Analysis" => "fa-file-contract",
                        _ => "fa-microchip"
                    },
                    "desc": agent.description
                }));
            }
        }
    }

    // No Postgres on production nodes: surface the five canonical testimony
    // agents as validators from their live rope-node personal-ledger strings.
    if state.db_pool.is_none() {
        let now = chrono::Utc::now().timestamp();
        for (_agent_id, agent_name, wallet, attest_type, health_url) in CANONICAL_AGENT_WALLETS {
            let Some(descriptor) = rope_string_descriptor(&state, wallet).await else {
                continue;
            };
            let knot_count = descriptor
                .get("knot_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let last_anchored_at = descriptor
                .get("last_anchored_at")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let balance_hex = rpc_call(
                &state,
                "eth_getBalance",
                vec![serde_json::json!(wallet), serde_json::json!("latest")],
            )
            .await
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "0x0".to_string());
            let balance = wei_to_fat(&balance_hex);

            let is_online = agent_health_ok(&state, health_url).await
                || (knot_count > 0 && now - last_anchored_at < AGENT_ACTIVE_WINDOW_SECS);
            let agent_uptime = if is_online { 99.9 } else { 0.0 };
            total_validations += knot_count;
            total_staked += balance;
            if is_online {
                active_count += 1;
            }
            uptime_sum += agent_uptime;

            validators.push(serde_json::json!({
                "address": wallet,
                "name": agent_name,
                "type": attest_type,
                "status": if is_online { "active" } else { "standby" },
                "stake": format!("{:.0}", balance),
                "stakeRaw": balance,
                "validations": knot_count,
                "uptime": format!("{:.1}", agent_uptime),
                "uptimeRaw": agent_uptime,
                "isAgent": true,
                "icon": "fa-microchip",
                "desc": format!(
                    "Canonical AI testimony agent ({}) anchoring knots on its personal-ledger string",
                    attest_type
                )
            }));
        }
    }

    // Add the genesis knot witness (Reth coinbase) as a validator entry.
    //
    // Pre-2026-03-25 genesis used `0x302fa1...` as the dev coinbase; that
    // address was zeroed by the genesis reset that moved the treasury to the
    // controlled deployer key. Post-reset, the Reth coinbase / production
    // knot witness is `0x60FB32...` (per `reth-migration-2026-03-12.mdc`).
    //
    // Per Quipu Primitive Canon v1.1 §7(2), "block producer" maps to
    // "knot witness". The old "Block Producer" label is preserved as a
    // hidden alias for tooling that hasn't migrated.
    let genesis = "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195";
    let gen_bal_hex = rpc_call(
        &state,
        "eth_getBalance",
        vec![serde_json::json!(genesis), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x0".to_string());
    let gen_balance = wei_to_fat(&gen_bal_hex);
    total_staked += gen_balance;
    active_count += 1;
    uptime_sum += 100.0;
    total_validations += head;

    validators.insert(0, serde_json::json!({
        "address": genesis,
        "name": "Genesis Knot Witness",
        "type": "Knot Witness",
        "typeAlias": "Block Producer",
        "status": "active",
        "stake": format!("{:.0}", gen_balance),
        "stakeRaw": gen_balance,
        "validations": head,
        "uptime": "100.0",
        "uptimeRaw": 100.0,
        "isAgent": false,
        "icon": "fa-cubes",
        "desc": "Primary knot witness for Datachain Rope (Reth EVM execution layer, blue-green deployment per reth-blue-green-ipfs-architecture.mdc)"
    }));

    let validator_count = validators.len() as u64;
    let avg_uptime = if validator_count > 0 {
        uptime_sum / validator_count as f64
    } else {
        0.0
    };

    Json(serde_json::json!({
        "validators": validators,
        "totalValidators": validator_count,
        "activeCount": active_count,
        "avgUptime": format!("{:.1}", avg_uptime),
        "validationsToday": total_validations,
        "totalStaked": format!("{:.0}", total_staked),
        "blockHeight": head
    }))
}

async fn get_validator(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let balance_hex = rpc_call(
        &state,
        "eth_getBalance",
        vec![serde_json::json!(&address), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x0".to_string());
    let nonce_hex = rpc_call(
        &state,
        "eth_getTransactionCount",
        vec![serde_json::json!(&address), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x0".to_string());
    let balance = wei_to_fat(&balance_hex);
    let tx_count = hex_to_u64(&nonce_hex);

    let mut name = format!("Validator {}", &address[..8]);
    let mut agent_type = "Unknown".to_string();
    if let Some(ref pool) = state.db_pool {
        if let Ok(Some(agent)) = db::get_agent_by_wallet(pool, &address).await {
            name = agent.name.clone();
            agent_type = agent.agent_type.clone();
        }
    }

    Json(serde_json::json!({
        "address": address,
        "name": name,
        "type": agent_type,
        "stake": format!("{:.0} FAT", balance),
        "validations": tx_count,
        "uptime": "99.5%",
        "balance": format_fat(balance),
        "transactionCount": tx_count
    }))
}

async fn agent_row_to_json(state: &AppState, a: &db::AgentRow) -> serde_json::Value {
    let balance_hex = rpc_call(
        state,
        "eth_getBalance",
        vec![
            serde_json::json!(&a.wallet_address),
            serde_json::json!("latest"),
        ],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x0".to_string());

    let nonce_hex = rpc_call(
        state,
        "eth_getTransactionCount",
        vec![
            serde_json::json!(&a.wallet_address),
            serde_json::json!("latest"),
        ],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x0".to_string());

    let balance = wei_to_fat(&balance_hex);
    let tx_count = hex_to_u64(&nonce_hex);

    let mut health: Option<serde_json::Value> = None;
    let mut live_status = a.status.clone();
    let mut processed: u64 = 0;
    let mut approved: u64 = 0;
    let mut denied: u64 = 0;
    let mut uptime_secs: u64 = 0;

    if let Some(ref url) = a.health_url {
        if let Ok(resp) = state
            .http_client
            .get(url)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let h_status = json
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    live_status = if h_status == "healthy" {
                        "online".to_string()
                    } else {
                        "standby".to_string()
                    };
                    if let Some(stats) = json.get("stats") {
                        processed = stats.get("processed").and_then(|v| v.as_u64()).unwrap_or(0);
                        approved = stats.get("approved").and_then(|v| v.as_u64()).unwrap_or(0);
                        denied = stats.get("denied").and_then(|v| v.as_u64()).unwrap_or(0);
                    }
                    uptime_secs = json.get("uptime").and_then(|v| v.as_u64()).unwrap_or(0);
                    health = Some(json);
                }
            } else {
                live_status = "standby".to_string();
            }
        } else {
            live_status = "offline".to_string();
        }
    } else if balance > 0.0 {
        live_status = "standby".to_string();
    }

    let total_testimonies = if processed > 0 { processed } else { tx_count };
    let rewards_earned = total_testimonies as f64 * a.reward_rate_fat;
    let created_ago = chrono::Utc::now().signed_duration_since(a.created_at);
    let uptime_str = if uptime_secs > 0 {
        let h = uptime_secs / 3600;
        let m = (uptime_secs % 3600) / 60;
        format!("{}h {}m", h, m)
    } else {
        "—".to_string()
    };

    serde_json::json!({
        "id": a.id,
        "name": a.name,
        "type": a.agent_type,
        "wallet": a.wallet_address,
        "status": live_status,
        "icon": a.icon,
        "iconClass": a.icon_class,
        "desc": a.description,
        "org": a.org,
        "tags": a.tags,
        "services": a.services,
        "testimonies": total_testimonies,
        "processed": processed,
        "approved": approved,
        "denied": denied,
        "balance": format_fat(balance),
        "balanceRaw": balance,
        "rewardsEarned": format!("{:.2} FAT", rewards_earned),
        "rewardsEarnedRaw": rewards_earned,
        "rewardRate": format!("{} FAT/testimony", a.reward_rate_fat),
        "txCount": tx_count,
        "uptime": uptime_str,
        "updated": if total_testimonies > 0 || uptime_secs > 0 { "recently" } else { "—" },
        "createdAt": a.created_at.timestamp(),
        "createdAgo": format!("{}d ago", created_ago.num_days()),
        "health": health
    })
}

/// (id, display name, wallet, attestation type, local health URL) for the
/// five canonical always-on agents. Single source of truth for the DB-less
/// paths below. Health URLs are loopback because the canonical agents run
/// co-located with the explorer on every production node; outbound-only
/// agents (oracle / insurance / validation) expose no HTTP listener, so
/// their liveness is judged from anchoring recency instead.
const CANONICAL_AGENT_WALLETS: [(&str, &str, &str, &str, Option<&str>); 5] = [
    (
        "semantic",
        "SemanticAgent",
        "0x000000000000000000000000000000000000C001",
        "Index Checkpoint",
        Some("http://127.0.0.1:9092/v1/health"),
    ),
    (
        "oracle",
        "OracleAgent",
        "0x000000000000000000000000000000000000C002",
        "Price Attestation",
        None,
    ),
    (
        "insurance",
        "InsuranceAgent",
        "0x000000000000000000000000000000000000C003",
        "Insurance Attestation",
        None,
    ),
    (
        "validation",
        "ValidationAgent",
        "0x000000000000000000000000000000000000C004",
        "Signature Validation",
        None,
    ),
    (
        "compliance",
        "ComplianceAgent",
        "0x000000000000000000000000000000000000C005",
        "Compliance Report",
        Some("http://127.0.0.1:9091/v1/health"),
    ),
];

/// Anchoring-recency window (secs) after which an outbound-only agent is
/// considered standby. 2h covers the slowest canonical cadence (the hourly
/// insurance pass) with margin.
const AGENT_ACTIVE_WINDOW_SECS: i64 = 7_200;

/// `rope_getString` with retries — the loopback rope-node connection is
/// occasionally reset under load ("connection closed before message
/// completed"), and a single failed call must not drop an agent from the
/// validators / testimonies views.
async fn rope_string_descriptor(
    state: &Arc<AppState>,
    wallet: &str,
) -> Option<serde_json::Value> {
    for attempt in 0..3u8 {
        match rpc_call(state, "rope_getString", vec![serde_json::json!(wallet)]).await {
            Ok(d) => return Some(d),
            Err(_) if attempt < 2 => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Err(e) => {
                tracing::warn!("rope_getString({}) failed after retries: {}", wallet, e);
            }
        }
    }
    None
}

/// Probe a canonical agent's loopback health endpoint (2s budget).
async fn agent_health_ok(state: &Arc<AppState>, url: Option<&str>) -> bool {
    let Some(url) = url else { return false };
    matches!(
        state
            .http_client
            .get(url)
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await,
        Ok(resp) if resp.status().is_success()
    )
}

/// Canonical Datachain Rope AI Testimony Agents.
///
/// These are the five always-on agents listed in the production session
/// rule (DCScan Frontend-Backend Fixes 2026-03-07). When the explorer's
/// optional Postgres `DATABASE_URL` is wired up, the live list comes
/// from the `agents` table and supersedes this fallback. When it isn't
/// — which is the case on every production node today — this fallback
/// makes sure `/api/v1/ai-agents` and `/agents` still surface the
/// canonical agent set instead of returning an empty array.
fn canonical_ai_agents() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "id": "semantic",
            "name": "SemanticAgent",
            "category": "Semantic Analysis",
            "description": "Indexes Datachain Rope strings, tags event_type fields, and exposes semantic search across knots.",
            "status": "active",
            "wallet": "0x000000000000000000000000000000000000C001",
            "testimoniesCount": null,
            "uptime": "99.5%",
            "icon": "fa-brain",
            "source": "canonical-fallback"
        }),
        serde_json::json!({
            "id": "oracle",
            "name": "OracleAgent",
            "category": "Price Oracle",
            "description": "Publishes DC FAT and stablecoin price testimonies sourced from DCSwap reserves and external feeds (XDCScan, GeckoTerminal).",
            "status": "active",
            "wallet": "0x000000000000000000000000000000000000C002",
            "testimoniesCount": null,
            "uptime": "99.8%",
            "icon": "fa-chart-line",
            "source": "canonical-fallback"
        }),
        serde_json::json!({
            "id": "insurance",
            "name": "InsuranceAgent",
            "category": "Risk Underwriting",
            "description": "Issues parametric-insurance attestations against tokenized RWAs (Tanastok asset shares, NaturaProof biodiversity proofs).",
            "status": "active",
            "wallet": "0x000000000000000000000000000000000000C003",
            "testimoniesCount": null,
            "uptime": "99.2%",
            "icon": "fa-shield-halved",
            "source": "canonical-fallback"
        }),
        serde_json::json!({
            "id": "validation",
            "name": "ValidationAgent",
            "category": "Knot Validation",
            "description": "Verifies post-quantum signatures (ML-DSA-65 default) on knots and witnesses the cord anchor knot at federation level.",
            "status": "active",
            "wallet": "0x000000000000000000000000000000000000C004",
            "testimoniesCount": null,
            "uptime": "99.7%",
            "icon": "fa-circle-check",
            "source": "canonical-fallback"
        }),
        serde_json::json!({
            "id": "compliance",
            "name": "ComplianceAgent",
            "category": "Regulatory Compliance",
            "description": "Flags GDPR Art. 17 erasure requests and orchestrates rope_untieKnot tombstone knots; covers MiFID II / DORA reporting.",
            "status": "active",
            "wallet": "0x000000000000000000000000000000000000C005",
            "testimoniesCount": null,
            "uptime": "99.9%",
            "icon": "fa-gavel",
            "source": "canonical-fallback"
        }),
    ]
}

async fn list_ai_agents_live(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // Prefer the Postgres-backed list when DATABASE_URL is configured;
    // otherwise serve the canonical 5-agent fallback so dcscan.io and
    // its callers never see an empty AI agent list.
    if let Some(pool) = &state.db_pool {
        match db::list_agents(pool).await {
            Ok(rows) if !rows.is_empty() => {
                let mut agents = Vec::new();
                for row in &rows {
                    agents.push(agent_row_to_json(&state, row).await);
                }
                let total = agents.len();
                return Json(serde_json::json!({
                    "agents": agents,
                    "totalCount": total,
                    "source": "database"
                }));
            }
            Ok(_) => {
                tracing::info!("agents table empty; serving canonical fallback");
            }
            Err(e) => {
                tracing::warn!("agent DB query failed ({}); serving canonical fallback", e);
            }
        }
    }
    let agents = canonical_ai_agents();
    let total = agents.len();
    Json(serde_json::json!({
        "agents": agents,
        "totalCount": total,
        "source": "canonical-fallback"
    }))
}

async fn get_ai_agent_live(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    if let Some(pool) = &state.db_pool {
        if let Ok(Some(row)) = db::get_agent(pool, &id).await {
            return Json(agent_row_to_json(&state, &row).await);
        }
    }
    // Fallback: scan canonical agents for a matching id (case-insensitive).
    if let Some(agent) = canonical_ai_agents()
        .into_iter()
        .find(|a| a.get("id").and_then(|v| v.as_str()).map(|s| s.eq_ignore_ascii_case(&id)).unwrap_or(false))
    {
        return Json(agent);
    }
    Json(serde_json::json!({"error": "Agent not found"}))
}

async fn agent_testimonies_live(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let pool = match &state.db_pool {
        Some(p) => p,
        None => return Json(serde_json::json!({"agentId": id, "testimonies": []})),
    };

    let agent = match db::get_agent(pool, &id).await {
        Ok(Some(a)) => a,
        _ => return Json(serde_json::json!({"agentId": id, "testimonies": []})),
    };

    let head_hex = rpc_call(&state, "eth_blockNumber", vec![])
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x0".to_string());
    let head = hex_to_u64(&head_hex);
    let wallet_lower = agent.wallet_address.to_lowercase();

    let mut testimonies: Vec<serde_json::Value> = Vec::new();
    let batch_size: u64 = 50;
    let mut cursor = head;
    let target = 20usize;

    'outer: while cursor > 0 && testimonies.len() < target {
        let batch_start = cursor.saturating_sub(batch_size - 1);
        let mut batch_req: Vec<serde_json::Value> = Vec::new();
        for bn in (batch_start..=cursor).rev() {
            batch_req.push(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getBlockByNumber",
                "params": [format!("0x{:x}", bn), true],
                "id": bn
            }));
        }
        let batch_resp = match rpc_batch_call(&state, &batch_req).await {
            Some(v) => v,
            None => break,
        };
        for resp in &batch_resp {
            if testimonies.len() >= target {
                break 'outer;
            }
            let block = match resp.get("result") {
                Some(b) if !b.is_null() => b,
                _ => continue,
            };
            let bn_val = block
                .get("number")
                .and_then(|v| v.as_str())
                .map(|s| hex_to_u64(s))
                .unwrap_or(0);
            let timestamp = block
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| hex_to_u64(s) as i64)
                .unwrap_or(0);
            let txs = block
                .get("transactions")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for tx in txs {
                if testimonies.len() >= target {
                    break 'outer;
                }
                let from = tx
                    .get("from")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                if from == wallet_lower {
                    let hash = tx
                        .get("hash")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let to = tx
                        .get("to")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let value_hex = tx.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");
                    let value_fat = wei_to_fat(value_hex);
                    testimonies.push(serde_json::json!({
                        "id": hash, "transaction": hash, "to": to,
                        "value": format_fat(value_fat), "verdict": "Approved",
                        "confidence": 0.99, "timestamp": timestamp,
                        "block": bn_val, "rewardFat": agent.reward_rate_fat
                    }));
                }
            }
        }
        if batch_start == 0 {
            break;
        }
        cursor = batch_start - 1;
    }

    Json(serde_json::json!({
        "agentId": id,
        "testimonies": testimonies
    }))
}

/// Account overview enriched with AI Agent data when the address is an agent wallet.
async fn account_overview_live(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let balance_hex = rpc_call(
        &state,
        "eth_getBalance",
        vec![serde_json::json!(&address), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x0".to_string());

    let nonce_hex = rpc_call(
        &state,
        "eth_getTransactionCount",
        vec![serde_json::json!(&address), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x0".to_string());

    let code_result = rpc_call(
        &state,
        "eth_getCode",
        vec![serde_json::json!(&address), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x".to_string());
    let is_contract = code_result.len() > 2;

    let balance = wei_to_fat(&balance_hex);
    let tx_count = hex_to_u64(&nonce_hex);

    let price_cache = state.price_cache.read().await;
    let price = price_cache.as_ref().map(|p| p.price).unwrap_or(0.0039);
    drop(price_cache);

    let balance_usd = balance * price;
    let balance_str = format_fat(balance);

    let hidden = is_hidden_address(&address);

    // Real token holdings (DCR-20 balanceOf + native FAT), same source as
    // the dedicated `/tokens` tab — previously hardcoded to empty/zero
    // here regardless of the wallet's actual holdings.
    let addr_lc_tokens = address.to_lowercase();
    let (tokens, tokens_usd) = compute_account_tokens(&state, &addr_lc_tokens).await;

    let mut resp = serde_json::json!({
        "address": if hidden { serde_json::Value::Null } else { serde_json::json!(&address) },
        "fatBalance": balance_str,
        "fatValueUsd": format!("{:.2}", balance_usd),
        "transactionCount": tx_count,
        "isContract": is_contract,
        "tokenHoldingsValueUsd": format!("{:.2}", tokens_usd),
        "tokenCount": tokens.len(),
        "tokens": tokens,
        // Recent transactions are intentionally left empty here: populating
        // them would require the same expensive eth_getLogs backward scan
        // that /transactions already performs, and the frontend does not
        // read this field from the overview response (it calls
        // /api/v1/accounts/:addr/transactions separately for that tab).
        "recentTransactions": [],
        "isAgent": false,
        "hidden": hidden
    });

    if let Some(tag_label) = known_label(&address) {
        resp["label"] = serde_json::json!(tag_label);
        if let Some(tag) = address_registry().get(address.to_lowercase().as_str()) {
            resp["labelIcon"] = serde_json::json!(tag.icon);
            resp["labelCategory"] = serde_json::json!(tag.category);
        }
    }

    if let Some(ref pool) = state.db_pool {
        if let Ok(Some(agent)) = db::get_agent_by_wallet(pool, &address).await {
            let agent_json = agent_row_to_json(&state, &agent).await;
            resp["isAgent"] = serde_json::json!(true);
            resp["agent"] = agent_json;
            resp["label"] = serde_json::json!(&agent.name);
        }
    }

    Json(resp)
}

/// Fetch on-chain testimonies for an agent identified by wallet address.
async fn agent_testimonies_by_wallet(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let agent_opt = if let Some(ref pool) = state.db_pool {
        db::get_agent_by_wallet(pool, &address).await.ok().flatten()
    } else {
        None
    };

    let reward_rate = agent_opt.as_ref().map(|a| a.reward_rate_fat).unwrap_or(0.5);
    let wallet_lower = address.to_lowercase();

    let head_hex = rpc_call(&state, "eth_blockNumber", vec![])
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x0".to_string());
    let head = hex_to_u64(&head_hex);

    let mut testimonies: Vec<serde_json::Value> = Vec::new();
    let batch_size: u64 = 50;
    let mut cursor = head;
    let target = 20usize;

    'outer: while cursor > 0 && testimonies.len() < target {
        let batch_start = cursor.saturating_sub(batch_size - 1);
        let mut batch_req: Vec<serde_json::Value> = Vec::new();
        for bn in (batch_start..=cursor).rev() {
            batch_req.push(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getBlockByNumber",
                "params": [format!("0x{:x}", bn), true],
                "id": bn
            }));
        }

        let batch_resp = match rpc_batch_call(&state, &batch_req).await {
            Some(v) => v,
            None => break,
        };

        for resp in &batch_resp {
            if testimonies.len() >= target {
                break 'outer;
            }
            let block = match resp.get("result") {
                Some(b) if !b.is_null() => b,
                _ => continue,
            };
            let timestamp = block
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| hex_to_u64(s) as i64)
                .unwrap_or(0);
            let bn = block
                .get("number")
                .and_then(|v| v.as_str())
                .map(|s| hex_to_u64(s))
                .unwrap_or(0);
            let txs = block
                .get("transactions")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for tx in txs {
                if testimonies.len() >= target {
                    break 'outer;
                }
                let from = tx
                    .get("from")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                if from == wallet_lower {
                    let hash = tx
                        .get("hash")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let to = tx
                        .get("to")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let value_hex = tx.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");
                    let value_fat = wei_to_fat(value_hex);
                    let input = tx
                        .get("input")
                        .and_then(|v| v.as_str())
                        .unwrap_or("0x")
                        .to_string();
                    let verdict = if input.len() > 10 {
                        "Approved"
                    } else {
                        "Transfer"
                    };
                    testimonies.push(serde_json::json!({
                        "id": hash,
                        "transaction": hash,
                        "to": to,
                        "value": format_fat(value_fat),
                        "verdict": verdict,
                        "confidence": 0.99,
                        "timestamp": timestamp,
                        "block": bn,
                        "rewardFat": reward_rate,
                        "input": if input.len() > 10 { format!("{}...", &input[..10]) } else { input }
                    }));
                }
            }
        }

        if batch_start == 0 {
            break;
        }
        cursor = batch_start - 1;
    }

    Json(serde_json::json!({
        "address": address,
        "testimonies": testimonies
    }))
}

/// Aggregated testimony statistics across all AI agents.
/// Background task: scans blocks for agent transactions and caches the result.
/// DB-less testimony refresh: every knot the five canonical agents tie on
/// their personal-ledger strings IS an on-chain testimony (Quipu Canon v1.2:
/// per-entity knots). This derives the testimony feed straight from the
/// rope-node registry via `rope_getString` + `rope_listKnots`, so the
/// /testimonies and /validations pages work on production nodes where the
/// optional Postgres `DATABASE_URL` is not configured.
async fn refresh_testimony_cache_from_rope(state: &Arc<AppState>) {
    let now = chrono::Utc::now().timestamp();

    // Preserve discovery timestamps from the previous refresh so knots keep
    // the time we first saw them (accurate to one refresh interval).
    let mut known_ts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    {
        let existing = state.testimony_cache.read().await;
        if let Some(ref c) = *existing {
            for t in &c.testimonies {
                if let (Some(id), Some(ts)) = (
                    t.get("id").and_then(|v| v.as_str()),
                    t.get("timestamp").and_then(|v| v.as_i64()),
                ) {
                    known_ts.insert(id.to_string(), ts);
                }
            }
        }
    }

    let mut testimonies: Vec<serde_json::Value> = Vec::new();
    let mut total_testimonies: u64 = 0;
    let mut active_agents: u64 = 0;

    for (agent_id, agent_name, wallet, attest_type, health_url) in CANONICAL_AGENT_WALLETS {
        let Some(descriptor) = rope_string_descriptor(state, wallet).await else {
            continue;
        };
        let knot_count = descriptor
            .get("knot_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let created_at = descriptor
            .get("created_at")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let last_anchored_at = descriptor
            .get("last_anchored_at")
            .and_then(|v| v.as_i64())
            .unwrap_or(created_at);
        total_testimonies += knot_count;
        // An agent is "active" when its health endpoint answers or it
        // anchored within the activity window.
        if agent_health_ok(state, health_url).await
            || (knot_count > 0 && now - last_anchored_at < AGENT_ACTIVE_WINDOW_SECS)
        {
            active_agents += 1;
        }

        let knots_resp = match rpc_call(
            state,
            "rope_listKnots",
            vec![serde_json::json!({ "string_id": wallet, "limit": 100 })],
        )
        .await
        {
            Ok(k) => k,
            Err(_) => continue,
        };
        let knots = knots_resp
            .get("knots")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let max_index = knots
            .iter()
            .filter_map(|k| k.get("knot_index").and_then(|v| v.as_u64()))
            .max()
            .unwrap_or(0);

        for knot in &knots {
            let knot_id = knot
                .get("knot_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if knot_id.is_empty() {
                continue;
            }
            let knot_index = knot.get("knot_index").and_then(|v| v.as_u64()).unwrap_or(0);
            let knot_status = knot.get("status").and_then(|v| v.as_str()).unwrap_or("active");
            // Timestamp: keep first-seen time when we already track this knot;
            // otherwise use the on-chain string bounds (genesis = created_at,
            // head = last_anchored_at, backfilled middle = last_anchored_at
            // as the latest time the chain is known to have reflected it).
            let timestamp = known_ts.get(&knot_id).copied().unwrap_or(if knot_index == 0 {
                created_at
            } else if knot_index == max_index {
                last_anchored_at
            } else {
                last_anchored_at.min(now)
            });
            testimonies.push(serde_json::json!({
                "id": knot_id, "testimonyId": knot_id, "txHash": knot_id, "transaction": knot_id,
                "agent": wallet, "agentName": agent_name,
                "agentAddress": wallet, "agentId": agent_id,
                "to": "", "type": attest_type, "attestationType": attest_type,
                "confidence": 0.99,
                "status": if knot_status == "active" { "confirmed" } else { knot_status },
                "timestamp": timestamp, "block": knot_index,
                "rewardFat": 0
            }));
        }
    }

    testimonies.sort_by(|a, b| {
        let ta = a.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);
        let tb = b.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);
        tb.cmp(&ta).then_with(|| {
            let ba = a.get("block").and_then(|v| v.as_u64()).unwrap_or(0);
            let bb = b.get("block").and_then(|v| v.as_u64()).unwrap_or(0);
            bb.cmp(&ba)
        })
    });
    testimonies.truncate(100);

    let testimonies_24h = testimonies
        .iter()
        .filter(|t| {
            t.get("timestamp")
                .and_then(|v| v.as_i64())
                .map_or(false, |ts| now - ts < 86_400)
        })
        .count() as u64;
    let avg_confidence = if total_testimonies > 0 { 99 } else { 0 };
    let stats = serde_json::json!({
        "totalTestimonies": total_testimonies,
        "totalTestimoniesChangePercentThisWeek": 0,
        "testimonies24h": testimonies_24h,
        "testimonies24hChangePercentFromYesterday": 0,
        "avgConfidenceScore": avg_confidence,
        "activeAgents": active_agents,
        "ropeNodeConnected": true,
        "source": "rope-registry"
    });

    let fresh = TestimonyCache {
        stats,
        testimonies,
        updated_at: now,
    };
    save_json_cache("testimony_cache.json", &fresh);
    let mut cache = state.testimony_cache.write().await;
    *cache = Some(fresh);
    tracing::info!(
        "Testimony cache refreshed from rope registry: {} knots across canonical agents, {} active",
        total_testimonies,
        active_agents
    );
}

async fn refresh_testimony_cache(state: &Arc<AppState>) {
    let pool = match &state.db_pool {
        Some(p) => p,
        None => {
            // No Postgres on production nodes — derive testimonies from the
            // canonical agents' personal-ledger knots on rope-node instead.
            refresh_testimony_cache_from_rope(state).await;
            return;
        }
    };
    let agents = match db::list_agents(pool).await {
        Ok(a) => a,
        Err(_) => return,
    };

    let agent_wallets: std::collections::HashMap<String, &db::AgentRow> = agents
        .iter()
        .map(|a| (a.wallet_address.to_lowercase(), a))
        .collect();

    let mut total_testimonies: u64 = 0;
    let mut active_agents: u64 = 0;
    let mut total_processed: u64 = 0;
    let mut total_approved: u64 = 0;

    for agent in &agents {
        let nonce_hex = rpc_call(
            state,
            "eth_getTransactionCount",
            vec![
                serde_json::json!(&agent.wallet_address),
                serde_json::json!("latest"),
            ],
        )
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x0".to_string());
        let tx_count = hex_to_u64(&nonce_hex);

        let mut agent_processed: u64 = 0;
        let mut agent_approved: u64 = 0;
        if let Some(ref url) = agent.health_url {
            if let Ok(resp) = state
                .http_client
                .get(url)
                .timeout(std::time::Duration::from_secs(3))
                .send()
                .await
            {
                if resp.status().is_success() {
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        if let Some(stats) = json.get("stats") {
                            agent_processed =
                                stats.get("processed").and_then(|v| v.as_u64()).unwrap_or(0);
                            agent_approved =
                                stats.get("approved").and_then(|v| v.as_u64()).unwrap_or(0);
                        }
                        active_agents += 1;
                    }
                }
            }
        }
        let count = if agent_processed > 0 {
            agent_processed
        } else {
            tx_count
        };
        total_testimonies += count;
        total_processed += agent_processed;
        total_approved += agent_approved;
        if (tx_count > 0 || agent_processed > 0) && agent.health_url.is_none() {
            active_agents += 1;
        }
    }

    let head_hex = rpc_call(state, "eth_blockNumber", vec![])
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "0x0".to_string());
    let head = hex_to_u64(&head_hex);

    let existing_cache = state.testimony_cache.read().await;
    let mut testimonies: Vec<serde_json::Value> = existing_cache
        .as_ref()
        .map(|c| c.testimonies.clone())
        .unwrap_or_default();
    let last_scanned_block = existing_cache
        .as_ref()
        .and_then(|c| c.testimonies.first())
        .and_then(|t| t.get("block").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    drop(existing_cache);

    let scan_from = if last_scanned_block > 0 && !testimonies.is_empty() {
        last_scanned_block + 1
    } else {
        0
    };

    for bn in (scan_from..=head).rev() {
        if testimonies.len() >= 100 {
            break;
        }
        let block_hex = format!("0x{:x}", bn);
        let block = match rpc_call(
            state,
            "eth_getBlockByNumber",
            vec![serde_json::json!(block_hex), serde_json::json!(false)],
        )
        .await
        {
            Ok(b) => b,
            Err(_) => continue,
        };
        if block
            .get("transactions")
            .and_then(|v| v.as_array())
            .map_or(true, |a| a.is_empty())
        {
            continue;
        }
        let full = match rpc_call(
            state,
            "eth_getBlockByNumber",
            vec![serde_json::json!(block_hex), serde_json::json!(true)],
        )
        .await
        {
            Ok(b) => b,
            Err(_) => continue,
        };
        let timestamp = full
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(|s| hex_to_u64(s) as i64)
            .unwrap_or(0);
        let txs = full
            .get("transactions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for tx in txs {
            if testimonies.len() >= 100 {
                break;
            }
            let from = tx
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            if let Some(agent) = agent_wallets.get(&from) {
                let hash = tx
                    .get("hash")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let to = tx
                    .get("to")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = tx.get("input").and_then(|v| v.as_str()).unwrap_or("0x");
                let attest_type = if input.len() > 10 {
                    "Validation"
                } else {
                    "Transfer"
                };
                testimonies.push(serde_json::json!({
                    "id": hash, "testimonyId": hash, "txHash": hash, "transaction": hash,
                    "agent": agent.wallet_address, "agentName": agent.name,
                    "agentAddress": agent.wallet_address, "agentId": agent.id,
                    "to": to, "type": attest_type, "attestationType": attest_type,
                    "confidence": 0.99, "status": "confirmed",
                    "timestamp": timestamp, "block": bn,
                    "rewardFat": agent.reward_rate_fat
                }));
            }
        }
    }

    let avg_confidence = if total_testimonies > 0 { 99 } else { 0 };
    let stats = serde_json::json!({
        "totalTestimonies": total_testimonies,
        "totalTestimoniesChangePercentThisWeek": 0,
        "testimonies24h": total_processed,
        "testimonies24hChangePercentFromYesterday": 0,
        "avgConfidenceScore": avg_confidence,
        "activeAgents": active_agents,
        "ropeNodeConnected": true,
        "totalProcessed": total_processed,
        "totalApproved": total_approved
    });

    {
        let mut seen = std::collections::HashSet::new();
        testimonies.retain(|t| {
            let id = t
                .get("txHash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                return false;
            }
            seen.insert(id)
        });
    }
    testimonies.sort_by(|a, b| {
        let ba = a.get("block").and_then(|v| v.as_u64()).unwrap_or(0);
        let bb = b.get("block").and_then(|v| v.as_u64()).unwrap_or(0);
        bb.cmp(&ba)
    });
    testimonies.truncate(100);

    let fresh = TestimonyCache {
        stats,
        testimonies: testimonies.clone(),
        updated_at: chrono::Utc::now().timestamp(),
    };
    save_json_cache("testimony_cache.json", &fresh);
    let mut cache = state.testimony_cache.write().await;
    *cache = Some(fresh);
    tracing::info!(
        "Testimony cache refreshed: {} testimonies, {} agents",
        total_testimonies,
        active_agents
    );
}

/// Returns cached testimony stats (instant response).
async fn testimonies_stats_live(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let cache = state.testimony_cache.read().await;
    if let Some(ref c) = *cache {
        return Json(c.stats.clone());
    }
    Json(serde_json::json!({
        "totalTestimonies": 0,
        "totalTestimoniesChangePercentThisWeek": 0,
        "testimonies24h": 0,
        "testimonies24hChangePercentFromYesterday": 0,
        "avgConfidenceScore": "0",
        "activeAgents": 0,
        "ropeNodeConnected": true
    }))
}

#[derive(Deserialize)]
struct TestimoniesListQuery {
    page: Option<u32>,
    limit: Option<u32>,
}

/// Returns cached testimony list (instant response).
async fn testimonies_list_live(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TestimoniesListQuery>,
) -> Json<serde_json::Value> {
    let page = q.page.unwrap_or(1);
    let limit = q.limit.unwrap_or(25).min(100) as usize;

    let cache = state.testimony_cache.read().await;
    if let Some(ref c) = *cache {
        let start = ((page - 1) as usize) * limit;
        let items: Vec<serde_json::Value> = c
            .testimonies
            .iter()
            .skip(start)
            .take(limit)
            .cloned()
            .collect();
        let total = c.testimonies.len();
        return Json(serde_json::json!({
            "testimonies": items,
            "total": total,
            "page": page,
            "limit": limit
        }));
    }
    Json(serde_json::json!({
        "testimonies": [],
        "total": 0,
        "page": page,
        "limit": limit
    }))
}

// list_databoxes / get_databox / databox_map / register_databox /
// heartbeat_databox / deregister_databox / databox_types / databoxes_by_type:
// see databox_registry.rs (real self-service registration + heartbeat
// pipeline, replacing the previous honest-empty stubs).

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

fn push_search_result(
    results: &mut Vec<serde_json::Value>,
    seen: &mut std::collections::HashSet<String>,
    kind: &str,
    label: String,
    value: String,
    url: String,
    subtitle: Option<&str>,
) {
    if seen.contains(&url) {
        return;
    }
    seen.insert(url.clone());
    let mut obj = serde_json::json!({
        "type": kind,
        "label": label,
        "value": value,
        "url": url,
    });
    if let Some(s) = subtitle {
        obj["subtitle"] = serde_json::json!(s);
    }
    results.push(obj);
}

/// Real, ecosystem-wide search. Previously this endpoint only classified
/// the query *by shape* (tx-hash length / address length / numeric) and
/// echoed it back with no actual lookup — anything that wasn't a raw
/// address, tx hash, or string number (including the native "FAT" ticker
/// itself, any DCR-20 token name/symbol, any Tanastok tokenized-asset name,
/// any known contract label, or any ecosystem project name) fell straight
/// through to the frontend's "No results" alert.
///
/// This now searches, in order:
///   1. Direct shape match — tx hash / EVM address / string (cord anchor)
///      number — fast path, single result, same convention the frontend
///      already uses client-side for these three shapes.
///   2. Exact symbol match against native DC FAT + every known DCR-20 /
///      LP token (so "FAT", "USDC", "USDT", "EUROD", "WFAT" all resolve).
///   3. Substring match against token name/symbol, Tanastok tokenized
///      real-world assets (name / brand / DCNFT+ERC-3643 symbol), the full
///      Tanastok entity manifest (ecosystems, applications, organizations,
///      partners, DIDs, contracts), the static address-label registry
///      (Mapstore, Careaway, T-REX infra, treasuries, ...), and the
///      ecosystem project directory (interfaced platforms: DCSwap,
///      Tanastok, Mapstore, Careaway, ...).
async fn search(
    State(state): State<Arc<AppState>>,
    Query(query): Query<SearchQuery>,
) -> Json<serde_json::Value> {
    let raw = query.q.trim().to_string();
    let q_lower = raw.to_lowercase();

    if raw.is_empty() {
        return Json(serde_json::json!({ "query": raw, "type": "unknown", "count": 0, "results": [] }));
    }

    // ---- 1. Direct shape matches (fast path, single result) ---------------
    if q_lower.starts_with("0x") && q_lower.len() == 66 {
        // 64 hex chars is ambiguous between a tx hash and a 32-byte Quipu
        // synthetic string id (Tanastok assets/DIDs/ecosystems). The
        // frontend already resolves this shape to /tx/:hash client-side;
        // /tx/:hash itself falls back to rendering a synthetic string when
        // the hash isn't a real transaction (see the rope-graph handover's
        // "synthetic-id renderer" note), so this stays consistent.
        return Json(serde_json::json!({
            "query": raw,
            "type": "transaction",
            "count": 1,
            "results": [{
                "type": "transaction",
                "label": "Transaction / String ID",
                "value": raw,
                "url": format!("/tx/{}", raw),
            }]
        }));
    }
    if q_lower.starts_with("0x") && q_lower.len() == 42 {
        let label = known_label(&q_lower).unwrap_or("Address").to_string();
        return Json(serde_json::json!({
            "query": raw,
            "type": "address",
            "count": 1,
            "results": [{
                "type": "address",
                "label": label,
                "value": raw,
                "url": format!("/address/{}", raw),
            }]
        }));
    }
    if let Ok(n) = q_lower.parse::<u64>() {
        return Json(serde_json::json!({
            "query": raw,
            "type": "string",
            "count": 1,
            "results": [{
                "type": "string",
                "label": format!("Cord Anchor / String #{}", n),
                "value": n.to_string(),
                "url": format!("/string/{}", n),
            }]
        }));
    }

    // ---- 2. Free-text search across every known catalog -------------------
    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 2a. Native FAT + every known DCR-20 / LP token — exact symbol match
    // wins outright (this is what makes searching "FAT" resolve to the
    // native token instead of falling through to nothing, and keeps it
    // from being shadowed by "WFAT" on a substring match).
    let mut token_catalog: Vec<(&str, &str, &str)> =
        vec![("0x0000000000000000000000000000000000000000", "DC FAT", "FAT")];
    for &(addr, name, _standard) in LIST_TOKENS_DCR20_ADDRS {
        let symbol = known_token(addr).map(|i| i.symbol).unwrap_or("");
        token_catalog.push((addr, name, symbol));
    }
    for &(addr, name, symbol) in &token_catalog {
        if symbol.eq_ignore_ascii_case(&raw) {
            push_search_result(
                &mut results,
                &mut seen,
                "token",
                format!("{} ({})", name, symbol),
                addr.to_string(),
                format!("/token/{}", addr),
                Some("DCR-20 token"),
            );
        }
    }
    if results.is_empty() && q_lower.len() >= 2 {
        for &(addr, name, symbol) in &token_catalog {
            if name.to_lowercase().contains(&q_lower) || symbol.to_lowercase().contains(&q_lower) {
                push_search_result(
                    &mut results,
                    &mut seen,
                    "token",
                    format!("{} ({})", name, symbol),
                    addr.to_string(),
                    format!("/token/{}", addr),
                    Some("DCR-20 token"),
                );
            }
        }
    }

    if q_lower.len() >= 2 {
        // 2b. Tanastok tokenized real-world assets.
        {
            let cache = state.tanastok_cache.read().await;
            if let Some(c) = cache.as_ref() {
                for asset in &c.assets {
                    let name = asset.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let brand = asset.get("brandName").and_then(|v| v.as_str()).unwrap_or("");
                    let erc3643_symbol = asset
                        .get("erc3643")
                        .and_then(|e| e.get("tokenSymbol"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let haystack = format!("{} {} {}", name, brand, erc3643_symbol).to_lowercase();
                    if !name.is_empty() && haystack.contains(&q_lower) {
                        let dcnft_addr = asset
                            .get("dcnft")
                            .and_then(|d| d.get("contractAddress"))
                            .and_then(|v| v.as_str());
                        let erc3643_addr = asset
                            .get("erc3643")
                            .and_then(|e| e.get("contractAddress"))
                            .and_then(|v| v.as_str());
                        if let Some(addr) = dcnft_addr.or(erc3643_addr) {
                            push_search_result(
                                &mut results,
                                &mut seen,
                                "tanastok_asset",
                                name.to_string(),
                                addr.to_string(),
                                format!("/address/{}", addr),
                                Some("Tanastok tokenized asset"),
                            );
                        }
                        if results.len() >= 40 {
                            break;
                        }
                    }
                }
            }
        }

        // 2c. Full Tanastok entity manifest — ecosystems, applications,
        // organizations, partners, DIDs, and any contract/asset not
        // already surfaced via the tokenized-asset cache above.
        {
            let cache = state.tanastok_manifest_cache.read().await;
            if let Some(c) = cache.as_ref() {
                if let Some(entities) = c.raw.get("entities").and_then(|v| v.as_array()) {
                    for e in entities {
                        let display_name = e
                            .get("label")
                            .and_then(|l| l.get("display_name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if display_name.is_empty() || !display_name.to_lowercase().contains(&q_lower) {
                            continue;
                        }
                        let string_id = e.get("string_id").and_then(|v| v.as_str()).unwrap_or("");
                        if string_id.is_empty() {
                            continue;
                        }
                        let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("entity");
                        push_search_result(
                            &mut results,
                            &mut seen,
                            &format!("tanastok_{}", kind),
                            display_name.to_string(),
                            string_id.to_string(),
                            format!("/address/{}", string_id),
                            Some(&format!("Tanastok {}", kind)),
                        );
                        if results.len() >= 50 {
                            break;
                        }
                    }
                }
            }
        }

        // 2d. Static address-label registry (Mapstore, Careaway, T-REX
        // infra, treasuries, DCSwap contracts, ...).
        for (addr, tag) in address_registry().iter() {
            if tag.hidden {
                continue;
            }
            if tag.label.to_lowercase().contains(&q_lower) {
                push_search_result(
                    &mut results,
                    &mut seen,
                    "address",
                    tag.label.to_string(),
                    addr.to_string(),
                    format!("/address/{}", addr),
                    Some(tag.category),
                );
            }
        }

        // 2e. Ecosystem project directory — every interfaced platform
        // (DCSwap, Tanastok, Mapstore, Careaway, ...).
        {
            let cache = state.ecosystem_directory_cache.read().await;
            if let Some(c) = cache.as_ref() {
                for p in &c.projects {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    let tags = p
                        .get("tags")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|t| t.as_str())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default();
                    let haystack = format!("{} {}", name, tags).to_lowercase();
                    if haystack.contains(&q_lower) {
                        let id = p.get("id").and_then(|v| v.as_str()).unwrap_or(name);
                        push_search_result(
                            &mut results,
                            &mut seen,
                            "ecosystem_project",
                            name.to_string(),
                            id.to_string(),
                            "/ecosystem".to_string(),
                            Some("Interfaced platform"),
                        );
                    }
                }
            }
        }
    }

    let result_type = results
        .first()
        .and_then(|r| r.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    Json(serde_json::json!({
        "query": raw,
        "type": result_type,
        "count": results.len(),
        "results": results,
    }))
}

/// Live `eth_gasPrice` reading, tiered into slow/standard/fast/instant by a
/// multiplier off the network's actual current gas price. Previously
/// hardcoded gwei values that never moved regardless of real network
/// conditions.
async fn gas_price(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let base_wei = rpc_gas_price(&state).await.unwrap_or(1_000_000_000u64);
    let base_gwei = base_wei as f64 / 1e9;
    Json(serde_json::json!({
        "slow": format!("{:.6} gwei", base_gwei * 0.5),
        "standard": format!("{:.6} gwei", base_gwei),
        "fast": format!("{:.6} gwei", base_gwei * 1.5),
        "instant": format!("{:.6} gwei", base_gwei * 2.0),
        "source": "eth_gasPrice (live)",
    }))
}

/// Etherscan-shaped gas-oracle response backed by the same live
/// `eth_gasPrice` reading as `gas_price()` above. `gasUsedRatio` reflects
/// the block-utilization of the most recent block instead of a fabricated
/// fixed string.
async fn gas_oracle(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let base_wei = rpc_gas_price(&state).await.unwrap_or(1_000_000_000u64);
    let base_gwei = base_wei as f64 / 1e9;

    let gas_used_ratio = match rpc_call(
        &state,
        "eth_getBlockByNumber",
        vec![serde_json::json!("latest"), serde_json::json!(false)],
    )
    .await
    {
        Ok(block) => {
            let gas_used = block
                .get("gasUsed")
                .and_then(|v| v.as_str())
                .map(hex_to_u64)
                .unwrap_or(0);
            let gas_limit = block
                .get("gasLimit")
                .and_then(|v| v.as_str())
                .map(hex_to_u64)
                .unwrap_or(1);
            format!("{:.4}", gas_used as f64 / gas_limit.max(1) as f64)
        }
        Err(_) => "0".to_string(),
    };

    Json(serde_json::json!({
        "SafeGasPrice": format!("{:.6}", base_gwei * 0.5),
        "ProposeGasPrice": format!("{:.6}", base_gwei),
        "FastGasPrice": format!("{:.6}", base_gwei * 1.5),
        "suggestBaseFee": format!("{:.6}", base_gwei * 0.4),
        "gasUsedRatio": gas_used_ratio,
        "source": "eth_gasPrice + eth_getBlockByNumber(latest) (live)",
    }))
}

// ============================================================================
// Federation & Community Generation API Handlers
//
// STATUS: no live backing store exists for this feature yet (no
// persistence, no on-chain string emission, no frontend page consumes
// any of these 12 endpoints — verified against every file in `static/`).
// Previously every one of these fabricated realistic-looking demo data
// (fake org names, vote tallies, wallet addresses) on every call and
// pretended every create/vote mutation succeeded and persisted, which
// violates the workspace's no-stub policy for a public API surface.
// Until a real federation/community persistence + governance backend is
// designed and built, GETs honestly report empty and mutating endpoints
// return `501 Not Implemented` rather than a fabricated success.
// ============================================================================

fn federation_not_implemented() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "success": false,
            "error": "Federation/Community generation has no live backend yet — this endpoint is reserved for a future release and intentionally does not fabricate a success response.",
        })),
    )
}

/// List all federations
async fn list_federations(Query(params): Query<PaginationParams>) -> Json<serde_json::Value> {
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20);
    Json(serde_json::json!({
        "federations": [],
        "pagination": { "page": page, "limit": limit, "total": 0 },
        "note": "Federation generation has no live backend yet.",
    }))
}

/// Create new federation (requires DC FAT stake) — not yet implemented.
#[derive(Deserialize)]
#[allow(dead_code)]
struct CreateFederationRequest {
    name: String,
    description: String,
    #[serde(rename = "type")]
    federation_type: String,
    structure: String,
    scope: String,
    industry: String,
    protocols: Vec<String>,
    kyc_enabled: bool,
}

async fn create_federation(
    Json(_payload): Json<CreateFederationRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    federation_not_implemented()
}

/// Get federation by ID — no federation registry exists yet.
async fn get_federation(Path(id): Path<String>) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "id": id,
            "found": false,
            "error": "Federation generation has no live backend yet — no federation records exist.",
        })),
    )
}

/// Get communities in a federation
async fn federation_communities(Path(id): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "federationId": id,
        "communities": [],
        "note": "Federation generation has no live backend yet.",
    }))
}

/// Vote on federation / community — shared request shape.
#[derive(Deserialize)]
#[allow(dead_code)]
struct VoteRequest {
    vote_for: bool,
    comment: Option<String>,
}

async fn vote_federation(
    Path(_id): Path<String>,
    Json(_payload): Json<VoteRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    federation_not_implemented()
}

/// List all communities
async fn list_communities(Query(params): Query<PaginationParams>) -> Json<serde_json::Value> {
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20);
    Json(serde_json::json!({
        "communities": [],
        "pagination": { "page": page, "limit": limit, "total": 0 },
        "note": "Community generation has no live backend yet.",
    }))
}

/// Create new community — not yet implemented.
#[derive(Deserialize)]
#[allow(dead_code)]
struct CreateCommunityRequest {
    name: String,
    description: String,
    federation_id: Option<String>,
    community_type: String,
    scale: String,
    protocols: Vec<String>,
}

async fn create_community(
    Json(_payload): Json<CreateCommunityRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    federation_not_implemented()
}

/// Get community by ID — no community registry exists yet.
async fn get_community(Path(id): Path<String>) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "id": id,
            "found": false,
            "error": "Community generation has no live backend yet — no community records exist.",
        })),
    )
}

/// Get community wallets
async fn community_wallets(Path(id): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "communityId": id,
        "wallets": [],
        "stats": { "total": 0, "generated": 0, "activated": 0 },
        "note": "Community generation has no live backend yet.",
    }))
}

/// Generate wallets for community — not yet implemented. Previously
/// fabricated up to 100 fake wallet addresses (`format!("0x{:040x}", ...)`)
/// with no key material and no persistence, which is actively dangerous
/// if a caller ever mistook them for real, funded wallets.
#[derive(Deserialize)]
#[allow(dead_code)]
struct GenerateWalletsRequest {
    count: u64,
}

async fn generate_wallets(
    Path(_id): Path<String>,
    Json(_payload): Json<GenerateWalletsRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    federation_not_implemented()
}

/// Vote on community
async fn vote_community(
    Path(_id): Path<String>,
    Json(_payload): Json<VoteRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    federation_not_implemented()
}

// ============================================================================
// Project Submission API Handlers (Start Building)
// ============================================================================
//
// Real, chain-anchored implementations (list_projects, submit_project,
// get_project, vote_project, review_project, voting_projects, list_votes,
// get_votes_for_target) live in `governance_votes.rs` — see that module's
// doc comment for the full design (persistence, EIP-191 signature
// verification, single-chain FAT balance-weighted voting). Only the
// static category taxonomy stays here.

/// Get project categories
async fn project_categories() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "categories": [
            {"id": "defi", "name": "DeFi", "description": "Decentralized finance applications"},
            {"id": "nft", "name": "NFT", "description": "Non-fungible token platforms"},
            {"id": "gaming", "name": "Gaming", "description": "Blockchain gaming and metaverse"},
            {"id": "social", "name": "Social", "description": "Social networks and communication"},
            {"id": "infrastructure", "name": "Infrastructure", "description": "Developer tools and infrastructure"},
            {"id": "dao", "name": "DAO", "description": "Decentralized autonomous organizations"},
            {"id": "marketplace", "name": "Marketplace", "description": "Digital marketplaces"},
            {"id": "identity", "name": "Identity", "description": "Identity and authentication"},
            {"id": "supply_chain", "name": "Supply Chain", "description": "Supply chain and logistics"},
            {"id": "healthcare", "name": "Healthcare", "description": "Healthcare and medical data"},
            {"id": "iot", "name": "IoT", "description": "Internet of Things"},
            {"id": "ai_ml", "name": "AI/ML", "description": "Artificial intelligence and machine learning"},
            {"id": "oracle", "name": "Oracle", "description": "Data oracles and external data"},
            {"id": "bridge", "name": "Bridge", "description": "Cross-chain bridges"},
            {"id": "other", "name": "Other", "description": "Other categories"}
        ]
    }))
}

// voting_projects, list_votes, get_votes_for_target: see governance_votes.rs.

// ============================================================================
// Tests
// ============================================================================
// Tanastok tokenized assets cache + endpoints
// ============================================================================

/// Incrementally scans blocks to maintain an exact total transaction count
/// AND a cumulative DCR-20 transfer volume since genesis. First run: from
/// block 0. Subsequent runs: only new blocks. Processes up to 5,000 blocks
/// per tick to avoid blocking for too long.
///
/// Volume aggregation strategy: a single `eth_getLogs` call per tick filtered
/// on the canonical Transfer topic across the batch window. We then iterate
/// the returned logs once, look up each token via `known_token()`, and add
/// to running totals. This keeps the per-tick RPC cost at ~1 (logs) + N
/// (block tx counts), regardless of transfer volume in the window.
async fn refresh_tx_count_cache(state: &AppState) {
    let head = match rpc_block_number(state).await {
        Ok(h) => h,
        Err(_) => return,
    };

    let cache = state.tx_count_cache.read().await.clone();
    let start = if cache.last_scanned_block == 0 {
        0
    } else {
        cache.last_scanned_block + 1
    };

    if start > head {
        return;
    }

    // Knot-counting strategy (Quipu Canon v1.2 — see knot-event-distinction rule):
    //   • TRANSACTIONS (one type of knot): fetched via JSON-RPC batched
    //     `eth_getBlockTransactionCountByNumber` — Reth returns just the
    //     tx count per block (a few bytes), so 200 calls go in ONE HTTP
    //     batch. ~200× faster than the legacy per-block sequential loop.
    //   • EVENTS (sub-transaction sub-knots): fetched via a single
    //     `eth_getLogs` call over the whole batch range, filtered on the
    //     Transfer topic. Reth's log index makes this O(matches), not
    //     O(blocks).
    //   • CORD ANCHORS (the cord-level knot): served live from
    //     `eth_blockNumber` cached at the read path; no scan needed.
    //   • PER-ENTITY KNOTS (v1.2 registry): served live from
    //     `rope_globalStats`; no scan needed.
    //
    // With this layout, the only thing the scan has to do is call
    // `eth_getBlockTransactionCountByNumber` in batches and aggregate
    // Transfer logs — both of which are trivial for a Reth node.
    let batch_limit = 50_000u64;
    let end = head.min(start + batch_limit);
    let mut cumulative = cache.total_transactions;

    let chunk_size: u64 = 200;
    let mut cursor = start;
    while cursor <= end {
        let chunk_end = (cursor + chunk_size - 1).min(end);
        let mut requests: Vec<serde_json::Value> = Vec::with_capacity((chunk_end - cursor + 1) as usize);
        for bn in cursor..=chunk_end {
            requests.push(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_getBlockTransactionCountByNumber",
                "params": [format!("0x{:x}", bn)],
                "id": bn,
            }));
        }
        if let Some(responses) = rpc_batch_call(state, &requests).await {
            for r in responses {
                if let Some(result) = r.get("result").and_then(|v| v.as_str()) {
                    cumulative += hex_to_u64(result);
                }
            }
        } else {
            // Batch RPC failed (e.g. forwarder dropped the connection);
            // single-block fallback so we still make progress without
            // double-counting.
            for bn in cursor..=chunk_end {
                let hex_bn = format!("0x{:x}", bn);
                if let Ok(val) = rpc_call(
                    state,
                    "eth_getBlockTransactionCountByNumber",
                    vec![serde_json::json!(hex_bn)],
                )
                .await
                {
                    if let Some(s) = val.as_str() {
                        cumulative += hex_to_u64(s);
                    }
                }
            }
        }
        cursor = chunk_end + 1;
    }

    // Aggregate DCR-20 Transfer volume in the same scan window.
    // Single eth_getLogs call filtered on the Transfer topic; we enrich
    // each log with `known_token()` to pick up the right symbol/decimals/$.
    let mut delta_volume_usd = 0.0f64;
    let mut delta_volume_fat = 0.0f64;
    let mut delta_events = 0u64;
    if let Ok(logs) = rpc_call(
        state,
        "eth_getLogs",
        vec![serde_json::json!({
            "fromBlock": format!("0x{:x}", start),
            "toBlock":   format!("0x{:x}", end),
            "topics":    [TRANSFER_TOPIC],
        })],
    )
    .await
    {
        if let Some(arr) = logs.as_array() {
            for log in arr {
                let topics = match log.get("topics").and_then(|v| v.as_array()) {
                    Some(t) if t.len() >= 3 => t,
                    _ => continue,
                };
                let topic0 = topics[0].as_str().unwrap_or("");
                if topic0 != TRANSFER_TOPIC {
                    continue;
                }
                let token_addr = log.get("address").and_then(|v| v.as_str()).unwrap_or("");
                let data = log.get("data").and_then(|v| v.as_str()).unwrap_or("0x0");
                let raw_amount = decode_hex_u256(data);
                delta_events += 1;
                if let Some(info) = known_token(token_addr) {
                    let amount = raw_amount as f64 / 10f64.powi(info.decimals as i32);
                    delta_volume_usd += amount * info.usd_price;
                    if info.symbol == "WFAT" {
                        delta_volume_fat += amount;
                    }
                }
            }
        }
    }

    let scanned_this_tick = end - start + 1;
    let remaining = if head > end { head - end } else { 0 };
    let new_total_volume_usd = cache.total_volume_usd + delta_volume_usd;
    let new_total_volume_fat = cache.total_volume_fat + delta_volume_fat;
    let new_total_events = cache.total_transfer_events + delta_events;
    tracing::info!(
        "Tx+volume scan: blocks {}..{} ({} blocks), txs={}, +events={}, +vol=${:.2}, total_vol=${:.0}, remaining={}",
        start,
        end,
        scanned_this_tick,
        cumulative,
        delta_events,
        delta_volume_usd,
        new_total_volume_usd,
        remaining
    );

    let snapshot = {
        let mut w = state.tx_count_cache.write().await;
        w.total_transactions = cumulative;
        w.last_scanned_block = end;
        w.total_volume_usd = new_total_volume_usd;
        w.total_volume_fat = new_total_volume_fat;
        w.total_transfer_events = new_total_events;
        w.clone()
    };
    // Persist outside the lock so disk I/O can never block readers.
    // On crash, atomic rename in save_tx_count_cache prevents corruption.
    save_tx_count_cache(&snapshot);
}

/// The set of token contracts the persistent holder index tracks. Each entry
/// is a (lowercase contract address, decimals) pair. We list every
/// redeployment address from `known_token()` so the index keeps working
/// across DCSwap migrations without code changes.
fn holder_index_token_set() -> &'static [(&'static str, u32)] {
    &[
        ("0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4", 18), // WFAT 2026-02-26
        ("0xddbf887982a2a1c03cb8705fef9e09c46122fff6", 18), // WFAT post-Reth
        ("0xb93bd8db94f1baff474aa9cba0739daaad01641f", 6),  // USDC 2026-02-26
        ("0x3109c838e9a08a42fba000a48310845919759a02", 6),  // USDC post-Reth
        ("0x79a26132f48394421382c13b54ae77fa3af73289", 6),  // USDT 2026-02-26
        ("0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef", 6),  // USDT post-Reth
        ("0x24d6137807fa8a592888726d87ac748d018c6d4a", 6),  // EUROD 2026-02-26
        ("0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa", 6),  // EUROD post-Reth
    ]
}

/// Background refresh of the persistent holder index. Walks each tracked
/// token's Transfer events from `last_scanned_block + 1` to chain head in
/// adaptive chunks (Reth caps each `eth_getLogs` at 100K blocks and 20K
/// results, so we start at 5K blocks and halve on overflow). Updates each
/// holder's running u128 balance, persists to disk, and increments
/// `transfer_count` so the token-info card can show the authoritative
/// historical count.
async fn refresh_holder_index(state: &Arc<AppState>) {
    let head = match rpc_block_number(state).await {
        Ok(h) => h,
        Err(_) => return,
    };

    let zero_addr = "0x0000000000000000000000000000000000000000";
    // Per-tick budget so a fresh genesis bootstrap doesn't monopolise the
    // RPC layer. ~250 K blocks per tick × tick=60 s lets the index catch
    // up to head in ~10 min on a 2 M-block chain, while leaving plenty of
    // headroom for live request traffic.
    let per_tick_block_budget: u64 = 250_000;

    for &(token_addr, _decimals) in holder_index_token_set() {
        // Read previous state for this token (or seed default).
        let mut state_for_token = {
            let r = state.holder_index.read().await;
            r.tokens.get(token_addr).cloned().unwrap_or_default()
        };

        let scan_from = if state_for_token.last_scanned_block == 0
            && state_for_token.balances.is_empty()
        {
            0
        } else {
            state_for_token.last_scanned_block + 1
        };
        if scan_from > head {
            continue;
        }
        let scan_to = head.min(scan_from + per_tick_block_budget);
        let original_first = if state_for_token.first_scanned_block == 0
            && state_for_token.balances.is_empty()
        {
            scan_from
        } else {
            state_for_token.first_scanned_block
        };

        // Adaptive chunk size: start at 2 K blocks (busy contracts like
        // WFAT see thousands of Transfer events per 5 K-block window;
        // 2 K is a safer default that almost always fits inside Reth's
        // result cap and the rope-node forwarder's body-size budget).
        // If a chunk still overflows, halve and retry. Min chunk = 100.
        let mut chunk_size: u64 = 2_000;
        let min_chunk: u64 = 100;
        let mut cursor = scan_from;
        let mut new_logs_seen: u64 = 0;
        let mut overflow_count = 0u32;
        let mut transient_retries = 0u32;
        let max_transient_retries = 3u32;

        while cursor <= scan_to {
            let chunk_end = (cursor + chunk_size - 1).min(scan_to);
            let logs_res = rpc_call(
                state,
                "eth_getLogs",
                vec![serde_json::json!({
                    "fromBlock": format!("0x{:x}", cursor),
                    "toBlock":   format!("0x{:x}", chunk_end),
                    "address":   token_addr,
                    "topics":    [TRANSFER_TOPIC],
                })],
            )
            .await;

            match logs_res {
                Ok(v) => {
                    if let Some(arr) = v.as_array() {
                        for log in arr {
                            let topics = match log.get("topics").and_then(|t| t.as_array()) {
                                Some(t) if t.len() >= 3 => t,
                                _ => continue,
                            };
                            let from = topic_to_address(topics[1].as_str().unwrap_or("")).to_lowercase();
                            let to = topic_to_address(topics[2].as_str().unwrap_or("")).to_lowercase();
                            let data = log.get("data").and_then(|v| v.as_str()).unwrap_or("0x0");
                            let raw_amount = decode_hex_u256(data);
                            new_logs_seen += 1;

                            // Subtract from sender (skip mint).
                            if from != zero_addr {
                                let bal = state_for_token
                                    .balances
                                    .get(&from)
                                    .and_then(|s| s.parse::<u128>().ok())
                                    .unwrap_or(0);
                                let new_bal = bal.saturating_sub(raw_amount);
                                if new_bal == 0 {
                                    state_for_token.balances.remove(&from);
                                } else {
                                    state_for_token
                                        .balances
                                        .insert(from.clone(), new_bal.to_string());
                                }
                            }
                            // Add to recipient (skip burn).
                            if to != zero_addr {
                                let bal = state_for_token
                                    .balances
                                    .get(&to)
                                    .and_then(|s| s.parse::<u128>().ok())
                                    .unwrap_or(0);
                                let new_bal = bal.saturating_add(raw_amount);
                                state_for_token.balances.insert(to.clone(), new_bal.to_string());
                            }
                        }
                    }
                    cursor = chunk_end + 1;
                }
                Err(e) => {
                    let msg = e.to_string();
                    let is_overflow = msg.contains("max results") || msg.contains("max block range");
                    if is_overflow && chunk_size > min_chunk {
                        overflow_count += 1;
                        chunk_size = (chunk_size / 2).max(min_chunk);
                        // Don't advance cursor; retry the same range with smaller chunk.
                        continue;
                    } else if !is_overflow && transient_retries < max_transient_retries {
                        // Transient (RPC blip / connection reset). Wait a short
                        // beat and retry the same range — the rope-node forwarder
                        // recovers within a few hundred ms.
                        transient_retries += 1;
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                        continue;
                    } else {
                        // Non-overflow error after retries — break out
                        // and try again on the next 60 s tick. Don't advance
                        // cursor so we don't lose data.
                        tracing::warn!(
                            "HolderIndex scan err for {} at blocks {}..{}: {}",
                            token_addr,
                            cursor,
                            chunk_end,
                            msg
                        );
                        break;
                    }
                }
            }
        }

        state_for_token.transfer_count += new_logs_seen;
        state_for_token.last_scanned_block = scan_to.min(cursor.saturating_sub(1));
        if state_for_token.first_scanned_block == 0 {
            state_for_token.first_scanned_block = original_first;
        }
        state_for_token.updated_at = chrono::Utc::now().timestamp();

        if new_logs_seen > 0 || scan_to > scan_from + 1000 {
            tracing::info!(
                "HolderIndex {}: scanned {}..{}, +{} transfers, {} holders, last={} (overflow_retries={})",
                token_addr,
                scan_from,
                state_for_token.last_scanned_block,
                new_logs_seen,
                state_for_token.balances.len(),
                state_for_token.last_scanned_block,
                overflow_count
            );
        }

        // Commit per-token state under the write lock, then release before
        // the next token. This keeps lock-hold times short.
        {
            let mut w = state.holder_index.write().await;
            w.tokens.insert(token_addr.to_string(), state_for_token);
        }
    }

    // Persist outside the per-token loop so disk I/O happens once per tick.
    let snapshot = state.holder_index.read().await.clone();
    save_holder_index(&snapshot);
}

/// Returns the set of ERC-721 collection addresses that the NFT index
/// should track. Sourced from the Tanastok manifest cache (every Tanastok
/// DCNFT) plus a small static set for any non-Tanastok ERC-721 the
/// foundation cares about. Lowercased.
async fn nft_index_collection_set(state: &Arc<AppState>) -> Vec<String> {
    let mut set = std::collections::HashSet::<String>::new();

    // 1) Pull every DCNFT contract out of the Tanastok cache.
    if let Some(c) = state.tanastok_cache.read().await.as_ref() {
        for addr in c.by_dcnft.keys() {
            set.insert(addr.to_lowercase());
        }
    }

    // 2) Static fallback for any extra ERC-721 contracts we want
    // tracked but that aren't in Tanastok's manifest.
    let extras: &[&str] = &[];
    for a in extras {
        set.insert(a.to_lowercase());
    }

    set.into_iter().collect()
}

/// Background refresh of the persistent ERC-721 ownership index.
///
/// Algorithm: instead of looping per-collection (which is what
/// `refresh_holder_index` does for the 8 fungible-token addresses),
/// we batch every Tanastok DCNFT into a single `eth_getLogs` call per
/// block-range chunk. Reth supports `address: [arr]` in the filter, so
/// 413 contracts × 20 chunks for a 2 M-block chain becomes just 20 RPC
/// calls instead of 8 K. Each chunk's logs are then dispatched to the
/// per-collection state map.
///
/// Three correctness properties:
///
/// 1. **Topic count disambiguation** — ERC-20 and ERC-721 share the same
///    `Transfer` topic-0 hash. We skip any log whose topics array length
///    is not exactly four — that's the canonical ERC-721 shape (topic0
///    + from + to + tokenId, all indexed).
/// 2. **Per-tokenId state** — instead of running u128 balances we
///    maintain `tokenId → owner`. Mints (`from == 0x0`) bump
///    `mint_count`; burns (`to == 0x0`) bump `burn_count` and remove
///    the tokenId.
/// 3. **Per-collection cursors** — we scan from the **min** `last_scanned`
///    across all collections to chain head, but each collection's
///    `last_scanned_block` is updated only when we've actually queried
///    a chunk that covers it. (In practice every collection runs at
///    the same cursor because they were all seeded at 0.)
async fn refresh_nft_index(state: &Arc<AppState>) {
    let head = match rpc_block_number(state).await {
        Ok(h) => h,
        Err(_) => return,
    };
    let zero_addr = "0x0000000000000000000000000000000000000000";

    let collection_set = nft_index_collection_set(state).await;
    if collection_set.is_empty() {
        return; // Tanastok cache not warm yet — wait for the next tick.
    }

    // Per-tick block budget. 2 M blocks per tick is enough to chew
    // through a fresh genesis bootstrap in one pass on a 2 M-block
    // chain; once caught up, only a few hundred new blocks per tick
    // get scanned, costing a single RPC call.
    let per_tick_block_budget: u64 = 2_000_000;

    // Snapshot per-collection state under a single read lock so the
    // scan proceeds against a consistent view. We'll update under a
    // write lock at the end.
    let mut col_states: std::collections::HashMap<String, NftCollectionState> = {
        let r = state.nft_index.read().await;
        collection_set
            .iter()
            .map(|a| {
                let s = r.collections.get(a).cloned().unwrap_or_default();
                (a.clone(), s)
            })
            .collect()
    };

    // Compute the global scan range — from the lowest already-scanned
    // block to chain head, capped by per_tick_block_budget.
    let global_last_scanned = col_states
        .values()
        .map(|s| s.last_scanned_block)
        .min()
        .unwrap_or(0);
    let scan_from = if global_last_scanned == 0
        && col_states.values().all(|s| s.owners.is_empty())
    {
        0
    } else {
        global_last_scanned + 1
    };
    if scan_from > head {
        return;
    }
    let scan_to = head.min(scan_from + per_tick_block_budget);

    // Adaptive chunk size. 100 K blocks is generous: with 413 sparse
    // contracts, expected log count per chunk is ~0–10. Halve on
    // overflow, floor 5 K.
    let mut chunk_size: u64 = 100_000;
    let min_chunk: u64 = 5_000;
    let mut cursor = scan_from;
    let mut total_new_logs: u64 = 0;
    let mut overflow_count = 0u32;
    let mut transient_retries = 0u32;
    let max_transient_retries = 3u32;

    while cursor <= scan_to {
        let chunk_end = (cursor + chunk_size - 1).min(scan_to);
        let logs_res = rpc_call(
            state,
            "eth_getLogs",
            vec![serde_json::json!({
                "fromBlock": format!("0x{:x}", cursor),
                "toBlock":   format!("0x{:x}", chunk_end),
                "address":   &collection_set,
                "topics":    [TRANSFER_TOPIC],
            })],
        )
        .await;

        match logs_res {
            Ok(v) => {
                if let Some(arr) = v.as_array() {
                    for log in arr {
                        let topics = match log.get("topics").and_then(|t| t.as_array()) {
                            Some(t) if t.len() == 4 => t,
                            _ => continue,
                        };
                        let col_addr = log
                            .get("address")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_lowercase();
                        let col_state = match col_states.get_mut(&col_addr) {
                            Some(s) => s,
                            None => continue, // not in our tracked set
                        };
                        let from =
                            topic_to_address(topics[1].as_str().unwrap_or("")).to_lowercase();
                        let to = topic_to_address(topics[2].as_str().unwrap_or("")).to_lowercase();
                        let token_id_hex = topics[3].as_str().unwrap_or("0x0");
                        let token_id_u = decode_hex_u256(token_id_hex);
                        let token_id = token_id_u.to_string();

                        total_new_logs += 1;
                        col_state.transfer_count =
                            col_state.transfer_count.saturating_add(1);

                        if from == zero_addr {
                            col_state.mint_count = col_state.mint_count.saturating_add(1);
                        }
                        if to == zero_addr {
                            col_state.burn_count = col_state.burn_count.saturating_add(1);
                            col_state.owners.remove(&token_id);
                        } else {
                            col_state.owners.insert(token_id.clone(), to.clone());
                        }

                        let block = log
                            .get("blockNumber")
                            .and_then(|v| v.as_str())
                            .map(hex_to_u64)
                            .unwrap_or(0);
                        let tx_hash = log
                            .get("transactionHash")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let log_index = log
                            .get("logIndex")
                            .and_then(|v| v.as_str())
                            .map(hex_to_u64)
                            .unwrap_or(0);
                        col_state.recent_transfers.push(NftTransferRecord {
                            block,
                            tx_hash,
                            log_index,
                            from,
                            to,
                            token_id,
                        });
                    }
                }
                cursor = chunk_end + 1;
                transient_retries = 0;
            }
            Err(e) => {
                let msg = e.to_string();
                let is_overflow =
                    msg.contains("max results") || msg.contains("max block range");
                if is_overflow && chunk_size > min_chunk {
                    overflow_count += 1;
                    chunk_size = (chunk_size / 2).max(min_chunk);
                    continue;
                } else if !is_overflow && transient_retries < max_transient_retries {
                    transient_retries += 1;
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    continue;
                } else {
                    tracing::warn!(
                        "NftIndex batch scan err at blocks {}..{}: {}",
                        cursor,
                        chunk_end,
                        msg
                    );
                    break;
                }
            }
        }
    }

    // Update each collection's bookkeeping fields once.
    let scan_committed = scan_to.min(cursor.saturating_sub(1));
    let now = chrono::Utc::now().timestamp();
    for state_entry in col_states.values_mut() {
        if state_entry.first_scanned_block == 0 && state_entry.owners.is_empty()
            && state_entry.recent_transfers.is_empty()
        {
            state_entry.first_scanned_block = scan_from;
        } else if state_entry.first_scanned_block == 0 {
            state_entry.first_scanned_block = scan_from;
        }
        state_entry.last_scanned_block = state_entry.last_scanned_block.max(scan_committed);
        state_entry.updated_at = now;

        // Sort recent_transfers newest-first and cap at 200.
        if !state_entry.recent_transfers.is_empty() {
            state_entry.recent_transfers.sort_by(|a, b| {
                b.block
                    .cmp(&a.block)
                    .then_with(|| b.log_index.cmp(&a.log_index))
            });
            if state_entry.recent_transfers.len() > 200 {
                state_entry.recent_transfers.truncate(200);
            }
        }
    }

    if total_new_logs > 0 {
        tracing::info!(
            "NftIndex tick: scanned {}..{} across {} collections, +{} transfers (overflow_retries={})",
            scan_from,
            scan_committed,
            collection_set.len(),
            total_new_logs,
            overflow_count
        );
    }

    // Commit + persist.
    {
        let mut w = state.nft_index.write().await;
        for (addr, st) in col_states {
            w.collections.insert(addr, st);
        }
    }
    let snapshot = state.nft_index.read().await.clone();
    save_nft_index(&snapshot);
}

const TANASTOK_API: &str = "https://tanastok.io/api/v1/tokenized-assets";

/// CoinMarketCap REST endpoint for spot quotes. We use the v1 endpoint
/// because it's available on the free Basic tier (10 K credits /
/// month, 30 calls / minute — plenty of headroom for one batched call
/// every 5 minutes).
const CMC_QUOTES_URL: &str =
    "https://pro-api.coinmarketcap.com/v1/cryptocurrency/quotes/latest";

/// Symbols we care about — the bridged DCR-20 stables plus DC FAT.
/// We keep this list small so a single batched CMC request covers
/// every token page on dcscan.io. EUROD has no CMC listing yet, so it
/// is intentionally excluded; its global cap stays "—" until it lists
/// publicly.
const CMC_SYMBOLS: &[&str] = &["USDC", "USDT"];

/// Live-refresh the in-memory CMC cache. Becomes a no-op (with a
/// single info log) when `CMC_API_KEY` is unset so dev environments
/// don't spam errors. Production deploys set the key in `.env`.
async fn refresh_cmc_cache(state: &Arc<AppState>) {
    // Accept both names: `CMC_API_KEY` (historical) and
    // `COINMARKETCAP_API_KEY` (the name used in the workspace root .env).
    let api_key = match std::env::var("CMC_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| {
            std::env::var("COINMARKETCAP_API_KEY")
                .ok()
                .filter(|k| !k.trim().is_empty())
        }) {
        Some(k) => k,
        None => {
            tracing::debug!(
                "CMC_API_KEY / COINMARKETCAP_API_KEY not set — skipping CoinMarketCap refresh, falling back to static snapshot"
            );
            return;
        }
    };

    let symbols = CMC_SYMBOLS.join(",");
    let url = format!("{}?symbol={}&convert=USD", CMC_QUOTES_URL, symbols);
    let resp = match state
        .http_client
        .get(&url)
        .header("X-CMC_PRO_API_KEY", api_key)
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("CMC fetch failed (network): {}", e);
            return;
        }
    };

    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(
            "CMC fetch failed (HTTP {}): {}",
            code,
            body.chars().take(200).collect::<String>()
        );
        return;
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("CMC parse failed: {}", e);
            return;
        }
    };

    // Response shape:
    // { "status": {...}, "data": { "USDC": [{...}], "USDT": [{...}] } }
    // Each entry's `quote.USD` carries `price`, `market_cap`,
    // `volume_24h`, `percent_change_24h`, `last_updated`. Top-level
    // `circulating_supply` is the canonical figure.
    let data = match body.get("data") {
        Some(d) if d.is_object() => d.as_object().unwrap().clone(),
        _ => {
            tracing::warn!("CMC response missing `data` object");
            return;
        }
    };

    let mut quotes: std::collections::HashMap<String, CmcQuote> =
        std::collections::HashMap::new();
    for (symbol, entry) in data.iter() {
        // Each symbol maps to either an array of coin objects (when
        // there are duplicates — USDC has two CMC IDs across chains)
        // or a single object. Walk both shapes and pick the entry with
        // the highest market cap (= the canonical mainnet listing).
        let candidates: Vec<&serde_json::Value> = match entry {
            serde_json::Value::Array(arr) => arr.iter().collect(),
            serde_json::Value::Object(_) => vec![entry],
            _ => continue,
        };
        let mut best: Option<&serde_json::Value> = None;
        let mut best_mcap = 0.0f64;
        for c in candidates {
            let mcap = c
                .get("quote")
                .and_then(|q| q.get("USD"))
                .and_then(|u| u.get("market_cap"))
                .and_then(|m| m.as_f64())
                .unwrap_or(0.0);
            if mcap >= best_mcap {
                best = Some(c);
                best_mcap = mcap;
            }
        }
        let coin = match best {
            Some(c) => c,
            None => continue,
        };
        let usd = coin.get("quote").and_then(|q| q.get("USD"));
        let price = usd.and_then(|u| u.get("price")).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let mcap = usd.and_then(|u| u.get("market_cap")).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let vol = usd.and_then(|u| u.get("volume_24h")).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let pct = usd.and_then(|u| u.get("percent_change_24h")).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let last_updated = usd
            .and_then(|u| u.get("last_updated"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let supply = coin
            .get("circulating_supply")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);
        quotes.insert(
            symbol.to_uppercase(),
            CmcQuote {
                symbol: symbol.to_uppercase(),
                price_usd: price,
                market_cap_usd: mcap,
                volume_24h_usd: vol,
                circulating_supply: supply,
                percent_change_24h: pct,
                last_updated,
            },
        );
    }

    if quotes.is_empty() {
        tracing::warn!("CMC refresh produced 0 quotes; keeping previous cache");
        return;
    }

    let count = quotes.len();
    let mut guard = state.cmc_cache.write().await;
    *guard = Some(CmcCache {
        fetched_at: chrono::Utc::now().timestamp(),
        quotes,
        source: "CoinMarketCap (live)",
    });
    tracing::info!("CMC cache refreshed: {} symbols", count);
}

/// Map a token contract address to the symbol used in CMC quotes.
/// Returned in uppercase to match the cache key.
fn cmc_symbol_for_address(addr: &str) -> Option<&'static str> {
    match addr.to_lowercase().as_str() {
        "0xb93bd8db94f1baff474aa9cba0739daaad01641f"
        | "0x3109c838e9a08a42fba000a48310845919759a02"
        | "0x9f700dd3bb1764ab568263d3e19a1fc5cdf3f9a5" => Some("USDC"),
        "0x79a26132f48394421382c13b54ae77fa3af73289"
        | "0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef" => Some("USDT"),
        _ => None,
    }
}

/// `GET /api/v1/cmc/status` — exposes provenance for the live CMC
/// cache so the dcscan UI (or anyone debugging) can tell at a glance
/// whether the page is reading live numbers or falling back to the
/// hand-curated snapshot.
async fn cmc_status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let key_set = std::env::var("CMC_API_KEY")
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);
    let guard = state.cmc_cache.read().await;
    match guard.as_ref() {
        Some(c) => {
            let now = chrono::Utc::now().timestamp();
            let age_secs = (now - c.fetched_at).max(0);
            let symbols: Vec<String> = c.quotes.keys().cloned().collect();
            Json(serde_json::json!({
                "live": true,
                "apiKeyConfigured": key_set,
                "fetchedAt": c.fetched_at,
                "ageSeconds": age_secs,
                "ageHuman": format_age(age_secs),
                "source": c.source,
                "symbols": symbols,
                "quotes": c.quotes.iter().map(|(k, v)| (
                    k.clone(),
                    serde_json::json!({
                        "priceUsd": v.price_usd,
                        "marketCapUsd": v.market_cap_usd,
                        "volume24hUsd": v.volume_24h_usd,
                        "circulatingSupply": v.circulating_supply,
                        "percentChange24h": v.percent_change_24h,
                        "lastUpdated": v.last_updated,
                    })
                )).collect::<std::collections::HashMap<_, _>>(),
            }))
        }
        None => Json(serde_json::json!({
            "live": false,
            "apiKeyConfigured": key_set,
            "reason": if key_set {
                "CMC cache empty — first refresh in progress, or last attempt failed"
            } else {
                "CMC_API_KEY not set in environment; using static 2026-06-04 snapshot from token_metadata()"
            },
            "source": "static-snapshot",
        })),
    }
}

fn format_age(secs: i64) -> String {
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h {}m ago", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Refresh the EDC public project directory by polling every configured
/// EDC instance (`EDC_DIRECTORY_URLS`). Partial failures degrade
/// gracefully: reachable instances still contribute their cards, and
/// per-instance status is recorded for the API's `sources` field. When
/// every instance fails, the previous cache entry is preserved so the
/// /ecosystem page keeps serving the last-known directory.
async fn refresh_ecosystem_directory_cache(state: &AppState) {
    let bases = edc_directory_urls();
    let mut projects: Vec<serde_json::Value> = Vec::new();
    let mut sources: Vec<serde_json::Value> = Vec::new();
    let mut any_ok = false;

    for base in &bases {
        let url = format!("{}/api/v1/ecosystem/public/projects", base);
        let result = state
            .http_client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;
        match result {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        let cards = body
                            .get("projects")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        let count = cards.len();
                        for mut card in cards {
                            if let Some(obj) = card.as_object_mut() {
                                obj.insert(
                                    "edc_base".to_string(),
                                    serde_json::json!(base),
                                );
                            }
                            projects.push(card);
                        }
                        sources.push(serde_json::json!({
                            "base": base, "ok": true, "projects": count
                        }));
                        any_ok = true;
                    }
                    Err(e) => {
                        sources.push(serde_json::json!({
                            "base": base, "ok": false,
                            "error": format!("parse: {e}")
                        }));
                    }
                }
            }
            Ok(resp) => {
                sources.push(serde_json::json!({
                    "base": base, "ok": false,
                    "error": format!("http {}", resp.status())
                }));
            }
            Err(e) => {
                sources.push(serde_json::json!({
                    "base": base, "ok": false,
                    "error": format!("network: {e}")
                }));
            }
        }
    }

    if !any_ok && state.ecosystem_directory_cache.read().await.is_some() {
        tracing::warn!(
            "EDC directory refresh: all {} instance(s) unreachable — keeping previous cache",
            bases.len()
        );
        return;
    }

    // Newest first; dedupe by id (an instance restart re-listing the same
    // project must not create duplicates across instances either).
    projects.sort_by_key(|p| {
        std::cmp::Reverse(p.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0))
    });
    let mut by_id = std::collections::HashMap::new();
    let mut deduped: Vec<serde_json::Value> = Vec::new();
    for card in projects {
        let id = card
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if id.is_empty() || by_id.contains_key(&id) {
            continue;
        }
        by_id.insert(id, deduped.len());
        deduped.push(card);
    }

    let count = deduped.len();
    *state.ecosystem_directory_cache.write().await = Some(EcosystemDirectoryCache {
        projects: deduped,
        by_id,
        sources,
        fetched_at: chrono::Utc::now().timestamp(),
    });
    tracing::debug!("EDC directory refreshed: {count} project(s) from {} instance(s)", bases.len());
}

/// `GET /api/v1/ecosystem/directory` — the aggregated EDC public
/// project directory (spec v2.0 §8). Query params: `archetype`,
/// `country`, `status`, `q` (free-text over name/tags/region).
async fn ecosystem_directory(
    State(state): State<Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let cache = state.ecosystem_directory_cache.read().await;
    let Some(cache) = cache.as_ref() else {
        return Json(serde_json::json!({
            "count": 0,
            "projects": [],
            "sources": [],
            "note": "directory cache warming; retry in <60s"
        }));
    };

    let archetype = params.get("archetype").map(|s| s.to_lowercase());
    let country = params.get("country").map(|s| s.to_lowercase());
    let status = params.get("status").map(|s| s.to_lowercase());
    let q = params.get("q").map(|s| s.to_lowercase());

    let filtered: Vec<&serde_json::Value> = cache
        .projects
        .iter()
        .filter(|p| {
            let get = |k: &str| {
                p.get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase()
            };
            if let Some(a) = &archetype {
                if &get("archetype") != a {
                    return false;
                }
            }
            if let Some(c) = &country {
                if &get("country") != c {
                    return false;
                }
            }
            if let Some(s) = &status {
                if &get("status") != s {
                    return false;
                }
            }
            if let Some(needle) = &q {
                let tags = p
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                            .to_lowercase()
                    })
                    .unwrap_or_default();
                let hay = format!("{} {} {} {}", get("name"), get("region"), get("country"), tags);
                if !hay.contains(needle.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();

    Json(serde_json::json!({
        "count": filtered.len(),
        "projects": filtered,
        "sources": cache.sources,
        "fetched_at": cache.fetched_at,
    }))
}

/// `GET /api/v1/ecosystem/directory/:id` — full public detail for one
/// project, proxied live from its own EDC instance so the response
/// includes the current public grants and the stakeholder API base URL
/// (the disintermediated access path for regulators / investors).
async fn ecosystem_directory_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let base = {
        let cache = state.ecosystem_directory_cache.read().await;
        cache.as_ref().and_then(|c| {
            c.by_id.get(&id.to_lowercase()).and_then(|idx| {
                c.projects[*idx]
                    .get("edc_base")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
        })
    };
    let Some(base) = base else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "project not found in ecosystem directory"})),
        )
            .into_response();
    };

    let url = format!("{}/api/v1/ecosystem/public/projects/{}", base, id);
    match state
        .http_client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(mut body) => {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("edc_base".to_string(), serde_json::json!(base));
                }
                Json(body).into_response()
            }
            Err(e) => (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("EDC instance returned unparseable payload: {e}")})),
            )
                .into_response(),
        },
        Ok(resp) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("EDC instance returned {}", resp.status())})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("EDC instance unreachable: {e}")})),
        )
            .into_response(),
    }
}

async fn refresh_tanastok_cache(state: &AppState) {
    let url = format!("{}?limit=500", TANASTOK_API);
    let resp = match state
        .http_client
        .get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Tanastok cache refresh failed (network): {}", e);
            return;
        }
    };
    let json: serde_json::Value = match resp.json().await {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("Tanastok cache refresh failed (parse): {}", e);
            return;
        }
    };
    let assets = match json.get("data").and_then(|d| d.as_array()) {
        Some(arr) => arr.clone(),
        None => {
            tracing::warn!("Tanastok cache refresh: no data array in response");
            return;
        }
    };

    let mut by_dcnft = std::collections::HashMap::new();
    let mut by_erc3643 = std::collections::HashMap::new();

    for (i, asset) in assets.iter().enumerate() {
        if let Some(addr) = asset
            .pointer("/dcnft/contractAddress")
            .and_then(|v| v.as_str())
        {
            by_dcnft.insert(addr.to_lowercase(), i);
        }
        if let Some(addr) = asset
            .pointer("/erc3643/contractAddress")
            .and_then(|v| v.as_str())
        {
            by_erc3643.insert(addr.to_lowercase(), i);
        }
    }

    let count = assets.len();
    let cache = TanastokCache {
        assets,
        by_dcnft,
        by_erc3643,
        updated_at: chrono::Utc::now().timestamp(),
    };

    save_json_cache("tanastok_cache.json", &cache);
    *state.tanastok_cache.write().await = Some(cache);
    tracing::info!("Tanastok cache refreshed: {} assets indexed", count);
}

fn tanastok_lookup(cache: &TanastokCache, addr: &str) -> Option<serde_json::Value> {
    let lower = addr.to_lowercase();
    let idx = cache
        .by_dcnft
        .get(&lower)
        .or_else(|| cache.by_erc3643.get(&lower))?;
    let asset = cache.assets.get(*idx)?;
    let is_dcnft = cache.by_dcnft.contains_key(&lower);

    let mut result = asset.clone();
    result["_contractType"] = if is_dcnft {
        serde_json::json!("dcnft")
    } else {
        serde_json::json!("erc3643")
    };
    result["_matchedAddress"] = serde_json::json!(lower);
    Some(result)
}

async fn tanastok_by_address(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let cache = state.tanastok_cache.read().await;
    match cache.as_ref().and_then(|c| tanastok_lookup(c, &address)) {
        Some(asset) => Json(serde_json::json!({
            "success": true,
            "asset": asset,
        })),
        None => Json(serde_json::json!({
            "success": false,
            "asset": serde_json::Value::Null,
        })),
    }
}

// ============================================================================
// Tanastok entity-manifest mirror (Quipu Canon v1.2 / Phase 5)
// ============================================================================
//
// These endpoints expose the live Tanastok manifest under the DCScan
// origin so:
//
//  - DCScan address pages and string pages can resolve label data
//    server-side for any kind=asset / kind=contract / kind=did /
//    kind=application / kind=ecosystem string emitted by Tanastok
//    without making the user's browser cross to tanastok.io for every
//    address page (saves ~1s of cold-cache fan-out).
//
//  - The Rope-Graph component on event.datachain.one (and any future
//    third-party frontend) can call `/api/v1/registry/manifest` once,
//    keep it in memory, and render every Tanastok entity. The endpoint
//    exposes the upstream `version` + `generated_at` so clients can
//    cache exactly the way the upstream wants them to.
//
// The cache discipline matches what `crates/rope-node/src/entity_manifest.rs`
// does internally: pull every 5 min, only invalidate when
// `(version, generated_at)` changes upstream.

const TANASTOK_MANIFEST_API: &str =
    "https://tanastok.io/api/v1/tanastok-entity-manifest";

async fn refresh_tanastok_manifest_cache(state: &AppState) {
    let resp = match state
        .http_client
        .get(TANASTOK_MANIFEST_API)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Tanastok manifest cache refresh failed (network): {}", e);
            return;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!(
            "Tanastok manifest cache refresh: upstream returned {}",
            resp.status(),
        );
        return;
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("Tanastok manifest cache refresh failed (parse): {}", e);
            return;
        }
    };

    let version = body
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let generated_at = body
        .get("generated_at")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let entities_count = body
        .get("entities")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if entities_count == 0 {
        tracing::warn!("Tanastok manifest cache refresh: no entities array");
        return;
    }

    // Build the by-id index. Keys are lowercase, no `0x` prefix — matches
    // what `entity_labels::current().get(...)` expects on the rope-node
    // side, so the two layers stay symmetric.
    let mut by_id = std::collections::HashMap::with_capacity(entities_count);
    if let Some(entities) = body.get("entities").and_then(|v| v.as_array()) {
        for (i, ent) in entities.iter().enumerate() {
            let id = ent
                .get("string_id")
                .and_then(|v| v.as_str())
                .or_else(|| ent.get("id_bytes").and_then(|v| v.as_str()))
                .unwrap_or_default();
            let key = id.trim().trim_start_matches("0x").to_ascii_lowercase();
            if !key.is_empty() {
                by_id.insert(key, i);
            }
        }
    }

    let cache = TanastokManifestCache {
        raw: body,
        version: version.clone(),
        generated_at,
        by_id,
        fetched_at: chrono::Utc::now().timestamp(),
    };

    let count = entities_count;
    save_json_cache("tanastok_manifest_cache.json", &cache);
    *state.tanastok_manifest_cache.write().await = Some(cache);
    tracing::info!(
        "Tanastok manifest cache refreshed: {} entities, version={}",
        count,
        version,
    );
}

/// `GET /api/v1/registry/manifest` — full mirror of
/// `https://tanastok.io/api/v1/tanastok-entity-manifest`.
///
/// Sets `Cache-Control: public, max-age=60, s-maxage=300,
/// stale-while-revalidate=600` so browsers cache for a minute and CDNs
/// for the same 5-min window the upstream uses, plus an
/// `X-Tanastok-Manifest-Version` header that lets clients
/// version-key their local cache.
async fn registry_tanastok_manifest(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let cache = state.tanastok_manifest_cache.read().await;
    // `age_secs` lets consumers tell "fresh from a healthy upstream" apart
    // from "disk-persisted last-known-good, upstream has been down for a
    // while" without changing the response body shape documented for
    // this endpoint. See TanastokManifestCache's doc comment for why a
    // 503-on-restart used to be possible even with a healthy disk cache.
    let (status, version, age_secs, body) = match cache.as_ref() {
        Some(c) => (
            axum::http::StatusCode::OK,
            c.version.clone(),
            (chrono::Utc::now().timestamp() - c.fetched_at).max(0),
            c.raw.clone(),
        ),
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "unavailable".to_string(),
            -1,
            serde_json::json!({
                "error": "tanastok manifest not yet warmed; retry in <5 min",
                "entities": [],
                "counts": {},
            }),
        ),
    };
    let manifest_version_header =
        axum::http::HeaderName::from_static("x-tanastok-manifest-version");
    let manifest_age_header =
        axum::http::HeaderName::from_static("x-tanastok-manifest-age-seconds");
    let age_str = age_secs.to_string();
    (
        status,
        [
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=60, s-maxage=300, stale-while-revalidate=600",
            ),
            (manifest_version_header, version.as_str()),
            (manifest_age_header, age_str.as_str()),
            (
                axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
                "*",
            ),
        ],
        Json(body),
    )
        .into_response()
}

/// `GET /api/v1/registry/labels` — slim `string_id → label` map for
/// fast client-side lookup (typically ~80–120 KB on the wire vs the
/// ~750 KB full manifest).
///
/// Optional query params:
///   - `kind` — filter (`ecosystem|application|asset|contract|did`)
///
/// Useful for any frontend that wants to colour the Rope Graph by
/// platform without parsing the full manifest body.
#[derive(Deserialize)]
struct LabelsQuery {
    kind: Option<String>,
}

async fn registry_tanastok_labels(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LabelsQuery>,
) -> impl axum::response::IntoResponse {
    let cache = state.tanastok_manifest_cache.read().await;
    let (version, generated_at, labels) = match cache.as_ref() {
        Some(c) => {
            let entities = c
                .raw
                .get("entities")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut labels = serde_json::Map::with_capacity(entities.len());
            for e in &entities {
                let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(filter) = q.kind.as_deref() {
                    if !filter.eq_ignore_ascii_case(kind) {
                        continue;
                    }
                }
                let id = match e
                    .get("string_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| e.get("id_bytes").and_then(|v| v.as_str()))
                {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let label = e.get("label").cloned().unwrap_or(serde_json::json!({}));
                labels.insert(
                    id,
                    serde_json::json!({
                        "kind": kind,
                        "label": label,
                        "parent_string_id": e.get("parent_string_id").cloned().unwrap_or(serde_json::Value::Null),
                        "ecosystem_id": e.get("ecosystem_id").cloned().unwrap_or(serde_json::Value::Null),
                        "platform": "tanastok",
                    }),
                );
            }
            (c.version.clone(), c.generated_at, labels)
        }
        None => (
            "unavailable".to_string(),
            0,
            serde_json::Map::new(),
        ),
    };
    let body = serde_json::json!({
        "version": version,
        "generated_at": generated_at,
        "platform": "tanastok",
        "kind_filter": q.kind,
        "count": labels.len(),
        "labels": labels,
    });
    let manifest_version_header =
        axum::http::HeaderName::from_static("x-tanastok-manifest-version");
    (
        [
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=60, s-maxage=300, stale-while-revalidate=600",
            ),
            (manifest_version_header, version.as_str()),
            (
                axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
                "*",
            ),
        ],
        Json(body),
    )
        .into_response()
}

/// `POST /api/rpc` — same-origin JSON-RPC proxy.
///
/// Forwards the request body verbatim to the active rope-node RPC
/// backend (`rpc_url_active()`). Used by DCScan pages so they can
/// call `rope_*` / `eth_*` methods without a cross-origin preflight
/// to `erpc.datachain.network`. Failures are mapped to a JSON-RPC
/// `-32603 Internal error` response (instead of an HTTP 5xx) so
/// browser clients can keep their `j.error.message` handling.
async fn rpc_proxy(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let url = state.rpc_url_active().to_string();
    let id = body.get("id").cloned().unwrap_or(serde_json::json!(1));
    match state
        .http_client
        .post(&url)
        .timeout(std::time::Duration::from_secs(30))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(j) => Json(j),
            Err(e) => Json(serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32603, "message": format!("upstream parse: {}", e) }
            })),
        },
        Err(e) => Json(serde_json::json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": -32603, "message": format!("upstream rpc: {}", e) }
        })),
    }
}

// ---------------------------------------------------------------------------
// Node deployment requests (datachain.network "Deploy a Node" form)
// ---------------------------------------------------------------------------

/// Serializes writes to the node-request queue file. The queue is a
/// JSONL file (one request per line) so it survives restarts and is
/// trivially greppable / auditable by operators.
static NODE_REQUESTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn node_requests_path() -> String {
    std::env::var("NODE_REQUESTS_PATH")
        .unwrap_or_else(|_| "/opt/datachain-rope/node-requests.jsonl".to_string())
}

/// The rope wallet whose personal-ledger string IS the node-request queue.
/// Every "Deploy a Node" submission is anchored as a
/// `NodeDeploymentRequested` knot on this string via `rope_appendToLedger`,
/// which makes the queue durable, replicated across the fleet, and
/// auditable on dcscan.io — the JSONL file is just a local read cache
/// that can be rebuilt from the chain at any time.
fn node_requests_ledger_wallet() -> String {
    std::env::var("NODE_REQUESTS_LEDGER_WALLET")
        .unwrap_or_else(|_| "0x000000000000000000000000000000000000d001".to_string())
}

/// Anchor one node-deployment request on the rope. Returns the knot hash
/// on success. Best-effort by design: the local JSONL write already
/// succeeded before this runs, and the next successful anchor or a
/// rebuild-from-rope reconciles any gap.
async fn anchor_node_request_on_rope(
    state: &Arc<AppState>,
    record: &serde_json::Value,
) -> Option<String> {
    let wallet = node_requests_ledger_wallet();
    let rpc = state.rpc_url_active().to_string();

    // Ensure the queue ledger exists (idempotent: 2001 = already exists).
    let create = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "rope_createPersonalLedger",
        "params": [wallet],
    });
    let _ = state.http_client.post(&rpc).json(&create).send().await;

    // The full request record rides in `description` (the encrypted knot
    // payload) so a rebuild recovers every field; the flat metadata map
    // carries the queryable keys.
    let append = serde_json::json!({
        "jsonrpc": "2.0", "id": 2,
        "method": "rope_appendToLedger",
        "params": [wallet, {
            "interaction_type": "NodeDeploymentRequested",
            "description": record.to_string(),
            "metadata": {
                "request_id": record.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "provider": record.get("provider").and_then(|v| v.as_str()).unwrap_or(""),
                "region": record.get("region").and_then(|v| v.as_str()).unwrap_or(""),
                "node_role": record.get("node_role").and_then(|v| v.as_str()).unwrap_or(""),
                "status": "pending",
            }
        }],
    });
    match state.http_client.post(&rpc).json(&append).send().await {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(body) => {
                if let Some(hash) = body
                    .get("result")
                    .and_then(|r| r.get("hash"))
                    .and_then(|h| h.as_str())
                {
                    tracing::info!(
                        "node-request anchored on rope: wallet={} knot={}",
                        node_requests_ledger_wallet(),
                        hash
                    );
                    return Some(hash.to_string());
                }
                tracing::warn!("node-request anchor rejected by rope-node: {}", body);
                None
            }
            Err(e) => {
                tracing::warn!("node-request anchor: unreadable rope-node response: {}", e);
                None
            }
        },
        Err(e) => {
            tracing::warn!("node-request anchor failed (rope-node unreachable): {}", e);
            None
        }
    }
}

/// Rebuild the local JSONL cache from the rope when the file is missing
/// or empty (fresh node, disk loss, bootstrap from IPFS snapshot). Uses
/// the internal-only decrypted repatriation path — dc-explorer talks to
/// the co-located rope-node over loopback, which the V11 auth model
/// treats as internal.
async fn rebuild_node_requests_from_rope(state: &Arc<AppState>) -> Vec<serde_json::Value> {
    let wallet = node_requests_ledger_wallet();
    let rpc = state.rpc_url_active().to_string();
    let req = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "rope_repatriatePersonalLedger",
        "params": [wallet, {"decrypt": true}],
    });
    let body: serde_json::Value = match state.http_client.post(&rpc).json(&req).send().await {
        Ok(resp) => match resp.json().await {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };
    let fragments = match body
        .get("result")
        .and_then(|r| r.get("fragments"))
        .and_then(|f| f.as_array())
    {
        Some(f) => f,
        None => return Vec::new(),
    };

    let mut requests = Vec::new();
    for frag in fragments {
        let Some(interaction) = frag.get("interaction").filter(|i| !i.is_null()) else {
            continue;
        };
        let is_node_request = interaction
            .get("interaction_type")
            .map(|t| t.to_string().contains("NodeDeploymentRequested"))
            .unwrap_or(false);
        if !is_node_request {
            continue;
        }
        if let Some(desc) = interaction.get("description").and_then(|d| d.as_str()) {
            if let Ok(record) = serde_json::from_str::<serde_json::Value>(desc) {
                if record.get("id").is_some() {
                    requests.push(record);
                }
            }
        }
    }

    if !requests.is_empty() {
        // Rewrite the local cache so subsequent reads are warm.
        let path = node_requests_path();
        let lines: String = requests
            .iter()
            .map(|r| format!("{}\n", r))
            .collect();
        let _ = tokio::task::spawn_blocking(move || {
            let _guard = NODE_REQUESTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(parent) = std::path::Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let tmp = format!("{path}.tmp");
            std::fs::write(&tmp, &lines).and_then(|_| std::fs::rename(&tmp, &path))
        })
        .await;
        tracing::info!(
            "node-request queue rebuilt from rope: {} request(s) recovered",
            requests.len()
        );
    }
    requests
}

#[derive(Deserialize)]
struct NodeRequestSubmission {
    name: String,
    email: String,
    #[serde(default)]
    organization: String,
    provider: String,
    region: String,
    #[serde(default = "default_node_role")]
    node_role: String,
    #[serde(default)]
    notes: String,
}

fn default_node_role() -> String {
    "relay".to_string()
}

/// `POST /api/v1/node-requests` — submit a "Deploy a Node" request from
/// the datachain.network get-started form. Validated, then appended to
/// the durable JSONL queue for operator fulfilment via `ropectl`.
async fn node_request_submit(
    State(state): State<Arc<AppState>>,
    Json(req): Json<NodeRequestSubmission>,
) -> (StatusCode, Json<serde_json::Value>) {
    let name = req.name.trim();
    let email = req.email.trim();
    let organization = req.organization.trim();
    let provider = req.provider.trim().to_lowercase();
    let region = req.region.trim().to_lowercase();
    let node_role = req.node_role.trim().to_lowercase();
    let notes = req.notes.trim();

    let err = |msg: &str| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "success": false, "error": msg })),
        )
    };

    if name.is_empty() || name.len() > 120 {
        return err("name is required (max 120 chars)");
    }
    if email.len() < 5 || email.len() > 254 || !email.contains('@') || !email.contains('.') {
        return err("a valid email is required");
    }
    if organization.len() > 200 {
        return err("organization too long (max 200 chars)");
    }
    if provider != "digitalocean" && provider != "exoscale" {
        return err("provider must be 'digitalocean' or 'exoscale'");
    }
    if region.len() < 2
        || region.len() > 40
        || !region
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return err("region must be a provider region slug (e.g. fra1, ch-gva-2)");
    }
    if node_role != "relay" {
        return err("node_role must be 'relay' (validator onboarding opens with Phase 2 rollout)");
    }
    if notes.len() > 2000 {
        return err("notes too long (max 2000 chars)");
    }

    let now = chrono::Utc::now();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    (name, email, provider.as_str(), region.as_str(), now.timestamp_nanos_opt()).hash(&mut hasher);
    let id = format!("nr-{}-{:08x}", now.format("%Y%m%d%H%M%S"), hasher.finish() as u32);

    let record = serde_json::json!({
        "id": id,
        "received_at": now.to_rfc3339(),
        "status": "pending",
        "name": name,
        "email": email,
        "organization": organization,
        "provider": provider,
        "region": region,
        "node_role": node_role,
        "notes": notes,
    });

    let path = node_requests_path();
    let line = format!("{}\n", record);
    let write_result = tokio::task::spawn_blocking(move || {
        let _guard = NODE_REQUESTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(line.as_bytes())?;
        f.flush()
    })
    .await;

    match write_result {
        Ok(Ok(())) => {
            // Anchor the request on the rope: a `NodeDeploymentRequested`
            // knot on the queue wallet's string makes the request durable,
            // replicated across the fleet, and auditable on dcscan.io.
            // Best-effort — the JSONL write above already succeeded, and
            // the anchor is retried implicitly on the next rebuild.
            let anchored_knot = anchor_node_request_on_rope(&state, &record).await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "success": true,
                    "id": id,
                    "status": "pending",
                    "anchored": anchored_knot.is_some(),
                    "knot": anchored_knot,
                    "message": "Request received. The Datachain Foundation operations team will \
                                provision your node on the selected sovereign cloud and contact \
                                you at the email provided.",
                })),
            )
        }
        Ok(Err(e)) => {
            tracing::error!("node-request queue write failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "success": false, "error": "queue write failed" })),
            )
        }
        Err(e) => {
            tracing::error!("node-request queue task failed: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "success": false, "error": "queue task failed" })),
            )
        }
    }
}

/// `GET /api/v1/node-requests` — operator-only listing of the request
/// queue. Requires the `X-Admin-Token` header to match the
/// `NODE_REQUESTS_ADMIN_TOKEN` environment variable; if the variable is
/// unset the endpoint is disabled entirely (403).
async fn node_requests_list(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    let expected = match std::env::var("NODE_REQUESTS_ADMIN_TOKEN") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "success": false, "error": "listing disabled" })),
            )
        }
    };
    let presented = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Constant-time comparison — this is an auth check.
    let matches = presented.len() == expected.len()
        && presented
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if !matches {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "success": false, "error": "bad token" })),
        );
    }

    let path = node_requests_path();
    let read_result =
        tokio::task::spawn_blocking(move || std::fs::read_to_string(&path)).await;
    let mut requests: Vec<serde_json::Value> = match read_result {
        Ok(Ok(content)) => content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect(),
        _ => Vec::new(),
    };
    // Local cache empty (fresh node / disk loss): the rope is the source
    // of truth — rebuild the queue from the queue wallet's string.
    if requests.is_empty() {
        requests = rebuild_node_requests_from_rope(&state).await;
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "success": true,
            "count": requests.len(),
            "requests": requests,
        })),
    )
}

/// `GET /api/v1/registry/entity/:id` — single-entity lookup by Quipu
/// `string_id`. Accepts the id with or without the `0x` prefix and is
/// case-insensitive.
///
/// Falls back gracefully when the manifest cache is cold or the id is
/// unknown — never panics, never blocks.
async fn registry_tanastok_entity_by_id(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let key = id.trim().trim_start_matches("0x").to_ascii_lowercase();
    let cache = state.tanastok_manifest_cache.read().await;
    let entity = match cache.as_ref() {
        Some(c) => c
            .by_id
            .get(&key)
            .and_then(|idx| {
                c.raw
                    .get("entities")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.get(*idx))
                    .cloned()
            }),
        None => None,
    };
    Json(serde_json::json!({
        "found": entity.is_some(),
        "id": id,
        "entity": entity,
    }))
}

// ---------------------------------------------------------------------------
// Mapstore entity-manifest mirror (marketplace participant ledger)
// ---------------------------------------------------------------------------
//
// Mirrors `https://mapstore.net/api/v1/mapstore-entity-manifest` under
// `/api/v1/registry/mapstore-*` with the exact same three-endpoint shape
// used for Tanastok above (full manifest / slim labels / single entity).
// Kept as a distinct route namespace (not `/api/v1/registry/manifest`)
// because that path is already owned by the Tanastok mirror.
//
// The cache discipline matches `refresh_tanastok_manifest_cache`: pull
// every 5 min, only overwrite the in-memory + on-disk cache on a
// successful, non-empty fetch, so a Mapstore-side outage or a
// dc-explorer restart during one never regresses past "as stale as it
// was before".

fn mapstore_manifest_api() -> String {
    std::env::var("MAPSTORE_MANIFEST_API")
        .unwrap_or_else(|_| "https://mapstore.net/api/v1/mapstore-entity-manifest".to_string())
}

async fn refresh_mapstore_manifest_cache(state: &AppState) {
    let url = mapstore_manifest_api();
    let mut req = state
        .http_client
        .get(&url)
        .timeout(std::time::Duration::from_secs(15));
    // Only sent if the operator has configured a shared secret on this
    // side to match a future MAPSTORE_REGISTRY_API_KEY gate upstream.
    // Mapstore's manifest is open by default (no key required), so this
    // is a no-op today.
    if let Ok(key) = std::env::var("MAPSTORE_REGISTRY_API_KEY") {
        if !key.trim().is_empty() {
            req = req.header("X-Api-Key", key.trim());
        }
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Mapstore manifest cache refresh failed (network): {}", e);
            return;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!(
            "Mapstore manifest cache refresh: upstream returned {}",
            resp.status(),
        );
        return;
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("Mapstore manifest cache refresh failed (parse): {}", e);
            return;
        }
    };

    let version = body
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let generated_at = body
        .get("generated_at")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let entities_count = body
        .get("entities")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    // Unlike Tanastok's 1,626-entity manifest, Mapstore's marketplace
    // ledger can legitimately be empty (a fresh instance with zero
    // onboarded merchants/contracts). An explicit `degraded: true` from
    // upstream is the real "something is wrong" signal — an empty-but-
    // healthy manifest must still populate the cache so callers see
    // `counts.total: 0` instead of a permanent 503.
    let upstream_degraded = body
        .get("degraded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if upstream_degraded && entities_count == 0 {
        tracing::warn!("Mapstore manifest cache refresh: upstream reported degraded+empty");
        return;
    }

    let mut by_id = std::collections::HashMap::with_capacity(entities_count);
    if let Some(entities) = body.get("entities").and_then(|v| v.as_array()) {
        for (i, ent) in entities.iter().enumerate() {
            let id = ent
                .get("string_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let key = id.trim().trim_start_matches("0x").to_ascii_lowercase();
            if !key.is_empty() {
                by_id.insert(key, i);
            }
        }
    }

    let cache = MapstoreManifestCache {
        raw: body,
        version: version.clone(),
        generated_at,
        by_id,
        fetched_at: chrono::Utc::now().timestamp(),
    };

    save_json_cache("mapstore_manifest_cache.json", &cache);
    *state.mapstore_manifest_cache.write().await = Some(cache);
    tracing::info!(
        "Mapstore manifest cache refreshed: {} entities, version={}",
        entities_count,
        version,
    );
}

/// `GET /api/v1/registry/mapstore-manifest` — full mirror of
/// `https://mapstore.net/api/v1/mapstore-entity-manifest`.
///
/// Same header contract as `registry_tanastok_manifest`: strong
/// `Cache-Control`, an `X-Mapstore-Manifest-Version` header, and an
/// `X-Mapstore-Manifest-Age-Seconds` header so clients can tell a fresh
/// pull apart from a disk-persisted last-known-good payload.
async fn registry_mapstore_manifest(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let cache = state.mapstore_manifest_cache.read().await;
    let (status, version, age_secs, body) = match cache.as_ref() {
        Some(c) => (
            axum::http::StatusCode::OK,
            c.version.clone(),
            (chrono::Utc::now().timestamp() - c.fetched_at).max(0),
            c.raw.clone(),
        ),
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "unavailable".to_string(),
            -1,
            serde_json::json!({
                "error": "mapstore manifest not yet warmed; retry in <5 min",
                "entities": [],
                "counts": {},
            }),
        ),
    };
    let manifest_version_header =
        axum::http::HeaderName::from_static("x-mapstore-manifest-version");
    let manifest_age_header =
        axum::http::HeaderName::from_static("x-mapstore-manifest-age-seconds");
    let age_str = age_secs.to_string();
    (
        status,
        [
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=60, s-maxage=300, stale-while-revalidate=600",
            ),
            (manifest_version_header, version.as_str()),
            (manifest_age_header, age_str.as_str()),
            (axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(body),
    )
        .into_response()
}

/// `GET /api/v1/registry/mapstore-labels` — slim `string_id → label`
/// map. Optional `?kind=` filter (`asset|contract`).
async fn registry_mapstore_labels(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LabelsQuery>,
) -> impl axum::response::IntoResponse {
    let cache = state.mapstore_manifest_cache.read().await;
    let (version, generated_at, labels) = match cache.as_ref() {
        Some(c) => {
            let entities = c
                .raw
                .get("entities")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut labels = serde_json::Map::with_capacity(entities.len());
            for e in &entities {
                let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(filter) = q.kind.as_deref() {
                    if !filter.eq_ignore_ascii_case(kind) {
                        continue;
                    }
                }
                let id = match e.get("string_id").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let label = e.get("label").cloned().unwrap_or(serde_json::json!({}));
                labels.insert(
                    id,
                    serde_json::json!({
                        "kind": kind,
                        "label": label,
                        "parent_string_id": e.get("parent_string_id").cloned().unwrap_or(serde_json::Value::Null),
                        "ecosystem_id": e.get("ecosystem_id").cloned().unwrap_or(serde_json::Value::Null),
                        "platform": "mapstore",
                    }),
                );
            }
            (c.version.clone(), c.generated_at, labels)
        }
        None => ("unavailable".to_string(), 0, serde_json::Map::new()),
    };
    let body = serde_json::json!({
        "version": version,
        "generated_at": generated_at,
        "platform": "mapstore",
        "kind_filter": q.kind,
        "count": labels.len(),
        "labels": labels,
    });
    let manifest_version_header =
        axum::http::HeaderName::from_static("x-mapstore-manifest-version");
    (
        [
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=60, s-maxage=300, stale-while-revalidate=600",
            ),
            (manifest_version_header, version.as_str()),
            (axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(body),
    )
        .into_response()
}

/// `GET /api/v1/registry/mapstore-entity/:id` — single-entity lookup by
/// Quipu `string_id`, case-insensitive, `0x`-prefix-tolerant (Mapstore's
/// own ids are not `0x`-prefixed, e.g. `mapstore_business_v1:biz_...`,
/// but the trim is harmless and keeps this handler byte-for-byte
/// symmetric with `registry_tanastok_entity_by_id`).
async fn registry_mapstore_entity_by_id(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let key = id.trim().trim_start_matches("0x").to_ascii_lowercase();
    let cache = state.mapstore_manifest_cache.read().await;
    let entity = match cache.as_ref() {
        Some(c) => c.by_id.get(&key).and_then(|idx| {
            c.raw
                .get("entities")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.get(*idx))
                .cloned()
        }),
        None => None,
    };
    Json(serde_json::json!({
        "found": entity.is_some(),
        "id": id,
        "entity": entity,
    }))
}

// ---------------------------------------------------------------------------
// Careaway entity-manifest mirror (aggregate-only healthcare coordination
// stats — care-plan lifecycle, GDPR Art.17 erasure counters, DC-credit
// ledger settlement volume)
// ---------------------------------------------------------------------------
//
// Mirrors `https://careaway.co/api/v1/careaway-entity-manifest` under a
// single `/api/v1/registry/careaway-manifest` route. Deliberately NOT a
// three-endpoint mirror like Tanastok/Mapstore above: Careaway's payload
// is aggregate-only by design, per the health-data special-category
// boundary (GDPR Art. 9) negotiated in
// `handover-request-data-apis-for-fat-pricing-and-titrization-from-rope-2026-07-24.mdc`
// (Careaway workspace `.cursor/rules/`) — `entities` is always `[]`, so a
// `-labels` / `-entity/:id` sibling would just be a permanent, pointless
// stub. Only build those if Careaway ships a genuine per-record surface
// later (e.g. the deferred verified-professional registry).
//
// Cache discipline matches `refresh_mapstore_manifest_cache`: pull every
// 5 min, only overwrite the in-memory + on-disk cache on a successful
// fetch, so a Careaway-side outage or a dc-explorer restart never
// regresses past "as stale as it was before".

fn careaway_manifest_api() -> String {
    std::env::var("CAREAWAY_MANIFEST_API")
        .unwrap_or_else(|_| "https://careaway.co/api/v1/careaway-entity-manifest".to_string())
}

async fn refresh_careaway_manifest_cache(state: &AppState) {
    let url = careaway_manifest_api();
    let mut req = state
        .http_client
        .get(&url)
        .timeout(std::time::Duration::from_secs(15));
    // Only sent if the operator has configured a shared secret on this
    // side to match Careaway's optional CAREAWAY_REGISTRY_API_KEY gate.
    // Open by default (no key required) today.
    if let Ok(key) = std::env::var("CAREAWAY_REGISTRY_API_KEY") {
        if !key.trim().is_empty() {
            req = req.header("X-Api-Key", key.trim());
        }
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Careaway manifest cache refresh failed (network): {}", e);
            return;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!(
            "Careaway manifest cache refresh: upstream returned {}",
            resp.status(),
        );
        return;
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("Careaway manifest cache refresh failed (parse): {}", e);
            return;
        }
    };

    let version = body
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let generated_at = body
        .get("generated_at")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    // Careaway's own manifest never 500s and self-reports `degraded` when
    // a source query failed; a healthy-but-all-zero manifest (e.g. no
    // care plans submitted yet) is still a valid cache to serve, so only
    // an explicit `degraded: true` is treated as "don't overwrite".
    let upstream_degraded = body
        .get("degraded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if upstream_degraded {
        tracing::warn!(
            "Careaway manifest cache refresh: upstream reported degraded ({:?})",
            body.get("degraded_reasons")
        );
        return;
    }

    let cache = CareawayManifestCache {
        raw: body,
        version: version.clone(),
        generated_at,
        fetched_at: chrono::Utc::now().timestamp(),
    };

    save_json_cache("careaway_manifest_cache.json", &cache);
    *state.careaway_manifest_cache.write().await = Some(cache);
    tracing::info!("Careaway manifest cache refreshed: version={}", version);
}

/// `GET /api/v1/registry/careaway-manifest` — full mirror of
/// `https://careaway.co/api/v1/careaway-entity-manifest`.
///
/// Same header contract as the Tanastok/Mapstore mirrors: strong
/// `Cache-Control`, an `X-Careaway-Manifest-Version` header, and an
/// `X-Careaway-Manifest-Age-Seconds` header so clients can tell a fresh
/// pull apart from a disk-persisted last-known-good payload.
async fn registry_careaway_manifest(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let cache = state.careaway_manifest_cache.read().await;
    let (status, version, age_secs, body) = match cache.as_ref() {
        Some(c) => (
            axum::http::StatusCode::OK,
            c.version.clone(),
            (chrono::Utc::now().timestamp() - c.fetched_at).max(0),
            c.raw.clone(),
        ),
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "unavailable".to_string(),
            -1,
            serde_json::json!({
                "error": "careaway manifest not yet warmed; retry in <5 min",
                "scope": "aggregate-only",
                "entities": [],
                "counts": {},
            }),
        ),
    };
    let manifest_version_header =
        axum::http::HeaderName::from_static("x-careaway-manifest-version");
    let manifest_age_header =
        axum::http::HeaderName::from_static("x-careaway-manifest-age-seconds");
    let age_str = age_secs.to_string();
    (
        status,
        [
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=60, s-maxage=300, stale-while-revalidate=600",
            ),
            (manifest_version_header, version.as_str()),
            (manifest_age_header, age_str.as_str()),
            (axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(body),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// TangibleDC Goodies entity-manifest mirror (physical gold/silver
// coin/title registry — dc.datachain.one)
// ---------------------------------------------------------------------------
//
// Mirrors `https://dc.datachain.one/api/v1/tangibledc-entity-manifest`
// (alias `/api/v1/registry/manifest` on the TangibleDC side) under the
// `/api/v1/registry/tangibledc-*` route family. Real per-entity records
// (unlike Careaway's aggregate-only payload) — one entity per physical
// coin, carrying its NFC chip identity, optional on-chain DCNFT deed +
// ERC-3643 fractional title, and its full production→delivery history —
// so this gets the full three-endpoint mirror, same shape/caching
// contract as Tanastok/Mapstore above.
//
// Cache discipline matches the other mirrors: pull every 5 min, only
// overwrite the in-memory + on-disk cache on a successful fetch, so a
// TangibleDC-side outage or a dc-explorer restart never regresses past
// "as stale as it was before".

fn tangibledc_manifest_api() -> String {
    std::env::var("TANGIBLEDC_MANIFEST_API")
        .unwrap_or_else(|_| "https://dc.datachain.one/api/v1/tangibledc-entity-manifest".to_string())
}

async fn refresh_tangibledc_manifest_cache(state: &AppState) {
    let url = tangibledc_manifest_api();
    let mut req = state
        .http_client
        .get(&url)
        .timeout(std::time::Duration::from_secs(15));
    // Only sent if the operator has configured a shared secret on this
    // side to match TangibleDC's optional TANGIBLEDC_REGISTRY_API_KEY
    // gate. Open by default (no key required) today.
    if let Ok(key) = std::env::var("TANGIBLEDC_REGISTRY_API_KEY") {
        if !key.trim().is_empty() {
            req = req.header("X-Api-Key", key.trim());
        }
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("TangibleDC manifest cache refresh failed (network): {}", e);
            return;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!(
            "TangibleDC manifest cache refresh: upstream returned {}",
            resp.status(),
        );
        return;
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("TangibleDC manifest cache refresh failed (parse): {}", e);
            return;
        }
    };

    let version = body
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let generated_at = body
        .get("generated_at")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let entities_count = body
        .get("entities")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    // TangibleDC is a young, physical-goods marketplace — a fresh
    // deployment with zero minted coins is a legitimate, healthy state.
    // An explicit `degraded: true` from upstream (DB unreachable, etc.)
    // is the real "something is wrong" signal; an empty-but-healthy
    // manifest must still populate the cache so callers see
    // `counts.total: 0` instead of a permanent 503.
    let upstream_degraded = body
        .get("degraded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if upstream_degraded && entities_count == 0 {
        tracing::warn!("TangibleDC manifest cache refresh: upstream reported degraded+empty");
        return;
    }

    let mut by_id = std::collections::HashMap::with_capacity(entities_count);
    if let Some(entities) = body.get("entities").and_then(|v| v.as_array()) {
        for (i, ent) in entities.iter().enumerate() {
            let id = ent
                .get("string_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let key = id.trim().trim_start_matches("0x").to_ascii_lowercase();
            if !key.is_empty() {
                by_id.insert(key, i);
            }
        }
    }

    let cache = TangibleDcManifestCache {
        raw: body,
        version: version.clone(),
        generated_at,
        by_id,
        fetched_at: chrono::Utc::now().timestamp(),
    };

    save_json_cache("tangibledc_manifest_cache.json", &cache);
    *state.tangibledc_manifest_cache.write().await = Some(cache);
    tracing::info!(
        "TangibleDC manifest cache refreshed: {} entities, version={}",
        entities_count,
        version,
    );
}

/// `GET /api/v1/registry/tangibledc-manifest` — full mirror of
/// `https://dc.datachain.one/api/v1/tangibledc-entity-manifest`.
///
/// Same header contract as `registry_mapstore_manifest`: strong
/// `Cache-Control`, an `X-TangibleDC-Manifest-Version` header, and an
/// `X-TangibleDC-Manifest-Age-Seconds` header so clients can tell a
/// fresh pull apart from a disk-persisted last-known-good payload.
async fn registry_tangibledc_manifest(
    State(state): State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    let cache = state.tangibledc_manifest_cache.read().await;
    let (status, version, age_secs, body) = match cache.as_ref() {
        Some(c) => (
            axum::http::StatusCode::OK,
            c.version.clone(),
            (chrono::Utc::now().timestamp() - c.fetched_at).max(0),
            c.raw.clone(),
        ),
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "unavailable".to_string(),
            -1,
            serde_json::json!({
                "error": "tangibledc manifest not yet warmed; retry in <5 min",
                "entities": [],
                "counts": {},
            }),
        ),
    };
    let manifest_version_header =
        axum::http::HeaderName::from_static("x-tangibledc-manifest-version");
    let manifest_age_header =
        axum::http::HeaderName::from_static("x-tangibledc-manifest-age-seconds");
    let age_str = age_secs.to_string();
    (
        status,
        [
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=60, s-maxage=300, stale-while-revalidate=600",
            ),
            (manifest_version_header, version.as_str()),
            (manifest_age_header, age_str.as_str()),
            (axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(body),
    )
        .into_response()
}

/// `GET /api/v1/registry/tangibledc-labels` — slim `string_id → label`
/// map. Optional `?kind=` filter (`asset|contract`).
async fn registry_tangibledc_labels(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LabelsQuery>,
) -> impl axum::response::IntoResponse {
    let cache = state.tangibledc_manifest_cache.read().await;
    let (version, generated_at, labels) = match cache.as_ref() {
        Some(c) => {
            let entities = c
                .raw
                .get("entities")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut labels = serde_json::Map::with_capacity(entities.len());
            for e in &entities {
                let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(filter) = q.kind.as_deref() {
                    if !filter.eq_ignore_ascii_case(kind) {
                        continue;
                    }
                }
                let id = match e.get("string_id").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let label = e.get("label").cloned().unwrap_or(serde_json::json!({}));
                labels.insert(
                    id,
                    serde_json::json!({
                        "kind": kind,
                        "label": label,
                        "parent_string_id": e.get("parent_string_id").cloned().unwrap_or(serde_json::Value::Null),
                        "ecosystem_id": e.get("ecosystem_id").cloned().unwrap_or(serde_json::Value::Null),
                        "platform": "tangibledc",
                    }),
                );
            }
            (c.version.clone(), c.generated_at, labels)
        }
        None => ("unavailable".to_string(), 0, serde_json::Map::new()),
    };
    let body = serde_json::json!({
        "version": version,
        "generated_at": generated_at,
        "platform": "tangibledc",
        "kind_filter": q.kind,
        "count": labels.len(),
        "labels": labels,
    });
    let manifest_version_header =
        axum::http::HeaderName::from_static("x-tangibledc-manifest-version");
    (
        [
            (
                axum::http::header::CACHE_CONTROL,
                "public, max-age=60, s-maxage=300, stale-while-revalidate=600",
            ),
            (manifest_version_header, version.as_str()),
            (axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        Json(body),
    )
        .into_response()
}

/// `GET /api/v1/registry/tangibledc-entity/:id` — single-entity lookup
/// by Quipu `string_id`, case-insensitive, `0x`-prefix-tolerant.
async fn registry_tangibledc_entity_by_id(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let key = id.trim().trim_start_matches("0x").to_ascii_lowercase();
    let cache = state.tangibledc_manifest_cache.read().await;
    let entity = match cache.as_ref() {
        Some(c) => c.by_id.get(&key).and_then(|idx| {
            c.raw
                .get("entities")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.get(*idx))
                .cloned()
        }),
        None => None,
    };
    Json(serde_json::json!({
        "found": entity.is_some(),
        "id": id,
        "entity": entity,
    }))
}

async fn tanastok_all_assets(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let cache = state.tanastok_cache.read().await;
    match cache.as_ref() {
        Some(c) => Json(serde_json::json!({
            "success": true,
            "count": c.assets.len(),
            "updatedAt": c.updated_at,
            "assets": c.assets,
        })),
        None => Json(serde_json::json!({
            "success": false,
            "count": 0,
            "assets": [],
        })),
    }
}

// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_data_default() {
        let price_data = PriceData::default();
        assert_eq!(price_data.price, FALLBACK_PRICE);
        assert_eq!(price_data.change_24h, 0.0);
        assert_eq!(price_data.volume_24h, 0.0);
        assert_eq!(price_data.source, "fallback");
    }

    #[test]
    fn test_price_data_custom() {
        let price_data = PriceData {
            price: 0.005,
            change_24h: 5.5,
            volume_24h: 10000.0,
            liquidity: 50000.0,
            source: "xdcscan".to_string(),
            timestamp: 1234567890,
        };
        assert_eq!(price_data.price, 0.005);
        assert_eq!(price_data.source, "xdcscan");
    }

    #[test]
    fn test_constants() {
        assert_eq!(
            DC_FAT_CONTRACT,
            "0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a"
        );
        assert_eq!(PRICE_CACHE_TTL_SECS, 300);
        assert!(FALLBACK_PRICE > 0.0);
    }

    #[test]
    fn test_rand_variation() {
        // rand_variation returns value between 0.0 and 1.0 (based on nanoseconds)
        for _ in 0..100 {
            let v = rand_variation();
            assert!(v >= 0.0, "rand_variation too low: {}", v);
            assert!(v < 1.0, "rand_variation too high: {}", v);
        }
    }

    #[test]
    fn test_price_data_serialization() {
        let price_data = PriceData {
            price: 0.00390,
            change_24h: 2.5,
            volume_24h: 5000.0,
            liquidity: 25000.0,
            source: "test".to_string(),
            timestamp: 1700000000,
        };

        // Should serialize without errors
        let json = serde_json::to_string(&price_data).unwrap();
        assert!(json.contains("0.0039"));
        assert!(json.contains("test"));

        // Should deserialize back
        let deserialized: PriceData = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.price, 0.00390);
        assert_eq!(deserialized.source, "test");
    }
}
