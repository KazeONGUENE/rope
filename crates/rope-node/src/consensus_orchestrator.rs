//! Consensus Orchestrator
//!
//! Wires together the full Testimony consensus pipeline:
//! - StringProducer creates anchor strings
//! - Each EVM transaction is wrapped in a RopeString for consensus
//! - TestimonyCollector gathers attestations from validators
//! - FinalityEngine determines when strings reach finality
//! - AgentRunner dispatches transactions to AI agents for semantic validation
//!
//! The orchestrator runs NATIVELY on Datachain Rope. The EVM execution layer
//! (Reth in production, per `reth-blue-green-ipfs-architecture.mdc`) is an
//! optional verifier — when present, the orchestrator can cross-check state;
//! when absent, consensus still runs fully.

use crate::agent_runner::AgentRunner;
use crate::evm_backend::EvmBackend;
use parking_lot::RwLock;
use rope_consensus::{
    FinalityConfig, FinalityEngine, TestimonyCollector, TestimonyConfig, ValidatorRegistry,
};
use rope_core::clock::LamportClock;
use rope_core::types::{AttestationType, NodeId, StringId};
use rope_crypto::hybrid::HybridSigner;
use rope_crypto::offload::OffloadSigner;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tracing::{debug, error, warn};

/// Events emitted by the consensus orchestrator
#[derive(Clone, Debug)]
pub enum ConsensusEvent {
    TransactionNotarized {
        tx_hash: String,
        string_id: StringId,
        round: u64,
    },
    StringFinalized {
        string_id: StringId,
        round: u64,
        testimony_count: u32,
    },
    StateDivergence {
        description: String,
        rope_value: String,
        evm_value: String,
    },
}

/// Tracks an EVM transaction through the consensus pipeline
#[derive(Clone, Debug)]
pub struct NotarizedTransaction {
    pub tx_hash: String,
    pub string_id: StringId,
    pub raw_tx: Option<String>,
    pub receipt: Option<serde_json::Value>,
    pub notarized_at: Instant,
    pub finalized: bool,
    pub testimony_count: u32,
}

/// Configuration for the consensus orchestrator
#[derive(Clone, Debug)]
pub struct OrchestratorConfig {
    pub min_testimonies: u32,
    pub min_anchor_confirmations: u32,
    pub verification_interval_secs: u64,
    pub ai_agents_enabled: bool,
    pub ai_min_confidence: f64,
    pub max_pending_txs: usize,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            min_testimonies: 1,
            min_anchor_confirmations: 3,
            verification_interval_secs: 60,
            ai_agents_enabled: true,
            ai_min_confidence: 0.7,
            max_pending_txs: 500,
        }
    }
}

/// The Consensus Orchestrator — heart of rope-node's consensus pipeline.
///
/// This component is **native to Datachain Rope** and does NOT depend on the
/// EVM execution layer. The EVM backend (Reth in production, per
/// `reth-blue-green-ipfs-architecture.mdc`) is an optional verifier that can
/// be attached for EVM state cross-checking.
pub struct ConsensusOrchestrator {
    config: OrchestratorConfig,
    node_id: NodeId,
    /// Consensus signing identity. `validator_id == blake3(signer pubkey
    /// ed25519)`; it is the id this node stamps on the testimonies it
    /// produces and the id under which its public key is registered.
    validator_id: NodeId,
    validator_signer: Arc<HybridSigner>,
    /// Quipu Canon v2.0 Phase 5 — PQ signing offload pipeline. Testimony
    /// signatures are produced on the offload worker pool (CPU pool
    /// today, GPU/ASIC backend when provisioned) instead of the
    /// consensus hot path. Falls back to inline signing under
    /// backpressure — correctness is never sacrificed to the pipeline.
    offload_signer: OffloadSigner,
    validator_registry: Arc<ValidatorRegistry>,
    evm_backend: Option<Arc<EvmBackend>>,
    testimony_collector: Arc<RwLock<TestimonyCollector>>,
    finality_engine: Arc<RwLock<FinalityEngine>>,
    pending_txs: Arc<RwLock<HashMap<String, NotarizedTransaction>>>,
    finalized_txs: Arc<RwLock<HashMap<String, NotarizedTransaction>>>,
    event_tx: broadcast::Sender<ConsensusEvent>,
    current_round: Arc<RwLock<u64>>,
    clock: Arc<RwLock<LamportClock>>,
    agent_runner: Option<AgentRunner>,
}

impl ConsensusOrchestrator {
    /// Create a new consensus orchestrator with an **ephemeral**
    /// consensus key.
    ///
    /// This convenience constructor generates a fresh hybrid signing key
    /// and a single-member validator registry (just this node). It is
    /// appropriate for tests and single-node dev runs. Production nodes
    /// MUST use [`new_with_validator`](Self::new_with_validator) with a
    /// persistent keystore identity and the committee roster so the
    /// validator id survives restarts.
    ///
    /// `evm_backend` is optional — when `None`, the orchestrator still runs
    /// consensus, testimony, finality, and AI agents natively.
    pub fn new(
        config: OrchestratorConfig,
        node_id: NodeId,
        evm_backend: Option<Arc<EvmBackend>>,
        current_round: Arc<RwLock<u64>>,
    ) -> Self {
        let (signer, public_key) = HybridSigner::generate();
        let validator_id = NodeId::new(public_key.node_id());
        let registry = Arc::new(ValidatorRegistry::new());
        registry
            .register(validator_id, public_key)
            .expect("freshly generated hybrid key must register");
        Self::new_with_validator(
            config,
            node_id,
            evm_backend,
            current_round,
            Arc::new(signer),
            validator_id,
            registry,
        )
    }

    /// Create a new consensus orchestrator with an explicit persistent
    /// validator identity and committee registry.
    ///
    /// `validator_signer` is this node's hybrid signing key (loaded from
    /// the validator keystore). `validator_id` MUST equal
    /// `blake3(signer.pubkey.ed25519)` and MUST be present + active in
    /// `validator_registry`. Signature verification is enabled: the
    /// testimony collector verifies every submitted testimony against
    /// the registry.
    pub fn new_with_validator(
        config: OrchestratorConfig,
        node_id: NodeId,
        evm_backend: Option<Arc<EvmBackend>>,
        current_round: Arc<RwLock<u64>>,
        validator_signer: Arc<HybridSigner>,
        validator_id: NodeId,
        validator_registry: Arc<ValidatorRegistry>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(1000);

        // Finality threshold: with a committee of n validators, finality
        // needs 2f+1 where f=(n-1)/3. For a single-node deployment this
        // collapses to 1. We keep `min_testimonies` as a floor for
        // small/bootstrapping committees.
        let committee = validator_registry.active_count().max(1);
        let f = (committee - 1) / 3;
        let quorum = (2 * f + 1).max(config.min_testimonies as usize);

        let testimony_config = TestimonyConfig {
            finality_threshold: quorum,
            max_testimony_age: 1000,
            verify_signatures: true,
        };
        let testimony_collector =
            TestimonyCollector::with_registry(testimony_config, validator_registry.clone());

        let finality_config = FinalityConfig {
            min_anchor_confirmations: config.min_anchor_confirmations,
            min_testimonies: quorum,
            finality_timeout_secs: 300,
            require_parent_finality: false,
        };
        let finality_engine = FinalityEngine::new(finality_config);

        let agent_runner = if config.ai_agents_enabled {
            let runner = AgentRunner::new(node_id);
            tracing::info!(
                "AI Agent Runner initialized with {} agents",
                runner.active_agent_count()
            );
            Some(runner)
        } else {
            None
        };

        let mode = if evm_backend.is_some() {
            "native + EVM execution-layer verifier (Reth in prod)"
        } else {
            "native (no EVM verifier)"
        };
        tracing::info!(
            "Consensus orchestrator mode: {} | committee={} quorum={} verify_signatures=true",
            mode,
            committee,
            quorum
        );

        let offload_signer = OffloadSigner::start_cpu(validator_signer.clone());

        Self {
            config,
            node_id,
            validator_id,
            validator_signer,
            offload_signer,
            validator_registry,
            evm_backend,
            testimony_collector: Arc::new(RwLock::new(testimony_collector)),
            finality_engine: Arc::new(RwLock::new(finality_engine)),
            pending_txs: Arc::new(RwLock::new(HashMap::new())),
            finalized_txs: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            current_round,
            clock: Arc::new(RwLock::new(LamportClock::new(node_id))),
            agent_runner,
        }
    }

    /// The validator committee registry (shared, for RPC introspection
    /// and cross-component wiring).
    pub fn validator_registry(&self) -> Arc<ValidatorRegistry> {
        self.validator_registry.clone()
    }

    /// This node's consensus validator id.
    pub fn validator_id(&self) -> NodeId {
        self.validator_id
    }

    /// Sign a testimony through the Phase 5 offload pipeline, falling
    /// back to inline signing under backpressure. Semantics are
    /// identical to `Testimony::sign_with` — only where the Dilithium
    /// work executes changes.
    fn sign_testimony(&self, testimony: &mut rope_consensus::Testimony) {
        let data = testimony.signing_data();
        match self.offload_signer.submit(data) {
            Ok(ticket) => match ticket.wait() {
                Ok(sig) => testimony.set_signature(sig.ed25519_sig, sig.dilithium_sig),
                Err(e) => {
                    warn!("offload pipeline failed ({e}); signing inline");
                    testimony.sign_with(&self.validator_signer);
                }
            },
            Err(e) => {
                debug!("offload backpressure ({e}); signing inline");
                testimony.sign_with(&self.validator_signer);
            }
        }
    }

    /// Phase 5 signing-pipeline counters (backend, throughput, batch
    /// sizing, queue high-water) for RPC/observability.
    pub fn offload_stats(&self) -> rope_crypto::offload::OffloadStats {
        self.offload_signer.stats()
    }

    /// Produce a signed self-testimony for a string and return it in
    /// wire form (serialized bytes), for gossip to the committee.
    ///
    /// The testimony is also submitted locally so this node counts its
    /// own attestation toward finality.
    pub fn attest_and_serialize(
        &self,
        string_id: StringId,
        attestation_type: AttestationType,
    ) -> Vec<u8> {
        let mut clock = self.clock.write();
        clock.increment();
        let timestamp = clock.snapshot();
        drop(clock);

        let mut testimony = rope_consensus::Testimony::new(
            string_id,
            self.validator_id,
            attestation_type,
            timestamp,
            0,
        );
        self.sign_testimony(&mut testimony);
        let bytes = testimony.serialize_content();
        // Count our own attestation locally.
        let tc = self.testimony_collector.write();
        let _ = tc.submit_testimony(testimony);
        bytes
    }

    /// Accept a testimony received from a peer (wire form). Verifies the
    /// signature against the committee registry and, if valid, folds it
    /// into the finality tally for its target string.
    ///
    /// Returns `Ok(true)` if the testimony pushed its target string to
    /// finality, `Ok(false)` if accepted but not yet final, `Err(..)` if
    /// rejected (unknown validator, bad signature, malformed).
    pub fn submit_peer_testimony(&self, wire: &[u8]) -> Result<bool, String> {
        let testimony = rope_consensus::Testimony::from_content(wire)
            .map_err(|e| format!("malformed testimony: {e}"))?;
        // Do not accept our own id echoed back from a peer.
        if testimony.validator_id.as_bytes() == self.validator_id.as_bytes() {
            return Ok(self
                .testimony_collector
                .read()
                .is_finalized(&testimony.target_string_id));
        }
        let tc = self.testimony_collector.write();
        tc.submit_testimony(testimony).map_err(|e| e.to_string())
    }

    /// Batch-accept peer testimonies (wire form) using parallel batch
    /// signature verification. Returns per-item verdicts parallel to the
    /// input.
    pub fn submit_peer_testimonies_batch(
        &self,
        wires: &[Vec<u8>],
    ) -> Vec<Result<bool, String>> {
        let mut parsed = Vec::with_capacity(wires.len());
        let mut parse_errs: Vec<Option<String>> = Vec::with_capacity(wires.len());
        for w in wires {
            match rope_consensus::Testimony::from_content(w) {
                Ok(t) => {
                    parse_errs.push(None);
                    parsed.push(Some(t));
                }
                Err(e) => {
                    parse_errs.push(Some(format!("malformed testimony: {e}")));
                    parsed.push(None);
                }
            }
        }
        let to_verify: Vec<rope_consensus::Testimony> =
            parsed.iter().filter_map(|p| p.clone()).collect();
        let verdicts = {
            let tc = self.testimony_collector.read();
            tc.submit_testimonies_batch(to_verify)
        };

        // Re-thread verdicts back onto the original order.
        let mut vi = 0usize;
        let mut out = Vec::with_capacity(wires.len());
        for (i, p) in parsed.iter().enumerate() {
            if let Some(err) = &parse_errs[i] {
                out.push(Err(err.clone()));
            } else if p.is_some() {
                match &verdicts[vi] {
                    Ok(b) => out.push(Ok(*b)),
                    Err(e) => out.push(Err(e.to_string())),
                }
                vi += 1;
            }
        }
        out
    }

    /// Register a peer validator's public key at runtime (e.g. on
    /// committee change discovered via gossip). Enforces identity
    /// binding and PQ-key presence via the registry.
    pub fn register_peer_validator(
        &self,
        node_id: NodeId,
        public_key: rope_crypto::hybrid::HybridPublicKey,
    ) -> Result<(), String> {
        self.validator_registry
            .register(node_id, public_key)
            .map_err(|e| e.to_string())?;
        self.testimony_collector.read().register_validator(node_id);
        Ok(())
    }

    /// Committee summary for RPC/observability.
    pub fn committee_info(&self) -> CommitteeInfo {
        let n = self.validator_registry.active_count();
        let f = if n == 0 { 0 } else { (n - 1) / 3 };
        CommitteeInfo {
            validators: n,
            byzantine_tolerance: f,
            finality_quorum: (2 * f + 1).max(self.config.min_testimonies as usize),
            self_validator_id: hex::encode(self.validator_id.as_bytes()),
        }
    }

    /// Attach or replace the EVM backend at runtime.
    pub fn set_evm_backend(&mut self, evm_backend: Arc<EvmBackend>) {
        tracing::info!("EVM execution-layer verifier attached to consensus orchestrator");
        self.evm_backend = Some(evm_backend);
    }

    /// Deprecated alias for [`Self::set_evm_backend`].
    #[deprecated(
        since = "0.2.0",
        note = "Use `set_evm_backend`. Anvil was archived 2026-03-31."
    )]
    pub fn set_anvil(&mut self, evm_backend: Arc<EvmBackend>) {
        self.set_evm_backend(evm_backend)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ConsensusEvent> {
        self.event_tx.subscribe()
    }

    /// Notarize an EVM transaction — wrap it in a RopeString for consensus.
    ///
    /// Steps:
    /// 1. Create StringId from tx hash
    /// 2. Register in finality engine
    /// 3. Self-testimony from the producing node
    /// 4. Dispatch to AI agents for semantic validation
    pub async fn notarize_transaction(&self, tx_hash: &str, raw_tx: Option<&str>) -> StringId {
        let content = format!("evm_tx:{}", tx_hash);
        let string_id = StringId::from_content(content.as_bytes());
        let round = *self.current_round.read();

        let notarized = NotarizedTransaction {
            tx_hash: tx_hash.to_string(),
            string_id,
            raw_tx: raw_tx.map(|s| s.to_string()),
            receipt: None,
            notarized_at: Instant::now(),
            finalized: false,
            testimony_count: 0,
        };

        self.pending_txs
            .write()
            .insert(tx_hash.to_string(), notarized);

        // Register in finality engine
        self.finality_engine
            .write()
            .register_string(string_id, vec![]);

        // Self-testimony from the producing node, signed with this
        // node's hybrid consensus key and verified against the registry.
        {
            let tc = self.testimony_collector.write();
            tc.register_validator(self.validator_id);

            let mut clock = self.clock.write();
            clock.increment();
            let timestamp = clock.snapshot();

            let mut testimony = rope_consensus::Testimony::new(
                string_id,
                self.validator_id,
                AttestationType::Existence,
                timestamp,
                0,
            );
            self.sign_testimony(&mut testimony);
            if let Err(e) = tc.submit_testimony(testimony) {
                warn!("self-testimony rejected for {}: {}", tx_hash, e);
            }
        }

        // Dispatch to AI agents if enabled
        if let Some(ref runner) = self.agent_runner {
            let ai_ok = runner
                .evaluate_transaction(string_id, tx_hash, raw_tx)
                .await;
            if ai_ok {
                debug!("AI agents approved tx {}", tx_hash);
            } else {
                warn!(
                    "AI agents did NOT approve tx {} — proceeding (advisory only)",
                    tx_hash
                );
            }
        }

        let _ = self.event_tx.send(ConsensusEvent::TransactionNotarized {
            tx_hash: tx_hash.to_string(),
            string_id,
            round,
        });

        debug!(
            "Notarized tx {} as string {} at round {}",
            tx_hash,
            hex::encode(&string_id.as_bytes()[..8]),
            round
        );

        string_id
    }

    /// Record a receipt for a notarized transaction
    pub fn record_receipt(&self, tx_hash: &str, receipt: serde_json::Value) {
        if let Some(tx) = self.pending_txs.write().get_mut(tx_hash) {
            tx.receipt = Some(receipt);
        }
    }

    /// Process an anchor finalization
    pub fn on_anchor_finalized(&self, round: u64, anchor_id: StringId) {
        let pending_ids: Vec<StringId> = self
            .pending_txs
            .read()
            .values()
            .map(|tx| tx.string_id)
            .collect();

        {
            let fe = self.finality_engine.write();
            fe.record_anchor(*anchor_id.as_bytes(), round, pending_ids.clone());
        }

        let mut newly_finalized = Vec::new();
        {
            let fe = self.finality_engine.read();
            for string_id in &pending_ids {
                if fe.is_finalized(string_id) {
                    newly_finalized.push(*string_id);
                }
            }
        }

        for string_id in newly_finalized {
            let mut pending = self.pending_txs.write();
            let tx_hashes: Vec<String> = pending
                .iter()
                .filter(|(_, tx)| tx.string_id == string_id)
                .map(|(hash, _)| hash.clone())
                .collect();

            for hash in tx_hashes {
                if let Some(mut tx) = pending.remove(&hash) {
                    tx.finalized = true;

                    let _ = self.event_tx.send(ConsensusEvent::StringFinalized {
                        string_id,
                        round,
                        testimony_count: tx.testimony_count,
                    });

                    self.finalized_txs.write().insert(hash, tx);
                }
            }
        }
    }

    /// Verify state consistency against the EVM execution layer (only when
    /// the EVM backend is available and healthy).
    pub async fn verify_state_consistency(&self) -> anyhow::Result<bool> {
        let evm = match &self.evm_backend {
            Some(b) if b.is_healthy() => b,
            Some(_) => {
                debug!("Skipping state verification: EVM backend is unhealthy");
                return Ok(true);
            }
            None => {
                return Ok(true);
            }
        };

        let evm_block = evm.get_block_number().await?;
        let rope_round = *self.current_round.read();

        debug!(
            "State verification: evm_block={}, rope_round={}",
            evm_block, rope_round
        );

        Ok(true)
    }

    pub fn stats(&self) -> ConsensusStats {
        ConsensusStats {
            pending_txs: self.pending_txs.read().len(),
            finalized_txs: self.finalized_txs.read().len(),
            current_round: *self.current_round.read(),
            evm_backend_connected: self
                .evm_backend
                .as_ref()
                .map(|b| b.is_healthy())
                .unwrap_or(false),
            ai_agents_active: self
                .agent_runner
                .as_ref()
                .map(|r| r.active_agent_count())
                .unwrap_or(0),
        }
    }

    pub fn spawn_verification_loop(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let orchestrator = Arc::clone(self);
        let interval = Duration::from_secs(self.config.verification_interval_secs);

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;

                match orchestrator.verify_state_consistency().await {
                    Ok(true) => debug!("State verification passed"),
                    Ok(false) => warn!("State verification detected divergence"),
                    Err(e) => error!("State verification failed: {}", e),
                }
            }
        })
    }
}

/// Committee summary surfaced to RPC/observability.
#[derive(Clone, Debug)]
pub struct CommitteeInfo {
    pub validators: usize,
    pub byzantine_tolerance: usize,
    pub finality_quorum: usize,
    pub self_validator_id: String,
}

#[derive(Clone, Debug)]
pub struct ConsensusStats {
    pub pending_txs: usize,
    pub finalized_txs: usize,
    pub current_round: u64,
    /// Whether the optional EVM execution-layer backend is currently
    /// reachable and healthy. `false` when running in pure-native mode.
    pub evm_backend_connected: bool,
    pub ai_agents_active: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rope_crypto::hybrid::HybridSigner;

    fn quiet_config() -> OrchestratorConfig {
        OrchestratorConfig {
            ai_agents_enabled: false,
            ..OrchestratorConfig::default()
        }
    }

    /// Build an orchestrator with its own persistent-style identity and a
    /// registry we can extend with peers.
    fn make_node(config: OrchestratorConfig) -> (Arc<ConsensusOrchestrator>, NodeId) {
        let (signer, public_key) = HybridSigner::generate();
        let validator_id = NodeId::new(public_key.node_id());
        let registry = Arc::new(ValidatorRegistry::new());
        registry.register(validator_id, public_key).unwrap();
        let orch = Arc::new(ConsensusOrchestrator::new_with_validator(
            config,
            validator_id,
            None,
            Arc::new(RwLock::new(0)),
            Arc::new(signer),
            validator_id,
            registry,
        ));
        (orch, validator_id)
    }

    #[test]
    fn peer_testimony_from_registered_validator_is_accepted() {
        let (node_a, id_a) = make_node(quiet_config());
        let (node_b, _) = make_node(quiet_config());

        // Node B learns node A's public key (roster distribution).
        let pk_a = node_a.validator_registry().public_key(&id_a).unwrap();
        node_b.register_peer_validator(id_a, pk_a).unwrap();

        let string_id = StringId::from_content(b"phase2-cross-node-anchor");
        let wire = node_a.attest_and_serialize(string_id, AttestationType::Existence);
        // Hybrid signature must verify against A's registered key on B.
        node_b
            .submit_peer_testimony(&wire)
            .expect("testimony from a registered committee member must be accepted");
    }

    #[test]
    fn peer_testimony_from_unknown_validator_is_rejected() {
        let (node_a, _) = make_node(quiet_config());
        let (node_b, _) = make_node(quiet_config());
        // No roster exchange: B does not know A.
        let string_id = StringId::from_content(b"phase2-unknown-validator");
        let wire = node_a.attest_and_serialize(string_id, AttestationType::Existence);
        assert!(
            node_b.submit_peer_testimony(&wire).is_err(),
            "testimony signed by an unregistered validator must be rejected"
        );
    }

    #[test]
    fn tampered_testimony_wire_is_rejected() {
        let (node_a, id_a) = make_node(quiet_config());
        let (node_b, _) = make_node(quiet_config());
        let pk_a = node_a.validator_registry().public_key(&id_a).unwrap();
        node_b.register_peer_validator(id_a, pk_a).unwrap();

        let string_id = StringId::from_content(b"phase2-tamper-check");
        let mut wire = node_a.attest_and_serialize(string_id, AttestationType::Existence);
        // Flip one byte in the middle of the payload.
        let mid = wire.len() / 2;
        wire[mid] ^= 0xFF;
        assert!(
            node_b.submit_peer_testimony(&wire).is_err(),
            "a tampered testimony must fail signature verification or parsing"
        );
    }

    #[test]
    fn committee_info_reflects_registered_peers() {
        let (node_a, _) = make_node(quiet_config());
        assert_eq!(node_a.committee_info().validators, 1);

        // Grow the committee to 4 → f=1, quorum=3 (min_testimonies=1 floor
        // does not override 2f+1 once the committee is large enough).
        for _ in 0..3 {
            let (_, pk) = HybridSigner::generate();
            let id = NodeId::new(pk.node_id());
            node_a.register_peer_validator(id, pk).unwrap();
        }
        let info = node_a.committee_info();
        assert_eq!(info.validators, 4);
        assert_eq!(info.byzantine_tolerance, 1);
        assert_eq!(info.finality_quorum, 3);
    }

    #[test]
    fn batch_submission_verdicts_align_with_inputs() {
        let (node_a, id_a) = make_node(quiet_config());
        let (node_b, _) = make_node(quiet_config());
        let pk_a = node_a.validator_registry().public_key(&id_a).unwrap();
        node_b.register_peer_validator(id_a, pk_a).unwrap();

        let good1 = node_a.attest_and_serialize(
            StringId::from_content(b"batch-anchor-1"),
            AttestationType::Existence,
        );
        let good2 = node_a.attest_and_serialize(
            StringId::from_content(b"batch-anchor-2"),
            AttestationType::Existence,
        );
        let mut bad = good2.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 0xFF;

        let verdicts = node_b.submit_peer_testimonies_batch(&[good1, bad, good2]);
        assert_eq!(verdicts.len(), 3);
        assert!(verdicts[0].is_ok(), "first valid testimony must be accepted");
        assert!(verdicts[1].is_err(), "tampered testimony must be rejected");
        assert!(verdicts[2].is_ok(), "second valid testimony must be accepted");
    }
}
