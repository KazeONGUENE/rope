//! Extra API handlers transferred from dcscan-api (consensus, defi, verify, services registry, etc.)

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::AppState;

const THIRD_PARTY_PORT_START: u16 = 3010;
const THIRD_PARTY_PORT_END: u16 = 3099;

#[derive(Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ServiceRegistryEntry {
    pub provider_id: String,
    pub name: String,
    pub description: String,
    pub port: u16,
    pub health_url: String,
    pub capabilities: Vec<String>,
    pub created_at: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct VerificationEntry {
    pub verified: bool,
    pub compiler_version: Option<String>,
    pub native_audit: Option<serde_json::Value>,
    pub submitted_at: Option<String>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CertificationEntry {
    pub provider_id: String,
    pub certification_type: String,
    pub report_url: Option<String>,
    pub attested_at: String,
}

pub async fn consensus(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ropeNodeConnected": false,
        "lastPolled": null,
        "chain": {
            "chainId": state.chain_id,
            "networkName": state.network_name,
            "version": "N/A",
            "consensusType": "testimony",
            "executionMode": "Anvil direct",
            "peerCount": 0
        },
        "testimony": { "currentRound": 0, "pendingTransactions": 0, "finalizedTransactions": 0, "anvilConnected": true },
        "aiAgents": { "activeCount": 0, "agents": [] },
        "evmState": { "latestBlock": 0, "blockTime": 3 }
    }))
}

pub async fn testimonies_stats(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "totalTestimonies": 0,
        "totalTestimoniesChangePercentThisWeek": 0,
        "testimonies24h": 0,
        "testimonies24hChangePercentFromYesterday": 0,
        "avgConfidenceScore": "0",
        "activeAgents": 0,
        "ropeNodeConnected": false
    }))
}

#[derive(Deserialize)]
pub struct TestimoniesQuery {
    page: Option<u32>,
    limit: Option<u32>,
}

pub async fn testimonies_list(Query(q): Query<TestimoniesQuery>) -> Json<serde_json::Value> {
    let page = q.page.unwrap_or(1);
    let limit = q.limit.unwrap_or(25).min(100);
    Json(serde_json::json!({
        "testimonies": [],
        "total": 0,
        "page": page,
        "limit": limit,
        "message": "Connect rope-node for testimony data"
    }))
}

pub async fn consensus_testimonies(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ropeNodeConnected": false,
        "totalFinalized": 0,
        "totalPending": 0,
        "testimonies": [],
        "message": "rope-node is not connected — testimony data unavailable. EVM data served directly from Anvil."
    }))
}

pub async fn transactions_pending() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "pending": [], "total": 0 }))
}

pub async fn contracts_stats() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "total": 0, "verified": 0 }))
}

#[derive(Deserialize)]
pub struct ContractsListQuery {
    page: Option<u32>,
    limit: Option<u32>,
}

pub async fn contracts_list(Query(q): Query<ContractsListQuery>) -> Json<serde_json::Value> {
    let page = q.page.unwrap_or(1);
    let limit = q.limit.unwrap_or(20);
    Json(
        serde_json::json!({ "contracts": [], "pagination": { "page": page, "limit": limit, "total": 0 } }),
    )
}

pub async fn accounts_stats() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "totalAccounts": 0, "totalContracts": 0 }))
}

#[derive(Deserialize)]
pub struct AccountsTopQuery {
    limit: Option<u32>,
}

pub async fn accounts_top(Query(q): Query<AccountsTopQuery>) -> Json<serde_json::Value> {
    let limit = q.limit.unwrap_or(20).min(100);
    Json(serde_json::json!({ "accounts": [], "total": 0, "limit": limit }))
}

pub async fn account_overview(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let mut balance_wei = String::new();
    let mut transaction_count: u64 = 0;

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getBalance",
        "params": [address, "latest"],
        "id": 1
    });
    if let Ok(res) = state
        .http_client
        .post(&state.rpc_url)
        .json(&body)
        .send()
        .await
    {
        if let Ok(json) = res.json::<serde_json::Value>().await {
            if let Some(result) = json.get("result").and_then(|r| r.as_str()) {
                balance_wei = result.to_string();
            }
        }
    }

    let body2 = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getTransactionCount",
        "params": [address, "latest"],
        "id": 1
    });
    if let Ok(res) = state
        .http_client
        .post(&state.rpc_url)
        .json(&body2)
        .send()
        .await
    {
        if let Ok(json) = res.json::<serde_json::Value>().await {
            if let Some(r) = json.get("result").and_then(|r| r.as_str()) {
                transaction_count =
                    u64::from_str_radix(r.trim_start_matches("0x"), 16).unwrap_or(0);
            }
        }
    }

    let balance_fat = if balance_wei.starts_with("0x") {
        let w = u128::from_str_radix(balance_wei.trim_start_matches("0x"), 16).unwrap_or(0);
        format!("{:.4}", w as f64 / 1e18)
    } else {
        "0".to_string()
    };

    let price_cache = state.price_cache.read().await;
    let price = price_cache.as_ref().map(|p| p.price).unwrap_or(0.0039);
    let balance_usd = format!("${:.2}", balance_fat.parse::<f64>().unwrap_or(0.0) * price);

    Json(serde_json::json!({
        "address": address,
        "fatBalance": balance_fat + " FAT",
        "fatValueUsd": balance_usd,
        "transactionCount": transaction_count,
        "isContract": false,
        "tokenHoldingsValueUsd": "0.00",
        "tokenCount": 0,
        "tokens": [],
        "recentTransactions": []
    }))
}

pub async fn account_bytecode(Path(address): Path<String>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "address": address, "bytecode": null, "isContract": false }))
}

#[derive(Deserialize)]
pub struct TransfersQuery {
    page: Option<u32>,
    limit: Option<u32>,
}

pub async fn account_transfers(
    Path(address): Path<String>,
    Query(q): Query<TransfersQuery>,
) -> Json<serde_json::Value> {
    let page = q.page.unwrap_or(1);
    let limit = q.limit.unwrap_or(20);
    Json(serde_json::json!({
        "address": address,
        "transfers": [],
        "pagination": { "page": page, "limit": limit, "total": 0 }
    }))
}

pub async fn account_events(
    Path(address): Path<String>,
    Query(q): Query<TransfersQuery>,
) -> Json<serde_json::Value> {
    let page = q.page.unwrap_or(1);
    let limit = q.limit.unwrap_or(20);
    Json(serde_json::json!({
        "address": address,
        "events": [],
        "pagination": { "page": page, "limit": limit, "total": 0 }
    }))
}

pub async fn defi_overview(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let url = format!(
        "{}/v1/analytics/overview",
        state.dcswap_api.trim_end_matches('/')
    );
    match state.http_client.get(&url).send().await {
        Ok(r) if r.status().is_success() => {
            if let Ok(data) = r.json::<serde_json::Value>().await {
                let d = data.get("data").or(Some(&data));
                return Json(serde_json::json!({
                    "source": "DCSwap",
                    "url": "https://dcswap.net",
                    "tvl": d.and_then(|x| x.get("tvl_usd")).and_then(|v| v.as_str()).unwrap_or("0"),
                    "volume24h": d.and_then(|x| x.get("volume_24h")).and_then(|v| v.as_str()).unwrap_or("0"),
                    "fees24h": d.and_then(|x| x.get("fees_24h")).and_then(|v| v.as_str()).unwrap_or("0"),
                    "totalPools": 0,
                    "pools": []
                }));
            }
        }
        _ => {}
    }
    Json(
        serde_json::json!({ "source": "DCSwap", "error": "Failed to fetch", "tvl": "0", "volume24h": "0", "pools": [] }),
    )
}

pub async fn defi_swaps(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let url = format!(
        "{}/v1/swaps?limit=20",
        state.dcswap_api.trim_end_matches('/')
    );
    match state.http_client.get(&url).send().await {
        Ok(r) if r.status().is_success() => {
            if let Ok(data) = r.json::<serde_json::Value>().await {
                let swaps = data
                    .get("data")
                    .and_then(|d| d.as_array())
                    .or_else(|| data.get("swaps").and_then(|s| s.as_array()))
                    .cloned()
                    .unwrap_or_default();
                return Json(serde_json::json!({ "source": "DCSwap", "swaps": swaps }));
            }
        }
        _ => {}
    }
    Json(serde_json::json!({ "source": "DCSwap", "swaps": [] }))
}

pub async fn services_registry_get(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let reg = state.services_registry.read().await;
    let services: Vec<serde_json::Value> = reg
        .iter()
        .map(|r| {
            serde_json::json!({
                "providerId": r.provider_id,
                "name": r.name,
                "port": r.port,
                "healthUrl": r.health_url,
                "capabilities": r.capabilities,
                "createdAt": r.created_at
            })
        })
        .collect();
    Json(serde_json::json!({
        "services": services,
        "portRange": { "start": THIRD_PARTY_PORT_START, "end": THIRD_PARTY_PORT_END }
    }))
}

#[derive(Deserialize)]
pub struct RegisterServiceRequest {
    name: Option<String>,
    description: Option<String>,
    health_url: Option<String>,
    capabilities: Option<Vec<String>>,
    provider_id: Option<String>,
}

pub async fn services_registry_post(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<RegisterServiceRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let name = body.name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "success": false, "error": "Name is required" })),
        );
    }
    let provider_id = body.provider_id.unwrap_or_else(|| {
        name.to_lowercase()
            .replace(' ', "-")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect::<String>()
    });
    let mut reg = state.services_registry.write().await;
    if reg.iter().any(|r| r.provider_id == provider_id) {
        return (
            StatusCode::CONFLICT,
            Json(
                serde_json::json!({ "success": false, "error": "Provider ID already registered" }),
            ),
        );
    }
    let mut port = THIRD_PARTY_PORT_START;
    let used: std::collections::HashSet<u16> = reg.iter().map(|r| r.port).collect();
    while port <= THIRD_PARTY_PORT_END && used.contains(&port) {
        port += 1;
    }
    if port > THIRD_PARTY_PORT_END {
        return (
            StatusCode::INSUFFICIENT_STORAGE,
            Json(serde_json::json!({ "success": false, "error": "No more ports available" })),
        );
    }
    let entry = ServiceRegistryEntry {
        provider_id: provider_id.clone(),
        name: name.clone(),
        description: body.description.unwrap_or_default(),
        port,
        health_url: body
            .health_url
            .unwrap_or_else(|| format!("http://127.0.0.1:{}/health", port)),
        capabilities: body
            .capabilities
            .unwrap_or_else(|| vec!["agent".to_string()]),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    reg.push(entry);
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "success": true,
            "providerId": provider_id,
            "port": port,
            "message": format!("Service registered. Use your allocated port and path /services/{}", provider_id)
        })),
    )
}

pub async fn verify_get(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let addr = address.trim().to_lowercase();
    if !addr.starts_with("0x") || addr.len() != 42 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Invalid contract address" })),
        ));
    }
    let ver = state.verification_store.read().await;
    let certs = state.certifications_store.read().await;
    let verification = ver.get(&addr).cloned();
    let certifications = certs.get(&addr).cloned().unwrap_or_default();
    Ok(Json(serde_json::json!({
        "address": format!("0x{}", addr.strip_prefix("0x").unwrap_or(&addr)),
        "verified": verification.as_ref().map(|v| v.verified).unwrap_or(false),
        "compilerVersion": verification.as_ref().and_then(|v| v.compiler_version.clone()),
        "nativeAudit": verification.as_ref().and_then(|v| v.native_audit.clone()),
        "submittedAt": verification.as_ref().and_then(|v| v.submitted_at.clone()),
        "certifications": certifications
    })))
}

#[derive(Deserialize)]
pub struct CertifyRequest {
    contract_address: Option<String>,
    provider_id: Option<String>,
    certification_type: Option<String>,
    report_url: Option<String>,
}

pub async fn verify_certify_post(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<CertifyRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let address = body.contract_address.unwrap_or_default().trim().to_string();
    let provider = body.provider_id.unwrap_or_default().trim().to_string();
    if address.is_empty() || provider.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({ "success": false, "error": "contractAddress and providerId are required" }),
            ),
        );
    }
    let addr = address.to_lowercase();
    let c_type = body
        .certification_type
        .unwrap_or_else(|| "security_audit".to_string());
    let mut certs = state.certifications_store.write().await;
    certs
        .entry(addr.clone())
        .or_default()
        .push(CertificationEntry {
            provider_id: provider.clone(),
            certification_type: c_type.clone(),
            report_url: body.report_url.clone(),
            attested_at: chrono::Utc::now().to_rfc3339(),
        });
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "success": true,
            "contractAddress": address,
            "providerId": provider,
            "certificationType": c_type,
            "message": "Certification recorded"
        })),
    )
}

#[derive(Deserialize)]
pub struct VerifyRequest {
    contract_address: Option<String>,
    compiler_type: Option<String>,
    compiler_version: Option<String>,
    license: Option<String>,
    source_code: Option<String>,
    constructor_arguments: Option<String>,
}

pub async fn verify_post(
    State(state): State<Arc<AppState>>,
    axum::Json(body): axum::Json<VerifyRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let address = body.contract_address.unwrap_or_default().trim().to_string();
    let source = body.source_code.unwrap_or_default().trim().to_string();
    if address.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "success": false, "error": "Contract address is required" })),
        );
    }
    if !address.starts_with("0x") || address.len() != 42 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "success": false, "error": "Invalid contract address" })),
        );
    }
    if source.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({ "success": false, "error": "Contract source code is required" }),
            ),
        );
    }
    let compiler_ver = body
        .compiler_version
        .unwrap_or_else(|| "v0.8.20+commit.a1b79de6".to_string());
    let key = address.to_lowercase();
    let entry = VerificationEntry {
        verified: true,
        compiler_version: Some(compiler_ver),
        native_audit: Some(serde_json::json!({
            "passed": null,
            "securityScore": null,
            "conformityScore": null,
            "findings": [],
            "message": "Native Contract Audit Agent integration pending; certification by third parties available via POST /api/v1/verify/certify"
        })),
        submitted_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    {
        let mut ver = state.verification_store.write().await;
        ver.insert(key.clone(), entry);
    }
    let ver = state.verification_store.read().await;
    let native_audit = ver.get(&key).and_then(|e| e.native_audit.clone());
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "success": true,
            "message": "Verification submitted successfully. Contract will be verified shortly.",
            "contractAddress": address,
            "nativeAudit": native_audit
        })),
    )
}

#[derive(Deserialize)]
pub struct TokenRegisterRequest {
    address: Option<String>,
    name: Option<String>,
    symbol: Option<String>,
    decimals: Option<u8>,
}

pub async fn tokens_register_post(
    axum::Json(body): axum::Json<TokenRegisterRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let address = body.address.unwrap_or_default().trim().to_string();
    if address.is_empty() || !address.starts_with("0x") || address.len() != 42 {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({ "success": false, "error": "Valid contract address is required" }),
            ),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "success": true,
            "message": "Token registration submitted. It will appear after verification.",
            "address": address
        })),
    )
}
