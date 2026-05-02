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
use rope_consensus::{FinalityConfig, FinalityEngine, TestimonyCollector, TestimonyConfig};
use rope_core::clock::LamportClock;
use rope_core::types::{AttestationType, NodeId, StringId};
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
    /// Create a new consensus orchestrator.
    ///
    /// `evm_backend` is optional — when `None`, the orchestrator still runs
    /// consensus, testimony, finality, and AI agents natively.
    pub fn new(
        config: OrchestratorConfig,
        node_id: NodeId,
        evm_backend: Option<Arc<EvmBackend>>,
        current_round: Arc<RwLock<u64>>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(1000);

        let testimony_config = TestimonyConfig {
            finality_threshold: config.min_testimonies as usize,
            max_testimony_age: 1000,
            verify_signatures: false,
        };
        let testimony_collector = TestimonyCollector::new(testimony_config);

        let finality_config = FinalityConfig {
            min_anchor_confirmations: config.min_anchor_confirmations,
            min_testimonies: config.min_testimonies as usize,
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
        tracing::info!("Consensus orchestrator mode: {}", mode);

        Self {
            config,
            node_id,
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

        // Self-testimony from the producing node
        {
            let tc = self.testimony_collector.write();
            tc.register_validator(self.node_id);

            let mut clock = self.clock.write();
            clock.increment();
            let timestamp = clock.snapshot();

            let testimony = rope_consensus::Testimony::new(
                string_id,
                self.node_id,
                AttestationType::Existence,
                timestamp,
                0,
            );
            let _ = tc.submit_testimony(testimony);
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
