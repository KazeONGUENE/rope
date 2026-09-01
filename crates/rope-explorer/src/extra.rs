//! Extra API handlers transferred from dcscan-api (consensus, defi, verify, services registry, etc.)

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
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

/// Live chain/consensus status. Previously a hardcoded stub that always
/// claimed `ropeNodeConnected: false` and `executionMode: "Anvil direct"`
/// (Anvil was archived 2026-03-31 - see `reth-blue-green-ipfs-architecture.mdc`).
/// Now probes the real RPC fleet (`eth_blockNumber`, `net_peerCount`) and
/// mirrors the live testimony cache for agent/round counters.
pub async fn consensus(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let block_result = crate::rpc_call(&state, "eth_blockNumber", vec![]).await;
    let rope_node_connected = block_result.is_ok();
    let latest_block = block_result
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .map(|s| crate::hex_to_u64(&s))
        .unwrap_or(0);

    // net_peerCount is best-effort - Reth's RPC forwarder does not always
    // expose it on every deployment. A failed call is surfaced as `null`,
    // never as a fabricated fixed number.
    let peer_count = crate::rpc_call(&state, "net_peerCount", vec![])
        .await
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .map(|s| crate::hex_to_u64(&s));

    let (finalized_testimonies, active_agents) = {
        let cache = state.testimony_cache.read().await;
        match cache.as_ref() {
            Some(c) => (
                c.stats
                    .get("totalTestimonies")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                c.stats
                    .get("activeAgents")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
            ),
            None => (0, 0),
        }
    };

    Json(serde_json::json!({
        "ropeNodeConnected": rope_node_connected,
        "lastPolled": chrono::Utc::now().to_rfc3339(),
        "chain": {
            "chainId": state.chain_id,
            "networkName": state.network_name,
            "version": "rope-node (Quipu Canon v1.2) / Reth v1.11.2",
            "consensusType": "testimony",
            "executionMode": "Reth (blue-green fleet)",
            "peerCount": peer_count
        },
        "testimony": {
            "currentRound": latest_block,
            "pendingTransactions": 0,
            "finalizedTransactions": finalized_testimonies,
            "executionLayerConnected": rope_node_connected
        },
        "aiAgents": { "activeCount": active_agents, "agents": [] },
        "evmState": { "latestBlock": latest_block, "blockTime": 3 }
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

/// Mirrors the live testimony cache (same source `testimonies_list_live` /
/// `testimonies_stats_live` read from - refreshed every 60s by the
/// background scanner) instead of the previous hardcoded "rope-node is not
/// connected" stub, which lied even while 30k+ real testimonies were being
/// served correctly at `/api/v1/testimonies`.
pub async fn consensus_testimonies(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let cache = state.testimony_cache.read().await;
    match cache.as_ref() {
        Some(c) => {
            let total_finalized = c
                .stats
                .get("totalTestimonies")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            Json(serde_json::json!({
                "ropeNodeConnected": true,
                "totalFinalized": total_finalized,
                "totalPending": 0,
                "testimonies": c.testimonies.clone(),
                "updatedAt": c.updated_at,
                "message": "Live testimony data from rope-node's canonical AI agents (oracle/insurance/validation/semantic/compliance)."
            }))
        }
        None => Json(serde_json::json!({
            "ropeNodeConnected": false,
            "totalFinalized": 0,
            "totalPending": 0,
            "testimonies": [],
            "message": "Testimony cache is warming (refreshed every 60s) - retry shortly."
        })),
    }
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

/// Known DCR-20 token contract addresses used by the address-page
/// "Transfers" tab to scan cross-token history for a given wallet/contract.
/// Same live+planned address set as `known_token()` / the `known_dcr20`
/// list in `account_tokens` (main.rs) so the explorer stays consistent
/// across `/address`, `/token`, and `/tokens`.
const KNOWN_DCR20_ADDRS: &[&str] = &[
    "0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4", // WFAT (live)
    "0xddbf887982a2a1c03cb8705fef9e09c46122fff6", // WFAT (planned)
    "0xb93bd8db94f1baff474aa9cba0739daaad01641f", // USDC (live)
    "0x3109c838e9a08a42fba000a48310845919759a02", // USDC (planned)
    "0x79a26132f48394421382c13b54ae77fa3af73289", // USDT (live)
    "0x73e3cc285b962c4c6b6b1503d8fd8ac745f6b1ef", // USDT (planned)
    "0x24d6137807fa8a592888726d87ac748d018c6d4a", // EUROD (live)
    "0xc784ea07aae35b22630df7e3f3ae9e2ccc64f1aa", // EUROD (planned)
];

fn topic_from_address(addr: &str) -> String {
    let clean = addr.trim_start_matches("0x").to_lowercase();
    format!("0x{:0>64}", clean)
}

/// Real `eth_getCode` lookup, with best-effort DCR-20 interface detection
/// (name/symbol/decimals/totalSupply) for the "Contract" tab. Previously
/// always returned `bytecode: null` regardless of the address, which the
/// frontend rendered as "This address is an EOA - no bytecode" even for
/// live contracts like WFAT (`0x285eecf5…b3e4`, ~5.6 KB real bytecode).
pub async fn account_bytecode(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    // Retry-tolerant eth_getCode - mirrors the pattern already used on the
    // token page (`get_token`) and `account_overview_live`. A transient RPC
    // blip must never be reported as "this is an EOA".
    let code = {
        let mut out: Option<String> = None;
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(tokio::time::Duration::from_millis(200 * attempt as u64)).await;
            }
            if let Ok(v) = crate::rpc_call(
                &state,
                "eth_getCode",
                vec![serde_json::json!(&address), serde_json::json!("latest")],
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
    let is_contract = code != "0x" && code.len() > 2;
    let bytecode_size = if is_contract { (code.len() - 2) / 2 } else { 0 };

    let mut resp = serde_json::json!({
        "address": address,
        "bytecode": if is_contract { serde_json::json!(code) } else { serde_json::Value::Null },
        "isContract": is_contract,
        "bytecodeSize": bytecode_size,
    });

    if is_contract {
        // Best-effort ERC-20/DCR-20 interface probe. Read-only and safe on
        // any contract - non-token contracts simply return empty/revert
        // data, surfaced honestly as `null` rather than a fabricated label.
        let name_hex = crate::eth_call_token_method(&state, &address, "0x06fdde03").await;
        let symbol_hex = crate::eth_call_token_method(&state, &address, "0x95d89b41").await;
        let decimals_hex = crate::eth_call_token_method(&state, &address, "0x313ce567").await;
        let total_supply_hex = crate::eth_call_token_method(&state, &address, "0x18160ddd").await;

        let name = name_hex.as_deref().and_then(crate::decode_abi_string);
        let symbol = symbol_hex.as_deref().and_then(crate::decode_abi_string);
        let decimals = decimals_hex.as_deref().map(|h| crate::hex_to_u64(h) as u32);
        let total_supply_raw = total_supply_hex.as_deref().map(crate::decode_hex_u256);

        if name.is_some() || symbol.is_some() {
            resp["detectedInterface"] = serde_json::json!("DCR-20 (ERC-20 compatible)");
            resp["tokenMetadata"] = serde_json::json!({
                "name": name,
                "symbol": symbol,
                "decimals": decimals,
                "totalSupplyRaw": total_supply_raw.map(|v| v.to_string()),
            });
        }

        if let Some(label) = crate::known_label(&address) {
            resp["label"] = serde_json::json!(label);
        }

        // Etherscan-style Contract tab: when the repository has the
        // canonical source for this address, attach it next to the live
        // runtime bytecode. Status is "source published", never a fake
        // compiler-matched Sourcify badge.
        if let Some(pubsrc) = crate::verified_contracts::published_source(&address) {
            let functions: Vec<serde_json::Value> = pubsrc
                .abi
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("function"))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            resp["contractName"] = serde_json::json!(pubsrc.contract_name);
            resp["compiler"] = serde_json::json!(pubsrc.compiler);
            resp["license"] = serde_json::json!(pubsrc.license);
            resp["optimization"] = serde_json::json!(pubsrc.optimization);
            resp["evmVersion"] = serde_json::json!(pubsrc.evm_version);
            resp["sourcePath"] = serde_json::json!(pubsrc.source_path);
            resp["sourceCode"] = serde_json::json!(pubsrc.source);
            resp["abi"] = pubsrc.abi.clone();
            resp["abiFunctions"] = serde_json::json!(functions);
            resp["sourceAvailable"] = serde_json::json!(true);
            resp["verificationKind"] = serde_json::json!("repository-source");
            resp["verificationNote"] = serde_json::json!(
                "Source is the canonical repository copy. dcscan does not recompile and byte-compare; runtime bytecode below is live eth_getCode."
            );
        } else {
            resp["sourceAvailable"] = serde_json::json!(false);
            resp["verificationKind"] = serde_json::json!("unverified");
            resp["verificationNote"] = serde_json::json!(
                "No published source for this address. Runtime bytecode is live eth_getCode."
            );
        }
    }

    Json(resp)
}

pub fn pool_token_addr(p: &serde_json::Value, key: &str) -> String {
    p.get(key)
        .and_then(|t| {
            t.get("address")
                .and_then(|v| v.as_str())
                .or_else(|| t.as_str())
        })
        .unwrap_or("")
        .to_lowercase()
}

pub fn pool_volume_usd(p: &serde_json::Value) -> f64 {
    if let Some(v) = p.get("volume_24h").and_then(|v| v.as_f64()) {
        return v;
    }
    p.get("volume_24h")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// Per-token 24h USD volume from `dcswap.net/v1/pools`. Zero means
/// unknown / unreachable, never a fabricated print.
pub async fn dcswap_volumes_by_token(
    state: &AppState,
) -> std::collections::HashMap<String, f64> {
    let mut map = std::collections::HashMap::new();
    let dcswap_base = state.dcswap_api.trim_end_matches('/');
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
        Err(_) => return map,
    };
    let pools = pools_json
        .get("data")
        .and_then(|d| d.get("pools"))
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    for p in pools {
        let vol = pool_volume_usd(&p);
        if vol <= 0.0 {
            continue;
        }
        for key in ["token_a", "token_b"] {
            let addr = pool_token_addr(&p, key);
            if addr.len() == 42 {
                *map.entry(addr).or_insert(0.0) += vol;
            }
        }
    }
    map
}

/// Sum `volume_24h` across DCSwap pools that contain `addr_lc`.
/// Returns 0.0 when the indexer is unreachable - callers must treat
/// zero as "unknown", never as a fabricated print.
pub async fn dcswap_volume_24h_usd(state: &AppState, addr_lc: &str) -> f64 {
    dcswap_volumes_by_token(state)
        .await
        .get(&addr_lc.to_lowercase())
        .copied()
        .unwrap_or(0.0)
}

#[derive(Deserialize)]
pub struct TransfersQuery {
    page: Option<u32>,
    limit: Option<u32>,
}

/// Convert a `LogRef` from the per-address index into the same
/// `transfers[]` JSON shape produced by the legacy `eth_getLogs` scan
/// below. Returns `None` when:
///   - `topics[0]` is not the DCR-20 `Transfer` event (address may be
///     tagged in unrelated logs like `Approval`, `Sync`, etc.)
///   - `emitter` is not a known DCR-20 contract (unknown / new token)
///   - the log's indexed role is `Emitter` (address is the token itself,
///     not a sender/receiver of the transfer)
///   - topics 1 and 2 are absent / malformed (defensive against
///     non-standard token contracts that reuse the Transfer topic)
///
/// The returned JSON is byte-compatible with the legacy path so the
/// frontend needs zero changes.
fn transfer_json_from_addr_index_log(
    log: &rope_addr_index::schema::LogRef,
) -> Option<serde_json::Value> {
    // Constant-time comparison of topic0 against the DCR-20 Transfer
    // event signature. Stored as raw bytes so we don't allocate a hex
    // string per row - even at 1M rows the cost is negligible but the
    // comparison here runs inside a tight loop.
    const TRANSFER_TOPIC_BYTES: [u8; 32] = [
        0xdd, 0xf2, 0x52, 0xad, 0x1b, 0xe2, 0xc8, 0x9b, 0x69, 0xc2, 0xb0, 0x68, 0xfc, 0x37, 0x8d, 0xaa,
        0x95, 0x2b, 0xa7, 0xf1, 0x63, 0xc4, 0xa1, 0x16, 0x28, 0xf5, 0x5a, 0x4d, 0xf5, 0x23, 0xb3, 0xef,
    ];
    if log.topics.first().map(|t| t.as_slice()) != Some(&TRANSFER_TOPIC_BYTES[..]) {
        return None;
    }
    // Role must be Topic1 (sender) or Topic2 (receiver). Emitter role
    // means the address is the token contract - we surface those in the
    // /events tab, not /transfers. Topic3 is Approval-family and does
    // not appear for Transfer.
    match log.role {
        rope_addr_index::schema::LogRole::Topic1 | rope_addr_index::schema::LogRole::Topic2 => {}
        _ => return None,
    }
    let token_addr = format!("0x{}", hex_lower(&log.emitter));
    let info = crate::known_token(&token_addr)?;
    let from_bytes = log.topics.get(1)?;
    let to_bytes = log.topics.get(2)?;
    // Topic-encoded addresses: last 20 bytes of the 32-byte topic.
    let from = format!("0x{}", hex_lower(&from_bytes[12..]));
    let to = format!("0x{}", hex_lower(&to_bytes[12..]));
    // `Transfer(uint256)` data is a single 32-byte big-endian integer.
    // We normalise to f64 for display, exactly matching the legacy path.
    let raw = decode_u256_be(&log.data);
    let value = raw as f64 / 10f64.powi(info.decimals as i32);
    let tx_hash = format!("0x{}", hex_lower(&log.tx_hash));
    Some(serde_json::json!({
        "transactionHash": tx_hash,
        "blockNumber": log.block_number,
        "from": from,
        "to": to,
        "value": value,
        "valueRaw": raw.to_string(),
        "tokenAddress": token_addr,
        "tokenSymbol": info.symbol,
    }))
}

/// Stamp USD on a Transfer JSON row via PriceLens (WFAT ≡ FAT).
fn attach_transfer_usd(row: &mut serde_json::Value, lens: &crate::PriceLens) {
    let token = row
        .get("tokenAddress")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let value = row.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let usd = crate::known_token(token)
        .map(|info| value * lens.price_for(&info))
        .unwrap_or(0.0);
    if let Some(map) = row.as_object_mut() {
        map.insert("usdRaw".to_string(), serde_json::json!(usd));
        map.insert(
            "usdValue".to_string(),
            serde_json::json!(if usd > 0.0 {
                format!("${:.2}", usd)
            } else {
                "-".to_string()
            }),
        );
    }
}

/// Lower-hex encoder that stays vendored inside `extra.rs` so we don't
/// have to add `hex` to this file's imports (it's already a transitive
/// dependency via the rest of the crate).
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// Decode a raw big-endian u256 payload into a u128, saturating on
/// overflow. Every real DCR-20 `Transfer.value` fits comfortably in
/// u128 (2^128 wei is ~340 undecillion FAT, far above the asymptotic
/// supply of 18e9 FAT). Kept `pub(super)`-clean because the helper is
/// only used by `transfer_json_from_addr_index_log`.
fn decode_u256_be(data: &[u8]) -> u128 {
    if data.is_empty() {
        return 0;
    }
    // Take the last 16 bytes (u128 fits in the low 128 bits of the u256).
    // Higher bits are silently dropped; caller is documented above.
    let start = data.len().saturating_sub(16);
    let slice = &data[start..];
    let mut arr = [0u8; 16];
    arr[16 - slice.len()..].copy_from_slice(slice);
    u128::from_be_bytes(arr)
}

/// Try to answer the /transfers query from the persistent per-address
/// index. Returns `Some(json)` when the index is open AND we found at
/// least one DCR-20 Transfer this address participated in. Returns
/// `None` on every failure mode (index absent, reader error, panic in
/// the blocking task, or filtered-to-empty page) so the legacy
/// chunked `eth_getLogs` scan can still take over and probe the last
/// 50k blocks for the specific address.
///
/// The reader is called with a limit inflated by ×4 to leave headroom
/// after we filter down to just DCR-20 Transfer events - the index
/// stores every log the address participated in, not just Transfers.
async fn try_account_transfers_from_index(
    state: &AppState,
    address: &str,
    limit: usize,
) -> Option<serde_json::Value> {
    let idx = state.addr_index.as_ref()?.clone();
    let addr_owned = address.to_string();
    // Over-fetch so filtering to Transfer-only + known-token-only still
    // leaves us with `limit` real rows in the common case.
    let raw_limit = limit.saturating_mul(4).min(500);
    let scan = tokio::task::spawn_blocking(move || idx.logs(&addr_owned, raw_limit, None)).await;
    let page = match scan {
        Ok(Ok(page)) => page,
        Ok(Err(e)) => {
            tracing::warn!(
                address = %address,
                error = ?e,
                "per-address index reader failed for /api/v1/accounts/:addr/transfers - \
                 falling back to legacy chunked eth_getLogs scan",
            );
            return None;
        }
        Err(join_err) => {
            tracing::warn!(
                address = %address,
                error = ?join_err,
                "per-address index blocking task panicked on /transfers - falling back to \
                 legacy chunked eth_getLogs scan",
            );
            return None;
        }
    };
    let lens = crate::PriceLens::snapshot(state).await;
    let mut out: Vec<serde_json::Value> = Vec::with_capacity(limit);
    for log in page.items.iter() {
        if out.len() >= limit {
            break;
        }
        if let Some(mut j) = transfer_json_from_addr_index_log(log) {
            attach_transfer_usd(&mut j, &lens);
            out.push(j);
        }
    }
    if out.is_empty() {
        // Writer may not have indexed this address yet, OR the address
        // has never participated in a DCR-20 Transfer. Either way we
        // want the legacy scan to try before we surface an empty page.
        tracing::debug!(
            address = %address,
            raw_page_len = page.items.len(),
            "per-address index returned 0 filtered Transfer rows - falling back to legacy scan"
        );
        return None;
    }
    // Server-side sort in case the index and RocksDB reverse-cursor
    // ordering ever drifted; legacy path did the same guard.
    out.sort_by(|a, b| {
        let ba = a.get("blockNumber").and_then(|v| v.as_u64()).unwrap_or(0);
        let bb = b.get("blockNumber").and_then(|v| v.as_u64()).unwrap_or(0);
        bb.cmp(&ba)
    });
    Some(serde_json::json!({
        "address": address,
        "transfers": out,
        "pagination": { "page": 1, "limit": limit, "total": out.len() },
        "hasMore": page.next_cursor.is_some(),
        "nextCursor": page.next_cursor,
        "source": "addr-index (per-address RocksDB index, DCR-20 Transfer events, newest first)",
    }))
}

/// Real DCR-20 `Transfer` log scan for the address-page "Transfers" tab.
/// Scans `eth_getLogs` across the known DCR-20 token contracts for this
/// address as either sender (`topics[1]`) or recipient (`topics[2]`),
/// chunked backward from head. Previously always returned `transfers: []`.
pub async fn account_transfers(
    Path(address): Path<String>,
    Query(q): Query<TransfersQuery>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let page = q.page.unwrap_or(1);
    let limit = q.limit.unwrap_or(20).min(100) as usize;
    let addr_lc = address.to_lowercase();
    let addr_topic = topic_from_address(&addr_lc);

    // Reader-first path: if the per-address index is open, prefer it. On
    // any failure (index absent, reader error, or zero filtered rows for
    // this specific address) fall through silently to the legacy scan.
    if let Some(v) = try_account_transfers_from_index(&state, &address, limit).await {
        return Json(v);
    }

    let head = crate::rpc_block_number(&state).await.unwrap_or(0);
    let lens = crate::PriceLens::snapshot(&state).await;
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    const MAX_LOOKBACK: u64 = 50_000;
    const CHUNK: u64 = 2_000;

    if head > 0 {
        let oldest = head.saturating_sub(MAX_LOOKBACK);
        let mut cursor = head;

        while cursor > oldest && out.len() < limit {
            let from_block = cursor.saturating_sub(CHUNK).max(oldest);
            for topic_filter in [
                serde_json::json!([crate::TRANSFER_TOPIC, addr_topic.clone()]),
                serde_json::json!([crate::TRANSFER_TOPIC, serde_json::Value::Null, addr_topic.clone()]),
            ] {
                if let Ok(v) = crate::rpc_call(
                    &state,
                    "eth_getLogs",
                    vec![serde_json::json!({
                        "fromBlock": format!("0x{:x}", from_block),
                        "toBlock": format!("0x{:x}", cursor),
                        "address": KNOWN_DCR20_ADDRS,
                        "topics": topic_filter,
                    })],
                )
                .await
                {
                    if let Some(arr) = v.as_array() {
                        for log in arr.iter().rev() {
                            let tx_hash = log
                                .get("transactionHash")
                                .and_then(|x| x.as_str())
                                .unwrap_or("");
                            let log_index = log
                                .get("logIndex")
                                .and_then(|x| x.as_str())
                                .unwrap_or("0x0");
                            let key = format!("{}:{}", tx_hash, log_index);
                            if tx_hash.is_empty() || !seen.insert(key) {
                                continue;
                            }
                            let token_addr = log
                                .get("address")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_lowercase();
                            let info = crate::known_token(&token_addr);
                            let topics = log
                                .get("topics")
                                .and_then(|x| x.as_array())
                                .cloned()
                                .unwrap_or_default();
                            let from = topics
                                .get(1)
                                .and_then(|t| t.as_str())
                                .map(crate::topic_to_address)
                                .unwrap_or_default();
                            let to = topics
                                .get(2)
                                .and_then(|t| t.as_str())
                                .map(crate::topic_to_address)
                                .unwrap_or_default();
                            let data = log.get("data").and_then(|x| x.as_str()).unwrap_or("0x0");
                            let raw = crate::decode_hex_u256(data);
                            let decimals = info.as_ref().map(|i| i.decimals).unwrap_or(18);
                            let value = raw as f64 / 10f64.powi(decimals as i32);
                            let block = log
                                .get("blockNumber")
                                .and_then(|x| x.as_str())
                                .map(crate::hex_to_u64)
                                .unwrap_or(0);
                            let mut row = serde_json::json!({
                                "transactionHash": tx_hash,
                                "blockNumber": block,
                                "from": from,
                                "to": to,
                                "value": value,
                                "valueRaw": raw.to_string(),
                                "tokenAddress": token_addr,
                                "tokenSymbol": info.as_ref().map(|i| i.symbol).unwrap_or("?"),
                            });
                            attach_transfer_usd(&mut row, &lens);
                            out.push(row);
                        }
                    }
                }
            }
            if from_block == oldest {
                break;
            }
            cursor = from_block.saturating_sub(1);
        }
    }

    out.sort_by(|a, b| {
        let ba = a.get("blockNumber").and_then(|v| v.as_u64()).unwrap_or(0);
        let bb = b.get("blockNumber").and_then(|v| v.as_u64()).unwrap_or(0);
        bb.cmp(&ba)
    });
    out.truncate(limit);

    Json(serde_json::json!({
        "address": address,
        "transfers": out,
        "pagination": { "page": page, "limit": limit, "total": out.len() },
        "scanWindowBlocks": MAX_LOOKBACK,
        "source": "eth_getLogs (DCR-20 Transfer events across known token contracts)",
    }))
}

/// Real contract event-log scan for the address-page "Events" tab.
/// EOAs cannot emit logs, so an EOA request is answered honestly (empty
/// with an explanatory note) rather than an indistinguishable bare `[]`.
/// Previously always returned `events: []` regardless of address type.
pub async fn account_events(
    Path(address): Path<String>,
    Query(q): Query<TransfersQuery>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let page = q.page.unwrap_or(1);
    let limit = q.limit.unwrap_or(20).min(100) as usize;
    let addr_lc = address.to_lowercase();

    let code = crate::rpc_call(
        &state,
        "eth_getCode",
        vec![serde_json::json!(&addr_lc), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x".to_string());
    let is_contract = code != "0x" && code.len() > 2;

    if !is_contract {
        return Json(serde_json::json!({
            "address": address,
            "events": [],
            "pagination": { "page": page, "limit": limit, "total": 0 },
            "note": "This address is an EOA - externally-owned accounts cannot emit log events.",
        }));
    }

    let head = crate::rpc_block_number(&state).await.unwrap_or(0);
    let mut out: Vec<serde_json::Value> = Vec::new();
    const MAX_LOOKBACK: u64 = 50_000;
    const CHUNK: u64 = 2_000;

    if head > 0 {
        let oldest = head.saturating_sub(MAX_LOOKBACK);
        let mut cursor = head;
        while cursor > oldest && out.len() < limit {
            let from_block = cursor.saturating_sub(CHUNK).max(oldest);
            if let Ok(v) = crate::rpc_call(
                &state,
                "eth_getLogs",
                vec![serde_json::json!({
                    "fromBlock": format!("0x{:x}", from_block),
                    "toBlock": format!("0x{:x}", cursor),
                    "address": addr_lc,
                })],
            )
            .await
            {
                if let Some(arr) = v.as_array() {
                    for log in arr.iter().rev() {
                        let tx_hash = log
                            .get("transactionHash")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let block = log
                            .get("blockNumber")
                            .and_then(|x| x.as_str())
                            .map(crate::hex_to_u64)
                            .unwrap_or(0);
                        let log_index = log
                            .get("logIndex")
                            .and_then(|x| x.as_str())
                            .unwrap_or("0x0")
                            .to_string();
                        let topics: Vec<String> = log
                            .get("topics")
                            .and_then(|x| x.as_array())
                            .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                            .unwrap_or_default();
                        let data = log
                            .get("data")
                            .and_then(|x| x.as_str())
                            .unwrap_or("0x")
                            .to_string();
                        out.push(serde_json::json!({
                            "transactionHash": tx_hash,
                            "blockNumber": block,
                            "logIndex": log_index,
                            "topics": topics,
                            "data": data,
                        }));
                        if out.len() >= limit {
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
    }

    out.sort_by(|a, b| {
        let ba = a.get("blockNumber").and_then(|v| v.as_u64()).unwrap_or(0);
        let bb = b.get("blockNumber").and_then(|v| v.as_u64()).unwrap_or(0);
        bb.cmp(&ba)
    });
    out.truncate(limit);

    Json(serde_json::json!({
        "address": address,
        "events": out,
        "pagination": { "page": page, "limit": limit, "total": out.len() },
        "scanWindowBlocks": MAX_LOOKBACK,
        "source": "eth_getLogs (contract event log scan)",
    }))
}

/// Live DCSwap pool list - queries the real `/v1/pools` endpoint. Previously
/// `defi_overview` hardcoded `pools: [] / totalPools: 0` even when the
/// analytics summary call succeeded, so the Defi page showed TVL/volume
/// numbers with no pools to back them up.
async fn fetch_dcswap_pools(state: &AppState) -> Vec<serde_json::Value> {
    let url = format!("{}/v1/pools", state.dcswap_api.trim_end_matches('/'));
    match state.http_client.get(&url).send().await {
        Ok(r) if r.status().is_success() => {
            if let Ok(data) = r.json::<serde_json::Value>().await {
                // DCSwap wraps the pool list as {"success":true,"data":{"pools":[...]}}.
                // Fall back to a top-level "pools" key in case the shape changes.
                return data
                    .get("data")
                    .and_then(|d| d.get("pools"))
                    .or_else(|| data.get("pools"))
                    .and_then(|p| p.as_array())
                    .cloned()
                    .unwrap_or_default();
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

pub async fn defi_overview(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let url = format!(
        "{}/v1/analytics/overview",
        state.dcswap_api.trim_end_matches('/')
    );
    let pools = fetch_dcswap_pools(&state).await;

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
                    "totalPools": pools.len(),
                    "pools": pools
                }));
            }
        }
        _ => {}
    }
    Json(serde_json::json!({
        "source": "DCSwap",
        "error": "Failed to fetch analytics overview",
        "tvl": "0",
        "volume24h": "0",
        "totalPools": pools.len(),
        "pools": pools
    }))
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
    // CERBER WATCH - registered services are listed publicly; the name,
    // description, and health_url are all attacker-influenced free text.
    if let Err(resp) = crate::security_guard::validate_fields(&[
        ("name", name.as_str()),
        ("description", body.description.as_deref().unwrap_or("")),
        ("health_url", body.health_url.as_deref().unwrap_or("")),
        ("provider_id", body.provider_id.as_deref().unwrap_or("")),
    ]) {
        return resp;
    }
    // CERBER WATCH - SSRF guard: `health_url` is not just displayed text,
    // it is dialed by this server on a periodic health-check loop
    // (`agent_health_ok`), which makes it a genuine server-side outbound
    // request target. Reject cloud-metadata / loopback / private /
    // link-local targets at submission time; the fetch path re-checks this
    // again with full DNS resolution as defense-in-depth.
    if let Some(url) = body.health_url.as_deref() {
        if let Err(resp) = crate::security_guard::validate_outbound_url("health_url", url) {
            return resp;
        }
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

/// Records a third-party certification (e.g. a security audit badge) for
/// a contract address. **Authenticated** - see finding C8 of
/// `docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md`.
///
/// Before this fix this endpoint accepted an arbitrary `provider_id`
/// string from any anonymous caller, so anyone could inject a fabricated
/// "CertiK audited this contract" (or any other trusted auditor's name)
/// entry that would then render on the public `/address` page - a direct
/// impersonation / rug-pull-enablement vector against users who trust
/// the certification badge.
///
/// The caller must now present the exact secret provisioned for the
/// claimed `provider_id` via the `X-Provider-Secret` header (see
/// `certification_providers::CertificationProviderRegistry`). Providers
/// are onboarded out-of-band by the Datachain Foundation (env var
/// `DCSCAN_CERTIFICATION_PROVIDERS`) once a real due-diligence
/// relationship exists - there is no self-service path, by design: a
/// self-service "prove you're a Datachain ID user" key would not prove
/// "you are the real CertiK". When no provider has been onboarded yet
/// (the out-of-the-box state), every submission is honestly refused with
/// `501 Not Implemented` rather than silently accepted - no stub
/// acceptance.
pub async fn verify_certify_post(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
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
    if !address.starts_with("0x") || address.len() != 42 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "success": false, "error": "Invalid contract address" })),
        );
    }

    if state.certification_providers.is_empty() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({
                "success": false,
                "error": "no_certification_providers_onboarded",
                "message": "Third-party contract certification is not yet available - no \
                    certification provider has been onboarded on this explorer. This endpoint \
                    intentionally refuses every submission rather than recording an \
                    unauthenticated claim. Contact the Datachain Foundation to onboard as a \
                    certification provider."
            })),
        );
    }

    let presented_secret = headers
        .get("x-provider-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let provider_record = match state
        .certification_providers
        .authenticate(&provider, presented_secret)
    {
        Some(record) => record,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "success": false,
                    "error": "invalid_provider_credentials",
                    "message": "The providerId is not onboarded, or the X-Provider-Secret \
                        header is missing/incorrect for that provider. Certifications can only \
                        be recorded by the party holding that provider's dedicated secret."
                })),
            );
        }
    };

    let addr = address.to_lowercase();
    let c_type = body
        .certification_type
        .unwrap_or_else(|| "security_audit".to_string());
    let provider_id = provider.to_lowercase();

    // CERBER WATCH - the provider secret only proves the caller is a
    // legitimate onboarded provider, not that its own systems weren't
    // compromised into sending a malicious payload. `certification_type`
    // and `report_url` are both rendered on the public `/address` page.
    if let Err(resp) = crate::security_guard::validate_fields(&[
        ("certification_type", c_type.as_str()),
        ("report_url", body.report_url.as_deref().unwrap_or("")),
    ]) {
        return resp;
    }

    let mut certs = state.certifications_store.write().await;
    certs
        .entry(addr.clone())
        .or_default()
        .push(CertificationEntry {
            provider_id: provider_id.clone(),
            certification_type: c_type.clone(),
            report_url: body.report_url.clone(),
            attested_at: chrono::Utc::now().to_rfc3339(),
        });
    tracing::info!(
        provider_id = %provider_id,
        provider_display_name = %provider_record.display_name,
        contract_address = %address,
        "certification recorded"
    );
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "success": true,
            "contractAddress": address,
            "providerId": provider_id,
            "providerDisplayName": provider_record.display_name,
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

/// Accepts a source-verification submission and records it honestly.
/// Previously set `verified: true` unconditionally on any well-formed
/// submission with zero bytecode/source matching - a fabricated "verified"
/// badge with no compile-and-match behind it. This handler now (a) confirms
/// the address is a real deployed contract via `eth_getCode` before
/// accepting anything, and (b) records the submission as
/// `verified: false` / `status: "pending_review"`, since this explorer does
/// not yet run a solc compile-and-bytecode-match pipeline. No caller of
/// `verify_get`/the `/contracts` list should treat a submission alone as
/// proof of verification.
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

    let code = crate::rpc_call(
        &state,
        "eth_getCode",
        vec![serde_json::json!(&address), serde_json::json!("latest")],
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(String::from))
    .unwrap_or_else(|| "0x".to_string());
    if code == "0x" || code.len() <= 2 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": "No contract bytecode found at this address on Datachain Rope - cannot verify an address with no deployed code."
            })),
        );
    }

    // CERBER WATCH - `compiler_type`/`compiler_version`/`license`/
    // `constructor_arguments` are structured metadata fields rendered on
    // the public contract page. Deliberately excludes `source_code`
    // itself (see `security_guard` module docs: block comments are
    // ubiquitous in real Solidity and would false-positive on every
    // legitimate submission under the generic SQL-comment heuristic).
    if let Err(resp) = crate::security_guard::validate_fields(&[
        ("compiler_type", body.compiler_type.as_deref().unwrap_or("")),
        ("compiler_version", body.compiler_version.as_deref().unwrap_or("")),
        ("license", body.license.as_deref().unwrap_or("")),
        ("constructor_arguments", body.constructor_arguments.as_deref().unwrap_or("")),
    ]) {
        return resp;
    }

    let compiler_ver = body
        .compiler_version
        .unwrap_or_else(|| "v0.8.20+commit.a1b79de6".to_string());
    let key = address.to_lowercase();
    let entry = VerificationEntry {
        verified: false,
        compiler_version: Some(compiler_ver),
        native_audit: Some(serde_json::json!({
            "passed": null,
            "securityScore": null,
            "conformityScore": null,
            "findings": [],
            "message": "Automated compile-and-bytecode-match verification is not implemented yet. This submission is recorded as pending manual review, not verified. Third-party certification is available via POST /api/v1/verify/certify."
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
            "status": "pending_review",
            "message": "Submission recorded as pending manual review. This explorer does not yet perform automated compile-and-bytecode-match verification, so this contract is NOT marked as verified.",
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

/// Previously claimed "Token registration submitted. It will appear after
/// verification." for every well-formed request while persisting nothing
/// and having no mechanism that would ever make the token "appear"
/// anywhere (the `/tokens` list is driven entirely by the hardcoded
/// canonical DCR-20 address set in `main.rs`, not by any registry this
/// handler writes to). Honest response: acknowledge receipt of the
/// well-formed request but state plainly that self-service token listing
/// is not implemented yet, rather than promising an outcome that cannot
/// happen.
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
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "success": false,
            "address": address,
            "error": "Self-service token registration is not implemented yet - the /tokens list is currently a curated canonical DCR-20 address set, not a self-service registry. This submission was not persisted.",
        })),
    )
}
