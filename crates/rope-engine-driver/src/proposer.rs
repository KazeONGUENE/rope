//! The proposer — runs on exactly one node (BLUE, by committee
//! convention documented in the deployed roster) and drives one quorum
//! round per tick. It never advances its own chain unilaterally: the
//! proposed payload is only ever finalized — anywhere, including on the
//! proposer's own node — once real signatures from at least
//! `quorum_threshold()` distinct, independent committee machines have
//! each locally re-executed the block and accepted it.
//!
//! Rotation: fixed proposer only for now (documented, not hidden). A
//! future round-robin/leader-election extension can reuse this exact
//! propose/attest/commit shape — it only needs to change who is allowed
//! to call `run_one_round`, not the protocol.

use anyhow::{Context, Result};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tracing::{error, info, warn};

use crate::committee::Committee;
use crate::engine_client::EngineClient;
use crate::payload::summarize;
use crate::quorum_proto::{AttestRequest, AttestResponse, CertificateEntry, CommitRequest, CommitResponse};

pub struct ProposerConfig {
    pub tick_interval: Duration,
    pub fee_recipient: String,
    pub attest_timeout: Duration,
    pub commit_timeout: Duration,
}

static ROUND_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Seeds the round counter so restarts don't reuse round numbers already
/// attested by long-lived attester processes (which would trip the
/// no-double-vote guard for no good reason). Callers should seed this
/// with `now_unix_millis() << 8` or similar, monotonic-enough scheme; we
/// keep it simple: seed with current unix seconds, which is monotonic
/// across restarts as long as the clock doesn't go backwards.
pub fn seed_round_counter(seed: u64) {
    ROUND_COUNTER.store(seed, Ordering::SeqCst);
}

fn next_round() -> u64 {
    ROUND_COUNTER.fetch_add(1, Ordering::SeqCst)
}

pub async fn run(local: &EngineClient, http: &reqwest::Client, committee: &Committee, cfg: ProposerConfig) -> Result<()> {
    info!(
        "starting PROPOSER: {} committee members, quorum threshold {}",
        committee.len(),
        committee.quorum_threshold()
    );
    let mut ticker = tokio::time::interval(cfg.tick_interval);
    loop {
        ticker.tick().await;
        if let Err(e) = run_one_round(local, http, committee, &cfg).await {
            error!("quorum round failed (no block produced this tick): {e:#}");
        }
    }
}

async fn run_one_round(
    local: &EngineClient,
    http: &reqwest::Client,
    committee: &Committee,
    cfg: &ProposerConfig,
) -> Result<()> {
    let round = next_round();

    // --- Build phase: ask our own Reth to build a candidate from its txpool ---
    let head_num = local.block_number().await.context("block_number")?;
    let head_block = local
        .get_block_by_number(head_num, false)
        .await
        .context("get current head")?
        .ok_or_else(|| anyhow::anyhow!("head block {head_num} vanished"))?;
    let head_summary = summarize(&head_block)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let head_ts = crate::engine_client::parse_hex_u64(&head_block["timestamp"]).unwrap_or(0);
    let next_ts = std::cmp::max(now, head_ts + 1);

    let attrs = json!({
        "timestamp": format!("0x{:x}", next_ts),
        "prevRandao": format!("0x{}", "00".repeat(32)),
        "suggestedFeeRecipient": cfg.fee_recipient,
        "withdrawals": [],
    });

    let (fc_status, payload_id) = local
        .forkchoice_updated_v2(&head_summary.hash, &head_summary.hash, &head_summary.hash, Some(attrs))
        .await
        .context("forkchoiceUpdatedV2 (build request)")?;
    let payload_id = payload_id.ok_or_else(|| {
        anyhow::anyhow!("round {round}: no payloadId returned, fc status {:?}", fc_status)
    })?;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let built = local.get_payload_v2(&payload_id).await.context("getPayloadV2")?;
    let payload = built.get("executionPayload").cloned().unwrap_or(built);
    let candidate = summarize(&payload)?;

    info!(
        "round {round}: proposing block {} hash={} txs={}",
        candidate.number, candidate.hash, candidate.tx_count
    );

    // --- Attest phase: fan out to every committee member, including self ---
    let attest_req = AttestRequest {
        round,
        payload: payload.clone(),
    };

    let mut futures = Vec::with_capacity(committee.len());
    for member in &committee.members {
        let url = format!("{}/attest", member.attester_url.trim_end_matches('/'));
        let http = http.clone();
        let req = attest_req.clone();
        let name = member.name.clone();
        let expected_pubkey = member.pubkey_hex.clone();
        let timeout = cfg.attest_timeout;
        futures.push(tokio::spawn(async move {
            let result = tokio::time::timeout(timeout, http.post(&url).json(&req).send()).await;
            let resp = match result {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    warn!("round {round}: attest call to {name} transport error: {e}");
                    return None;
                }
                Err(_) => {
                    warn!("round {round}: attest call to {name} timed out after {timeout:?}");
                    return None;
                }
            };
            match resp.json::<AttestResponse>().await {
                Ok(a) if a.status == "VALID" && a.pubkey_hex == expected_pubkey && a.signature_hex.is_some() => {
                    Some(a)
                }
                Ok(a) => {
                    warn!(
                        "round {round}: {name} did not attest (status={}, pubkey_match={})",
                        a.status,
                        a.pubkey_hex == expected_pubkey
                    );
                    None
                }
                Err(e) => {
                    warn!("round {round}: {name} attest response unparseable: {e}");
                    None
                }
            }
        }));
    }

    let mut certificate = Vec::new();
    for f in futures {
        if let Ok(Some(attestation)) = f.await {
            certificate.push(CertificateEntry {
                pubkey_hex: attestation.pubkey_hex,
                signature_hex: attestation.signature_hex.unwrap(),
            });
        }
    }

    let threshold = committee.quorum_threshold();
    if certificate.len() < threshold {
        warn!(
            "round {round} FAILED: only {}/{} attestations, need {threshold} — no block produced this tick",
            certificate.len(),
            committee.len()
        );
        return Ok(());
    }

    info!(
        "round {round}: quorum reached ({}/{}, threshold {threshold}) — committing",
        certificate.len(),
        committee.len()
    );

    // --- Commit phase: broadcast the certificate to every member ---
    let commit_req = CommitRequest {
        round,
        payload: payload.clone(),
        certificate,
    };

    let mut commit_futures = Vec::with_capacity(committee.len());
    for member in &committee.members {
        let url = format!("{}/commit", member.attester_url.trim_end_matches('/'));
        let http = http.clone();
        let req = commit_req.clone();
        let name = member.name.clone();
        let timeout = cfg.commit_timeout;
        commit_futures.push(tokio::spawn(async move {
            let result = tokio::time::timeout(timeout, http.post(&url).json(&req).send()).await;
            match result {
                Ok(Ok(r)) => match r.json::<CommitResponse>().await {
                    Ok(c) if c.ok => Some(name),
                    Ok(c) => {
                        warn!("round {round}: {name} refused commit: {:?}", c.reason);
                        None
                    }
                    Err(e) => {
                        warn!("round {round}: {name} commit response unparseable: {e}");
                        None
                    }
                },
                Ok(Err(e)) => {
                    warn!("round {round}: commit call to {name} transport error: {e}");
                    None
                }
                Err(_) => {
                    warn!("round {round}: commit call to {name} timed out after {timeout:?}");
                    None
                }
            }
        }));
    }

    let mut committed = Vec::new();
    for f in commit_futures {
        if let Ok(Some(name)) = f.await {
            committed.push(name);
        }
    }

    info!(
        "round {round}: block {} hash={} committed on {}/{} nodes: {:?}",
        candidate.number,
        candidate.hash,
        committed.len(),
        committee.len(),
        committed
    );

    Ok(())
}
