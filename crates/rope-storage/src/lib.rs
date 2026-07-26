//! # Datachain Rope Storage
//!
//! Persistent storage using RocksDB with LSM optimization.
//!
//! ## Storage Layout
//!
//! - `lattice_db/` - String Lattice persistence
//! - `complement_db/` - Complement storage (separate for security)
//! - `state_db/` - OES and federation state
//!
//! ## Quipu Canon v2.0 Phase 1.5 — RocksDB-backed `LedgerStore`
//!
//! `ledger_db::LedgerStore` is the only store that has been promoted from
//! the v1.x in-memory `RwLock<HashMap>` placeholder to a real RocksDB
//! backend. Two constructors exist:
//!
//! - [`ledger_db::LedgerStore::new`] — pure in-memory mode (tests + the
//!   v1.x default). Backwards-compatible with every existing caller.
//! - [`ledger_db::LedgerStore::open`] — opens a RocksDB database at the
//!   given path, recovers state, and starts the background flusher.
//!   In-memory mirror is retained as a write-through cache for the
//!   read hot path; writes are mirrored to disk via the flusher.
//!
//! See [`rocksdb_persistence`] for the on-disk schema, durability
//! watermark, and recovery details.

pub mod lattice_db {
    //! Lattice persistence layer

    use parking_lot::RwLock;
    use std::collections::HashMap;

    /// M1 (2026-07-25 audit): default cap on the number of live entries this
    /// in-memory store will hold before it starts evicting to make room for
    /// new inserts. 2M entries * (32-byte key + typically-small knot blob) is
    /// a bounded, sane ceiling for a placeholder store; RocksDB-backed
    /// `ledger_db::LedgerStore` is the real production path (see module
    /// docs) and is not subject to this cap.
    pub const DEFAULT_MAX_LATTICE_ENTRIES: usize = 2_000_000;

    /// Simple in-memory lattice storage (RocksDB will replace this in production)
    ///
    /// Bounded: once `max_entries` live keys are held, further `put`s for a
    /// *new* key evict one existing entry first (O(1) arbitrary-victim
    /// eviction — this store has no access-recency tracking, so it cannot
    /// offer true LRU without extra bookkeeping; a random/arbitrary victim
    /// is a standard, well-understood bounded-cache eviction policy and is
    /// sufficient to close the unbounded-memory-growth exposure). Updates to
    /// an *existing* key never trigger eviction.
    pub struct LatticeStore {
        data: RwLock<HashMap<[u8; 32], Vec<u8>>>,
        max_entries: usize,
    }

    impl LatticeStore {
        pub fn new() -> Self {
            Self::with_capacity(DEFAULT_MAX_LATTICE_ENTRIES)
        }

        /// Construct with an explicit entry cap. `max_entries` is clamped to
        /// at least 1 so the store is never accidentally unusable.
        pub fn with_capacity(max_entries: usize) -> Self {
            Self {
                data: RwLock::new(HashMap::new()),
                max_entries: max_entries.max(1),
            }
        }

        pub fn put(&self, key: [u8; 32], value: Vec<u8>) {
            let mut data = self.data.write();
            if !data.contains_key(&key) && data.len() >= self.max_entries {
                if let Some(victim) = data.keys().next().copied() {
                    data.remove(&victim);
                }
            }
            data.insert(key, value);
        }

        pub fn get(&self, key: &[u8; 32]) -> Option<Vec<u8>> {
            self.data.read().get(key).cloned()
        }

        pub fn delete(&self, key: &[u8; 32]) -> bool {
            self.data.write().remove(key).is_some()
        }

        pub fn contains(&self, key: &[u8; 32]) -> bool {
            self.data.read().contains_key(key)
        }

        /// Current number of live entries.
        pub fn len(&self) -> usize {
            self.data.read().len()
        }

        pub fn is_empty(&self) -> bool {
            self.data.read().is_empty()
        }

        /// The configured entry cap.
        pub fn capacity(&self) -> usize {
            self.max_entries
        }
    }

    impl Default for LatticeStore {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod complement_db {
    //! Complement storage - isolated for security

    use parking_lot::RwLock;
    use std::collections::HashMap;

    /// M1 (2026-07-25 audit): see `lattice_db::DEFAULT_MAX_LATTICE_ENTRIES`
    /// for the rationale. Complement payloads are typically small (key
    /// shreds / OES material), so the default cap is generous.
    pub const DEFAULT_MAX_COMPLEMENT_ENTRIES: usize = 2_000_000;

    /// Complement storage with separate encryption context.
    ///
    /// Bounded the same way as `LatticeStore`: at capacity, inserting a new
    /// key evicts one arbitrary existing entry first. Updates to an
    /// existing key never evict.
    pub struct ComplementStore {
        data: RwLock<HashMap<[u8; 32], Vec<u8>>>,
        max_entries: usize,
    }

    impl ComplementStore {
        pub fn new() -> Self {
            Self::with_capacity(DEFAULT_MAX_COMPLEMENT_ENTRIES)
        }

        pub fn with_capacity(max_entries: usize) -> Self {
            Self {
                data: RwLock::new(HashMap::new()),
                max_entries: max_entries.max(1),
            }
        }

        pub fn store_complement(&self, string_id: [u8; 32], complement_data: Vec<u8>) {
            let mut data = self.data.write();
            if !data.contains_key(&string_id) && data.len() >= self.max_entries {
                if let Some(victim) = data.keys().next().copied() {
                    data.remove(&victim);
                }
            }
            data.insert(string_id, complement_data);
        }

        pub fn get_complement(&self, string_id: &[u8; 32]) -> Option<Vec<u8>> {
            self.data.read().get(string_id).cloned()
        }

        pub fn erase_complement(&self, string_id: &[u8; 32]) -> bool {
            self.data.write().remove(string_id).is_some()
        }

        pub fn len(&self) -> usize {
            self.data.read().len()
        }

        pub fn is_empty(&self) -> bool {
            self.data.read().is_empty()
        }

        pub fn capacity(&self) -> usize {
            self.max_entries
        }
    }

    impl Default for ComplementStore {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod state_db {
    //! OES and federation state persistence

    use parking_lot::RwLock;
    use std::collections::HashMap;

    /// M1 (2026-07-25 audit): OES/federation state entries are keyed by
    /// node/federation id (bounded by real-world node counts), so a much
    /// smaller cap than the lattice/complement stores is appropriate —
    /// still generous enough to never bind legitimate operation.
    pub const DEFAULT_MAX_STATE_ENTRIES: usize = 100_000;

    /// State persistence for OES and federation.
    ///
    /// Both maps are bounded independently: at capacity, inserting a new
    /// key evicts one arbitrary existing entry from that map first. Updates
    /// to an existing key never evict.
    pub struct StateStore {
        oes_states: RwLock<HashMap<String, Vec<u8>>>,
        federation_states: RwLock<HashMap<String, Vec<u8>>>,
        max_entries: usize,
    }

    impl StateStore {
        pub fn new() -> Self {
            Self::with_capacity(DEFAULT_MAX_STATE_ENTRIES)
        }

        pub fn with_capacity(max_entries: usize) -> Self {
            Self {
                oes_states: RwLock::new(HashMap::new()),
                federation_states: RwLock::new(HashMap::new()),
                max_entries: max_entries.max(1),
            }
        }

        pub fn save_oes_state(&self, node_id: &str, state: Vec<u8>) {
            let mut map = self.oes_states.write();
            if !map.contains_key(node_id) && map.len() >= self.max_entries {
                if let Some(victim) = map.keys().next().cloned() {
                    map.remove(&victim);
                }
            }
            map.insert(node_id.to_string(), state);
        }

        pub fn load_oes_state(&self, node_id: &str) -> Option<Vec<u8>> {
            self.oes_states.read().get(node_id).cloned()
        }

        pub fn save_federation_state(&self, fed_id: &str, state: Vec<u8>) {
            let mut map = self.federation_states.write();
            if !map.contains_key(fed_id) && map.len() >= self.max_entries {
                if let Some(victim) = map.keys().next().cloned() {
                    map.remove(&victim);
                }
            }
            map.insert(fed_id.to_string(), state);
        }

        pub fn load_federation_state(&self, fed_id: &str) -> Option<Vec<u8>> {
            self.federation_states.read().get(fed_id).cloned()
        }

        pub fn oes_state_count(&self) -> usize {
            self.oes_states.read().len()
        }

        pub fn federation_state_count(&self) -> usize {
            self.federation_states.read().len()
        }

        pub fn capacity(&self) -> usize {
            self.max_entries
        }
    }

    impl Default for StateStore {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod rocksdb_persistence;

pub mod ledger_db {
    //! Personal ledger storage — wallet→StringId index and piece map persistence.
    //!
    //! Provides the storage backend for the personal ledger model where each
    //! wallet maps to a chain of StringIds. Maintains reverse indexes for
    //! efficient lookups in both directions.
    //!
    //! Quipu Canon v2.0 Phase 1.5: optionally backed by RocksDB. Use
    //! [`LedgerStore::new`] for the in-memory v1.x mode, or
    //! [`LedgerStore::open`] for disk-persistent operation. The in-memory
    //! mirror always exists; disk writes go through the WriteBatch
    //! background flusher in [`crate::rocksdb_persistence`].

    use crate::rocksdb_persistence::{
        RecoveredState, RocksError, RocksPersistence, StoredTombstone, WriteOp,
    };
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Ledger descriptor stored per wallet
    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct StoredLedgerDescriptor {
        pub wallet_address: Vec<u8>,
        pub genesis_string_id: [u8; 32],
        pub head_string_id: [u8; 32],
        pub entry_count: u64,
        pub total_size_bytes: u64,
        pub oes_generation_at_creation: u64,
        pub current_oes_generation: u64,
        pub created_at: i64,
        pub last_appended_at: i64,
        pub is_deleted: bool,
        pub deleted_at: Option<i64>,
        pub replication_factor: u32,
    }

    /// Piece map entry for storage
    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct StoredPieceMap {
        pub string_id: [u8; 32],
        pub total_pieces: u32,
        pub total_size: u64,
        pub piece_hashes: Vec<[u8; 32]>,
        pub piece_sizes: Vec<u32>,
    }

    /// Personal ledger storage. In-memory mirror is always present for
    /// the read hot path; disk persistence is optional and enabled by
    /// constructing via [`Self::open`].
    pub struct LedgerStore {
        descriptors: RwLock<HashMap<Vec<u8>, StoredLedgerDescriptor>>,
        wallet_to_chain: RwLock<HashMap<Vec<u8>, Vec<[u8; 32]>>>,
        string_to_wallet: RwLock<HashMap<[u8; 32], Vec<u8>>>,
        piece_maps: RwLock<HashMap<[u8; 32], StoredPieceMap>>,
        head_index: RwLock<HashMap<Vec<u8>, [u8; 32]>>,

        /// Per-wallet append counter, used to assign `seq_in_wallet`
        /// values for the chain CF. Lives in-memory because the
        /// counter is purely a function of the chain length we already
        /// hold in `wallet_to_chain`.
        ///
        /// Wrapped in `RwLock<HashMap<…, AtomicU64>>` rather than a
        /// plain `RwLock<HashMap<…, u64>>` so that the per-wallet
        /// counter bump under [`Self::append_to_chain`] does not need
        /// the outer write lock once the AtomicU64 is allocated.
        wallet_append_counter: RwLock<HashMap<Vec<u8>, Arc<AtomicU64>>>,

        /// `Some(persistence)` when disk-backed; `None` for in-memory mode.
        persistence: Option<Arc<RocksPersistence>>,
        /// Highest seq number ever returned to a caller; tracked even in
        /// in-memory mode so callers can use the same `wait_durable` API
        /// (which is a no-op in in-memory mode).
        last_enqueued_seq: AtomicU64,
    }

    impl LedgerStore {
        /// Create a new empty in-memory store. No disk persistence.
        ///
        /// Backwards-compatible with every v1.x caller.
        pub fn new() -> Self {
            Self {
                descriptors: RwLock::new(HashMap::new()),
                wallet_to_chain: RwLock::new(HashMap::new()),
                string_to_wallet: RwLock::new(HashMap::new()),
                piece_maps: RwLock::new(HashMap::new()),
                head_index: RwLock::new(HashMap::new()),
                wallet_append_counter: RwLock::new(HashMap::new()),
                persistence: None,
                last_enqueued_seq: AtomicU64::new(0),
            }
        }

        /// Open or create a RocksDB-backed store at `path`. On open the
        /// store recovers its in-memory mirror from disk and resumes
        /// the durability watermark.
        ///
        /// Quipu Canon v2.0 Phase 1.5.
        pub fn open(path: impl AsRef<Path>) -> Result<Self, RocksError> {
            let (persistence, recovered) = RocksPersistence::open(path)?;
            Ok(Self::from_recovered(persistence, recovered))
        }

        /// Phase 1.6 — like [`Self::open`] but hands the recovered
        /// `RopeString` blobs and untie-tombstones back to the caller
        /// so it can rebuild the in-process `StringLattice`. The store
        /// itself only mirrors descriptor/chain/head/piece state; knot
        /// payloads live in the lattice.
        pub fn open_with_recovery(
            path: impl AsRef<Path>,
        ) -> Result<(Self, Vec<([u8; 32], Vec<u8>)>, Vec<([u8; 32], StoredTombstone)>), RocksError>
        {
            let (persistence, mut recovered) = RocksPersistence::open(path)?;
            let blobs = std::mem::take(&mut recovered.string_blobs);
            let tombstones = std::mem::take(&mut recovered.tombstones);
            Ok((Self::from_recovered(persistence, recovered), blobs, tombstones))
        }

        /// Build a store from an already-opened persistence handle and a
        /// recovery snapshot. Useful when the caller wants to share one
        /// `RocksPersistence` across multiple stores or to inspect the
        /// snapshot before constructing the store.
        pub fn from_recovered(
            persistence: Arc<RocksPersistence>,
            recovered: RecoveredState,
        ) -> Self {
            let mut descriptors = HashMap::new();
            for (w, d) in recovered.descriptors {
                descriptors.insert(w, d);
            }

            let mut head_index = HashMap::new();
            for (w, h) in recovered.heads {
                head_index.insert(w, h);
            }

            let mut wallet_to_chain = HashMap::new();
            let mut wallet_append_counter = HashMap::new();
            for (w, chain) in recovered.chains {
                let next_seq = chain.len() as u64;
                wallet_to_chain.insert(w.clone(), chain);
                wallet_append_counter.insert(w, Arc::new(AtomicU64::new(next_seq)));
            }

            let mut string_to_wallet = HashMap::new();
            for (sid, w) in recovered.reverse {
                string_to_wallet.insert(sid, w);
            }

            let mut piece_maps = HashMap::new();
            for (sid, pm) in recovered.pieces {
                piece_maps.insert(sid, pm);
            }

            Self {
                descriptors: RwLock::new(descriptors),
                wallet_to_chain: RwLock::new(wallet_to_chain),
                string_to_wallet: RwLock::new(string_to_wallet),
                piece_maps: RwLock::new(piece_maps),
                head_index: RwLock::new(head_index),
                wallet_append_counter: RwLock::new(wallet_append_counter),
                persistence: Some(persistence),
                last_enqueued_seq: AtomicU64::new(recovered.durable_seq),
            }
        }

        /// True if this store has a disk backend.
        pub fn is_persistent(&self) -> bool {
            self.persistence.is_some()
        }

        /// Highest sequence number assigned to any write returned by
        /// this store. In in-memory mode, `0` (no writes are
        /// sequence-tracked). In persistent mode, increases on every
        /// mutating call.
        pub fn last_enqueued_seq(&self) -> u64 {
            self.last_enqueued_seq.load(Ordering::Acquire)
        }

        /// Block until every write enqueued so far has been fsync'd.
        /// Returns `true` immediately in in-memory mode (vacuously
        /// durable since there is no disk).
        pub fn await_all_durable(&self, timeout: Duration) -> bool {
            match &self.persistence {
                None => true,
                Some(p) => p.wait_durable(self.last_enqueued_seq(), timeout),
            }
        }

        /// Block until the specific seq returned from a previous mutating
        /// call has been fsync'd. In in-memory mode, returns `true`
        /// immediately.
        pub fn wait_durable(&self, seq: u64, timeout: Duration) -> bool {
            match &self.persistence {
                None => true,
                Some(p) => p.wait_durable(seq, timeout),
            }
        }

        /// Helper: enqueue a write op (when persistent) and bump the
        /// store-level `last_enqueued_seq`. In-memory mode is a no-op.
        fn enqueue(&self, op: WriteOp) -> u64 {
            match &self.persistence {
                None => 0,
                Some(p) => match p.enqueue(op) {
                    Ok(seq) => {
                        // Single-thread monotonic store; `fetch_max` is
                        // overkill but cheap and self-documenting.
                        self.last_enqueued_seq.fetch_max(seq, Ordering::AcqRel);
                        seq
                    }
                    Err(e) => {
                        // Persistence stopped (e.g. disk full). Log and
                        // continue with the in-memory mirror — the node
                        // will keep operating but will not survive a
                        // restart. Operators should monitor
                        // `last_enqueued_seq()` vs `durable_seq()` for
                        // divergence.
                        tracing::error!("rope-storage enqueue failed: {e:?}");
                        0
                    }
                },
            }
        }

        pub fn put_descriptor(&self, wallet: &[u8], desc: StoredLedgerDescriptor) {
            self.head_index
                .write()
                .insert(wallet.to_vec(), desc.head_string_id);
            self.descriptors
                .write()
                .insert(wallet.to_vec(), desc.clone());
            self.enqueue(WriteOp::PutDescriptor {
                wallet: wallet.to_vec(),
                desc,
            });
        }

        pub fn get_descriptor(&self, wallet: &[u8]) -> Option<StoredLedgerDescriptor> {
            self.descriptors.read().get(wallet).cloned()
        }

        pub fn append_to_chain(&self, wallet: &[u8], string_id: [u8; 32]) {
            // Reserve a per-wallet sequence number BEFORE touching the
            // mirror, so concurrent appends to the same wallet land at
            // strictly increasing positions in both the in-memory
            // chain and the chain CF.
            let counter = {
                let read = self.wallet_append_counter.read();
                if let Some(c) = read.get(wallet) {
                    c.clone()
                } else {
                    drop(read);
                    let mut write = self.wallet_append_counter.write();
                    write
                        .entry(wallet.to_vec())
                        .or_insert_with(|| Arc::new(AtomicU64::new(0)))
                        .clone()
                }
            };
            let seq_in_wallet = counter.fetch_add(1, Ordering::AcqRel);

            self.wallet_to_chain
                .write()
                .entry(wallet.to_vec())
                .or_default()
                .push(string_id);
            self.string_to_wallet
                .write()
                .insert(string_id, wallet.to_vec());
            self.head_index.write().insert(wallet.to_vec(), string_id);
            self.enqueue(WriteOp::AppendChain {
                wallet: wallet.to_vec(),
                seq_in_wallet,
                string_id,
            });
        }

        pub fn get_chain(&self, wallet: &[u8]) -> Vec<[u8; 32]> {
            self.wallet_to_chain
                .read()
                .get(wallet)
                .cloned()
                .unwrap_or_default()
        }

        pub fn wallet_for_string(&self, string_id: &[u8; 32]) -> Option<Vec<u8>> {
            self.string_to_wallet.read().get(string_id).cloned()
        }

        pub fn head_for_wallet(&self, wallet: &[u8]) -> Option<[u8; 32]> {
            self.head_index.read().get(wallet).copied()
        }

        /// Phase 1.6 — persist a serialised `RopeString` blob (knot
        /// payload). Disk-only: the read hot path stays in the lattice.
        /// Returns the enqueue seq for optional `wait_durable`.
        pub fn put_string_blob(&self, string_id: [u8; 32], blob: Vec<u8>) -> u64 {
            self.enqueue(WriteOp::PutStringBlob { string_id, blob })
        }

        /// Phase 1.6 — cryptographic erasure on disk: delete a knot's
        /// payload blob. Used by both the whole-string erase pathway
        /// and the per-knot untie pathway.
        pub fn delete_string_blob(&self, string_id: [u8; 32]) -> u64 {
            self.enqueue(WriteOp::DeleteStringBlob { string_id })
        }

        /// Phase 1.6 — persist a canon v1.1 §4.2 untie-tombstone so
        /// the deliberate-absence record survives restarts.
        pub fn put_tombstone(&self, string_id: [u8; 32], tombstone: StoredTombstone) -> u64 {
            self.enqueue(WriteOp::PutTombstone {
                string_id,
                tombstone,
            })
        }

        pub fn put_piece_map(&self, string_id: [u8; 32], map: StoredPieceMap) {
            self.piece_maps.write().insert(string_id, map.clone());
            self.enqueue(WriteOp::PutPieceMap {
                string_id,
                piece_map: map,
            });
        }

        pub fn get_piece_map(&self, string_id: &[u8; 32]) -> Option<StoredPieceMap> {
            self.piece_maps.read().get(string_id).cloned()
        }

        pub fn mark_deleted(&self, wallet: &[u8]) -> bool {
            let now = chrono::Utc::now().timestamp();
            // Capture the updated desc inside the lock, then enqueue a
            // full `PutDescriptor` with the new state. We avoid the
            // older `WriteOp::MarkDeleted` read-modify-write path
            // because, if a fresh `PutDescriptor` for the same wallet
            // is already earlier in the same `WriteBatch`, the
            // `db.get_cf` inside the flusher would not see it (writes
            // in a `WriteBatch` only become visible to reads after
            // `db.write_opt`). Sending a self-contained
            // `PutDescriptor` is order-independent and atomic.
            let updated_desc: Option<StoredLedgerDescriptor> = {
                let mut descs = self.descriptors.write();
                if let Some(desc) = descs.get_mut(wallet) {
                    desc.is_deleted = true;
                    desc.deleted_at = Some(now);
                    Some(desc.clone())
                } else {
                    None
                }
            };
            match updated_desc {
                None => false,
                Some(desc) => {
                    self.enqueue(WriteOp::PutDescriptor {
                        wallet: wallet.to_vec(),
                        desc,
                    });
                    true
                }
            }
        }

        pub fn all_wallets(&self) -> Vec<Vec<u8>> {
            self.descriptors.read().keys().cloned().collect()
        }

        pub fn active_count(&self) -> usize {
            self.descriptors
                .read()
                .values()
                .filter(|d| !d.is_deleted)
                .count()
        }

        pub fn total_count(&self) -> usize {
            self.descriptors.read().len()
        }

        pub fn total_entries(&self) -> u64 {
            self.descriptors
                .read()
                .values()
                .map(|d| d.entry_count)
                .sum()
        }

        pub fn total_bytes(&self) -> u64 {
            self.descriptors
                .read()
                .values()
                .map(|d| d.total_size_bytes)
                .sum()
        }
    }

    impl Default for LedgerStore {
        fn default() -> Self {
            Self::new()
        }
    }
}

// Re-export for convenience
pub use complement_db::ComplementStore;
pub use lattice_db::LatticeStore;
pub use ledger_db::LedgerStore;
pub use rocksdb_persistence::{
    PersistenceStats, RecoveredState, RocksError, RocksPersistence, StoredTombstone, WriteOp,
};
pub use state_db::StateStore;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    mod lattice_store_tests {
        use super::*;

        #[test]
        fn test_lattice_store_creation() {
            let store = LatticeStore::new();
            let key = [1u8; 32];
            assert!(!store.contains(&key));
        }

        #[test]
        fn test_lattice_store_put_get() {
            let store = LatticeStore::new();
            let key = [2u8; 32];
            let value = vec![1, 2, 3, 4, 5];

            store.put(key, value.clone());

            let retrieved = store.get(&key);
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap(), value);
        }

        #[test]
        fn test_lattice_store_delete() {
            let store = LatticeStore::new();
            let key = [3u8; 32];
            let value = vec![10, 20, 30];

            store.put(key, value);
            assert!(store.contains(&key));

            let deleted = store.delete(&key);
            assert!(deleted);
            assert!(!store.contains(&key));
        }

        #[test]
        fn test_lattice_store_get_nonexistent() {
            let store = LatticeStore::new();
            let key = [4u8; 32];
            assert!(store.get(&key).is_none());
        }

        #[test]
        fn test_lattice_store_default() {
            let store: LatticeStore = Default::default();
            let key = [5u8; 32];
            assert!(!store.contains(&key));
        }

        /// M1 (2026-07-25 audit): the store must never grow past its
        /// configured cap, no matter how many distinct keys are inserted.
        #[test]
        fn test_lattice_store_bounded_eviction() {
            let store = LatticeStore::with_capacity(10);
            for i in 0u32..1000 {
                let mut key = [0u8; 32];
                key[0..4].copy_from_slice(&i.to_be_bytes());
                store.put(key, vec![1, 2, 3]);
                assert!(
                    store.len() <= 10,
                    "store grew past its cap of 10 (len={})",
                    store.len()
                );
            }
            assert_eq!(store.capacity(), 10);
        }

        /// Updating an already-present key must never trigger eviction —
        /// only *new* keys competing for a full store should evict.
        #[test]
        fn test_lattice_store_update_does_not_evict() {
            let store = LatticeStore::with_capacity(4);
            let keys: Vec<[u8; 32]> = (0u8..4)
                .map(|i| {
                    let mut k = [0u8; 32];
                    k[0] = i;
                    k
                })
                .collect();
            for k in &keys {
                store.put(*k, vec![0]);
            }
            assert_eq!(store.len(), 4);
            // Re-write every existing key repeatedly — must stay at 4 and
            // every original key must still resolve (no silent eviction of
            // live keys just from updates).
            for _ in 0..50 {
                for k in &keys {
                    store.put(*k, vec![9]);
                }
            }
            assert_eq!(store.len(), 4);
            for k in &keys {
                assert!(store.contains(k), "update-only churn must not evict");
            }
        }
    }

    mod complement_store_tests {
        use super::*;

        #[test]
        fn test_complement_store_creation() {
            let store = ComplementStore::new();
            let string_id = [1u8; 32];
            assert!(store.get_complement(&string_id).is_none());
        }

        #[test]
        fn test_complement_store_put_get() {
            let store = ComplementStore::new();
            let string_id = [2u8; 32];
            let complement = vec![100, 200, 255];

            store.store_complement(string_id, complement.clone());

            let retrieved = store.get_complement(&string_id);
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap(), complement);
        }

        #[test]
        fn test_complement_store_erase() {
            let store = ComplementStore::new();
            let string_id = [3u8; 32];
            let complement = vec![1, 2, 3];

            store.store_complement(string_id, complement);
            assert!(store.get_complement(&string_id).is_some());

            let erased = store.erase_complement(&string_id);
            assert!(erased);
            assert!(store.get_complement(&string_id).is_none());
        }

        #[test]
        fn test_complement_store_default() {
            let store: ComplementStore = Default::default();
            let string_id = [4u8; 32];
            assert!(store.get_complement(&string_id).is_none());
        }

        /// M1 (2026-07-25 audit): bounded eviction, same contract as
        /// `LatticeStore`.
        #[test]
        fn test_complement_store_bounded_eviction() {
            let store = ComplementStore::with_capacity(8);
            for i in 0u32..500 {
                let mut sid = [0u8; 32];
                sid[0..4].copy_from_slice(&i.to_be_bytes());
                store.store_complement(sid, vec![7; 16]);
                assert!(store.len() <= 8, "complement store exceeded its cap");
            }
            assert_eq!(store.capacity(), 8);
        }
    }

    mod state_store_tests {
        use super::*;

        #[test]
        fn test_state_store_creation() {
            let store = StateStore::new();
            assert!(store.load_oes_state("node1").is_none());
            assert!(store.load_federation_state("fed1").is_none());
        }

        #[test]
        fn test_oes_state_save_load() {
            let store = StateStore::new();
            let node_id = "node_abc";
            let state = vec![1, 2, 3, 4];

            store.save_oes_state(node_id, state.clone());

            let loaded = store.load_oes_state(node_id);
            assert!(loaded.is_some());
            assert_eq!(loaded.unwrap(), state);
        }

        #[test]
        fn test_federation_state_save_load() {
            let store = StateStore::new();
            let fed_id = "federation_xyz";
            let state = vec![10, 20, 30];

            store.save_federation_state(fed_id, state.clone());

            let loaded = store.load_federation_state(fed_id);
            assert!(loaded.is_some());
            assert_eq!(loaded.unwrap(), state);
        }

        #[test]
        fn test_state_store_default() {
            let store: StateStore = Default::default();
            assert!(store.load_oes_state("test").is_none());
        }

        /// M1 (2026-07-25 audit): each of the two maps is bounded
        /// independently.
        #[test]
        fn test_state_store_bounded_eviction() {
            let store = StateStore::with_capacity(5);
            for i in 0..200 {
                store.save_oes_state(&format!("node-{i}"), vec![1]);
                assert!(store.oes_state_count() <= 5);
            }
            for i in 0..200 {
                store.save_federation_state(&format!("fed-{i}"), vec![2]);
                assert!(store.federation_state_count() <= 5);
            }
            // The two maps must not share the cap budget.
            assert!(store.oes_state_count() <= 5 && store.federation_state_count() <= 5);
            assert_eq!(store.capacity(), 5);
        }
    }

    /// Quipu Canon v2.0 Phase 1.5 — RocksDB-backed `LedgerStore`.
    ///
    /// Verifies the integration between `LedgerStore`'s in-memory mirror
    /// and the persistence layer: writes against the public store API
    /// must be readable from a freshly-opened store at the same path.
    mod ledger_store_persistent_tests {
        use crate::ledger_db::{LedgerStore, StoredLedgerDescriptor, StoredPieceMap};
        use std::time::Duration;
        use tempfile::TempDir;

        fn dummy_descriptor(wallet: &[u8], head: [u8; 32]) -> StoredLedgerDescriptor {
            StoredLedgerDescriptor {
                wallet_address: wallet.to_vec(),
                genesis_string_id: head,
                head_string_id: head,
                entry_count: 1,
                total_size_bytes: 0,
                oes_generation_at_creation: 0,
                current_oes_generation: 0,
                created_at: 1234567890,
                last_appended_at: 1234567890,
                is_deleted: false,
                deleted_at: None,
                replication_factor: 5,
            }
        }

        #[test]
        fn in_memory_mode_is_not_persistent() {
            let s = LedgerStore::new();
            assert!(!s.is_persistent());
            // wait_durable is a no-op in in-memory mode.
            assert!(s.await_all_durable(Duration::from_millis(1)));
        }

        #[test]
        fn open_creates_empty_persistent_store() {
            let dir = TempDir::new().unwrap();
            let s = LedgerStore::open(dir.path()).unwrap();
            assert!(s.is_persistent());
            assert_eq!(s.total_count(), 0);
            assert_eq!(s.active_count(), 0);
            assert_eq!(s.last_enqueued_seq(), 0);
        }

        #[test]
        fn descriptor_roundtrips_through_disk() {
            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            let wallet = vec![0xAAu8; 20];
            let head = [0xCDu8; 32];

            {
                let s = LedgerStore::open(&path).unwrap();
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, head));
                // Block until the put is fsync'd before dropping the
                // store. Generously over the 10 ms flush tick.
                assert!(s.await_all_durable(Duration::from_secs(2)));
            }

            // Reopen and confirm the descriptor came back.
            let s2 = LedgerStore::open(&path).unwrap();
            let d = s2.get_descriptor(&wallet).expect("descriptor recovered");
            assert_eq!(d.head_string_id, head);
            assert_eq!(s2.total_count(), 1);
            assert_eq!(s2.active_count(), 1);
            assert_eq!(s2.head_for_wallet(&wallet), Some(head));
        }

        #[test]
        fn appended_chain_is_recovered_in_order() {
            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            let wallet = vec![0xBBu8; 20];
            let mut sids = Vec::new();
            for i in 0u8..16 {
                let mut sid = [0u8; 32];
                sid[0] = i;
                sids.push(sid);
            }

            {
                let s = LedgerStore::open(&path).unwrap();
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, sids[0]));
                for sid in &sids {
                    s.append_to_chain(&wallet, *sid);
                }
                assert!(s.await_all_durable(Duration::from_secs(2)));
                let in_mem = s.get_chain(&wallet);
                assert_eq!(in_mem, sids);
            }

            let s2 = LedgerStore::open(&path).unwrap();
            let recovered = s2.get_chain(&wallet);
            assert_eq!(
                recovered, sids,
                "chain order must survive recovery (big-endian seq encoding)"
            );
            // Head pointer must point at the LAST appended sid.
            assert_eq!(s2.head_for_wallet(&wallet), Some(*sids.last().unwrap()));
            // Reverse index must be intact too.
            for sid in &sids {
                assert_eq!(
                    s2.wallet_for_string(sid).as_deref(),
                    Some(wallet.as_slice())
                );
            }
        }

        #[test]
        fn mark_deleted_persists_across_reopen() {
            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            let wallet = vec![0xCCu8; 20];
            let head = [0xEFu8; 32];

            {
                let s = LedgerStore::open(&path).unwrap();
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, head));
                assert!(s.mark_deleted(&wallet));
                assert!(s.await_all_durable(Duration::from_secs(2)));
            }

            let s2 = LedgerStore::open(&path).unwrap();
            let d = s2.get_descriptor(&wallet).unwrap();
            assert!(d.is_deleted, "deletion flag must persist");
            assert!(d.deleted_at.is_some(), "deleted_at must persist");
            assert_eq!(s2.active_count(), 0);
            assert_eq!(s2.total_count(), 1);
        }

        #[test]
        fn piece_maps_persist_across_reopen() {
            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            let sid = [0x77u8; 32];
            let pm = StoredPieceMap {
                string_id: sid,
                total_pieces: 3,
                total_size: 96,
                piece_hashes: vec![[0x01; 32], [0x02; 32], [0x03; 32]],
                piece_sizes: vec![32, 32, 32],
            };

            {
                let s = LedgerStore::open(&path).unwrap();
                s.put_piece_map(sid, pm.clone());
                assert!(s.await_all_durable(Duration::from_secs(2)));
            }

            let s2 = LedgerStore::open(&path).unwrap();
            let recovered = s2.get_piece_map(&sid).expect("piece map recovered");
            assert_eq!(recovered.total_pieces, pm.total_pieces);
            assert_eq!(recovered.piece_hashes, pm.piece_hashes);
            assert_eq!(recovered.piece_sizes, pm.piece_sizes);
        }

        #[test]
        fn concurrent_appends_to_same_wallet_get_distinct_seqs_on_disk() {
            // Every concurrent append must land at a unique seq_in_wallet
            // position so the chain CF doesn't lose entries to overwrite.
            use std::sync::Arc as StdArc;
            use std::thread;

            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            let wallet = vec![0xDDu8; 20];

            const THREADS: usize = 8;
            const APPENDS_PER_THREAD: usize = 25;

            {
                let s = StdArc::new(LedgerStore::open(&path).unwrap());
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, [0u8; 32]));

                let mut handles = Vec::new();
                for tid in 0..THREADS as u8 {
                    let s = s.clone();
                    let wallet = wallet.clone();
                    handles.push(thread::spawn(move || {
                        for i in 0..APPENDS_PER_THREAD {
                            let mut sid = [0u8; 32];
                            sid[0] = tid;
                            sid[1] = i as u8;
                            s.append_to_chain(&wallet, sid);
                        }
                    }));
                }
                for h in handles {
                    h.join().unwrap();
                }
                assert!(s.await_all_durable(Duration::from_secs(5)));
                assert_eq!(s.get_chain(&wallet).len(), THREADS * APPENDS_PER_THREAD);
            }

            let s2 = LedgerStore::open(&path).unwrap();
            let recovered = s2.get_chain(&wallet);
            assert_eq!(
                recovered.len(),
                THREADS * APPENDS_PER_THREAD,
                "no concurrent append may collide on disk seq"
            );
            // All sids must be unique — i.e. no overwrites happened.
            let unique: std::collections::HashSet<_> = recovered.iter().collect();
            assert_eq!(unique.len(), THREADS * APPENDS_PER_THREAD);
        }

        #[test]
        fn unawaited_writes_survive_drop_via_final_drain() {
            // Caller does NOT call wait_durable. The persistence layer's
            // Drop impl must final-drain the channel before closing the
            // DB, so a fast-path append followed by an immediate drop
            // does not lose the write.
            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            let wallet = vec![0xEEu8; 20];
            let sid = [0xABu8; 32];

            {
                let s = LedgerStore::open(&path).unwrap();
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, sid));
                s.append_to_chain(&wallet, sid);
                // No await_all_durable — let Drop final-drain.
            }

            let s2 = LedgerStore::open(&path).unwrap();
            assert_eq!(s2.head_for_wallet(&wallet), Some(sid));
            assert_eq!(s2.get_chain(&wallet), vec![sid]);
        }
    }
}
