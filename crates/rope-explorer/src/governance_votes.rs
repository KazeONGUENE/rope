//! Governance Voting & Cause Platform - Phase 1 (real persistence + real
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
//!     EDC console), and weighted by the voter's REAL aggregate
//!     cross-chain DC/FAT balance (Ethereum legacy DC + XDC legacy DC +
//!     Rope native FAT) via `cross_chain_weight::aggregate_weight`.
//!     On-chain `VoteEscrow.castVote` uses the same weight under an
//!     EIP-191 attestation from the same aggregator.
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
//! `/api/v1/communities*`) - a separate, older feature area unrelated to
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
// EIP-191 wallet-signature verification - domain `DCROPE-VOTE-AUTH`.
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
pub(crate) fn recover_signer(message: &[u8], signature_hex: &str) -> Result<String, String> {
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
            "timestamp outside ±{VOTE_AUTH_WINDOW_SECS}s freshness window - sign again"
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
/// ballot. Anti-spam floor, not a governance parameter - env-overridable.
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
// Persistence - projects (mutable, one JSON object per project, rewritten
// atomically on every state transition) + ballots (append-only).
// ============================================================================

static PROJECTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static BALLOTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static NOMINATIONS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn projects_path() -> String {
    std::env::var("PROJECTS_PATH").unwrap_or_else(|_| "/opt/datachain-rope/projects.jsonl".into())
}

fn ballots_path() -> String {
    std::env::var("PROJECT_BALLOTS_PATH")
        .unwrap_or_else(|_| "/opt/datachain-rope/project-ballots.jsonl".into())
}

fn nominations_path() -> String {
    std::env::var("PROJECT_NOMINATIONS_PATH")
        .unwrap_or_else(|_| "/opt/datachain-rope/project-nominations.jsonl".into())
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

pub(crate) async fn anchor_governance_event(
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

/// Full verification for escrow link attestation (optional on link POST).
pub fn verify_escrow_link_signature(
    address: &str,
    project_id: &str,
    escrow_vote_id: u64,
    timestamp: i64,
    signature_hex: &str,
) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp();
    if (now - timestamp).abs() > VOTE_AUTH_WINDOW_SECS {
        return Err(format!(
            "timestamp outside ±{VOTE_AUTH_WINDOW_SECS}s freshness window - sign again"
        ));
    }
    let claimed = address.to_lowercase();
    if !claimed.starts_with("0x") || claimed.len() != 42 {
        return Err("voter_address must be a 0x-prefixed 20-byte hex address".into());
    }
    let message = format!("{VOTE_AUTH_DOMAIN}\nescrow_link\n{project_id}\n{escrow_vote_id}\n{timestamp}");
    let recovered = recover_signer(message.as_bytes(), signature_hex)?;
    if recovered != claimed {
        return Err("signature does not match the claimed voter_address".into());
    }
    Ok(claimed)
}

pub(crate) async fn load_projects_local() -> Vec<Value> {
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

async fn load_nominations_local() -> Vec<Value> {
    let path = nominations_path();
    tokio::task::spawn_blocking(move || load_jsonl_blocking(&path))
        .await
        .unwrap_or_default()
}

async fn append_nomination_local(nomination: &Value) -> std::io::Result<()> {
    let path = nominations_path();
    let line = format!("{nomination}\n");
    tokio::task::spawn_blocking(move || {
        let _guard = NOMINATIONS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

/// Public-safe nomination view (contact email/phone withheld).
fn sanitize_nomination_public(n: &Value) -> Value {
    json!({
        "id": n.get("id"),
        "projectId": n.get("projectId"),
        "orgName": n.get("orgName"),
        "legalEntity": n.get("legalEntity"),
        "mission": n.get("mission"),
        "impact": n.get("impact"),
        "requestedAmount": n.get("requestedAmount"),
        "requestedCurrency": n.get("requestedCurrency"),
        "milestones": n.get("milestones"),
        "references": n.get("references"),
        "website": n.get("website"),
        "nominatorName": n.get("nominatorName"),
        "nominatorAddress": n.get("nominatorAddress"),
        "createdAt": n.get("createdAt"),
        "status": n.get("status").cloned().unwrap_or(json!("submitted")),
        "knotHash": n.get("knotHash"),
    })
}

async fn nominations_for_project(project_id: &str) -> Vec<Value> {
    load_nominations_local()
        .await
        .into_iter()
        .filter(|n| n.get("projectId").and_then(|v| v.as_str()) == Some(project_id))
        .map(|n| sanitize_nomination_public(&n))
        .collect()
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

pub async fn load_projects(state: &Arc<AppState>) -> Vec<Value> {
    let local = load_projects_local().await;
    if local.is_empty() {
        rebuild_projects_from_rope(state).await
    } else {
        local
    }
}

/// Merge `fields` into the project record with matching `id` and persist.
/// Used by VoteEscrow linking and other governance extensions.
pub async fn patch_project_fields(
    id: &str,
    fields: serde_json::Map<String, Value>,
) -> Result<Value, String> {
    let mut projects = load_projects_local().await;
    let Some(idx) = projects
        .iter()
        .position(|p| p.get("id").and_then(|v| v.as_str()) == Some(id))
    else {
        return Err("project not found".into());
    };
    if let Some(obj) = projects[idx].as_object_mut() {
        for (k, v) in fields {
            obj.insert(k, v);
        }
    }
    let updated = projects[idx].clone();
    save_projects_local(&projects)
        .await
        .map_err(|e| format!("queue write failed: {e}"))?;
    Ok(updated)
}

/// Load a single project by id (local cache, chain rebuild if empty).
pub async fn get_project_by_id(state: &Arc<AppState>, id: &str) -> Option<Value> {
    load_projects(state)
        .await
        .into_iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(id))
}

/// Find a project whose on-chain `escrowVoteId` matches `vote_id`.
pub async fn find_project_by_escrow_vote_id(
    state: &Arc<AppState>,
    vote_id: u64,
) -> Option<Value> {
    load_projects(state)
        .await
        .into_iter()
        .find(|p| {
            p.get("escrowVoteId")
                .and_then(|v| v.as_u64())
                .or_else(|| {
                    p.get("escrowVoteId")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<u64>().ok())
                })
                == Some(vote_id)
        })
}

pub async fn load_ballots(state: &Arc<AppState>) -> Vec<Value> {
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
/// tally once the voting window has closed. Never mutates storage -
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

/// Public wrapper for effective status computation (used by NGO pipeline).
pub fn effective_status_for_project(project: &Value, ballots: &[Value], now: i64) -> String {
    let id = project.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let tally = tally_for_project(id, ballots);
    let raw_status = project
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending_review");
    let voting_ends_at = project.get("votingEndsAt").and_then(|v| v.as_i64());
    effective_status(raw_status, voting_ends_at, &tally, now)
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
        // Never expose submitter email on the public read surface.
        obj.remove("submitterEmail");
        let total_weight = tally.weight_for + tally.weight_against;
        let approval_pct = if total_weight > 0.0 {
            tally.weight_for / total_weight
        } else {
            0.0
        };
        let quorum_pct = if min_quorum_weight_fat() > 0.0 {
            (total_weight / min_quorum_weight_fat()).min(1.0)
        } else {
            0.0
        };
        obj.insert("totalWeightFat".into(), json!(total_weight));
        obj.insert("approvalPct".into(), json!(approval_pct));
        obj.insert("quorumPct".into(), json!(quorum_pct));
        if let Some(ends) = voting_ends_at {
            let secs_left = (ends - now).max(0);
            obj.insert("secondsRemaining".into(), json!(secs_left));
            obj.insert(
                "daysRemaining".into(),
                json!((secs_left as f64 / 86_400.0).ceil() as i64),
            );
        }
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
    /// Free-text search over name / tagline / description / creator / id.
    q: Option<String>,
    /// Comma-separated statuses (e.g. `voting,pending_review`).
    status: Option<String>,
    /// Comma-separated categories (e.g. `defi,other`).
    category: Option<String>,
    /// Comma-separated org types (`individual,business,institution`).
    #[serde(alias = "organizationType", alias = "org")]
    organization_type: Option<String>,
    /// Comma-separated stages (`idea,prototype,mvp,beta,production`).
    stage: Option<String>,
    /// Sort key: `newest` | `ending_soon` | `most_support` | `quorum` | `funding` | `name`.
    sort: Option<String>,
    /// When true, only projects that require AI testimony.
    #[serde(alias = "requiresAiTestimony", alias = "ai")]
    requires_ai_testimony: Option<bool>,
    /// Funding-ask bucket: `none` | `under_1k` | `1k_10k` | `10k_100k` | `over_100k`.
    #[serde(alias = "funding")]
    funding_bucket: Option<String>,
    /// Quorum-progress bucket: `under_25` | `25_50` | `50_75` | `75_100` | `over_100`.
    #[serde(alias = "quorum")]
    quorum_bucket: Option<String>,
    /// When true, include terminal statuses (approved / rejected*). Default true.
    #[serde(alias = "includeEnded")]
    include_ended: Option<bool>,
    /// Comma-separated vote classes: `project` | `cause` | `non_critical_feature`.
    #[serde(alias = "voteClass", alias = "class")]
    vote_class: Option<String>,
}

fn split_csv(raw: &Option<String>) -> Vec<String> {
    raw.as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

fn text_blob(p: &Value) -> String {
    [
        p.get("id").and_then(|v| v.as_str()).unwrap_or(""),
        p.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        p.get("tagline").and_then(|v| v.as_str()).unwrap_or(""),
        p.get("description").and_then(|v| v.as_str()).unwrap_or(""),
        p.get("submitterName").and_then(|v| v.as_str()).unwrap_or(""),
        p.get("organizationName").and_then(|v| v.as_str()).unwrap_or(""),
        p.get("category").and_then(|v| v.as_str()).unwrap_or(""),
    ]
    .join(" ")
    .to_ascii_lowercase()
}

fn funding_matches(p: &Value, bucket: &str) -> bool {
    let ask = p
        .get("fundingRequested")
        .and_then(|v| v.as_f64())
        .or_else(|| p.get("fundingRequested").and_then(|v| v.as_i64()).map(|n| n as f64))
        .unwrap_or(0.0);
    match bucket {
        "none" => ask <= 0.0,
        "under_1k" => ask > 0.0 && ask < 1_000.0,
        "1k_10k" => (1_000.0..10_000.0).contains(&ask),
        "10k_100k" => (10_000.0..100_000.0).contains(&ask),
        "over_100k" => ask >= 100_000.0,
        _ => true,
    }
}

fn quorum_matches(p: &Value, bucket: &str) -> bool {
    let pct = p.get("quorumPct").and_then(|v| v.as_f64()).unwrap_or(0.0) * 100.0;
    match bucket {
        "under_25" => pct < 25.0,
        "25_50" => (25.0..50.0).contains(&pct),
        "50_75" => (50.0..75.0).contains(&pct),
        "75_100" => (75.0..=100.0).contains(&pct),
        "over_100" => pct > 100.0,
        _ => true,
    }
}

fn is_ended_status(status: &str) -> bool {
    matches!(
        status,
        "approved"
            | "building"
            | "rejected"
            | "rejected_by_admin"
            | "rejected_no_quorum"
    )
}

fn count_by_field(projects: &[Value], field: &str) -> Value {
    let mut map = serde_json::Map::new();
    for p in projects {
        let key = p
            .get(field)
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let entry = map.entry(key).or_insert(json!(0));
        if let Some(n) = entry.as_u64() {
            *entry = json!(n + 1);
        }
    }
    Value::Object(map)
}

fn sort_projects(projects: &mut [Value], sort: &str) {
    match sort {
        "ending_soon" => projects.sort_by(|a, b| {
            let ae = a.get("votingEndsAt").and_then(|v| v.as_i64()).unwrap_or(i64::MAX);
            let be = b.get("votingEndsAt").and_then(|v| v.as_i64()).unwrap_or(i64::MAX);
            ae.cmp(&be)
        }),
        "most_support" => projects.sort_by(|a, b| {
            let aw = a.get("weightFor").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let bw = b.get("weightFor").and_then(|v| v.as_f64()).unwrap_or(0.0);
            bw.partial_cmp(&aw).unwrap_or(std::cmp::Ordering::Equal)
        }),
        "quorum" => projects.sort_by(|a, b| {
            let aq = a.get("quorumPct").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let bq = b.get("quorumPct").and_then(|v| v.as_f64()).unwrap_or(0.0);
            bq.partial_cmp(&aq).unwrap_or(std::cmp::Ordering::Equal)
        }),
        "funding" => projects.sort_by(|a, b| {
            let af = a
                .get("fundingRequested")
                .and_then(|v| v.as_f64())
                .or_else(|| a.get("fundingRequested").and_then(|v| v.as_i64()).map(|n| n as f64))
                .unwrap_or(0.0);
            let bf = b
                .get("fundingRequested")
                .and_then(|v| v.as_f64())
                .or_else(|| b.get("fundingRequested").and_then(|v| v.as_i64()).map(|n| n as f64))
                .unwrap_or(0.0);
            bf.partial_cmp(&af).unwrap_or(std::cmp::Ordering::Equal)
        }),
        "name" => projects.sort_by(|a, b| {
            let an = a.get("name").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
            let bn = b.get("name").and_then(|v| v.as_str()).unwrap_or("").to_ascii_lowercase();
            an.cmp(&bn)
        }),
        // newest (default)
        _ => projects.sort_by_key(|p| {
            std::cmp::Reverse(p.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0))
        }),
    }
}

/// When a voting window has ended, persist the terminal status on disk so
/// discover cards and owner UIs keep showing Approved / Rejected after the
/// ephemeral `attach_live_view` computation (and after explorer restarts).
async fn persist_closed_voting_windows(ballots: &[Value], now: i64) {
    let mut projects = load_projects_local().await;
    if projects.is_empty() {
        return;
    }
    let mut dirty = false;
    for project in projects.iter_mut() {
        let raw = project
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending_review")
            .to_string();
        if raw != "voting" {
            continue;
        }
        let id = project
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let ends_at = project.get("votingEndsAt").and_then(|v| v.as_i64());
        let tally = tally_for_project(&id, ballots);
        let next = effective_status(&raw, ends_at, &tally, now);
        if next == "voting" {
            continue;
        }
        if let Some(obj) = project.as_object_mut() {
            obj.insert("status".into(), json!(next));
            obj.insert("votingClosedAt".into(), json!(now));
            obj.insert(
                "closedReason".into(),
                json!(match next.as_str() {
                    "approved" => "voting_window_ended_approved",
                    "rejected" => "voting_window_ended_rejected",
                    "rejected_no_quorum" => "voting_window_ended_no_quorum",
                    _ => "voting_window_ended",
                }),
            );
            dirty = true;
        }
    }
    if dirty {
        if let Err(e) = save_projects_local(&projects).await {
            tracing::warn!("persist_closed_voting_windows failed: {e}");
        }
    }
}

/// `GET /api/v1/projects` - real, persisted, chain-anchored project
/// submissions with a live-computed status/tally view, Kickstarter-style
/// discover filters, sort, and facet aggregations.
pub async fn list_projects(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PageParams>,
) -> Json<Value> {
    let page = params.page.unwrap_or(1).max(1) as usize;
    let limit = params.limit.unwrap_or(20).clamp(1, 200) as usize;
    let now = chrono::Utc::now().timestamp();
    let sort = params
        .sort
        .as_deref()
        .unwrap_or("newest")
        .trim()
        .to_ascii_lowercase();
    let include_ended = params.include_ended.unwrap_or(true);
    let q = params
        .q
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let statuses = split_csv(&params.status);
    let categories = split_csv(&params.category);
    let org_types = split_csv(&params.organization_type);
    let stages = split_csv(&params.stage);
    let funding_bucket = params
        .funding_bucket
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let quorum_bucket = params
        .quorum_bucket
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    let ballots = load_ballots(&state).await;
    persist_closed_voting_windows(&ballots, now).await;
    let mut projects: Vec<Value> = load_projects(&state)
        .await
        .into_iter()
        .map(|p| attach_live_view(p, &ballots, now))
        .collect();

    // Facets are computed on the unfiltered corpus (Kickstarter-style
    // aggregation) so filter checkboxes keep stable counts.
    let mut vote_class_counts = serde_json::Map::new();
    for p in &projects {
        let key = p
            .get("voteClass")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("project")
            .to_string();
        let entry = vote_class_counts.entry(key).or_insert(json!(0));
        if let Some(n) = entry.as_u64() {
            *entry = json!(n + 1);
        }
    }
    let facets = json!({
        "status": count_by_field(&projects, "status"),
        "category": count_by_field(&projects, "category"),
        "organizationType": count_by_field(&projects, "organizationType"),
        "stage": count_by_field(&projects, "stage"),
        "voteClass": Value::Object(vote_class_counts),
        "total": projects.len(),
    });

    let vote_classes = split_csv(&params.vote_class);

    projects.retain(|p| {
        let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if !include_ended && is_ended_status(status) {
            return false;
        }
        if !vote_classes.is_empty() {
            let vc = p
                .get("voteClass")
                .and_then(|v| v.as_str())
                .unwrap_or("project")
                .to_ascii_lowercase();
            if !vote_classes.iter().any(|c| c == &vc) {
                return false;
            }
        }
        if !statuses.is_empty() && !statuses.iter().any(|s| s == &status.to_ascii_lowercase()) {
            // Aliases: "rejected" matches all rejected_* variants; "active" → voting.
            let aliased = statuses.iter().any(|s| match s.as_str() {
                "rejected" => status.starts_with("rejected"),
                "active" | "live" => status == "voting",
                "pending" => status == "pending_review",
                "ended" => is_ended_status(status),
                _ => false,
            });
            if !aliased {
                return false;
            }
        }
        if !categories.is_empty() {
            let cat = p
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !categories.iter().any(|c| c == &cat) {
                return false;
            }
        }
        if !org_types.is_empty() {
            let org = p
                .get("organizationType")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !org_types.iter().any(|o| o == &org) {
                return false;
            }
        }
        if !stages.is_empty() {
            let st = p
                .get("stage")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !stages.iter().any(|s| s == &st) {
                return false;
            }
        }
        if let Some(ai) = params.requires_ai_testimony {
            let flag = p
                .get("requiresAiTestimony")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if flag != ai {
                return false;
            }
        }
        if !funding_bucket.is_empty() && !funding_matches(p, &funding_bucket) {
            return false;
        }
        if !quorum_bucket.is_empty() && !quorum_matches(p, &quorum_bucket) {
            return false;
        }
        if !q.is_empty() && !text_blob(p).contains(&q) {
            return false;
        }
        true
    });

    sort_projects(&mut projects, &sort);
    let filtered_total = projects.len();
    let start = (page - 1) * limit;
    let page_items: Vec<Value> = projects.into_iter().skip(start).take(limit).collect();

    Json(json!({
        "projects": page_items,
        "pagination": {
            "page": page,
            "limit": limit,
            "total": filtered_total,
            "totalUnfiltered": facets.get("total").and_then(|v| v.as_u64()).unwrap_or(0),
        },
        "facets": facets,
        "applied": {
            "q": if q.is_empty() { Value::Null } else { json!(q) },
            "status": statuses,
            "category": categories,
            "organizationType": org_types,
            "stage": stages,
            "sort": sort,
            "requiresAiTestimony": params.requires_ai_testimony,
            "funding": if funding_bucket.is_empty() { Value::Null } else { json!(funding_bucket) },
            "quorum": if quorum_bucket.is_empty() { Value::Null } else { json!(quorum_bucket) },
            "includeEnded": include_ended,
            "voteClass": vote_classes,
        },
        "sortOptions": [
            {"id": "newest", "label": "Newest"},
            {"id": "ending_soon", "label": "Ending soon"},
            {"id": "most_support", "label": "Most support (FAT for)"},
            {"id": "quorum", "label": "Closest to quorum"},
            {"id": "funding", "label": "Highest funding ask"},
            {"id": "name", "label": "Name A-Z"},
        ],
    }))
}

/// `GET /api/v1/projects/voting` - projects currently accepting ballots.
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
    let ballots = load_ballots(&state).await;
    persist_closed_voting_windows(&ballots, now).await;
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
    let mut live = attach_live_view(project, &ballots, now);
    let nominations = nominations_for_project(&id).await;
    if let Some(obj) = live.as_object_mut() {
        obj.insert("nominationsCount".into(), json!(nominations.len()));
        obj.insert("nominations".into(), json!(nominations));
    }
    (StatusCode::OK, Json(live))
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
    /// `project` (default) | `cause` (NGO/donation) | `non_critical_feature`.
    #[serde(default = "default_vote_class", alias = "vote_class")]
    vote_class: String,
    /// `return` (default for causes) | `burn` | `reward` - maps to VoteEscrow.Disposition.
    #[serde(default = "default_disposition")]
    disposition: String,
    /// NGO/cause-specific fields (ignored for vote_class=project).
    legal_entity: Option<String>,
    mission: Option<String>,
    references: Option<String>,
    impact: Option<String>,
    #[serde(default, alias = "valueProposition")]
    value_proposition: Option<String>,
    value: Option<String>,
    /// Free-text team description or structured member list.
    team: Option<Value>,
    #[serde(default, alias = "contactEmail")]
    contact_email: Option<String>,
    #[serde(default, alias = "contactName")]
    contact_name: Option<String>,
    #[serde(default, alias = "contactPhone")]
    contact_phone: Option<String>,
    #[serde(default)]
    socials: Option<Value>,
    #[serde(default, alias = "whyVote1")]
    why_vote1: Option<String>,
    #[serde(default, alias = "whyVote2")]
    why_vote2: Option<String>,
    #[serde(default, alias = "whyVote3")]
    why_vote3: Option<String>,
    /// CriticalProtocol mint intent (admin-only submissions).
    #[serde(default, alias = "mintRecipient")]
    mint_recipient: Option<String>,
    #[serde(default, alias = "mintAmount")]
    mint_amount: Option<String>,
    #[serde(default, alias = "mintTokenId")]
    mint_token_id: Option<String>,
    #[serde(default, alias = "mintReason")]
    mint_reason: Option<String>,
    #[serde(default, alias = "submitterWallet")]
    submitter_wallet: Option<String>,
}

fn default_funding_currency() -> String {
    "FAT".to_string()
}

fn default_vote_class() -> String {
    "project".to_string()
}

fn default_disposition() -> String {
    "return".to_string()
}

fn normalize_vote_class(raw: &str) -> Result<&'static str, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "project" | "" => Ok("project"),
        "cause" | "ngo" | "donation" => Ok("cause"),
        "non_critical_feature" | "feature" => Ok("non_critical_feature"),
        // CriticalProtocol submissions are Foundation-only (admin token gate
        // in submit_project). Public form still cannot select this class.
        "critical_protocol" | "criticalprotocol" => Ok("critical_protocol"),
        other => Err(format!(
            "vote_class must be project|cause|non_critical_feature|critical_protocol (got {other})"
        )),
    }
}

fn normalize_disposition(raw: &str) -> Result<&'static str, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "return" | "" => Ok("return"),
        "burn" => Ok("burn"),
        "reward" => Ok("reward"),
        other => Err(format!("disposition must be return|burn|reward (got {other})")),
    }
}

/// `POST /api/v1/projects` - real submission: validated, durably
/// persisted, anchored on the rope, and (best-effort) confirmed by email
/// via the same SendGrid-backed mailer used by the contact form.
pub async fn submit_project(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
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

    let vote_class = match normalize_vote_class(&payload.vote_class) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": e })),
            )
        }
    };
    // CriticalProtocol is Foundation-gated: requires a `ProjectAdmin`
    // (or `MultiRole`) dynamic admin token + mint intent. Env-var
    // escape hatches are no longer consulted.
    if vote_class == "critical_protocol" {
        if let Err((code, body)) = crate::admin_tokens::require_role(
            &state.admin_tokens,
            &headers,
            crate::admin_tokens::Role::ProjectAdmin,
        )
        .await
        {
            return (code, body);
        }
        let recip = payload.mint_recipient.as_deref().unwrap_or("").trim();
        let amt = payload.mint_amount.as_deref().unwrap_or("").trim();
        if recip.is_empty() || amt.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "success": false,
                    "error": "critical_protocol requires mintRecipient and mintAmount (wei)"
                })),
            );
        }
    }
    let disposition = match normalize_disposition(&payload.disposition) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": e })),
            )
        }
    };
    // Causes default to Return disposition (Andrew: tokens come back after the window).
    let disposition = if vote_class == "cause" && payload.disposition.trim().is_empty() {
        "return"
    } else {
        disposition
    };

    // CERBER WATCH - every free-text field on this form is durably
    // persisted (`load_projects_local`/`save_projects_local`) and later
    // rendered on the public governance/voting pages, so it is both a
    // stored-XSS and a stored-SQLi-signal (against future SQL-backed
    // storage) surface. `source_code`-style fields are deliberately never
    // passed through this gate (see `security_guard` module docs) - none
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
        ("legal_entity", payload.legal_entity.as_deref().unwrap_or("")),
        ("mission", payload.mission.as_deref().unwrap_or("")),
        ("references", payload.references.as_deref().unwrap_or("")),
        ("impact", payload.impact.as_deref().unwrap_or("")),
        ("value_proposition", payload.value_proposition.as_deref().unwrap_or("")),
        ("value", payload.value.as_deref().unwrap_or("")),
        ("contact_email", payload.contact_email.as_deref().unwrap_or("")),
        ("contact_name", payload.contact_name.as_deref().unwrap_or("")),
        ("contact_phone", payload.contact_phone.as_deref().unwrap_or("")),
        ("why_vote1", payload.why_vote1.as_deref().unwrap_or("")),
        ("why_vote2", payload.why_vote2.as_deref().unwrap_or("")),
        ("why_vote3", payload.why_vote3.as_deref().unwrap_or("")),
    ]) {
        return resp;
    }
    if let Some(team) = payload.team.as_ref() {
        let team_str = match team {
            Value::String(s) => s.as_str(),
            Value::Array(arr) => {
                for item in arr {
                    if let Value::String(s) = item {
                        if let Err(resp) =
                            crate::security_guard::validate_fields(&[("team_member", s.as_str())])
                        {
                            return resp;
                        }
                    }
                }
                ""
            }
            _ => "",
        };
        if !team_str.is_empty() {
            if let Err(resp) =
                crate::security_guard::validate_fields(&[("team", team_str)])
            {
                return resp;
            }
        }
    }
    if let Some(socials) = payload.socials.as_ref().and_then(|v| v.as_object()) {
        for (key, val) in socials {
            if let Some(s) = val.as_str() {
                if let Err(resp) = crate::security_guard::validate_fields(&[(key.as_str(), s)]) {
                    return resp;
                }
            }
        }
    }

    if vote_class == "cause" {
        let mission = payload.mission.as_deref().unwrap_or("").trim();
        if mission.is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": "cause submissions require a mission statement" })),
            );
        }
        let has_impact = !payload.impact.as_deref().unwrap_or("").trim().is_empty();
        let has_value_prop = !payload.value_proposition.as_deref().unwrap_or("").trim().is_empty();
        let has_value = !payload.value.as_deref().unwrap_or("").trim().is_empty();
        if !has_impact && !has_value_prop && !has_value {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "success": false,
                    "error": "cause submissions require impact, valueProposition, or value"
                })),
            );
        }
        for (field, val) in [
            ("whyVote1", payload.why_vote1.as_deref()),
            ("whyVote2", payload.why_vote2.as_deref()),
            ("whyVote3", payload.why_vote3.as_deref()),
        ] {
            if val.unwrap_or("").trim().is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "success": false,
                        "error": format!("cause submissions require {field}")
                    })),
                );
            }
        }
        if let Some(email) = payload.contact_email.as_deref() {
            if !email.is_empty()
                && (!email.contains('@') || !email.contains('.') || email.len() > 254)
            {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "success": false, "error": "contact_email must be a valid email" })),
                );
            }
        }
    }

    let now = chrono::Utc::now();
    let id_prefix = match vote_class {
        "cause" => "cause",
        "critical_protocol" => "critical",
        _ => "proj",
    };
    let project_id = format!(
        "{id_prefix}-{}",
        uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("000")
    );

    let record = json!({
        "id": project_id,
        "name": name,
        "tagline": payload.tagline,
        "description": payload.description,
        "category": if vote_class == "cause" && payload.category.trim().is_empty() {
            "other".to_string()
        } else {
            payload.category.clone()
        },
        "stage": payload.stage,
        "organizationType": payload.organization_type,
        "organizationName": payload.organization_name,
        "submitterName": payload.submitter_name,
        "submitterEmail": payload.submitter_email,
        "submitterWallet": payload.submitter_wallet,
        // Owner-gated edit/publish/archive/resubmit + attaches use this wallet.
        "ownerWallet": payload.submitter_wallet,
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
        "voteClass": vote_class,
        "disposition": disposition,
        "legalEntity": payload.legal_entity,
        "mission": payload.mission,
        "references": payload.references,
        "impact": payload.impact,
        "valueProposition": payload.value_proposition,
        "value": payload.value,
        "team": payload.team,
        "contactEmail": payload.contact_email,
        "contactName": payload.contact_name,
        "contactPhone": payload.contact_phone,
        "socials": payload.socials,
        "whyVote1": payload.why_vote1,
        "whyVote2": payload.why_vote2,
        "whyVote3": payload.why_vote3,
        "mintRecipient": payload.mint_recipient,
        "mintAmount": payload.mint_amount,
        "mintTokenId": payload.mint_token_id,
        "mintReason": payload.mint_reason,
        // Causes default to the Phase-5 jury + pay-to-vote eligibility set.
        "eligibleVoterSet": if vote_class == "cause" {
            "jury_and_pay"
        } else {
            "all_holders"
        },
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
            "vote_class": vote_class,
            "disposition": disposition,
            "category": payload.category,
            "organization_type": payload.organization_type,
            "funding_requested": payload.funding_requested,
            "status": "pending_review",
        }),
    )
    .await;

    if let Some(email) = payload.submitter_email.clone().filter(|e| !e.is_empty()) {
        let kind_label = if vote_class == "cause" { "cause / NGO nomination" } else { "project" };
        state.mailer.send_background(
            email,
            format!("Datachain Rope - \"{name}\" received for review"),
            format!(
                "Thanks for submitting \"{name}\" ({kind_label}) on Datachain Rope.\n\n\
                 Your ID is {project_id}. Disposition for locked FAT: {disposition}.\n\n\
                 What happens next:\n\
                 1. The Datachain Foundation reviews your submission.\n\
                 2. Once approved, it enters a {days}-day community voting period.\n\
                 3. Holders vote with cross-chain weight (legacy DC on Ethereum + XDC + \
                 native FAT on Datachain Rope). Optional locked FAT follows disposition={disposition}.\n\
                 4. If approved (>= {threshold:.0}% of cast weight, with at least {quorum:.0} FAT-equivalent \
                 participation), the outcome proceeds (project build / NGO contractualization).\n\n\
                 Track status at https://dcscan.io/vote - search for {project_id}.\n\n\
                 - Datachain Foundation",
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

/// `POST /api/v1/projects/:id/review` - operator-only. Requires
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
    // Project review is gated by a dynamic `ProjectAdmin` (or
    // `MultiRole`) admin token via [`crate::admin_tokens::require_role`].
    // Env-var escape hatches are no longer consulted.
    if let Err((code, body)) = crate::admin_tokens::require_role(
        &state.admin_tokens,
        &headers,
        crate::admin_tokens::Role::ProjectAdmin,
    )
    .await
    {
        return (code, body);
    }

    let action = payload.action.trim().to_lowercase();
    if action != "approve" && action != "reject" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "action must be 'approve' or 'reject'" })),
        );
    }

    // CERBER WATCH - `reason` is admin-authenticated (token check above)
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

    let mut jury_drawn: Option<Value> = None;
    if action == "approve" {
        let vote_class = updated_project
            .get("voteClass")
            .and_then(|v| v.as_str())
            .unwrap_or("project");
        if vote_class == "cause" {
            match crate::ngo_pipeline::draw_jury_for_project(
                &state,
                &id,
                crate::jury::DEFAULT_JURY_FRACTION_BPS,
            )
            .await
            {
                Ok(draw) => jury_drawn = Some(draw),
                Err(e) => {
                    tracing::warn!("auto jury draw on cause approve failed for {id}: {e}");
                }
            }
        }
    }

    let project_out = if let Some(ref draw) = jury_drawn {
        draw.get("project").cloned().unwrap_or(updated_project.clone())
    } else {
        updated_project.clone()
    };

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "message": format!("Project {id} {action}d"),
            "project": project_out,
            "anchored": anchored.is_some(),
            "knot": anchored,
            "juryDrawn": jury_drawn,
        })),
    )
}

#[derive(Deserialize)]
pub struct ProjectVoteRequest {
    /// The voter's wallet address (checksummed or lowercase 0x… string).
    voter_address: String,
    vote_for: bool,
    /// Unix seconds - must be within ±300s of the server's clock.
    timestamp: i64,
    /// 65-byte `r||s||v` EIP-191 `personal_sign` signature, hex-encoded.
    signature: String,
    comment: Option<String>,
}

/// `POST /api/v1/projects/:id/vote` - real, signature-verified,
/// balance-weighted ballot casting (JSONL + knot audit trail).
///
/// Security model: the caller proves ownership of `voter_address` via an
/// EIP-191 `personal_sign` over a domain-separated, project- and
/// vote-bound message (`vote_message`). Voting weight is the voter's
/// REAL aggregate cross-chain DC/FAT balance (Ethereum + XDC + Rope)
/// from `cross_chain_weight::aggregate_weight`. When the project has an
/// `escrowVoteId`, the UI should ALSO submit `VoteEscrow.castVote` with
/// a fresh attestation - this handler remains the discover/tally cache
/// for Kickstarter-style cards; on-chain is authoritative for locked FAT.
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

    // CERBER WATCH - `blocked_signers` gate (finding H1/C4). A valid
    // `personal_sign` proof only shows the caller currently holds the
    // private key; it does not prove the key was never compromised. If a
    // denylisted signer (e.g. the compromised DCSwap deployer key) still
    // controls its key material, this stops it from casting a
    // FAT-balance-weighted governance vote.
    if let Err(resp) = crate::security_guard::check_signer(&voter) {
        return resp;
    }
    // CERBER WATCH - the voter-supplied `comment` is stored on the ballot
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

    // Cross-chain aggregate (Ethereum legacy DC + XDC legacy DC + Rope
    // native FAT). Same source of truth as VoteEscrow attestations.
    let breakdown = crate::cross_chain_weight::aggregate_weight(&state, &voter).await;
    if !breakdown.ethereum.ok && !breakdown.xdc.ok && !breakdown.rope.ok {
        tracing::warn!(
            "vote weight check failed for {}: eth={:?} xdc={:?} rope={:?}",
            voter,
            breakdown.ethereum.error,
            breakdown.xdc.error,
            breakdown.rope.error
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "success": false, "error": "could not verify cross-chain DC/FAT balance right now; try again shortly" })),
        );
    }
    let balance_wei: u128 = breakdown.total_wei.parse().unwrap_or(0);
    let weight_fat = wei_to_fat(balance_wei);
    if weight_fat < min_fat_balance_to_vote() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": format!(
                    "insufficient aggregate DC/FAT balance to vote: {weight_fat:.6} held across Ethereum+XDC+Rope, {min:.6} required",
                    min = min_fat_balance_to_vote()
                ),
                "breakdown": breakdown,
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
        "weight_source": "cross_chain_aggregate",
        "breakdown": {
            "ethereum_wei": breakdown.ethereum.balance_wei,
            "xdc_wei": breakdown.xdc.balance_wei,
            "rope_wei": breakdown.rope.balance_wei,
            "all_chains_ok": breakdown.all_chains_ok,
        },
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
            "message": format!("Vote {} on project {} recorded with weight {:.6} FAT-equivalent (cross-chain)", if payload.vote_for { "for" } else { "against" }, id, weight_fat),
            "vote": {
                "targetType": "project",
                "targetId": id,
                "voterAddress": voter,
                "voteFor": payload.vote_for,
                "weightFat": weight_fat,
                "weightSource": "cross_chain_aggregate",
                "comment": payload.comment,
                "timestamp": now,
            },
            "breakdown": breakdown,
            "escrowVoteId": project.get("escrowVoteId"),
            "anchored": anchored.is_some(),
            "knot": anchored,
        })),
    )
}

/// `GET /api/v1/votes` - global ballot listing (real data; every ballot
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

/// `GET /api/v1/votes/:target_type/:target_id` - real ballots + summary
/// for one target. Only `target_type == "project"` is wired to real data
/// in Phase 1 (the Federation/Community demo area is out of scope - see
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

// ============================================================================
// Governance pool membership - community members register email + the
// wallet addresses that hold their DC / DC FAT / WFAT across Rope (DCR-20),
// Ethereum (legacy ERC-20 / ERC-777 DC), and XDC (XRC-20 DC). Membership
// is proven with an EIP-191 signature over a domain-tagged message so a
// third party cannot enroll another person's address.
// ============================================================================

const POOL_AUTH_DOMAIN: &str = "DCROPE-GOV-POOL-JOIN";
const POOL_AUTH_WINDOW_SECS: i64 = 600;

static POOL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn governance_pool_path() -> String {
    std::env::var("GOVERNANCE_POOL_PATH")
        .unwrap_or_else(|_| "/opt/datachain-rope/governance-pool.jsonl".into())
}

fn normalize_evm_address(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    let hex = if let Some(rest) = s.strip_prefix("xdc").or_else(|| s.strip_prefix("XDC")) {
        format!("0x{rest}")
    } else {
        s.to_string()
    };
    let body = hex
        .strip_prefix("0x")
        .or_else(|| hex.strip_prefix("0X"))
        .unwrap_or(hex.as_str());
    if body.len() != 40 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("invalid EVM address: {raw}"));
    }
    Ok(format!("0x{}", body.to_ascii_lowercase()))
}

fn looks_like_email(email: &str) -> bool {
    let e = email.trim();
    let at = match e.find('@') {
        Some(i) if i > 0 && i + 1 < e.len() => i,
        _ => return false,
    };
    e[at + 1..].contains('.') && !e.contains(' ') && e.len() <= 254
}

fn pool_join_message(
    email: &str,
    rope: &str,
    eth: &str,
    xdc: &str,
    timestamp: i64,
) -> String {
    format!("{POOL_AUTH_DOMAIN}\n{email}\n{rope}\n{eth}\n{xdc}\n{timestamp}")
}

async fn load_pool_local() -> Vec<Value> {
    let path = governance_pool_path();
    tokio::task::spawn_blocking(move || load_jsonl_blocking(&path))
        .await
        .unwrap_or_default()
}

async fn save_pool_local(list: &[Value]) -> std::io::Result<()> {
    let path = governance_pool_path();
    let lines: String = list.iter().map(|r| format!("{r}\n")).collect();
    tokio::task::spawn_blocking(move || {
        let _guard = POOL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = format!("{path}.tmp");
        std::fs::write(&tmp, &lines).and_then(|_| std::fs::rename(&tmp, &path))
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())))
}

#[derive(Debug, Deserialize)]
pub struct JoinPoolBody {
    pub email: String,
    #[serde(default, alias = "ropeAddress", alias = "rope_address")]
    pub rope_address: Option<String>,
    #[serde(default, alias = "ethAddress", alias = "eth_address", alias = "ethereumAddress")]
    pub eth_address: Option<String>,
    #[serde(default, alias = "xdcAddress", alias = "xdc_address")]
    pub xdc_address: Option<String>,
    pub timestamp: i64,
    pub signature: String,
    /// Which registered address produced the signature (`rope` | `eth` | `xdc`).
    #[serde(default, alias = "signerChain", alias = "signer_chain")]
    pub signer_chain: Option<String>,
}

/// `GET /api/v1/governance/pool` - public membership stats (no PII).
pub async fn governance_pool_stats(State(_state): State<Arc<AppState>>) -> Json<Value> {
    let members = load_pool_local().await;
    let with_rope = members
        .iter()
        .filter(|m| {
            m.get("ropeAddress")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false)
        })
        .count();
    let with_eth = members
        .iter()
        .filter(|m| {
            m.get("ethAddress")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false)
        })
        .count();
    let with_xdc = members
        .iter()
        .filter(|m| {
            m.get("xdcAddress")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false)
        })
        .count();
    Json(json!({
        "success": true,
        "memberCount": members.len(),
        "withRopeAddress": with_rope,
        "withEthAddress": with_eth,
        "withXdcAddress": with_xdc,
        "chains": {
            "rope": { "chainId": 271828, "standard": "DCR-20", "tokens": ["DC FAT", "FAT", "WFAT"] },
            "ethereum": { "chainId": 1, "standard": "ERC-20 / ERC-777", "tokens": ["DC"] },
            "xdc": { "chainId": 50, "standard": "XRC-20", "tokens": ["DC"] },
        },
        "note": "Join via POST /api/v1/governance/pool/join with email + at least one wallet + EIP-191 signature.",
    }))
}

/// `POST /api/v1/governance/pool/join` - register (or update) a governance-pool member.
pub async fn join_governance_pool(
    State(state): State<Arc<AppState>>,
    Json(body): Json<JoinPoolBody>,
) -> (StatusCode, Json<Value>) {
    let email = body.email.trim().to_ascii_lowercase();
    if !looks_like_email(&email) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "valid email is required" })),
        );
    }

    let now = chrono::Utc::now().timestamp();
    if (now - body.timestamp).abs() > POOL_AUTH_WINDOW_SECS {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "signature timestamp outside ±10 minute window" })),
        );
    }

    let rope = match body.rope_address.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(a) => match normalize_evm_address(a) {
            Ok(v) => Some(v),
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "success": false, "error": format!("rope address: {e}") })),
                )
            }
        },
        None => None,
    };
    let eth = match body.eth_address.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(a) => match normalize_evm_address(a) {
            Ok(v) => Some(v),
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "success": false, "error": format!("ethereum address: {e}") })),
                )
            }
        },
        None => None,
    };
    let xdc = match body.xdc_address.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(a) => match normalize_evm_address(a) {
            Ok(v) => Some(v),
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "success": false, "error": format!("xdc address: {e}") })),
                )
            }
        },
        None => None,
    };

    if rope.is_none() && eth.is_none() && xdc.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "register at least one wallet: ropeAddress (DCR-20 / FAT / WFAT), ethAddress (ERC-20 DC), or xdcAddress (XRC-20 DC)"
            })),
        );
    }

    let rope_s = rope.clone().unwrap_or_else(|| "-".into());
    let eth_s = eth.clone().unwrap_or_else(|| "-".into());
    let xdc_s = xdc.clone().unwrap_or_else(|| "-".into());
    let message = pool_join_message(&email, &rope_s, &eth_s, &xdc_s, body.timestamp);

    let proven = match recover_signer(message.as_bytes(), &body.signature) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": format!("invalid signature: {e}") })),
            )
        }
    };

    let allowed = [rope.as_deref(), eth.as_deref(), xdc.as_deref()]
        .into_iter()
        .flatten()
        .any(|a| a == proven.as_str());
    if !allowed {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "success": false,
                "error": "signature recovered address does not match any registered wallet"
            })),
        );
    }

    let mut members = load_pool_local().await;
    let existing_idx = members.iter().position(|m| {
        m.get("email").and_then(|v| v.as_str()) == Some(email.as_str())
            || [rope.as_deref(), eth.as_deref(), xdc.as_deref()]
                .into_iter()
                .flatten()
                .any(|addr| {
                    m.get("ropeAddress").and_then(|v| v.as_str()) == Some(addr)
                        || m.get("ethAddress").and_then(|v| v.as_str()) == Some(addr)
                        || m.get("xdcAddress").and_then(|v| v.as_str()) == Some(addr)
                })
    });

    let id = existing_idx
        .and_then(|i| members[i].get("id").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_else(|| format!("gpool-{}", &uuid::Uuid::new_v4().to_string()[..8]));

    let record = json!({
        "id": id,
        "email": email,
        "ropeAddress": rope,
        "ethAddress": eth,
        "xdcAddress": xdc,
        "signerAddress": proven,
        "signerChain": body.signer_chain,
        "updatedAt": now,
        "createdAt": existing_idx
            .and_then(|i| members[i].get("createdAt").and_then(|v| v.as_i64()))
            .unwrap_or(now),
    });

    if let Some(i) = existing_idx {
        members[i] = record.clone();
    } else {
        members.push(record.clone());
    }

    if let Err(e) = save_pool_local(&members).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": format!("persist failed: {e}") })),
        );
    }

    let anchored = anchor_governance_event(
        &state,
        "GovernancePoolJoined",
        &record,
        json!({
            "email": record.get("email"),
            "ropeAddress": record.get("ropeAddress"),
            "ethAddress": record.get("ethAddress"),
            "xdcAddress": record.get("xdcAddress"),
            "signerAddress": proven,
        }),
    )
    .await;

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "member": {
                "id": record.get("id"),
                "email": record.get("email"),
                "ropeAddress": record.get("ropeAddress"),
                "ethAddress": record.get("ethAddress"),
                "xdcAddress": record.get("xdcAddress"),
                "createdAt": record.get("createdAt"),
                "updatedAt": record.get("updatedAt"),
            },
            "memberCount": members.len(),
            "knot": anchored,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;
    use std::sync::Mutex;

    /// Env-var based quorum/threshold helpers are process-global; serialize
    /// tests that mutate them so parallel cargo test workers don't race.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
        // a governance ballot - distinct domain tags are load-bearing.
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
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    #[test]
    fn normalize_evm_address_accepts_xdc_prefix() {
        let a = normalize_evm_address("xdc70997970C51812dc3A010C7d01b50e0d17dc79C8").unwrap();
        assert_eq!(a, "0x70997970c51812dc3a010c7d01b50e0d17dc79c8");
    }

    #[test]
    fn pool_join_signature_binds_email_and_wallets() {
        let key = SigningKey::from_bytes((&[0x22u8; 32]).into()).unwrap();
        let address = eth_address(&key);
        let now = 1_800_000_000;
        let email = "member@example.com";
        let msg = pool_join_message(email, &address, "-", "-", now);
        let sig = personal_sign(&key, msg.as_bytes());
        let proven = recover_signer(msg.as_bytes(), &sig).unwrap();
        assert_eq!(proven, address);
        // Tampering email invalidates.
        let bad = pool_join_message("other@example.com", &address, "-", "-", now);
        assert_ne!(recover_signer(bad.as_bytes(), &sig).ok(), Some(address));
    }

    #[test]
    fn looks_like_email_basic() {
        assert!(looks_like_email("a@b.co"));
        assert!(!looks_like_email("not-an-email"));
        assert!(!looks_like_email("@missing.local"));
    }

    #[test]
    fn attach_message_binds_kind_project_and_url() {
        let a = attach_message("cause-1", "document", "https://example.com/a.pdf", 100);
        let b = attach_message("cause-1", "media", "https://example.com/a.pdf", 100);
        let c = attach_message("cause-2", "document", "https://example.com/a.pdf", 100);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert!(a.contains("DCROPE-PROJECT-ATTACH"));
    }
}

// ============================================================================
// Project owner attachments - Documentation + Medias
// ============================================================================

const ATTACH_AUTH_DOMAIN: &str = "DCROPE-PROJECT-ATTACH";
const ATTACH_AUTH_WINDOW_SECS: i64 = 300;

fn attach_message(project_id: &str, kind: &str, url: &str, timestamp: i64) -> String {
    format!("{ATTACH_AUTH_DOMAIN}\n{kind}\n{project_id}\n{url}\n{timestamp}")
}

/// Returns true when the request carries a `ProjectAdmin`- or
/// `MultiRole`-scoped dynamic admin token (see [`crate::admin_tokens`]).
///
/// Replaces the pre-2026-08-14 env-var flow that consulted
/// `PROJECTS_ADMIN_TOKEN` directly. Env-var escape hatches are no
/// longer consulted; only tokens minted by the dynamic store work.
async fn admin_token_matches(state: &Arc<AppState>, headers: &HeaderMap) -> bool {
    let candidate = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .unwrap_or("");
    if candidate.is_empty() {
        return false;
    }
    match state.admin_tokens.verify_and_role(candidate).await {
        Some(role) => role.grants(crate::admin_tokens::Role::ProjectAdmin),
        None => false,
    }
}

fn project_owner_wallet(project: &Value) -> Option<String> {
    project
        .get("ownerWallet")
        .or_else(|| project.get("submitterWallet"))
        .or_else(|| project.get("submitter_wallet"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| s.starts_with("0x") && s.len() == 42)
}

async fn authorize_project_attach(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    project: &Value,
    project_id: &str,
    kind: &str,
    url: &str,
    owner_address: Option<&str>,
    timestamp: Option<i64>,
    signature: Option<&str>,
    now: i64,
) -> Result<String, (StatusCode, Json<Value>)> {
    if admin_token_matches(state, headers).await {
        return Ok("admin".into());
    }
    let (addr, ts, sig) = match (owner_address, timestamp, signature) {
        (Some(a), Some(t), Some(s)) if !a.is_empty() && !s.is_empty() => (a, t, s),
        _ => {
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({
                    "success": false,
                    "error": "attach requires X-Admin-Token or a signed owner-wallet proof"
                })),
            ));
        }
    };
    if (now - ts).abs() > ATTACH_AUTH_WINDOW_SECS {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "success": false,
                "error": format!(
                    "timestamp outside ±{ATTACH_AUTH_WINDOW_SECS}s freshness window - sign again"
                )
            })),
        ));
    }
    let claimed = addr.to_lowercase();
    let Some(owner) = project_owner_wallet(project) else {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "project has no ownerWallet/submitterWallet linked for signed attaches"
            })),
        ));
    };
    if claimed != owner {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "signer is not the linked project owner wallet"
            })),
        ));
    }
    let message = attach_message(project_id, kind, url, ts);
    let recovered = recover_signer(message.as_bytes(), sig).map_err(|e| {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "success": false, "error": format!("wallet signature verification failed: {e}") })),
        )
    })?;
    if recovered != claimed {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "success": false,
                "error": "signature does not match the claimed owner address"
            })),
        ));
    }
    if let Err(resp) = crate::security_guard::check_signer(&recovered) {
        return Err(resp);
    }
    Ok(recovered)
}

#[derive(Deserialize)]
pub struct AttachDocumentRequest {
    title: String,
    url: String,
    #[serde(default, alias = "mimeType")]
    mime_type: Option<String>,
    #[serde(default, alias = "ownerAddress")]
    owner_address: Option<String>,
    timestamp: Option<i64>,
    signature: Option<String>,
}

#[derive(Deserialize)]
pub struct AttachMediaRequest {
    title: String,
    url: String,
    /// `photo` | `video` | `audio` | `other`
    #[serde(default = "default_media_kind")]
    kind: String,
    #[serde(default, alias = "ownerAddress")]
    owner_address: Option<String>,
    timestamp: Option<i64>,
    signature: Option<String>,
}

fn default_media_kind() -> String {
    "photo".into()
}

async fn append_project_attachment(
    id: &str,
    array_key: &str,
    entry: Value,
) -> Result<Value, (StatusCode, Json<Value>)> {
    let mut projects = load_projects_local().await;
    let Some(project) = projects
        .iter_mut()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(id))
    else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "project not found" })),
        ));
    };
    {
        let obj = project.as_object_mut().ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": "corrupt project record" })),
            )
        })?;
        let list = obj
            .entry(array_key.to_string())
            .or_insert_with(|| json!([]));
        let arr = list.as_array_mut().ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "success": false,
                    "error": format!("{array_key} must be an array")
                })),
            )
        })?;
        if arr.len() >= 50 {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "success": false,
                    "error": format!("{array_key} cap reached (50)")
                })),
            ));
        }
        arr.push(entry);
    }
    let updated = project.clone();
    if let Err(e) = save_projects_local(&projects).await {
        tracing::error!("project queue write failed on attach: {}", e);
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "queue write failed" })),
        ));
    }
    Ok(updated)
}

/// `POST /api/v1/projects/:id/documents` - owner or admin attaches a document URL.
pub async fn attach_project_document(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<AttachDocumentRequest>,
) -> (StatusCode, Json<Value>) {
    let now = chrono::Utc::now().timestamp();
    let title = payload.title.trim();
    let url = payload.url.trim();
    if title.is_empty() || url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "title and url are required" })),
        );
    }
    if let Err(resp) = crate::security_guard::validate_fields(&[("title", title)]) {
        return resp;
    }
    if let Err(resp) = crate::security_guard::validate_outbound_url("url", url) {
        return resp;
    }
    let projects = load_projects_local().await;
    let Some(project) = projects
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "project not found" })),
        );
    };
    let auth_actor = match authorize_project_attach(
        &state,
        &headers,
        project,
        &id,
        "document",
        url,
        payload.owner_address.as_deref(),
        payload.timestamp,
        payload.signature.as_deref(),
        now,
    )
    .await
    {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let digest = Keccak256::digest(format!("{id}|{url}|{now}").as_bytes());
    let entry = json!({
        "id": format!("doc-{}", hex::encode(&digest[..8])),
        "title": title,
        "url": url,
        "mimeType": payload.mime_type.unwrap_or_else(|| "application/octet-stream".into()),
        "addedAt": now,
        "addedBy": auth_actor,
    });
    match append_project_attachment(&id, "documents", entry.clone()).await {
        Ok(updated) => (
            StatusCode::CREATED,
            Json(json!({ "success": true, "document": entry, "project": updated })),
        ),
        Err(resp) => resp,
    }
}

/// `POST /api/v1/projects/:id/media` - owner or admin attaches a media URL.
pub async fn attach_project_media(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<AttachMediaRequest>,
) -> (StatusCode, Json<Value>) {
    let now = chrono::Utc::now().timestamp();
    let title = payload.title.trim();
    let url = payload.url.trim();
    let kind = payload.kind.trim().to_lowercase();
    if title.is_empty() || url.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "title and url are required" })),
        );
    }
    if !matches!(kind.as_str(), "photo" | "video" | "audio" | "other") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "kind must be photo, video, audio, or other"
            })),
        );
    }
    if let Err(resp) = crate::security_guard::validate_fields(&[("title", title)]) {
        return resp;
    }
    if let Err(resp) = crate::security_guard::validate_outbound_url("url", url) {
        return resp;
    }
    let projects = load_projects_local().await;
    let Some(project) = projects
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "project not found" })),
        );
    };
    let auth_actor = match authorize_project_attach(
        &state,
        &headers,
        project,
        &id,
        "media",
        url,
        payload.owner_address.as_deref(),
        payload.timestamp,
        payload.signature.as_deref(),
        now,
    )
    .await
    {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let digest = Keccak256::digest(format!("{id}|{url}|{now}").as_bytes());
    let entry = json!({
        "id": format!("media-{}", hex::encode(&digest[..8])),
        "title": title,
        "url": url,
        "kind": kind,
        "addedAt": now,
        "addedBy": auth_actor,
    });
    match append_project_attachment(&id, "media", entry.clone()).await {
        Ok(updated) => (
            StatusCode::CREATED,
            Json(json!({ "success": true, "media": entry, "project": updated })),
        ),
        Err(resp) => resp,
    }
}

// ============================================================================
// NGO nominations against a campaign / mandate vote
// ============================================================================

const NOMINATE_AUTH_DOMAIN: &str = "DCROPE-NGO-NOMINATE";
const NOMINATE_AUTH_WINDOW_SECS: i64 = 300;

fn nominate_message(project_id: &str, org_name: &str, timestamp: i64) -> String {
    let org = org_name.trim().to_lowercase();
    format!("{NOMINATE_AUTH_DOMAIN}\n{project_id}\n{org}\n{timestamp}")
}

fn nominations_open_for(project: &Value, status: &str) -> bool {
    if project
        .get("nominationsOpen")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return matches!(
            status,
            "voting" | "approved" | "awaiting_wallet" | "building" | "pending_review"
        );
    }
    // Meta-mandate causes default to open nomination while voting.
    let vote_class = project
        .get("voteClass")
        .or_else(|| project.get("vote_class"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let vote_is_meta = project
        .get("voteIsMeta")
        .or_else(|| project.get("vote_is_meta"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    vote_class.eq_ignore_ascii_case("cause") && vote_is_meta && status == "voting"
}

#[derive(Deserialize)]
pub struct NominateNgoRequest {
    #[serde(alias = "orgName")]
    org_name: String,
    #[serde(alias = "legalEntity")]
    legal_entity: String,
    mission: String,
    #[serde(default)]
    impact: Option<String>,
    #[serde(default, alias = "requestedAmount")]
    requested_amount: Option<String>,
    #[serde(default = "default_nom_currency", alias = "requestedCurrency")]
    requested_currency: String,
    #[serde(default)]
    milestones: Option<String>,
    #[serde(default)]
    references: Option<String>,
    #[serde(default)]
    website: Option<String>,
    #[serde(alias = "nominatorName")]
    nominator_name: String,
    #[serde(default, alias = "nominatorEmail")]
    nominator_email: Option<String>,
    #[serde(default, alias = "contactEmail")]
    contact_email: Option<String>,
    #[serde(default, alias = "contactName")]
    contact_name: Option<String>,
    #[serde(alias = "nominatorAddress")]
    nominator_address: String,
    timestamp: i64,
    signature: String,
}

fn default_nom_currency() -> String {
    "DC".into()
}

/// `GET /api/v1/projects/:id/nominations`
pub async fn list_project_nominations(Path(id): Path<String>) -> (StatusCode, Json<Value>) {
    let nominations = nominations_for_project(&id).await;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "projectId": id,
            "count": nominations.len(),
            "nominations": nominations,
        })),
    )
}

/// `POST /api/v1/projects/:id/nominations` - wallet-signed NGO proposal.
pub async fn nominate_ngo_for_project(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<NominateNgoRequest>,
) -> (StatusCode, Json<Value>) {
    let now = chrono::Utc::now().timestamp();
    let org_name = payload.org_name.trim();
    let legal_entity = payload.legal_entity.trim();
    let mission = payload.mission.trim();
    let nominator_name = payload.nominator_name.trim();
    if org_name.is_empty() || legal_entity.is_empty() || mission.is_empty() || nominator_name.is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "orgName, legalEntity, mission, and nominatorName are required"
            })),
        );
    }
    if org_name.len() > 160 || legal_entity.len() > 200 || mission.len() > 4000 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "field too long" })),
        );
    }
    if let Err(resp) = crate::security_guard::validate_fields(&[
        ("orgName", org_name),
        ("legalEntity", legal_entity),
        ("mission", mission),
        ("impact", payload.impact.as_deref().unwrap_or("")),
        ("milestones", payload.milestones.as_deref().unwrap_or("")),
        ("references", payload.references.as_deref().unwrap_or("")),
        ("nominatorName", nominator_name),
        ("nominatorEmail", payload.nominator_email.as_deref().unwrap_or("")),
        ("contactEmail", payload.contact_email.as_deref().unwrap_or("")),
        ("contactName", payload.contact_name.as_deref().unwrap_or("")),
    ]) {
        return resp;
    }
    if let Some(site) = payload.website.as_deref() {
        if let Err(resp) = crate::security_guard::validate_outbound_url("website", site) {
            return resp;
        }
    }
    for email in [
        payload.nominator_email.as_deref(),
        payload.contact_email.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let e = email.trim();
        if !e.is_empty() && !looks_like_email(e) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": "invalid email address" })),
            );
        }
    }

    if (now - payload.timestamp).abs() > NOMINATE_AUTH_WINDOW_SECS {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "success": false,
                "error": format!(
                    "timestamp outside ±{NOMINATE_AUTH_WINDOW_SECS}s freshness window - sign again"
                )
            })),
        );
    }
    let claimed = payload.nominator_address.trim().to_lowercase();
    if !claimed.starts_with("0x") || claimed.len() != 42 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "nominatorAddress must be a 0x-prefixed 20-byte hex address"
            })),
        );
    }
    let message = nominate_message(&id, org_name, payload.timestamp);
    let recovered = match recover_signer(message.as_bytes(), &payload.signature) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "success": false,
                    "error": format!("wallet signature verification failed: {e}")
                })),
            );
        }
    };
    if recovered != claimed {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "success": false,
                "error": "signature does not match the claimed nominatorAddress"
            })),
        );
    }
    if let Err(resp) = crate::security_guard::check_signer(&recovered) {
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
    let raw_status = project
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending_review");
    let voting_ends_at = project.get("votingEndsAt").and_then(|v| v.as_i64());
    let status = effective_status(raw_status, voting_ends_at, &tally, now);
    if !nominations_open_for(project, &status) {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": format!("NGO nominations are not open for this project (status: {status})")
            })),
        );
    }

    let existing = load_nominations_local().await;
    let dup = existing.iter().any(|n| {
        n.get("projectId").and_then(|v| v.as_str()) == Some(id.as_str())
            && n.get("nominatorAddress")
                .and_then(|v| v.as_str())
                .map(|a| a.eq_ignore_ascii_case(&recovered))
                .unwrap_or(false)
            && n.get("orgName")
                .and_then(|v| v.as_str())
                .map(|o| o.eq_ignore_ascii_case(org_name))
                .unwrap_or(false)
    });
    if dup {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": "this wallet has already nominated this organisation for this campaign"
            })),
        );
    }
    if existing
        .iter()
        .filter(|n| n.get("projectId").and_then(|v| v.as_str()) == Some(id.as_str()))
        .count()
        >= 200
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "nomination cap reached for this campaign (200)" })),
        );
    }

    let digest = Keccak256::digest(format!("{id}|{org_name}|{recovered}|{now}").as_bytes());
    let nom_id = format!("nom-{}", hex::encode(&digest[..8]));
    let record = json!({
        "id": nom_id,
        "projectId": id,
        "orgName": org_name,
        "legalEntity": legal_entity,
        "mission": mission,
        "impact": payload.impact.as_deref().unwrap_or("").trim(),
        "requestedAmount": payload.requested_amount.as_deref().unwrap_or("").trim(),
        "requestedCurrency": payload.requested_currency.trim(),
        "milestones": payload.milestones.as_deref().unwrap_or("").trim(),
        "references": payload.references.as_deref().unwrap_or("").trim(),
        "website": payload.website.as_deref().unwrap_or("").trim(),
        "nominatorName": nominator_name,
        "nominatorEmail": payload.nominator_email.as_deref().unwrap_or("").trim(),
        "contactName": payload.contact_name.as_deref().unwrap_or("").trim(),
        "contactEmail": payload.contact_email.as_deref().unwrap_or("").trim(),
        "nominatorAddress": recovered,
        "createdAt": now,
        "status": "submitted",
    });

    if let Err(e) = append_nomination_local(&record).await {
        tracing::error!("nomination write failed: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "nomination write failed" })),
        );
    }

    let knot = anchor_governance_event(
        &state,
        "NgoNominated",
        &sanitize_nomination_public(&record),
        json!({
            "project_id": id,
            "nomination_id": nom_id,
            "nominator": recovered,
            "org_name": org_name,
        }),
    )
    .await;

    let mut public = sanitize_nomination_public(&record);
    if let Some(obj) = public.as_object_mut() {
        if let Some(h) = knot {
            obj.insert("knotHash".into(), json!(h));
        }
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "success": true,
            "nomination": public,
            "message": "NGO nomination recorded. It remains a candidate for the residual DC allocation; this campaign vote establishes community authority, not the final beneficiary."
        })),
    )
}

// ============================================================================
// Owner lifecycle - create is submit_project; edit / publish / archive /
// resubmit are owner-wallet signed (or X-Admin-Token). Domain-separated
// from vote and attach signatures so a ballot signature cannot be replayed
// as an archive.
// ============================================================================

const LIFECYCLE_AUTH_DOMAIN: &str = "DCROPE-PROJECT-LIFECYCLE";
const LIFECYCLE_AUTH_WINDOW_SECS: i64 = 300;

fn lifecycle_message(action: &str, project_id: &str, timestamp: i64) -> String {
    format!("{LIFECYCLE_AUTH_DOMAIN}\n{action}\n{project_id}\n{timestamp}")
}

async fn authorize_project_owner(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    project: &Value,
    project_id: &str,
    action: &str,
    owner_address: Option<&str>,
    timestamp: Option<i64>,
    signature: Option<&str>,
    now: i64,
) -> Result<String, (StatusCode, Json<Value>)> {
    if admin_token_matches(state, headers).await {
        return Ok("admin".into());
    }
    let (addr, ts, sig) = match (owner_address, timestamp, signature) {
        (Some(a), Some(t), Some(s)) if !a.is_empty() && !s.is_empty() => (a, t, s),
        _ => {
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({
                    "success": false,
                    "error": "lifecycle requires X-Admin-Token or a signed owner-wallet proof"
                })),
            ));
        }
    };
    if (now - ts).abs() > LIFECYCLE_AUTH_WINDOW_SECS {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "success": false,
                "error": format!(
                    "timestamp outside ±{LIFECYCLE_AUTH_WINDOW_SECS}s freshness window - sign again"
                )
            })),
        ));
    }
    let claimed = addr.to_lowercase();
    let Some(owner) = project_owner_wallet(project) else {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "project has no ownerWallet/submitterWallet linked"
            })),
        ));
    };
    if claimed != owner {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "signer is not the linked project owner wallet"
            })),
        ));
    }
    let message = lifecycle_message(action, project_id, ts);
    let recovered = recover_signer(message.as_bytes(), sig).map_err(|e| {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "success": false,
                "error": format!("wallet signature verification failed: {e}")
            })),
        )
    })?;
    if recovered != claimed {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "success": false,
                "error": "signature does not match the claimed owner address"
            })),
        ));
    }
    if let Err(resp) = crate::security_guard::check_signer(&recovered) {
        return Err(resp);
    }
    Ok(recovered)
}

#[derive(Deserialize)]
pub struct UpdateProjectRequest {
    #[serde(default, alias = "ownerAddress")]
    owner_address: Option<String>,
    timestamp: Option<i64>,
    signature: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tagline: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    mission: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    stage: Option<String>,
    #[serde(default, alias = "organizationName")]
    organization_name: Option<String>,
    #[serde(default, alias = "fundingBreakdown")]
    funding_breakdown: Option<String>,
    #[serde(default, alias = "fundingAskLabel")]
    funding_ask_label: Option<String>,
    #[serde(default, alias = "heroImage")]
    hero_image: Option<String>,
    #[serde(default)]
    features: Option<Vec<Value>>,
    #[serde(default, alias = "useCases")]
    use_cases: Option<String>,
    #[serde(default)]
    milestones: Option<Vec<Value>>,
    #[serde(default, alias = "techStack")]
    tech_stack: Option<Vec<String>>,
    #[serde(default, alias = "techStackDetail")]
    tech_stack_detail: Option<String>,
    #[serde(default, alias = "architectureDescription")]
    architecture_description: Option<String>,
    #[serde(default, alias = "ownerWallet")]
    owner_wallet: Option<String>,
}

/// `PATCH /api/v1/projects/:id` - owner edits campaign copy / metadata.
/// Tally fields and vote windows are not owner-writable.
pub async fn update_project(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<UpdateProjectRequest>,
) -> (StatusCode, Json<Value>) {
    let now = chrono::Utc::now().timestamp();
    let mut projects = load_projects_local().await;
    let Some(idx) = projects
        .iter()
        .position(|p| p.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "project not found" })),
        );
    };
    let project = projects[idx].clone();
    let status = project
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending_review");
    // While live voting is open, only non-tally copy may change; terminal
    // archive/approved states still allow corrections until resubmit.
    let editable = matches!(
        status,
        "pending_review"
            | "draft"
            | "archived"
            | "rejected"
            | "rejected_by_admin"
            | "rejected_no_quorum"
            | "voting"
            | "approved"
            | "building"
            | "awaiting_wallet"
    );
    if !editable {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": format!("project cannot be edited in status {status}")
            })),
        );
    }
    if let Err(e) = authorize_project_owner(
        &state,
        &headers,
        &project,
        &id,
        "update",
        payload.owner_address.as_deref(),
        payload.timestamp,
        payload.signature.as_deref(),
        now,
    )
    .await
    {
        return e;
    }

    let obj = projects[idx].as_object_mut().unwrap();
    let mut set_str = |key: &str, val: Option<&String>| {
        if let Some(v) = val {
            let t = v.trim();
            if !t.is_empty() {
                obj.insert(key.into(), json!(t));
            }
        }
    };
    set_str("name", payload.name.as_ref());
    set_str("tagline", payload.tagline.as_ref());
    set_str("description", payload.description.as_ref());
    set_str("mission", payload.mission.as_ref());
    set_str("category", payload.category.as_ref());
    set_str("stage", payload.stage.as_ref());
    set_str("organizationName", payload.organization_name.as_ref());
    set_str("fundingBreakdown", payload.funding_breakdown.as_ref());
    set_str("fundingAskLabel", payload.funding_ask_label.as_ref());
    set_str("heroImage", payload.hero_image.as_ref());
    set_str("useCases", payload.use_cases.as_ref());
    set_str("techStackDetail", payload.tech_stack_detail.as_ref());
    set_str(
        "architectureDescription",
        payload.architecture_description.as_ref(),
    );
    if let Some(feats) = payload.features {
        obj.insert("features".into(), json!(feats));
    }
    if let Some(ms) = payload.milestones {
        obj.insert("milestones".into(), json!(ms));
    }
    if let Some(ts) = payload.tech_stack {
        obj.insert("techStack".into(), json!(ts));
    }
    // Admin-only owner wallet bind (or first-time bind when unset).
    if let Some(ow) = payload.owner_wallet.as_deref() {
        let ow = ow.trim().to_lowercase();
        if ow.starts_with("0x") && ow.len() == 42 {
            let can_set = admin_token_matches(&state, &headers).await
                || project_owner_wallet(&project).is_none();
            if can_set {
                obj.insert("ownerWallet".into(), json!(ow));
                obj.insert("submitterWallet".into(), json!(ow));
            }
        }
    }
    obj.insert("updatedAt".into(), json!(now));

    if let Err(e) = save_projects_local(&projects).await {
        tracing::error!("update_project save failed: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "project update failed" })),
        );
    }

    let knot = anchor_governance_event(
        &state,
        "ProjectUpdated",
        &projects[idx],
        json!({ "project_id": id, "action": "update" }),
    )
    .await;

    let ballots = load_ballots(&state).await;
    let mut live = attach_live_view(projects[idx].clone(), &ballots, now);
    if let Some(o) = live.as_object_mut() {
        if let Some(h) = knot {
            o.insert("knotHash".into(), json!(h));
        }
    }
    (
        StatusCode::OK,
        Json(json!({ "success": true, "project": live })),
    )
}

#[derive(Deserialize)]
pub struct LifecycleRequest {
    /// `publish` | `archive` | `resubmit`
    action: String,
    #[serde(default, alias = "ownerAddress")]
    owner_address: Option<String>,
    timestamp: Option<i64>,
    signature: Option<String>,
    /// Optional: bind owner wallet when using admin token (Foundation causes).
    #[serde(default, alias = "ownerWallet")]
    owner_wallet: Option<String>,
}

/// `POST /api/v1/projects/:id/lifecycle` - publish / archive / resubmit.
pub async fn project_lifecycle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<LifecycleRequest>,
) -> (StatusCode, Json<Value>) {
    let now = chrono::Utc::now().timestamp();
    let action = payload.action.trim().to_ascii_lowercase();
    if !matches!(action.as_str(), "publish" | "archive" | "resubmit") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "action must be publish|archive|resubmit"
            })),
        );
    }

    let mut projects = load_projects_local().await;
    let Some(idx) = projects
        .iter()
        .position(|p| p.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "project not found" })),
        );
    };

    // Optional admin bind of owner before auth (Foundation bootstrap).
    if admin_token_matches(&state, &headers).await {
        if let Some(ow) = payload.owner_wallet.as_deref() {
            let ow = ow.trim().to_lowercase();
            if ow.starts_with("0x") && ow.len() == 42 {
                if let Some(obj) = projects[idx].as_object_mut() {
                    obj.insert("ownerWallet".into(), json!(ow));
                    obj.insert("submitterWallet".into(), json!(ow));
                }
            }
        }
    }

    let project = projects[idx].clone();
    if let Err(e) = authorize_project_owner(
        &state,
        &headers,
        &project,
        &id,
        &action,
        payload.owner_address.as_deref(),
        payload.timestamp,
        payload.signature.as_deref(),
        now,
    )
    .await
    {
        return e;
    }

    let status = project
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending_review");
    let next_status = match action.as_str() {
        "publish" => {
            if !matches!(status, "pending_review" | "draft" | "archived") {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "success": false,
                        "error": format!("cannot publish from status {status}")
                    })),
                );
            }
            // Causes / projects move straight to voting for 7 days (same
            // window as review_project approve path).
            "voting"
        }
        "archive" => {
            if status == "voting" {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "success": false,
                        "error": "cannot archive while voting is open - wait for the window to close or reject via admin review"
                    })),
                );
            }
            "archived"
        }
        "resubmit" => {
            if !matches!(
                status,
                "archived" | "rejected" | "rejected_by_admin" | "rejected_no_quorum"
            ) {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "success": false,
                        "error": format!("cannot resubmit from status {status}")
                    })),
                );
            }
            "pending_review"
        }
        _ => unreachable!(),
    };

    {
        let obj = projects[idx].as_object_mut().unwrap();
        obj.insert("status".into(), json!(next_status));
        obj.insert("updatedAt".into(), json!(now));
        obj.insert("lifecycleAction".into(), json!(action));
        if next_status == "voting" {
            let days = std::env::var("VOTING_PERIOD_DAYS")
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(7)
                .max(1);
            obj.insert("votingStartedAt".into(), json!(now));
            obj.insert("votingEndsAt".into(), json!(now + days * 86_400));
            obj.remove("votingClosedAt");
            obj.remove("closedReason");
        }
        if next_status == "pending_review" {
            obj.remove("votingStartedAt");
            obj.remove("votingEndsAt");
            obj.remove("votingClosedAt");
            obj.remove("closedReason");
            obj.insert("resubmittedAt".into(), json!(now));
        }
        if next_status == "archived" {
            obj.insert("archivedAt".into(), json!(now));
        }
    }

    if let Err(e) = save_projects_local(&projects).await {
        tracing::error!("project_lifecycle save failed: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "lifecycle update failed" })),
        );
    }

    let event_type = match action.as_str() {
        "publish" => "ProjectPublished",
        "archive" => "ProjectArchived",
        "resubmit" => "ProjectResubmitted",
        _ => "ProjectLifecycle",
    };
    let knot = anchor_governance_event(
        &state,
        event_type,
        &projects[idx],
        json!({ "project_id": id, "action": action, "status": next_status }),
    )
    .await;

    let ballots = load_ballots(&state).await;
    let mut live = attach_live_view(projects[idx].clone(), &ballots, now);
    if let Some(o) = live.as_object_mut() {
        if let Some(h) = knot {
            o.insert("knotHash".into(), json!(h));
        }
    }
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "action": action,
            "project": live
        })),
    )
}
