//! VoteEscrow HTTP helpers - on-chain reads + project↔escrow linking +
//! transaction-prep payloads for the governance UI.
//!
//! Contract: `contracts/src/governance/VoteEscrow.sol` on chain 271828.
//! Cross-chain weight attestations live in `cross_chain_weight.rs`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha3::{Digest, Keccak256};
use std::sync::Arc;

use crate::cross_chain_weight::{rope_chain_id, vote_escrow_address};
use crate::governance_votes::{anchor_governance_event, get_project_by_id, patch_project_fields};
use crate::AppState;

// ============================================================================
// ABI selectors - keccak256(signature)[0:4], verified via `cast sig`.
// ============================================================================

const SEL_VOTES_LENGTH: &str = "0xde4f6347";
const SEL_GET_VOTE: &str = "0x5a55c1f0";

// ============================================================================
// eth_call helper (Rope RPC fleet failover - same pattern as governance_votes)
// ============================================================================

async fn eth_call_contract(
    state: &Arc<AppState>,
    contract: &str,
    data: &str,
) -> Result<Vec<u8>, String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_call",
        "params": [{ "to": contract, "data": data }, "latest"],
    });
    let mut last_err = "no RPC endpoint configured".to_string();
    for url in &state.rpc_urls {
        let resp = state
            .http_client
            .post(url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(12))
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
            let hex_val = result.trim_start_matches("0x");
            if hex_val.is_empty() {
                return Ok(Vec::new());
            }
            return hex::decode(hex_val).map_err(|e| format!("{url}: bad hex: {e}"));
        }
        if let Some(err) = parsed.get("error") {
            last_err = format!("{url}: rpc error {err}");
            continue;
        }
    }
    Err(last_err)
}

fn u256_calldata(value: u64) -> String {
    format!("{:064x}", value)
}

// ============================================================================
// ABI decode - `getVote(uint256)` returns `VoteConfig` (18 × 32-byte words).
// Layout matches VoteEscrow.sol struct field order (Solidity memory tuple).
// ============================================================================

fn decode_u256_slot(data: &[u8], index: usize) -> String {
    let start = index * 32;
    if data.len() < start + 32 {
        return "0".to_string();
    }
    let slot = &data[start..start + 32];
    if slot.iter().all(|&b| b == 0) {
        return "0".to_string();
    }
    if slot[..16].iter().all(|&b| b == 0) {
        let mut arr = [0u8; 16];
        arr.copy_from_slice(&slot[16..]);
        return u128::from_be_bytes(arr).to_string();
    }
    format!("0x{}", hex::encode(slot))
}

fn decode_address_slot(data: &[u8], index: usize) -> String {
    let start = index * 32;
    if data.len() < start + 32 {
        return "0x0000000000000000000000000000000000000000".to_string();
    }
    format!("0x{}", hex::encode(&data[start + 12..start + 32]))
}

fn decode_bool_slot(data: &[u8], index: usize) -> bool {
    let start = index * 32;
    data.get(start + 31).copied().unwrap_or(0) != 0
}

fn decode_u8_slot(data: &[u8], index: usize) -> u8 {
    let start = index * 32;
    data.get(start + 31).copied().unwrap_or(0)
}

fn decode_bytes32_slot(data: &[u8], index: usize) -> String {
    let start = index * 32;
    if data.len() < start + 32 {
        return "0x0000000000000000000000000000000000000000000000000000000000000000".to_string();
    }
    format!("0x{}", hex::encode(&data[start..start + 32]))
}

fn vote_class_name(v: u8) -> &'static str {
    match v {
        0 => "Project",
        1 => "Cause",
        2 => "CriticalProtocol",
        3 => "NonCriticalFeature",
        _ => "Unknown",
    }
}

fn disposition_name(v: u8) -> &'static str {
    match v {
        0 => "Burn",
        1 => "Return",
        2 => "Reward",
        _ => "Unknown",
    }
}

fn outcome_name(v: u8) -> &'static str {
    match v {
        0 => "Pending",
        1 => "Approved",
        2 => "Rejected",
        3 => "NoQuorum",
        _ => "Unknown",
    }
}

fn parse_vote_class_param(s: &str) -> Result<u8, String> {
    match s.to_lowercase().as_str() {
        "project" => Ok(0),
        "cause" => Ok(1),
        "critical_protocol" | "criticalprotocol" => Ok(2),
        "non_critical_feature" | "noncriticalfeature" => Ok(3),
        other => Err(format!("unknown vote_class: {other}")),
    }
}

fn parse_disposition_param(s: &str) -> Result<u8, String> {
    match s.to_lowercase().as_str() {
        "burn" => Ok(0),
        "return" => Ok(1),
        "reward" => Ok(2),
        other => Err(format!("unknown disposition: {other}")),
    }
}

fn project_metadata_hash(project_id: &str, name: &str) -> String {
    let mut h = Keccak256::new();
    h.update(project_id.as_bytes());
    h.update(name.as_bytes());
    format!("0x{}", hex::encode(h.finalize()))
}

fn min_weight_to_vote_wei() -> String {
    std::env::var("VOTE_ESCROW_MIN_WEIGHT_TO_VOTE")
        .unwrap_or_else(|_| "1000000000000000000".into())
}

fn quorum_weight_wei() -> String {
    std::env::var("VOTE_ESCROW_QUORUM_WEIGHT")
        .unwrap_or_else(|_| "1000000000000000000000000".into())
}

fn approval_threshold_bps() -> u16 {
    std::env::var("VOTE_ESCROW_APPROVAL_THRESHOLD_BPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5100)
}

fn voting_period_secs() -> i64 {
    std::env::var("VOTING_PERIOD_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(7)
        * 86_400
}

fn eligible_set_name(id: u8) -> &'static str {
    match id {
        0 => "AllHolders",
        1 => "JuryAndPay",
        _ => "Unknown",
    }
}

fn decode_get_vote(raw: &[u8]) -> Result<Value, String> {
    // Phase 5 VoteConfig is 23 words (was 18 before eligibleVoterSet/pay fees).
    let min_words = if raw.len() >= 23 * 32 { 23 } else { 18 };
    if raw.len() < min_words * 32 {
        return Err(format!(
            "getVote returned {} bytes, expected at least {}",
            raw.len(),
            18 * 32
        ));
    }

    let vote_class = decode_u8_slot(raw, 0);
    let disposition = decode_u8_slot(raw, 1);
    let bps_slot = &raw[6 * 32 + 30..6 * 32 + 32];
    let approval_threshold_bps = u16::from_be_bytes(bps_slot.try_into().unwrap());

    let mut out = json!({
        "voteClass": vote_class_name(vote_class),
        "voteClassId": vote_class,
        "disposition": disposition_name(disposition),
        "dispositionId": disposition,
        "startsAt": decode_u256_slot(raw, 2),
        "endsAt": decode_u256_slot(raw, 3),
        "minWeightToVote": decode_u256_slot(raw, 4),
        "quorumWeight": decode_u256_slot(raw, 5),
        "approvalThresholdBps": approval_threshold_bps,
        "rewardPoolAmount": decode_u256_slot(raw, 7),
        "rewardPoolFunder": decode_address_slot(raw, 8),
        "metadataHash": decode_bytes32_slot(raw, 9),
        "creator": decode_address_slot(raw, 10),
        "finalized": decode_bool_slot(raw, 11),
        "burnSwept": decode_bool_slot(raw, 12),
        "outcome": outcome_name(decode_u8_slot(raw, 13)),
        "outcomeId": decode_u8_slot(raw, 13),
        "totalWeightFor": decode_u256_slot(raw, 14),
        "totalWeightAgainst": decode_u256_slot(raw, 15),
        "totalLockedFor": decode_u256_slot(raw, 16),
        "totalLockedAgainst": decode_u256_slot(raw, 17),
    });
    if raw.len() >= 23 * 32 {
        let elig = decode_u8_slot(raw, 18);
        if let Some(obj) = out.as_object_mut() {
            obj.insert("eligibleVoterSet".into(), json!(eligible_set_name(elig)));
            obj.insert("eligibleVoterSetId".into(), json!(elig));
            obj.insert("payToVoteFee".into(), json!(decode_u256_slot(raw, 19)));
            obj.insert("totalPayFees".into(), json!(decode_u256_slot(raw, 20)));
            obj.insert("createdAt".into(), json!(decode_u256_slot(raw, 21)));
            obj.insert("jurySet".into(), json!(decode_bool_slot(raw, 22)));
        }
    }
    Ok(out)
}

fn contract_address_or_err() -> Result<String, (StatusCode, Json<Value>)> {
    vote_escrow_address().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "success": false,
                "error": "VOTE_ESCROW_ADDRESS not configured",
            })),
        )
    })
}

const CREATE_VOTE_ABI: &str = r#"[
  {
    "type": "function",
    "name": "createVote",
    "inputs": [
      {
        "name": "params",
        "type": "tuple",
        "components": [
          { "name": "voteClass", "type": "uint8" },
          { "name": "disposition", "type": "uint8" },
          { "name": "startsAt", "type": "uint64" },
          { "name": "endsAt", "type": "uint64" },
          { "name": "minWeightToVote", "type": "uint256" },
          { "name": "quorumWeight", "type": "uint256" },
          { "name": "approvalThresholdBps", "type": "uint16" },
          { "name": "rewardPoolFunder", "type": "address" },
          { "name": "metadataHash", "type": "bytes32" },
          { "name": "eligibleVoterSet", "type": "uint8" },
          { "name": "payToVoteFee", "type": "uint256" }
        ]
      },
      { "name": "creatorWeight", "type": "uint256" },
      { "name": "creatorExpiresAt", "type": "uint256" },
      { "name": "creatorAttestation", "type": "bytes" }
    ],
    "outputs": [{ "name": "voteId", "type": "uint256" }],
    "stateMutability": "payable"
  },
  {
    "type": "function",
    "name": "payToVote",
    "inputs": [{ "name": "voteId", "type": "uint256" }],
    "outputs": [],
    "stateMutability": "payable"
  },
  {
    "type": "function",
    "name": "setJury",
    "inputs": [
      { "name": "voteId", "type": "uint256" },
      { "name": "jurors", "type": "address[]" }
    ],
    "outputs": [],
    "stateMutability": "nonpayable"
  }
]"#;

fn pay_to_vote_fee_wei_default() -> String {
    std::env::var("VOTE_PAY_TO_VOTE_FEE_WEI")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "100000000000000000000".to_string()) // 100 FAT
}

// ============================================================================
// HTTP handlers
// ============================================================================

/// `GET /api/v1/governance/escrow`
pub async fn escrow_info(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<Value>) {
    let address = match contract_address_or_err() {
        Ok(a) => a,
        Err(resp) => return resp,
    };

    let raw = match eth_call_contract(&state, &address, SEL_VOTES_LENGTH).await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "success": false, "error": e })),
            );
        }
    };

    let votes_length = if raw.len() >= 32 {
        decode_u256_slot(&raw, 0)
    } else if raw.is_empty() {
        "0".to_string()
    } else {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "success": false,
                "error": format!("votesLength() returned unexpected length {}", raw.len()),
            })),
        );
    };

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "address": address,
            "chain_id": rope_chain_id(),
            "votes_length": votes_length,
        })),
    )
}

/// `GET /api/v1/governance/escrow/:vote_id`
pub async fn get_escrow_vote(
    State(state): State<Arc<AppState>>,
    Path(vote_id): Path<u64>,
) -> (StatusCode, Json<Value>) {
    let address = match contract_address_or_err() {
        Ok(a) => a,
        Err(resp) => return resp,
    };

    let calldata = format!("{}{}", SEL_GET_VOTE, u256_calldata(vote_id));
    let raw = match eth_call_contract(&state, &address, &calldata).await {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "success": false, "error": e })),
            );
        }
    };

    if raw.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({
                "success": false,
                "error": format!("vote {vote_id} not found (empty eth_call result)"),
            })),
        );
    }

    match decode_get_vote(&raw) {
        Ok(vote) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "vote_id": vote_id,
                "address": address,
                "chain_id": rope_chain_id(),
                "vote": vote,
            })),
        ),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "success": false, "error": e })),
        ),
    }
}

#[derive(Deserialize)]
pub struct EscrowLinkRequest {
    escrow_vote_id: u64,
    tx_hash: String,
    voter_address: Option<String>,
    signature: Option<String>,
    timestamp: Option<i64>,
}

/// On-chain `getVote` decode for other modules (CriticalProtocol → MintingGovernance).
pub async fn fetch_decoded_vote(
    state: &Arc<AppState>,
    vote_id: u64,
) -> Result<Value, String> {
    let address = vote_escrow_address().ok_or_else(|| "VOTE_ESCROW_ADDRESS not configured".to_string())?;
    let data = format!("{}{}", SEL_GET_VOTE, u256_calldata(vote_id));
    let raw = eth_call_contract(state, &address, &data).await?;
    if raw.is_empty() {
        return Err(format!("vote {vote_id} not found (empty eth_call result)"));
    }
    decode_get_vote(&raw)
}

/// `POST /api/v1/projects/:id/escrow/link`
pub async fn link_escrow_vote(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<EscrowLinkRequest>,
) -> (StatusCode, Json<Value>) {
    let tx_hash = payload.tx_hash.trim();
    if !tx_hash.starts_with("0x") || tx_hash.len() != 66 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "tx_hash must be a 32-byte 0x-prefixed hex string",
            })),
        );
    }
    if hex::decode(&tx_hash[2..]).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "tx_hash is not valid hex",
            })),
        );
    }

    let project = match get_project_by_id(&state, &id).await {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "success": false, "error": "project not found" })),
            );
        }
    };

    if let Some(existing) = project.get("escrowVoteId").and_then(|v| v.as_u64()) {
        if existing != payload.escrow_vote_id {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "success": false,
                    "error": format!(
                        "project already linked to escrow vote {existing}; unlink before re-linking"
                    ),
                })),
            );
        }
    }

    let all_projects = crate::governance_votes::load_projects_local().await;
    for other in &all_projects {
        if other.get("id").and_then(|v| v.as_str()) == Some(id.as_str()) {
            continue;
        }
        if other.get("escrowVoteId").and_then(|v| v.as_u64()) == Some(payload.escrow_vote_id) {
            let other_id = other.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "success": false,
                    "error": format!(
                        "escrow vote {} is already linked to project {other_id}",
                        payload.escrow_vote_id
                    ),
                })),
            );
        }
    }

    if let (Some(addr), Some(sig)) = (&payload.voter_address, &payload.signature) {
        let ts = payload.timestamp.unwrap_or_else(|| chrono::Utc::now().timestamp());
        if let Err(e) = crate::governance_votes::verify_escrow_link_signature(
            addr,
            &id,
            payload.escrow_vote_id,
            ts,
            sig,
        ) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": e })),
            );
        }
    }

    let mut fields = Map::new();
    fields.insert("escrowVoteId".into(), json!(payload.escrow_vote_id));
    fields.insert("escrowCreateTx".into(), json!(tx_hash));
    if let Some(ref addr) = payload.voter_address {
        fields.insert("escrowLinkedBy".into(), json!(addr.to_lowercase()));
    }

    let updated = match patch_project_fields(&id, fields).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e })),
            );
        }
    };

    let anchored = anchor_governance_event(
        &state,
        "EscrowVoteLinked",
        &updated,
        json!({
            "project_id": id,
            "escrow_vote_id": payload.escrow_vote_id,
            "tx_hash": tx_hash,
            "voter_address": payload.voter_address,
        }),
    )
    .await;

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "project": updated,
            "anchored": anchored.is_some(),
            "knot": anchored,
        })),
    )
}

#[derive(Deserialize)]
pub struct PrepareCreateQuery {
    project_id: String,
    vote_class: String,
    disposition: String,
}

/// `GET /api/v1/governance/escrow/prepare/create?project_id=&vote_class=&disposition=`
pub async fn prepare_create(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PrepareCreateQuery>,
) -> (StatusCode, Json<Value>) {
    let address = match contract_address_or_err() {
        Ok(a) => a,
        Err(resp) => return resp,
    };

    let vote_class_id = match parse_vote_class_param(&query.vote_class) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": e })),
            );
        }
    };
    let disposition_id = match parse_disposition_param(&query.disposition) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": e })),
            );
        }
    };

    let project = match get_project_by_id(&state, &query.project_id).await {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "success": false, "error": "project not found" })),
            );
        }
    };

    let name = project
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let metadata_hash = project_metadata_hash(&query.project_id, name);

    let now = chrono::Utc::now().timestamp() as u64;
    let ends_at = project
        .get("votingEndsAt")
        .and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|i| i as u64)))
        .unwrap_or(now + voting_period_secs() as u64);

    let min_weight = min_weight_to_vote_wei();
    let quorum = quorum_weight_wei();
    let bps = approval_threshold_bps();

    let elig_raw = project
        .get("eligibleVoterSet")
        .and_then(|v| v.as_str())
        .unwrap_or(if vote_class_id == 1 { "jury_and_pay" } else { "all_holders" });
    let (eligible_id, eligible_name, pay_fee) = match elig_raw.to_ascii_lowercase().as_str() {
        "jury_and_pay" | "juryandpay" | "1" => (
            1u8,
            "JuryAndPay",
            project
                .get("payToVoteFeeWei")
                .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| v.as_u64().map(|n| n.to_string())))
                .unwrap_or_else(pay_to_vote_fee_wei_default),
        ),
        _ => (0u8, "AllHolders", "0".to_string()),
    };

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "contract": address,
            "chain_id": rope_chain_id(),
            "project_id": query.project_id,
            "metadata_hash": metadata_hash,
            "create_vote_params": {
                "voteClass": vote_class_id,
                "voteClassName": vote_class_name(vote_class_id),
                "disposition": disposition_id,
                "dispositionName": disposition_name(disposition_id),
                "startsAt": now.to_string(),
                "endsAt": ends_at.to_string(),
                "minWeightToVote": min_weight,
                "quorumWeight": quorum,
                "approvalThresholdBps": bps,
                "rewardPoolFunder": "0x0000000000000000000000000000000000000000",
                "metadataHash": metadata_hash,
                "eligibleVoterSet": eligible_id,
                "eligibleVoterSetName": eligible_name,
                "payToVoteFee": pay_fee,
            },
            "jury": project.get("jury").cloned().unwrap_or(json!([])),
            "abi_fragment": serde_json::from_str::<Value>(CREATE_VOTE_ABI).unwrap_or(json!([])),
            "weight_attestation": {
                "purpose": "create",
                "url": format!("/api/v1/governance/weight/{{address}}?purpose=create"),
            },
            "notes": [
                "Fetch cross-chain weight + creator attestation via GET /api/v1/governance/weight/:address?purpose=create before submitting createVote.",
                "When disposition is Reward, msg.value must fund the reward pool; otherwise msg.value must be 0.",
                "JuryAndPay: after createVote, creator/owner must call setJury with the off-chain jury list; non-jurors call payToVote(fee) before castVote.",
            ],
        })),
    )
}

#[derive(Deserialize)]
pub struct PrepareCastQuery {
    project_id: String,
}

/// `GET /api/v1/governance/escrow/prepare/cast?project_id=`
pub async fn prepare_cast(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PrepareCastQuery>,
) -> (StatusCode, Json<Value>) {
    let address = match contract_address_or_err() {
        Ok(a) => a,
        Err(resp) => return resp,
    };

    let project = match get_project_by_id(&state, &query.project_id).await {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "success": false, "error": "project not found" })),
            );
        }
    };

    let Some(vote_id) = project.get("escrowVoteId").and_then(|v| v.as_u64()) else {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": "project has no escrowVoteId - link an on-chain vote first via POST /api/v1/projects/:id/escrow/link",
            })),
        );
    };

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "contract": address,
            "vote_id": vote_id,
            "chain_id": rope_chain_id(),
            "project_id": query.project_id,
            "weight_attestation": {
                "purpose": "cast",
                "vote_id": vote_id,
                "url": format!("/api/v1/governance/weight/{{address}}?purpose=cast&vote_id={vote_id}"),
            },
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_hash_is_deterministic() {
        let h1 = project_metadata_hash("proj-abc", "My Project");
        let h2 = project_metadata_hash("proj-abc", "My Project");
        assert_eq!(h1, h2);
        assert!(h1.starts_with("0x") && h1.len() == 66);
    }

    #[test]
    fn decode_get_vote_parses_18_words() {
        let mut raw = vec![0u8; 18 * 32];
        raw[31] = 0; // Project
        raw[32 + 31] = 1; // Return
        raw[2 * 32 + 31] = 100; // startsAt low byte
        raw[3 * 32 + 31] = 200; // endsAt
        raw[6 * 32 + 30] = 0x13; // 5100 bps (0x13EC) - uint16 right-aligned in word 6
        raw[6 * 32 + 31] = 0xEC;
        raw[11 * 32 + 31] = 1; // finalized

        let v = decode_get_vote(&raw).unwrap();
        assert_eq!(v["voteClass"], "Project");
        assert_eq!(v["disposition"], "Return");
        assert_eq!(v["startsAt"], "100");
        assert_eq!(v["endsAt"], "200");
        assert_eq!(v["approvalThresholdBps"], 5100);
        assert_eq!(v["finalized"], true);
    }
}
