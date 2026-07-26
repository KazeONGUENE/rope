//! The attester service — runs on **every** node in the committee
//! (proposer included). This is the half of the quorum protocol that
//! makes it real rather than theatrical: each machine independently
//! re-executes the proposed block against its own local Reth state via
//! `engine_newPayloadV2` (real state-transition validation, not a
//! rubber stamp) before it will ever sign an attestation for it, and
//! independently re-verifies the collected signature set against the
//! committee roster before it will finalize anything locally.
//!
//! A single dishonest or broken proposer cannot force a bad block through
//! this path: attesters that disagree simply don't sign, the round fails
//! to reach quorum, and no one — including the proposer — advances their
//! local head. No quorum, no block. That is the intended, correct
//! behaviour for a BFT-style gate, not a bug.

use anyhow::Result;
use axum::{extract::State, routing::post, Json, Router};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::committee::Committee;
use crate::engine_client::EngineClient;
use crate::identity::{self, NodeIdentity};
use crate::payload::{build_payload_from_block, summarize};
use crate::quorum_proto::{AttestRequest, AttestResponse, CommitRequest, CommitResponse};

pub struct AttesterState {
    pub engine: EngineClient,
    pub identity: NodeIdentity,
    pub committee: Committee,
    /// Read-only handle onto a trusted node (the proposer, by convention)
    /// this attester can pull historical blocks from when it discovers
    /// it is missing some — e.g. after this process (or its host) was
    /// restarted and the committee kept producing blocks without it.
    /// Engine API import is a strict parent-must-already-exist protocol
    /// (unlike P2P sync, nothing here auto-backfills), so without this a
    /// restarted node would be stuck returning SYNCING forever, unable to
    /// ever attest or commit again. `None` disables catch-up (e.g. in
    /// tests where every round is expected to be delivered in order).
    catch_up_source: Option<EngineClient>,
    /// round -> block_hash this node has already attested to. Refusing to
    /// sign a second, different hash for a round already attested is the
    /// basic BFT safety rule ("no double voting"); it is what stops a
    /// proposer that observed a failed round from quietly trying a second,
    /// different payload under the same round number.
    attested_rounds: Mutex<HashMap<u64, String>>,
}

pub fn build_router(state: Arc<AttesterState>) -> Router {
    Router::new()
        .route("/attest", post(handle_attest))
        .route("/commit", post(handle_commit))
        .route("/healthz", axum::routing::get(handle_healthz))
        .with_state(state)
}

/// Convenience constructor with catch-up disabled — used by tests that
/// exercise a single, always-in-order round sequence.
#[allow(dead_code)]
pub fn new_state(engine: EngineClient, identity: NodeIdentity, committee: Committee) -> AttesterState {
    new_state_with_catch_up(engine, identity, committee, None)
}

pub fn new_state_with_catch_up(
    engine: EngineClient,
    identity: NodeIdentity,
    committee: Committee,
    catch_up_source: Option<EngineClient>,
) -> AttesterState {
    AttesterState {
        engine,
        identity,
        committee,
        catch_up_source,
        attested_rounds: Mutex::new(HashMap::new()),
    }
}

/// Backfills this node's local Reth from `catch_up_source` up to (and
/// including) `target_number`, one block at a time via the same
/// newPayloadV2 + forkchoiceUpdatedV2 sequence the quorum protocol itself
/// uses — so a recovering node re-validates every block it catches up on
/// exactly as strictly as a freshly-proposed one, never trusting the
/// catch-up source blindly.
async fn catch_up_to(state: &AttesterState, target_number: u64) -> Result<(), String> {
    let source = state
        .catch_up_source
        .as_ref()
        .ok_or_else(|| "no catch-up source configured".to_string())?;

    let mut local_head = state
        .engine
        .block_number()
        .await
        .map_err(|e| format!("reading local head during catch-up: {e}"))?;

    if local_head >= target_number {
        return Ok(());
    }

    info!(
        "catch-up starting: local_head={local_head} target={target_number} ({} blocks behind)",
        target_number - local_head
    );

    while local_head < target_number {
        let n = local_head + 1;
        let block = source
            .get_block_by_number(n, false)
            .await
            .map_err(|e| format!("fetching block {n} from catch-up source: {e}"))?
            .ok_or_else(|| format!("catch-up source has no block {n}"))?;

        let payload = build_payload_from_block(source, &block)
            .await
            .map_err(|e| format!("building payload for catch-up block {n}: {e}"))?;
        let summary = summarize(&payload).map_err(|e| format!("summarizing catch-up block {n}: {e}"))?;

        let status = state
            .engine
            .new_payload_v2(&payload)
            .await
            .map_err(|e| format!("newPayloadV2 for catch-up block {n}: {e}"))?;
        if status != "VALID" {
            return Err(format!("catch-up block {n} rejected locally: status={status}"));
        }

        state
            .engine
            .forkchoice_updated_v2(&summary.hash, &summary.hash, &summary.hash, None)
            .await
            .map_err(|e| format!("forkchoiceUpdatedV2 for catch-up block {n}: {e}"))?;

        local_head = n;
    }

    info!("catch-up complete: now at block {local_head}");
    Ok(())
}

async fn handle_healthz() -> &'static str {
    "ok"
}

async fn handle_attest(
    State(state): State<Arc<AttesterState>>,
    Json(req): Json<AttestRequest>,
) -> Json<AttestResponse> {
    let pubkey_hex = state.identity.pubkey_hex();

    let summary = match summarize(&req.payload) {
        Ok(s) => s,
        Err(e) => {
            warn!("attest round {}: malformed payload: {e:#}", req.round);
            return Json(AttestResponse {
                pubkey_hex,
                status: "INVALID".to_string(),
                block_number: 0,
                block_hash: String::new(),
                signature_hex: None,
                reason: Some(format!("malformed payload: {e}")),
            });
        }
    };

    {
        let attested = state.attested_rounds.lock().await;
        if let Some(prior_hash) = attested.get(&req.round) {
            if prior_hash != &summary.hash {
                warn!(
                    "attest round {} REJECTED: already attested {prior_hash} for this round, refusing to double-vote for {}",
                    req.round, summary.hash
                );
                return Json(AttestResponse {
                    pubkey_hex,
                    status: "INVALID".to_string(),
                    block_number: summary.number,
                    block_hash: summary.hash,
                    signature_hex: None,
                    reason: Some("round already attested with a different block hash".to_string()),
                });
            }
        }
    }

    let status = match state.engine.new_payload_v2(&req.payload).await {
        Ok(s) => s,
        Err(e) => {
            warn!("attest round {} newPayloadV2 transport error: {e:#}", req.round);
            return Json(AttestResponse {
                pubkey_hex,
                status: "INVALID".to_string(),
                block_number: summary.number,
                block_hash: summary.hash,
                signature_hex: None,
                reason: Some(format!("local engine_newPayloadV2 failed: {e}")),
            });
        }
    };

    // SYNCING means our local chain doesn't yet have this payload's parent
    // (typically: this node fell behind while its attester/host was down).
    // Attempt to backfill from the catch-up source and retry exactly once
    // before giving up — this is what lets a restarted node rejoin the
    // committee on its own instead of being stuck forever.
    let status = if status == "SYNCING" && state.catch_up_source.is_some() {
        match catch_up_to(&state, summary.number.saturating_sub(1)).await {
            Ok(()) => {
                info!(
                    "attest round {}: caught up, retrying newPayloadV2 for block {}",
                    req.round, summary.number
                );
                match state.engine.new_payload_v2(&req.payload).await {
                    Ok(s) => s,
                    Err(e) => {
                        warn!("attest round {} retry newPayloadV2 transport error: {e:#}", req.round);
                        return Json(AttestResponse {
                            pubkey_hex,
                            status: "INVALID".to_string(),
                            block_number: summary.number,
                            block_hash: summary.hash,
                            signature_hex: None,
                            reason: Some(format!("post-catch-up engine_newPayloadV2 failed: {e}")),
                        });
                    }
                }
            }
            Err(reason) => {
                warn!("attest round {}: catch-up failed: {reason}", req.round);
                return Json(AttestResponse {
                    pubkey_hex,
                    status: "INVALID".to_string(),
                    block_number: summary.number,
                    block_hash: summary.hash,
                    signature_hex: None,
                    reason: Some(format!("catch-up failed: {reason}")),
                });
            }
        }
    } else {
        status
    };

    if status != "VALID" {
        warn!(
            "attest round {} block {}: local Reth rejected payload, status={status}",
            req.round, summary.number
        );
        return Json(AttestResponse {
            pubkey_hex,
            status,
            block_number: summary.number,
            block_hash: summary.hash,
            signature_hex: None,
            reason: Some("local independent execution did not return VALID".to_string()),
        });
    }

    let msg = identity::attest_message(req.round, summary.number, &summary.hash);
    let sig = state.identity.sign(&msg);

    {
        let mut attested = state.attested_rounds.lock().await;
        attested.insert(req.round, summary.hash.clone());
    }

    info!(
        "attested round {} block {} hash={} — independently validated VALID",
        req.round, summary.number, summary.hash
    );

    Json(AttestResponse {
        pubkey_hex,
        status: "VALID".to_string(),
        block_number: summary.number,
        block_hash: summary.hash,
        signature_hex: Some(hex::encode(sig)),
        reason: None,
    })
}

async fn handle_commit(
    State(state): State<Arc<AttesterState>>,
    Json(req): Json<CommitRequest>,
) -> Json<CommitResponse> {
    let summary = match summarize(&req.payload) {
        Ok(s) => s,
        Err(e) => {
            return Json(CommitResponse {
                ok: false,
                block_number: 0,
                finalized_hash: None,
                reason: Some(format!("malformed payload: {e}")),
            })
        }
    };

    let msg = identity::attest_message(req.round, summary.number, &summary.hash);

    let mut valid_signers = std::collections::HashSet::new();
    for entry in &req.certificate {
        if !state.committee.contains_pubkey(&entry.pubkey_hex) {
            warn!(
                "commit round {}: certificate entry from non-committee pubkey {}, ignoring",
                req.round, entry.pubkey_hex
            );
            continue;
        }
        match identity::verify(&entry.pubkey_hex, &msg, &entry.signature_hex) {
            Ok(true) => {
                valid_signers.insert(entry.pubkey_hex.clone());
            }
            Ok(false) => {
                warn!(
                    "commit round {}: signature from {} does NOT verify, ignoring",
                    req.round, entry.pubkey_hex
                );
            }
            Err(e) => {
                warn!(
                    "commit round {}: bad signature encoding from {}: {e:#}",
                    req.round, entry.pubkey_hex
                );
            }
        }
    }

    let threshold = state.committee.quorum_threshold();
    if valid_signers.len() < threshold {
        warn!(
            "commit round {} REJECTED: only {}/{} valid signatures, need {}",
            req.round,
            valid_signers.len(),
            state.committee.len(),
            threshold
        );
        return Json(CommitResponse {
            ok: false,
            block_number: summary.number,
            finalized_hash: None,
            reason: Some(format!(
                "quorum not met: {}/{} valid signatures, need {}",
                valid_signers.len(),
                state.committee.len(),
                threshold
            )),
        });
    }

    // Belt-and-suspenders: re-validate locally before finalizing, in case
    // this node never saw the /attest round (e.g. it was briefly
    // unreachable) — we never finalize a payload we have not ourselves
    // executed and accepted, quorum certificate or not. If we're behind,
    // try to catch up first (same as the /attest path) before concluding
    // we genuinely disagree with the rest of the committee.
    let recheck = state.engine.new_payload_v2(&req.payload).await;
    let recheck = match recheck {
        Ok(status) if status == "SYNCING" && state.catch_up_source.is_some() => {
            match catch_up_to(&state, summary.number.saturating_sub(1)).await {
                Ok(()) => state.engine.new_payload_v2(&req.payload).await,
                Err(reason) => {
                    warn!("commit round {}: catch-up failed: {reason}", req.round);
                    return Json(CommitResponse {
                        ok: false,
                        block_number: summary.number,
                        finalized_hash: None,
                        reason: Some(format!("catch-up failed: {reason}")),
                    });
                }
            }
        }
        other => other,
    };

    match recheck {
        Ok(status) if status == "VALID" => {}
        Ok(status) => {
            warn!(
                "commit round {}: quorum was reached by others but THIS node's local execution says {status}, refusing to finalize",
                req.round
            );
            return Json(CommitResponse {
                ok: false,
                block_number: summary.number,
                finalized_hash: None,
                reason: Some(format!("local execution disagrees with quorum: status={status}")),
            });
        }
        Err(e) => {
            return Json(CommitResponse {
                ok: false,
                block_number: summary.number,
                finalized_hash: None,
                reason: Some(format!("local newPayloadV2 failed during commit: {e}")),
            });
        }
    }

    match state
        .engine
        .forkchoice_updated_v2(&summary.hash, &summary.hash, &summary.hash, None)
        .await
    {
        Ok((fc_status, _)) => {
            info!(
                "committed round {} block {} hash={} — {}/{} quorum, fc={:?}",
                req.round,
                summary.number,
                summary.hash,
                valid_signers.len(),
                state.committee.len(),
                fc_status.get("status")
            );
            Json(CommitResponse {
                ok: true,
                block_number: summary.number,
                finalized_hash: Some(summary.hash),
                reason: None,
            })
        }
        Err(e) => Json(CommitResponse {
            ok: false,
            block_number: summary.number,
            finalized_hash: None,
            reason: Some(format!("forkchoiceUpdatedV2 failed: {e}")),
        }),
    }
}

pub async fn serve(state: Arc<AttesterState>, bind_addr: &str) -> Result<()> {
    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    info!("attester listening on {bind_addr}");
    axum::serve(listener, router).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::committee::CommitteeMember;
    use crate::identity::NodeIdentity;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use ed25519_dalek::SigningKey;
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    fn dummy_committee(members: Vec<(&str, &str)>) -> Committee {
        Committee {
            members: members
                .into_iter()
                .map(|(name, pk)| CommitteeMember {
                    name: name.to_string(),
                    pubkey_hex: pk.to_string(),
                    attester_url: "http://unused".to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn test_committee_threshold_used_by_commit_handler() {
        let c = dummy_committee(vec![("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")]);
        assert_eq!(c.quorum_threshold(), 3);
    }

    fn identity_from_seed(seed: u8) -> NodeIdentity {
        let signing_key = SigningKey::from_bytes(&[seed; 32]);
        let verifying_key = signing_key.verifying_key();
        NodeIdentity {
            signing_key,
            verifying_key,
        }
    }

    /// A mock EVM execution layer standing in for Reth's Engine API +
    /// plain RPC, so the attester's real HTTP handlers can be exercised
    /// end-to-end without touching a live node. `accept` controls whether
    /// `engine_newPayloadV2` reports the block as VALID (an honest node
    /// agreeing) or INVALID (an honest node independently disagreeing —
    /// this is the case that must make the whole round fail safely).
    async fn spawn_mock_evm(accept: bool) -> String {
        async fn handler(
            axum::extract::State(accept): axum::extract::State<bool>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let result = match method {
                "engine_newPayloadV2" => {
                    json!({"status": if accept { "VALID" } else { "INVALID" }})
                }
                "engine_forkchoiceUpdatedV2" => {
                    json!({"payloadStatus": {"status": "VALID"}, "payloadId": null})
                }
                "eth_blockNumber" => json!("0x1"),
                _ => json!(null),
            };
            Json(json!({"jsonrpc": "2.0", "id": 1, "result": result}))
        }

        let router = Router::new().route("/", post(handler)).with_state(accept);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}/")
    }

    fn sample_payload(number: u64, hash: &str) -> serde_json::Value {
        json!({
            "parentHash": "0xparent",
            "feeRecipient": "0x0000000000000000000000000000000000000000",
            "stateRoot": "0x1",
            "receiptsRoot": "0x2",
            "blockNumber": format!("0x{:x}", number),
            "gasLimit": "0x1c9c380",
            "gasUsed": "0x0",
            "timestamp": "0x1",
            "blockHash": hash,
            "transactions": [],
            "withdrawals": [],
        })
    }

    async fn call_router(router: Router, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, json)
    }

    #[tokio::test]
    async fn test_attest_then_commit_reaches_quorum_and_finalizes() {
        let mock_url = spawn_mock_evm(true).await;
        let jwt_hex = hex::encode([1u8; 32]);

        // Real production shape: 4-node committee (BLUE/GREEN/DO-rpc-1/
        // DO-rpc-2) -> f=1 -> quorum_threshold=3. Node A is the server
        // under test; B, C, D are simulated by signing directly with
        // their own identities — the same effect as three more real
        // machines independently calling /attest.
        let identity_a = identity_from_seed(21);
        let identity_b = identity_from_seed(22);
        let identity_c = identity_from_seed(23);
        let identity_d = identity_from_seed(24);

        let committee = dummy_committee(vec![
            ("A", &identity_a.pubkey_hex()),
            ("B", &identity_b.pubkey_hex()),
            ("C", &identity_c.pubkey_hex()),
            ("D", &identity_d.pubkey_hex()),
        ]);
        assert_eq!(committee.quorum_threshold(), 3);

        let engine = EngineClient::new(mock_url.clone(), mock_url.clone(), &jwt_hex).unwrap();
        let state = Arc::new(new_state(engine, identity_a, committee));
        let router = build_router(state);

        let payload = sample_payload(42, "0xblockhash42");

        let (status, resp) = call_router(
            router.clone(),
            "/attest",
            json!({"round": 1, "payload": payload}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resp["status"], "VALID");
        let sig_a = resp["signature_hex"].as_str().unwrap().to_string();
        let pk_a = resp["pubkey_hex"].as_str().unwrap().to_string();

        // B and C sign independently over the identical canonical message
        // (D deliberately does not — 3-of-4 must still be enough).
        let msg = crate::identity::attest_message(1, 42, "0xblockhash42");
        let sig_b = hex::encode(identity_b.sign(&msg));
        let sig_c = hex::encode(identity_c.sign(&msg));

        let certificate = vec![
            json!({"pubkey_hex": pk_a, "signature_hex": sig_a}),
            json!({"pubkey_hex": identity_b.pubkey_hex(), "signature_hex": sig_b}),
            json!({"pubkey_hex": identity_c.pubkey_hex(), "signature_hex": sig_c}),
        ];

        let (status, resp) = call_router(
            router,
            "/commit",
            json!({"round": 1, "payload": payload, "certificate": certificate}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["finalized_hash"], "0xblockhash42");
    }

    #[tokio::test]
    async fn test_commit_rejected_below_quorum_threshold() {
        let mock_url = spawn_mock_evm(true).await;
        let jwt_hex = hex::encode([2u8; 32]);

        let identity_a = identity_from_seed(31);
        let identity_b = identity_from_seed(32);
        let identity_c = identity_from_seed(33);

        // n=3 -> f=0 -> threshold = 1... use 4 to force threshold=3 so a
        // single signature is clearly insufficient.
        let identity_d = identity_from_seed(34);
        let committee = dummy_committee(vec![
            ("A", &identity_a.pubkey_hex()),
            ("B", &identity_b.pubkey_hex()),
            ("C", &identity_c.pubkey_hex()),
            ("D", &identity_d.pubkey_hex()),
        ]);
        assert_eq!(committee.quorum_threshold(), 3);

        let payload = sample_payload(7, "0xonlyone");
        let msg = crate::identity::attest_message(9, 7, "0xonlyone");
        let sig_a = hex::encode(identity_a.sign(&msg));
        let pk_a = identity_a.pubkey_hex();

        let engine = EngineClient::new(mock_url.clone(), mock_url.clone(), &jwt_hex).unwrap();
        let state = Arc::new(new_state(engine, identity_a, committee));
        let router = build_router(state);

        let certificate = vec![json!({"pubkey_hex": pk_a, "signature_hex": sig_a})];

        let (status, resp) = call_router(
            router,
            "/commit",
            json!({"round": 9, "payload": payload, "certificate": certificate}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resp["ok"], false);
        assert!(resp["reason"].as_str().unwrap().contains("quorum not met"));
    }

    #[tokio::test]
    async fn test_attest_refuses_when_local_execution_disagrees() {
        // The mock EVM here represents an honest node whose OWN Reth
        // independently rejects the proposed payload — this must never
        // produce a signature, no matter what the proposer claims.
        let mock_url = spawn_mock_evm(false).await;
        let jwt_hex = hex::encode([3u8; 32]);
        let identity_a = identity_from_seed(41);
        let committee = dummy_committee(vec![("A", &identity_a.pubkey_hex())]);

        let engine = EngineClient::new(mock_url.clone(), mock_url.clone(), &jwt_hex).unwrap();
        let state = Arc::new(new_state(engine, identity_a, committee));
        let router = build_router(state);

        let payload = sample_payload(1, "0xbad");
        let (status, resp) = call_router(
            router,
            "/attest",
            json!({"round": 1, "payload": payload}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resp["status"], "INVALID");
        assert!(resp["signature_hex"].is_null());
    }

    /// A richer mock EVM that behaves like a *real* Reth instance with
    /// respect to chain continuity: it tracks its own head number, and
    /// `engine_newPayloadV2` returns SYNCING for any block whose number
    /// is more than one ahead of its current head (i.e. the parent is
    /// missing) — exactly the behaviour that made node4 in the live
    /// sandbox test get stuck until catch-up was added. `forkchoiceUpdatedV2`
    /// advances the tracked head so a sequence of accepted payloads moves
    /// this mock forward just like a real chain.
    async fn spawn_stateful_mock_evm(start_head: u64) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};

        #[derive(Clone)]
        struct Shared(std::sync::Arc<AtomicU64>);

        async fn handler(
            axum::extract::State(head): axum::extract::State<Shared>,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
            let result = match method {
                "eth_blockNumber" => {
                    json!(format!("0x{:x}", head.0.load(Ordering::SeqCst)))
                }
                "engine_newPayloadV2" => {
                    let params = body.get("params").and_then(|p| p.as_array());
                    let payload = params.and_then(|p| p.first());
                    let block_number = payload
                        .and_then(|p| p.get("blockNumber"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                        .unwrap_or(0);
                    let current = head.0.load(Ordering::SeqCst);
                    if block_number <= current + 1 {
                        json!({"status": "VALID"})
                    } else {
                        json!({"status": "SYNCING"})
                    }
                }
                "engine_forkchoiceUpdatedV2" => {
                    // In this mock, advancing forkchoice always means "the
                    // just-validated block became head" — good enough to
                    // exercise the attester's catch-up loop, which calls
                    // newPayloadV2 then forkchoiceUpdatedV2 per block in
                    // strictly increasing order.
                    head.0.fetch_add(1, Ordering::SeqCst);
                    json!({"payloadStatus": {"status": "VALID"}, "payloadId": null})
                }
                "eth_getBlockByNumber" => {
                    let params = body.get("params").and_then(|p| p.as_array());
                    let n_hex = params
                        .and_then(|p| p.first())
                        .and_then(|v| v.as_str())
                        .unwrap_or("0x0");
                    let n = u64::from_str_radix(n_hex.trim_start_matches("0x"), 16).unwrap_or(0);
                    json!({
                        "number": format!("0x{:x}", n),
                        "hash": format!("0xcatchup{n}"),
                        "parentHash": format!("0xcatchup{}", n.saturating_sub(1)),
                        "stateRoot": "0x1",
                        "receiptsRoot": "0x2",
                        "gasLimit": "0x1c9c380",
                        "gasUsed": "0x0",
                        "timestamp": "0x1",
                        "transactions": [],
                        "withdrawals": [],
                    })
                }
                _ => json!(null),
            };
            Json(json!({"jsonrpc": "2.0", "id": 1, "result": result}))
        }

        let shared = Shared(std::sync::Arc::new(AtomicU64::new(start_head)));
        let router = Router::new().route("/", post(handler)).with_state(shared);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        format!("http://{addr}/")
    }

    #[tokio::test]
    async fn test_attest_catches_up_when_behind_then_succeeds() {
        // This node's local Reth is stuck at block 2 (e.g. it just
        // restarted), but the round being attested is for block 5 — a
        // 3-block gap. Without catch-up this must return INVALID/SYNCING
        // forever; with catch-up configured it should backfill 3,4 and
        // then accept block 5.
        let local_mock = spawn_stateful_mock_evm(2).await;
        let catch_up_mock = spawn_stateful_mock_evm(10).await; // "ahead" trusted source
        let jwt_hex = hex::encode([5u8; 32]);
        let identity_a = identity_from_seed(61);
        let committee = dummy_committee(vec![("A", &identity_a.pubkey_hex())]);

        let engine = EngineClient::new(local_mock.clone(), local_mock.clone(), &jwt_hex).unwrap();
        let catch_up_source = EngineClient::new_readonly(catch_up_mock).unwrap();
        let state = Arc::new(new_state_with_catch_up(
            engine,
            identity_a,
            committee,
            Some(catch_up_source),
        ));
        let router = build_router(state);

        let payload = sample_payload(5, "0xblock5");
        let (status, resp) = call_router(router, "/attest", json!({"round": 1, "payload": payload})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resp["status"], "VALID", "expected catch-up to succeed: {resp}");
        assert!(resp["signature_hex"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_attest_without_catch_up_source_stays_syncing() {
        // Same gap as above, but no catch_up_source configured — must
        // fail cleanly (never silently accept, never panic) rather than
        // pretend to succeed.
        let local_mock = spawn_stateful_mock_evm(2).await;
        let jwt_hex = hex::encode([6u8; 32]);
        let identity_a = identity_from_seed(62);
        let committee = dummy_committee(vec![("A", &identity_a.pubkey_hex())]);

        let engine = EngineClient::new(local_mock.clone(), local_mock.clone(), &jwt_hex).unwrap();
        let state = Arc::new(new_state(engine, identity_a, committee));
        let router = build_router(state);

        let payload = sample_payload(5, "0xblock5");
        let (status, resp) = call_router(router, "/attest", json!({"round": 1, "payload": payload})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(resp["status"], "SYNCING");
        assert!(resp["signature_hex"].is_null());
    }

    #[tokio::test]
    async fn test_attest_refuses_double_vote_on_same_round() {
        let mock_url = spawn_mock_evm(true).await;
        let jwt_hex = hex::encode([4u8; 32]);
        let identity_a = identity_from_seed(51);
        let committee = dummy_committee(vec![("A", &identity_a.pubkey_hex())]);

        let engine = EngineClient::new(mock_url.clone(), mock_url.clone(), &jwt_hex).unwrap();
        let state = Arc::new(new_state(engine, identity_a, committee));
        let router = build_router(state);

        let payload_1 = sample_payload(5, "0xfirst");
        let (_, resp1) = call_router(
            router.clone(),
            "/attest",
            json!({"round": 3, "payload": payload_1}),
        )
        .await;
        assert_eq!(resp1["status"], "VALID");

        // Same round, DIFFERENT hash — must be refused even though the
        // mock EVM would happily accept it too.
        let payload_2 = sample_payload(5, "0xsecond-different-hash");
        let (_, resp2) = call_router(
            router,
            "/attest",
            json!({"round": 3, "payload": payload_2}),
        )
        .await;
        assert_eq!(resp2["status"], "INVALID");
        assert!(resp2["reason"]
            .as_str()
            .unwrap()
            .contains("already attested"));
    }
}
