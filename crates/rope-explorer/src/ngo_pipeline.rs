//! NGO Phase 5 - contractualization pipeline for governance cause votes.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha3::{Digest, Keccak256};
use std::sync::Arc;

use crate::cross_chain_weight::{rope_chain_id, vote_escrow_address};
use crate::governance_votes::{
    anchor_governance_event, load_ballots, load_projects, load_projects_local, patch_project_fields,
};
use crate::jury::{is_juror, normalize_address, normalize_pool, select_jury, DEFAULT_JURY_FRACTION_BPS};
use crate::AppState;

const COMPROMISED_DEPLOYER: &str = "0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195";
const TREASURY_AUTH_DOMAIN: &str = "DCROPE-NGO-TREASURY";

static PAID_RIGHTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static NGO_GRANTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn ngo_grants_path() -> String {
    std::env::var("NGO_GRANTS_PATH")
        .unwrap_or_else(|_| "/opt/datachain-rope/ngo-grants.jsonl".into())
}

pub fn vote_paid_rights_path() -> String {
    std::env::var("VOTE_PAID_RIGHTS_PATH")
        .unwrap_or_else(|_| "/opt/datachain-rope/vote-paid-rights.jsonl".into())
}

fn governance_pool_path() -> String {
    std::env::var("GOVERNANCE_POOL_PATH")
        .unwrap_or_else(|_| "/opt/datachain-rope/governance-pool.jsonl".into())
}

pub fn pay_to_vote_fee_wei() -> String {
    std::env::var("VOTE_PAY_TO_VOTE_FEE_WEI").unwrap_or_else(|_| "100000000000000000000".into())
}

fn legacy_dc_eth_address() -> String {
    std::env::var("LEGACY_DC_ETH_ADDRESS")
        .unwrap_or_else(|_| "0x0b44547be0a0df5dcd5327de8ea73680517c5a54".to_string())
}

fn eth_rpc_urls() -> Vec<String> {
    std::env::var("ETH_RPC_URL")
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_else(|_| {
            vec![
                "https://ethereum-rpc.publicnode.com".to_string(),
                "https://eth.drpc.org".to_string(),
            ]
        })
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

async fn load_paid_rights_local() -> Vec<Value> {
    let path = vote_paid_rights_path();
    tokio::task::spawn_blocking(move || load_jsonl_blocking(&path))
        .await
        .unwrap_or_default()
}

async fn append_paid_right_local(record: &Value) -> std::io::Result<()> {
    let path = vote_paid_rights_path();
    let line = format!("{record}\n");
    tokio::task::spawn_blocking(move || {
        let _guard = PAID_RIGHTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

async fn append_ngo_grant_local(record: &Value) -> std::io::Result<()> {
    let path = ngo_grants_path();
    let line = format!("{record}\n");
    tokio::task::spawn_blocking(move || {
        let _guard = NGO_GRANTS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

pub fn has_paid_vote_right(records: &[Value], project_id: &str, vote_id: u64, voter: &str) -> bool {
    let Some(voter_norm) = normalize_address(voter) else {
        return false;
    };
    records.iter().any(|r| {
        r.get("project_id").and_then(|v| v.as_str()) == Some(project_id)
            && r.get("vote_id").and_then(|v| v.as_u64()) == Some(vote_id)
            && r.get("voter_address")
                .and_then(|v| v.as_str())
                .and_then(|a| normalize_address(a))
                .as_ref()
                == Some(&voter_norm)
    })
}

pub async fn has_paid_vote_right_live(project_id: &str, vote_id: u64, voter: &str) -> bool {
    let records = load_paid_rights_local().await;
    has_paid_vote_right(&records, project_id, vote_id, voter)
}

/// Load all paid vote-right records (for attestation eligibility checks).
pub async fn load_paid_rights_for_check() -> Vec<Value> {
    load_paid_rights_local().await
}

fn eligible_voter_set_is_jury_and_pay(project: &Value) -> bool {
    project
        .get("eligibleVoterSet")
        .or_else(|| project.get("eligible_voter_set"))
        .and_then(|v| v.as_str())
        .map(|s| {
            let lower = s.to_ascii_lowercase();
            lower == "jury_and_pay" || lower == "juryandpay"
        })
        .unwrap_or(false)
}

fn project_jury_list(project: &Value) -> Vec<String> {
    project
        .get("jury")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter_map(|s| normalize_address(s))
                .collect()
        })
        .unwrap_or_default()
}

/// NGO-pipeline admin gate. Since 2026-08-14 this delegates to
/// [`crate::admin_tokens::require_role`] with the `ProjectAdmin` role
/// (a `MultiRole` token also satisfies the check per
/// [`crate::admin_tokens::Role::grants`]). Env-var escape hatches
/// (`PROJECTS_ADMIN_TOKEN`) are no longer consulted; only dynamic
/// admin tokens minted through `/api/v1/admin-tokens/*` work.
pub async fn check_admin_token(
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

async fn load_governance_pool_addresses() -> Vec<String> {
    let path = governance_pool_path();
    let members: Vec<Value> = tokio::task::spawn_blocking(move || load_jsonl_blocking(&path))
        .await
        .unwrap_or_default();
    let mut addrs: Vec<String> = members
        .iter()
        .filter_map(|m| {
            m.get("ropeAddress")
                .or_else(|| m.get("rope_address"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .and_then(|s| normalize_address(s))
                .or_else(|| {
                    m.get("ethAddress")
                        .or_else(|| m.get("eth_address"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .and_then(|s| normalize_address(s))
                })
                .or_else(|| {
                    m.get("xdcAddress")
                        .or_else(|| m.get("xdc_address"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .and_then(|s| normalize_address(s))
                })
        })
        .collect();
    addrs.sort();
    addrs.dedup();
    addrs
}

pub async fn fetch_block_entropy(state: &Arc<AppState>) -> Result<[u8; 32], String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getBlockByNumber",
        "params": ["latest", false],
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
        if let Some(hash) = parsed
            .get("result")
            .and_then(|r| r.get("hash"))
            .and_then(|h| h.as_str())
        {
            let hex_val = hash.trim_start_matches("0x");
            if let Ok(bytes) = hex::decode(hex_val) {
                if bytes.len() == 32 {
                    let mut out = [0u8; 32];
                    out.copy_from_slice(&bytes);
                    return Ok(out);
                }
            }
            last_err = format!("{url}: unexpected block hash {hash}");
            continue;
        }
        if let Some(err) = parsed.get("error") {
            last_err = format!("{url}: rpc error {err}");
        }
    }
    Err(last_err)
}

pub async fn draw_jury_for_project(
    state: &Arc<AppState>,
    project_id: &str,
    fraction_bps: u16,
) -> Result<Value, String> {
    let projects = load_projects_local().await;
    let Some(project) = projects
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(project_id))
    else {
        return Err("project not found".into());
    };

    let vote_class = project
        .get("voteClass")
        .and_then(|v| v.as_str())
        .unwrap_or("project");
    if vote_class != "cause" {
        return Err("jury draw applies only to vote_class=cause projects".into());
    }

    let pool = normalize_pool(&load_governance_pool_addresses().await);
    if pool.is_empty() {
        return Err(
            "governance pool is empty - enroll members via POST /api/v1/governance/pool/join"
                .into(),
        );
    }

    let entropy = fetch_block_entropy(state).await?;
    let (jurors, jury_proof) = select_jury(&pool, &entropy, fraction_bps);
    let jury_seed = format!("0x{}", hex::encode(entropy));
    let jury_proof_hex = format!("0x{}", hex::encode(jury_proof));
    let fee = pay_to_vote_fee_wei();

    let mut fields = Map::new();
    fields.insert("jury".into(), json!(jurors));
    fields.insert("jurySeed".into(), json!(jury_seed));
    fields.insert("juryProof".into(), json!(jury_proof_hex));
    fields.insert("eligibleVoterSet".into(), json!("jury_and_pay"));
    fields.insert("payToVoteFeeWei".into(), json!(fee));
    if project.get("disposition").is_none() {
        fields.insert("disposition".into(), json!("return"));
    }

    let updated = patch_project_fields(project_id, fields).await?;

    let anchored = anchor_governance_event(
        state,
        "JuryDrawn",
        &updated,
        json!({
            "project_id": project_id,
            "jury_count": jurors.len(),
            "pool_size": pool.len(),
            "fraction_bps": fraction_bps,
            "jury_seed": jury_seed,
            "jury_proof": jury_proof_hex,
        }),
    )
    .await;

    Ok(json!({
        "project": updated,
        "jury": jurors,
        "jurySeed": jury_seed,
        "juryProof": jury_proof_hex,
        "poolSize": pool.len(),
        "fractionBps": fraction_bps,
        "anchored": anchored.is_some(),
        "knot": anchored,
    }))
}

pub async fn draw_jury_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if let Err(resp) = check_admin_token(&state, &headers).await {
        return resp;
    }
    match draw_jury_for_project(&state, &id, DEFAULT_JURY_FRACTION_BPS).await {
        Ok(body) => (
            StatusCode::OK,
            Json(json!({ "success": true, "project": body.get("project"), "jury": body.get("jury"), "jurySeed": body.get("jurySeed"), "juryProof": body.get("juryProof"), "poolSize": body.get("poolSize"), "fractionBps": body.get("fractionBps"), "anchored": body.get("anchored"), "knot": body.get("knot") })),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": e })),
        ),
    }
}

#[derive(Deserialize)]
pub struct EscrowPayBody {
    vote_id: u64,
    tx_hash: String,
    voter_address: String,
    signature: Option<String>,
    timestamp: Option<i64>,
}

async fn eth_rpc_result(
    client: &reqwest::Client,
    rpc_urls: &[String],
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let mut last_err = "no RPC endpoint".to_string();
    for url in rpc_urls {
        let resp = client
            .post(url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(15))
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
                last_err = format!("{url}: {e}");
                continue;
            }
        };
        if let Some(result) = parsed.get("result") {
            if result.is_null() {
                last_err = format!("{url}: {method} returned null");
                continue;
            }
            return Ok(result.clone());
        }
        if let Some(err) = parsed.get("error") {
            last_err = format!("{url}: {err}");
        }
    }
    Err(last_err)
}

async fn eth_get_receipt(
    client: &reqwest::Client,
    rpc_urls: &[String],
    tx_hash: &str,
) -> Result<Value, String> {
    eth_rpc_result(
        client,
        rpc_urls,
        "eth_getTransactionReceipt",
        json!([tx_hash]),
    )
    .await
}

async fn eth_get_transaction(
    client: &reqwest::Client,
    rpc_urls: &[String],
    tx_hash: &str,
) -> Result<Value, String> {
    eth_rpc_result(
        client,
        rpc_urls,
        "eth_getTransactionByHash",
        json!([tx_hash]),
    )
    .await
}

pub async fn escrow_pay(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<EscrowPayBody>,
) -> (StatusCode, Json<Value>) {
    let Some(voter) = normalize_address(&payload.voter_address) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "invalid voter_address" })),
        );
    };

    if let Err(resp) = crate::security_guard::check_signer(&voter) {
        return resp;
    }

    let project = match crate::governance_votes::find_project_by_escrow_vote_id(
        &state,
        payload.vote_id,
    )
    .await
    {
        Some(p) => p,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "success": false, "error": "no project linked to this vote_id" })),
            )
        }
    };
    let project_id = project
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let escrow = match vote_escrow_address() {
        Some(a) => a.to_ascii_lowercase(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "success": false, "error": "VOTE_ESCROW_ADDRESS not configured" })),
            )
        }
    };

    let rope_urls: Vec<String> = state.rpc_urls.clone();
    let receipt = match eth_get_receipt(&state.http_client, &rope_urls, &payload.tx_hash).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": format!("could not fetch receipt: {e}") })),
            )
        }
    };

    let status_ok = receipt
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s == "0x1" || s == "1")
        .unwrap_or(false);
    if !status_ok {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "transaction did not succeed on-chain" })),
        );
    }

    let from = receipt
        .get("from")
        .and_then(|v| v.as_str())
        .and_then(|s| normalize_address(s))
        .unwrap_or_default();
    if from != voter {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "receipt from address does not match voter_address" })),
        );
    }

    let to = receipt
        .get("to")
        .and_then(|v| v.as_str())
        .and_then(|s| normalize_address(s))
        .unwrap_or_default();
    if to != escrow {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "receipt to address is not VoteEscrow" })),
        );
    }

    // `value` is on the signed transaction, not the receipt (JSON-RPC shape).
    let tx = match eth_get_transaction(&state.http_client, &rope_urls, &payload.tx_hash).await {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": format!("could not fetch transaction: {e}") })),
            )
        }
    };
    let input = tx.get("input").and_then(|v| v.as_str()).unwrap_or("0x");
    // payToVote(uint256) selector = bytes4(keccak256("payToVote(uint256)"))
    let pay_selector = "0x402cc259";
    if !input.to_ascii_lowercase().starts_with(pay_selector) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "transaction is not a VoteEscrow.payToVote call",
            })),
        );
    }
    let value_hex = tx.get("value").and_then(|v| v.as_str()).unwrap_or("0x0");
    let paid_wei = u128::from_str_radix(value_hex.trim_start_matches("0x"), 16).unwrap_or(0);
    let required_wei: u128 = project
        .get("payToVoteFeeWei")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .or_else(|| pay_to_vote_fee_wei().parse().ok())
        .unwrap_or(100_000_000_000_000_000_000);
    if paid_wei < required_wei {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": format!("paid value {paid_wei} wei is below required fee {required_wei} wei"),
            })),
        );
    }

    if has_paid_vote_right_live(&project_id, payload.vote_id, &voter).await {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "success": false, "error": "pay-to-vote right already recorded for this voter" })),
        );
    }

    let now = chrono::Utc::now().timestamp();
    let record = json!({
        "project_id": project_id,
        "vote_id": payload.vote_id,
        "voter_address": voter,
        "tx_hash": payload.tx_hash,
        "paid_wei": paid_wei.to_string(),
        "recorded_at": now,
    });

    if let Err(e) = append_paid_right_local(&record).await {
        tracing::error!("paid-right write failed: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "could not persist paid right" })),
        );
    }

    let anchored = anchor_governance_event(
        &state,
        "PayToVoteRecorded",
        &record,
        json!({ "project_id": project_id, "vote_id": payload.vote_id, "voter": voter }),
    )
    .await;

    (
        StatusCode::CREATED,
        Json(json!({
            "success": true,
            "paid_right": record,
            "anchored": anchored.is_some(),
            "knot": anchored,
        })),
    )
}

pub async fn finalize_cause(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if let Err(resp) = check_admin_token(&state, &headers).await {
        return resp;
    }

    let now = chrono::Utc::now().timestamp();
    let ballots = load_ballots(&state).await;
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

    let status = crate::governance_votes::effective_status_for_project(project, &ballots, now);
    if status != "approved" {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": format!("project is not approved (effective status: {status})"),
            })),
        );
    }

    let mut fields = Map::new();
    fields.insert("status".into(), json!("awaiting_wallet"));
    fields.insert("finalizedAt".into(), json!(now));

    let updated = match patch_project_fields(&id, fields).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e })),
            )
        }
    };

    let cause_name = updated.get("name").and_then(|v| v.as_str()).unwrap_or(&id);
    let wallet_url = format!("https://dcscan.io/create-wallet?cause={id}&ref=ngo-win");

    let mut emailed: Vec<String> = Vec::new();
    for key in ["submitterEmail", "contactEmail"] {
        if let Some(email) = updated.get(key).and_then(|v| v.as_str()).filter(|e| !e.is_empty())
        {
            state.mailer.send_background(
                email.to_string(),
                "Your NGO was selected - create your Datachain Rope wallet".to_string(),
                format!(
                    "Congratulations - \"{cause_name}\" was approved by the Datachain Rope community.\n\n\
                     Cause ID: {id}\n\n\
                     Next steps:\n\
                     1. Create your Datachain Rope wallet: {wallet_url}\n\
                     2. Register your NGO treasury on the cause page (EIP-191 signature required).\n\
                     3. After registration, claim your legacy ERC-20 DC grant on Ethereum and your FAT cause-token grant on Datachain Rope.\n\n\
                     Track progress at https://dcscan.io/vote - search for {id}.\n\n\
                     - Datachain Foundation"
                ),
            );
            emailed.push(email.to_string());
        }
    }

    let anchored = anchor_governance_event(
        &state,
        "CauseWinnerSelected",
        &updated,
        json!({ "project_id": id, "wallet_url": wallet_url }),
    )
    .await;

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "project": updated,
            "walletUrl": wallet_url,
            "emailed": emailed,
            "anchored": anchored.is_some(),
            "knot": anchored,
        })),
    )
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
    let signature = Signature::try_from(&raw[..64]).map_err(|e| format!("signature parse: {e}"))?;
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

#[derive(Deserialize)]
pub struct RegisterTreasuryBody {
    treasury_address: String,
    signature: String,
    timestamp: i64,
}

const TREASURY_AUTH_WINDOW_SECS: i64 = 300;

pub async fn register_treasury(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(payload): Json<RegisterTreasuryBody>,
) -> (StatusCode, Json<Value>) {
    let treasury = match normalize_address(&payload.treasury_address) {
        Some(a) => a,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": "invalid treasury_address" })),
            )
        }
    };

    if let Err(resp) = crate::security_guard::check_signer(&treasury) {
        return resp;
    }

    let now = chrono::Utc::now().timestamp();
    if (now - payload.timestamp).abs() > TREASURY_AUTH_WINDOW_SECS {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "timestamp outside freshness window" })),
        );
    }

    let message = format!(
        "{TREASURY_AUTH_DOMAIN}\n{id}\n{treasury}\n{}",
        payload.timestamp
    );
    let recovered = match recover_signer(message.as_bytes(), &payload.signature) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": format!("signature verification failed: {e}") })),
            )
        }
    };
    if recovered != treasury {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "success": false, "error": "signature does not match treasury_address" })),
        );
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

    let milestones = project.get("milestones").cloned().unwrap_or(json!([]));

    let mut fields = Map::new();
    fields.insert("ngoTreasury".into(), json!(treasury));
    fields.insert("status".into(), json!("contractualizing"));
    fields.insert("treasuryRegisteredAt".into(), json!(now));

    let updated = match patch_project_fields(&id, fields).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e })),
            )
        }
    };

    let anchored = anchor_governance_event(
        &state,
        "CauseContractualized",
        &updated,
        json!({
            "project_id": id,
            "treasury": treasury,
            "milestones": milestones,
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

fn refuse_compromised_key(hex_key: &str) -> Option<(StatusCode, Json<Value>)> {
    let sk_bytes = match hex::decode(hex_key.trim_start_matches("0x").trim_start_matches("0X")) {
        Ok(b) if b.len() == 32 => b,
        _ => return None,
    };
    let signing_key = match SigningKey::from_bytes(sk_bytes.as_slice().into()) {
        Ok(k) => k,
        Err(_) => return None,
    };
    let pk = VerifyingKey::from(&signing_key);
    let encoded = pk.to_encoded_point(false);
    let raw = &encoded.as_bytes()[1..];
    let mut h = Keccak256::new();
    h.update(raw);
    let digest = h.finalize();
    let addr = format!("0x{}", hex::encode(&digest[12..]));
    if addr == COMPROMISED_DEPLOYER {
        Some((
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "refusing compromised deployer key for grant execution",
            })),
        ))
    } else {
        None
    }
}

async fn eth_call_balance_of(
    client: &reqwest::Client,
    rpc_urls: &[String],
    token: &str,
    holder: &str,
) -> Result<u128, String> {
    let holder_norm = normalize_address(holder).ok_or_else(|| "invalid holder".to_string())?;
    let mut slot = [0u8; 32];
    slot[12..].copy_from_slice(
        &hex::decode(holder_norm.trim_start_matches("0x")).map_err(|e| e.to_string())?,
    );
    let call_data = format!("0x70a08231{}", hex::encode(slot));
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_call",
        "params": [{ "to": token, "data": call_data }, "latest"],
    });
    let mut last_err = "no RPC".to_string();
    for url in rpc_urls {
        let resp = client
            .post(url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(15))
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
                last_err = format!("{url}: {e}");
                continue;
            }
        };
        if let Some(result) = parsed.get("result").and_then(|r| r.as_str()) {
            let hex_val = result.trim_start_matches("0x");
            if hex_val.is_empty() {
                return Ok(0);
            }
            return u128::from_str_radix(hex_val, 16).map_err(|e| e.to_string());
        }
    }
    Err(last_err)
}

async fn eth_rpc_call(
    client: &reqwest::Client,
    rpc_urls: &[String],
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let mut last_err = "no RPC".to_string();
    for url in rpc_urls {
        let resp = client
            .post(url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(20))
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
                last_err = format!("{url}: {e}");
                continue;
            }
        };
        if let Some(result) = parsed.get("result") {
            return Ok(result.clone());
        }
        if let Some(err) = parsed.get("error") {
            last_err = format!("{url}: {err}");
        }
    }
    Err(last_err)
}

fn encode_erc20_transfer(to: &str, amount: u128) -> Vec<u8> {
    let mut data = vec![0xa9, 0x05, 0x9c, 0xbb];
    let to_norm = normalize_address(to).expect("treasury");
    let to_bytes = hex::decode(to_norm.trim_start_matches("0x")).expect("hex");
    let mut addr_slot = [0u8; 32];
    addr_slot[12..].copy_from_slice(&to_bytes);
    data.extend_from_slice(&addr_slot);
    let mut amt_slot = [0u8; 32];
    amt_slot[16..].copy_from_slice(&amount.to_be_bytes());
    data.extend_from_slice(&amt_slot);
    data
}

fn rlp_encode_u64(n: u64) -> Vec<u8> {
    if n == 0 {
        return vec![0x80];
    }
    let mut bytes = n.to_be_bytes().to_vec();
    while !bytes.is_empty() && bytes[0] == 0 {
        bytes.remove(0);
    }
    if bytes.first().copied().unwrap_or(0) & 0x80 != 0 {
        bytes.insert(0, 0);
    }
    let mut out = vec![0x80 + bytes.len() as u8];
    out.extend_from_slice(&bytes);
    out
}

fn rlp_encode_u128(n: u128) -> Vec<u8> {
    if n == 0 {
        return vec![0x80];
    }
    let mut bytes = n.to_be_bytes().to_vec();
    while !bytes.is_empty() && bytes[0] == 0 {
        bytes.remove(0);
    }
    if bytes.first().copied().unwrap_or(0) & 0x80 != 0 {
        bytes.insert(0, 0);
    }
    let mut out = vec![0x80 + bytes.len() as u8];
    out.extend_from_slice(&bytes);
    out
}

fn rlp_encode_bytes(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return vec![0x80];
    }
    if data.len() == 1 && data[0] < 0x80 {
        return vec![data[0]];
    }
    if data.len() <= 55 {
        let mut out = vec![0x80 + data.len() as u8];
        out.extend_from_slice(data);
        out
    } else {
        let len_bytes = (data.len() as u64).to_be_bytes();
        let len_start = len_bytes.iter().position(|&b| b != 0).unwrap_or(7);
        let len_slice = &len_bytes[len_start..];
        let mut out = vec![0xb7 + len_slice.len() as u8];
        out.extend_from_slice(len_slice);
        out.extend_from_slice(data);
        out
    }
}

fn rlp_encode_list(items: &[Vec<u8>]) -> Vec<u8> {
    let payload: Vec<u8> = items.iter().flatten().copied().collect();
    if payload.len() <= 55 {
        let mut out = vec![0xc0 + payload.len() as u8];
        out.extend_from_slice(&payload);
        out
    } else {
        let len_bytes = (payload.len() as u64).to_be_bytes();
        let len_start = len_bytes.iter().position(|&b| b != 0).unwrap_or(7);
        let len_slice = &len_bytes[len_start..];
        let mut out = vec![0xf7 + len_slice.len() as u8];
        out.extend_from_slice(len_slice);
        out.extend_from_slice(&payload);
        out
    }
}

fn sign_legacy_eip155_tx(
    chain_id: u64,
    nonce: u64,
    gas_price: u128,
    gas_limit: u64,
    to: &[u8; 20],
    value: u128,
    data: &[u8],
    signing_key: &SigningKey,
) -> Result<Vec<u8>, String> {
    let unsigned = rlp_encode_list(&[
        rlp_encode_u64(nonce),
        rlp_encode_u128(gas_price),
        rlp_encode_u64(gas_limit),
        rlp_encode_bytes(to),
        rlp_encode_u128(value),
        rlp_encode_bytes(data),
        rlp_encode_u64(chain_id),
        rlp_encode_bytes(&[]),
        rlp_encode_bytes(&[]),
    ]);

    let mut h = Keccak256::new();
    h.update(&unsigned);
    let hash = h.finalize();

    let (sig, recid) = signing_key
        .sign_prehash_recoverable(hash.as_slice())
        .map_err(|e| format!("sign: {e}"))?;

    let v = u64::from(recid.to_byte()) + 35 + chain_id * 2;
    let sig_bytes = sig.to_bytes();
    let r = &sig_bytes[0..32];
    let s = &sig_bytes[32..64];

    let signed = rlp_encode_list(&[
        rlp_encode_u64(nonce),
        rlp_encode_u128(gas_price),
        rlp_encode_u64(gas_limit),
        rlp_encode_bytes(to),
        rlp_encode_u128(value),
        rlp_encode_bytes(data),
        rlp_encode_u64(v),
        rlp_encode_bytes(r),
        rlp_encode_bytes(s),
    ]);
    Ok(signed)
}

pub async fn execute_dc_grant(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if let Err(resp) = check_admin_token(&state, &headers).await {
        return resp;
    }

    let key_hex = match std::env::var("LEGACY_DC_HOLDER_PRIVATE_KEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "success": false,
                    "error": "LEGACY_DC_HOLDER_PRIVATE_KEY not configured - grant not executed",
                })),
            )
        }
    };

    if let Some(resp) = refuse_compromised_key(&key_hex) {
        return resp;
    }

    let sk_bytes = hex::decode(key_hex.trim_start_matches("0x").trim_start_matches("0X"))
        .ok()
        .filter(|b| b.len() == 32);
    let Some(sk_bytes) = sk_bytes else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "invalid LEGACY_DC_HOLDER_PRIVATE_KEY" })),
        );
    };
    let signing_key = match SigningKey::from_bytes(sk_bytes.as_slice().into()) {
        Ok(k) => k,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": format!("invalid holder key: {e}") })),
            )
        }
    };
    let pk = VerifyingKey::from(&signing_key);
    let encoded = pk.to_encoded_point(false);
    let raw_pk = &encoded.as_bytes()[1..];
    let mut h = Keccak256::new();
    h.update(raw_pk);
    let digest = h.finalize();
    let holder_addr = format!("0x{}", hex::encode(&digest[12..]));

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

    let treasury = project
        .get("ngoTreasury")
        .and_then(|v| v.as_str())
        .or_else(|| project.get("ngo_treasury").and_then(|v| v.as_str()));
    let Some(treasury) = treasury.and_then(|t| normalize_address(t)) else {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "success": false, "error": "register treasury first" })),
        );
    };

    let token = legacy_dc_eth_address();
    let eth_urls = eth_rpc_urls();

    let balance = match eth_call_balance_of(&state.http_client, &eth_urls, &token, &holder_addr).await
    {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "success": false, "error": format!("balanceOf failed: {e}") })),
            )
        }
    };
    if balance == 0 {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": "holder has zero legacy DC balance on Ethereum",
                "holder": holder_addr,
                "token": token,
            })),
        );
    }

    let nonce_hex = eth_rpc_call(
        &state.http_client,
        &eth_urls,
        "eth_getTransactionCount",
        json!([holder_addr, "pending"]),
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(|s| s.to_string()))
    .unwrap_or_else(|| "0x0".to_string());
    let nonce = u64::from_str_radix(nonce_hex.trim_start_matches("0x"), 16).unwrap_or(0);

    let gas_price_hex = eth_rpc_call(
        &state.http_client,
        &eth_urls,
        "eth_gasPrice",
        json!([]),
    )
    .await
    .ok()
    .and_then(|v| v.as_str().map(|s| s.to_string()))
    .unwrap_or_else(|| "0x3b9aca00".to_string());
    let gas_price = u128::from_str_radix(gas_price_hex.trim_start_matches("0x"), 16).unwrap_or(1_000_000_000);

    let token_bytes: [u8; 20] = {
        let b = hex::decode(token.trim_start_matches("0x")).unwrap();
        let mut arr = [0u8; 20];
        arr.copy_from_slice(&b);
        arr
    };

    let calldata = encode_erc20_transfer(&treasury, balance);
    let raw_tx = match sign_legacy_eip155_tx(
        1,
        nonce,
        gas_price,
        120_000,
        &token_bytes,
        0,
        &calldata,
        &signing_key,
    ) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e })),
            )
        }
    };

    let raw_hex = format!("0x{}", hex::encode(&raw_tx));
    let send_result = eth_rpc_call(
        &state.http_client,
        &eth_urls,
        "eth_sendRawTransaction",
        json!([raw_hex]),
    )
    .await;

    let tx_hash = match send_result {
        Ok(v) => v.as_str().unwrap_or("").to_string(),
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "success": false, "error": format!("eth_sendRawTransaction failed: {e}") })),
            )
        }
    };

    let now = chrono::Utc::now().timestamp();
    let mut fields = Map::new();
    fields.insert("dcGrantTxHash".into(), json!(tx_hash));
    fields.insert("dcGrantAmountWei".into(), json!(balance.to_string()));
    fields.insert("dcGrantExecutedAt".into(), json!(now));
    fields.insert("status".into(), json!("granted"));

    let updated = match patch_project_fields(&id, fields).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e, "tx_hash": tx_hash })),
            )
        }
    };

    let grant_record = json!({
        "project_id": id,
        "treasury": treasury,
        "token": token,
        "amount_wei": balance.to_string(),
        "tx_hash": tx_hash,
        "holder": holder_addr,
        "executed_at": now,
    });
    let _ = append_ngo_grant_local(&grant_record).await;

    let anchored = anchor_governance_event(
        &state,
        "DcGrantExecuted",
        &grant_record,
        json!({ "project_id": id, "tx_hash": tx_hash }),
    )
    .await;

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "project": updated,
            "txHash": tx_hash,
            "amountWei": balance.to_string(),
            "holder": holder_addr,
            "token": token,
            "note": "LEGACY_DC_ETH_ADDRESS is the ERC-20 token contract; LEGACY_DC_HOLDER_PRIVATE_KEY is the EOA that holds the balance",
            "anchored": anchored.is_some(),
            "knot": anchored,
        })),
    )
}

fn abi_encode_string(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = vec![0u8; 32];
    out[24..].copy_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
    let pad = (32 - (bytes.len() % 32)) % 32;
    out.extend(std::iter::repeat(0u8).take(pad));
    out
}

fn encode_grant_cause(
    cause_id: &[u8; 32],
    treasury: &str,
    name: &str,
    symbol: &str,
    max_supply: u128,
    fat_grant_wei: u128,
) -> Result<Vec<u8>, String> {
    // grantCause(bytes32,address,string,string,uint256,uint256)
    let mut data = hex::decode("7d93132b").map_err(|e| e.to_string())?;
    data.extend_from_slice(cause_id);
    let mut addr_slot = [0u8; 32];
    let tb = hex::decode(treasury.trim_start_matches("0x")).map_err(|e| e.to_string())?;
    if tb.len() != 20 {
        return Err("treasury must be 20 bytes".into());
    }
    addr_slot[12..].copy_from_slice(&tb);
    data.extend_from_slice(&addr_slot);
    // offsets into the ABI head (6 words = 192)
    let mut off_name = [0u8; 32];
    off_name[31] = 0xc0; // 192
    data.extend_from_slice(&off_name);
    let name_enc = abi_encode_string(name);
    let mut off_sym = [0u8; 32];
    let sym_off = 192u64 + name_enc.len() as u64;
    off_sym[24..].copy_from_slice(&sym_off.to_be_bytes());
    data.extend_from_slice(&off_sym);
    let mut max_slot = [0u8; 32];
    max_slot[16..].copy_from_slice(&max_supply.to_be_bytes());
    data.extend_from_slice(&max_slot);
    let mut fat_slot = [0u8; 32];
    fat_slot[16..].copy_from_slice(&fat_grant_wei.to_be_bytes());
    data.extend_from_slice(&fat_slot);
    data.extend_from_slice(&name_enc);
    data.extend_from_slice(&abi_encode_string(symbol));
    Ok(data)
}

fn encode_fund_grant(cause_id: &[u8; 32]) -> Vec<u8> {
    // fundGrant(bytes32)
    let mut data = hex::decode("9e1cdcfe").expect("hex");
    data.extend_from_slice(cause_id);
    data
}

fn cause_id_from_project(id: &str) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(id.as_bytes());
    let out = hasher.finalize();
    let mut id32 = [0u8; 32];
    id32.copy_from_slice(&out);
    id32
}

async fn send_signed_legacy_tx(
    client: &reqwest::Client,
    rpc_urls: &[String],
    chain_id: u64,
    from_key: &SigningKey,
    to: &[u8; 20],
    value: u128,
    data: &[u8],
    gas_limit: u64,
) -> Result<String, String> {
    let pk = VerifyingKey::from(from_key);
    let encoded = pk.to_encoded_point(false);
    let raw_pk = &encoded.as_bytes()[1..];
    let mut h = Keccak256::new();
    h.update(raw_pk);
    let digest = h.finalize();
    let from = format!("0x{}", hex::encode(&digest[12..]));

    let nonce_hex = eth_rpc_call(
        client,
        rpc_urls,
        "eth_getTransactionCount",
        json!([from, "pending"]),
    )
    .await?
    .as_str()
    .unwrap_or("0x0")
    .to_string();
    let nonce = u64::from_str_radix(nonce_hex.trim_start_matches("0x"), 16).unwrap_or(0);
    let gas_price_hex = eth_rpc_call(client, rpc_urls, "eth_gasPrice", json!([]))
        .await?
        .as_str()
        .unwrap_or("0x3b9aca00")
        .to_string();
    let gas_price =
        u128::from_str_radix(gas_price_hex.trim_start_matches("0x"), 16).unwrap_or(1_000_000_000);

    let raw_tx = sign_legacy_eip155_tx(
        chain_id, nonce, gas_price, gas_limit, to, value, data, from_key,
    )?;
    let raw_hex = format!("0x{}", hex::encode(&raw_tx));
    let send_result = eth_rpc_call(client, rpc_urls, "eth_sendRawTransaction", json!([raw_hex])).await?;
    send_result
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("eth_sendRawTransaction returned non-string: {send_result}"))
}

pub async fn grant_cause_token(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if let Err(resp) = check_admin_token(&state, &headers).await {
        return resp;
    }

    let key_hex = std::env::var("CAUSE_TOKEN_GRANTOR_PRIVATE_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("VOTE_ESCROW_CREATOR_PRIVATE_KEY").ok())
        .filter(|s| !s.trim().is_empty());
    let Some(key_hex) = key_hex else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "success": false,
                "error": "CAUSE_TOKEN_GRANTOR_PRIVATE_KEY (or VOTE_ESCROW_CREATOR_PRIVATE_KEY) not configured",
            })),
        );
    };
    if let Some(resp) = refuse_compromised_key(&key_hex) {
        return resp;
    }
    let sk_bytes = hex::decode(key_hex.trim_start_matches("0x").trim_start_matches("0X"))
        .ok()
        .filter(|b| b.len() == 32);
    let Some(sk_bytes) = sk_bytes else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "invalid grantor private key" })),
        );
    };
    let signing_key = match SigningKey::from_bytes(sk_bytes.as_slice().into()) {
        Ok(k) => k,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": format!("invalid grantor key: {e}") })),
            )
        }
    };

    let factory = match std::env::var("CAUSE_TOKEN_FACTORY_ADDRESS") {
        Ok(a) if !a.trim().is_empty() => a,
        _ => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "success": false, "error": "CAUSE_TOKEN_FACTORY_ADDRESS not configured" })),
            )
        }
    };
    let fat_grant: u128 = std::env::var("CAUSE_TOKEN_FAT_GRANT_WEI")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000_000_000_000_000); // 1 FAT default
    let max_supply: u128 = std::env::var("CAUSE_TOKEN_MAX_SUPPLY_WEI")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000_000_000_000_000_000_000); // 1M * 1e18

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

    let treasury = project
        .get("ngoTreasury")
        .and_then(|v| v.as_str())
        .and_then(|t| normalize_address(t));
    let Some(treasury) = treasury else {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "success": false, "error": "register treasury first" })),
        );
    };

    let cause_name = project
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&id);
    let token_name = format!("{cause_name} Cause");
    let token_symbol = {
        let cleaned: String = cause_name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(6)
            .collect::<String>()
            .to_ascii_uppercase();
        if cleaned.is_empty() {
            "CAUSE".to_string()
        } else {
            cleaned
        }
    };

    let cause_id = cause_id_from_project(&id);
    let factory_bytes: [u8; 20] = {
        let b = hex::decode(factory.trim_start_matches("0x")).unwrap_or_default();
        let mut arr = [0u8; 20];
        if b.len() != 20 {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": "CAUSE_TOKEN_FACTORY_ADDRESS invalid" })),
            );
        }
        arr.copy_from_slice(&b);
        arr
    };

    let grant_data = match encode_grant_cause(
        &cause_id,
        &treasury,
        &token_name,
        &token_symbol,
        max_supply,
        fat_grant,
    ) {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e })),
            )
        }
    };

    let rope_urls = state.rpc_urls.clone();
    let chain_id = rope_chain_id();
    let grant_tx = match send_signed_legacy_tx(
        &state.http_client,
        &rope_urls,
        chain_id,
        &signing_key,
        &factory_bytes,
        0,
        &grant_data,
        400_000,
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "success": false, "error": format!("grantCause tx failed: {e}") })),
            )
        }
    };

    // Brief pause so the next nonce is ready; fundGrant is permissionless.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let fund_data = encode_fund_grant(&cause_id);
    let fund_tx = match send_signed_legacy_tx(
        &state.http_client,
        &rope_urls,
        chain_id,
        &signing_key,
        &factory_bytes,
        fat_grant,
        &fund_data,
        200_000,
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "success": false,
                    "error": format!("fundGrant tx failed after grantCause: {e}"),
                    "grantCauseTx": grant_tx,
                })),
            )
        }
    };

    let grant = json!({
        "projectId": id,
        "treasury": treasury,
        "asset": "native FAT (Datachain Rope)",
        "fatGrantWei": fat_grant.to_string(),
        "maxSupplyWei": max_supply.to_string(),
        "tokenName": token_name,
        "tokenSymbol": token_symbol,
        "causeId": format!("0x{}", hex::encode(cause_id)),
        "factory": factory,
        "grantCauseTx": grant_tx,
        "fundGrantTx": fund_tx,
        "claimHint": "NGO treasury calls CauseTokenFactory.claimGrant(causeId) to receive FAT + DCR-20 CauseToken",
        "recordedAt": chrono::Utc::now().timestamp(),
    });

    let mut fields = Map::new();
    fields.insert("causeTokenGrant".into(), grant.clone());
    fields.insert("causeTokenGrantTxHash".into(), json!(fund_tx));
    fields.insert("status".into(), json!("cause_token_funded"));

    let updated = match patch_project_fields(&id, fields).await {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "error": e, "grant": grant })),
            )
        }
    };

    if let Some(email) = project
        .get("contactEmail")
        .or_else(|| project.get("submitterEmail"))
        .and_then(|v| v.as_str())
        .filter(|e| !e.is_empty())
    {
        state.mailer.send_background(
            email.to_string(),
            format!("Cause token FAT grant funded - {id}"),
            format!(
                "Your approved cause \"{cause_name}\" has a native FAT grant funded on Datachain Rope.\n\n\
                 Treasury: {treasury}\n\
                 Factory: {factory}\n\
                 Cause ID (bytes32): 0x{}\n\
                 FAT grant: {fat_grant} wei\n\
                 fundGrant tx: {fund_tx}\n\n\
                 Next: from your treasury wallet, call claimGrant on the factory to receive the FAT \
                 and your DCR-20 Cause Token.\n\n\
                 - Datachain Foundation",
                hex::encode(cause_id),
            ),
        );
    }

    let anchored = anchor_governance_event(
        &state,
        "CauseTokenGrantRecorded",
        &updated,
        json!({ "project_id": id, "treasury": treasury }),
    )
    .await;

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "project": updated,
            "causeTokenGrant": grant,
            "grantCauseTx": grant_tx,
            "fundGrantTx": fund_tx,
            "anchored": anchored.is_some(),
            "knot": anchored,
        })),
    )
}

pub fn voter_may_attest_cause(
    project: &Value,
    paid_records: &[Value],
    vote_id: u64,
    voter: &str,
) -> (bool, bool, bool) {
    if !eligible_voter_set_is_jury_and_pay(project) {
        return (true, false, false);
    }
    let jury = project_jury_list(project);
    let is_juror_flag = is_juror(&jury, voter);
    let project_id = project.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let has_paid = has_paid_vote_right(paid_records, project_id, vote_id, voter);
    let allowed = is_juror_flag || has_paid;
    (allowed, is_juror_flag, has_paid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_paid_vote_right_matches_normalized() {
        let records = vec![json!({
            "project_id": "cause-abc",
            "vote_id": 7,
            "voter_address": "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd",
        })];
        assert!(has_paid_vote_right(
            &records,
            "cause-abc",
            7,
            "0xABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD"
        ));
        assert!(!has_paid_vote_right(
            &records,
            "cause-abc",
            8,
            "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd"
        ));
    }

    #[test]
    fn encode_erc20_transfer_selector() {
        let data = encode_erc20_transfer(
            "0x00000000000000000000000000000000000000dEaD",
            1_000,
        );
        assert_eq!(&data[0..4], &[0xa9, 0x05, 0x9c, 0xbb]);
        assert_eq!(data.len(), 4 + 32 + 32);
    }

    #[test]
    fn voter_may_attest_open_when_not_jury_mode() {
        let project = json!({ "id": "cause-x", "eligibleVoterSet": "all_holders" });
        let (ok, _, _) = voter_may_attest_cause(
            &project,
            &[],
            1,
            "0x0000000000000000000000000000000000000001",
        );
        assert!(ok);
    }

    #[test]
    fn voter_may_attest_jury_or_paid() {
        let project = json!({
            "id": "cause-y",
            "eligibleVoterSet": "jury_and_pay",
            "jury": ["0x0000000000000000000000000000000000000002"]
        });
        let (ok_j, is_j, has_p) = voter_may_attest_cause(
            &project,
            &[],
            3,
            "0x0000000000000000000000000000000000000002",
        );
        assert!(ok_j && is_j && !has_p);

        let records = vec![json!({
            "project_id": "cause-y",
            "vote_id": 3,
            "voter_address": "0x0000000000000000000000000000000000000003",
        })];
        let (ok_p, is_j2, has_p2) = voter_may_attest_cause(
            &project,
            &records,
            3,
            "0x0000000000000000000000000000000000000003",
        );
        assert!(ok_p && !is_j2 && has_p2);
    }
}
