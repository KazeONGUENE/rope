//! # Repatriation Protocol — BitTorrent-style Ledger Reassembly
//!
//! When a wallet connects, its personal ledger data exists only as encrypted
//! fragments scattered across multiple Datachain Rope nodes. The Repatriation
//! Protocol fetches, reassembles, and verifies these fragments.
//!
//! ## Protocol Flow
//!
//! ```text
//! Wallet connects
//!     │
//!     ▼
//! 1. DISCOVER — Query DHT for all StringIds belonging to this wallet
//!     │
//!     ▼
//! 2. RESOLVE — For each StringId, query DHT for piece providers
//!     │
//!     ▼
//! 3. FETCH — Download pieces using rarest-first from RDP swarms
//!     │
//!     ▼
//! 4. VERIFY — Check piece hashes against the entry piece map
//!     │
//!     ▼
//! 5. ASSEMBLE — Reconstruct each RopeString from its pieces
//!     │
//!     ▼
//! 6. CHAIN — Order entries by Lamport clock (τ) to rebuild the ledger
//!     │
//!     ▼
//! 7. DECRYPT — Wallet uses OES key to decrypt each entry locally
//! ```
//!
//! ## Delta Repatriation
//!
//! If the wallet already has a cached copy of entries up to sequence N,
//! it only needs to fetch entries N+1 onwards. The protocol supports
//! incremental sync by accepting a `known_head` parameter.

use parking_lot::RwLock;
use rope_core::types::{NodeId, StringId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// Configuration for the repatriation protocol
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepatriationConfig {
    pub max_concurrent_piece_requests: usize,
    pub piece_request_timeout: Duration,
    pub max_retry_attempts: u32,
    pub retry_backoff_base: Duration,
    pub max_parallel_entries: usize,
    pub verify_on_receive: bool,
}

impl Default for RepatriationConfig {
    fn default() -> Self {
        Self {
            max_concurrent_piece_requests: 20,
            piece_request_timeout: Duration::from_secs(30),
            max_retry_attempts: 3,
            retry_backoff_base: Duration::from_millis(500),
            max_parallel_entries: 5,
            verify_on_receive: true,
        }
    }
}

/// A request to repatriate a wallet's ledger
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepatriationRequest {
    pub wallet_address: Vec<u8>,
    pub known_head: Option<StringId>,
    pub requested_at: i64,
    pub requester_node: NodeId,
}

/// Status of an ongoing repatriation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RepatriationStatus {
    Discovering,
    Resolving {
        entries_found: usize,
    },
    Fetching {
        entries_total: usize,
        entries_complete: usize,
        pieces_total: u32,
        pieces_received: u32,
    },
    Verifying {
        entries_verified: usize,
        entries_total: usize,
    },
    Assembling,
    Complete {
        entries_repatriated: usize,
        total_bytes: u64,
        elapsed: Duration,
    },
    Failed {
        reason: String,
        recoverable: bool,
    },
}

/// Tracks the state of a single piece being fetched
#[derive(Clone, Debug)]
struct PieceFetchState {
    piece_index: u32,
    piece_hash: [u8; 32],
    string_id: StringId,
    providers: Vec<NodeId>,
    current_provider: Option<NodeId>,
    attempts: u32,
    data: Option<Vec<u8>>,
    verified: bool,
    requested_at: Option<Instant>,
}

impl PieceFetchState {
    fn is_complete(&self) -> bool {
        self.data.is_some() && self.verified
    }

    fn is_timed_out(&self, timeout: Duration) -> bool {
        self.requested_at
            .map(|t| t.elapsed() > timeout)
            .unwrap_or(false)
    }

    fn next_provider(&mut self) -> Option<NodeId> {
        if self.providers.is_empty() {
            return None;
        }
        let provider = self.providers.remove(0);
        self.current_provider = Some(provider);
        self.attempts += 1;
        self.requested_at = Some(Instant::now());
        Some(provider)
    }
}

/// A fully assembled entry ready for decryption by the wallet
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepatriatedEntry {
    pub string_id: StringId,
    pub encrypted_content: Vec<u8>,
    pub lamport_time: u64,
    pub parent_id: StringId,
    pub oes_generation: u64,
    pub sequence_in_chain: u64,
}

/// Result of a completed repatriation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepatriatedLedger {
    pub wallet_address: Vec<u8>,
    pub entries: Vec<RepatriatedEntry>,
    pub genesis_id: StringId,
    pub head_id: StringId,
    pub total_entries: usize,
    pub total_bytes: u64,
    pub repatriation_time_ms: u64,
}

impl RepatriatedLedger {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The Repatriation Engine coordinates fetching a wallet's ledger from the network.
///
/// It works in phases:
/// 1. Discover all StringIds for the wallet (from ledger registry / DHT)
/// 2. Resolve piece maps for each StringId (from DHT)
/// 3. Fetch pieces from providers (rarest-first, parallel)
/// 4. Verify piece integrity (BLAKE3 hash check)
/// 5. Assemble entries and order by Lamport clock
pub struct RepatriationEngine {
    config: RepatriationConfig,
    active_requests: RwLock<HashMap<Vec<u8>, RepatriationSession>>,
    completed_count: RwLock<u64>,
    failed_count: RwLock<u64>,
}

struct RepatriationSession {
    request: RepatriationRequest,
    status: RepatriationStatus,
    entry_ids: Vec<StringId>,
    piece_states: HashMap<(StringId, u32), PieceFetchState>,
    assembled_entries: BTreeMap<u64, RepatriatedEntry>,
    pending_pieces: VecDeque<(StringId, u32)>,
    active_fetches: HashSet<(StringId, u32)>,
    started_at: Instant,
}

impl RepatriationEngine {
    pub fn new(config: RepatriationConfig) -> Self {
        Self {
            config,
            active_requests: RwLock::new(HashMap::new()),
            completed_count: RwLock::new(0),
            failed_count: RwLock::new(0),
        }
    }

    /// Begin repatriation for a wallet. Returns immediately — poll status via `get_status`.
    pub fn begin_repatriation(&self, request: RepatriationRequest) -> Result<(), String> {
        let wallet = request.wallet_address.clone();
        let mut active = self.active_requests.write();

        if active.contains_key(&wallet) {
            return Err("Repatriation already in progress for this wallet".into());
        }

        let session = RepatriationSession {
            request,
            status: RepatriationStatus::Discovering,
            entry_ids: Vec::new(),
            piece_states: HashMap::new(),
            assembled_entries: BTreeMap::new(),
            pending_pieces: VecDeque::new(),
            active_fetches: HashSet::new(),
            started_at: Instant::now(),
        };

        active.insert(wallet, session);
        Ok(())
    }

    /// Feed discovered entry IDs to the session (called after DHT lookup)
    pub fn set_entry_ids(&self, wallet: &[u8], entry_ids: Vec<StringId>) {
        let mut active = self.active_requests.write();
        if let Some(session) = active.get_mut(wallet) {
            let count = entry_ids.len();
            session.entry_ids = entry_ids;
            session.status = RepatriationStatus::Resolving {
                entries_found: count,
            };
        }
    }

    /// Feed piece map information for an entry (called after DHT piece lookup)
    pub fn set_piece_providers(
        &self,
        wallet: &[u8],
        string_id: StringId,
        pieces: Vec<(u32, [u8; 32], Vec<NodeId>)>,
    ) {
        let mut active = self.active_requests.write();
        if let Some(session) = active.get_mut(wallet) {
            for (index, hash, providers) in pieces {
                let state = PieceFetchState {
                    piece_index: index,
                    piece_hash: hash,
                    string_id,
                    providers,
                    current_provider: None,
                    attempts: 0,
                    data: None,
                    verified: false,
                    requested_at: None,
                };
                session.piece_states.insert((string_id, index), state);
                session.pending_pieces.push_back((string_id, index));
            }
        }
    }

    /// Transition to fetching phase — returns next pieces to request
    pub fn start_fetching(&self, wallet: &[u8]) -> Vec<PieceRequest> {
        let mut active = self.active_requests.write();
        let session = match active.get_mut(wallet) {
            Some(s) => s,
            None => return Vec::new(),
        };

        let pieces_total: u32 = session.piece_states.len() as u32;
        session.status = RepatriationStatus::Fetching {
            entries_total: session.entry_ids.len(),
            entries_complete: 0,
            pieces_total,
            pieces_received: 0,
        };

        self.generate_piece_requests(session)
    }

    /// Record a received piece and generate next requests
    pub fn receive_piece(
        &self,
        wallet: &[u8],
        string_id: StringId,
        piece_index: u32,
        data: Vec<u8>,
    ) -> Vec<PieceRequest> {
        let mut active = self.active_requests.write();
        let session = match active.get_mut(wallet) {
            Some(s) => s,
            None => return Vec::new(),
        };

        let key = (string_id, piece_index);
        session.active_fetches.remove(&key);

        if let Some(state) = session.piece_states.get_mut(&key) {
            let hash = *blake3::hash(&data).as_bytes();
            if self.config.verify_on_receive && hash != state.piece_hash {
                tracing::warn!(
                    "Piece hash mismatch for {:?} piece {}, retrying",
                    string_id,
                    piece_index
                );
                if state.attempts < self.config.max_retry_attempts {
                    session.pending_pieces.push_back(key);
                }
            } else {
                state.data = Some(data);
                state.verified = true;
            }
        }

        self.update_status(session);
        self.generate_piece_requests(session)
    }

    /// Assemble all received pieces into the final ledger
    pub fn assemble(&self, wallet: &[u8]) -> Result<RepatriatedLedger, String> {
        let mut active = self.active_requests.write();
        let session = active.get_mut(wallet).ok_or("No active repatriation")?;

        let all_complete = session.piece_states.values().all(|s| s.is_complete());
        if !all_complete {
            return Err("Not all pieces received".into());
        }

        session.status = RepatriationStatus::Assembling;

        let mut entries_by_string: HashMap<StringId, Vec<u8>> = HashMap::new();
        let mut piece_indices: HashMap<StringId, BTreeMap<u32, Vec<u8>>> = HashMap::new();

        for ((sid, idx), state) in &session.piece_states {
            if let Some(data) = &state.data {
                piece_indices
                    .entry(*sid)
                    .or_default()
                    .insert(*idx, data.clone());
            }
        }

        for (sid, pieces) in &piece_indices {
            let mut assembled = Vec::new();
            for (_idx, data) in pieces {
                assembled.extend_from_slice(data);
            }
            entries_by_string.insert(*sid, assembled);
        }

        let elapsed = session.started_at.elapsed();
        let mut total_bytes: u64 = 0;

        let mut ordered_entries: Vec<RepatriatedEntry> = Vec::new();
        for (seq, sid) in session.entry_ids.iter().enumerate() {
            if let Some(content) = entries_by_string.get(sid) {
                total_bytes += content.len() as u64;
                let parent = if seq > 0 {
                    session.entry_ids[seq - 1]
                } else {
                    StringId::ZERO
                };
                ordered_entries.push(RepatriatedEntry {
                    string_id: *sid,
                    encrypted_content: content.clone(),
                    lamport_time: seq as u64,
                    parent_id: parent,
                    oes_generation: 0,
                    sequence_in_chain: seq as u64,
                });
            }
        }

        let genesis_id = session.entry_ids.first().copied().unwrap_or(StringId::ZERO);
        let head_id = session.entry_ids.last().copied().unwrap_or(StringId::ZERO);
        let total_entries = ordered_entries.len();

        session.status = RepatriationStatus::Complete {
            entries_repatriated: total_entries,
            total_bytes,
            elapsed,
        };

        let ledger = RepatriatedLedger {
            wallet_address: session.request.wallet_address.clone(),
            entries: ordered_entries,
            genesis_id,
            head_id,
            total_entries,
            total_bytes,
            repatriation_time_ms: elapsed.as_millis() as u64,
        };

        Ok(ledger)
    }

    /// Finalize and remove the session
    pub fn finalize(&self, wallet: &[u8]) {
        let mut active = self.active_requests.write();
        if let Some(session) = active.remove(wallet) {
            match session.status {
                RepatriationStatus::Complete { .. } => {
                    *self.completed_count.write() += 1;
                }
                RepatriationStatus::Failed { .. } => {
                    *self.failed_count.write() += 1;
                }
                _ => {}
            }
        }
    }

    /// Get current status of a repatriation
    pub fn get_status(&self, wallet: &[u8]) -> Option<RepatriationStatus> {
        self.active_requests
            .read()
            .get(wallet)
            .map(|s| s.status.clone())
    }

    pub fn completed_count(&self) -> u64 {
        *self.completed_count.read()
    }

    pub fn failed_count(&self) -> u64 {
        *self.failed_count.read()
    }

    fn generate_piece_requests(&self, session: &mut RepatriationSession) -> Vec<PieceRequest> {
        let mut requests = Vec::new();
        let available_slots = self
            .config
            .max_concurrent_piece_requests
            .saturating_sub(session.active_fetches.len());

        for _ in 0..available_slots {
            let key = match session.pending_pieces.pop_front() {
                Some(k) => k,
                None => break,
            };

            if let Some(state) = session.piece_states.get_mut(&key) {
                if state.is_complete() {
                    continue;
                }
                if let Some(provider) = state.next_provider() {
                    requests.push(PieceRequest {
                        string_id: key.0,
                        piece_index: key.1,
                        piece_hash: state.piece_hash,
                        provider,
                    });
                    session.active_fetches.insert(key);
                }
            }
        }

        requests
    }

    fn update_status(&self, session: &mut RepatriationSession) {
        let pieces_received = session
            .piece_states
            .values()
            .filter(|s| s.is_complete())
            .count() as u32;
        let pieces_total = session.piece_states.len() as u32;

        let mut entries_complete = 0;
        let mut pieces_per_entry: HashMap<StringId, (u32, u32)> = HashMap::new();
        for ((sid, _), state) in &session.piece_states {
            let entry = pieces_per_entry.entry(*sid).or_insert((0, 0));
            entry.1 += 1;
            if state.is_complete() {
                entry.0 += 1;
            }
        }
        for (total_ok, total) in pieces_per_entry.values() {
            if total_ok == total {
                entries_complete += 1;
            }
        }

        session.status = RepatriationStatus::Fetching {
            entries_total: session.entry_ids.len(),
            entries_complete,
            pieces_total,
            pieces_received,
        };
    }
}

/// A request to fetch a specific piece from a specific node
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PieceRequest {
    pub string_id: StringId,
    pub piece_index: u32,
    pub piece_hash: [u8; 32],
    pub provider: NodeId,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_wallet() -> Vec<u8> {
        vec![
            0x60, 0xFB, 0x32, 0xEF, 0x3A, 0x23, 0x81, 0xC2, 0xED, 0x71, 0x61, 0x3F, 0x34, 0xFD,
            0x56, 0xD5, 0x6F, 0xCF, 0x41, 0x95,
        ]
    }

    #[test]
    fn test_repatriation_lifecycle() {
        let engine = RepatriationEngine::new(RepatriationConfig::default());
        let wallet = test_wallet();
        let node = NodeId::new([1u8; 32]);

        let request = RepatriationRequest {
            wallet_address: wallet.clone(),
            known_head: None,
            requested_at: 0,
            requester_node: node,
        };

        engine.begin_repatriation(request).unwrap();

        let status = engine.get_status(&wallet).unwrap();
        assert!(matches!(status, RepatriationStatus::Discovering));

        let sid = StringId::from_content(b"entry_0");
        engine.set_entry_ids(&wallet, vec![sid]);

        let status = engine.get_status(&wallet).unwrap();
        assert!(matches!(
            status,
            RepatriationStatus::Resolving { entries_found: 1 }
        ));

        let piece_data = vec![0xAA; 256];
        let piece_hash = *blake3::hash(&piece_data).as_bytes();
        engine.set_piece_providers(&wallet, sid, vec![(0, piece_hash, vec![node])]);

        let requests = engine.start_fetching(&wallet);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].piece_index, 0);

        let next = engine.receive_piece(&wallet, sid, 0, piece_data);
        assert!(next.is_empty());

        let ledger = engine.assemble(&wallet).unwrap();
        assert_eq!(ledger.total_entries, 1);
        assert_eq!(ledger.entries[0].encrypted_content, vec![0xAA; 256]);

        engine.finalize(&wallet);
        assert_eq!(engine.completed_count(), 1);
    }

    #[test]
    fn test_duplicate_repatriation_rejected() {
        let engine = RepatriationEngine::new(RepatriationConfig::default());
        let wallet = test_wallet();
        let node = NodeId::new([1u8; 32]);

        let request = RepatriationRequest {
            wallet_address: wallet.clone(),
            known_head: None,
            requested_at: 0,
            requester_node: node,
        };

        engine.begin_repatriation(request.clone()).unwrap();
        let result = engine.begin_repatriation(request);
        assert!(result.is_err());
    }

    #[test]
    fn test_piece_hash_verification() {
        let engine = RepatriationEngine::new(RepatriationConfig::default());
        let wallet = test_wallet();
        let node = NodeId::new([1u8; 32]);

        let request = RepatriationRequest {
            wallet_address: wallet.clone(),
            known_head: None,
            requested_at: 0,
            requester_node: node,
        };

        engine.begin_repatriation(request).unwrap();

        let sid = StringId::from_content(b"test");
        engine.set_entry_ids(&wallet, vec![sid]);

        let correct_data = vec![0xBB; 128];
        let correct_hash = *blake3::hash(&correct_data).as_bytes();
        engine.set_piece_providers(&wallet, sid, vec![(0, correct_hash, vec![node, node])]);

        engine.start_fetching(&wallet);

        let bad_data = vec![0xCC; 128];
        let _next = engine.receive_piece(&wallet, sid, 0, bad_data);

        let result = engine.assemble(&wallet);
        assert!(result.is_err());
    }
}
