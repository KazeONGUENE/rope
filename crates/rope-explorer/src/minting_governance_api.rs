//! CriticalProtocol → MintingGovernance bridge (production runtime).
//!
//! When a `VoteEscrow` vote of class `CriticalProtocol` finalises as
//! `Approved`, this module opens a durable staged minting proposal that
//! mirrors `rope-smartchain::governance::MintingGovernance`:
//!
//!   PendingAI → PendingGovernors → PendingFoundation → Approved → Executed
//!
//! VoteEscrow never mints. Timelock schedule/execute happens only after
//! the staged pipeline reaches `Approved`, via an explicit operator-
//! authenticated mark (or prepare payload for `DCSwapTimelock.schedule`).
//!
//! Persistence: JSONL at `MINTING_PROPOSALS_PATH` (default
//! `/opt/datachain-rope/minting-proposals.jsonl`), atomically rewritten
//! on every state transition. Each open/approve/execute anchors a knot
//! on the governance ledger wallet.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha3::{Digest, Keccak256};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::governance_votes::{anchor_governance_event, get_project_by_id, patch_project_fields, recover_signer};
use crate::vote_escrow_api::fetch_decoded_vote;
use crate::AppState;

const AUTH_DOMAIN: &str = "DCROPE-MINTING-GOV";
const AUTH_WINDOW_SECS: i64 = 300;

const DEFAULT_AI_AGENTS: &[&str] = &[
    "0x000000000000000000000000000000000000c001",
    "0x000000000000000000000000000000000000c002",
    "0x000000000000000000000000000000000000c003",
    "0x000000000000000000000000000000000000c004",
    "0x000000000000000000000000000000000000c005",
];

const DEFAULT_VALIDATORS: &[&str] = &[
    "0x000000000000000000000000000000000000c001",
    "0x000000000000000000000000000000000000c002",
    "0x000000000000000000000000000000000000c003",
    "0x000000000000000000000000000000000000c004",
    "0x000000000000000000000000000000000000c005",
    "0x302fa11a6e784dfa89f96942a919c09b45559676",
];

/// Compromised historical deployer - refused for every privileged role.
const COMPROMISED_DEPLOYER: &str = "0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195";

/// Process-wide write lock so concurrent stage submissions cannot race
/// the JSONL rewrite.
static STORE_LOCK: RwLock<()> = RwLock::const_new(());

fn proposals_path() -> PathBuf {
    PathBuf::from(
        std::env::var("MINTING_PROPOSALS_PATH")
            .unwrap_or_else(|_| "/opt/datachain-rope/minting-proposals.jsonl".into()),
    )
}

fn required_ai() -> usize {
    std::env::var("MINTING_GOV_REQUIRED_AI")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
}

fn required_governors() -> usize {
    std::env::var("MINTING_GOV_REQUIRED_GOVERNORS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
}

fn required_foundation() -> usize {
    std::env::var("MINTING_GOV_REQUIRED_FOUNDATION")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2)
}

fn voting_timeout_secs() -> i64 {
    std::env::var("MINTING_GOV_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(86_400)
}

fn normalize_addr(a: &str) -> Result<String, String> {
    let s = a.trim().to_lowercase();
    if !s.starts_with("0x") || s.len() != 42 {
        return Err(format!("invalid address: {a}"));
    }
    if hex::decode(&s[2..]).is_err() {
        return Err(format!("invalid address hex: {a}"));
    }
    if s == COMPROMISED_DEPLOYER {
        return Err("compromised deployer address is refused".into());
    }
    Ok(s)
}

fn addr_to_bytes32(addr: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    if let Ok(bytes) = hex::decode(addr.trim_start_matches("0x")) {
        if bytes.len() == 20 {
            out[12..].copy_from_slice(&bytes);
        } else if bytes.len() == 32 {
            out.copy_from_slice(&bytes);
        }
    }
    out
}

fn bytes32_to_hex(b: &[u8; 32]) -> String {
    format!("0x{}", hex::encode(b))
}

fn parse_addr_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .filter_map(|s| normalize_addr(s).ok())
        .collect()
}

fn validator_pool() -> Vec<String> {
    let from_env = std::env::var("MINTING_GOVERNANCE_VALIDATORS")
        .ok()
        .map(|s| parse_addr_list(&s))
        .unwrap_or_default();
    if from_env.len() >= required_governors() {
        return from_env;
    }
    let mut set: HashSet<String> = from_env.into_iter().collect();
    for a in DEFAULT_VALIDATORS {
        if let Ok(n) = normalize_addr(a) {
            set.insert(n);
        }
    }
    let mut v: Vec<_> = set.into_iter().collect();
    v.sort();
    v
}

fn foundation_pool() -> Vec<(String, String)> {
    // Format: `0xaddr:Role,0xaddr2:Role` or bare addresses (role=Board).
    // Env overrides; otherwise seed the two Foundation EOAs already used
    // in production ops (Timelock interim admin + guardian).
    let raw = std::env::var("MINTING_GOVERNANCE_FOUNDATION").unwrap_or_else(|_| {
        "0xa0A503D3FeE4682B5d914efdcC97A8Dc00568144:Executive,0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb:Board".into()
    });
    let mut out = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (addr, role) = match part.split_once(':') {
            Some((a, r)) => (a, r.trim().to_string()),
            None => (part, "Board".to_string()),
        };
        if let Ok(a) = normalize_addr(addr) {
            out.push((a, role));
        }
    }
    out
}

fn ai_agent_set() -> HashSet<String> {
    let mut set: HashSet<String> = HashSet::new();
    if let Ok(raw) = std::env::var("MINTING_GOVERNANCE_AI_AGENTS") {
        for a in parse_addr_list(&raw) {
            set.insert(a);
        }
    }
    if set.is_empty() {
        for a in DEFAULT_AI_AGENTS {
            if let Ok(n) = normalize_addr(a) {
                set.insert(n);
            }
        }
    }
    set
}

fn load_proposals_blocking() -> Vec<Value> {
    let path = proposals_path();
    std::fs::read_to_string(&path)
        .map(|content| {
            content
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn save_proposals_blocking(rows: &[Value]) -> Result<(), String> {
    let path = proposals_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
    std::fs::write(&tmp, body).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

async fn with_store<F, T>(f: F) -> T
where
    F: FnOnce(&mut Vec<Value>) -> T,
{
    let _guard = STORE_LOCK.write().await;
    let mut rows = load_proposals_blocking();
    let out = f(&mut rows);
    if let Err(e) = save_proposals_blocking(&rows) {
        tracing::error!("minting proposals persist failed: {e}");
    }
    out
}

fn select_governors(entropy: &[u8; 32]) -> Result<Value, String> {
    let validators = validator_pool();
    let foundation = foundation_pool();
    let n_gov = required_governors();
    let n_found = required_foundation();
    if validators.len() < n_gov {
        return Err(format!(
            "Insufficient validators: {n_gov} required, {} available (set MINTING_GOVERNANCE_VALIDATORS)",
            validators.len()
        ));
    }
    if foundation.len() < n_found {
        return Err(format!(
            "Insufficient foundation members: {n_found} required, {} available (set MINTING_GOVERNANCE_FOUNDATION)",
            foundation.len()
        ));
    }

    let mut selected = Vec::new();
    let mut selection_state = *entropy;
    for i in 0..n_gov {
        selection_state =
            *blake3::hash(&[&selection_state[..], &(i as u32).to_le_bytes()].concat()).as_bytes();
        let index =
            u64::from_le_bytes(selection_state[0..8].try_into().unwrap()) as usize % validators.len();
        let mut candidate = validators[index].clone();
        let mut attempts = 0u32;
        while selected.iter().any(|s: &String| s == &candidate) && attempts < 100 {
            selection_state = *blake3::hash(&selection_state).as_bytes();
            let new_index = u64::from_le_bytes(selection_state[0..8].try_into().unwrap()) as usize
                % validators.len();
            candidate = validators[new_index].clone();
            attempts += 1;
        }
        selected.push(candidate);
    }

    let foundation_wallets: Vec<String> = foundation
        .iter()
        .take(n_found)
        .map(|(a, _)| a.clone())
        .collect();

    let now = chrono::Utc::now().timestamp();
    let selection_proof =
        *blake3::hash(&[entropy.as_slice(), &now.to_le_bytes()].concat()).as_bytes();

    Ok(json!({
        "randomGovernors": selected,
        "foundationMembers": foundation_wallets,
        "selectedAt": now,
        "selectionProof": bytes32_to_hex(&selection_proof),
        "expiresAt": now + voting_timeout_secs(),
    }))
}

fn proposal_id_bytes(
    token_id: &[u8; 32],
    amount: u128,
    recipient: &[u8; 32],
    now: i64,
) -> [u8; 32] {
    *blake3::hash(
        &[
            &token_id[..],
            &amount.to_le_bytes(),
            &recipient[..],
            &now.to_le_bytes(),
        ]
        .concat(),
    )
    .as_bytes()
}

fn entropy_from_vote(vote_id: u64, metadata_hash: &str, project_id: &str) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(b"DCROPE/critical-protocol/minting-entropy/v1");
    h.update(vote_id.to_le_bytes());
    h.update(metadata_hash.as_bytes());
    h.update(project_id.as_bytes());
    let out = h.finalize();
    let mut e = [0u8; 32];
    e.copy_from_slice(&out);
    e
}

fn find_by_escrow(rows: &[Value], vote_id: u64) -> Option<&Value> {
    rows.iter()
        .find(|r| r.get("escrowVoteId").and_then(|v| v.as_u64()) == Some(vote_id))
}

fn find_by_id<'a>(rows: &'a [Value], id: &str) -> Option<&'a Value> {
    let needle = id.trim().to_lowercase();
    rows.iter().find(|r| {
        r.get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.eq_ignore_ascii_case(&needle))
            .unwrap_or(false)
    })
}

fn parse_amount_u128(raw: &str) -> Result<u128, String> {
    let s = raw.trim();
    if s.starts_with("0x") || s.starts_with("0X") {
        u128::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16)
            .map_err(|e| format!("amount hex: {e}"))
    } else {
        s.parse::<u128>()
            .map_err(|e| format!("amount decimal: {e}"))
    }
}

/// MintingGovernance timelock-record admin gate. Since 2026-08-14 this
/// delegates to [`crate::admin_tokens::require_role`] with the
/// `ProjectAdmin` role (a `MultiRole` token also satisfies the check
/// per [`crate::admin_tokens::Role::grants`]). Env-var escape hatches
/// (`PROJECTS_ADMIN_TOKEN`) are no longer consulted; only dynamic
/// admin tokens minted through `/api/v1/admin-tokens/*` work.
async fn admin_token_ok(
    state: &Arc<AppState>,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<Value>)> {
    crate::admin_tokens::require_role(
        &state.admin_tokens,
        headers,
        crate::admin_tokens::Role::ProjectAdmin,
    )
    .await
}

fn verify_stage_sig(
    stage: &str,
    proposal_id: &str,
    approved: bool,
    timestamp: i64,
    signer: &str,
    signature: &str,
) -> Result<String, String> {
    let now = chrono::Utc::now().timestamp();
    if (now - timestamp).abs() > AUTH_WINDOW_SECS {
        return Err(format!(
            "timestamp outside ±{AUTH_WINDOW_SECS}s freshness window - sign again"
        ));
    }
    let claimed = normalize_addr(signer)?;
    let message = format!(
        "{AUTH_DOMAIN}\n{stage}\n{}\n{approved}\n{timestamp}",
        proposal_id.to_lowercase()
    );
    let recovered = recover_signer(message.as_bytes(), signature)?;
    if recovered != claimed {
        return Err("signature does not match claimed signer".into());
    }
    Ok(claimed)
}

fn public_view(p: &Value) -> Value {
    let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("?");
    let ai = p
        .get("aiApprovals")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter(|x| x.get("approved").and_then(|b| b.as_bool()) == Some(true)).count())
        .unwrap_or(0);
    let gov = p
        .get("governorApprovals")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter(|x| x.get("approved").and_then(|b| b.as_bool()) == Some(true)).count())
        .unwrap_or(0);
    let found = p
        .get("foundationApprovals")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter(|x| x.get("approved").and_then(|b| b.as_bool()) == Some(true)).count())
        .unwrap_or(0);
    json!({
        "id": p.get("id"),
        "escrowVoteId": p.get("escrowVoteId"),
        "projectId": p.get("projectId"),
        "tokenId": p.get("tokenId"),
        "amount": p.get("amount"),
        "recipient": p.get("recipient"),
        "reason": p.get("reason"),
        "proposer": p.get("proposer"),
        "createdAt": p.get("createdAt"),
        "status": status,
        "pipeline": {
            "ai": { "approved": ai, "required": required_ai(), "stage": "PendingAI" },
            "governors": { "approved": gov, "required": required_governors(), "stage": "PendingGovernors" },
            "foundation": { "approved": found, "required": required_foundation(), "stage": "PendingFoundation" },
        },
        "governorSelection": p.get("governorSelection"),
        "aiApprovals": p.get("aiApprovals"),
        "governorApprovals": p.get("governorApprovals"),
        "foundationApprovals": p.get("foundationApprovals"),
        "timelockReady": status == "Approved" || status == "Scheduled" || status == "Executed",
        "timelockOpId": p.get("timelockOpId"),
        "timelockTxHash": p.get("timelockTxHash"),
        "executedTxHash": p.get("executedTxHash"),
        "openKnotHash": p.get("openKnotHash"),
        "requirements": {
            "aiAgents": required_ai(),
            "randomGovernors": required_governors(),
            "foundationMembers": required_foundation(),
            "totalRequired": required_ai() + required_governors() + required_foundation(),
        }
    })
}

// ============================================================================
// HTTP handlers
// ============================================================================

/// `GET /api/v1/governance/minting` - list proposals + registry readiness.
pub async fn list_proposals(State(_state): State<Arc<AppState>>) -> Json<Value> {
    let rows = load_proposals_blocking();
    let validators = validator_pool();
    let foundation = foundation_pool();
    Json(json!({
        "success": true,
        "count": rows.len(),
        "proposals": rows.iter().map(public_view).collect::<Vec<_>>(),
        "registry": {
            "validators": validators.len(),
            "foundationMembers": foundation.len(),
            "aiAgents": ai_agent_set().len(),
            "ready": validators.len() >= required_governors()
                && foundation.len() >= required_foundation(),
        },
        "note": "CriticalProtocol VoteEscrow Approved → open proposal → AI → random governors → Foundation → Timelock"
    }))
}

/// `GET /api/v1/governance/minting/:id`
pub async fn get_proposal(Path(id): Path<String>) -> (StatusCode, Json<Value>) {
    let rows = load_proposals_blocking();
    match find_by_id(&rows, &id) {
        Some(p) => (StatusCode::OK, Json(json!({ "success": true, "proposal": public_view(p) }))),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "proposal not found" })),
        ),
    }
}

#[derive(Deserialize)]
pub struct OpenFromEscrowRequest {
    escrow_vote_id: u64,
    #[serde(default)]
    project_id: Option<String>,
    /// Optional override when project JSONL lacks mint fields.
    #[serde(default)]
    recipient: Option<String>,
    #[serde(default)]
    amount: Option<String>,
    #[serde(default)]
    token_id: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /api/v1/governance/minting/open-from-escrow`
///
/// Reads live VoteEscrow state. Opens a MintingGovernance proposal only
/// when `voteClass == CriticalProtocol`, `finalized`, and `outcome == Approved`.
pub async fn open_from_escrow(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<OpenFromEscrowRequest>,
) -> (StatusCode, Json<Value>) {
    let vote = match fetch_decoded_vote(&state, payload.escrow_vote_id).await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "success": false, "error": e })),
            );
        }
    };

    let class_id = vote.get("voteClassId").and_then(|v| v.as_u64()).unwrap_or(999);
    let finalized = vote.get("finalized").and_then(|v| v.as_bool()).unwrap_or(false);
    let outcome = vote.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
    if class_id != 2 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": format!("vote is {:?}, not CriticalProtocol", vote.get("voteClass")),
            })),
        );
    }
    if !finalized || outcome != "Approved" {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": format!("vote not Approved yet (finalized={finalized}, outcome={outcome})"),
            })),
        );
    }

    {
        let rows = load_proposals_blocking();
        if let Some(existing) = find_by_escrow(&rows, payload.escrow_vote_id) {
            return (
                StatusCode::OK,
                Json(json!({
                    "success": true,
                    "already_open": true,
                    "proposal": public_view(existing),
                })),
            );
        }
    }

    let project_id = payload
        .project_id
        .clone()
        .or_else(|| {
            // Resolve via escrow link on project JSONL.
            None
        })
        .unwrap_or_default();

    let project = if !project_id.is_empty() {
        get_project_by_id(&state, &project_id).await
    } else {
        crate::governance_votes::find_project_by_escrow_vote_id(&state, payload.escrow_vote_id)
            .await
    };

    let project_id = project
        .as_ref()
        .and_then(|p| p.get("id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    let recipient_raw = payload
        .recipient
        .as_deref()
        .or_else(|| {
            project
                .as_ref()
                .and_then(|p| p.get("mintRecipient").and_then(|v| v.as_str()))
        })
        .or_else(|| {
            project
                .as_ref()
                .and_then(|p| p.get("submitterWallet").and_then(|v| v.as_str()))
        })
        .unwrap_or("");
    let recipient = match normalize_addr(recipient_raw) {
        Ok(a) => a,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "success": false,
                    "error": "mint recipient required (project.mintRecipient or request.recipient)"
                })),
            );
        }
    };

    let amount_raw = payload
        .amount
        .as_deref()
        .or_else(|| {
            project
                .as_ref()
                .and_then(|p| p.get("mintAmount").and_then(|v| v.as_str()))
        })
        .unwrap_or("0");
    let amount = match parse_amount_u128(amount_raw) {
        Ok(a) if a > 0 => a,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "success": false,
                    "error": "mint amount required (project.mintAmount or request.amount, wei)"
                })),
            );
        }
    };

    let token_raw = payload
        .token_id
        .as_deref()
        .or_else(|| {
            project
                .as_ref()
                .and_then(|p| p.get("mintTokenId").and_then(|v| v.as_str()))
        })
        .unwrap_or(
            "0x0000000000000000000000000000000000000000000000000000000000000000",
        );
    let token_bytes = {
        let hex = token_raw.trim_start_matches("0x");
        let decoded = hex::decode(hex).unwrap_or_default();
        let mut t = [0u8; 32];
        if decoded.len() == 32 {
            t.copy_from_slice(&decoded);
        } else if decoded.len() == 20 {
            t[12..].copy_from_slice(&decoded);
        }
        t
    };

    let reason = payload
        .reason
        .clone()
        .or_else(|| {
            project
                .as_ref()
                .and_then(|p| p.get("mintReason").and_then(|v| v.as_str()).map(|s| s.to_string()))
        })
        .or_else(|| {
            project
                .as_ref()
                .and_then(|p| p.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()))
        })
        .unwrap_or_else(|| format!("CriticalProtocol escrow vote {}", payload.escrow_vote_id));

    let metadata_hash = vote
        .get("metadataHash")
        .and_then(|v| v.as_str())
        .unwrap_or("0x");
    let proposer = vote
        .get("creator")
        .and_then(|v| v.as_str())
        .unwrap_or("0x0000000000000000000000000000000000000000")
        .to_lowercase();

    let entropy = entropy_from_vote(payload.escrow_vote_id, metadata_hash, &project_id);
    let selection = match select_governors(&entropy) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "success": false, "error": e })),
            );
        }
    };

    let now = chrono::Utc::now().timestamp();
    let recipient_b = addr_to_bytes32(&recipient);
    let id = proposal_id_bytes(&token_bytes, amount, &recipient_b, now);
    let id_hex = bytes32_to_hex(&id);

    let mut proposal = json!({
        "id": id_hex,
        "escrowVoteId": payload.escrow_vote_id,
        "projectId": project_id,
        "tokenId": bytes32_to_hex(&token_bytes),
        "amount": amount.to_string(),
        "recipient": recipient,
        "reason": reason,
        "proposer": proposer,
        "createdAt": now,
        "status": "PendingAI",
        "aiApprovals": [],
        "governorApprovals": [],
        "foundationApprovals": [],
        "governorSelection": selection,
        "metadataHash": metadata_hash,
        "entropy": bytes32_to_hex(&entropy),
        "timelockOpId": Value::Null,
        "timelockTxHash": Value::Null,
        "executedTxHash": Value::Null,
    });

    let knot = anchor_governance_event(
        &state,
        "CriticalProtocolMintingProposalOpened",
        &proposal,
        json!({
            "escrowVoteId": payload.escrow_vote_id,
            "proposalId": id_hex,
            "pipeline": "MintingGovernance",
        }),
    )
    .await;
    if let Some(ref h) = knot {
        if let Some(obj) = proposal.as_object_mut() {
            obj.insert("openKnotHash".into(), json!(h));
        }
    }

    with_store(|rows| {
        rows.push(proposal.clone());
    })
    .await;

    if !project_id.is_empty() {
        let mut fields = Map::new();
        fields.insert("mintingProposalId".into(), json!(id_hex));
        fields.insert("mintingStatus".into(), json!("PendingAI"));
        let _ = patch_project_fields(&project_id, fields).await;
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "success": true,
            "already_open": false,
            "proposal": public_view(&proposal),
            "vote": vote,
        })),
    )
}

#[derive(Deserialize)]
pub struct StageApprovalRequest {
    signer: String,
    approved: bool,
    timestamp: i64,
    signature: String,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    member_name: Option<String>,
}

/// `POST /api/v1/governance/minting/:id/ai`
pub async fn submit_ai(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<StageApprovalRequest>,
) -> (StatusCode, Json<Value>) {
    stage_approve(&state, &id, "ai", payload).await
}

/// `POST /api/v1/governance/minting/:id/governor`
pub async fn submit_governor(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<StageApprovalRequest>,
) -> (StatusCode, Json<Value>) {
    stage_approve(&state, &id, "governor", payload).await
}

/// `POST /api/v1/governance/minting/:id/foundation`
pub async fn submit_foundation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<StageApprovalRequest>,
) -> (StatusCode, Json<Value>) {
    stage_approve(&state, &id, "foundation", payload).await
}

async fn stage_approve(
    state: &Arc<AppState>,
    id: &str,
    stage: &str,
    payload: StageApprovalRequest,
) -> (StatusCode, Json<Value>) {
    let signer = match verify_stage_sig(
        stage,
        id,
        payload.approved,
        payload.timestamp,
        &payload.signer,
        &payload.signature,
    ) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": e })),
            );
        }
    };

    if let Err(resp) = crate::security_guard::check_signer(&signer) {
        return resp;
    }

    let result = with_store(|rows| {
        let idx = rows.iter().position(|r| {
            r.get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case(id))
                .unwrap_or(false)
        });
        let Some(idx) = idx else {
            return Err(("not_found", "proposal not found".to_string()));
        };
        let proposal = &mut rows[idx];
        let status = proposal
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match stage {
            "ai" => {
                if status != "PendingAI" {
                    return Err(("conflict", format!("expected PendingAI, got {status}")));
                }
                if !ai_agent_set().contains(&signer) {
                    return Err(("forbidden", "signer is not a registered AI agent wallet".into()));
                }
                let arr = proposal
                    .get_mut("aiApprovals")
                    .and_then(|v| v.as_array_mut())
                    .ok_or(("internal", "aiApprovals missing".into()))?;
                if arr.iter().any(|a| {
                    a.get("agentId")
                        .and_then(|v| v.as_str())
                        .map(|s| s.eq_ignore_ascii_case(&signer))
                        .unwrap_or(false)
                }) {
                    return Err(("conflict", "Already voted".into()));
                }
                arr.push(json!({
                    "agentId": signer,
                    "agentType": payload.agent_type.clone().unwrap_or_else(|| "TestimonyAgent".into()),
                    "approved": payload.approved,
                    "confidence": payload.confidence.unwrap_or(1.0),
                    "reasoning": payload.reasoning.clone().unwrap_or_default(),
                    "timestamp": payload.timestamp,
                    "signature": payload.signature,
                }));
                let approved_count = arr
                    .iter()
                    .filter(|a| a.get("approved").and_then(|b| b.as_bool()) == Some(true))
                    .count();
                if !payload.approved {
                    if let Some(obj) = proposal.as_object_mut() {
                        obj.insert(
                            "status".into(),
                            json!({ "Rejected": { "reason": "AI agent rejected" } }),
                        );
                        // Normalize to string status for clients.
                        obj.insert("status".into(), json!("Rejected"));
                        obj.insert("rejectReason".into(), json!("AI agent rejected"));
                    }
                } else if approved_count >= required_ai() {
                    if let Some(obj) = proposal.as_object_mut() {
                        obj.insert("status".into(), json!("PendingGovernors"));
                    }
                }
            }
            "governor" => {
                if status != "PendingGovernors" {
                    return Err(("conflict", format!("expected PendingGovernors, got {status}")));
                }
                let governors = proposal
                    .pointer("/governorSelection/randomGovernors")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let allowed = governors.iter().any(|g| {
                    g.as_str()
                        .map(|s| s.eq_ignore_ascii_case(&signer))
                        .unwrap_or(false)
                });
                if !allowed {
                    return Err(("forbidden", "NotAuthorized - signer not in governor selection".into()));
                }
                let arr = proposal
                    .get_mut("governorApprovals")
                    .and_then(|v| v.as_array_mut())
                    .ok_or(("internal", "governorApprovals missing".into()))?;
                if arr.iter().any(|a| {
                    a.get("governorWallet")
                        .and_then(|v| v.as_str())
                        .map(|s| s.eq_ignore_ascii_case(&signer))
                        .unwrap_or(false)
                }) {
                    return Err(("conflict", "Already voted".into()));
                }
                arr.push(json!({
                    "governorWallet": signer,
                    "approved": payload.approved,
                    "comment": payload.comment,
                    "timestamp": payload.timestamp,
                    "signature": payload.signature,
                }));
                if !payload.approved {
                    if let Some(obj) = proposal.as_object_mut() {
                        obj.insert("status".into(), json!("Rejected"));
                        obj.insert("rejectReason".into(), json!("governor rejected"));
                    }
                } else {
                    let approved_count = arr
                        .iter()
                        .filter(|a| a.get("approved").and_then(|b| b.as_bool()) == Some(true))
                        .count();
                    if approved_count >= required_governors() {
                        if let Some(obj) = proposal.as_object_mut() {
                            obj.insert("status".into(), json!("PendingFoundation"));
                        }
                    }
                }
            }
            "foundation" => {
                if status != "PendingFoundation" {
                    return Err(("conflict", format!("expected PendingFoundation, got {status}")));
                }
                let members = proposal
                    .pointer("/governorSelection/foundationMembers")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let allowed = members.iter().any(|g| {
                    g.as_str()
                        .map(|s| s.eq_ignore_ascii_case(&signer))
                        .unwrap_or(false)
                });
                if !allowed {
                    return Err(("forbidden", "NotAuthorized - signer not in foundation selection".into()));
                }
                let arr = proposal
                    .get_mut("foundationApprovals")
                    .and_then(|v| v.as_array_mut())
                    .ok_or(("internal", "foundationApprovals missing".into()))?;
                if arr.iter().any(|a| {
                    a.get("memberWallet")
                        .and_then(|v| v.as_str())
                        .map(|s| s.eq_ignore_ascii_case(&signer))
                        .unwrap_or(false)
                }) {
                    return Err(("conflict", "Already voted".into()));
                }
                arr.push(json!({
                    "memberWallet": signer,
                    "memberName": payload.member_name.clone().unwrap_or_else(|| "Foundation".into()),
                    "approved": payload.approved,
                    "comment": payload.comment,
                    "timestamp": payload.timestamp,
                    "signature": payload.signature,
                }));
                if !payload.approved {
                    if let Some(obj) = proposal.as_object_mut() {
                        obj.insert("status".into(), json!("Rejected"));
                        obj.insert("rejectReason".into(), json!("foundation rejected"));
                    }
                } else {
                    let approved_count = arr
                        .iter()
                        .filter(|a| a.get("approved").and_then(|b| b.as_bool()) == Some(true))
                        .count();
                    if approved_count >= required_foundation() {
                        if let Some(obj) = proposal.as_object_mut() {
                            obj.insert("status".into(), json!("Approved"));
                            obj.insert("timelockReady".into(), json!(true));
                        }
                    }
                }
            }
            other => return Err(("bad_request", format!("unknown stage {other}"))),
        }

        Ok(proposal.clone())
    })
    .await;

    match result {
        Ok(proposal) => {
            let status = proposal
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let _ = anchor_governance_event(
                state,
                "CriticalProtocolMintingStageVote",
                &json!({
                    "proposalId": id,
                    "stage": stage,
                    "signer": signer,
                    "approved": payload.approved,
                    "status": status,
                }),
                json!({ "stage": stage }),
            )
            .await;
            if let Some(pid) = proposal.get("projectId").and_then(|v| v.as_str()) {
                if !pid.is_empty() {
                    let mut fields = Map::new();
                    fields.insert("mintingStatus".into(), json!(status));
                    let _ = patch_project_fields(pid, fields).await;
                }
            }
            (
                StatusCode::OK,
                Json(json!({ "success": true, "proposal": public_view(&proposal) })),
            )
        }
        Err(("not_found", msg)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": msg })),
        ),
        Err(("forbidden", msg)) => (
            StatusCode::FORBIDDEN,
            Json(json!({ "success": false, "error": msg })),
        ),
        Err(("conflict", msg)) => (
            StatusCode::CONFLICT,
            Json(json!({ "success": false, "error": msg })),
        ),
        Err((_, msg)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": msg })),
        ),
    }
}

#[derive(Deserialize)]
pub struct MarkTimelockRequest {
    /// `schedule` | `execute`
    action: String,
    tx_hash: String,
    #[serde(default)]
    timelock_op_id: Option<String>,
}

/// `POST /api/v1/governance/minting/:id/timelock`
/// Admin-gated: records Timelock schedule/execute after MintingGovernance Approved.
/// Does not invent approvals - only advances Approved → Scheduled → Executed.
pub async fn mark_timelock(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<MarkTimelockRequest>,
) -> (StatusCode, Json<Value>) {
    if let Err(e) = admin_token_ok(&state, &headers).await {
        return e;
    }
    let tx = payload.tx_hash.trim().to_lowercase();
    if !tx.starts_with("0x") || tx.len() != 66 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "tx_hash must be 32-byte 0x hex" })),
        );
    }

    let action = payload.action.trim().to_ascii_lowercase();
    let result = with_store(|rows| {
        let idx = rows.iter().position(|r| {
            r.get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case(&id))
                .unwrap_or(false)
        });
        let Some(idx) = idx else {
            return Err(("not_found", "proposal not found".to_string()));
        };
        let proposal = &mut rows[idx];
        let status = proposal
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        match action.as_str() {
            "schedule" => {
                if status != "Approved" {
                    return Err(("conflict", format!("expected Approved, got {status}")));
                }
                if let Some(obj) = proposal.as_object_mut() {
                    obj.insert("status".into(), json!("Scheduled"));
                    obj.insert("timelockTxHash".into(), json!(tx));
                    if let Some(op) = &payload.timelock_op_id {
                        obj.insert("timelockOpId".into(), json!(op));
                    }
                }
            }
            "execute" => {
                if status != "Approved" && status != "Scheduled" {
                    return Err(("conflict", format!("expected Approved|Scheduled, got {status}")));
                }
                if let Some(obj) = proposal.as_object_mut() {
                    obj.insert("status".into(), json!("Executed"));
                    obj.insert("executedTxHash".into(), json!(tx));
                    if let Some(op) = &payload.timelock_op_id {
                        obj.insert("timelockOpId".into(), json!(op));
                    }
                }
            }
            other => return Err(("bad_request", format!("action must be schedule|execute (got {other})"))),
        }
        Ok(proposal.clone())
    })
    .await;

    match result {
        Ok(proposal) => {
            let _ = anchor_governance_event(
                &state,
                "CriticalProtocolMintingTimelock",
                &json!({
                    "proposalId": id,
                    "action": action,
                    "txHash": tx,
                }),
                json!({ "action": action }),
            )
            .await;
            if let Some(pid) = proposal.get("projectId").and_then(|v| v.as_str()) {
                if !pid.is_empty() {
                    let mut fields = Map::new();
                    fields.insert(
                        "mintingStatus".into(),
                        json!(proposal.get("status").and_then(|v| v.as_str()).unwrap_or("")),
                    );
                    let _ = patch_project_fields(pid, fields).await;
                }
            }
            (
                StatusCode::OK,
                Json(json!({ "success": true, "proposal": public_view(&proposal) })),
            )
        }
        Err(("not_found", msg)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": msg })),
        ),
        Err(("conflict", msg)) => (
            StatusCode::CONFLICT,
            Json(json!({ "success": false, "error": msg })),
        ),
        Err((_, msg)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": msg })),
        ),
    }
}

/// `GET /api/v1/governance/minting/:id/prepare-timelock`
/// Returns a schedule payload for DCSwapTimelock once status is Approved.
pub async fn prepare_timelock(Path(id): Path<String>) -> (StatusCode, Json<Value>) {
    let rows = load_proposals_blocking();
    let Some(p) = find_by_id(&rows, &id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "proposal not found" })),
        );
    };
    let status = p.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status != "Approved" && status != "Scheduled" {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": format!("proposal not ready for Timelock (status={status})"),
            })),
        );
    }
    let timelock = std::env::var("DCSWAP_TIMELOCK_ADDRESS")
        .unwrap_or_else(|_| "0x50Cfc56D81603A61660B8c6306e7Cb6E6693532c".into());
    let target = std::env::var("MINTING_TIMELOCK_TARGET").unwrap_or_else(|_| timelock.clone());
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "timelock": timelock,
            "chainId": 271828,
            "proposal": public_view(p),
            "schedule": {
                "target": target,
                "value": "0",
                "data": "0x",
                "predecessor": "0x0000000000000000000000000000000000000000000000000000000000000000",
                "salt": p.get("id"),
                "delayHintSecs": 3600,
                "note": "Operator builds calldata for the approved mint/param change, then calls Timelock.schedule. Record the tx via POST …/timelock with action=schedule."
            }
        })),
    )
}

/// `GET /api/v1/governance/influence` - voting-weight leaderboard from ballots.
pub async fn influence_leaderboard(State(state): State<Arc<AppState>>) -> Json<Value> {
    let ballots = crate::governance_votes::load_ballots(&state).await;
    let mut by_addr: std::collections::HashMap<String, (f64, u64, u64)> =
        std::collections::HashMap::new();
    for b in &ballots {
        let addr = b
            .get("voter_address")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        if addr.is_empty() {
            continue;
        }
        // Ballots persist `weight_fat` (see governance_votes::vote_project).
        // Accept camelCase and legacy `weight` aliases so the leaderboard
        // never silently zeros out real participation.
        let weight = b
            .get("weight_fat")
            .or_else(|| b.get("weightFat"))
            .or_else(|| b.get("weight"))
            .and_then(|v| v.as_f64())
            .or_else(|| {
                b.get("weight_fat")
                    .or_else(|| b.get("weightFat"))
                    .or_else(|| b.get("weight"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok())
            })
            .unwrap_or(0.0);
        let for_vote = b.get("vote_for").and_then(|v| v.as_bool()).unwrap_or(false);
        let entry = by_addr.entry(addr).or_insert((0.0, 0, 0));
        entry.0 += weight;
        entry.1 += 1;
        if for_vote {
            entry.2 += 1;
        }
    }
    let mut rows: Vec<Value> = by_addr
        .into_iter()
        .map(|(address, (weight, ballots_n, for_n))| {
            json!({
                "address": address,
                "totalWeight": weight,
                "ballots": ballots_n,
                "votesFor": for_n,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        let wa = a.get("totalWeight").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let wb = b.get("totalWeight").and_then(|v| v.as_f64()).unwrap_or(0.0);
        wb.partial_cmp(&wa).unwrap_or(std::cmp::Ordering::Equal)
    });
    let limit = 50usize;
    rows.truncate(limit);
    Json(json!({
        "success": true,
        "count": rows.len(),
        "leaderboard": rows,
        "source": "project-ballots.jsonl (discover cache; on-chain VoteEscrow is authoritative for escrow votes)"
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_is_deterministic() {
        let a = entropy_from_vote(7, "0xabc", "proj-1");
        let b = entropy_from_vote(7, "0xabc", "proj-1");
        assert_eq!(a, b);
        let c = entropy_from_vote(8, "0xabc", "proj-1");
        assert_ne!(a, c);
    }

    #[test]
    fn select_governors_needs_pool() {
        // With defaults (≥5 validators + ≥2 foundation after filter) this should succeed.
        let e = [9u8; 32];
        // foundation_pool may filter invalid placeholder - ensure we don't panic.
        let _ = select_governors(&e);
    }

    #[test]
    fn normalize_refuses_compromised() {
        assert!(normalize_addr(COMPROMISED_DEPLOYER).is_err());
    }
}
