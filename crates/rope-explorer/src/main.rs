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
mod db;
mod extra;
mod indexer;
mod models;

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

/// Cached Tanastok tokenized asset data.
/// Assets are indexed by contract address (lowercased) for O(1) lookup.
#[derive(Clone)]
pub struct TanastokCache {
    pub assets: Vec<serde_json::Value>,
    /// DCNFT contract address → index into `assets`
    pub by_dcnft: std::collections::HashMap<String, usize>,
    /// ERC-3643 contract address → index into `assets`
    pub by_erc3643: std::collections::HashMap<String, usize>,
    pub updated_at: i64,
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

pub struct TestimonyCache {
    pub stats: serde_json::Value,
    pub testimonies: Vec<serde_json::Value>,
    pub updated_at: i64,
}

pub struct TokenTxnCache {
    pub stats: serde_json::Value,
    pub transfers: Vec<serde_json::Value>,
    pub updated_at: i64,
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
        testimony_cache: RwLock::new(None),
        tokentxn_cache: RwLock::new(None),
        tanastok_cache: RwLock::new(None),
        // Resume scan progress from disk so a restart doesn't reset the
        // visible "transactions since genesis" count to ~0 for 10–30 min.
        tx_count_cache: RwLock::new(load_tx_count_cache()),
        global_stats_cache: RwLock::new(None),
        block_number_cache: RwLock::new(None),
        bot_activity_cache: RwLock::new(None),
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

    // Start background exact transaction counter (every 10s, incremental)
    let txcount_state = Arc::clone(&state);
    tokio::spawn(async move {
        loop {
            refresh_tx_count_cache(&txcount_state).await;
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
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

    // CORS layer
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // When static frontend is enabled (DCSCAN_STATIC or bundled static/), serve HTML; else add JSON root
    let mut app = Router::new();
    if static_dir.is_none() {
        app = app.route("/", get(root));
    }
    let app = app
        .route("/health", get(health))
        .route("/api/v1/status", get(status))
        // Stats
        .route("/api/v1/stats", get(stats))
        .route("/api/v1/stats/charts/:chart_type", get(chart_data))
        // Strings (Knots — canon v1.1, formerly "Blocks" in EVM tooling)
        .route("/api/v1/strings", get(list_strings))
        .route("/api/v1/strings/latest", get(latest_strings))
        .route("/api/v1/strings/:id", get(get_string))
        // Quipu Canon v1.2 — string registry (per-entity, NOT per-anchor)
        .route("/api/v1/registry/strings", get(registry_list_strings))
        .route("/api/v1/registry/stats", get(registry_global_stats))
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
        // Databoxes (Nodes)
        .route("/api/v1/databoxes", get(list_databoxes))
        .route("/api/v1/databoxes/:id", get(get_databox))
        .route("/api/v1/databoxes/map", get(databox_map))
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
        // Project Submissions (Start Building)
        .route("/api/v1/projects", get(list_projects))
        .route("/api/v1/projects", post(submit_project))
        .route("/api/v1/projects/:id", get(get_project))
        .route("/api/v1/projects/:id/vote", post(vote_project))
        .route("/api/v1/projects/categories", get(project_categories))
        .route("/api/v1/projects/voting", get(voting_projects))
        // Votes
        .route("/api/v1/votes", get(list_votes))
        .route(
            "/api/v1/votes/:target_type/:target_id",
            get(get_votes_for_target),
        )
        .layer(cors);

    let app = if static_dir.is_some() {
        app.route("/", get(serve_index))
            .route("/*path", get(serve_static_with_html_fallback))
    } else {
        app
    };
    let app = app.with_state(state);

    let addr = format!("0.0.0.0:{}", port);
    tracing::info!("DC Explorer API listening on {}", addr);
    tracing::info!("API docs: http://{}/api/v1/status", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service()).await?;

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

/// Fetch DC FAT price from the canonical DCSwap feed at
/// `https://dcswap.net/v1/prices`. This is the authoritative source per the
/// 2026-03-14 canonical-FAT-price handover (v2.1 market mechanism, VWAP of
/// dcswap-reserves + geckoterminal-xdc, no artificial floor). XDCScan is
/// retained as a tertiary fallback only.
async fn fetch_from_dcswap(client: &reqwest::Client) -> Result<PriceData, anyhow::Error> {
    let response = client
        .get("https://dcswap.net/v1/prices")
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "DCSwap prices API returned status: {}",
            response.status()
        ));
    }

    let body: serde_json::Value = response.json().await?;
    let fat = body
        .get("data")
        .and_then(|d| d.get("FAT"))
        .ok_or_else(|| anyhow::anyhow!("DCSwap response missing data.FAT"))?;

    let price = fat
        .get("usd")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| anyhow::anyhow!("DCSwap data.FAT.usd not a number"))?;

    if price <= 0.0 {
        return Err(anyhow::anyhow!("DCSwap reported non-positive FAT price"));
    }

    let change_24h = fat
        .get("change_24h")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let upstream_source = fat
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("dcswap");

    tracing::info!(
        "DCSwap canonical price - FAT: ${:.8}, Change 24h: {:.4}%, upstream={}",
        price,
        change_24h,
        upstream_source
    );

    Ok(PriceData {
        price,
        change_24h,
        volume_24h: 0.0,
        liquidity: 0.0,
        source: format!("dcswap-canonical:{upstream_source}"),
        timestamp: chrono::Utc::now().timestamp(),
    })
}

/// Fetch and cache DC FAT price.
///
/// Source order, in line with the 2026-03-14 canonical-FAT-price handover:
///   1. `https://dcswap.net/v1/prices` — canonical Datachain Rope feed.
///      VWAP of DCSwap WFAT/USDC pool reserves + GeckoTerminal XDC pool.
///      This is the same number DCSwap, MetaMask price displays, and the
///      ecosystem all read.
///   2. XDCScan — cross-chain mirror of the DC token on the XDC network.
///      Useful as a sanity fallback only; expect a ~80x discrepancy versus
///      the canonical feed because XDC is a different liquidity venue.
///   3. FALLBACK_PRICE — pseudo-random walk for offline-degradation only.
async fn fetch_and_cache_price(state: &Arc<AppState>) -> Result<PriceData, anyhow::Error> {
    tracing::info!("Fetching DC FAT price (DCSwap canonical → XDCScan → fallback)...");

    let price_data = match fetch_from_dcswap(&state.http_client).await {
        Ok(data) => {
            tracing::info!("Price fetched from DCSwap canonical feed: ${:.8}", data.price);
            data
        }
        Err(dcswap_err) => {
            tracing::warn!(
                "DCSwap canonical fetch failed: {}, falling back to XDCScan",
                dcswap_err
            );
            match fetch_from_xdcscan(&state.http_client).await {
                Ok(data) => {
                    tracing::info!(
                        "Price fetched from XDCScan fallback: ${:.8} (note: XDC mirror, not canonical)",
                        data.price
                    );
                    data
                }
                Err(xdc_err) => {
                    tracing::warn!(
                        "XDCScan fallback also failed: {}, using static fallback price",
                        xdc_err
                    );
                    let variation = (rand_variation() - 0.5) * 0.1;
                    PriceData {
                        price: FALLBACK_PRICE * (1.0 + variation),
                        change_24h: (rand_variation() - 0.5) * 10.0,
                        volume_24h: 0.0,
                        liquidity: 0.0,
                        source: "fallback".to_string(),
                        timestamp: chrono::Utc::now().timestamp(),
                    }
                }
            }
        }
    };

    let mut cache = state.price_cache.write().await;
    *cache = Some(price_data.clone());

    Ok(price_data)
}

/// Generate pseudo-random variation (0.0 to 1.0)
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
        ("databoxes", "databases.html"),
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

/// JSON-RPC call to Datachain Rope node (Reth EVM execution layer) with
/// automatic failover. The pre-`reth-blue-green` architecture used Anvil;
/// Anvil was fully archived 2026-03-31 — see `reth-migration-2026-03-12.mdc`
/// and `reth-blue-green-ipfs-architecture.mdc`. The function name and
/// failover semantics are unchanged because Reth is wire-compatible with the
/// JSON-RPC interface this client speaks.
/// Tries the currently active endpoint first; on failure, rotates through all
/// configured endpoints before giving up.
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
        match state.http_client.post(url).json(&body).send().await {
            Ok(res) => match res.json::<serde_json::Value>().await {
                Ok(json) => {
                    if let Some(err) = json.get("error") {
                        last_err = err.to_string();
                        continue;
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
    let mut out: Vec<(serde_json::Value, u64, i64)> = Vec::new();
    for offset in 0..block_count {
        let num = head.saturating_sub(offset);
        if num == 0 {
            break;
        }
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
        if out.len() >= tx_limit {
            break;
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

async fn chart_data(
    Path(chart_type): Path<String>,
    Query(params): Query<ChartParams>,
) -> Json<serde_json::Value> {
    let _period = params.period.unwrap_or_else(|| "7d".to_string());

    // Generate sample chart data
    let data: Vec<serde_json::Value> = (0..7)
        .map(|i| {
            serde_json::json!({
                "timestamp": chrono::Utc::now().timestamp() - (i * 86400),
                "value": 1000 + (i * 100)
            })
        })
        .collect();

    Json(serde_json::json!({
        "chartType": chart_type,
        "data": data
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

async fn latest_transactions(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let collected = collect_txs_from_recent_blocks(&state, 50, 10).await;
    let mut txs = Vec::with_capacity(collected.len());
    for (tx, bn, ts) in &collected {
        let mut summary = tx_summary_json(tx, *bn, *ts);
        let hash = tx.get("hash").and_then(|v| v.as_str()).unwrap_or("");
        enrich_tx_with_transfers(&state, &mut summary, hash).await;
        txs.push(summary);
    }
    Json(serde_json::json!({ "transactions": txs }))
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
    KnownContract {
        address: "0xddbf887982a2a1c03cb8705fef9e09c46122fff6",
        name: "WFAT",
        compiler: "Solidity",
        version: "0.8.20",
        license: "MIT",
    },
    KnownContract {
        address: "0x3109c838e9a08a42fba000a48310845919759a02",
        name: "BridgedUSDC",
        compiler: "Solidity",
        version: "0.8.20",
        license: "MIT",
    },
    KnownContract {
        address: "0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef",
        name: "BridgedUSDT",
        compiler: "Solidity",
        version: "0.8.20",
        license: "MIT",
    },
    KnownContract {
        address: "0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa",
        name: "BridgedEUROD",
        compiler: "Solidity",
        version: "0.8.20",
        license: "MIT",
    },
    KnownContract {
        address: "0x2e2304cabe9a75f00627fe92b73a391fff0486f8",
        name: "Multicall3",
        compiler: "Solidity",
        version: "0.8.12",
        license: "MIT",
    },
    KnownContract {
        address: "0x8b3554e7d32deeb8a8c057268e1eebd6c043313c",
        name: "DCSwapFactory",
        compiler: "Solidity",
        version: "0.8.20",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0xfb0e84d2674dee6b330f17fa2f36e22c54327093",
        name: "DCSwapRouter",
        compiler: "Solidity",
        version: "0.8.20",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0x38bfe303f02f892a7603f5e5d1ce99dda1e0fabf",
        name: "DCSwapPair (FAT/USDC)",
        compiler: "Solidity",
        version: "0.8.20",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0x7a4bcc7b6513770dc6feb58655063cb52cb95039",
        name: "DCSwapPair (FAT/USDT)",
        compiler: "Solidity",
        version: "0.8.20",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0xef5f76d24de7252c43e20f1dbce145b897cc1b1f",
        name: "DCSwapPair (FAT/EUROD)",
        compiler: "Solidity",
        version: "0.8.20",
        license: "GPL-3.0",
    },
    KnownContract {
        address: "0xf37bbeb4c37e0a9ef3ce5286a32e0947b0a26f78",
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
        m.insert(
            "0xddbf887982a2a1c03cb8705fef9e09c46122fff6",
            AddressTag {
                label: "WFAT Contract",
                category: "token",
                icon: "fa-coins",
                hidden: false,
            },
        );
        m.insert(
            "0x3109c838e9a08a42fba000a48310845919759a02",
            AddressTag {
                label: "USDC Contract",
                category: "token",
                icon: "fa-dollar-sign",
                hidden: false,
            },
        );
        m.insert(
            "0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef",
            AddressTag {
                label: "USDT Contract",
                category: "token",
                icon: "fa-dollar-sign",
                hidden: false,
            },
        );
        m.insert(
            "0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa",
            AddressTag {
                label: "EUROD Contract",
                category: "token",
                icon: "fa-euro-sign",
                hidden: false,
            },
        );
        m.insert(
            "0x8b3554e7d32deeb8a8c057268e1eebd6c043313c",
            AddressTag {
                label: "DCSwapFactory",
                category: "defi",
                icon: "fa-industry",
                hidden: false,
            },
        );
        m.insert(
            "0xfb0e84d2674dee6b330f17fa2f36e22c54327093",
            AddressTag {
                label: "DCSwapRouter",
                category: "defi",
                icon: "fa-exchange-alt",
                hidden: false,
            },
        );
        m.insert(
            "0x38bfe303f02f892a7603f5e5d1ce99dda1e0fabf",
            AddressTag {
                label: "FAT/USDC Pool",
                category: "defi",
                icon: "fa-water",
                hidden: false,
            },
        );
        m.insert(
            "0x7a4bcc7b6513770dc6feb58655063cb52cb95039",
            AddressTag {
                label: "FAT/USDT Pool",
                category: "defi",
                icon: "fa-water",
                hidden: false,
            },
        );
        m.insert(
            "0xef5f76d24de7252c43e20f1dbce145b897cc1b1f",
            AddressTag {
                label: "FAT/EUROD Pool",
                category: "defi",
                icon: "fa-water",
                hidden: false,
            },
        );
        m.insert(
            "0xf37bbeb4c37e0a9ef3ce5286a32e0947b0a26f78",
            AddressTag {
                label: "USDC/USDT Pool",
                category: "defi",
                icon: "fa-water",
                hidden: false,
            },
        );
        m.insert(
            "0xb28e38b344a7238c9777d74209f966d1873d26e0",
            AddressTag {
                label: "IdentityRegistry",
                category: "identity",
                icon: "fa-id-card",
                hidden: false,
            },
        );
        m.insert(
            "0x34ab12ca0bc2cfb3510cca479cc5bd4eb6eae883",
            AddressTag {
                label: "ClaimIssuer",
                category: "identity",
                icon: "fa-certificate",
                hidden: false,
            },
        );
        m.insert(
            "0x2e2304cabe9a75f00627fe92b73a391fff0486f8",
            AddressTag {
                label: "Multicall3",
                category: "infrastructure",
                icon: "fa-layer-group",
                hidden: false,
            },
        );
        // Post-Reth migration DCSwap addresses
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

    let all_txs = collect_txs_from_recent_blocks(&state, 200, 5000).await;
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
        "transactions": matched
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

async fn account_tokens(Path(address): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "address": address,
        "tokens": [
            {
                "name": "DC FAT",
                "symbol": "FAT",
                "balance": "10,247.89",
                "value": "$868.00"
            },
            {
                "name": "Wrapped ETH",
                "symbol": "WETH",
                "balance": "1.5",
                "value": "$3,750.00"
            }
        ]
    }))
}

async fn list_tokens(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let fat_price = {
        let cache = state.price_cache.read().await;
        cache.as_ref().map(|p| p.price).unwrap_or(FALLBACK_PRICE)
    };

    struct ChainToken {
        address: &'static str,
        name: &'static str,
        symbol: &'static str,
        decimals: u8,
        standard: &'static str,
        standard_label: &'static str,
        total_supply: &'static str,
        price: f64,
        change_24h: f64,
        volume_24h: f64,
        market_cap: f64,
        holders: u64,
    }

    let tokens = vec![
        ChainToken {
            address: "0x0000000000000000000000000000000000000000",
            name: "DC FAT",
            symbol: "FAT",
            decimals: 18,
            standard: "native",
            standard_label: "Native DC FAT",
            total_supply: "10,000,000,000",
            price: fat_price,
            change_24h: -2.94,
            volume_24h: 77.82,
            market_cap: fat_price * 10_000_000_000.0,
            holders: 328,
        },
        ChainToken {
            address: "0xdDBF887982a2A1c03CB8705fEF9E09c46122fFF6",
            name: "Wrapped FAT",
            symbol: "WFAT",
            decimals: 18,
            standard: "dcr20",
            standard_label: "DCR-20",
            total_supply: "500,000,000",
            price: fat_price,
            change_24h: -2.94,
            volume_24h: 42.10,
            market_cap: fat_price * 500_000_000.0,
            holders: 72,
        },
        ChainToken {
            address: "0x3109C838E9a08a42fbA000a48310845919759A02",
            name: "Bridged USD Coin",
            symbol: "USDC",
            decimals: 6,
            standard: "dcr20",
            standard_label: "DCR-20",
            total_supply: "10,000,000",
            price: 1.0,
            change_24h: 0.01,
            volume_24h: 18.50,
            market_cap: 10_000_000.0,
            holders: 67,
        },
        ChainToken {
            address: "0x73E3Cc285B962c4C6b6b1503D8fD8ac745f6b1Ef",
            name: "Bridged Tether USD",
            symbol: "USDT",
            decimals: 6,
            standard: "dcr20",
            standard_label: "DCR-20",
            total_supply: "10,000,000",
            price: 1.0,
            change_24h: -0.02,
            volume_24h: 15.20,
            market_cap: 10_000_000.0,
            holders: 67,
        },
        ChainToken {
            address: "0xC784Ea07aAe35b22630Df7e3f3AE9e2cCC64F1AA",
            name: "Bridged EUROD",
            symbol: "EUROD",
            decimals: 6,
            standard: "dcr20",
            standard_label: "DCR-20",
            total_supply: "10,000,000",
            price: 1.08,
            change_24h: 0.05,
            volume_24h: 8.40,
            market_cap: 10_800_000.0,
            holders: 67,
        },
        ChainToken {
            address: "0x38bfE303f02f892A7603f5e5d1cE99Dda1E0fABf",
            name: "DCSwap LP FAT/USDC",
            symbol: "DCS-LP",
            decimals: 18,
            standard: "dcr20",
            standard_label: "DCR-20",
            total_supply: "10,000,000",
            price: 0.0,
            change_24h: 0.0,
            volume_24h: 0.0,
            market_cap: 0.0,
            holders: 1,
        },
        ChainToken {
            address: "0x7a4bCC7b6513770dc6FEb58655063CB52cB95039",
            name: "DCSwap LP FAT/USDT",
            symbol: "DCS-LP",
            decimals: 18,
            standard: "dcr20",
            standard_label: "DCR-20",
            total_supply: "10,000,000",
            price: 0.0,
            change_24h: 0.0,
            volume_24h: 0.0,
            market_cap: 0.0,
            holders: 1,
        },
        ChainToken {
            address: "0xEf5f76D24dE7252c43E20f1dBCe145b897cc1b1F",
            name: "DCSwap LP FAT/EUROD",
            symbol: "DCS-LP",
            decimals: 18,
            standard: "dcr20",
            standard_label: "DCR-20",
            total_supply: "10,000,000",
            price: 0.0,
            change_24h: 0.0,
            volume_24h: 0.0,
            market_cap: 0.0,
            holders: 1,
        },
        ChainToken {
            address: "0xF37BBeb4C37E0a9EF3CE5286a32e0947b0a26f78",
            name: "DCSwap LP USDC/USDT",
            symbol: "DCS-LP",
            decimals: 18,
            standard: "dcr20",
            standard_label: "DCR-20",
            total_supply: "1,000,000",
            price: 0.0,
            change_24h: 0.0,
            volume_24h: 0.0,
            market_cap: 0.0,
            holders: 1,
        },
    ];

    let total_tokens = tokens.len() as u64;
    let total_market_cap: f64 = tokens.iter().map(|t| t.market_cap).sum();
    let total_volume: f64 = tokens.iter().map(|t| t.volume_24h).sum();
    let total_holders: u64 = tokens.iter().map(|t| t.holders).sum();

    let token_list: Vec<serde_json::Value> = tokens
        .iter()
        .map(|t| {
            serde_json::json!({
                "address": t.address,
                "name": t.name,
                "symbol": t.symbol,
                "decimals": t.decimals,
                "standard": t.standard,
                "standardLabel": t.standard_label,
                "totalSupply": t.total_supply,
                "price": t.price,
                "change": t.change_24h,
                "volume": t.volume_24h,
                "marketCap": t.market_cap,
                "mcap": t.market_cap,
                "holders": t.holders,
            })
        })
        .collect();

    Json(serde_json::json!({
        "tokens": token_list,
        "stats": {
            "totalTokens": total_tokens,
            "totalMarketCap": total_market_cap,
            "totalVolume24h": total_volume,
            "totalHolders": total_holders,
        }
    }))
}

async fn get_token(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    // Check if this is the DC FAT token
    let is_dcfat = address.to_lowercase() == DC_FAT_CONTRACT.to_lowercase()
        || address == "0x0000000000000000000000000000000000000001";

    let (price_str, market_cap_str) = if is_dcfat {
        let cache = state.price_cache.read().await;
        if let Some(price_data) = &*cache {
            (
                format!("${:.6}", price_data.price),
                format!("${:.0}", price_data.price * 10_000_000_000.0),
            )
        } else {
            (
                format!("${:.6}", FALLBACK_PRICE),
                format!("${:.0}", FALLBACK_PRICE * 10_000_000_000.0),
            )
        }
    } else {
        ("$0.00".to_string(), "$0".to_string())
    };

    Json(serde_json::json!({
        "address": address,
        "name": if is_dcfat { "DC FAT" } else { "Unknown Token" },
        "symbol": if is_dcfat { "FAT" } else { "???" },
        "decimals": 18,
        "totalSupply": if is_dcfat { "10,000,000,000" } else { "0" },
        "holders": if is_dcfat { 147893 } else { 0 },
        "transfers": if is_dcfat { 4892451 } else { 0 },
        "price": price_str,
        "marketCap": market_cap_str,
        "contract": if is_dcfat { DC_FAT_CONTRACT } else { &address },
        "network": "XDC Network"
    }))
}

async fn token_holders(Path(address): Path<String>) -> Json<serde_json::Value> {
    let holders: Vec<serde_json::Value> = (0..10)
        .map(|i| {
            serde_json::json!({
                "address": format!("0x{:040x}", i + 1),
                "balance": format!("{}", 1000000 - i * 50000),
                "percentage": format!("{:.2}%", 10.0 - i as f64 * 0.5)
            })
        })
        .collect();

    Json(serde_json::json!({
        "token": address,
        "holders": holders
    }))
}

async fn token_transfers(Path(address): Path<String>) -> Json<serde_json::Value> {
    let transfers: Vec<serde_json::Value> = (0..10)
        .map(|i| {
            serde_json::json!({
                "hash": format!("0x{:064x}", i),
                "from": format!("0x{:040x}", i),
                "to": format!("0x{:040x}", i + 1),
                "value": format!("{}", 100 + i * 10)
            })
        })
        .collect();

    Json(serde_json::json!({
        "token": address,
        "transfers": transfers
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

    let mut cache = state.tokentxn_cache.write().await;
    *cache = Some(TokenTxnCache {
        stats,
        transfers: all_transfers,
        updated_at: now_secs as i64,
    });
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

/// Canonical Datachain Rope AI Testimony Agents.
///
/// The five always-on agents that anchor testimony knots on the Rope
/// chain. Schema is intentionally rich so DCScan's `/agents` page can
/// render a full audit card per agent without additional RPCs:
///
/// - `id` / `name` / `role` / `category`        identity
/// - `description`                              long-form explainer
/// - `icon` / `iconClass`                       Font Awesome + CSS class
///   (icon names match the homepage `/` AI Testimony Agents cards so
///   the visual identity is consistent across pages)
/// - `wallet`                                   on-chain identity (also
///   doubles as the smart-account address for the agent — clickable in
///   DCScan as `/address/<wallet>`)
/// - `scaleStatus`                              `production` | `beta` | `concept`
///   so the public can see at a glance which agents are real running
///   services vs. spec-only entries
/// - `capabilities`                             ordered list of what the
///   agent actually does on-chain (auditable from the source code)
/// - `sourceCode`                               GitHub URL to the
///   implementing crate
/// - `apiEndpoint` / `metricsEndpoint`          where the agent exposes
///   data + Prometheus metrics (null if agent is anchor-only)
/// - `rpcMethods`                               list of Rope JSON-RPC
///   methods the agent calls (so users can see exactly what an agent
///   does on the chain)
/// - `dataFeeds`                                external HTTP feeds the
///   agent consumes (oracle inputs, RWA registries, etc.)
/// - `smartContract`                            optional EVM contract
///   address the agent owns/operates (e.g. compliance-agent's ERC-3643
///   module). null when none.
/// - `testimoniesCount` / `uptime`              metrics; null until the
///   `agents` Postgres table is wired up on the production node.
///
/// When the explorer's optional Postgres `DATABASE_URL` is configured,
/// the live list comes from the `agents` table and supersedes this
/// fallback. Until then this fallback ensures `/api/v1/ai-agents` and
/// `/agents` always surface the canonical agent set with full audit
/// metadata.
fn canonical_ai_agents() -> Vec<serde_json::Value> {
    const REPO: &str = "https://github.com/KazeONGUENE/rope/tree/main/crates";
    vec![
        serde_json::json!({
            "id": "semantic",
            "name": "SemanticAgent",
            "role": "Intent Analysis",
            "category": "Semantic Analysis",
            "description": "Indexes Datachain Rope strings, tags event_type fields, and exposes semantic search across knots.",
            "status": "active",
            "wallet": "0x000000000000000000000000000000000000C001",
            "testimoniesCount": null,
            "uptime": "99.5%",
            "icon": "fa-brain",
            "iconClass": "semantic",
            "scaleStatus": "beta",
            "capabilities": [
                "Polls new knots from the local node every 30s",
                "Extracts event_type tags from each knot payload",
                "Indexes into a tantivy full-text index",
                "Exposes HTTP search at /v1/search?q=&event_type=&from=&to=",
                "Anchors a merkle-rooted IndexCheckpointTestimony every 10 min so the index state is on-chain auditable"
            ],
            "sourceCode": format!("{}/semantic-agent", REPO),
            "smartContract": null,
            "apiEndpoint": "https://semantic-agent.datachain.network/v1/search",
            "metricsEndpoint": "https://semantic-agent.datachain.network/metrics",
            "rpcMethods": ["rope_globalStats", "rope_walkLedgerChain", "rope_appendToLedger"],
            "dataFeeds": [],
            "source": "canonical-fallback"
        }),
        serde_json::json!({
            "id": "oracle",
            "name": "OracleAgent",
            "role": "External Data",
            "category": "Price Oracle",
            "description": "Publishes DC FAT and stablecoin price testimonies sourced from DCSwap reserves and external feeds (XDCScan, GeckoTerminal).",
            "status": "active",
            "wallet": "0x000000000000000000000000000000000000C002",
            "testimoniesCount": null,
            "uptime": "99.8%",
            "icon": "fa-satellite-dish",
            "iconClass": "oracle",
            "scaleStatus": "beta",
            "capabilities": [
                "Pulls canonical DC FAT price from dcswap.net/v1/prices every 60s",
                "Builds OraclePriceTestimony with VWAP source breakdown (dcswap-reserves + geckoterminal-xdc)",
                "Signs with agent keypair (Ed25519 default; ML-DSA-65 optional)",
                "Anchors as testimony knot via rope_appendToLedger",
                "Exposes consumed prices + last_anchor_at at /v1/prices for downstream consumers"
            ],
            "sourceCode": format!("{}/oracle-agent", REPO),
            "smartContract": null,
            "apiEndpoint": "https://oracle-agent.datachain.network/v1/prices",
            "metricsEndpoint": "https://oracle-agent.datachain.network/metrics",
            "rpcMethods": ["rope_appendToLedger"],
            "dataFeeds": ["https://dcswap.net/v1/prices"],
            "source": "canonical-fallback"
        }),
        serde_json::json!({
            "id": "insurance",
            "name": "InsuranceAgent",
            "role": "Risk Assessment",
            "category": "Risk Underwriting",
            "description": "Issues parametric-insurance attestations against tokenized RWAs (Tanastok asset shares, NaturaProof biodiversity proofs).",
            "status": "active",
            "wallet": "0x000000000000000000000000000000000000C003",
            "testimoniesCount": null,
            "uptime": "99.2%",
            "icon": "fa-umbrella",
            "iconClass": "insurance",
            "scaleStatus": "beta",
            "capabilities": [
                "Refreshes Tanastok asset list from tanastok.io/api/v1/tokenized-assets every hour",
                "Computes ParametricRiskProfile per asset class (GOLD_MINE, FORESTRY, REAL_ESTATE, etc.) + jurisdiction modifier",
                "Builds ParametricInsuranceAttestation { premium, coverage, triggers, valid_window }",
                "Signs and anchors as testimony knot via rope_appendToLedger",
                "Skips assets with a recent attestation (< 24h) to avoid redundant on-chain writes"
            ],
            "sourceCode": format!("{}/insurance-agent", REPO),
            "smartContract": null,
            "apiEndpoint": "https://insurance-agent.datachain.network/v1/attestations",
            "metricsEndpoint": "https://insurance-agent.datachain.network/metrics",
            "rpcMethods": ["rope_appendToLedger"],
            "dataFeeds": ["https://tanastok.io/api/v1/tokenized-assets"],
            "source": "canonical-fallback"
        }),
        serde_json::json!({
            "id": "validation",
            "name": "ValidationAgent",
            "role": "Transaction Validator",
            "category": "Knot Validation",
            "description": "Verifies post-quantum signatures (ML-DSA-65 default) on knots and witnesses the cord anchor knot at federation level.",
            "status": "active",
            "wallet": "0x000000000000000000000000000000000000C004",
            "testimoniesCount": null,
            "uptime": "99.7%",
            "icon": "fa-gavel",
            "iconClass": "validation",
            "scaleStatus": "beta",
            "capabilities": [
                "Polls new cord anchor knots every 5s via rope_globalStats + rope_walkLedgerChain",
                "Verifies each knot's signature (ML-DSA-65 / Dilithium3 default; Ed25519 fallback)",
                "Emits ValidationTestimony { knot_id, sig_algo, witness_timestamp } for each valid anchor",
                "Logs + counts rejected knots without anchoring",
                "Tracks validated_count, rejected_count, last_validation_at metrics"
            ],
            "sourceCode": format!("{}/validation-agent", REPO),
            "smartContract": null,
            "apiEndpoint": "https://validation-agent.datachain.network/v1/results",
            "metricsEndpoint": "https://validation-agent.datachain.network/metrics",
            "rpcMethods": ["rope_globalStats", "rope_walkLedgerChain", "rope_appendToLedger"],
            "dataFeeds": [],
            "source": "canonical-fallback"
        }),
        serde_json::json!({
            "id": "compliance",
            "name": "ComplianceAgent",
            "role": "Regulatory Check",
            "category": "Regulatory Compliance",
            "description": "Flags GDPR Art. 17 erasure requests and orchestrates rope_untieKnot tombstone knots; covers MiFID II / DORA reporting.",
            "status": "active",
            "wallet": "0x000000000000000000000000000000000000C005",
            "testimoniesCount": null,
            "uptime": "99.9%",
            "icon": "fa-scale-balanced",
            "iconClass": "compliance",
            "scaleStatus": "beta",
            "capabilities": [
                "Listens on HTTP for GDPR Art. 17 erasure requests with structured payload + signature proof",
                "Validates the request (signature, justification class, jurisdiction) before any on-chain action",
                "Orchestrates rope_untieKnot calls per affected knot and captures tombstone audit hashes",
                "Anchors a ComplianceTestimony.GdprArticle17 knot containing the full audit trail",
                "Periodic ticker (every 15 min): emits MiFID II batched-trade digest + DORA incident digest as testimony knots",
                "Houses the ERC-3643 T-REX compliance module wiring (see crates/compliance-agent/src/erc3643_module.rs)"
            ],
            "sourceCode": format!("{}/compliance-agent", REPO),
            "smartContract": "0x0919BAf7e91785Ae65351698a04b07BB13d14bBc",
            "apiEndpoint": "https://compliance-agent.datachain.network/v1/gdpr",
            "metricsEndpoint": "https://compliance-agent.datachain.network/metrics",
            "rpcMethods": ["rope_untieKnot", "rope_appendToLedger"],
            "dataFeeds": [],
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

    let mut resp = serde_json::json!({
        "address": if hidden { serde_json::Value::Null } else { serde_json::json!(&address) },
        "fatBalance": balance_str,
        "fatValueUsd": format!("{:.2}", balance_usd),
        "transactionCount": tx_count,
        "isContract": is_contract,
        "tokenHoldingsValueUsd": "0.00",
        "tokenCount": 0,
        "tokens": [],
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
async fn refresh_testimony_cache(state: &Arc<AppState>) {
    let pool = match &state.db_pool {
        Some(p) => p,
        None => return,
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

    let mut cache = state.testimony_cache.write().await;
    *cache = Some(TestimonyCache {
        stats,
        testimonies: testimonies.clone(),
        updated_at: chrono::Utc::now().timestamp(),
    });
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

async fn list_databoxes() -> Json<serde_json::Value> {
    let databoxes: Vec<serde_json::Value> = (0..20)
        .map(|i| {
            let locations = vec![
                ("Paris", 48.8566, 2.3522),
                ("New York", 40.7128, -74.0060),
                ("Tokyo", 35.6762, 139.6503),
                ("London", 51.5074, -0.1278),
                ("Singapore", 1.3521, 103.8198),
            ];
            let (city, lat, lng) = locations[i % 5];

            serde_json::json!({
                "id": format!("databox-{}", i + 1),
                "name": format!("Databox {}", i + 1),
                "location": {
                    "city": city,
                    "lat": lat + i as f64 * 0.1,
                    "lng": lng + i as f64 * 0.1
                },
                "status": "Online",
                "stringsStored": 124789 + i * 1000,
                "uptime": "99.9%"
            })
        })
        .collect();

    Json(serde_json::json!({
        "databoxes": databoxes,
        "totalCount": 284
    }))
}

async fn get_databox(Path(id): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "id": id,
        "name": "Databox 1",
        "location": {
            "city": "Paris",
            "country": "France",
            "lat": 48.8566,
            "lng": 2.3522
        },
        "status": "Online",
        "stringsStored": 124789,
        "uptime": "99.9%",
        "bandwidth": "10 Gbps",
        "storage": "100 TB"
    }))
}

async fn databox_map() -> Json<serde_json::Value> {
    let markers: Vec<serde_json::Value> = vec![
        serde_json::json!({"city": "Paris", "lat": 48.8566, "lng": 2.3522, "count": 12}),
        serde_json::json!({"city": "New York", "lat": 40.7128, "lng": -74.0060, "count": 24}),
        serde_json::json!({"city": "Tokyo", "lat": 35.6762, "lng": 139.6503, "count": 18}),
        serde_json::json!({"city": "London", "lat": 51.5074, "lng": -0.1278, "count": 15}),
        serde_json::json!({"city": "Singapore", "lat": 1.3521, "lng": 103.8198, "count": 21}),
        serde_json::json!({"city": "São Paulo", "lat": -23.5505, "lng": -46.6333, "count": 8}),
        serde_json::json!({"city": "Sydney", "lat": -33.8688, "lng": 151.2093, "count": 11}),
        serde_json::json!({"city": "Dubai", "lat": 25.2048, "lng": 55.2708, "count": 9}),
    ];

    Json(serde_json::json!({
        "markers": markers,
        "totalDataboxes": 284
    }))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search(Query(query): Query<SearchQuery>) -> Json<serde_json::Value> {
    let q = query.q.to_lowercase();

    let result_type = if q.starts_with("0x") && q.len() == 66 {
        "transaction"
    } else if q.starts_with("0x") && q.len() == 42 {
        "account"
    } else if q.parse::<u64>().is_ok() {
        "string"
    } else {
        "unknown"
    };

    Json(serde_json::json!({
        "query": query.q,
        "type": result_type,
        "results": [
            {
                "type": result_type,
                "value": query.q,
                "url": format!("/api/v1/{}/{}", result_type, query.q)
            }
        ]
    }))
}

async fn gas_price() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "slow": "0.0005 gwei",
        "standard": "0.001 gwei",
        "fast": "0.002 gwei",
        "instant": "0.005 gwei"
    }))
}

async fn gas_oracle() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "SafeGasPrice": "0.0005",
        "ProposeGasPrice": "0.001",
        "FastGasPrice": "0.002",
        "suggestBaseFee": "0.0003",
        "gasUsedRatio": "0.4,0.5,0.3,0.6,0.5"
    }))
}

// ============================================================================
// Federation & Community Generation API Handlers
// ============================================================================

/// List all federations
async fn list_federations(Query(params): Query<PaginationParams>) -> Json<serde_json::Value> {
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20);

    let federations: Vec<serde_json::Value> = vec![
        serde_json::json!({
            "id": "fed-001",
            "name": "European Smart Cities Federation",
            "description": "Federation for European smart city infrastructure and IoT management",
            "type": "structured",
            "structure": "multicellular",
            "scope": "regional",
            "industry": "public_institution",
            "status": "active",
            "dataWalletsGenerated": 1500000,
            "dataWalletsTotal": 10000000,
            "communitiesCount": 12,
            "protocols": ["datachain", "ethereum", "hyperledger"],
            "kycEnabled": true,
            "createdAt": chrono::Utc::now().timestamp() - 86400 * 180,
            "votesFor": 2847,
            "votesAgainst": 421
        }),
        serde_json::json!({
            "id": "fed-002",
            "name": "Global Banking Consortium",
            "description": "International banking federation for cross-border transactions",
            "type": "structured",
            "structure": "multicellular",
            "scope": "global",
            "industry": "banking",
            "status": "active",
            "dataWalletsGenerated": 5200000,
            "dataWalletsTotal": 10000000,
            "communitiesCount": 28,
            "protocols": ["datachain", "swift", "sepa"],
            "kycEnabled": true,
            "createdAt": chrono::Utc::now().timestamp() - 86400 * 365,
            "votesFor": 8924,
            "votesAgainst": 1247
        }),
        serde_json::json!({
            "id": "fed-003",
            "name": "Healthcare Data Exchange",
            "description": "Secure medical records and healthcare data federation",
            "type": "structured",
            "structure": "monocellular",
            "scope": "regional",
            "industry": "healthcare",
            "status": "voting",
            "dataWalletsGenerated": 0,
            "dataWalletsTotal": 10000000,
            "communitiesCount": 0,
            "protocols": ["datachain", "hyperledger"],
            "kycEnabled": true,
            "createdAt": chrono::Utc::now().timestamp() - 86400 * 14,
            "votesFor": 1892,
            "votesAgainst": 847
        }),
        serde_json::json!({
            "id": "fed-004",
            "name": "AI Research Network",
            "description": "Autonomous federation for AI/ML research and data sharing",
            "type": "autonomous",
            "structure": "multicellular",
            "scope": "global",
            "industry": "technology",
            "status": "active",
            "dataWalletsGenerated": 3100000,
            "dataWalletsTotal": 10000000,
            "communitiesCount": 45,
            "protocols": ["datachain", "ipfs", "tangle"],
            "kycEnabled": false,
            "createdAt": chrono::Utc::now().timestamp() - 86400 * 90,
            "votesFor": 5247,
            "votesAgainst": 892
        }),
    ];

    Json(serde_json::json!({
        "federations": federations,
        "pagination": {
            "page": page,
            "limit": limit,
            "total": 4
        }
    }))
}

/// Create new federation (requires DC FAT stake)
#[derive(Deserialize)]
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
    Json(payload): Json<CreateFederationRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    // In production, this would:
    // 1. Verify DC FAT stake
    // 2. Create federation in database
    // 3. Start voting period

    let federation_id = format!(
        "fed-{}",
        uuid::Uuid::new_v4()
            .to_string()
            .split('-')
            .next()
            .unwrap_or("000")
    );

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "success": true,
            "message": "Federation created and submitted for community vote",
            "federation": {
                "id": federation_id,
                "name": payload.name,
                "description": payload.description,
                "type": payload.federation_type,
                "structure": payload.structure,
                "scope": payload.scope,
                "industry": payload.industry,
                "protocols": payload.protocols,
                "kycEnabled": payload.kyc_enabled,
                "status": "pending_vote",
                "dataWalletsTotal": 10000000,
                "dataWalletsGenerated": 0,
                "votingEndsAt": chrono::Utc::now().timestamp() + 7 * 24 * 60 * 60,
                "createdAt": chrono::Utc::now().timestamp()
            }
        })),
    )
}

/// Get federation by ID
async fn get_federation(Path(id): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "id": id,
        "name": "European Smart Cities Federation",
        "description": "Federation for European smart city infrastructure and IoT management. This federation enables municipalities, suppliers, and AI systems to collaborate on ecosystemic autonomous maintenance.",
        "type": "structured",
        "structure": "multicellular",
        "scope": "regional",
        "industry": "public_institution",
        "status": "active",
        "creatorAddress": "0x7f3a8d2e4b1c9f0a",
        "instanceUrl": "https://smartcities.datachain.network",
        "genesisEntry": "0x1234567890abcdef",
        "dataWallets": {
            "total": 10000000,
            "generated": 1500000,
            "activated": 847293
        },
        "individualChains": {
            "total": 10000000,
            "generated": 892471
        },
        "protocols": {
            "native": ["datachain", "hyperledger"],
            "external": ["ethereum", "wanchain"]
        },
        "identity": {
            "kycAmlEnabled": true,
            "swiftIntegration": false,
            "sepaIntegration": true,
            "protocols": ["epassport", "iso_iec_24760_1"]
        },
        "predictability": {
            "enabled": true,
            "features": ["adaptability", "matching", "retracement", "contract_mining", "risk_management", "fraud_detection", "scoring"]
        },
        "cryptoCurrencies": ["dc", "bitcoin", "eth", "eos", "wan"],
        "consensusType": "PoA",
        "communities": 12,
        "voting": {
            "votesFor": 2847,
            "votesAgainst": 421,
            "requiredVotes": 1000,
            "approvalThreshold": 0.51
        },
        "createdAt": chrono::Utc::now().timestamp() - 86400 * 180,
        "activatedAt": chrono::Utc::now().timestamp() - 86400 * 170
    }))
}

/// Get communities in a federation
async fn federation_communities(Path(id): Path<String>) -> Json<serde_json::Value> {
    let communities: Vec<serde_json::Value> = (0..5)
        .map(|i| {
            serde_json::json!({
                "id": format!("comm-{}", i + 1),
                "federationId": id,
                "name": format!("Community {}", i + 1),
                "type": if i % 2 == 0 { "structured" } else { "autonomous" },
                "status": "active",
                "dataWalletsGenerated": 500000 + i * 100000,
                "members": 1000 + i * 200
            })
        })
        .collect();

    Json(serde_json::json!({
        "federationId": id,
        "communities": communities
    }))
}

/// Vote on federation
#[derive(Deserialize)]
struct VoteRequest {
    vote_for: bool,
    comment: Option<String>,
}

async fn vote_federation(
    Path(id): Path<String>,
    Json(payload): Json<VoteRequest>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "success": true,
        "message": format!("Vote {} on federation {}", if payload.vote_for { "for" } else { "against" }, id),
        "vote": {
            "targetType": "federation",
            "targetId": id,
            "voteFor": payload.vote_for,
            "comment": payload.comment,
            "timestamp": chrono::Utc::now().timestamp()
        }
    }))
}

/// List all communities
async fn list_communities(Query(params): Query<PaginationParams>) -> Json<serde_json::Value> {
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20);

    let communities: Vec<serde_json::Value> = vec![
        serde_json::json!({
            "id": "comm-001",
            "federationId": "fed-001",
            "name": "Paris Smart Infrastructure",
            "description": "Smart city infrastructure for Paris metropolitan area",
            "type": "structured",
            "scale": "large",
            "status": "active",
            "dataWalletsGenerated": 750000,
            "members": 2847,
            "createdAt": chrono::Utc::now().timestamp() - 86400 * 90
        }),
        serde_json::json!({
            "id": "comm-002",
            "federationId": "fed-002",
            "name": "Cross-Border Payments Network",
            "description": "Real-time cross-border payment processing community",
            "type": "structured",
            "scale": "enterprise",
            "status": "active",
            "dataWalletsGenerated": 2100000,
            "members": 8924,
            "createdAt": chrono::Utc::now().timestamp() - 86400 * 200
        }),
        serde_json::json!({
            "id": "comm-003",
            "federationId": "fed-004",
            "name": "ML Model Marketplace",
            "description": "Decentralized marketplace for ML models and datasets",
            "type": "autonomous",
            "scale": "medium",
            "status": "voting",
            "dataWalletsGenerated": 0,
            "members": 0,
            "votesFor": 892,
            "votesAgainst": 247,
            "createdAt": chrono::Utc::now().timestamp() - 86400 * 7
        }),
    ];

    Json(serde_json::json!({
        "communities": communities,
        "pagination": {
            "page": page,
            "limit": limit,
            "total": 3
        }
    }))
}

/// Create new community
#[derive(Deserialize)]
struct CreateCommunityRequest {
    name: String,
    description: String,
    federation_id: Option<String>,
    community_type: String,
    scale: String,
    protocols: Vec<String>,
}

async fn create_community(
    Json(payload): Json<CreateCommunityRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let community_id = format!(
        "comm-{}",
        uuid::Uuid::new_v4()
            .to_string()
            .split('-')
            .next()
            .unwrap_or("000")
    );

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "success": true,
            "message": "Community created and submitted for community vote",
            "community": {
                "id": community_id,
                "federationId": payload.federation_id,
                "name": payload.name,
                "description": payload.description,
                "type": payload.community_type,
                "scale": payload.scale,
                "protocols": payload.protocols,
                "status": "pending_vote",
                "dataWalletsTotal": 10000000,
                "votingEndsAt": chrono::Utc::now().timestamp() + 7 * 24 * 60 * 60,
                "createdAt": chrono::Utc::now().timestamp()
            }
        })),
    )
}

/// Get community by ID
async fn get_community(Path(id): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "id": id,
        "federationId": "fed-001",
        "name": "Paris Smart Infrastructure",
        "description": "Smart city infrastructure for Paris metropolitan area. Manages IoT sensors, traffic systems, and municipal maintenance.",
        "type": "structured",
        "scale": "large",
        "status": "active",
        "instanceUrl": "https://paris.smartcities.datachain.network",
        "genesisEntry": "0xabcdef1234567890",
        "dataWallets": {
            "total": 10000000,
            "generated": 750000,
            "activated": 521892
        },
        "protocols": {
            "native": ["datachain"],
            "external": ["ethereum"]
        },
        "kycAmlEnabled": true,
        "predictabilityEnabled": true,
        "members": 2847,
        "assets": 15892,
        "voting": {
            "votesFor": 1892,
            "votesAgainst": 247,
            "requiredVotes": 500,
            "approvalThreshold": 0.51
        },
        "createdAt": chrono::Utc::now().timestamp() - 86400 * 90,
        "activatedAt": chrono::Utc::now().timestamp() - 86400 * 83
    }))
}

/// Get community wallets
async fn community_wallets(Path(id): Path<String>) -> Json<serde_json::Value> {
    let wallets: Vec<serde_json::Value> = (0..10)
        .map(|i| {
            serde_json::json!({
                "id": format!("wallet-{}", i + 1),
                "communityId": id,
                "address": format!("0x{:040x}", i + 1000),
                "type": "standard",
                "isActivated": i < 7,
                "label": if i < 7 { Some(format!("User Wallet {}", i + 1)) } else { None::<String> },
                "createdAt": chrono::Utc::now().timestamp() - i as i64 * 3600
            })
        })
        .collect();

    Json(serde_json::json!({
        "communityId": id,
        "wallets": wallets,
        "stats": {
            "total": 10000000,
            "generated": 750000,
            "activated": 521892
        }
    }))
}

/// Generate wallets for community
#[derive(Deserialize)]
struct GenerateWalletsRequest {
    count: u64,
}

async fn generate_wallets(
    Path(id): Path<String>,
    Json(payload): Json<GenerateWalletsRequest>,
) -> Json<serde_json::Value> {
    let wallets: Vec<serde_json::Value> = (0..payload.count.min(100))
        .map(|i| {
            serde_json::json!({
                "id": format!("wallet-new-{}", i + 1),
                "communityId": id,
                "address": format!("0x{:040x}", chrono::Utc::now().timestamp() as u64 + i),
                "type": "standard",
                "isActivated": false,
                "createdAt": chrono::Utc::now().timestamp()
            })
        })
        .collect();

    Json(serde_json::json!({
        "success": true,
        "message": format!("Generated {} wallets for community {}", wallets.len(), id),
        "wallets": wallets
    }))
}

/// Vote on community
async fn vote_community(
    Path(id): Path<String>,
    Json(payload): Json<VoteRequest>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "success": true,
        "message": format!("Vote {} on community {}", if payload.vote_for { "for" } else { "against" }, id),
        "vote": {
            "targetType": "community",
            "targetId": id,
            "voteFor": payload.vote_for,
            "comment": payload.comment,
            "timestamp": chrono::Utc::now().timestamp()
        }
    }))
}

// ============================================================================
// Project Submission API Handlers (Start Building)
// ============================================================================

/// List all project submissions
async fn list_projects(Query(params): Query<PaginationParams>) -> Json<serde_json::Value> {
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20);

    let projects: Vec<serde_json::Value> = vec![
        serde_json::json!({
            "id": "proj-001",
            "name": "DCSwap",
            "tagline": "Decentralized exchange for DCR-20 tokens",
            "category": "defi",
            "stage": "mvp",
            "organizationType": "business",
            "status": "approved",
            "votesFor": 2847,
            "votesAgainst": 421,
            "fundingRequested": 50000,
            "fundingCurrency": "FAT",
            "createdAt": chrono::Utc::now().timestamp() - 86400 * 60
        }),
        serde_json::json!({
            "id": "proj-002",
            "name": "DataMarket",
            "tagline": "P2P marketplace for AI training datasets",
            "category": "marketplace",
            "stage": "prototype",
            "organizationType": "institution",
            "status": "voting",
            "votesFor": 1247,
            "votesAgainst": 892,
            "fundingRequested": 100000,
            "fundingCurrency": "FAT",
            "votingEndsAt": chrono::Utc::now().timestamp() + 3 * 24 * 60 * 60,
            "createdAt": chrono::Utc::now().timestamp() - 86400 * 7
        }),
        serde_json::json!({
            "id": "proj-003",
            "name": "IdentityVault",
            "tagline": "Self-sovereign identity management",
            "category": "identity",
            "stage": "idea",
            "organizationType": "individual",
            "status": "pending_review",
            "votesFor": 0,
            "votesAgainst": 0,
            "fundingRequested": 25000,
            "fundingCurrency": "FAT",
            "createdAt": chrono::Utc::now().timestamp() - 86400 * 2
        }),
        serde_json::json!({
            "id": "proj-004",
            "name": "ChainBridge",
            "tagline": "Cross-chain asset bridge for DC ecosystem",
            "category": "bridge",
            "stage": "beta",
            "organizationType": "business",
            "status": "building",
            "votesFor": 5892,
            "votesAgainst": 847,
            "fundingRequested": 200000,
            "fundingCurrency": "FAT",
            "createdAt": chrono::Utc::now().timestamp() - 86400 * 120
        }),
    ];

    Json(serde_json::json!({
        "projects": projects,
        "pagination": {
            "page": page,
            "limit": limit,
            "total": 4
        }
    }))
}

/// Submit new project (Start Building)
#[derive(Deserialize)]
struct SubmitProjectRequest {
    name: String,
    tagline: Option<String>,
    description: String,
    category: String,
    stage: String,
    organization_type: String,
    organization_name: Option<String>,
    submitter_name: Option<String>,
    submitter_email: Option<String>,
    tech_stack: Vec<String>,
    architecture_description: Option<String>,
    features: Vec<serde_json::Value>,
    use_cases: Option<String>,
    target_users: Option<String>,
    requires_ai_testimony: bool,
    whitepaper_url: Option<String>,
    documentation_url: Option<String>,
    github_url: Option<String>,
    website_url: Option<String>,
    demo_url: Option<String>,
    team_members: Vec<serde_json::Value>,
    milestones: Vec<serde_json::Value>,
    funding_requested: u64,
    funding_currency: String,
    funding_breakdown: Option<String>,
}

async fn submit_project(
    Json(payload): Json<SubmitProjectRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let project_id = format!(
        "proj-{}",
        uuid::Uuid::new_v4()
            .to_string()
            .split('-')
            .next()
            .unwrap_or("000")
    );

    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "success": true,
            "message": "Project submitted successfully and pending review",
            "project": {
                "id": project_id,
                "name": payload.name,
                "tagline": payload.tagline,
                "description": payload.description,
                "category": payload.category,
                "stage": payload.stage,
                "organizationType": payload.organization_type,
                "organizationName": payload.organization_name,
                "submitterName": payload.submitter_name,
                "submitterEmail": payload.submitter_email,
                "techStack": payload.tech_stack,
                "features": payload.features,
                "requiresAiTestimony": payload.requires_ai_testimony,
                "teamMembers": payload.team_members,
                "milestones": payload.milestones,
                "fundingRequested": payload.funding_requested,
                "fundingCurrency": payload.funding_currency,
                "status": "pending_review",
                "createdAt": chrono::Utc::now().timestamp()
            },
            "nextSteps": [
                "Your project will be reviewed by the Datachain Foundation",
                "Once approved, it will enter a 7-day community voting period",
                "DC FAT holders will vote to approve or reject your project",
                "If approved with 51%+ votes, your project can start building on Datachain Rope"
            ]
        })),
    )
}

/// Get project by ID
async fn get_project(Path(id): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "id": id,
        "name": "DCSwap",
        "tagline": "Decentralized exchange for DCR-20 tokens",
        "description": "DCSwap is a fully decentralized exchange protocol built on Datachain Rope. It enables trustless token swaps with AI-validated transactions and ultra-low fees.",
        "category": "defi",
        "stage": "mvp",
        "organizationType": "business",
        "organizationName": "DCSwap Labs",
        "submitterName": "Alex Chen",
        "submitterEmail": "alex@dcswap.io",
        "status": "approved",
        "techStack": ["Rust", "TypeScript", "React", "Solidity"],
        "architectureDescription": "Smart contract-based AMM with AI testimony verification",
        "features": [
            {"name": "Token Swaps", "description": "Instant DCR-20 token swaps", "priority": "high"},
            {"name": "Liquidity Pools", "description": "Provide liquidity and earn fees", "priority": "high"},
            {"name": "AI Verification", "description": "AI agents validate large trades", "priority": "medium"}
        ],
        "useCases": "Token trading, liquidity provision, price discovery",
        "targetUsers": "DeFi traders, liquidity providers, projects launching tokens",
        "requiresAiTestimony": true,
        "aiAgentRequirements": "Validation of trades > 10,000 FAT",
        "whitepaperUrl": "https://dcswap.io/whitepaper.pdf",
        "documentationUrl": "https://docs.dcswap.io",
        "githubUrl": "https://github.com/dcswap/dcswap-core",
        "websiteUrl": "https://dcswap.io",
        "demoUrl": "https://demo.dcswap.io",
        "teamMembers": [
            {"name": "Alex Chen", "role": "CEO", "linkedinUrl": "https://linkedin.com/in/alexchen"},
            {"name": "Sarah Kim", "role": "CTO", "githubUrl": "https://github.com/sarahkim"}
        ],
        "milestones": [
            {"title": "Smart Contract Development", "description": "Core AMM contracts", "targetDate": "2026-02-01", "isCompleted": true},
            {"title": "Frontend Launch", "description": "Trading interface", "targetDate": "2026-03-01", "isCompleted": true},
            {"title": "Mainnet Launch", "description": "Full production launch", "targetDate": "2026-04-01", "isCompleted": false}
        ],
        "fundingRequested": 50000,
        "fundingCurrency": "FAT",
        "fundingBreakdown": "Development: 30,000 FAT\nAudit: 10,000 FAT\nMarketing: 5,000 FAT\nOperations: 5,000 FAT",
        "voting": {
            "votesFor": 2847,
            "votesAgainst": 421,
            "requiredVotes": 100,
            "approvalThreshold": 0.51,
            "votingStartedAt": chrono::Utc::now().timestamp() - 86400 * 53,
            "votingEndedAt": chrono::Utc::now().timestamp() - 86400 * 46
        },
        "createdAt": chrono::Utc::now().timestamp() - 86400 * 60,
        "approvedAt": chrono::Utc::now().timestamp() - 86400 * 46
    }))
}

/// Vote on project
async fn vote_project(
    Path(id): Path<String>,
    Json(payload): Json<VoteRequest>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "success": true,
        "message": format!("Vote {} on project {}", if payload.vote_for { "for" } else { "against" }, id),
        "vote": {
            "targetType": "project",
            "targetId": id,
            "voteFor": payload.vote_for,
            "comment": payload.comment,
            "timestamp": chrono::Utc::now().timestamp()
        }
    }))
}

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

/// Get projects currently in voting
async fn voting_projects() -> Json<serde_json::Value> {
    let projects: Vec<serde_json::Value> = vec![serde_json::json!({
        "id": "proj-002",
        "name": "DataMarket",
        "tagline": "P2P marketplace for AI training datasets",
        "category": "marketplace",
        "stage": "prototype",
        "status": "voting",
        "votesFor": 1247,
        "votesAgainst": 892,
        "requiredVotes": 100,
        "approvalThreshold": 0.51,
        "votingEndsAt": chrono::Utc::now().timestamp() + 3 * 24 * 60 * 60,
        "timeRemaining": "3 days"
    })];

    Json(serde_json::json!({
        "votingProjects": projects,
        "total": 1
    }))
}

/// List all votes
async fn list_votes(Query(params): Query<PaginationParams>) -> Json<serde_json::Value> {
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20);

    let votes: Vec<serde_json::Value> = (0..10)
        .map(|i| {
            serde_json::json!({
                "id": format!("vote-{}", i + 1),
                "voterAddress": format!("0x{:040x}", i + 1000),
                "targetType": if i % 3 == 0 { "federation" } else if i % 3 == 1 { "community" } else { "project" },
                "targetId": format!("{}-00{}", if i % 3 == 0 { "fed" } else if i % 3 == 1 { "comm" } else { "proj" }, i / 3 + 1),
                "voteFor": i % 4 != 0,
                "voteWeight": 100 + i * 50,
                "timestamp": chrono::Utc::now().timestamp() - i as i64 * 3600
            })
        })
        .collect();

    Json(serde_json::json!({
        "votes": votes,
        "pagination": {
            "page": page,
            "limit": limit,
            "total": 10
        }
    }))
}

/// Get votes for specific target
async fn get_votes_for_target(
    Path((target_type, target_id)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let votes: Vec<serde_json::Value> = (0..5)
        .map(|i| {
            serde_json::json!({
                "id": format!("vote-{}-{}", target_id, i + 1),
                "voterAddress": format!("0x{:040x}", i + 1000),
                "voteFor": i % 3 != 0,
                "voteWeight": 100 + i * 50,
                "comment": if i % 2 == 0 { Some("Great project!") } else { None::<&str> },
                "timestamp": chrono::Utc::now().timestamp() - i as i64 * 3600
            })
        })
        .collect();

    Json(serde_json::json!({
        "targetType": target_type,
        "targetId": target_id,
        "votes": votes,
        "summary": {
            "totalVotes": 5,
            "votesFor": 3,
            "votesAgainst": 2,
            "totalWeight": 750,
            "weightFor": 450,
            "weightAgainst": 300
        }
    }))
}

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

const TANASTOK_API: &str = "https://tanastok.io/api/v1/tokenized-assets";

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
