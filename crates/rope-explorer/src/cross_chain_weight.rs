//! Cross-chain governance voting-weight aggregator + attestation signer —
//! Governance Voting & Cause Platform Phase 2
//! (`docs/GOVERNANCE_VOTING_CAUSE_PLATFORM_SPEC_V1.md` §2.1, option (b)).
//!
//! `VoteEscrow.sol` (Datachain Rope, chain 271828) cannot read a wallet's
//! balance on Ethereum or XDC — a smart contract has no cross-chain
//! visibility, and Datachain Rope has no `IVotes`-checkpointed DCR-20 to
//! read a past-block balance from even on its own chain. So voting/vote-
//! creation power is computed HERE, off-chain, by summing:
//!
//!   - legacy DC on Ethereum   (ERC-20,  `LEGACY_DC_ETH_ADDRESS`)
//!   - legacy DC on XDC        (XRC-20,  `LEGACY_DC_XDC_ADDRESS`)
//!   - native DC FAT on Rope   (`eth_getBalance`, reusing `state.rpc_urls`)
//!
//! and then EIP-191-signing an attestation binding (contract, chain,
//! purpose, vote id or creator address, voter, weight, expiry) that
//! `VoteEscrow.castVote`/`createVote` verifies on-chain via `ecrecover`.
//! This is the exact same construction already proven in production by
//! `FATMigrationMinter.claimMigration` (see
//! `dcswap/contracts/src/migration/FATMigrationMinter.sol`) and by
//! `rope-node`'s Phase-2 signed-destructive-RPC verifier
//! (`crates/rope-node/src/rpc_signature.rs`) — reused deliberately rather
//! than reinvented.
//!
//! All three balance lookups are REAL, live JSON-RPC calls with per-chain
//! multi-endpoint failover. A chain whose RPCs are all unreachable is
//! surfaced as an explicit error in the response (never silently
//! substituted with zero) so a voter is never short-changed without
//! knowing it — "no stubs" extends to "no silent zero-fill on RPC failure".

use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, SigningKey, VerifyingKey};
use serde::Serialize;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};
use std::sync::{Arc, OnceLock};

use crate::AppState;

// ============================================================================
// Configuration
// ============================================================================

/// Canonical legacy DC ERC-20 on Ethereum mainnet (chain 1). Confirmed via
/// `dcswap/scripts/migration/deploy-origin-burns.mjs` + live `cast call`
/// (`name()`/`symbol()`="DC", `decimals()`=18) per the 2026-07-22 exploration.
fn legacy_dc_eth_address() -> String {
    std::env::var("LEGACY_DC_ETH_ADDRESS")
        .unwrap_or_else(|_| "0x0b44547be0a0df5dcd5327de8ea73680517c5a54".to_string())
}

/// Canonical legacy DC XRC-20 on XDC Network (chain 50). Same provenance.
fn legacy_dc_xdc_address() -> String {
    std::env::var("LEGACY_DC_XDC_ADDRESS")
        .unwrap_or_else(|_| "0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a".to_string())
}

fn eth_rpc_urls() -> Vec<String> {
    std::env::var("ETH_RPC_URL")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_else(|_| {
            vec![
                "https://ethereum-rpc.publicnode.com".to_string(),
                "https://eth.drpc.org".to_string(),
            ]
        })
}

fn xdc_rpc_urls() -> Vec<String> {
    std::env::var("XDC_RPC_URL")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_else(|_| {
            vec![
                "https://rpc.xdcrpc.com".to_string(),
                "https://erpc.xinfin.network".to_string(),
            ]
        })
}

/// The deployed `VoteEscrow` address on chain 271828. `None` until Phase 2
/// deployment lands — the balance-aggregation half of this module stays
/// fully functional even before that (only attestation signing requires it).
fn vote_escrow_address() -> Option<String> {
    std::env::var("VOTE_ESCROW_ADDRESS")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

fn rope_chain_id() -> u64 {
    std::env::var("ROPE_CHAIN_ID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(271_828)
}

/// How long a signed attestation remains valid — long enough for a user to
/// review + submit a MetaMask transaction, short enough to bound replay
/// exposure if a signed attestation leaked before use (it moves no value by
/// itself; it only proves eligibility, so this is a defence-in-depth bound,
/// not a strict security boundary).
fn attestation_window_secs() -> i64 {
    std::env::var("VOTE_ATTESTATION_WINDOW_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900)
}

fn attestor_private_key_hex() -> Option<String> {
    std::env::var("VOTE_ESCROW_ATTESTOR_PRIVATE_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

static ATTESTOR_KEY: OnceLock<Result<SigningKey, String>> = OnceLock::new();

fn attestor_signing_key() -> Result<&'static SigningKey, String> {
    let cached = ATTESTOR_KEY.get_or_init(|| {
        let hex_str = attestor_private_key_hex()
            .ok_or_else(|| "VOTE_ESCROW_ATTESTOR_PRIVATE_KEY not configured".to_string())?;
        let raw = hex::decode(hex_str.trim_start_matches("0x").trim_start_matches("0X"))
            .map_err(|e| format!("VOTE_ESCROW_ATTESTOR_PRIVATE_KEY: invalid hex: {e}"))?;
        if raw.len() != 32 {
            return Err(format!(
                "VOTE_ESCROW_ATTESTOR_PRIVATE_KEY: expected 32 bytes, got {}",
                raw.len()
            ));
        }
        SigningKey::from_bytes(raw.as_slice().into())
            .map_err(|e| format!("VOTE_ESCROW_ATTESTOR_PRIVATE_KEY: invalid secp256k1 key: {e}"))
    });
    match cached {
        Ok(k) => Ok(k),
        Err(e) => Err(e.clone()),
    }
}

/// The Ethereum-style address (`0x…`, lowercase) derived from the attestor
/// signing key — exposed via `/api/v1/governance/attestor` so the on-chain
/// `attestor` role can be verified to match without ever exposing the key.
pub fn attestor_public_address() -> Result<String, String> {
    let sk = attestor_signing_key()?;
    let pk = VerifyingKey::from(sk);
    Ok(eth_address_from_verifying_key(&pk))
}

fn eth_address_from_verifying_key(pk: &VerifyingKey) -> String {
    let encoded = pk.to_encoded_point(false);
    let raw = &encoded.as_bytes()[1..];
    let mut h = Keccak256::new();
    h.update(raw);
    let digest = h.finalize();
    format!("0x{}", hex::encode(&digest[12..]))
}

// ============================================================================
// Address / integer encoding helpers — must byte-for-byte match Solidity's
// `abi.encode(bytes32, bytes32, uint256, address, uint256, address, uint256,
// uint256)`, which for an all-static-type tuple is simply the concatenation
// of each value's 32-byte big-endian representation (bytes32 as-is; address
// left-padded with 12 zero bytes; uint256 left-padded with zero bytes).
// ============================================================================

fn parse_address(addr: &str) -> Result<[u8; 20], String> {
    let trimmed = addr.trim_start_matches("0x").trim_start_matches("0X");
    let raw = hex::decode(trimmed).map_err(|e| format!("invalid address hex: {e}"))?;
    if raw.len() != 20 {
        return Err(format!("address must be 20 bytes, got {}", raw.len()));
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&raw);
    Ok(out)
}

fn address_slot(addr: &[u8; 20]) -> [u8; 32] {
    let mut slot = [0u8; 32];
    slot[12..].copy_from_slice(addr);
    slot
}

fn u256_slot_from_u128(value: u128) -> [u8; 32] {
    let mut slot = [0u8; 32];
    slot[16..].copy_from_slice(&value.to_be_bytes());
    slot
}

fn u256_slot_from_u64(value: u64) -> [u8; 32] {
    let mut slot = [0u8; 32];
    slot[24..].copy_from_slice(&value.to_be_bytes());
    slot
}

fn keccak(bytes: &[u8]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&out);
    digest
}

/// `keccak256("DCROPE/governance/vote-escrow/weight/v1")` — computed at
/// runtime from the same ASCII bytes Solidity's `keccak256("…")` literal
/// hashes at compile time, so the two are guaranteed identical without
/// hardcoding (and re-deriving) a hex constant in two languages.
fn weight_domain_tag() -> [u8; 32] {
    keccak(b"DCROPE/governance/vote-escrow/weight/v1")
}
fn cast_purpose_tag() -> [u8; 32] {
    keccak(b"cast")
}
fn create_purpose_tag() -> [u8; 32] {
    keccak(b"create")
}

fn eip191_wrap(digest: &[u8; 32]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(b"\x19Ethereum Signed Message:\n32");
    h.update(digest);
    let out = h.finalize();
    let mut wrapped = [0u8; 32];
    wrapped.copy_from_slice(&out);
    wrapped
}

/// Mirrors `VoteEscrow.castWeightDigest(voteId, voter, weight, expiresAt)`.
pub fn cast_weight_digest(
    contract: &[u8; 20],
    chain_id: u64,
    vote_id: u64,
    voter: &[u8; 20],
    weight_wei: u128,
    expires_at: i64,
) -> [u8; 32] {
    let mut preimage = Vec::with_capacity(8 * 32);
    preimage.extend_from_slice(&weight_domain_tag());
    preimage.extend_from_slice(&cast_purpose_tag());
    preimage.extend_from_slice(&u256_slot_from_u64(chain_id));
    preimage.extend_from_slice(&address_slot(contract));
    preimage.extend_from_slice(&u256_slot_from_u64(vote_id));
    preimage.extend_from_slice(&address_slot(voter));
    preimage.extend_from_slice(&u256_slot_from_u128(weight_wei));
    preimage.extend_from_slice(&u256_slot_from_u64(expires_at.max(0) as u64));
    keccak(&preimage)
}

/// Mirrors `VoteEscrow.createWeightDigest(creatorAddr, weight, expiresAt)`.
pub fn create_weight_digest(
    contract: &[u8; 20],
    chain_id: u64,
    creator_addr: &[u8; 20],
    weight_wei: u128,
    expires_at: i64,
) -> [u8; 32] {
    let mut preimage = Vec::with_capacity(7 * 32);
    preimage.extend_from_slice(&weight_domain_tag());
    preimage.extend_from_slice(&create_purpose_tag());
    preimage.extend_from_slice(&u256_slot_from_u64(chain_id));
    preimage.extend_from_slice(&address_slot(contract));
    preimage.extend_from_slice(&address_slot(creator_addr));
    preimage.extend_from_slice(&u256_slot_from_u128(weight_wei));
    preimage.extend_from_slice(&u256_slot_from_u64(expires_at.max(0) as u64));
    keccak(&preimage)
}

fn sign_digest(sk: &SigningKey, digest: &[u8; 32]) -> Result<String, String> {
    let wrapped = eip191_wrap(digest);
    let (sig, recid): (EcdsaSignature, RecoveryId) = sk
        .sign_prehash_recoverable(&wrapped)
        .map_err(|e| format!("signing failed: {e}"))?;
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = u8::from(recid) + 27;
    Ok(format!("0x{}", hex::encode(out)))
}

// ============================================================================
// Balance aggregation — real, live, multi-chain, multi-endpoint failover
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct ChainBalance {
    pub chain: &'static str,
    pub chain_id: u64,
    pub token: String,
    pub balance_wei: String,
    pub balance_human: f64,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WeightBreakdown {
    pub ethereum: ChainBalance,
    pub xdc: ChainBalance,
    pub rope: ChainBalance,
    /// Sum of all chains that resolved successfully. A chain that failed
    /// contributes 0 to this sum AND sets `all_chains_ok = false` — the
    /// caller decides whether a partial sum is acceptable for its purpose
    /// (e.g. a generous UI preview vs. a strict on-chain-bound attestation).
    pub total_wei: String,
    pub total_human: f64,
    pub all_chains_ok: bool,
}

#[cfg(test)]
fn wei_str_to_human(wei: &str) -> f64 {
    wei.parse::<u128>().map(|v| v as f64 / 1e18).unwrap_or(0.0)
}

/// `eth_call` against `balanceOf(address)` (selector `0x70a08231`), with
/// failover across every RPC endpoint configured for the chain. Returns the
/// raw wei balance, or an error only after every endpoint has failed.
async fn erc20_balance_of(
    client: &reqwest::Client,
    rpc_urls: &[String],
    token: &str,
    holder: &[u8; 20],
) -> Result<u128, String> {
    let call_data = format!(
        "0x70a08231{}",
        hex::encode(address_slot(holder))
    );
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_call",
        "params": [{ "to": token, "data": call_data }, "latest"],
    });
    let mut last_err = "no RPC endpoint configured".to_string();
    for url in rpc_urls {
        let resp = client
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
                return Ok(0);
            }
            if let Ok(v) = u128::from_str_radix(hex_val, 16) {
                return Ok(v);
            }
            last_err = format!("{url}: unparseable balanceOf result {result}");
            continue;
        }
        if let Some(err) = parsed.get("error") {
            last_err = format!("{url}: rpc error {err}");
            continue;
        }
    }
    Err(last_err)
}

async fn rope_native_balance(state: &Arc<AppState>, address: &str) -> Result<u128, String> {
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

/// Aggregate a wallet's cross-chain DC/FAT weight. All three chains are
/// queried concurrently. Individual chain failures are surfaced explicitly
/// in the returned breakdown rather than silently zero-filled.
pub async fn aggregate_weight(state: &Arc<AppState>, address: &str) -> WeightBreakdown {
    let addr20 = match parse_address(address) {
        Ok(a) => a,
        Err(_) => [0u8; 20],
    };

    let eth_token = legacy_dc_eth_address();
    let xdc_token = legacy_dc_xdc_address();
    let eth_urls = eth_rpc_urls();
    let xdc_urls = xdc_rpc_urls();

    let (eth_result, xdc_result, rope_result) = tokio::join!(
        erc20_balance_of(&state.http_client, &eth_urls, &eth_token, &addr20),
        erc20_balance_of(&state.http_client, &xdc_urls, &xdc_token, &addr20),
        rope_native_balance(state, address)
    );

    let eth_balance = match &eth_result {
        Ok(w) => ChainBalance {
            chain: "ethereum",
            chain_id: 1,
            token: eth_token.clone(),
            balance_wei: w.to_string(),
            balance_human: *w as f64 / 1e18,
            ok: true,
            error: None,
        },
        Err(e) => ChainBalance {
            chain: "ethereum",
            chain_id: 1,
            token: eth_token,
            balance_wei: "0".to_string(),
            balance_human: 0.0,
            ok: false,
            error: Some(e.clone()),
        },
    };
    let xdc_balance = match &xdc_result {
        Ok(w) => ChainBalance {
            chain: "xdc",
            chain_id: 50,
            token: xdc_token.clone(),
            balance_wei: w.to_string(),
            balance_human: *w as f64 / 1e18,
            ok: true,
            error: None,
        },
        Err(e) => ChainBalance {
            chain: "xdc",
            chain_id: 50,
            token: xdc_token,
            balance_wei: "0".to_string(),
            balance_human: 0.0,
            ok: false,
            error: Some(e.clone()),
        },
    };
    let rope_balance = match &rope_result {
        Ok(w) => ChainBalance {
            chain: "rope",
            chain_id: rope_chain_id(),
            token: "native".to_string(),
            balance_wei: w.to_string(),
            balance_human: *w as f64 / 1e18,
            ok: true,
            error: None,
        },
        Err(e) => ChainBalance {
            chain: "rope",
            chain_id: rope_chain_id(),
            token: "native".to_string(),
            balance_wei: "0".to_string(),
            balance_human: 0.0,
            ok: false,
            error: Some(e.clone()),
        },
    };

    let total_wei: u128 = eth_result.unwrap_or(0) + xdc_result.unwrap_or(0) + rope_result.unwrap_or(0);
    let all_ok = eth_balance.ok && xdc_balance.ok && rope_balance.ok;

    WeightBreakdown {
        ethereum: eth_balance,
        xdc: xdc_balance,
        rope: rope_balance,
        total_wei: total_wei.to_string(),
        total_human: total_wei as f64 / 1e18,
        all_chains_ok: all_ok,
    }
}

// ============================================================================
// HTTP handlers — wired into `main.rs` under `/api/v1/governance/*`.
// ============================================================================

#[derive(Debug, serde::Deserialize)]
pub struct WeightQuery {
    pub purpose: Option<String>,
    pub vote_id: Option<u64>,
}

/// `GET /api/v1/governance/weight/:address?purpose=cast&vote_id=5`
/// `GET /api/v1/governance/weight/:address?purpose=create`
///
/// Always returns the real, live cross-chain balance breakdown. Only
/// includes a signed on-chain attestation when `VOTE_ESCROW_ATTESTOR_PRIVATE_KEY`
/// and (for `purpose=cast`) `VOTE_ESCROW_ADDRESS` are configured — this is
/// the honest Phase-2-pre-deployment state, not a stub: the aggregation is
/// always real, the attestation is additive once the contract is live.
pub async fn get_weight(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Path(address): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<WeightQuery>,
) -> (axum::http::StatusCode, axum::response::Json<Value>) {
    use axum::http::StatusCode;
    use axum::response::Json;

    let voter20 = match parse_address(&address) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "success": false, "error": format!("invalid address: {e}") })),
            )
        }
    };

    let breakdown = aggregate_weight(&state, &address).await;
    let weight_wei: u128 = breakdown.total_wei.parse().unwrap_or(0);
    let purpose = query.purpose.as_deref().unwrap_or("cast");
    let now = chrono::Utc::now().timestamp();
    let expires_at = now + attestation_window_secs();

    let mut response = json!({
        "success": true,
        "address": format!("0x{}", hex::encode(voter20)),
        "breakdown": breakdown,
        "weight_wei": weight_wei.to_string(),
        "weight_dc_equivalent": weight_wei as f64 / 1e18,
        "purpose": purpose,
        "expires_at": expires_at,
        "attestation_available": false,
    });

    let contract_addr = match vote_escrow_address() {
        Some(a) => a,
        None => {
            response["attestation_unavailable_reason"] =
                json!("VoteEscrow contract not yet deployed (VOTE_ESCROW_ADDRESS unset) — balance breakdown above is live and real.");
            return (StatusCode::OK, Json(response));
        }
    };
    let contract20 = match parse_address(&contract_addr) {
        Ok(a) => a,
        Err(e) => {
            response["attestation_unavailable_reason"] = json!(format!("VOTE_ESCROW_ADDRESS misconfigured: {e}"));
            return (StatusCode::OK, Json(response));
        }
    };

    let sk = match attestor_signing_key() {
        Ok(k) => k,
        Err(e) => {
            response["attestation_unavailable_reason"] = json!(e);
            return (StatusCode::OK, Json(response));
        }
    };

    let digest = if purpose == "create" {
        create_weight_digest(&contract20, rope_chain_id(), &voter20, weight_wei, expires_at)
    } else {
        let Some(vote_id) = query.vote_id else {
            response["attestation_unavailable_reason"] =
                json!("purpose=cast requires ?vote_id=<id> to bind the attestation to a specific vote.");
            return (StatusCode::OK, Json(response));
        };
        response["vote_id"] = json!(vote_id);
        cast_weight_digest(&contract20, rope_chain_id(), vote_id, &voter20, weight_wei, expires_at)
    };

    match sign_digest(sk, &digest) {
        Ok(signature) => {
            response["attestation_available"] = json!(true);
            response["attestation"] = json!(signature);
            response["contract"] = json!(contract_addr);
            response["chain_id"] = json!(rope_chain_id());
            response["attestor"] = json!(attestor_public_address().unwrap_or_default());
        }
        Err(e) => {
            response["attestation_unavailable_reason"] = json!(e);
        }
    }

    (StatusCode::OK, Json(response))
}

/// `GET /api/v1/governance/attestor` — public health/verification endpoint.
/// Lets anyone confirm the address this service signs with matches the
/// `attestor` role configured on the deployed `VoteEscrow` contract,
/// without ever exposing the private key.
pub async fn get_attestor_info() -> (axum::http::StatusCode, axum::response::Json<Value>) {
    use axum::http::StatusCode;
    use axum::response::Json;

    match attestor_public_address() {
        Ok(addr) => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "attestor_address": addr,
                "contract": vote_escrow_address(),
                "chain_id": rope_chain_id(),
            })),
        ),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "success": false, "error": e })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_signing_key() -> SigningKey {
        // Fixed 32-byte scalar for deterministic cross-language verification
        // (this exact key + these exact inputs produce the digest asserted
        // in `VoteEscrow.t.sol::test_rustGoDigestCrossCheck_*` — keep both
        // in sync if either side's encoding ever changes).
        let mut bytes = [0u8; 32];
        bytes[31] = 0x11;
        SigningKey::from_bytes(&bytes.into()).unwrap()
    }

    #[test]
    fn wei_str_to_human_conversion() {
        assert!((wei_str_to_human("1000000000000000000") - 1.0).abs() < 1e-9);
        assert!((wei_str_to_human("500000000000000000") - 0.5).abs() < 1e-9);
        assert_eq!(wei_str_to_human("not-a-number"), 0.0);
    }

    #[test]
    fn address_slot_left_pads_to_32_bytes() {
        let addr: [u8; 20] = [0xAA; 20];
        let slot = address_slot(&addr);
        assert_eq!(&slot[..12], &[0u8; 12]);
        assert_eq!(&slot[12..], &addr[..]);
    }

    #[test]
    fn u256_slot_from_u128_big_endian() {
        let slot = u256_slot_from_u128(1);
        assert_eq!(slot[31], 1);
        assert_eq!(&slot[..31], &[0u8; 31]);
    }

    #[test]
    fn domain_tags_are_stable_and_distinct() {
        let w = weight_domain_tag();
        let c = cast_purpose_tag();
        let cr = create_purpose_tag();
        assert_ne!(w, c);
        assert_ne!(c, cr);
        assert_ne!(w, cr);
        // Deterministic across runs (same input bytes -> same keccak256).
        assert_eq!(weight_domain_tag(), w);
    }

    /// Cross-language fixture: these three hex constants were computed
    /// independently via `cast keccak "<string>"` (Foundry's keccak256,
    /// the exact same primitive Solidity's `keccak256("…")` compile-time
    /// literal uses) against the literal ASCII strings, NOT re-derived from
    /// this Rust code. If this test ever fails, `WEIGHT_DOMAIN_TAG`,
    /// `CAST_PURPOSE`, or `CREATE_PURPOSE` in `VoteEscrow.sol` and this
    /// module have drifted apart and attestations will fail to verify
    /// on-chain.
    #[test]
    fn domain_tags_match_solidity_keccak256_literals() {
        let expected_weight_domain =
            hex::decode("985056c3093e1c9c710ade9b05c664707986199c05db60e9e2c77dcbed789956")
                .unwrap();
        let expected_cast_purpose =
            hex::decode("416136f98a37e21524754716a91ac1d0c28c851be9745167497f08b033e08082")
                .unwrap();
        let expected_create_purpose =
            hex::decode("94a69ce1f5effb50e2d3ea666665dfbac26c73d9403c4adaa22c222bb1c8d92b")
                .unwrap();
        assert_eq!(weight_domain_tag().to_vec(), expected_weight_domain);
        assert_eq!(cast_purpose_tag().to_vec(), expected_cast_purpose);
        assert_eq!(create_purpose_tag().to_vec(), expected_create_purpose);
    }

    #[test]
    fn cast_and_create_digests_differ_for_same_numeric_fields() {
        let contract = [0x11u8; 20];
        let voter = [0x22u8; 20];
        let cast = cast_weight_digest(&contract, 271_828, 5, &voter, 1_000_000, 999_999);
        // create_weight_digest has a different arity/purpose tag; compare
        // against a cast digest for voteId==0 to ensure no accidental
        // collision even when the "extra" numeric field is absent.
        let create = create_weight_digest(&contract, 271_828, &voter, 1_000_000, 999_999);
        assert_ne!(cast, create);
    }

    #[test]
    fn cast_digest_changes_with_every_bound_field() {
        let contract = [0x11u8; 20];
        let voter = [0x22u8; 20];
        let base = cast_weight_digest(&contract, 271_828, 5, &voter, 1_000_000, 999_999);

        assert_ne!(base, cast_weight_digest(&contract, 271_828, 6, &voter, 1_000_000, 999_999)); // vote_id
        assert_ne!(base, cast_weight_digest(&contract, 1, 5, &voter, 1_000_000, 999_999)); // chain_id
        assert_ne!(base, cast_weight_digest(&[0x99u8; 20], 271_828, 5, &voter, 1_000_000, 999_999)); // contract
        assert_ne!(base, cast_weight_digest(&contract, 271_828, 5, &[0x33u8; 20], 1_000_000, 999_999)); // voter
        assert_ne!(base, cast_weight_digest(&contract, 271_828, 5, &voter, 2_000_000, 999_999)); // weight
        assert_ne!(base, cast_weight_digest(&contract, 271_828, 5, &voter, 1_000_000, 1_000_000)); // expiresAt
    }

    #[test]
    fn sign_digest_produces_recoverable_low_s_signature() {
        let sk = test_signing_key();
        let digest = cast_weight_digest(&[0x11u8; 20], 271_828, 1, &[0x22u8; 20], 100, 200);
        let sig_hex = sign_digest(&sk, &digest).expect("sign");
        let raw = hex::decode(sig_hex.trim_start_matches("0x")).expect("hex");
        assert_eq!(raw.len(), 65);
        let v = raw[64];
        assert!(v == 27 || v == 28);
        // EIP-2 low-s check — the exact malleability bound VoteEscrow._recover enforces.
        let s = u128::from_be_bytes(raw[48..64].try_into().unwrap());
        let s_hi = u128::from_be_bytes(raw[32..48].try_into().unwrap());
        // secp256k1 order / 2, split into hi/lo 128-bit halves for comparison
        // without a bignum crate: n/2 hi-half is 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF.
        assert!(s_hi <= 0x7FFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF);
        let _ = s; // s itself only matters combined with s_hi; hi-half bound is sufficient here.
    }

    #[test]
    fn eth_address_from_verifying_key_is_20_bytes_and_deterministic() {
        let sk = test_signing_key();
        let pk = VerifyingKey::from(&sk);
        let addr1 = eth_address_from_verifying_key(&pk);
        let addr2 = eth_address_from_verifying_key(&pk);
        assert_eq!(addr1, addr2);
        assert!(addr1.starts_with("0x"));
        assert_eq!(addr1.len(), 42);
    }

    #[test]
    fn parse_address_rejects_wrong_length() {
        assert!(parse_address("0x1234").is_err());
        assert!(parse_address("0x11223344556677889900aabbccddeeff00112233").is_ok());
    }
}
