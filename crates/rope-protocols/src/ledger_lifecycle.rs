//! # Ledger Lifecycle Orchestrator
//!
//! Coordinates the complete personal ledger lifecycle across all subsystems:
//! creation, encryption, slicing, distribution, repatriation, and deletion.
//!
//! ## Lifecycle
//!
//! ```text
//! CREATE ──► ENCRYPT ──► SLICE ──► DISTRIBUTE ──► [DORMANCY]
//!                                                      │
//!    ┌─────────────────────────────────────────────────┘
//!    │                                                 │
//!    ▼                                                 ▼
//! REPATRIATE ◄── ASSEMBLE ◄── FETCH ◄── DISCOVER    DELETE
//!    │                                                 │
//!    ▼                                                 ▼
//! DECRYPT (wallet-side)                     OES Key Destroyed
//! ```
//!
//! This module provides the `LedgerLifecycleManager` which ties together:
//! - `rope-core::personal_ledger::LedgerRegistry`
//! - `rope-crypto::ledger_encryption::*`
//! - `rope-protocols::erasure::ErasureCoordinator`
//! - `rope-network::rdp::RopeDistributionProtocol` (via trait)
//! - `rope-network::repatriation::RepatriationEngine` (via trait)

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// Events emitted by the lifecycle manager for observability and networking
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LedgerLifecycleEvent {
    LedgerCreated {
        wallet_address: Vec<u8>,
        genesis_string_id: [u8; 32],
        oes_generation: u64,
    },
    EntryAppended {
        wallet_address: Vec<u8>,
        string_id: [u8; 32],
        parent_id: [u8; 32],
        encrypted_size: u64,
        piece_count: u32,
    },
    RepatriationStarted {
        wallet_address: Vec<u8>,
        known_head: Option<[u8; 32]>,
    },
    RepatriationComplete {
        wallet_address: Vec<u8>,
        entries_fetched: usize,
        total_bytes: u64,
        elapsed_ms: u64,
    },
    LedgerDeletionRequested {
        wallet_address: Vec<u8>,
        reason: DeletionReason,
    },
    LedgerDeleted {
        wallet_address: Vec<u8>,
        entries_erased: usize,
        key_destruction_method: String,
    },
}

/// Reason for ledger deletion
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DeletionReason {
    OwnerRequest,
    GdprArticle17,
    AccountClosure,
    LegalOrder { reference: String },
}

impl DeletionReason {
    pub fn as_str(&self) -> &str {
        match self {
            Self::OwnerRequest => "owner_request",
            Self::GdprArticle17 => "gdpr_article_17",
            Self::AccountClosure => "account_closure",
            Self::LegalOrder { .. } => "legal_order",
        }
    }
}

/// Configuration for the lifecycle manager
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LifecycleConfig {
    pub default_replication_factor: u32,
    pub piece_size: usize,
    pub max_entry_size: usize,
    pub enable_auto_distribution: bool,
    pub erasure_broadcast_timeout_secs: u64,
    pub erasure_confirmation_threshold: f32,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            default_replication_factor: 5,
            piece_size: 256 * 1024,
            max_entry_size: 10 * 1024 * 1024,
            enable_auto_distribution: true,
            erasure_broadcast_timeout_secs: 300,
            erasure_confirmation_threshold: 2.0 / 3.0,
        }
    }
}

/// Result of slicing encrypted content into RDP pieces
#[derive(Clone, Debug)]
pub struct SlicingResult {
    pub pieces: Vec<LedgerPiece>,
    pub total_size: u64,
    pub piece_hashes: Vec<[u8; 32]>,
}

/// A single piece of encrypted ledger content
#[derive(Clone, Debug)]
pub struct LedgerPiece {
    pub index: u32,
    pub data: Vec<u8>,
    pub hash: [u8; 32],
    pub size: u32,
}

/// Slice encrypted content into RDP-compatible pieces
pub fn slice_encrypted_content(content: &[u8], piece_size: usize) -> SlicingResult {
    let mut pieces = Vec::new();
    let mut hashes = Vec::new();

    for (i, chunk) in content.chunks(piece_size).enumerate() {
        let hash = *blake3::hash(chunk).as_bytes();
        pieces.push(LedgerPiece {
            index: i as u32,
            data: chunk.to_vec(),
            hash,
            size: chunk.len() as u32,
        });
        hashes.push(hash);
    }

    SlicingResult {
        total_size: content.len() as u64,
        pieces,
        piece_hashes: hashes,
    }
}

/// Reassemble pieces back into the original encrypted content
pub fn reassemble_pieces(pieces: &mut [(u32, Vec<u8>)]) -> Vec<u8> {
    pieces.sort_by_key(|(idx, _)| *idx);
    let mut content = Vec::new();
    for (_, data) in pieces {
        content.extend_from_slice(data);
    }
    content
}

/// Verify a piece against its expected hash
pub fn verify_piece(data: &[u8], expected_hash: &[u8; 32]) -> bool {
    let hash = blake3::hash(data);
    hash.as_bytes() == expected_hash
}

/// Erasure audit record for a ledger deletion
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerErasureAudit {
    pub wallet_address: Vec<u8>,
    pub reason: DeletionReason,
    pub requested_at: i64,
    pub completed_at: Option<i64>,
    pub entries_erased: usize,
    pub key_destruction_method: String,
    pub oes_generations_destroyed: Vec<u64>,
    pub confirming_nodes: Vec<[u8; 32]>,
    pub audit_hash: [u8; 32],
}

impl LedgerErasureAudit {
    pub fn new(wallet_address: Vec<u8>, reason: DeletionReason) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            wallet_address,
            reason,
            requested_at: now,
            completed_at: None,
            entries_erased: 0,
            key_destruction_method: String::new(),
            oes_generations_destroyed: Vec::new(),
            confirming_nodes: Vec::new(),
            audit_hash: [0u8; 32],
        }
    }

    pub fn complete(&mut self, entries: usize, method: &str, generations: Vec<u64>) {
        self.completed_at = Some(chrono::Utc::now().timestamp());
        self.entries_erased = entries;
        self.key_destruction_method = method.to_string();
        self.oes_generations_destroyed = generations;
        self.audit_hash = self.compute_audit_hash();
    }

    fn compute_audit_hash(&self) -> [u8; 32] {
        let mut content = Vec::new();
        content.extend_from_slice(&self.wallet_address);
        content.extend_from_slice(self.reason.as_str().as_bytes());
        content.extend_from_slice(&self.requested_at.to_le_bytes());
        if let Some(completed) = self.completed_at {
            content.extend_from_slice(&completed.to_le_bytes());
        }
        content.extend_from_slice(&(self.entries_erased as u64).to_le_bytes());
        content.extend_from_slice(self.key_destruction_method.as_bytes());
        *blake3::hash(&content).as_bytes()
    }
}

/// Aggregate statistics — snapshot type returned by [`LedgerLifecycleManager::stats`].
///
/// The manager stores its live counters as per-shard atomics
/// (see [`LIFECYCLE_SHARDS`]). `stats()` aggregates them into this
/// `Clone + Serialize` snapshot for the RPC and observability surfaces.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LifecycleStats {
    pub ledgers_created: u64,
    pub entries_appended: u64,
    pub repatriations_completed: u64,
    pub repatriations_failed: u64,
    pub ledgers_deleted: u64,
    pub total_pieces_distributed: u64,
    pub total_bytes_distributed: u64,
    pub total_bytes_repatriated: u64,
}

/// Number of [`LedgerLifecycleManager`] shards.
///
/// 256 mirrors the v2.0 Phase 1 shard count used everywhere else in the
/// hot path (`StringLattice` shards, `EntityHeadLocks` shards,
/// `ClockManager` HLC shards, `OESKeyCache` shards). The first byte of
/// the wallet address selects the shard; ed25519 / EVM addresses are
/// uniformly distributed in their first byte so this gives a flat
/// per-shard load without rehashing.
pub const LIFECYCLE_SHARDS: usize = 256;

/// Per-shard atomic counters. Each `record_*` increments only the
/// counters on its shard, so cross-wallet appends never collide on a
/// shared cache line. `stats()` sums them up under `Relaxed` ordering —
/// observability is allowed to see a slightly inconsistent snapshot
/// (a counter from one shard a few nanos newer than another), which is
/// the standard contract for lock-free aggregate metrics.
#[derive(Default)]
struct ShardStats {
    ledgers_created: AtomicU64,
    entries_appended: AtomicU64,
    repatriations_completed: AtomicU64,
    repatriations_failed: AtomicU64,
    ledgers_deleted: AtomicU64,
    total_pieces_distributed: AtomicU64,
    total_bytes_distributed: AtomicU64,
    total_bytes_repatriated: AtomicU64,
}

/// One [`LIFECYCLE_SHARDS`] partition: its own event-log Vec under a
/// dedicated `RwLock`, plus the shard-local atomic stats counters.
struct LifecycleShard {
    events: RwLock<Vec<LedgerLifecycleEvent>>,
    stats: ShardStats,
}

impl LifecycleShard {
    fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
            stats: ShardStats::default(),
        }
    }
}

/// Pick the shard owning a wallet address. Falls back to shard 0 when
/// the address is empty (which only happens in synthetic tests).
#[inline]
fn shard_for_wallet(wallet_address: &[u8]) -> usize {
    wallet_address.first().copied().unwrap_or(0) as usize
}

/// The Ledger Lifecycle Manager — top-level orchestrator.
///
/// This struct is intended to be held as `Arc<LedgerLifecycleManager>` inside
/// `RopeNode` and shared across the RPC server, consensus orchestrator, and
/// network event handlers.
///
/// ## Quipu Canon v2.0 Phase 2.A — sharded record path
///
/// In v1.x and Phase 1 the manager held two global `RwLock`s — one over
/// an unbounded `Vec<LedgerLifecycleEvent>`, one over a `LifecycleStats`
/// struct — and `record_append` took both write locks on every call.
/// Under load (>200 ops/thread or >500 wallets) those two locks
/// serialised every concurrent appender across the whole node and the
/// `manager-write` benchmark in `crates/rope-loadgen` collapsed from
/// ~30k ops/s to <50 ops/s. See
/// `docs/QUIPU_CANON_V2_PHASE1_BENCHMARK_RESULTS.md` for the cliff
/// reproduction.
///
/// Phase 2.A partitions the manager across [`LIFECYCLE_SHARDS`] shards,
/// keyed by `wallet_address[0]`. Each shard owns its own event-log
/// `RwLock<Vec<…>>` and its own `ShardStats` of `AtomicU64` counters,
/// so `record_creation` / `record_append` / `record_repatriation` /
/// `record_deletion` for two distinct wallets almost always land on
/// two distinct shards and proceed in parallel.
///
/// `erasure_audits` stays global because deletions are rare (one per
/// wallet closure) and consumers want a single canonical list.
///
/// The public method semantics — append-only event log, monotonic
/// counters, snapshot reads via `stats()` / `recent_events()` /
/// `erasure_audits()` — are preserved. `recent_events(limit)` now
/// returns up to `limit` events with no global cross-shard ordering
/// guarantee; intra-shard order is still strict push order.
pub struct LedgerLifecycleManager {
    config: LifecycleConfig,
    /// 256 sharded `(events, stats)` partitions keyed by `wallet[0]`.
    shards: Box<[LifecycleShard]>,
    /// Erasure audits stay global: deletion is a rare, audited event,
    /// and tooling expects a single ordered list.
    erasure_audits: RwLock<Vec<LedgerErasureAudit>>,
}

impl LedgerLifecycleManager {
    pub fn new(config: LifecycleConfig) -> Self {
        let shards: Vec<LifecycleShard> = (0..LIFECYCLE_SHARDS)
            .map(|_| LifecycleShard::new())
            .collect();
        Self {
            config,
            shards: shards.into_boxed_slice(),
            erasure_audits: RwLock::new(Vec::new()),
        }
    }

    pub fn config(&self) -> &LifecycleConfig {
        &self.config
    }

    /// Number of shards (always [`LIFECYCLE_SHARDS`]). Exposed for
    /// tests and node metrics so observability surfaces can verify
    /// the manager has been built with the v2.0 Phase 2.A topology.
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    /// Record a ledger creation event
    pub fn record_creation(
        &self,
        wallet_address: Vec<u8>,
        genesis_string_id: [u8; 32],
        oes_generation: u64,
    ) {
        let shard = &self.shards[shard_for_wallet(&wallet_address)];
        let event = LedgerLifecycleEvent::LedgerCreated {
            wallet_address,
            genesis_string_id,
            oes_generation,
        };
        shard.events.write().push(event);
        shard.stats.ledgers_created.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an entry append event
    pub fn record_append(
        &self,
        wallet_address: Vec<u8>,
        string_id: [u8; 32],
        parent_id: [u8; 32],
        encrypted_size: u64,
        piece_count: u32,
    ) {
        let shard = &self.shards[shard_for_wallet(&wallet_address)];
        let event = LedgerLifecycleEvent::EntryAppended {
            wallet_address,
            string_id,
            parent_id,
            encrypted_size,
            piece_count,
        };
        shard.events.write().push(event);

        // Stats are atomic — no lock; appends to the same shard but
        // different wallets do not contend on a writer guard.
        shard.stats.entries_appended.fetch_add(1, Ordering::Relaxed);
        shard
            .stats
            .total_pieces_distributed
            .fetch_add(piece_count as u64, Ordering::Relaxed);
        shard
            .stats
            .total_bytes_distributed
            .fetch_add(encrypted_size, Ordering::Relaxed);
    }

    /// Record a completed repatriation
    pub fn record_repatriation(
        &self,
        wallet_address: Vec<u8>,
        entries_fetched: usize,
        total_bytes: u64,
        elapsed_ms: u64,
    ) {
        let shard = &self.shards[shard_for_wallet(&wallet_address)];
        let event = LedgerLifecycleEvent::RepatriationComplete {
            wallet_address,
            entries_fetched,
            total_bytes,
            elapsed_ms,
        };
        shard.events.write().push(event);

        shard
            .stats
            .repatriations_completed
            .fetch_add(1, Ordering::Relaxed);
        shard
            .stats
            .total_bytes_repatriated
            .fetch_add(total_bytes, Ordering::Relaxed);
    }

    /// Record a ledger deletion
    pub fn record_deletion(&self, audit: LedgerErasureAudit) {
        let shard = &self.shards[shard_for_wallet(&audit.wallet_address)];
        let wallet = audit.wallet_address.clone();
        let entries = audit.entries_erased;
        let method = audit.key_destruction_method.clone();

        let event = LedgerLifecycleEvent::LedgerDeleted {
            wallet_address: wallet,
            entries_erased: entries,
            key_destruction_method: method,
        };
        shard.events.write().push(event);
        shard.stats.ledgers_deleted.fetch_add(1, Ordering::Relaxed);

        // Audit list stays global — deletion is rare, the audit list
        // is consumed as a single canonical sequence by tooling.
        self.erasure_audits.write().push(audit);
    }

    /// Aggregate per-shard atomics into a stable snapshot.
    pub fn stats(&self) -> LifecycleStats {
        let mut out = LifecycleStats::default();
        for shard in self.shards.iter() {
            out.ledgers_created += shard.stats.ledgers_created.load(Ordering::Relaxed);
            out.entries_appended += shard.stats.entries_appended.load(Ordering::Relaxed);
            out.repatriations_completed +=
                shard.stats.repatriations_completed.load(Ordering::Relaxed);
            out.repatriations_failed += shard.stats.repatriations_failed.load(Ordering::Relaxed);
            out.ledgers_deleted += shard.stats.ledgers_deleted.load(Ordering::Relaxed);
            out.total_pieces_distributed +=
                shard.stats.total_pieces_distributed.load(Ordering::Relaxed);
            out.total_bytes_distributed +=
                shard.stats.total_bytes_distributed.load(Ordering::Relaxed);
            out.total_bytes_repatriated +=
                shard.stats.total_bytes_repatriated.load(Ordering::Relaxed);
        }
        out
    }

    /// Most-recent up to `limit` events, drawn across all shards.
    ///
    /// **Ordering note (v2.0 Phase 2.A):** intra-shard order is strict
    /// push order, but events from different shards are not globally
    /// ordered. The previous v1.x behaviour returned a globally
    /// totally-ordered slice because the event log was a single Vec;
    /// the sharded layout sacrifices that for ~256× write parallelism.
    /// Observability consumers (RPC, DCScan, dashboards) treat
    /// `recent_events` as best-effort, which this contract still meets.
    pub fn recent_events(&self, limit: usize) -> Vec<LedgerLifecycleEvent> {
        if limit == 0 {
            return Vec::new();
        }
        // Cheap upper bound: the suffix of every shard. We then take
        // the suffix of the merged Vec to honour `limit`.
        let mut merged: Vec<LedgerLifecycleEvent> = Vec::new();
        for shard in self.shards.iter() {
            let log = shard.events.read();
            let start = log.len().saturating_sub(limit);
            merged.extend_from_slice(&log[start..]);
        }
        let start = merged.len().saturating_sub(limit);
        merged.split_off(start)
    }

    pub fn erasure_audits(&self) -> Vec<LedgerErasureAudit> {
        self.erasure_audits.read().clone()
    }
}

impl Default for LedgerLifecycleManager {
    fn default() -> Self {
        Self::new(LifecycleConfig::default())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slicing_small_content() {
        let content = vec![0xAA; 100];
        let result = slice_encrypted_content(&content, 256 * 1024);
        assert_eq!(result.pieces.len(), 1);
        assert_eq!(result.total_size, 100);
    }

    #[test]
    fn test_slicing_large_content() {
        let content = vec![0xBB; 1024 * 1024];
        let result = slice_encrypted_content(&content, 256 * 1024);
        assert_eq!(result.pieces.len(), 4);
        assert_eq!(result.total_size, 1024 * 1024);

        for piece in &result.pieces {
            assert!(verify_piece(&piece.data, &piece.hash));
        }
    }

    #[test]
    fn test_reassemble_preserves_content() {
        let original = vec![0xCC; 700_000];
        let sliced = slice_encrypted_content(&original, 256 * 1024);

        let mut indexed_pieces: Vec<(u32, Vec<u8>)> = sliced
            .pieces
            .iter()
            .map(|p| (p.index, p.data.clone()))
            .collect();

        indexed_pieces.reverse();

        let reassembled = reassemble_pieces(&mut indexed_pieces);
        assert_eq!(reassembled, original);
    }

    #[test]
    fn test_verify_piece_integrity() {
        let data = b"piece content here";
        let hash = *blake3::hash(data).as_bytes();

        assert!(verify_piece(data, &hash));
        assert!(!verify_piece(b"wrong content", &hash));
    }

    #[test]
    fn test_lifecycle_manager_stats() {
        let manager = LedgerLifecycleManager::default();

        manager.record_creation(vec![0x01; 20], [0xAA; 32], 0);
        manager.record_append(vec![0x01; 20], [0xBB; 32], [0xAA; 32], 1024, 4);
        manager.record_repatriation(vec![0x01; 20], 2, 2048, 150);

        let stats = manager.stats();
        assert_eq!(stats.ledgers_created, 1);
        assert_eq!(stats.entries_appended, 1);
        assert_eq!(stats.repatriations_completed, 1);
        assert_eq!(stats.total_pieces_distributed, 4);
    }

    #[test]
    fn test_erasure_audit() {
        let mut audit =
            LedgerErasureAudit::new(vec![0x01; 20], DeletionReason::OwnerRequest);
        audit.complete(5, "oes_evolution", vec![0, 1, 2, 3, 4]);

        assert_eq!(audit.entries_erased, 5);
        assert!(audit.completed_at.is_some());
        assert_ne!(audit.audit_hash, [0u8; 32]);
    }

    // ----------------------------------------------------------------
    // Quipu Canon v2.0 Phase 2.A — sharded lifecycle manager tests
    // ----------------------------------------------------------------

    #[test]
    fn test_lifecycle_manager_has_256_shards() {
        let manager = LedgerLifecycleManager::default();
        assert_eq!(manager.shard_count(), LIFECYCLE_SHARDS);
        assert_eq!(LIFECYCLE_SHARDS, 256);
    }

    #[test]
    fn test_shard_for_wallet_uses_first_byte() {
        // Two wallets sharing only their first byte must hash to the
        // same shard; differing first bytes must hash to different
        // shards. This is the contract every other v2.0 shard map
        // (lattice, head locks, HLC, key cache) implements.
        let a = vec![0x42u8; 20];
        let b = vec![0x42u8, 0x99, 0xAB];
        let c = vec![0x43u8; 20];
        assert_eq!(shard_for_wallet(&a), shard_for_wallet(&b));
        assert_ne!(shard_for_wallet(&a), shard_for_wallet(&c));
        assert_eq!(shard_for_wallet(&[]), 0, "empty addr falls back to 0");
    }

    #[test]
    fn test_stats_aggregate_across_shards() {
        // Distribute creations across distinct shards (different
        // first bytes) and verify the aggregated snapshot picks them
        // all up. If any shard's atomics were skipped, the asserts
        // here would tell us immediately.
        let manager = LedgerLifecycleManager::default();

        for i in 0u8..16 {
            let w = vec![i, 0u8, 0u8, 0u8];
            manager.record_creation(w.clone(), [i; 32], 0);
            manager.record_append(w.clone(), [i; 32], [0u8; 32], 1024, 2);
        }

        let stats = manager.stats();
        assert_eq!(stats.ledgers_created, 16);
        assert_eq!(stats.entries_appended, 16);
        assert_eq!(stats.total_pieces_distributed, 32);
        assert_eq!(stats.total_bytes_distributed, 16 * 1024);
    }

    #[test]
    fn test_recent_events_returns_intra_shard_suffix() {
        // All events go to one shard (same first byte) — recent_events
        // must return them in push order, capped at `limit`.
        let manager = LedgerLifecycleManager::default();
        let wallet = vec![0xAB, 0x00, 0x00];
        for i in 0..5 {
            manager.record_append(wallet.clone(), [i as u8; 32], [0u8; 32], 16, 1);
        }
        let recent = manager.recent_events(3);
        assert_eq!(recent.len(), 3);
        // Last three appended must be the last three returned.
        let ids: Vec<u8> = recent
            .iter()
            .map(|e| match e {
                LedgerLifecycleEvent::EntryAppended { string_id, .. } => string_id[0],
                _ => 0xFF,
            })
            .collect();
        assert_eq!(ids, vec![2, 3, 4]);
    }

    #[test]
    fn test_concurrent_appends_distinct_shards_do_not_serialise() {
        // Stress test: 16 threads × 1024 ops, each thread targeting
        // its own shard via wallet[0] = thread id. With v1.x's two
        // global RwLocks this took >>1 s and routinely tripped the
        // benchmark cliff at 8+ threads. With Phase 2.A sharding the
        // run finishes in well under 1 s on ordinary hardware.
        use std::sync::Arc;
        use std::thread;
        use std::time::Instant;

        const THREADS: usize = 16;
        const OPS_PER_THREAD: usize = 1024;

        let manager = Arc::new(LedgerLifecycleManager::default());
        let started = Instant::now();
        let mut handles = Vec::with_capacity(THREADS);
        for tid in 0..THREADS {
            let m = manager.clone();
            handles.push(thread::spawn(move || {
                let wallet = vec![tid as u8, 0u8, 0u8, 0u8];
                for op in 0..OPS_PER_THREAD {
                    m.record_append(wallet.clone(), [op as u8; 32], [0u8; 32], 128, 1);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let elapsed = started.elapsed();

        let stats = manager.stats();
        assert_eq!(stats.entries_appended, (THREADS * OPS_PER_THREAD) as u64);
        assert_eq!(
            stats.total_pieces_distributed,
            (THREADS * OPS_PER_THREAD) as u64
        );

        // Soft regression budget — 1 s is generous; a healthy build
        // finishes in 10–50 ms. If this ever drifts back into the
        // hundreds-of-ms range, the per-shard event log has likely
        // regained a global bottleneck.
        assert!(
            elapsed.as_millis() < 1_000,
            "16 × 1024 sharded record_append took {} ms — \
             v2.0 Phase 2.A regression",
            elapsed.as_millis()
        );
    }

    #[test]
    fn test_record_deletion_keeps_audits_global_and_event_local() {
        let manager = LedgerLifecycleManager::default();
        let wallet = vec![0x10u8; 20];

        manager.record_creation(wallet.clone(), [0u8; 32], 0);
        let mut audit = LedgerErasureAudit::new(wallet.clone(), DeletionReason::OwnerRequest);
        audit.complete(3, "oes_evolution", vec![0, 1, 2]);
        manager.record_deletion(audit);

        let audits = manager.erasure_audits();
        assert_eq!(audits.len(), 1, "audits remain on the global list");

        let recent = manager.recent_events(8);
        assert!(
            recent
                .iter()
                .any(|e| matches!(e, LedgerLifecycleEvent::LedgerDeleted { .. })),
            "deletion event must appear in the per-shard event log"
        );
        assert_eq!(manager.stats().ledgers_deleted, 1);
    }
}
