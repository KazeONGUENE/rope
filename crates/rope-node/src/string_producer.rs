//! String Production Engine
//!
//! Responsible for producing anchor strings at regular intervals (~4.2 seconds).
//! This is the equivalent of "block production" in traditional blockchains.

use parking_lot::RwLock;
use rope_core::clock::LamportClock;
use rope_core::string::{HybridSignature, PublicKey, RopeString};
use rope_core::types::{MutabilityClass, NodeId, StringId};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

/// String production configuration
#[derive(Clone, Debug)]
pub struct StringProducerConfig {
    /// Target interval between anchor strings (ms)
    pub string_interval_ms: u64,
    /// Minimum testimonies required for anchor
    pub min_testimonies: u32,
    /// Maximum pending strings before forcing anchor
    pub max_pending_strings: usize,
    /// Enable string production
    pub enabled: bool,
    /// This node's role (validator can produce)
    pub is_validator: bool,
}

impl Default for StringProducerConfig {
    fn default() -> Self {
        Self {
            string_interval_ms: 4200,
            min_testimonies: 1,
            max_pending_strings: 1000,
            enabled: true,
            is_validator: true,
        }
    }
}

/// Statistics for string production
#[derive(Clone, Debug, Default)]
pub struct ProductionStats {
    /// Total strings produced
    pub strings_produced: u64,
    /// Total anchor strings produced
    pub anchors_produced: u64,
    /// Current round
    pub current_round: u64,
    /// Average production time (ms)
    pub avg_production_time_ms: f64,
    /// Last production timestamp
    pub last_production: Option<i64>,
}

/// Events emitted by the string producer
#[derive(Clone, Debug)]
pub enum ProductionEvent {
    /// New string created
    StringCreated {
        string_id: StringId,
        round: u64,
        is_anchor: bool,
    },
    /// Anchor string finalized
    AnchorFinalized {
        anchor_id: StringId,
        round: u64,
        strings_included: usize,
    },
    /// Production error
    ProductionError { round: u64, error: String },
}

/// The String Producer - heart of the consensus engine
pub struct StringProducer {
    config: StringProducerConfig,
    node_id: NodeId,
    stats: Arc<RwLock<ProductionStats>>,
    event_tx: broadcast::Sender<ProductionEvent>,
    /// Current round (anchor number)
    current_round: Arc<RwLock<u64>>,
    /// Pending strings waiting to be included in next anchor
    pending_strings: Arc<RwLock<Vec<RopeString>>>,
    /// Last anchor string ID
    last_anchor_id: Arc<RwLock<Option<StringId>>>,
    /// Genesis string ID
    genesis_string_id: Option<StringId>,
    /// Lamport clock for ordering
    clock: Arc<RwLock<LamportClock>>,
}

impl StringProducer {
    /// Create a new string producer
    pub fn new(config: StringProducerConfig, node_id: NodeId) -> Self {
        let (event_tx, _) = broadcast::channel(1000);

        Self {
            config,
            node_id: node_id.clone(),
            stats: Arc::new(RwLock::new(ProductionStats::default())),
            event_tx,
            current_round: Arc::new(RwLock::new(0)),
            pending_strings: Arc::new(RwLock::new(Vec::new())),
            last_anchor_id: Arc::new(RwLock::new(None)),
            genesis_string_id: None,
            clock: Arc::new(RwLock::new(LamportClock::new(node_id))),
        }
    }

    /// Set genesis string ID
    pub fn set_genesis(&mut self, genesis_id: StringId) {
        self.genesis_string_id = Some(genesis_id);
        *self.last_anchor_id.write() = Some(genesis_id);
    }

    /// Get event receiver
    pub fn subscribe(&self) -> broadcast::Receiver<ProductionEvent> {
        self.event_tx.subscribe()
    }

    /// Get current stats
    pub fn stats(&self) -> ProductionStats {
        self.stats.read().clone()
    }

    /// Get current round
    pub fn current_round(&self) -> u64 {
        *self.current_round.read()
    }

    /// Add string to pending pool
    pub fn add_pending_string(&self, string: RopeString) {
        let mut pending = self.pending_strings.write();
        pending.push(string);

        // Check if we should force an anchor
        if pending.len() >= self.config.max_pending_strings {
            debug!("Max pending strings reached, anchor will be forced");
        }
    }

    /// Run the production loop
    pub async fn run(&mut self, mut shutdown_rx: mpsc::Receiver<()>) {
        if !self.config.enabled {
            info!("String production disabled");
            return;
        }

        if !self.config.is_validator {
            info!("Node is not a validator, string production disabled");
            return;
        }

        info!(
            "Starting string production (interval: {}ms, min_testimonies: {})",
            self.config.string_interval_ms, self.config.min_testimonies
        );

        let interval = Duration::from_millis(self.config.string_interval_ms);
        let mut last_production = Instant::now();

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("String producer shutting down");
                    break;
                }
                _ = tokio::time::sleep(interval.saturating_sub(last_production.elapsed())) => {
                    let start = Instant::now();

                    match self.produce_anchor() {
                        Ok(anchor_id) => {
                            let production_time = start.elapsed();
                            self.update_stats(production_time);

                            let round = *self.current_round.read();
                            info!(
                                "🔗 Anchor #{} produced: {} ({:.2}ms)",
                                round,
                                hex::encode(&anchor_id.as_bytes()[..8]),
                                production_time.as_secs_f64() * 1000.0
                            );
                        }
                        Err(e) => {
                            let round = *self.current_round.read();
                            warn!("Failed to produce anchor #{}: {}", round, e);

                            let _ = self.event_tx.send(ProductionEvent::ProductionError {
                                round,
                                error: e.to_string(),
                            });
                        }
                    }

                    last_production = Instant::now();
                }
            }
        }
    }

    /// Produce an anchor string
    fn produce_anchor(&self) -> anyhow::Result<StringId> {
        let current_round = {
            let mut round = self.current_round.write();
            *round += 1;
            *round
        };

        // Get parent (last anchor or genesis)
        let parent_id = self
            .last_anchor_id
            .read()
            .unwrap_or_else(|| self.genesis_string_id.unwrap_or(StringId::ZERO));

        // Collect pending strings
        let pending = {
            let mut pending = self.pending_strings.write();
            std::mem::take(&mut *pending)
        };
        let pending_count = pending.len();

        // Create anchor string
        let anchor = self.create_anchor_string(current_round, parent_id, &pending)?;
        let anchor_id = anchor.id();

        // Update last anchor
        *self.last_anchor_id.write() = Some(anchor_id);

        // Update stats
        {
            let mut stats = self.stats.write();
            stats.anchors_produced += 1;
            stats.strings_produced += 1 + pending_count as u64;
            stats.current_round = current_round;
            stats.last_production = Some(chrono::Utc::now().timestamp());
        }

        // Emit event
        let _ = self.event_tx.send(ProductionEvent::AnchorFinalized {
            anchor_id,
            round: current_round,
            strings_included: pending_count,
        });

        Ok(anchor_id)
    }

    /// Create an anchor string
    fn create_anchor_string(
        &self,
        round: u64,
        parent: StringId,
        pending: &[RopeString],
    ) -> anyhow::Result<RopeString> {
        // Create anchor payload
        let mut payload = Vec::new();

        // Magic bytes for anchor
        payload.extend_from_slice(b"DCRA"); // DataChain Rope Anchor

        // Round number
        payload.extend_from_slice(&round.to_le_bytes());

        // Timestamp
        let timestamp = chrono::Utc::now().timestamp();
        payload.extend_from_slice(&timestamp.to_le_bytes());

        // Number of included strings
        payload.extend_from_slice(&(pending.len() as u32).to_le_bytes());

        // Merkle root of included strings (simplified: just hash all IDs)
        let mut merkle_input = Vec::new();
        for s in pending {
            merkle_input.extend_from_slice(s.id().as_bytes());
        }
        if merkle_input.is_empty() {
            merkle_input = vec![0u8; 32]; // Empty anchor
        }
        let merkle_root = blake3::hash(&merkle_input);
        payload.extend_from_slice(merkle_root.as_bytes());

        // Get next clock tick
        let clock = {
            let mut c = self.clock.write();
            c.increment();
            c.snapshot()
        };

        // Create placeholder public key (in real implementation, this would be the node's key)
        let creator = PublicKey::from_ed25519(*self.node_id.as_bytes());

        // Build the anchor string using the builder pattern
        let anchor = RopeString::builder()
            .content(payload)
            .temporal_marker(clock)
            .add_parent(parent)
            .mutability_class(MutabilityClass::Immutable)
            .replication_factor(5)
            .creator(creator)
            .build()
            .map_err(|e| anyhow::anyhow!("Failed to build anchor: {}", e))?;

        Ok(anchor)
    }

    /// Update production statistics
    fn update_stats(&self, production_time: Duration) {
        let mut stats = self.stats.write();
        let time_ms = production_time.as_secs_f64() * 1000.0;

        // Exponential moving average for production time
        if stats.avg_production_time_ms == 0.0 {
            stats.avg_production_time_ms = time_ms;
        } else {
            stats.avg_production_time_ms = stats.avg_production_time_ms * 0.9 + time_ms * 0.1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_producer_creation() {
        let config = StringProducerConfig::default();
        let node_id = NodeId::new([1u8; 32]);
        let producer = StringProducer::new(config, node_id);

        assert_eq!(producer.current_round(), 0);
    }
}
