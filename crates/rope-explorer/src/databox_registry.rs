//! Global Databox Network — real registration + heartbeat registry.
//!
//! Replaces the previous fabricated `/api/v1/databoxes*` handlers (which
//! reported zero entries because no live pipeline existed) AND the
//! `databoxes.html` frontend's ~70 hardcoded fake city markers with a
//! genuine, production self-service registry:
//!
//!   - Any node operator running a Datachain Rope data source — a
//!     `rope deploy <provider> databox|rpc-slot|witness|community-node`
//!     instance, or an Ecosystem Deployment Console node hosting one of
//!     the four EDC roles (`ingestion_gateway`, `storage_ledger`,
//!     `ai_agent_host`, `federation_validator`) — can self-register by
//!     signing an EIP-191 message with the wallet that controls the
//!     node (domain-tagged `DCROPE-DATABOX-AUTH`, distinct from the
//!     vote/EDC/Datachain-ID domains so a captured signature can never
//!     be replayed across surfaces).
//!   - Liveness is a real heartbeat: the operator's node pings
//!     `POST /api/v1/databoxes/:id/heartbeat` periodically; `status` is
//!     computed lazily from `now - last_heartbeat_at` (never fabricated,
//!     never persisted as a stale flag).
//!   - Every type of data source gets its own discovery route
//!     (`/api/v1/databoxes/types` for the breakdown, and
//!     `/api/v1/databoxes/type/:type` for a filtered list), so any
//!     consumer can enumerate exactly one kind of network participant.
//!   - Registration and deregistration are anchored on-chain
//!     (`DataboxRegistered` / `DataboxDeregistered` knots on a dedicated
//!     ledger wallet) for durability and auditability, mirroring the
//!     project/vote queue in `governance_votes.rs`. Heartbeats are
//!     high-frequency liveness pings and are intentionally NOT anchored
//!     (that would spam the ledger); they are persisted locally only.
//!   - The map endpoint plots ONLY entries that have self-reported real
//!     coordinates — never synthesized geography.

use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};
use std::sync::Arc;

// ============================================================================
// EIP-191 wallet-signature verification — domain `DCROPE-DATABOX-AUTH`.
// Same k256/keccak construction as governance_votes.rs, reimplemented
// locally (module-private) so each domain tag stays unambiguous.
// ============================================================================

const DATABOX_AUTH_DOMAIN: &str = "DCROPE-DATABOX-AUTH";
const DATABOX_AUTH_WINDOW_SECS: i64 = 300;
/// A databox that hasn't heartbeated within this window is reported
/// `offline`, never fabricated as still-live.
const HEARTBEAT_TTL_SECS: i64 = 600;

/// The complete, canonical set of data-source types recognised by the
/// Global Databox Network registry. Mirrors `rope-cli`'s
/// `rope deploy <provider> <kind>` kinds (`databox`, `rpc_slot`,
/// `witness`, `community_node`) plus the four Ecosystem Deployment
/// Console node roles (`ingestion_gateway`, `storage_ledger`,
/// `ai_agent_host`, `federation_validator`) from
/// `rope-edc::types::NODE_ROLES`. Every one of these gets its own
/// filtered discovery route.
pub const DATABOX_TYPES: &[(&str, &str)] = &[
    ("databox", "Community Databox (Seeder)"),
    ("rpc_slot", "Public RPC Slot"),
    ("witness", "Testimony Witness"),
    ("community_node", "Community Node"),
    ("ingestion_gateway", "EDC Ingestion Gateway"),
    ("storage_ledger", "EDC Storage Ledger"),
    ("ai_agent_host", "EDC AI Agent Host"),
    ("federation_validator", "EDC Federation Validator"),
];

fn is_valid_databox_type(t: &str) -> bool {
    DATABOX_TYPES.iter().any(|(k, _)| *k == t)
}

fn eip191_digest(message: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(b"\x19Ethereum Signed Message:\n");
    hasher.update(message.len().to_string().as_bytes());
    hasher.update(message);
    let out = hasher.finalize();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&out);
    digest
}

fn recover_signer(message: &[u8], signature_hex: &str) -> Result<String, String> {
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

    let raw = hex::decode(signature_hex.trim_start_matches("0x").trim_start_matches("0X"))
        .map_err(|e| format!("signature hex: {e}"))?;
    if raw.len() != 65 {
        return Err(format!("signature must be 65 bytes, got {}", raw.len()));
    }
    let v = raw[64];
    let recovery_byte = match v {
        27 | 28 => v - 27,
        0 | 1 => v,
        other => return Err(format!("unexpected recovery id v={other}")),
    };
    let recovery_id =
        RecoveryId::try_from(recovery_byte).map_err(|e| format!("recovery id: {e}"))?;
    let signature =
        Signature::try_from(&raw[..64]).map_err(|e| format!("signature parse: {e}"))?;

    let digest = eip191_digest(message);
    let pubkey = VerifyingKey::recover_from_prehash(&digest, &signature, recovery_id)
        .map_err(|e| format!("recover: {e}"))?;

    let encoded = pubkey.to_encoded_point(false);
    let pubkey_bytes = &encoded.as_bytes()[1..];
    let mut hasher = Keccak256::new();
    hasher.update(pubkey_bytes);
    let hash = hasher.finalize();
    Ok(format!("0x{}", hex::encode(&hash[12..])))
}

fn register_message(name: &str, databox_type: &str, region: &str, timestamp: i64) -> String {
    format!("{DATABOX_AUTH_DOMAIN}\nregister\n{name}\n{databox_type}\n{region}\n{timestamp}")
}

fn heartbeat_message(id: &str, timestamp: i64) -> String {
    format!("{DATABOX_AUTH_DOMAIN}\nheartbeat\n{id}\n{timestamp}")
}

fn deregister_message(id: &str, timestamp: i64) -> String {
    format!("{DATABOX_AUTH_DOMAIN}\nderegister\n{id}\n{timestamp}")
}

fn verify_signature(
    address: &str,
    message: &str,
    timestamp: i64,
    signature_hex: &str,
    now: i64,
) -> Result<String, String> {
    if (now - timestamp).abs() > DATABOX_AUTH_WINDOW_SECS {
        return Err(format!(
            "timestamp outside ±{DATABOX_AUTH_WINDOW_SECS}s freshness window — sign again"
        ));
    }
    let claimed = address.to_lowercase();
    if !claimed.starts_with("0x") || claimed.len() != 42 {
        return Err("owner_address must be a 0x-prefixed 20-byte hex address".into());
    }
    let recovered = recover_signer(message.as_bytes(), signature_hex)?;
    if recovered != claimed {
        return Err("signature does not match the claimed owner_address".into());
    }
    Ok(claimed)
}

/// Deterministic databox id from `(owner_address, name)` so a re-submitted
/// registration from the same owner with the same name upserts in place
/// rather than creating a duplicate. Distinct owners or names always
/// produce distinct ids (keccak256, not a naive concatenation hash).
fn compute_databox_id(owner_address: &str, name: &str) -> String {
    let mut hasher = Keccak256::new();
    hasher.update(owner_address.to_lowercase().as_bytes());
    hasher.update(b"|");
    hasher.update(name.trim().to_lowercase().as_bytes());
    let hash = hasher.finalize();
    format!("dbx-{}", hex::encode(&hash[..16]))
}

// ============================================================================
// Persistence — a single mutable JSONL snapshot, rewritten atomically on
// every register/heartbeat/deregister. Same idiom as
// governance_votes.rs::{load,save}_projects_local.
// ============================================================================

static DATABOXES_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn databoxes_path() -> String {
    std::env::var("DATABOXES_PATH").unwrap_or_else(|_| "/opt/datachain-rope/databoxes.jsonl".into())
}

/// The rope wallet whose personal-ledger string anchors every
/// `DataboxRegistered` / `DataboxDeregistered` event, making the Global
/// Databox Network membership durable, replicated, and auditable on
/// dcscan.io. Distinct from the "Deploy a Node" (`…d001`) and governance
/// vote/cause (`…d002`) ledger wallets so the three event streams never
/// interleave.
fn databox_ledger_wallet() -> String {
    std::env::var("DATABOX_LEDGER_WALLET")
        .unwrap_or_else(|_| "0x000000000000000000000000000000000000d003".to_string())
}

/// Transport-layer errors (pooled-connection races against rope-node's
/// HTTP server — "connection closed before message completed" / "connection
/// reset by peer") are common on a fresh connection pool and are NOT a
/// real outage; retry once on a fresh connection before giving up. Mirrors
/// the pattern already deployed in `validation-agent`/`insurance-agent`
/// (see `handover-canonical-agents-live-from-rope-2026-05-05.mdc`).
async fn post_rpc_with_retry(state: &Arc<AppState>, rpc: &str, body: &Value) -> Result<Value, String> {
    for attempt in 0..2 {
        match state.http_client.post(rpc).json(body).send().await {
            Ok(resp) => {
                return resp
                    .json::<Value>()
                    .await
                    .map_err(|e| format!("unreadable rope-node response: {e}"));
            }
            Err(e) => {
                if attempt == 0 {
                    tracing::warn!("databox anchor transport error, retrying once: {}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
                return Err(format!("rope-node unreachable after retry: {e}"));
            }
        }
    }
    unreachable!()
}

async fn anchor_databox_event(
    state: &Arc<AppState>,
    interaction_type: &str,
    record: &Value,
    metadata: Value,
) -> Option<String> {
    let wallet = databox_ledger_wallet();
    let rpc = state.rpc_url_active().to_string();

    let create = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "rope_createPersonalLedger",
        "params": [wallet],
    });
    let _ = post_rpc_with_retry(state, &rpc, &create).await;

    let append = json!({
        "jsonrpc": "2.0", "id": 2,
        "method": "rope_appendToLedger",
        "params": [wallet, {
            "interaction_type": interaction_type,
            "description": record.to_string(),
            "metadata": metadata,
        }],
    });
    match post_rpc_with_retry(state, &rpc, &append).await {
        Ok(body) => {
            if let Some(hash) = body
                .get("result")
                .and_then(|r| r.get("hash"))
                .and_then(|h| h.as_str())
            {
                tracing::info!(
                    "databox event anchored on rope: type={} knot={}",
                    interaction_type,
                    hash
                );
                return Some(hash.to_string());
            }
            tracing::warn!("databox anchor rejected by rope-node: {}", body);
            None
        }
        Err(e) => {
            tracing::warn!("databox anchor failed: {}", e);
            None
        }
    }
}

fn load_jsonl_blocking(path: &str) -> Vec<Value> {
    std::fs::read_to_string(path)
        .map(|content| {
            content
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

async fn load_databoxes_local() -> Vec<Value> {
    let path = databoxes_path();
    tokio::task::spawn_blocking(move || load_jsonl_blocking(&path))
        .await
        .unwrap_or_default()
}

async fn save_databoxes_local(list: &[Value]) -> std::io::Result<()> {
    let path = databoxes_path();
    let lines: String = list.iter().map(|r| format!("{r}\n")).collect();
    tokio::task::spawn_blocking(move || {
        let _guard = DATABOXES_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, &lines).and_then(|_| std::fs::rename(&tmp, &path))
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
}

/// Rebuild the local cache from the rope ledger when the local file is
/// missing/empty (fresh node, disk loss, bootstrap). Folds
/// `DataboxRegistered` then `DataboxDeregistered` events in chain order.
/// Heartbeat history is NOT recoverable this way (by design — heartbeats
/// are not anchored), so a rebuilt entry shows as unheartbeated until the
/// operator's node pings again; this is honest, not a regression.
async fn rebuild_databoxes_from_rope(state: &Arc<AppState>) -> Vec<Value> {
    let wallet = databox_ledger_wallet();
    let rpc = state.rpc_url_active().to_string();
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "rope_repatriatePersonalLedger",
        "params": [wallet, {"decrypt": true}],
    });
    let body: Value = match post_rpc_with_retry(state, &rpc, &req).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("databox registry rebuild-from-rope failed: {}", e);
            return Vec::new();
        }
    };
    let Some(fragments) = body
        .get("result")
        .and_then(|r| r.get("fragments"))
        .and_then(|f| f.as_array())
        .cloned()
    else {
        return Vec::new();
    };

    let mut boxes: Vec<Value> = Vec::new();
    for frag in &fragments {
        let Some(interaction) = frag.get("interaction").filter(|i| !i.is_null()) else {
            continue;
        };
        let itype = interaction
            .get("interaction_type")
            .map(|t| t.to_string())
            .unwrap_or_default();
        let Some(desc) = interaction.get("description").and_then(|d| d.as_str()) else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<Value>(desc) else {
            continue;
        };
        if itype.contains("DataboxRegistered") {
            let Some(id) = payload.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            if let Some(existing) = boxes
                .iter_mut()
                .find(|b| b.get("id").and_then(|v| v.as_str()) == Some(id))
            {
                *existing = payload;
            } else {
                boxes.push(payload);
            }
        } else if itype.contains("DataboxDeregistered") {
            let Some(id) = payload.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            boxes.retain(|b| b.get("id").and_then(|v| v.as_str()) != Some(id));
        }
    }

    if !boxes.is_empty() {
        let _ = save_databoxes_local(&boxes).await;
        tracing::info!(
            "databox registry rebuilt from rope: {} databox(es) recovered",
            boxes.len()
        );
    }
    boxes
}

async fn load_databoxes(state: &Arc<AppState>) -> Vec<Value> {
    let local = load_databoxes_local().await;
    if local.is_empty() {
        rebuild_databoxes_from_rope(state).await
    } else {
        local
    }
}

// ============================================================================
// Live-view computation — status/liveness derived at read time, never
// persisted as a potentially-stale flag.
// ============================================================================

fn attach_live_view(mut entry: Value, now: i64) -> Value {
    let last_heartbeat_at = entry.get("last_heartbeat_at").and_then(|v| v.as_i64());
    let status = match last_heartbeat_at {
        Some(t) if now - t <= HEARTBEAT_TTL_SECS => "online",
        Some(_) => "offline",
        None => "registered",
    };
    let seconds_since_heartbeat = last_heartbeat_at.map(|t| (now - t).max(0));

    if let Some(obj) = entry.as_object_mut() {
        obj.insert("status".into(), json!(status));
        obj.insert(
            "secondsSinceLastHeartbeat".into(),
            json!(seconds_since_heartbeat),
        );
        obj.insert("heartbeatTtlSecs".into(), json!(HEARTBEAT_TTL_SECS));
    }
    entry
}

fn type_label(t: &str) -> &'static str {
    DATABOX_TYPES
        .iter()
        .find(|(k, _)| *k == t)
        .map(|(_, label)| *label)
        .unwrap_or("Unknown")
}

// ============================================================================
// HTTP handlers.
// ============================================================================

#[derive(Deserialize)]
pub struct ListParams {
    #[serde(rename = "type")]
    type_filter: Option<String>,
    status: Option<String>,
    region: Option<String>,
    page: Option<u32>,
    limit: Option<u32>,
}

/// `GET /api/v1/databoxes` — real, persisted, chain-anchored Global
/// Databox Network membership. Optional `?type=&status=&region=` filters.
pub async fn list_databoxes(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Json<Value> {
    let now = chrono::Utc::now().timestamp();
    let page = params.page.unwrap_or(1).max(1) as usize;
    let limit = params.limit.unwrap_or(50).clamp(1, 500) as usize;

    let mut boxes: Vec<Value> = load_databoxes(&state)
        .await
        .into_iter()
        .map(|b| attach_live_view(b, now))
        .collect();

    if let Some(t) = params.type_filter.as_deref() {
        boxes.retain(|b| b.get("databox_type").and_then(|v| v.as_str()) == Some(t));
    }
    if let Some(s) = params.status.as_deref() {
        boxes.retain(|b| b.get("status").and_then(|v| v.as_str()) == Some(s));
    }
    if let Some(r) = params.region.as_deref() {
        let r = r.to_lowercase();
        boxes.retain(|b| {
            b.get("region")
                .and_then(|v| v.as_str())
                .map(|v| v.to_lowercase() == r)
                .unwrap_or(false)
        });
    }

    boxes.sort_by_key(|b| std::cmp::Reverse(b.get("registered_at").and_then(|v| v.as_i64()).unwrap_or(0)));
    let total = boxes.len();
    let start = (page - 1) * limit;
    let page_items: Vec<Value> = boxes.into_iter().skip(start).take(limit).collect();

    Json(json!({
        "databoxes": page_items,
        "totalCount": total,
        "pagination": { "page": page, "limit": limit, "total": total },
        "note": if total == 0 {
            "Global Databox Network has no registered nodes yet — see POST /api/v1/databoxes/register to add one.".to_string()
        } else {
            format!("{total} live-registered node(s) — data is self-reported and signature-verified, not fabricated.")
        },
    }))
}

/// `GET /api/v1/databoxes/types` — per-type breakdown across the whole
/// registry. Satisfies discovery of "each type of data source" without
/// requiring the caller to know the taxonomy in advance.
pub async fn databox_types(State(state): State<Arc<AppState>>) -> Json<Value> {
    let boxes = load_databoxes(&state).await;
    let now = chrono::Utc::now().timestamp();
    let types: Vec<Value> = DATABOX_TYPES
        .iter()
        .map(|(key, label)| {
            let matching: Vec<&Value> = boxes
                .iter()
                .filter(|b| b.get("databox_type").and_then(|v| v.as_str()) == Some(*key))
                .collect();
            let online = matching
                .iter()
                .filter(|b| {
                    b.get("last_heartbeat_at")
                        .and_then(|v| v.as_i64())
                        .map(|t| now - t <= HEARTBEAT_TTL_SECS)
                        .unwrap_or(false)
                })
                .count();
            json!({
                "type": key,
                "label": label,
                "count": matching.len(),
                "online": online,
                "route": format!("/api/v1/databoxes/type/{key}"),
            })
        })
        .collect();
    Json(json!({
        "types": types,
        "totalDataboxes": boxes.len(),
    }))
}

/// `GET /api/v1/databoxes/type/:type` — dedicated route per data-source
/// type (e.g. `/api/v1/databoxes/type/witness`, `.../storage_ledger`).
pub async fn databoxes_by_type(
    State(state): State<Arc<AppState>>,
    Path(type_param): Path<String>,
) -> (StatusCode, Json<Value>) {
    if !is_valid_databox_type(&type_param) {
        let valid: Vec<&str> = DATABOX_TYPES.iter().map(|(k, _)| *k).collect();
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": format!("unknown databox type '{type_param}'"),
                "validTypes": valid,
            })),
        );
    }
    let now = chrono::Utc::now().timestamp();
    let boxes: Vec<Value> = load_databoxes(&state)
        .await
        .into_iter()
        .filter(|b| b.get("databox_type").and_then(|v| v.as_str()) == Some(type_param.as_str()))
        .map(|b| attach_live_view(b, now))
        .collect();
    let total = boxes.len();
    (
        StatusCode::OK,
        Json(json!({
            "type": type_param,
            "label": type_label(&type_param),
            "databoxes": boxes,
            "totalCount": total,
        })),
    )
}

/// `GET /api/v1/databoxes/:id`
pub async fn get_databox(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let now = chrono::Utc::now().timestamp();
    let boxes = load_databoxes(&state).await;
    match boxes.into_iter().find(|b| b.get("id").and_then(|v| v.as_str()) == Some(id.as_str())) {
        Some(entry) => (StatusCode::OK, Json(json!({ "found": true, "databox": attach_live_view(entry, now) }))),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "id": id,
                "found": false,
                "error": "no databox with this id is registered on the Global Databox Network.",
            })),
        ),
    }
}

/// `GET /api/v1/databoxes/map` — geo markers for entries that have
/// self-reported real coordinates only. Never synthesizes geography.
pub async fn databox_map(State(state): State<Arc<AppState>>) -> Json<Value> {
    let now = chrono::Utc::now().timestamp();
    let boxes = load_databoxes(&state).await;
    let markers: Vec<Value> = boxes
        .into_iter()
        .filter(|b| b.get("lat").and_then(|v| v.as_f64()).is_some() && b.get("lon").and_then(|v| v.as_f64()).is_some())
        .map(|b| attach_live_view(b, now))
        .map(|b| {
            json!({
                "id": b.get("id"),
                "name": b.get("name"),
                "type": b.get("databox_type"),
                "region": b.get("region"),
                "city": b.get("city"),
                "country": b.get("country"),
                "lat": b.get("lat"),
                "lon": b.get("lon"),
                "status": b.get("status"),
            })
        })
        .collect();
    Json(json!({
        "markers": markers,
        "totalDataboxes": markers.len(),
        "note": if markers.is_empty() {
            "No registered databox has reported geographic coordinates yet — the map plots only self-reported, signature-verified locations.".to_string()
        } else {
            format!("{} node(s) with self-reported coordinates.", markers.len())
        },
    }))
}

#[derive(Deserialize)]
pub struct RegisterRequest {
    owner_address: String,
    name: String,
    databox_type: String,
    region: Option<String>,
    city: Option<String>,
    country: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    endpoint_url: Option<String>,
    capacity_gb: Option<f64>,
    #[serde(default)]
    metadata: Value,
    timestamp: i64,
    signature: String,
}

/// `POST /api/v1/databoxes/register` — signature-verified self-service
/// registration. Re-submitting with the same `(owner_address, name)`
/// upserts the existing entry (e.g. to update coordinates or capacity).
pub async fn register_databox(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RegisterRequest>,
) -> (StatusCode, Json<Value>) {
    let name = payload.name.trim();
    if name.is_empty() || name.len() > 100 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "name is required (max 100 chars)" })),
        );
    }
    if !is_valid_databox_type(&payload.databox_type) {
        let valid: Vec<&str> = DATABOX_TYPES.iter().map(|(k, _)| *k).collect();
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": format!("databox_type must be one of {valid:?}"),
            })),
        );
    }
    if let (Some(lat), Some(lon)) = (payload.lat, payload.lon) {
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": "lat must be -90..90 and lon -180..180" })),
            );
        }
    }
    if let Some(cap) = payload.capacity_gb {
        if cap < 0.0 || cap > 1_000_000_000.0 {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": "capacity_gb out of range" })),
            );
        }
    }

    // CERBER WATCH — every free-text field here is persisted and later
    // rendered on the public databox map/registry page. `owner_address`
    // is checked against the blocklist below, once signature verification
    // has established it is the real owner (proof-of-key, not just a
    // claimed value in the request body).
    if let Err(resp) = crate::security_guard::validate_fields(&[
        ("name", name),
        ("databox_type", payload.databox_type.as_str()),
        ("region", payload.region.as_deref().unwrap_or("")),
        ("city", payload.city.as_deref().unwrap_or("")),
        ("country", payload.country.as_deref().unwrap_or("")),
        ("endpoint_url", payload.endpoint_url.as_deref().unwrap_or("")),
    ]) {
        return resp;
    }

    let region = payload.region.clone().unwrap_or_default();
    let now = chrono::Utc::now().timestamp();
    let message = register_message(name, &payload.databox_type, &region, payload.timestamp);
    let owner = match verify_signature(&payload.owner_address, &message, payload.timestamp, &payload.signature, now) {
        Ok(addr) => addr,
        Err(e) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": format!("signature verification failed: {e}") })),
            )
        }
    };

    // CERBER WATCH — `blocked_signers` gate (finding H1/C4), applied to
    // the signature-verified owner (not the unverified request field).
    if let Err(resp) = crate::security_guard::check_signer(&owner) {
        return resp;
    }

    let id = compute_databox_id(&owner, name);
    let mut boxes = load_databoxes(&state).await;
    let existing = boxes.iter().position(|b| b.get("id").and_then(|v| v.as_str()) == Some(id.as_str()));
    if let Some(idx) = existing {
        let same_owner = boxes[idx].get("owner_address").and_then(|v| v.as_str()) == Some(owner.as_str());
        if !same_owner {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "success": false, "error": "a databox with this name is already registered by a different owner" })),
            );
        }
    }

    let registered_at = existing
        .and_then(|idx| boxes[idx].get("registered_at").and_then(|v| v.as_i64()))
        .unwrap_or(now);
    let last_heartbeat_at = existing.and_then(|idx| boxes[idx].get("last_heartbeat_at").and_then(|v| v.as_i64()));
    let heartbeat_count = existing
        .and_then(|idx| boxes[idx].get("heartbeat_count").and_then(|v| v.as_u64()))
        .unwrap_or(0);

    let record = json!({
        "id": id,
        "owner_address": owner,
        "name": name,
        "databox_type": payload.databox_type,
        "region": payload.region,
        "city": payload.city,
        "country": payload.country,
        "lat": payload.lat,
        "lon": payload.lon,
        "endpoint_url": payload.endpoint_url,
        "capacity_gb": payload.capacity_gb,
        "metadata": payload.metadata,
        "registered_at": registered_at,
        "last_heartbeat_at": last_heartbeat_at,
        "heartbeat_count": heartbeat_count,
        "updated_at": now,
    });

    match existing {
        Some(idx) => boxes[idx] = record.clone(),
        None => boxes.push(record.clone()),
    }
    if let Err(e) = save_databoxes_local(&boxes).await {
        tracing::error!("failed to persist databox registry: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "failed to persist registration" })),
        );
    }

    let hash = anchor_databox_event(
        &state,
        "DataboxRegistered",
        &record,
        json!({ "id": id, "owner_address": owner, "databox_type": payload.databox_type }),
    )
    .await;

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "databox": attach_live_view(record, now),
            "anchorTxHash": hash,
        })),
    )
}

#[derive(Deserialize)]
pub struct HeartbeatRequest {
    owner_address: String,
    timestamp: i64,
    signature: String,
    #[serde(default)]
    metrics: Value,
}

/// `POST /api/v1/databoxes/:id/heartbeat` — signature-verified liveness
/// ping. Updates `last_heartbeat_at` + `heartbeat_count` locally; NOT
/// anchored on-chain (heartbeats are high-frequency, would spam the
/// ledger — only register/deregister are anchored).
pub async fn heartbeat_databox(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<HeartbeatRequest>,
) -> (StatusCode, Json<Value>) {
    let now = chrono::Utc::now().timestamp();
    let message = heartbeat_message(&id, payload.timestamp);
    let owner = match verify_signature(&payload.owner_address, &message, payload.timestamp, &payload.signature, now) {
        Ok(addr) => addr,
        Err(e) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": format!("signature verification failed: {e}") })),
            )
        }
    };

    // CERBER WATCH — `blocked_signers` gate (finding H1/C4).
    if let Err(resp) = crate::security_guard::check_signer(&owner) {
        return resp;
    }

    let mut boxes = load_databoxes(&state).await;
    let Some(idx) = boxes.iter().position(|b| b.get("id").and_then(|v| v.as_str()) == Some(id.as_str())) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "no databox with this id is registered" })),
        );
    };
    if boxes[idx].get("owner_address").and_then(|v| v.as_str()) != Some(owner.as_str()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "success": false, "error": "signature does not match the registered owner of this databox" })),
        );
    }

    let count = boxes[idx].get("heartbeat_count").and_then(|v| v.as_u64()).unwrap_or(0);
    if let Some(obj) = boxes[idx].as_object_mut() {
        obj.insert("last_heartbeat_at".into(), json!(now));
        obj.insert("heartbeat_count".into(), json!(count + 1));
        if !payload.metrics.is_null() {
            obj.insert("last_metrics".into(), payload.metrics.clone());
        }
    }
    let updated = boxes[idx].clone();

    if let Err(e) = save_databoxes_local(&boxes).await {
        tracing::error!("failed to persist databox heartbeat: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "failed to persist heartbeat" })),
        );
    }

    (StatusCode::OK, Json(json!({ "success": true, "databox": attach_live_view(updated, now) })))
}

#[derive(Deserialize)]
pub struct DeregisterRequest {
    owner_address: String,
    timestamp: i64,
    signature: String,
}

/// `POST /api/v1/databoxes/:id/deregister` — signature-verified removal.
pub async fn deregister_databox(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<DeregisterRequest>,
) -> (StatusCode, Json<Value>) {
    let now = chrono::Utc::now().timestamp();
    let message = deregister_message(&id, payload.timestamp);
    let owner = match verify_signature(&payload.owner_address, &message, payload.timestamp, &payload.signature, now) {
        Ok(addr) => addr,
        Err(e) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": format!("signature verification failed: {e}") })),
            )
        }
    };

    // CERBER WATCH — `blocked_signers` gate (finding H1/C4).
    if let Err(resp) = crate::security_guard::check_signer(&owner) {
        return resp;
    }

    let mut boxes = load_databoxes(&state).await;
    let Some(idx) = boxes.iter().position(|b| b.get("id").and_then(|v| v.as_str()) == Some(id.as_str())) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "no databox with this id is registered" })),
        );
    };
    if boxes[idx].get("owner_address").and_then(|v| v.as_str()) != Some(owner.as_str()) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "success": false, "error": "signature does not match the registered owner of this databox" })),
        );
    }
    let removed = boxes.remove(idx);
    if let Err(e) = save_databoxes_local(&boxes).await {
        tracing::error!("failed to persist databox deregistration: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "failed to persist deregistration" })),
        );
    }

    let hash = anchor_databox_event(
        &state,
        "DataboxDeregistered",
        &json!({ "id": id }),
        json!({ "id": id, "owner_address": owner }),
    )
    .await;

    (
        StatusCode::OK,
        Json(json!({ "success": true, "removed": removed, "anchorTxHash": hash })),
    )
}
