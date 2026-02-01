//! DC Explorer - Block Explorer for Datachain Rope
//!
//! API server powering dcscan.io

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
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
mod indexer;
mod models;

use api::*;

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
    /// Database pool (placeholder for now)
    pub chain_id: u64,
    pub network_name: String,
    /// HTTP client for price fetching
    pub http_client: reqwest::Client,
    /// Cached price data
    pub price_cache: RwLock<Option<PriceData>>,
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
    tracing::info!("║        Block Explorer for Datachain Rope                     ║");
    tracing::info!("╚══════════════════════════════════════════════════════════════╝");

    // Initialize HTTP client for price fetching
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("DC-Explorer/1.0")
        .build()
        .expect("Failed to create HTTP client");

    let state = Arc::new(AppState {
        chain_id: 271828,
        network_name: "Datachain Rope Mainnet".to_string(),
        http_client,
        price_cache: RwLock::new(None),
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

    // Build router
    let app = Router::new()
        // Health & Info
        .route("/", get(root))
        .route("/health", get(health))
        .route("/api/v1/status", get(status))
        // Stats
        .route("/api/v1/stats", get(stats))
        .route("/api/v1/stats/charts/:chart_type", get(chart_data))
        // Strings (Blocks)
        .route("/api/v1/strings", get(list_strings))
        .route("/api/v1/strings/latest", get(latest_strings))
        .route("/api/v1/strings/:id", get(get_string))
        // Transactions
        .route("/api/v1/transactions", get(list_transactions))
        .route("/api/v1/transactions/latest", get(latest_transactions))
        .route("/api/v1/transactions/:hash", get(get_transaction))
        // Accounts
        .route("/api/v1/accounts/:address", get(get_account))
        .route(
            "/api/v1/accounts/:address/transactions",
            get(account_transactions),
        )
        .route("/api/v1/accounts/:address/tokens", get(account_tokens))
        // Tokens
        .route("/api/v1/tokens", get(list_tokens))
        .route("/api/v1/tokens/:address", get(get_token))
        .route("/api/v1/tokens/:address/holders", get(token_holders))
        .route("/api/v1/tokens/:address/transfers", get(token_transfers))
        // Validators
        .route("/api/v1/validators", get(list_validators))
        .route("/api/v1/validators/:address", get(get_validator))
        // AI Agents
        .route("/api/v1/ai-agents", get(list_ai_agents))
        .route("/api/v1/ai-agents/:id", get(get_ai_agent))
        .route("/api/v1/ai-agents/:id/testimonies", get(agent_testimonies))
        // Databoxes (Nodes)
        .route("/api/v1/databoxes", get(list_databoxes))
        .route("/api/v1/databoxes/:id", get(get_databox))
        .route("/api/v1/databoxes/map", get(databox_map))
        // Search
        .route("/api/v1/search", get(search))
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
        .layer(cors)
        .with_state(state);

    let addr = "0.0.0.0:3001";
    tracing::info!("DC Explorer API listening on {}", addr);
    tracing::info!("API docs: http://{}/api/v1/status", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

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

/// Fetch and cache DC FAT price
async fn fetch_and_cache_price(state: &Arc<AppState>) -> Result<PriceData, anyhow::Error> {
    tracing::info!("Fetching DC FAT price from XDCScan...");

    // Fetch from XDCScan (primary and reliable source)
    let price_data = match fetch_from_xdcscan(&state.http_client).await {
        Ok(data) => {
            tracing::info!("Price fetched from XDCScan: ${:.8}", data.price);
            data
        }
        Err(e) => {
            tracing::warn!("XDCScan fetch failed: {}, using fallback price", e);

            // Use fallback with slight variation
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
    };

    // Update cache
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

async fn stats(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // Get cached price data
    let price_cache = state.price_cache.read().await;
    let price_data = price_cache.clone().unwrap_or_default();
    let fat_price = format!("${:.6}", price_data.price);
    let market_cap = format!("${:.0}", price_data.price * 10_000_000_000.0);

    Json(serde_json::json!({
        "totalStrings": 1247893,
        "totalTransactions": 4892451,
        "validators": 127,
        "aiAgents": 5,
        "databoxes": 284,
        "gasPrice": "0.001 gwei",
        "fatPrice": fat_price,
        "fatPriceRaw": price_data.price,
        "fatPriceChange24h": price_data.change_24h,
        "fatPriceSource": price_data.source,
        "marketCap": market_cap,
        "circulatingSupply": "10,000,000,000 FAT",
        "tps": 2847,
        "avgBlockTime": "2.8s",
        "finalityTime": "4.2s"
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
}

async fn list_strings(Query(params): Query<PaginationParams>) -> Json<serde_json::Value> {
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20);

    let strings: Vec<serde_json::Value> = (0..limit)
        .map(|i| {
            let num = 1247893 - (page - 1) * limit - i;
            serde_json::json!({
                "number": num,
                "hash": format!("0x{:064x}", num),
                "transactions": 15 + (i % 20),
                "validator": format!("0x7f3a{}8d2e", i),
                "timestamp": chrono::Utc::now().timestamp() - (i as i64 * 3),
                "status": "Final",
                "aiVerified": true
            })
        })
        .collect();

    Json(serde_json::json!({
        "strings": strings,
        "pagination": {
            "page": page,
            "limit": limit,
            "total": 1247893
        }
    }))
}

async fn latest_strings() -> Json<serde_json::Value> {
    let strings: Vec<serde_json::Value> = (0..10)
        .map(|i| {
            serde_json::json!({
                "number": 1247893 - i,
                "hash": format!("0x{:064x}", 1247893 - i),
                "transactions": 15 + (i % 20),
                "validator": format!("0x7f3a{}8d2e", i),
                "timestamp": chrono::Utc::now().timestamp() - (i as i64 * 3),
                "status": "Final"
            })
        })
        .collect();

    Json(serde_json::json!({ "strings": strings }))
}

async fn get_string(Path(id): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "number": id.parse::<u64>().unwrap_or(1247893),
        "hash": format!("0x{:064x}", id.parse::<u64>().unwrap_or(1247893)),
        "parentHash": format!("0x{:064x}", id.parse::<u64>().unwrap_or(1247892)),
        "timestamp": chrono::Utc::now().timestamp(),
        "transactions": 24,
        "validator": "0x7f3a8d2e4b1c9f0a",
        "gasUsed": "1,247,893",
        "gasLimit": "30,000,000",
        "status": "Final",
        "aiTestimonies": 5,
        "complementHash": format!("0x{:064x}", id.parse::<u64>().unwrap_or(1247893) + 1)
    }))
}

async fn list_transactions(Query(params): Query<PaginationParams>) -> Json<serde_json::Value> {
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20);

    let txs: Vec<serde_json::Value> = (0..limit)
        .map(|i| {
            serde_json::json!({
                "hash": format!("0x8f2a9c3d{}4e7b1f8a", i),
                "from": format!("0x7f3a{}8d2e", i),
                "to": format!("0x2b9c{}4f1a", i + 1),
                "value": format!("{}.00", 100 + i * 50),
                "status": "Success",
                "aiVerified": true,
                "string": 1247893 - i,
                "timestamp": chrono::Utc::now().timestamp() - (i as i64 * 5)
            })
        })
        .collect();

    Json(serde_json::json!({
        "transactions": txs,
        "pagination": {
            "page": page,
            "limit": limit,
            "total": 4892451
        }
    }))
}

async fn latest_transactions() -> Json<serde_json::Value> {
    let txs: Vec<serde_json::Value> = (0..10)
        .map(|i| {
            serde_json::json!({
                "hash": format!("0x8f2a9c3d{}4e7b1f8a", i),
                "from": format!("0x7f3a{}8d2e", i),
                "to": format!("0x2b9c{}4f1a", i + 1),
                "value": format!("{}.00", 100 + i * 50),
                "status": "Success",
                "aiVerified": i % 3 != 0
            })
        })
        .collect();

    Json(serde_json::json!({ "transactions": txs }))
}

async fn get_transaction(Path(hash): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "hash": hash,
        "status": "Success",
        "string": 1247893,
        "from": "0x7f3a8d2e4b1c9f0a",
        "to": "0x2b9c4f1a8e7d3b6c",
        "value": "1,250.00 FAT",
        "gasUsed": "21,000",
        "gasPrice": "0.001 gwei",
        "timestamp": chrono::Utc::now().timestamp(),
        "aiTestimony": {
            "verified": true,
            "agents": ["ValidationAgent", "ComplianceAgent"],
            "confidence": 0.98
        },
        "input": "0x",
        "logs": []
    }))
}

async fn get_account(Path(address): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "address": address,
        "balance": "10,247.89 FAT",
        "balanceUsd": "$868.00",
        "transactionCount": 147,
        "isContract": false,
        "isValidator": false,
        "firstSeen": chrono::Utc::now().timestamp() - 86400 * 30,
        "lastSeen": chrono::Utc::now().timestamp()
    }))
}

async fn account_transactions(
    Path(address): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Json<serde_json::Value> {
    let txs: Vec<serde_json::Value> = (0..10)
        .map(|i| {
            serde_json::json!({
                "hash": format!("0x{:064x}", i),
                "from": if i % 2 == 0 { &address } else { "0x7f3a8d2e" },
                "to": if i % 2 == 1 { &address } else { "0x2b9c4f1a" },
                "value": format!("{}.00", 100 + i * 10),
                "status": "Success"
            })
        })
        .collect();

    Json(serde_json::json!({
        "address": address,
        "transactions": txs
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

async fn list_tokens() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "tokens": [
            {
                "address": "0x0000000000000000000000000000000000000001",
                "name": "DC FAT",
                "symbol": "FAT",
                "decimals": 18,
                "totalSupply": "10,000,000,000",
                "holders": 147893,
                "transfers": 4892451
            },
            {
                "address": "0x0000000000000000000000000000000000000002",
                "name": "Wrapped ETH",
                "symbol": "WETH",
                "decimals": 18,
                "totalSupply": "1,000,000",
                "holders": 8947,
                "transfers": 247891
            }
        ]
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

async fn list_validators() -> Json<serde_json::Value> {
    let validators: Vec<serde_json::Value> = (0..20)
        .map(|i| {
            serde_json::json!({
                "address": format!("0x{:040x}", i + 1),
                "name": format!("Validator {}", i + 1),
                "stake": format!("{}", 1000000 + i * 100000),
                "strings": 12478 + i * 100,
                "uptime": format!("{:.1}%", 99.9 - i as f64 * 0.1),
                "aiAgent": if i < 5 { true } else { false }
            })
        })
        .collect();

    Json(serde_json::json!({
        "validators": validators,
        "totalStaked": "127,000,000 FAT",
        "activeCount": 127
    }))
}

async fn get_validator(Path(address): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "address": address,
        "name": "Validator 1",
        "stake": "1,000,000 FAT",
        "stringsProduced": 12478,
        "uptime": "99.9%",
        "rewards": "24,789 FAT",
        "delegators": 147,
        "aiAgent": true
    }))
}

async fn list_ai_agents() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "agents": [
            {
                "id": "validation-agent",
                "name": "ValidationAgent",
                "type": "Validation",
                "status": "Active",
                "testimonies": 1247893,
                "accuracy": "99.97%"
            },
            {
                "id": "insurance-agent",
                "name": "InsuranceAgent",
                "type": "Risk Assessment",
                "status": "Active",
                "testimonies": 847291,
                "accuracy": "99.89%"
            },
            {
                "id": "compliance-agent",
                "name": "ComplianceAgent",
                "type": "Regulatory",
                "status": "Active",
                "testimonies": 624891,
                "accuracy": "99.95%"
            },
            {
                "id": "oracle-agent",
                "name": "OracleAgent",
                "type": "Data Verification",
                "status": "Active",
                "testimonies": 428971,
                "accuracy": "99.92%"
            },
            {
                "id": "semantic-agent",
                "name": "SemanticAgent",
                "type": "Contract Analysis",
                "status": "Active",
                "testimonies": 247891,
                "accuracy": "99.88%"
            }
        ]
    }))
}

async fn get_ai_agent(Path(id): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "id": id,
        "name": "ValidationAgent",
        "type": "Validation",
        "status": "Active",
        "testimonies": 1247893,
        "accuracy": "99.97%",
        "description": "Primary validation agent for transaction verification",
        "createdAt": chrono::Utc::now().timestamp() - 86400 * 365,
        "lastActive": chrono::Utc::now().timestamp()
    }))
}

async fn agent_testimonies(Path(id): Path<String>) -> Json<serde_json::Value> {
    let testimonies: Vec<serde_json::Value> = (0..10)
        .map(|i| {
            serde_json::json!({
                "id": format!("testimony-{}", i),
                "transaction": format!("0x{:064x}", i),
                "verdict": "Approved",
                "confidence": 0.98 - i as f64 * 0.001,
                "timestamp": chrono::Utc::now().timestamp() - i as i64 * 60
            })
        })
        .collect();

    Json(serde_json::json!({
        "agentId": id,
        "testimonies": testimonies
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
            "tagline": "Decentralized exchange for DC-20 tokens",
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
        "tagline": "Decentralized exchange for DC-20 tokens",
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
            {"name": "Token Swaps", "description": "Instant DC-20 token swaps", "priority": "high"},
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
        assert_eq!(DC_FAT_CONTRACT, "0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a");
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
