//! Governance Voting & Cause Platform — Phase 1 (real persistence + real
//! single-chain balance checks).
//!
//! Per `docs/GOVERNANCE_VOTING_CAUSE_PLATFORM_SPEC_V1.md`, this module
//! replaces the previously-mocked `/api/v1/projects*` and
//! `/api/v1/votes*` handlers with production data:
//!
//!   - Project submissions are persisted to a durable JSONL queue AND
//!     anchored as `ProjectSubmitted` / `ProjectReviewed` knots on the
//!     governance ledger wallet's Quipu Canon v1.2 string, exactly like
//!     the "Deploy a Node" request queue (see `node_requests_path()` in
//!     `main.rs`). The JSONL file is a local read cache that is
//!     rebuildable from the chain at any time.
//!   - Ballots are append-only, signature-verified (EIP-191
//!     `personal_sign`, domain-tagged `DCROPE-VOTE-AUTH` so a signature
//!     captured here can never be replayed against Datachain ID or the
//!     EDC console), and weighted by the voter's REAL native DC FAT
//!     balance on Datachain Rope at the time of the vote (`eth_getBalance`
//!     against the live RPC fleet — single-chain for Phase 1; Phase 3 of
//!     the spec adds cross-chain (Ethereum + XDC) aggregation).
//!   - Double-voting is rejected. Voting below the minimum FAT balance
//!     is rejected. Votes cast outside the `voting` window are rejected.
//!   - Project status transitions (`pending_review` → `voting` →
//!     `approved`/`rejected`) are computed deterministically: admin
//!     review (`X-Admin-Token`-gated, mirrors `NODE_REQUESTS_ADMIN_TOKEN`)
//!     opens the voting window; the terminal outcome is derived lazily
//!     from the real ballot tally against the quorum + approval
//!     threshold, matching the copy already shown on `/vote`
//!     ("51%+ votes with minimum 1,000,000 FAT participation").
//!
//! Explicitly OUT of scope for this module (left untouched in `main.rs`):
//! the Federation/Community "Deploy an Instance" demo (`/api/v1/federations*`,
//! `/api/v1/communities*`) — a separate, older feature area unrelated to
//! the project/cause voting platform this module implements.

use crate::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};
use std::sync::Arc;

// ============================================================================
// EIP-191 wallet-signature verification — domain `DCROPE-VOTE-AUTH`.
//
// Same k256/keccak construction as `rope-idp::walletsig` and rope-node's
// Phase-2 destructive-RPC verifier, reimplemented locally so rope-explorer
// does not need to depend on the rope-idp binary. The domain tag is
// distinct from `DATACHAIN-ID-AUTH` and `EDC-CONSOLE-AUTH`, so a signature
// captured on one surface can never be replayed on another.
// ============================================================================

const VOTE_AUTH_DOMAIN: &str = "DCROPE-VOTE-AUTH";
const VOTE_AUTH_WINDOW_SECS: i64 = 300;

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

/// Recover the Ethereum address (`0x…`, lowercase) that produced the
/// 65-byte `r||s||v` signature over `message` (EIP-191 wrapped).
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

/// The canonical message a wallet signs to cast a ballot on a project.
/// Binding the message to `project_id` + `vote_for` prevents a captured
/// signature from being replayed against a different project or flipped
/// to the opposite vote.
fn vote_message(project_id: &str, vote_for: bool, timestamp: i64) -> String {
    format!("{VOTE_AUTH_DOMAIN}\nvote\n{project_id}\n{vote_for}\n{timestamp}")
}

/// Full verification: freshness window + signature recovery + claimed
/// address equality. Returns the lowercase proven address.
fn verify_vote_signature(
    address: &str,
    project_id: &str,
    vote_for: bool,
    timestamp: i64,
    signature_hex: &str,
    now: i64,
) -> Result<String, String> {
    if (now - timestamp).abs() > VOTE_AUTH_WINDOW_SECS {
        return Err(format!(
            "timestamp outside ±{VOTE_AUTH_WINDOW_SECS}s freshness window — sign again"
        ));
    }
    let claimed = address.to_lowercase();
    if !claimed.starts_with("0x") || claimed.len() != 42 {
        return Err("voter_address must be a 0x-prefixed 20-byte hex address".into());
    }
    let message = vote_message(project_id, vote_for, timestamp);
    let recovered = recover_signer(message.as_bytes(), signature_hex)?;
    if recovered != claimed {
        return Err("signature does not match the claimed voter_address".into());
    }
    Ok(claimed)
}

// ============================================================================
// Real single-chain (Rope-native) FAT balance check.
// ============================================================================

/// Minimum native DC FAT balance (in whole FAT) required to cast a
/// ballot. Anti-spam floor, not a governance parameter — env-overridable.
fn min_fat_balance_to_vote() -> f64 {
    std::env::var("VOTE_MIN_FAT_BALANCE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0)
}

/// Minimum aggregate FAT weight (`votesFor + votesAgainst`) required for
/// a voting window to reach quorum. Matches the copy already shown on
/// `/vote` ("minimum 1,000,000 FAT participation").
fn min_quorum_weight_fat() -> f64 {
    std::env::var("VOTE_MIN_QUORUM_FAT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1_000_000.0)
}

/// Approval threshold as a fraction of `weight_for / (weight_for +
/// weight_against)`. Matches the copy already shown on `/vote` ("51%+
/// votes").
fn approval_threshold() -> f64 {
    std::env::var("VOTE_APPROVAL_THRESHOLD_BPS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .map(|bps| bps / 10_000.0)
        .unwrap_or(0.51)
}

fn voting_period_days() -> i64 {
    std::env::var("VOTING_PERIOD_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(7)
}

/// `eth_getBalance` against the live Rope RPC fleet, with failover across
/// every configured endpoint (not just the currently-"active" one) since
/// vote casting is low-frequency and correctness matters more than speed
/// here. Returns the balance in wei.
async fn fetch_fat_balance_wei(state: &Arc<AppState>, address: &str) -> Result<u128, String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getBalance",
        "params": [address, "latest"],
    });
    let mut last_err = "no RPC endpoint configured".to_string();
    for url in &state.rpc_urls {
        let resp = state
            .http_client
            .post(url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("{url}: {e}");
                continue;
            }
        };
        let parsed: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                last_err = format!("{url}: unreadable response: {e}");
                continue;
            }
        };
        if let Some(result) = parsed.get("result").and_then(|r| r.as_str()) {
            if let Ok(v) = u128::from_str_radix(result.trim_start_matches("0x"), 16) {
                return Ok(v);
            }
        }
        if let Some(err) = parsed.get("error") {
            last_err = format!("{url}: rpc error {err}");
            continue;
        }
    }
    Err(last_err)
}

fn wei_to_fat(wei: u128) -> f64 {
    wei as f64 / 1e18
}

// ============================================================================
// Persistence — projects (mutable, one JSON object per project, rewritten
// atomically on every state transition) + ballots (append-only).
// ============================================================================

static PROJECTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static BALLOTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn projects_path() -> String {
    std::env::var("PROJECTS_PATH").unwrap_or_else(|_| "/opt/datachain-rope/projects.jsonl".into())
}

fn ballots_path() -> String {
    std::env::var("PROJECT_BALLOTS_PATH")
        .unwrap_or_else(|_| "/opt/datachain-rope/project-ballots.jsonl".into())
}

/// The rope wallet whose personal-ledger string IS the governance
/// vote/cause queue. Every submission, review decision, and ballot is
/// anchored as a knot on this string via `rope_appendToLedger`, making
/// the queue durable, replicated across the fleet, and auditable on
/// dcscan.io. Distinct from the "Deploy a Node" queue wallet
/// (`…d001`) so the two event streams don't interleave.
fn governance_ledger_wallet() -> String {
    std::env::var("GOVERNANCE_LEDGER_WALLET")
        .unwrap_or_else(|_| "0x000000000000000000000000000000000000d002".to_string())
}

async fn anchor_governance_event(
    state: &Arc<AppState>,
    interaction_type: &str,
    record: &Value,
    metadata: Value,
) -> Option<String> {
    let wallet = governance_ledger_wallet();
    let rpc = state.rpc_url_active().to_string();

    let create = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "rope_createPersonalLedger",
        "params": [wallet],
    });
    let _ = state.http_client.post(&rpc).json(&create).send().await;

    let append = json!({
        "jsonrpc": "2.0", "id": 2,
        "method": "rope_appendToLedger",
        "params": [wallet, {
            "interaction_type": interaction_type,
            "description": record.to_string(),
            "metadata": metadata,
        }],
    });
    match state.http_client.post(&rpc).json(&append).send().await {
        Ok(resp) => match resp.json::<Value>().await {
            Ok(body) => {
                if let Some(hash) = body
                    .get("result")
                    .and_then(|r| r.get("hash"))
                    .and_then(|h| h.as_str())
                {
                    tracing::info!(
                        "governance event anchored on rope: type={} knot={}",
                        interaction_type,
                        hash
                    );
                    return Some(hash.to_string());
                }
                tracing::warn!("governance anchor rejected by rope-node: {}", body);
                None
            }
            Err(e) => {
                tracing::warn!("governance anchor: unreadable rope-node response: {}", e);
                None
            }
        },
        Err(e) => {
            tracing::warn!("governance anchor failed (rope-node unreachable): {}", e);
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

async fn load_projects_local() -> Vec<Value> {
    let path = projects_path();
    tokio::task::spawn_blocking(move || load_jsonl_blocking(&path))
        .await
        .unwrap_or_default()
}

async fn save_projects_local(list: &[Value]) -> std::io::Result<()> {
    let path = projects_path();
    let lines: String = list.iter().map(|r| format!("{r}\n")).collect();
    tokio::task::spawn_blocking(move || {
        let _guard = PROJECTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, &lines).and_then(|_| std::fs::rename(&tmp, &path))
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
}

async fn load_ballots_local() -> Vec<Value> {
    let path = ballots_path();
    tokio::task::spawn_blocking(move || load_jsonl_blocking(&path))
        .await
        .unwrap_or_default()
}

async fn append_ballot_local(ballot: &Value) -> std::io::Result<()> {
    let path = ballots_path();
    let line = format!("{ballot}\n");
    tokio::task::spawn_blocking(move || {
        let _guard = BALLOTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::io::Write;
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        f.write_all(line.as_bytes())?;
        f.flush()
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
}

/// Rebuild the local project cache from the rope by folding
/// `ProjectSubmitted` then `ProjectReviewed` events, in chain order, when
/// the local file is missing/empty (fresh node, disk loss, bootstrap).
async fn rebuild_projects_from_rope(state: &Arc<AppState>) -> Vec<Value> {
    let fragments = match repatriate_governance_ledger(state).await {
        Some(f) => f,
        None => return Vec::new(),
    };

    let mut projects: Vec<Value> = Vec::new();
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
        if itype.contains("ProjectSubmitted") {
            if payload.get("id").is_some() {
                projects.push(payload);
            }
        } else if itype.contains("ProjectReviewed") {
            let Some(id) = payload.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            if let Some(existing) = projects
                .iter_mut()
                .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(id))
            {
                if let Some(obj) = existing.as_object_mut() {
                    if let Some(updates) = payload.get("updates").and_then(|u| u.as_object()) {
                        for (k, v) in updates {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }
    }

    if !projects.is_empty() {
        let _ = save_projects_local(&projects).await;
        tracing::info!(
            "project queue rebuilt from rope: {} project(s) recovered",
            projects.len()
        );
    }
    projects
}

/// Rebuild the local ballots cache from the rope by extracting every
/// `ProjectVoteCast` knot.
async fn rebuild_ballots_from_rope(state: &Arc<AppState>) -> Vec<Value> {
    let fragments = match repatriate_governance_ledger(state).await {
        Some(f) => f,
        None => return Vec::new(),
    };

    let mut ballots: Vec<Value> = Vec::new();
    for frag in &fragments {
        let Some(interaction) = frag.get("interaction").filter(|i| !i.is_null()) else {
            continue;
        };
        let is_ballot = interaction
            .get("interaction_type")
            .map(|t| t.to_string().contains("ProjectVoteCast"))
            .unwrap_or(false);
        if !is_ballot {
            continue;
        }
        if let Some(desc) = interaction.get("description").and_then(|d| d.as_str()) {
            if let Ok(ballot) = serde_json::from_str::<Value>(desc) {
                if ballot.get("project_id").is_some() {
                    ballots.push(ballot);
                }
            }
        }
    }

    if !ballots.is_empty() {
        let path = ballots_path();
        let lines: String = ballots.iter().map(|r| format!("{r}\n")).collect();
        let _ = tokio::task::spawn_blocking(move || {
            let _guard = BALLOTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(parent) = std::path::Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let tmp = format!("{path}.tmp");
            std::fs::write(&tmp, &lines).and_then(|_| std::fs::rename(&tmp, &path))
        })
        .await;
        tracing::info!(
            "ballot queue rebuilt from rope: {} ballot(s) recovered",
            ballots.len()
        );
    }
    ballots
}

async fn repatriate_governance_ledger(state: &Arc<AppState>) -> Option<Vec<Value>> {
    let wallet = governance_ledger_wallet();
    let rpc = state.rpc_url_active().to_string();
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "rope_repatriatePersonalLedger",
        "params": [wallet, {"decrypt": true}],
    });
    let body: Value = state
        .http_client
        .post(&rpc)
        .json(&req)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    body.get("result")
        .and_then(|r| r.get("fragments"))
        .and_then(|f| f.as_array())
        .cloned()
}

async fn load_projects(state: &Arc<AppState>) -> Vec<Value> {
    let local = load_projects_local().await;
    if local.is_empty() {
        rebuild_projects_from_rope(state).await
    } else {
        local
    }
}

async fn load_ballots(state: &Arc<AppState>) -> Vec<Value> {
    let local = load_ballots_local().await;
    if local.is_empty() {
        rebuild_ballots_from_rope(state).await
    } else {
        local
    }
}

// ============================================================================
// Tally + effective status computation.
// ============================================================================

#[derive(Default, Clone, Copy)]
struct Tally {
    votes_for: u64,
    votes_against: u64,
    weight_for: f64,
    weight_against: f64,
}

fn tally_for_project(project_id: &str, ballots: &[Value]) -> Tally {
    let mut t = Tally::default();
    for b in ballots {
        if b.get("project_id").and_then(|v| v.as_str()) != Some(project_id) {
            continue;
        }
        let vote_for = b.get("vote_for").and_then(|v| v.as_bool()).unwrap_or(false);
        let weight = b.get("weight_fat").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if vote_for {
            t.votes_for += 1;
            t.weight_for += weight;
        } else {
            t.votes_against += 1;
            t.weight_against += weight;
        }
    }
    t
}

/// Computes the effective (display) status of a project, deriving the
/// terminal `approved`/`rejected` outcome lazily from the real ballot
/// tally once the voting window has closed. Never mutates storage —
/// pure function of stored state + now.
fn effective_status(raw_status: &str, voting_ends_at: Option<i64>, tally: &Tally, now: i64) -> String {
    if raw_status != "voting" {
        return raw_status.to_string();
    }
    let Some(ends_at) = voting_ends_at else {
        return "voting".to_string();
    };
    if now < ends_at {
        return "voting".to_string();
    }
    let total_weight = tally.weight_for + tally.weight_against;
    if total_weight < min_quorum_weight_fat() {
        return "rejected_no_quorum".to_string();
    }
    let approval_ratio = tally.weight_for / total_weight;
    if approval_ratio >= approval_threshold() {
        "approved".to_string()
    } else {
        "rejected".to_string()
    }
}

fn attach_live_view(mut project: Value, ballots: &[Value], now: i64) -> Value {
    let id = project
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tally = tally_for_project(&id, ballots);
    let raw_status = project
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending_review")
        .to_string();
    let voting_ends_at = project.get("votingEndsAt").and_then(|v| v.as_i64());
    let status = effective_status(&raw_status, voting_ends_at, &tally, now);

    if let Some(obj) = project.as_object_mut() {
        obj.insert("status".into(), json!(status));
        obj.insert("votesFor".into(), json!(tally.votes_for));
        obj.insert("votesAgainst".into(), json!(tally.votes_against));
        obj.insert("weightFor".into(), json!(tally.weight_for));
        obj.insert("weightAgainst".into(), json!(tally.weight_against));
        obj.insert("requiredQuorumFat".into(), json!(min_quorum_weight_fat()));
        obj.insert(
            "approvalThreshold".into(),
            json!(approval_threshold()),
        );
    }
    project
}

// ============================================================================
// HTTP handlers.
// ============================================================================

#[derive(Deserialize)]
pub struct PageParams {
    page: Option<u32>,
    limit: Option<u32>,
}

/// `GET /api/v1/projects` — real, persisted, chain-anchored project
/// submissions with a live-computed status/tally view.
pub async fn list_projects(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PageParams>,
) -> Json<Value> {
    let page = params.page.unwrap_or(1).max(1) as usize;
    let limit = params.limit.unwrap_or(20).clamp(1, 200) as usize;
    let now = chrono::Utc::now().timestamp();

    let mut projects = load_projects(&state).await;
    // Newest first.
    projects.sort_by_key(|p| std::cmp::Reverse(p.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0)));
    let ballots = load_ballots(&state).await;
    let total = projects.len();

    let start = (page - 1) * limit;
    let page_items: Vec<Value> = projects
        .into_iter()
        .skip(start)
        .take(limit)
        .map(|p| attach_live_view(p, &ballots, now))
        .collect();

    Json(json!({
        "projects": page_items,
        "pagination": { "page": page, "limit": limit, "total": total }
    }))
}

/// `GET /api/v1/projects/voting` — projects currently accepting ballots.
pub async fn voting_projects(State(state): State<Arc<AppState>>) -> Json<Value> {
    let now = chrono::Utc::now().timestamp();
    let projects = load_projects(&state).await;
    let ballots = load_ballots(&state).await;

    let live: Vec<Value> = projects
        .into_iter()
        .map(|p| attach_live_view(p, &ballots, now))
        .filter(|p| p.get("status").and_then(|v| v.as_str()) == Some("voting"))
        .collect();

    Json(json!({ "votingProjects": live.len(), "total": live.len(), "projects": live }))
}

/// `GET /api/v1/projects/:id`
pub async fn get_project(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> (StatusCode, Json<Value>) {
    let now = chrono::Utc::now().timestamp();
    let projects = load_projects(&state).await;
    let Some(project) = projects
        .into_iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "project not found" })),
        );
    };
    let ballots = load_ballots(&state).await;
    (StatusCode::OK, Json(attach_live_view(project, &ballots, now)))
}

#[derive(Deserialize)]
pub struct SubmitProjectRequest {
    name: String,
    tagline: Option<String>,
    description: String,
    category: String,
    stage: String,
    organization_type: String,
    organization_name: Option<String>,
    submitter_name: Option<String>,
    submitter_email: Option<String>,
    #[serde(default)]
    tech_stack: Vec<String>,
    architecture_description: Option<String>,
    #[serde(default)]
    features: Vec<Value>,
    use_cases: Option<String>,
    target_users: Option<String>,
    #[serde(default)]
    requires_ai_testimony: bool,
    whitepaper_url: Option<String>,
    documentation_url: Option<String>,
    github_url: Option<String>,
    website_url: Option<String>,
    demo_url: Option<String>,
    #[serde(default)]
    team_members: Vec<Value>,
    #[serde(default)]
    milestones: Vec<Value>,
    #[serde(default)]
    funding_requested: u64,
    #[serde(default = "default_funding_currency")]
    funding_currency: String,
    funding_breakdown: Option<String>,
}

fn default_funding_currency() -> String {
    "FAT".to_string()
}

/// `POST /api/v1/projects` — real submission: validated, durably
/// persisted, anchored on the rope, and (best-effort) confirmed by email
/// via the same SendGrid-backed mailer used by the contact form.
pub async fn submit_project(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SubmitProjectRequest>,
) -> (StatusCode, Json<Value>) {
    let name = payload.name.trim();
    if name.is_empty() || name.len() > 200 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "name is required (max 200 chars)" })),
        );
    }
    if payload.description.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "description is required" })),
        );
    }
    if let Some(email) = payload.submitter_email.as_deref() {
        if !email.is_empty() && (!email.contains('@') || !email.contains('.') || email.len() > 254) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": "submitter_email must be a valid email" })),
            );
        }
    }

    // CERBER WATCH — every free-text field on this form is durably
    // persisted (`load_projects_local`/`save_projects_local`) and later
    // rendered on the public governance/voting pages, so it is both a
    // stored-XSS and a stored-SQLi-signal (against future SQL-backed
    // storage) surface. `source_code`-style fields are deliberately never
    // passed through this gate (see `security_guard` module docs) — none
    // of these fields carry raw source code.
    if let Err(resp) = crate::security_guard::validate_fields(&[
        ("name", name),
        ("tagline", payload.tagline.as_deref().unwrap_or("")),
        ("description", payload.description.as_str()),
        ("category", payload.category.as_str()),
        ("stage", payload.stage.as_str()),
        ("organization_type", payload.organization_type.as_str()),
        ("organization_name", payload.organization_name.as_deref().unwrap_or("")),
        ("submitter_name", payload.submitter_name.as_deref().unwrap_or("")),
        ("architecture_description", payload.architecture_description.as_deref().unwrap_or("")),
        ("use_cases", payload.use_cases.as_deref().unwrap_or("")),
        ("target_users", payload.target_users.as_deref().unwrap_or("")),
        ("whitepaper_url", payload.whitepaper_url.as_deref().unwrap_or("")),
        ("documentation_url", payload.documentation_url.as_deref().unwrap_or("")),
        ("github_url", payload.github_url.as_deref().unwrap_or("")),
        ("website_url", payload.website_url.as_deref().unwrap_or("")),
        ("demo_url", payload.demo_url.as_deref().unwrap_or("")),
        ("funding_breakdown", payload.funding_breakdown.as_deref().unwrap_or("")),
    ]) {
        return resp;
    }

    let now = chrono::Utc::now();
    let project_id = format!(
        "proj-{}",
        uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("000")
    );

    let record = json!({
        "id": project_id,
        "name": name,
        "tagline": payload.tagline,
        "description": payload.description,
        "category": payload.category,
        "stage": payload.stage,
        "organizationType": payload.organization_type,
        "organizationName": payload.organization_name,
        "submitterName": payload.submitter_name,
        "submitterEmail": payload.submitter_email,
        "techStack": payload.tech_stack,
        "architectureDescription": payload.architecture_description,
        "features": payload.features,
        "useCases": payload.use_cases,
        "targetUsers": payload.target_users,
        "requiresAiTestimony": payload.requires_ai_testimony,
        "whitepaperUrl": payload.whitepaper_url,
        "documentationUrl": payload.documentation_url,
        "githubUrl": payload.github_url,
        "websiteUrl": payload.website_url,
        "demoUrl": payload.demo_url,
        "teamMembers": payload.team_members,
        "milestones": payload.milestones,
        "fundingRequested": payload.funding_requested,
        "fundingCurrency": payload.funding_currency,
        "fundingBreakdown": payload.funding_breakdown,
        "status": "pending_review",
        "createdAt": now.timestamp(),
    });

    let mut projects = load_projects_local().await;
    projects.push(record.clone());
    if let Err(e) = save_projects_local(&projects).await {
        tracing::error!("project queue write failed: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "queue write failed" })),
        );
    }

    let anchored = anchor_governance_event(
        &state,
        "ProjectSubmitted",
        &record,
        json!({
            "project_id": project_id,
            "category": payload.category,
            "organization_type": payload.organization_type,
            "funding_requested": payload.funding_requested,
            "status": "pending_review",
        }),
    )
    .await;

    if let Some(email) = payload.submitter_email.clone().filter(|e| !e.is_empty()) {
        state.mailer.send_background(
            email,
            format!("Datachain Rope — \"{name}\" received for review"),
            format!(
                "Thanks for submitting \"{name}\" to build on Datachain Rope.\n\n\
                 Your project ID is {project_id}.\n\n\
                 What happens next:\n\
                 1. The Datachain Foundation reviews your submission.\n\
                 2. Once approved, it enters a {days}-day community voting period.\n\
                 3. DC FAT holders vote to approve or reject your project (their voting weight \
                 is their live on-chain DC FAT balance on Datachain Rope).\n\
                 4. If approved (>= {threshold:.0}% of cast weight, with at least {quorum:.0} FAT \
                 of total participation), your project can start building on Datachain Rope.\n\n\
                 Track your project's status at https://dcscan.io/vote — search for {project_id}.\n\n\
                 — Datachain Foundation",
                days = voting_period_days(),
                threshold = approval_threshold() * 100.0,
                quorum = min_quorum_weight_fat(),
            ),
        );
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "success": true,
            "message": "Project submitted successfully and pending review",
            "project": record,
            "anchored": anchored.is_some(),
            "knot": anchored,
            "nextSteps": [
                "Your project will be reviewed by the Datachain Foundation",
                format!("Once approved, it will enter a {}-day community voting period", voting_period_days()),
                "DC FAT holders vote with their real on-chain FAT balance as voting weight",
                format!("If approved with >= {:.0}% weighted votes (min {:.0} FAT participation), your project can start building on Datachain Rope", approval_threshold() * 100.0, min_quorum_weight_fat()),
            ]
        })),
    )
}

#[derive(Deserialize)]
pub struct ReviewRequest {
    action: String,
    reason: Option<String>,
}

/// `POST /api/v1/projects/:id/review` — operator-only. Requires
/// `X-Admin-Token` to match `PROJECTS_ADMIN_TOKEN`; disabled entirely
/// (403) if that env var is unset, mirroring the node-requests admin
/// gate. Moves a `pending_review` project into `voting` (opening a real
/// voting window) or `rejected_by_admin`.
pub async fn review_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<ReviewRequest>,
) -> (StatusCode, Json<Value>) {
    let expected = match std::env::var("PROJECTS_ADMIN_TOKEN") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "success": false, "error": "project review disabled (PROJECTS_ADMIN_TOKEN not set)" })),
            )
        }
    };
    let presented = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let matches = presented.len() == expected.len()
        && presented
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if !matches {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "success": false, "error": "bad token" })),
        );
    }

    let action = payload.action.trim().to_lowercase();
    if action != "approve" && action != "reject" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "action must be 'approve' or 'reject'" })),
        );
    }

    // CERBER WATCH — `reason` is admin-authenticated (token check above)
    // but still gets persisted onto the project record and rendered back
    // to the submitter and any public project page. Defense-in-depth:
    // an admin token leak should not become a stored-XSS primitive too.
    if let Err(resp) =
        crate::security_guard::validate_fields(&[("reason", payload.reason.as_deref().unwrap_or(""))])
    {
        return resp;
    }

    let mut projects = load_projects_local().await;
    let Some(project) = projects
        .iter_mut()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "project not found" })),
        );
    };
    let Some(status) = project.get("status").and_then(|v| v.as_str()) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "corrupt project record" })),
        );
    };
    if status != "pending_review" {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "success": false, "error": format!("project is not pending review (status: {status})") })),
        );
    }

    let now = chrono::Utc::now().timestamp();
    let updates = if action == "approve" {
        let ends_at = now + voting_period_days() * 86_400;
        json!({
            "status": "voting",
            "votingStartedAt": now,
            "votingEndsAt": ends_at,
        })
    } else {
        json!({
            "status": "rejected_by_admin",
            "rejectionReason": payload.reason,
        })
    };

    if let Some(obj) = project.as_object_mut() {
        if let Some(u) = updates.as_object() {
            for (k, v) in u {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    let updated_project = project.clone();

    if let Err(e) = save_projects_local(&projects).await {
        tracing::error!("project queue write failed on review: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "queue write failed" })),
        );
    }

    let event_record = json!({ "id": id, "updates": updates });
    let anchored = anchor_governance_event(
        &state,
        "ProjectReviewed",
        &event_record,
        json!({ "project_id": id, "action": action }),
    )
    .await;

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "message": format!("Project {id} {action}d"),
            "project": updated_project,
            "anchored": anchored.is_some(),
            "knot": anchored,
        })),
    )
}

#[derive(Deserialize)]
pub struct ProjectVoteRequest {
    /// The voter's wallet address (checksummed or lowercase 0x… string).
    voter_address: String,
    vote_for: bool,
    /// Unix seconds — must be within ±300s of the server's clock.
    timestamp: i64,
    /// 65-byte `r||s||v` EIP-191 `personal_sign` signature, hex-encoded.
    signature: String,
    comment: Option<String>,
}

/// `POST /api/v1/projects/:id/vote` — real, signature-verified,
/// balance-weighted ballot casting.
///
/// Security model: the caller proves ownership of `voter_address` via an
/// EIP-191 `personal_sign` over a domain-separated, project- and
/// vote-bound message (`vote_message`). Voting weight is the voter's
/// REAL, live native DC FAT balance on Datachain Rope
/// (`eth_getBalance`) — single-chain for Phase 1 (see module docs).
pub async fn vote_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<ProjectVoteRequest>,
) -> (StatusCode, Json<Value>) {
    let now = chrono::Utc::now().timestamp();

    let voter = match verify_vote_signature(
        &payload.voter_address,
        &id,
        payload.vote_for,
        payload.timestamp,
        &payload.signature,
        now,
    ) {
        Ok(addr) => addr,
        Err(e) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": format!("wallet signature verification failed: {e}") })),
            )
        }
    };

    // CERBER WATCH — `blocked_signers` gate (finding H1/C4). A valid
    // `personal_sign` proof only shows the caller currently holds the
    // private key; it does not prove the key was never compromised. If a
    // denylisted signer (e.g. the compromised DCSwap deployer key) still
    // controls its key material, this stops it from casting a
    // FAT-balance-weighted governance vote.
    if let Err(resp) = crate::security_guard::check_signer(&voter) {
        return resp;
    }
    // CERBER WATCH — the voter-supplied `comment` is stored on the ballot
    // and rendered on the public project/vote page.
    if let Err(resp) =
        crate::security_guard::validate_fields(&[("comment", payload.comment.as_deref().unwrap_or(""))])
    {
        return resp;
    }

    let projects = load_projects(&state).await;
    let Some(project) = projects
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "project not found" })),
        );
    };

    let ballots = load_ballots(&state).await;
    let tally = tally_for_project(&id, &ballots);
    let raw_status = project.get("status").and_then(|v| v.as_str()).unwrap_or("pending_review");
    let voting_ends_at = project.get("votingEndsAt").and_then(|v| v.as_i64());
    let status = effective_status(raw_status, voting_ends_at, &tally, now);
    if status != "voting" {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "success": false, "error": format!("project is not open for voting (status: {status})") })),
        );
    }

    let already_voted = ballots.iter().any(|b| {
        b.get("project_id").and_then(|v| v.as_str()) == Some(id.as_str())
            && b.get("voter_address")
                .and_then(|v| v.as_str())
                .map(|a| a.eq_ignore_ascii_case(&voter))
                .unwrap_or(false)
    });
    if already_voted {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "success": false, "error": "this wallet has already voted on this project" })),
        );
    }

    let balance_wei = match fetch_fat_balance_wei(&state, &voter).await {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("vote balance check failed for {}: {}", voter, e);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "success": false, "error": "could not verify on-chain FAT balance right now; try again shortly" })),
            );
        }
    };
    let weight_fat = wei_to_fat(balance_wei);
    if weight_fat < min_fat_balance_to_vote() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": format!(
                    "insufficient DC FAT balance to vote: {weight_fat:.6} FAT held, {min:.6} FAT required",
                    min = min_fat_balance_to_vote()
                )
            })),
        );
    }

    let ballot = json!({
        "id": format!("ballot-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("000")),
        "project_id": id,
        "voter_address": voter,
        "vote_for": payload.vote_for,
        "weight_fat": weight_fat,
        "balance_wei": balance_wei.to_string(),
        "comment": payload.comment,
        "timestamp": now,
    });

    if let Err(e) = append_ballot_local(&ballot).await {
        tracing::error!("ballot queue write failed: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "ballot write failed" })),
        );
    }

    let anchored = anchor_governance_event(
        &state,
        "ProjectVoteCast",
        &ballot,
        json!({
            "project_id": id,
            "voter_address": voter,
            "vote_for": payload.vote_for,
            "weight_fat": weight_fat,
        }),
    )
    .await;

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "message": format!("Vote {} on project {} recorded with weight {:.6} FAT", if payload.vote_for { "for" } else { "against" }, id, weight_fat),
            "vote": {
                "targetType": "project",
                "targetId": id,
                "voterAddress": voter,
                "voteFor": payload.vote_for,
                "weightFat": weight_fat,
                "comment": payload.comment,
                "timestamp": now,
            },
            "anchored": anchored.is_some(),
            "knot": anchored,
        })),
    )
}

/// `GET /api/v1/votes` — global ballot listing (real data; every ballot
/// cast via `vote_project` is a `project`-targeted, signature-verified,
/// balance-weighted row).
pub async fn list_votes(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PageParams>,
) -> Json<Value> {
    let page = params.page.unwrap_or(1).max(1) as usize;
    let limit = params.limit.unwrap_or(20).clamp(1, 200) as usize;

    let mut ballots = load_ballots(&state).await;
    ballots.sort_by_key(|b| std::cmp::Reverse(b.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0)));
    let total = ballots.len();
    let start = (page - 1) * limit;

    let votes: Vec<Value> = ballots
        .into_iter()
        .skip(start)
        .take(limit)
        .map(|b| {
            json!({
                "id": b.get("id"),
                "voterAddress": b.get("voter_address"),
                "targetType": "project",
                "targetId": b.get("project_id"),
                "voteFor": b.get("vote_for"),
                "voteWeight": b.get("weight_fat"),
                "comment": b.get("comment"),
                "timestamp": b.get("timestamp"),
            })
        })
        .collect();

    Json(json!({
        "votes": votes,
        "pagination": { "page": page, "limit": limit, "total": total }
    }))
}

/// `GET /api/v1/votes/:target_type/:target_id` — real ballots + summary
/// for one target. Only `target_type == "project"` is wired to real data
/// in Phase 1 (the Federation/Community demo area is out of scope — see
/// module docs); any other target type returns an honest empty result
/// rather than fabricated numbers.
pub async fn get_votes_for_target(
    State(state): State<Arc<AppState>>,
    Path((target_type, target_id)): Path<(String, String)>,
) -> Json<Value> {
    if target_type != "project" {
        return Json(json!({
            "targetType": target_type,
            "targetId": target_id,
            "votes": [],
            "summary": { "totalVotes": 0, "votesFor": 0, "votesAgainst": 0, "totalWeight": 0.0, "weightFor": 0.0, "weightAgainst": 0.0 },
            "note": "real voting is currently wired for targetType=\"project\" only",
        }));
    }

    let ballots = load_ballots(&state).await;
    let matching: Vec<&Value> = ballots
        .iter()
        .filter(|b| b.get("project_id").and_then(|v| v.as_str()) == Some(target_id.as_str()))
        .collect();
    let tally = tally_for_project(&target_id, &ballots);

    let votes: Vec<Value> = matching
        .into_iter()
        .map(|b| {
            json!({
                "id": b.get("id"),
                "voterAddress": b.get("voter_address"),
                "voteFor": b.get("vote_for"),
                "voteWeight": b.get("weight_fat"),
                "comment": b.get("comment"),
                "timestamp": b.get("timestamp"),
            })
        })
        .collect();

    Json(json!({
        "targetType": target_type,
        "targetId": target_id,
        "votes": votes,
        "summary": {
            "totalVotes": tally.votes_for + tally.votes_against,
            "votesFor": tally.votes_for,
            "votesAgainst": tally.votes_against,
            "totalWeight": tally.weight_for + tally.weight_against,
            "weightFor": tally.weight_for,
            "weightAgainst": tally.weight_against,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;

    fn personal_sign(key: &SigningKey, message: &[u8]) -> String {
        let digest = eip191_digest(message);
        let (sig, rid) = key.sign_prehash_recoverable(&digest).expect("sign");
        let mut raw = sig.to_bytes().to_vec();
        raw.push(rid.to_byte() + 27);
        format!("0x{}", hex::encode(raw))
    }

    fn eth_address(key: &SigningKey) -> String {
        let encoded = key.verifying_key().to_encoded_point(false);
        let mut hasher = Keccak256::new();
        hasher.update(&encoded.as_bytes()[1..]);
        let hash = hasher.finalize();
        format!("0x{}", hex::encode(&hash[12..]))
    }

    #[test]
    fn vote_signature_roundtrip() {
        let key = SigningKey::from_bytes((&[0x11u8; 32]).into()).unwrap();
        let address = eth_address(&key);
        let now = 1_800_000_000;
        let message = vote_message("proj-abc123", true, now);
        let sig = personal_sign(&key, message.as_bytes());
        let proven = verify_vote_signature(&address, "proj-abc123", true, now, &sig, now).expect("verify");
        assert_eq!(proven, address);
    }

    #[test]
    fn vote_signature_wrong_project_rejected() {
        let key = SigningKey::from_bytes((&[0x11u8; 32]).into()).unwrap();
        let address = eth_address(&key);
        let now = 1_800_000_000;
        let message = vote_message("proj-abc123", true, now);
        let sig = personal_sign(&key, message.as_bytes());
        // Same signature, different project id claimed -> must fail.
        assert!(verify_vote_signature(&address, "proj-different", true, now, &sig, now).is_err());
    }

    #[test]
    fn vote_signature_flipped_direction_rejected() {
        let key = SigningKey::from_bytes((&[0x11u8; 32]).into()).unwrap();
        let address = eth_address(&key);
        let now = 1_800_000_000;
        let message = vote_message("proj-abc123", true, now);
        let sig = personal_sign(&key, message.as_bytes());
        // Same signature, but claiming the opposite vote direction -> must fail.
        assert!(verify_vote_signature(&address, "proj-abc123", false, now, &sig, now).is_err());
    }

    #[test]
    fn vote_signature_stale_timestamp_rejected() {
        let key = SigningKey::from_bytes((&[0x11u8; 32]).into()).unwrap();
        let address = eth_address(&key);
        let now = 1_800_000_000;
        let ts = now - VOTE_AUTH_WINDOW_SECS - 1;
        let message = vote_message("proj-abc123", true, ts);
        let sig = personal_sign(&key, message.as_bytes());
        assert!(verify_vote_signature(&address, "proj-abc123", true, ts, &sig, now).is_err());
    }

    #[test]
    fn vote_signature_cross_domain_rejected() {
        // A Datachain-ID or EDC-console signature must never authenticate
        // a governance ballot — distinct domain tags are load-bearing.
        let key = SigningKey::from_bytes((&[0x11u8; 32]).into()).unwrap();
        let address = eth_address(&key);
        let now = 1_800_000_000;
        let foreign_message = format!("DATACHAIN-ID-AUTH\n{}\n{}", address, now);
        let sig = personal_sign(&key, foreign_message.as_bytes());
        assert!(verify_vote_signature(&address, "proj-abc123", true, now, &sig, now).is_err());
    }

    #[test]
    fn tally_counts_and_weights_correctly() {
        let ballots = vec![
            json!({"project_id": "p1", "vote_for": true, "weight_fat": 100.0}),
            json!({"project_id": "p1", "vote_for": true, "weight_fat": 50.0}),
            json!({"project_id": "p1", "vote_for": false, "weight_fat": 30.0}),
            json!({"project_id": "p2", "vote_for": false, "weight_fat": 999.0}),
        ];
        let t = tally_for_project("p1", &ballots);
        assert_eq!(t.votes_for, 2);
        assert_eq!(t.votes_against, 1);
        assert!((t.weight_for - 150.0).abs() < 1e-9);
        assert!((t.weight_against - 30.0).abs() < 1e-9);
    }

    #[test]
    fn effective_status_open_window_stays_voting() {
        let t = Tally { votes_for: 1, votes_against: 0, weight_for: 10.0, weight_against: 0.0 };
        let now = 1_000_000;
        assert_eq!(effective_status("voting", Some(now + 3600), &t, now), "voting");
    }

    #[test]
    fn effective_status_closed_window_approved_above_threshold_and_quorum() {
        std::env::set_var("VOTE_MIN_QUORUM_FAT", "1000");
        std::env::set_var("VOTE_APPROVAL_THRESHOLD_BPS", "5100");
        let t = Tally { votes_for: 10, votes_against: 5, weight_for: 800.0, weight_against: 300.0 };
        let now = 1_000_000;
        assert_eq!(effective_status("voting", Some(now - 1), &t, now), "approved");
        std::env::remove_var("VOTE_MIN_QUORUM_FAT");
        std::env::remove_var("VOTE_APPROVAL_THRESHOLD_BPS");
    }

    #[test]
    fn effective_status_closed_window_rejected_below_threshold() {
        std::env::set_var("VOTE_MIN_QUORUM_FAT", "1");
        std::env::set_var("VOTE_APPROVAL_THRESHOLD_BPS", "5100");
        let t = Tally { votes_for: 4, votes_against: 6, weight_for: 40.0, weight_against: 60.0 };
        let now = 1_000_000;
        assert_eq!(effective_status("voting", Some(now - 1), &t, now), "rejected");
        std::env::remove_var("VOTE_MIN_QUORUM_FAT");
        std::env::remove_var("VOTE_APPROVAL_THRESHOLD_BPS");
    }

    #[test]
    fn effective_status_closed_window_no_quorum() {
        std::env::set_var("VOTE_MIN_QUORUM_FAT", "1000000");
        let t = Tally { votes_for: 1, votes_against: 0, weight_for: 10.0, weight_against: 0.0 };
        let now = 1_000_000;
        assert_eq!(effective_status("voting", Some(now - 1), &t, now), "rejected_no_quorum");
        std::env::remove_var("VOTE_MIN_QUORUM_FAT");
    }

    #[test]
    fn effective_status_non_voting_passthrough() {
        let t = Tally::default();
        assert_eq!(effective_status("pending_review", None, &t, 0), "pending_review");
        assert_eq!(effective_status("rejected_by_admin", None, &t, 0), "rejected_by_admin");
    }

    #[test]
    fn wei_to_fat_conversion() {
        assert!((wei_to_fat(1_000_000_000_000_000_000u128) - 1.0).abs() < 1e-9);
        assert!((wei_to_fat(500_000_000_000_000_000u128) - 0.5).abs() < 1e-9);
    }
}
