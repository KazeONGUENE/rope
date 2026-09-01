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

/// Quipu Canon v2.0 Phase 2.B — parallel-writer RocksDB persistence
/// backend. See the module docs for the design, correctness proof,
/// and the `next_seq` fix that closed a false-ack bug present in the
/// v2 tree.
///
/// Opt-in per-process at [`LedgerStore::open`] time via the env var
/// `ROPE_LEDGER_P2B=1`. Default is off, i.e. the legacy single-flusher
/// [`rocksdb_persistence::RocksPersistence`] backend continues to serve
/// every `LedgerStore::open` caller until an operator explicitly
/// enables the parallel backend.
pub mod rocksdb_persistence_p2b;

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
    //!
    //! Phase 2.B (2026-08-12): [`LedgerStore::open`] and friends now
    //! transparently select between the legacy single-flusher backend
    //! ([`crate::rocksdb_persistence::RocksPersistence`]) and the
    //! parallel-writer backend
    //! ([`crate::rocksdb_persistence_p2b::RocksPersistenceP2b`]) based on
    //! the `ROPE_LEDGER_P2B` env var. Off by default. On-disk format is
    //! identical; either backend can open a DB the other wrote (see the
    //! p2b module docs on watermark recovery).

    use crate::rocksdb_persistence::{
        RecoveredState, RocksError, RocksPersistence, StoredTombstone, WriteOp,
    };
    use crate::rocksdb_persistence_p2b::RocksPersistenceP2b;
    use parking_lot::RwLock;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Return true iff the caller has opted into the Phase 2.B parallel
    /// RocksDB persistence backend via `ROPE_LEDGER_P2B=1|true|yes|on`.
    /// Default is `false` so every existing deployment continues to
    /// use the legacy single-flusher backend without any operator
    /// change. Kept as a free function so tests can inspect the same
    /// contract the constructors use.
    pub fn p2b_backend_enabled() -> bool {
        match std::env::var("ROPE_LEDGER_P2B") {
            Ok(v) => matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            Err(_) => false,
        }
    }

    /// Which persistence backend a given [`LedgerStore`] is bound to.
    /// Constructed lazily by [`LedgerStore::open`] and friends, based
    /// on [`p2b_backend_enabled`]. All disk-touching methods on
    /// `LedgerStore` fan out through this enum, so the two backends
    /// stay isolated behind the same in-memory-mirror API.
    ///
    /// Kept intentionally simple: no trait, no dynamic dispatch, no
    /// `Box<dyn ...>`. The variants are small (single `Arc`), match
    /// arms are cheap, and inlining across the enum keeps the hot
    /// path identical to a direct call.
    pub(crate) enum PersistenceBackend {
        /// Legacy Phase 1.5 / 1.6 single-flusher backend. Every
        /// installation before 2026-08-12 uses this.
        Legacy(Arc<RocksPersistence>),
        /// Phase 2.B parallel-writer backend. Enabled by
        /// `ROPE_LEDGER_P2B=1`. See [`crate::rocksdb_persistence_p2b`].
        Parallel(Arc<RocksPersistenceP2b>),
    }

    impl PersistenceBackend {
        /// Enqueue a write op. Errors (including
        /// [`RocksError::QueueFull`]) are propagated verbatim from the
        /// underlying backend so the ack-after-enqueue contract holds
        /// in both modes.
        pub(crate) fn enqueue(&self, op: WriteOp) -> Result<u64, RocksError> {
            match self {
                PersistenceBackend::Legacy(p) => p.enqueue(op),
                PersistenceBackend::Parallel(p) => p.enqueue(op),
            }
        }

        /// Block until the given sequence number is on durable disk,
        /// or timeout elapses. Semantics identical in both backends:
        /// `wait_durable(seq_returned_by_enqueue)` blocks until every
        /// shard/flusher that received a seq ≥ `seq` has flushed at
        /// least `seq` to disk.
        pub(crate) fn wait_durable(&self, seq: u64, timeout: Duration) -> bool {
            match self {
                PersistenceBackend::Legacy(p) => p.wait_durable(seq, timeout),
                PersistenceBackend::Parallel(p) => p.wait_durable(seq, timeout),
            }
        }

        pub(crate) fn read_string_blob(
            &self,
            string_id: &[u8; 32],
        ) -> Result<Option<Vec<u8>>, RocksError> {
            match self {
                PersistenceBackend::Legacy(p) => p.read_string_blob(string_id),
                PersistenceBackend::Parallel(p) => p.read_string_blob(string_id),
            }
        }

        pub(crate) fn stream_string_blobs<F>(
            &self,
            batch_size: usize,
            sleep_between_batches: Duration,
            handler: F,
        ) -> Result<usize, RocksError>
        where
            F: FnMut(Vec<([u8; 32], Vec<u8>)>) -> Result<(), RocksError>,
        {
            match self {
                PersistenceBackend::Legacy(p) => {
                    p.stream_string_blobs(batch_size, sleep_between_batches, handler)
                }
                PersistenceBackend::Parallel(p) => {
                    p.stream_string_blobs(batch_size, sleep_between_batches, handler)
                }
            }
        }

        /// Debug-only: identifies which backend variant is in use.
        /// Used by `is_persistent_p2b()` and by tests that need to
        /// assert the correct backend was selected without exposing
        /// the enum publicly.
        pub(crate) fn is_p2b(&self) -> bool {
            matches!(self, PersistenceBackend::Parallel(_))
        }
    }

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
        /// Wraps either the legacy single-flusher backend or the Phase 2.B
        /// parallel-writer backend; selection happens at construction time.
        persistence: Option<PersistenceBackend>,
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
        /// Quipu Canon v2.0 Phase 1.5 (legacy backend) / Phase 2.B
        /// (parallel backend). Backend selection is governed by the
        /// `ROPE_LEDGER_P2B` env var (see [`p2b_backend_enabled`]);
        /// on-disk format is identical so either backend can open a
        /// DB the other wrote.
        pub fn open(path: impl AsRef<Path>) -> Result<Self, RocksError> {
            if p2b_backend_enabled() {
                let (persistence, recovered) = RocksPersistenceP2b::open(path)?;
                Ok(Self::from_recovered_p2b(persistence, recovered))
            } else {
                let (persistence, recovered) = RocksPersistence::open(path)?;
                Ok(Self::from_recovered(persistence, recovered))
            }
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
            if p2b_backend_enabled() {
                let (persistence, mut recovered) = RocksPersistenceP2b::open(path)?;
                let blobs = std::mem::take(&mut recovered.string_blobs);
                let tombstones = std::mem::take(&mut recovered.tombstones);
                Ok((
                    Self::from_recovered_p2b(persistence, recovered),
                    blobs,
                    tombstones,
                ))
            } else {
                let (persistence, mut recovered) = RocksPersistence::open(path)?;
                let blobs = std::mem::take(&mut recovered.string_blobs);
                let tombstones = std::mem::take(&mut recovered.tombstones);
                Ok((Self::from_recovered(persistence, recovered), blobs, tombstones))
            }
        }

        /// Phase 1.6.1 (2026-08-11 P1) — like [`Self::open_with_recovery`]
        /// but skips the eager scan of the (potentially huge) `strings`
        /// column family. The returned `blobs` vector is **always
        /// empty**; the caller (typically `LedgerManager`) is expected
        /// to load knot payloads on demand via [`Self::read_string_blob`]
        /// and/or drive a background rehydration pass via
        /// [`Self::stream_string_blobs`].
        ///
        /// Tombstones are still returned eagerly because they are small
        /// (< 200 bytes each) and the lattice needs the complete
        /// tombstone set to reject reads for untied knots at any
        /// callsite. Descriptors, chains, reverse index, piece maps,
        /// and heads are also loaded eagerly — same reason: small
        /// aggregate size + hot-path readers depend on them.
        ///
        /// Why this exists: at 532K knots the eager blob load costs
        /// ~5 min of CPU + ~4.5 GB of RSS at boot, blowing the systemd
        /// cgroup memory ceiling and crash-looping the service before
        /// the RPC listener binds. See §11 of the 2026-08-11 handover.
        pub fn open_with_recovery_lazy(
            path: impl AsRef<Path>,
        ) -> Result<(Self, Vec<([u8; 32], Vec<u8>)>, Vec<([u8; 32], StoredTombstone)>), RocksError>
        {
            if p2b_backend_enabled() {
                let (persistence, mut recovered) = RocksPersistenceP2b::open_lazy(path)?;
                debug_assert!(
                    recovered.string_blobs.is_empty(),
                    "open_lazy must not eagerly load string_blobs (p2b)"
                );
                let blobs = std::mem::take(&mut recovered.string_blobs);
                let tombstones = std::mem::take(&mut recovered.tombstones);
                Ok((
                    Self::from_recovered_p2b(persistence, recovered),
                    blobs,
                    tombstones,
                ))
            } else {
                let (persistence, mut recovered) = RocksPersistence::open_lazy(path)?;
                debug_assert!(
                    recovered.string_blobs.is_empty(),
                    "open_lazy must not eagerly load string_blobs"
                );
                let blobs = std::mem::take(&mut recovered.string_blobs);
                let tombstones = std::mem::take(&mut recovered.tombstones);
                Ok((Self::from_recovered(persistence, recovered), blobs, tombstones))
            }
        }

        /// Build a store from an already-opened legacy persistence handle
        /// and a recovery snapshot. Useful when the caller wants to share
        /// one `RocksPersistence` across multiple stores or to inspect the
        /// snapshot before constructing the store.
        pub fn from_recovered(
            persistence: Arc<RocksPersistence>,
            recovered: RecoveredState,
        ) -> Self {
            Self::from_recovered_inner(
                PersistenceBackend::Legacy(persistence),
                recovered,
            )
        }

        /// Phase 2.B — build a store from an already-opened parallel
        /// persistence handle and a recovery snapshot. Mirror image of
        /// [`Self::from_recovered`] for the parallel backend.
        pub fn from_recovered_p2b(
            persistence: Arc<RocksPersistenceP2b>,
            recovered: RecoveredState,
        ) -> Self {
            Self::from_recovered_inner(
                PersistenceBackend::Parallel(persistence),
                recovered,
            )
        }

        /// Internal constructor shared by both backend variants. Rebuilds
        /// the in-memory mirror (descriptors, heads, chains, reverse
        /// index, piece maps, per-wallet append counters) from the
        /// `RecoveredState` snapshot and wraps the caller-supplied
        /// backend enum.
        fn from_recovered_inner(
            persistence: PersistenceBackend,
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

        /// Phase 2.B (2026-08-12) — true iff this store is running on
        /// the parallel-writer backend. Returns `false` for in-memory
        /// stores and for legacy-backend stores. Intended for tests,
        /// metrics endpoints, and operator diagnostics — the RPC hot
        /// path is backend-agnostic.
        pub fn is_persistent_p2b(&self) -> bool {
            self.persistence.as_ref().map(|p| p.is_p2b()).unwrap_or(false)
        }

        /// Phase 1.6.1 (2026-08-11 P1) — direct point-read of a
        /// serialised RopeString blob from disk. Returns `None` if the
        /// id was never persisted OR if the store is in in-memory mode
        /// (no disk backend to read from).
        ///
        /// Callers deserialise the blob themselves — the store treats
        /// blobs as opaque `Vec<u8>` because `RopeString` lives in
        /// `rope-core` and rope-storage must remain schema-agnostic.
        pub fn read_string_blob(
            &self,
            string_id: &[u8; 32],
        ) -> Result<Option<Vec<u8>>, RocksError> {
            match &self.persistence {
                None => Ok(None),
                Some(p) => p.read_string_blob(string_id),
            }
        }

        /// Phase 1.6.1 (2026-08-11 P1) — stream every persisted
        /// RopeString blob in fixed-size batches, sleeping
        /// `sleep_between_batches` between batches to bound RSS and
        /// disk I/O contention. Returns the total number of blobs
        /// streamed. No-op (returns 0) in in-memory mode.
        ///
        /// Designed for the background rehydration task: the caller
        /// wakes up every batch, hands the batch to the lattice
        /// restorer, and then sleeps briefly so a fresh boot never
        /// spikes past its cgroup memory ceiling.
        pub fn stream_string_blobs<F>(
            &self,
            batch_size: usize,
            sleep_between_batches: Duration,
            handler: F,
        ) -> Result<usize, RocksError>
        where
            F: FnMut(Vec<([u8; 32], Vec<u8>)>) -> Result<(), RocksError>,
        {
            match &self.persistence {
                None => Ok(0),
                Some(p) => p.stream_string_blobs(batch_size, sleep_between_batches, handler),
            }
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
        /// store-level `last_enqueued_seq`. In-memory mode is a no-op
        /// success (`Ok(0)`).
        ///
        /// 2026-07-27: errors (including [`RocksError::QueueFull`]) are
        /// propagated — never swallowed. Ack-after-enqueue must not
        /// report success when the flusher queue rejected the write.
        fn enqueue(&self, op: WriteOp) -> Result<u64, RocksError> {
            match &self.persistence {
                None => Ok(0),
                Some(p) => {
                    let seq = p.enqueue(op)?;
                    self.last_enqueued_seq.fetch_max(seq, Ordering::AcqRel);
                    Ok(seq)
                }
            }
        }

        pub fn put_descriptor(
            &self,
            wallet: &[u8],
            desc: StoredLedgerDescriptor,
        ) -> Result<u64, RocksError> {
            // Enqueue first so a full queue never mutates the mirror
            // under a false "accepted" RPC.
            let seq = self.enqueue(WriteOp::PutDescriptor {
                wallet: wallet.to_vec(),
                desc: desc.clone(),
            })?;
            self.head_index
                .write()
                .insert(wallet.to_vec(), desc.head_string_id);
            self.descriptors
                .write()
                .insert(wallet.to_vec(), desc);
            Ok(seq)
        }

        pub fn get_descriptor(&self, wallet: &[u8]) -> Option<StoredLedgerDescriptor> {
            self.descriptors.read().get(wallet).cloned()
        }

        pub fn append_to_chain(
            &self,
            wallet: &[u8],
            string_id: [u8; 32],
        ) -> Result<u64, RocksError> {
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

            let seq = self.enqueue(WriteOp::AppendChain {
                wallet: wallet.to_vec(),
                seq_in_wallet,
                string_id,
            })?;
            self.wallet_to_chain
                .write()
                .entry(wallet.to_vec())
                .or_default()
                .push(string_id);
            self.string_to_wallet
                .write()
                .insert(string_id, wallet.to_vec());
            self.head_index.write().insert(wallet.to_vec(), string_id);
            Ok(seq)
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
        pub fn put_string_blob(
            &self,
            string_id: [u8; 32],
            blob: Vec<u8>,
        ) -> Result<u64, RocksError> {
            self.enqueue(WriteOp::PutStringBlob { string_id, blob })
        }

        /// Phase 1.6 — cryptographic erasure on disk: delete a knot's
        /// payload blob. Used by both the whole-string erase pathway
        /// and the per-knot untie pathway.
        pub fn delete_string_blob(&self, string_id: [u8; 32]) -> Result<u64, RocksError> {
            self.enqueue(WriteOp::DeleteStringBlob { string_id })
        }

        /// Phase 1.6 — persist a canon v1.1 §4.2 untie-tombstone so
        /// the deliberate-absence record survives restarts.
        pub fn put_tombstone(
            &self,
            string_id: [u8; 32],
            tombstone: StoredTombstone,
        ) -> Result<u64, RocksError> {
            self.enqueue(WriteOp::PutTombstone {
                string_id,
                tombstone,
            })
        }

        pub fn put_piece_map(
            &self,
            string_id: [u8; 32],
            map: StoredPieceMap,
        ) -> Result<u64, RocksError> {
            let seq = self.enqueue(WriteOp::PutPieceMap {
                string_id,
                piece_map: map.clone(),
            })?;
            self.piece_maps.write().insert(string_id, map);
            Ok(seq)
        }

        pub fn get_piece_map(&self, string_id: &[u8; 32]) -> Option<StoredPieceMap> {
            self.piece_maps.read().get(string_id).cloned()
        }

        pub fn mark_deleted(&self, wallet: &[u8]) -> Result<bool, RocksError> {
            let now = chrono::Utc::now().timestamp();
            // Capture a would-be-updated desc, enqueue first, then flip
            // the mirror — same enqueue-before-mirror rule as
            // [`Self::put_descriptor`].
            let updated_desc: Option<StoredLedgerDescriptor> = {
                let descs = self.descriptors.read();
                descs.get(wallet).map(|desc| {
                    let mut d = desc.clone();
                    d.is_deleted = true;
                    d.deleted_at = Some(now);
                    d
                })
            };
            match updated_desc {
                None => Ok(false),
                Some(desc) => {
                    self.enqueue(WriteOp::PutDescriptor {
                        wallet: wallet.to_vec(),
                        desc: desc.clone(),
                    })?;
                    let mut descs = self.descriptors.write();
                    if let Some(live) = descs.get_mut(wallet) {
                        live.is_deleted = true;
                        live.deleted_at = Some(now);
                    }
                    Ok(true)
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
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, head)).unwrap();
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
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, sids[0])).unwrap();
                for sid in &sids {
                    s.append_to_chain(&wallet, *sid).unwrap();
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
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, head)).unwrap();
                assert!(s.mark_deleted(&wallet).unwrap());
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
                s.put_piece_map(sid, pm.clone()).unwrap();
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
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, [0u8; 32])).unwrap();

                let mut handles = Vec::new();
                for tid in 0..THREADS as u8 {
                    let s = s.clone();
                    let wallet = wallet.clone();
                    handles.push(thread::spawn(move || {
                        for i in 0..APPENDS_PER_THREAD {
                            let mut sid = [0u8; 32];
                            sid[0] = tid;
                            sid[1] = i as u8;
                            s.append_to_chain(&wallet, sid).unwrap();
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
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, sid)).unwrap();
                s.append_to_chain(&wallet, sid).unwrap();
                // No await_all_durable — let Drop final-drain.
            }

            let s2 = LedgerStore::open(&path).unwrap();
            assert_eq!(s2.head_for_wallet(&wallet), Some(sid));
            assert_eq!(s2.get_chain(&wallet), vec![sid]);
        }

        // ==== Phase 1.6.1 — lazy rehydration API ====

        #[test]
        fn lazy_open_returns_empty_blob_vec_but_keeps_data_on_disk() {
            // Write a batch of string blobs eagerly (via the normal
            // put_string_blob path), then re-open in lazy mode and
            // confirm (a) the eager blob vec is empty, (b) the blobs
            // are still readable point-by-point on demand.
            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            let mut expected: Vec<([u8; 32], Vec<u8>)> = Vec::new();
            {
                let s = LedgerStore::open(&path).unwrap();
                for i in 0u8..8 {
                    let mut sid = [0u8; 32];
                    sid[0] = i;
                    // Distinct payload per blob so we can catch swaps.
                    let payload = vec![0xAA, i, 0xBB, i, 0xCC];
                    s.put_string_blob(sid, payload.clone()).unwrap();
                    expected.push((sid, payload));
                }
                assert!(s.await_all_durable(Duration::from_secs(2)));
            }

            let (s2, blobs, tombstones) =
                LedgerStore::open_with_recovery_lazy(&path).unwrap();
            assert!(
                blobs.is_empty(),
                "lazy recovery must NOT eagerly load knot payloads"
            );
            assert!(
                tombstones.is_empty(),
                "no tombstones were written in this test"
            );
            for (sid, payload) in &expected {
                let got = s2
                    .read_string_blob(sid)
                    .unwrap()
                    .expect("blob must still be on disk after lazy open");
                assert_eq!(&got, payload, "point-read must return the exact payload");
            }
            // A missing id must return None (not an error).
            let missing = [0xFFu8; 32];
            assert!(s2.read_string_blob(&missing).unwrap().is_none());
        }

        #[test]
        fn stream_string_blobs_visits_every_blob_in_batches() {
            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            const N: usize = 47; // deliberately not a multiple of batch_size
            const BATCH: usize = 10;
            let mut expected: std::collections::HashMap<[u8; 32], Vec<u8>> =
                std::collections::HashMap::new();
            {
                let s = LedgerStore::open(&path).unwrap();
                for i in 0..N {
                    let mut sid = [0u8; 32];
                    sid[0] = (i / 256) as u8;
                    sid[1] = (i % 256) as u8;
                    let payload = vec![i as u8; 16];
                    s.put_string_blob(sid, payload.clone()).unwrap();
                    expected.insert(sid, payload);
                }
                assert!(s.await_all_durable(Duration::from_secs(2)));
            }

            let (s2, _blobs, _tombstones) =
                LedgerStore::open_with_recovery_lazy(&path).unwrap();
            let mut seen: std::collections::HashMap<[u8; 32], Vec<u8>> =
                std::collections::HashMap::new();
            let mut batches_seen = 0usize;
            let mut max_batch_len = 0usize;
            let total = s2
                .stream_string_blobs(BATCH, Duration::ZERO, |batch| {
                    batches_seen += 1;
                    max_batch_len = max_batch_len.max(batch.len());
                    for (sid, payload) in batch {
                        seen.insert(sid, payload);
                    }
                    Ok(())
                })
                .unwrap();
            assert_eq!(total, N, "stream must visit every persisted blob");
            assert_eq!(seen.len(), N);
            assert_eq!(seen, expected, "each blob must round-trip byte-for-byte");
            assert!(
                max_batch_len <= BATCH,
                "no batch may exceed the requested batch_size"
            );
            // 47 / 10 = 4 full batches + 1 remainder = 5 batches
            assert_eq!(batches_seen, 5);
        }

        #[test]
        fn stream_string_blobs_on_empty_store_returns_zero() {
            let dir = TempDir::new().unwrap();
            let (s, _blobs, _tomb) =
                LedgerStore::open_with_recovery_lazy(dir.path()).unwrap();
            let mut invoked = false;
            let total = s
                .stream_string_blobs(64, Duration::ZERO, |_batch| {
                    invoked = true;
                    Ok(())
                })
                .unwrap();
            assert_eq!(total, 0);
            assert!(
                !invoked,
                "handler must not be called when there are zero blobs"
            );
        }

        #[test]
        fn in_memory_store_lazy_apis_are_noops() {
            let s = LedgerStore::new();
            let sid = [0xAAu8; 32];
            assert!(s.read_string_blob(&sid).unwrap().is_none());
            let total = s
                .stream_string_blobs(8, Duration::ZERO, |_batch| Ok(()))
                .unwrap();
            assert_eq!(total, 0);
        }
    }

    /// Phase 2.B backend routing tests. These exercise the parallel-writer
    /// backend through the same public [`LedgerStore`] surface every prod
    /// call site already uses, and additionally through the direct
    /// `from_recovered_p2b` construction path that bypasses the env-var
    /// check (so tests never mutate process-global `ROPE_LEDGER_P2B`, which
    /// would race with other parallel tests).
    ///
    /// The invariant under test: whichever backend `LedgerStore` is bound
    /// to, the semantics on the in-memory mirror + on-disk state must be
    /// identical (same descriptors, same chain order, same tombstones,
    /// same lazy-blob API) as the legacy backend. If a future refactor
    /// silently drops a code path in only one backend, one of these tests
    /// will catch it.
    #[cfg(test)]
    mod ledger_store_p2b_tests {
        use crate::ledger_db::{LedgerStore, StoredLedgerDescriptor};
        use crate::rocksdb_persistence_p2b::RocksPersistenceP2b;
        use std::sync::Arc;
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

        /// Build a `LedgerStore` explicitly wired to the Phase 2.B backend
        /// using the same on-disk recovery pipeline the env-flagged
        /// constructor uses, but without touching `ROPE_LEDGER_P2B`.
        fn open_p2b(path: &std::path::Path) -> LedgerStore {
            let (persistence, recovered): (Arc<RocksPersistenceP2b>, _) =
                RocksPersistenceP2b::open(path).expect("open p2b persistence");
            LedgerStore::from_recovered_p2b(persistence, recovered)
        }

        #[test]
        fn p2b_backend_reports_as_persistent_and_p2b() {
            let dir = TempDir::new().unwrap();
            let s = open_p2b(dir.path());
            assert!(s.is_persistent(), "p2b backend is a persistent backend");
            assert!(
                s.is_persistent_p2b(),
                "is_persistent_p2b must be true when backend is Parallel"
            );
        }

        #[test]
        fn legacy_backend_does_not_report_as_p2b() {
            // The default env-off path must go to the Legacy backend and
            // is_persistent_p2b must be false there. We rely on the
            // default-off behaviour of `ROPE_LEDGER_P2B` — if a soak
            // machine exported the env var, this assertion would flip
            // and we'd want to catch that in CI.
            let dir = TempDir::new().unwrap();
            let s = LedgerStore::open(dir.path()).unwrap();
            assert!(s.is_persistent());
            assert!(
                !s.is_persistent_p2b(),
                "LedgerStore::open with ROPE_LEDGER_P2B unset must route to Legacy"
            );
        }

        #[test]
        fn p2b_descriptor_roundtrips_through_disk() {
            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            let wallet = vec![0x11u8; 20];
            let head = [0x22u8; 32];

            {
                let s = open_p2b(&path);
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, head))
                    .unwrap();
                assert!(
                    s.await_all_durable(Duration::from_secs(2)),
                    "await_all_durable must return true on the p2b backend"
                );
            }

            // Reopen (still on p2b — same fn) and confirm the descriptor
            // came back through the sharded flushers + shard-scoped
            // watermarks.
            let s2 = open_p2b(&path);
            let d = s2
                .get_descriptor(&wallet)
                .expect("descriptor recovered from p2b backend");
            assert_eq!(d.head_string_id, head);
            assert_eq!(s2.total_count(), 1);
            assert_eq!(s2.active_count(), 1);
            assert_eq!(s2.head_for_wallet(&wallet), Some(head));
        }

        #[test]
        fn p2b_chain_survives_recovery_in_order() {
            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            let wallet = vec![0x33u8; 20];
            let mut sids = Vec::new();
            for i in 0u8..24 {
                let mut sid = [0u8; 32];
                sid[0] = i;
                sids.push(sid);
            }

            {
                let s = open_p2b(&path);
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, sids[0]))
                    .unwrap();
                for sid in &sids {
                    s.append_to_chain(&wallet, *sid).unwrap();
                }
                assert!(s.await_all_durable(Duration::from_secs(5)));
                assert_eq!(s.get_chain(&wallet), sids);
            }

            let s2 = open_p2b(&path);
            let recovered = s2.get_chain(&wallet);
            assert_eq!(
                recovered, sids,
                "p2b chain order must survive recovery across sharded flushers"
            );
            assert_eq!(s2.head_for_wallet(&wallet), Some(*sids.last().unwrap()));
        }

        #[test]
        fn p2b_concurrent_appends_across_wallets_never_lose_writes() {
            // Multiple wallets, multiple threads. Under Phase 2.B each
            // wallet's writes land on the shard chosen by its 20-byte
            // address, so cross-wallet appends fan out across shards in
            // parallel. Every append must survive both wait_durable and
            // full-restart recovery.
            use std::thread;

            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            const WALLETS: u8 = 12;
            const APPENDS_PER_WALLET: usize = 32;

            {
                let s = Arc::new(open_p2b(&path));
                // Seed every wallet with a descriptor first.
                for w in 0..WALLETS {
                    let wallet = vec![w; 20];
                    s.put_descriptor(&wallet, dummy_descriptor(&wallet, [0u8; 32]))
                        .unwrap();
                }

                let mut handles = Vec::new();
                for w in 0..WALLETS {
                    let s = s.clone();
                    handles.push(thread::spawn(move || {
                        let wallet = vec![w; 20];
                        for i in 0..APPENDS_PER_WALLET {
                            let mut sid = [0u8; 32];
                            sid[0] = w;
                            sid[1] = i as u8;
                            s.append_to_chain(&wallet, sid).unwrap();
                        }
                    }));
                }
                for h in handles {
                    h.join().unwrap();
                }
                assert!(
                    s.await_all_durable(Duration::from_secs(10)),
                    "every shard's per-shard watermark must land in the timeout"
                );
                for w in 0..WALLETS {
                    let wallet = vec![w; 20];
                    assert_eq!(
                        s.get_chain(&wallet).len(),
                        APPENDS_PER_WALLET,
                        "in-memory mirror must match per-wallet append count"
                    );
                }
            }

            // Full-restart recovery on p2b: nothing may be lost, and
            // per-wallet order must be preserved (each wallet's writes
            // go to a single deterministic shard, so this transitively
            // proves shard-recovery ordering).
            let s2 = open_p2b(&path);
            for w in 0..WALLETS {
                let wallet = vec![w; 20];
                let chain = s2.get_chain(&wallet);
                assert_eq!(
                    chain.len(),
                    APPENDS_PER_WALLET,
                    "no writes may be dropped for wallet {} after p2b recovery",
                    w
                );
                for (i, sid) in chain.iter().enumerate() {
                    assert_eq!(sid[0], w, "sid[0] tag must be preserved");
                    assert_eq!(
                        sid[1], i as u8,
                        "sid[1] index must reflect append order for wallet {}",
                        w
                    );
                }
            }
        }

        #[test]
        fn p2b_and_legacy_produce_the_same_on_disk_state_for_same_ops() {
            // Cross-backend equivalence smoke: run the identical operation
            // sequence through Legacy and through P2B, then reopen each
            // with its own backend and assert the recovered view is
            // pointwise identical. If a future refactor drops or reorders
            // any WriteOp variant on only one side, this catches it.
            let dir_legacy = TempDir::new().unwrap();
            let dir_p2b = TempDir::new().unwrap();
            let wallet_a = vec![0x55u8; 20];
            let wallet_b = vec![0x66u8; 20];
            let head_a = [0xA1u8; 32];
            let head_b = [0xB2u8; 32];

            for path in [dir_legacy.path(), dir_p2b.path()] {
                let s = if path == dir_p2b.path() {
                    open_p2b(path)
                } else {
                    LedgerStore::open(path).unwrap()
                };
                s.put_descriptor(&wallet_a, dummy_descriptor(&wallet_a, head_a))
                    .unwrap();
                s.put_descriptor(&wallet_b, dummy_descriptor(&wallet_b, head_b))
                    .unwrap();
                for i in 0..5u8 {
                    let mut sid = [0u8; 32];
                    sid[0] = i;
                    s.append_to_chain(&wallet_a, sid).unwrap();
                }
                for i in 0..3u8 {
                    let mut sid = [0u8; 32];
                    sid[0] = 0x80 | i;
                    s.append_to_chain(&wallet_b, sid).unwrap();
                }
                assert!(s.await_all_durable(Duration::from_secs(5)));
            }

            let s_legacy = LedgerStore::open(dir_legacy.path()).unwrap();
            let s_p2b = open_p2b(dir_p2b.path());

            assert_eq!(s_legacy.get_chain(&wallet_a), s_p2b.get_chain(&wallet_a));
            assert_eq!(s_legacy.get_chain(&wallet_b), s_p2b.get_chain(&wallet_b));
            assert_eq!(
                s_legacy.head_for_wallet(&wallet_a),
                s_p2b.head_for_wallet(&wallet_a)
            );
            assert_eq!(
                s_legacy.head_for_wallet(&wallet_b),
                s_p2b.head_for_wallet(&wallet_b)
            );
            assert_eq!(s_legacy.total_count(), s_p2b.total_count());
            assert_eq!(s_legacy.active_count(), s_p2b.active_count());
        }

        #[test]
        fn p2b_lazy_open_returns_empty_blob_vec_but_keeps_data_on_disk() {
            // Mirror image of the legacy `lazy_open_returns_empty_blob_vec_but_keeps_data_on_disk`
            // test, but through the parallel backend's lazy open path.
            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            let mut expected: Vec<([u8; 32], Vec<u8>)> = Vec::new();
            {
                let s = open_p2b(&path);
                for i in 0u8..8 {
                    let mut sid = [0u8; 32];
                    sid[0] = i;
                    let payload = vec![0xAA, i, 0xBB, i, 0xCC];
                    s.put_string_blob(sid, payload.clone()).unwrap();
                    expected.push((sid, payload));
                }
                assert!(s.await_all_durable(Duration::from_secs(2)));
            }

            // Directly open in p2b lazy mode (bypassing the env var).
            let (persistence, recovered) =
                RocksPersistenceP2b::open_lazy(&path).unwrap();
            assert!(
                recovered.string_blobs.is_empty(),
                "p2b lazy recovery must NOT eagerly load knot payloads"
            );
            assert!(recovered.tombstones.is_empty());
            let s2 = LedgerStore::from_recovered_p2b(persistence, recovered);
            for (sid, payload) in &expected {
                let got = s2
                    .read_string_blob(sid)
                    .unwrap()
                    .expect("blob must still be on disk after p2b lazy open");
                assert_eq!(&got, payload);
            }
            let missing = [0xFFu8; 32];
            assert!(s2.read_string_blob(&missing).unwrap().is_none());
        }

        #[test]
        fn p2b_unawaited_writes_survive_drop_via_final_drain() {
            // Every shard's Drop impl must final-drain its channel before
            // closing the shared DB handle. A fast-path append followed
            // by an immediate drop must not lose the write, on ANY
            // shard the wallet maps to.
            let dir = TempDir::new().unwrap();
            let path = dir.path().to_path_buf();
            let wallet = vec![0x77u8; 20];
            let sid = [0xCDu8; 32];

            {
                let s = open_p2b(&path);
                s.put_descriptor(&wallet, dummy_descriptor(&wallet, sid))
                    .unwrap();
                s.append_to_chain(&wallet, sid).unwrap();
                // No await — rely on shard Drop to final-drain.
            }

            let s2 = open_p2b(&path);
            assert_eq!(s2.head_for_wallet(&wallet), Some(sid));
            assert_eq!(s2.get_chain(&wallet), vec![sid]);
        }
    }
}
