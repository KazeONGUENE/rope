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

/// The Ledger Lifecycle Manager — top-level orchestrator.
///
/// This struct is intended to be held as `Arc<LedgerLifecycleManager>` inside
/// `RopeNode` and shared across the RPC server, consensus orchestrator, and
/// network event handlers.
pub struct LedgerLifecycleManager {
    config: LifecycleConfig,
    event_log: RwLock<Vec<LedgerLifecycleEvent>>,
    erasure_audits: RwLock<Vec<LedgerErasureAudit>>,
    stats: RwLock<LifecycleStats>,
}

/// Aggregate statistics
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

impl LedgerLifecycleManager {
    pub fn new(config: LifecycleConfig) -> Self {
        Self {
            config,
            event_log: RwLock::new(Vec::new()),
            erasure_audits: RwLock::new(Vec::new()),
            stats: RwLock::new(LifecycleStats::default()),
        }
    }

    pub fn config(&self) -> &LifecycleConfig {
        &self.config
    }

    /// Record a ledger creation event
    pub fn record_creation(
        &self,
        wallet_address: Vec<u8>,
        genesis_string_id: [u8; 32],
        oes_generation: u64,
    ) {
        let event = LedgerLifecycleEvent::LedgerCreated {
            wallet_address,
            genesis_string_id,
            oes_generation,
        };
        self.event_log.write().push(event);
        self.stats.write().ledgers_created += 1;
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
        let event = LedgerLifecycleEvent::EntryAppended {
            wallet_address,
            string_id,
            parent_id,
            encrypted_size,
            piece_count,
        };
        self.event_log.write().push(event);

        let mut stats = self.stats.write();
        stats.entries_appended += 1;
        stats.total_pieces_distributed += piece_count as u64;
        stats.total_bytes_distributed += encrypted_size;
    }

    /// Record a completed repatriation
    pub fn record_repatriation(
        &self,
        wallet_address: Vec<u8>,
        entries_fetched: usize,
        total_bytes: u64,
        elapsed_ms: u64,
    ) {
        let event = LedgerLifecycleEvent::RepatriationComplete {
            wallet_address,
            entries_fetched,
            total_bytes,
            elapsed_ms,
        };
        self.event_log.write().push(event);

        let mut stats = self.stats.write();
        stats.repatriations_completed += 1;
        stats.total_bytes_repatriated += total_bytes;
    }

    /// Record a ledger deletion
    pub fn record_deletion(&self, audit: LedgerErasureAudit) {
        let wallet = audit.wallet_address.clone();
        let entries = audit.entries_erased;
        let method = audit.key_destruction_method.clone();

        let event = LedgerLifecycleEvent::LedgerDeleted {
            wallet_address: wallet,
            entries_erased: entries,
            key_destruction_method: method,
        };
        self.event_log.write().push(event);
        self.erasure_audits.write().push(audit);
        self.stats.write().ledgers_deleted += 1;
    }

    pub fn stats(&self) -> LifecycleStats {
        self.stats.read().clone()
    }

    pub fn recent_events(&self, limit: usize) -> Vec<LedgerLifecycleEvent> {
        let log = self.event_log.read();
        let start = log.len().saturating_sub(limit);
        log[start..].to_vec()
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
        let mut audit = LedgerErasureAudit::new(vec![0x01; 20], DeletionReason::OwnerRequest);
        audit.complete(5, "oes_evolution", vec![0, 1, 2, 3, 4]);

        assert_eq!(audit.entries_erased, 5);
        assert!(audit.completed_at.is_some());
        assert_ne!(audit.audit_hash, [0u8; 32]);
    }
}
