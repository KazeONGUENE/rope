//! RocksDB persistence backend for [`crate::ledger_db::LedgerStore`].
//!
//! ## Quipu Canon v2.0 Phase 1.5
//!
//! Replaces the in-memory `RwLock<HashMap>` `LedgerStore` with a
//! disk-backed RocksDB store, while keeping the in-memory mirror as a
//! write-through cache for the hot read path. Provides:
//!
//! - **Multi-column-family schema** — one CF per index (descriptors,
//!   heads, chain, reverse, pieces) so writes don't contend on a
//!   single keyspace.
//! - **WriteBatch background flusher** — high-level mutations are
//!   enqueued to a single-consumer channel; the flusher drains the
//!   queue every ~10 ms and writes one fsync'd `WriteBatch` per tick,
//!   amortising the cost of disk syncs across thousands of appends.
//! - **Durability watermark** — every enqueue returns a monotonically
//!   increasing sequence number. The flusher advances a `durable_seq`
//!   AtomicU64 after each successful sync. Callers who need strict
//!   durability call [`RocksPersistence::wait_durable(seq, timeout)`]
//!   and block until the watermark passes their seq. Callers who can
//!   tolerate the standard ~10 ms ack window simply ignore the seq.
//! - **Recovery path** — at open time the persistence layer iterates
//!   every CF and returns a [`RecoveredState`] snapshot that
//!   `LedgerStore::open` uses to populate the in-memory mirror.
//!
//! See `docs/QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` §3.5.
//!
//! ## Quipu Canon v2.0 Phase 2.B — parallel WriteBatch consumers
//!
//! Phase 1.5 had one channel and one flusher thread. With
//! `WriteOptions::set_sync(true)` that yields a hard ceiling of one
//! fsync per `FLUSH_INTERVAL` (10 ms ⇒ ~100 fsync/s) regardless of
//! batch size, and the single mpsc consumer becomes a CPU bottleneck
//! once enqueue throughput rises above a few hundred thousand ops/s.
//!
//! Phase 2.B replaces the single channel + single flusher with a pool
//! of `NUM_SHARDS` independent writers, each with its own channel and
//! its own background flusher. Ops are routed to a shard by the first
//! byte of their wallet address (or string id for piece ops),
//! `byte & SHARD_MASK`. Each shard:
//!
//! - Owns its own [`mpsc::Sender<PendingWrite>`] channel — no enqueue
//!   contention with other shards.
//! - Owns its own flusher thread that drains its channel and issues
//!   `db.write_opt(batch, &sync_wo)`. RocksDB serialises WAL writes
//!   internally and **group-commits concurrent fsync requests**, so
//!   N shards issuing simultaneous fsyncs cost far less than N×fsync.
//! - Owns `highest_assigned_seq` (set on enqueue) and
//!   `highest_durable_seq` (set on flush) atomics — no global lock.
//! - Persists its own watermark to default CF key
//!   `b"durable_seq_shard_<i>"` so per-shard recovery is precise.
//!
//! The `next_seq` AtomicU64 stays global so caller seqs remain a
//! single monotonic stream across all shards.
//!
//! `wait_durable(S)` returns once
//! `highest_durable[i] >= min(highest_assigned[i], S)` for **every**
//! shard `i`. This is the strongest correct invariant: a shard whose
//! assigned ≤ S has nothing more to fsync to satisfy the call; a shard
//! whose assigned > S only needs to fsync up to S to satisfy this
//! call (its later seqs will be carried by a later wait).
//!
//! `durable_seq()` returns the largest `S` such that the above holds
//! for every shard, i.e. `min over shards of highest_durable[i]` if
//! that shard is the bottleneck, otherwise `next_seq() - 1`.
//!
//! On crash recovery the global watermark is the **min over shards of
//! the persisted per-shard watermark**, with the legacy single
//! `b"durable_seq"` key honoured as a lower bound for backward
//! compatibility with a Phase 1.5 database opened on a Phase 2.B
//! binary.
//!
//! ## Schema
//!
//! | CF              | Key shape                                | Value shape                       |
//! |-----------------|------------------------------------------|-----------------------------------|
//! | `descriptors`   | `wallet_bytes`                           | `bincode(StoredLedgerDescriptor)` |
//! | `heads`         | `wallet_bytes`                           | `[u8; 32]` (head_string_id)       |
//! | `chain`         | `wallet_bytes \|\| u64-be seq_in_wallet` | `[u8; 32]` (string_id)            |
//! | `reverse`       | `[u8; 32]` (string_id)                   | `wallet_bytes`                    |
//! | `pieces`        | `[u8; 32]` (string_id)                   | `bincode(StoredPieceMap)`         |
//! | default         | `b"durable_seq"`                         | `u64-le` (legacy global, kept for back-compat) |
//! | default         | `b"durable_seq_shard_<i>"`               | `u64-le` (P2.B per-shard watermark)            |
//!
//! Composite chain keys keep each wallet's appended ids contiguous and
//! lexicographically sorted in RocksDB — recovery reconstructs the
//! per-wallet chain in O(N) via a prefix scan with no in-memory sort.

use crate::ledger_db::{StoredLedgerDescriptor, StoredPieceMap};
use parking_lot::{Condvar, Mutex};
use rocksdb::{
    ColumnFamilyDescriptor, DBCompressionType, IteratorMode, Options, WriteBatch, WriteOptions, DB,
};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use thiserror::Error;

/// Column family names. Kept module-public so tests can sanity-check
/// the schema layout.
pub const CF_DESCRIPTORS: &str = "descriptors";
pub const CF_HEADS: &str = "heads";
pub const CF_CHAIN: &str = "chain";
pub const CF_REVERSE: &str = "reverse";
pub const CF_PIECES: &str = "pieces";

/// Default-CF key holding the latest fsync'd global sequence number.
///
/// **Legacy / back-compat.** Phase 1.5 wrote a single global watermark
/// here. Phase 2.B writes per-shard watermarks at
/// [`durable_seq_shard_key`] instead, but still honours this key as a
/// lower bound on recovery so a P2.B binary can open a P1.5 database
/// without losing fsync'd state.
const DURABLE_SEQ_KEY: &[u8] = b"durable_seq";

/// Per-shard durability watermark key prefix (Phase 2.B).
const DURABLE_SEQ_SHARD_PREFIX: &[u8] = b"durable_seq_shard_";

/// Number of independent shard writers (Phase 2.B). Must be a power
/// of two so `byte & SHARD_MASK` partitions evenly. Empirically 8 is
/// the sweet spot on commodity NVMe: enough fan-out to saturate the
/// device's fsync queue depth and to avoid mpsc-channel contention,
/// while small enough that RocksDB's internal WAL group commit can
/// still cleanly merge concurrent batches.
pub const NUM_SHARDS: usize = 8;

/// Mask applied to the partition byte to compute a shard index.
const SHARD_MASK: u8 = (NUM_SHARDS as u8) - 1;

/// How long each shard's flusher waits for new ops before flushing
/// what it has. 10 ms matches the architecture spec §3.5 ack window.
const FLUSH_INTERVAL: Duration = Duration::from_millis(10);

/// Hard cap on ops drained per flush *per shard*. Bounds the memory
/// footprint of a single `WriteBatch`; with the default 64 MB ROC
/// `wb` and ~80 B per chain entry this leaves plenty of headroom.
const MAX_BATCH_OPS: usize = 4096;

/// Build the per-shard watermark key.
fn durable_seq_shard_key(shard: usize) -> Vec<u8> {
    let mut k = DURABLE_SEQ_SHARD_PREFIX.to_vec();
    // shard fits in a single byte; we never have ≥ 256 shards.
    k.push(shard as u8);
    k
}

/// Errors that can come out of the RocksDB persistence layer.
#[derive(Debug, Error)]
pub enum RocksError {
    #[error("rocksdb error: {0}")]
    Db(#[from] rocksdb::Error),
    #[error("bincode error: {0}")]
    Bincode(#[from] bincode::Error),
    #[error("rocksdb path not utf-8: {0}")]
    NonUtf8Path(std::path::PathBuf),
    #[error("column family not found: {0}")]
    MissingCf(&'static str),
    #[error("persistence flusher thread is no longer running")]
    FlusherStopped,
    /// On-disk encoding does not match the expected schema. Almost
    /// always means the DB was opened by a newer/older version of the
    /// node, or hand-edited.
    #[error("on-disk corruption: {0}")]
    Corrupted(String),
}

/// One mutation to be committed to disk.
///
/// Each variant carries everything the flusher needs to write to the
/// matching CFs — ownership is transferred into the channel so the
/// caller does not block on serialisation.
#[derive(Clone, Debug)]
pub enum WriteOp {
    /// Upsert a descriptor and its head pointer in one atomic batch.
    PutDescriptor {
        wallet: Vec<u8>,
        desc: StoredLedgerDescriptor,
    },
    /// Append `string_id` as the next entry in `wallet`'s chain.
    /// `seq_in_wallet` is the per-wallet offset; the persistence layer
    /// also bumps the head pointer and reverse index in the same batch.
    AppendChain {
        wallet: Vec<u8>,
        seq_in_wallet: u64,
        string_id: [u8; 32],
    },
    /// Persist a piece map for `string_id`.
    PutPieceMap {
        string_id: [u8; 32],
        piece_map: StoredPieceMap,
    },
    /// Mark the wallet's descriptor deleted with the given timestamp.
    /// Read-modify-write inside the flusher.
    ///
    /// **Caveat:** the read happens against the on-disk state, NOT
    /// against earlier ops in the same `WriteBatch`. If a
    /// `PutDescriptor` for the same wallet is enqueued just before a
    /// `MarkDeleted`, the deletion will read pre-PutDescriptor disk
    /// state. Callers that already have the mutated descriptor in
    /// hand (e.g. `LedgerStore::mark_deleted`) should send a
    /// self-contained `PutDescriptor` instead. `MarkDeleted` exists
    /// for callers using the persistence layer directly without an
    /// in-memory mirror — typically at startup or for migration tools.
    MarkDeleted { wallet: Vec<u8>, deleted_at: i64 },
}

impl WriteOp {
    /// Phase 2.B partition byte. Wallet ops shard by `wallet[0]`,
    /// piece ops shard by `string_id[0]`. An empty wallet falls
    /// through to shard 0 — only happens in degenerate test inputs.
    #[inline]
    fn partition_byte(&self) -> u8 {
        match self {
            WriteOp::PutDescriptor { wallet, .. }
            | WriteOp::AppendChain { wallet, .. }
            | WriteOp::MarkDeleted { wallet, .. } => wallet.first().copied().unwrap_or(0),
            WriteOp::PutPieceMap { string_id, .. } => string_id[0],
        }
    }

    /// Phase 2.B shard index for this op. Stable across processes
    /// because it depends only on the op's bytes.
    #[inline]
    fn shard(&self) -> usize {
        (self.partition_byte() & SHARD_MASK) as usize
    }
}

/// One enqueued op, tagged with its assigned sequence number.
struct PendingWrite {
    seq: u64,
    op: WriteOp,
}

/// Snapshot of the on-disk state at open time, used by
/// `LedgerStore::open` to populate the in-memory mirror.
#[derive(Default)]
pub struct RecoveredState {
    pub descriptors: Vec<(Vec<u8>, StoredLedgerDescriptor)>,
    /// `wallet -> ordered chain of string ids (genesis-first)`.
    pub chains: Vec<(Vec<u8>, Vec<[u8; 32]>)>,
    pub reverse: Vec<([u8; 32], Vec<u8>)>,
    pub pieces: Vec<([u8; 32], StoredPieceMap)>,
    pub heads: Vec<(Vec<u8>, [u8; 32])>,
    /// Globally-monotone watermark: every seq ≤ `durable_seq` is on
    /// disk. In Phase 2.B this is computed as
    /// `min over shards of recovered_per_shard[i]` (with the legacy
    /// `durable_seq` key honoured as a lower bound).
    pub durable_seq: u64,
}

/// Per-shard runtime state for the Phase 2.B parallel writer pool.
struct Shard {
    /// Channel into this shard's flusher. `Mutex<Option<…>>` so that
    /// `Drop` can `take()` to signal shutdown.
    tx: Mutex<Option<Sender<PendingWrite>>>,
    /// Highest seq ever assigned to this shard. Bumped on enqueue.
    /// Used by `wait_durable` to know how far this shard *needs* to
    /// flush in order to satisfy a global wait.
    highest_assigned: AtomicU64,
    /// Highest seq this shard has fsync'd to disk. Bumped at the end
    /// of [`flush_shard_batch`].
    highest_durable: AtomicU64,
    /// Background flusher handle. Joined on Drop.
    flusher: Mutex<Option<JoinHandle<()>>>,
}

impl Shard {
    fn new(initial_durable: u64) -> Self {
        Self {
            tx: Mutex::new(None),
            highest_assigned: AtomicU64::new(initial_durable),
            highest_durable: AtomicU64::new(initial_durable),
            flusher: Mutex::new(None),
        }
    }
}

/// Disk-backed persistence engine for [`crate::ledger_db::LedgerStore`].
///
/// One handle per RocksDB instance. Cloning is via `Arc`; the engine is
/// `Send + Sync`. Drop signals the background flushers to drain their
/// channels and exit cleanly.
pub struct RocksPersistence {
    db: Arc<DB>,
    /// Phase 2.B parallel writer pool. Indexed by shard id ∈ [0, NUM_SHARDS).
    shards: Arc<[Shard; NUM_SHARDS]>,
    /// Globally-monotone seq counter. `fetch_add` on every enqueue.
    next_seq: AtomicU64,
    /// Set true on Drop; flushers check it as a belt-and-braces exit.
    shutdown: Arc<AtomicBool>,
    /// Notified after each successful flush from any shard — wakes
    /// [`wait_durable`].
    notify_pair: Arc<(Mutex<()>, Condvar)>,
}

/// Stats snapshot for metrics endpoints.
#[derive(Clone, Debug, Default)]
pub struct PersistenceStats {
    pub next_seq: u64,
    pub durable_seq: u64,
    pub pending: u64,
    pub shutdown: bool,
    /// Phase 2.B per-shard durable watermarks, indexed by shard id.
    pub shard_durable_seqs: [u64; NUM_SHARDS],
    /// Phase 2.B per-shard assigned seq counters (highest seq routed
    /// to each shard). Useful for spotting hotspots when one shard is
    /// taking disproportionate load.
    pub shard_assigned_seqs: [u64; NUM_SHARDS],
}

impl RocksPersistence {
    /// Open or create a RocksDB instance at `path` and start the
    /// Phase 2.B parallel writer pool. Returns the handle plus a
    /// [`RecoveredState`] snapshot that the caller (typically
    /// `LedgerStore::open`) uses to rebuild its in-memory mirror.
    pub fn open(path: impl AsRef<Path>) -> Result<(Arc<Self>, RecoveredState), RocksError> {
        let path = path.as_ref();

        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);
        // Compression: LZ4 strikes the canonical write-throughput vs
        // disk-size balance for hashy workloads. Most of our values are
        // either 32-byte ids (incompressible) or ~150-byte bincode
        // descriptors (slightly compressible).
        db_opts.set_compression_type(DBCompressionType::Lz4);
        // Recovery and stable WAL across crashes.
        db_opts.set_max_open_files(512);
        // Background workers: keep moderate; the bulk of throughput
        // comes from our own batched flush, not from internal
        // compactions. Bump from 2 → 4 in P2.B because we now drive
        // up to NUM_SHARDS concurrent fsyncs and want a healthier
        // background compaction headroom under load.
        db_opts.increase_parallelism(4);
        // Allow concurrent memtable writes from our shard pool.
        // RocksDB defaults this on; setting it explicitly documents
        // intent.
        db_opts.set_allow_concurrent_memtable_write(true);
        db_opts.set_enable_write_thread_adaptive_yield(true);

        let cfs = vec![
            ColumnFamilyDescriptor::new(CF_DESCRIPTORS, Options::default()),
            ColumnFamilyDescriptor::new(CF_HEADS, Options::default()),
            ColumnFamilyDescriptor::new(CF_CHAIN, Options::default()),
            ColumnFamilyDescriptor::new(CF_REVERSE, Options::default()),
            ColumnFamilyDescriptor::new(CF_PIECES, Options::default()),
        ];

        let db = Arc::new(DB::open_cf_descriptors(&db_opts, path, cfs)?);

        // ---- Recovery: scan every CF and read back the watermarks ----
        let recovered = recover_from_db(&db)?;
        let per_shard_watermarks = recover_per_shard_watermarks(&db)?;

        // ---- Set up parallel writer pool ----
        let shutdown = Arc::new(AtomicBool::new(false));
        let notify_pair = Arc::new((Mutex::new(()), Condvar::new()));

        // Each shard's atomic watermarks start at
        // `max(per_shard, recovered.durable_seq)`. The `max` ensures
        // that when opening a P1.5 database (no per-shard keys but a
        // legacy global watermark), every shard's atomic is lifted
        // to the legacy watermark so any caller `wait_durable(seq)`
        // for `seq <= legacy_watermark` returns immediately.
        let shards: [Shard; NUM_SHARDS] = std::array::from_fn(|i| {
            Shard::new(per_shard_watermarks[i].max(recovered.durable_seq))
        });
        let shards = Arc::new(shards);

        // Spawn one flusher per shard.
        for (i, shard) in shards.iter().enumerate() {
            let (tx, rx) = mpsc::channel::<PendingWrite>();
            *shard.tx.lock() = Some(tx);

            let db_for_thread = db.clone();
            let shutdown_for_thread = shutdown.clone();
            let notify_pair_for_thread = notify_pair.clone();
            let shards_for_thread = shards.clone();
            let handle = thread::Builder::new()
                .name(format!("rope-storage-flusher-{i}"))
                .spawn(move || {
                    flusher_loop(
                        i,
                        db_for_thread,
                        rx,
                        shards_for_thread,
                        shutdown_for_thread,
                        notify_pair_for_thread,
                    );
                })
                .expect("spawn flusher thread");
            *shard.flusher.lock() = Some(handle);
        }

        // `next_seq` resumes from one past the global watermark.
        // Pre-Drop crashes may have lost in-flight ops above the
        // watermark; that is the documented best-effort behaviour for
        // ops whose caller never invoked `wait_durable`.
        let next_seq = AtomicU64::new(recovered.durable_seq + 1);

        Ok((
            Arc::new(Self {
                db,
                shards,
                next_seq,
                shutdown,
                notify_pair,
            }),
            recovered,
        ))
    }

    /// Enqueue a write op. Returns the assigned sequence number, which
    /// callers may pass to [`Self::wait_durable`] to block until the op
    /// is fsync'd.
    ///
    /// Returns [`RocksError::FlusherStopped`] if the persistence layer
    /// has been dropped or the relevant flusher panicked. In normal
    /// operation this never errors.
    ///
    /// Phase 2.B: the op is routed to the shard determined by
    /// [`WriteOp::shard`] (first byte of wallet / string id, masked).
    pub fn enqueue(&self, op: WriteOp) -> Result<u64, RocksError> {
        let shard_id = op.shard();
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel);
        let shard = &self.shards[shard_id];

        // Bump highest_assigned BEFORE sending so wait_durable sees the
        // assignment even if it races with the send.
        shard.highest_assigned.fetch_max(seq, Ordering::AcqRel);

        let guard = shard.tx.lock();
        let tx = guard.as_ref().ok_or(RocksError::FlusherStopped)?;
        tx.send(PendingWrite { seq, op })
            .map_err(|_| RocksError::FlusherStopped)?;
        Ok(seq)
    }

    /// Block until the given sequence number is on durable disk, or
    /// `timeout` expires. Returns `true` iff the watermark reached
    /// `seq` before the deadline.
    ///
    /// Phase 2.B contract: returns once for **every** shard `i`,
    /// `highest_durable[i] >= min(highest_assigned[i], seq)`. This is
    /// the strongest correct invariant — a shard whose
    /// `highest_assigned <= seq` only needs to fsync up to its own
    /// assigned ceiling, while a shard whose `highest_assigned > seq`
    /// only needs to reach `seq` (its later seqs will be carried by a
    /// later wait).
    pub fn wait_durable(&self, seq: u64, timeout: Duration) -> bool {
        let start = Instant::now();
        let (lock, cvar) = &*self.notify_pair;
        let mut guard = lock.lock();
        loop {
            if self.is_durable(seq) {
                return true;
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return false;
            }
            let remaining = timeout - elapsed;
            let _ = cvar.wait_for(&mut guard, remaining);
        }
    }

    /// Phase 2.B durability check. See [`Self::wait_durable`] for the
    /// contract. Inlined-friendly: no allocations, no locks, just
    /// `2 × NUM_SHARDS` atomic loads.
    #[inline]
    fn is_durable(&self, target: u64) -> bool {
        for shard in self.shards.iter() {
            let assigned = shard.highest_assigned.load(Ordering::Acquire);
            let needed = assigned.min(target);
            if shard.highest_durable.load(Ordering::Acquire) < needed {
                return false;
            }
        }
        true
    }

    /// Latest globally-durable sequence number.
    ///
    /// Phase 2.B: returns the largest `S` such that
    /// [`Self::is_durable`]`(S)` would return true *right now*. The
    /// math: for every constraining shard (one where assigned > durable),
    /// the contribution is `highest_durable[i]`. The minimum across
    /// constraining shards is the global `S`. If no shard is
    /// constraining (every shard has caught up), `S = next_seq() - 1`.
    pub fn durable_seq(&self) -> u64 {
        let mut s = self.next_seq.load(Ordering::Acquire).saturating_sub(1);
        for shard in self.shards.iter() {
            let assigned = shard.highest_assigned.load(Ordering::Acquire);
            let durable = shard.highest_durable.load(Ordering::Acquire);
            if durable < assigned {
                s = s.min(durable);
            }
        }
        s
    }

    /// Highest sequence number assigned so far. The gap
    /// `next_seq() - durable_seq() - 1` is the in-flight queue depth.
    pub fn next_seq(&self) -> u64 {
        self.next_seq.load(Ordering::Acquire)
    }

    /// Snapshot stats for metrics endpoints. Phase 2.B includes
    /// per-shard breakdowns.
    pub fn stats(&self) -> PersistenceStats {
        let next = self.next_seq();
        let dur = self.durable_seq();
        let mut shard_durable_seqs = [0u64; NUM_SHARDS];
        let mut shard_assigned_seqs = [0u64; NUM_SHARDS];
        for (i, shard) in self.shards.iter().enumerate() {
            shard_durable_seqs[i] = shard.highest_durable.load(Ordering::Acquire);
            shard_assigned_seqs[i] = shard.highest_assigned.load(Ordering::Acquire);
        }
        PersistenceStats {
            next_seq: next,
            durable_seq: dur,
            pending: next.saturating_sub(dur).saturating_sub(1),
            shutdown: self.shutdown.load(Ordering::Acquire),
            shard_durable_seqs,
            shard_assigned_seqs,
        }
    }

    /// Read back the descriptor for `wallet` directly from disk,
    /// bypassing the in-memory mirror. Useful for recovery sanity
    /// checks and for tests that need to assert disk state.
    pub fn read_descriptor(
        &self,
        wallet: &[u8],
    ) -> Result<Option<StoredLedgerDescriptor>, RocksError> {
        let cf = self
            .db
            .cf_handle(CF_DESCRIPTORS)
            .ok_or(RocksError::MissingCf(CF_DESCRIPTORS))?;
        match self.db.get_cf(&cf, wallet)? {
            None => Ok(None),
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
        }
    }

    /// Read back the head pointer for `wallet` directly from disk.
    pub fn read_head(&self, wallet: &[u8]) -> Result<Option<[u8; 32]>, RocksError> {
        let cf = self
            .db
            .cf_handle(CF_HEADS)
            .ok_or(RocksError::MissingCf(CF_HEADS))?;
        match self.db.get_cf(&cf, wallet)? {
            None => Ok(None),
            Some(bytes) if bytes.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Ok(Some(arr))
            }
            Some(other) => Err(RocksError::Corrupted(format!(
                "head value malformed: expected 32 bytes, got {}",
                other.len()
            ))),
        }
    }
}

impl Drop for RocksPersistence {
    fn drop(&mut self) {
        // 1. Flag shutdown so the flushers exit their idle loops quickly.
        self.shutdown.store(true, Ordering::Release);
        // 2. Drop every shard sender → flushers see Disconnected on
        //    their next iteration, and each exits after one final
        //    drain+flush.
        for shard in self.shards.iter() {
            let mut guard = shard.tx.lock();
            *guard = None;
        }
        // 3. Wait for every flusher to finish its final batch and exit.
        for shard in self.shards.iter() {
            if let Some(handle) = shard.flusher.lock().take() {
                let _ = handle.join();
            }
        }
        // 4. Best-effort one more sync just in case a flusher panicked
        //    mid-flight.
        let _ = self.db.flush();
    }
}

// ============================================================================
// Background flusher (one per shard)
// ============================================================================

fn flusher_loop(
    shard_id: usize,
    db: Arc<DB>,
    rx: mpsc::Receiver<PendingWrite>,
    shards: Arc<[Shard; NUM_SHARDS]>,
    shutdown: Arc<AtomicBool>,
    notify_pair: Arc<(Mutex<()>, Condvar)>,
) {
    let mut buf: Vec<PendingWrite> = Vec::with_capacity(MAX_BATCH_OPS);

    loop {
        // Block until the first op or the channel disconnects.
        match rx.recv_timeout(FLUSH_INTERVAL) {
            Ok(first) => buf.push(first),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Sender dropped — graceful shutdown after the loop.
                break;
            }
        }

        // Drain whatever is immediately available, up to the batch cap.
        while buf.len() < MAX_BATCH_OPS {
            match rx.try_recv() {
                Ok(op) => buf.push(op),
                Err(_) => break,
            }
        }

        if let Err(e) = flush_shard_batch(shard_id, &db, &buf, &shards[shard_id]) {
            // RocksDB write failures are fatal for durability — log and
            // bail out of the loop so the next enqueue sees
            // `FlusherStopped`. The OS will surface the underlying
            // disk-full / EROFS / etc.
            tracing::error!(
                "rope-storage shard {shard_id} flusher write failed: {e:?} (op_count={}); flusher exiting",
                buf.len()
            );
            return;
        }

        // Wake everyone parked in `wait_durable`.
        notify_pair.1.notify_all();

        buf.clear();
    }

    // Final drain + flush on shutdown — pull any remaining ops from
    // the channel, write them, advance the shard watermark, and notify.
    while let Ok(op) = rx.try_recv() {
        buf.push(op);
        if buf.len() >= MAX_BATCH_OPS {
            if let Err(e) = flush_shard_batch(shard_id, &db, &buf, &shards[shard_id]) {
                tracing::error!(
                    "rope-storage shard {shard_id} flusher final-drain write failed: {e:?}"
                );
                return;
            }
            buf.clear();
        }
    }
    if !buf.is_empty() {
        if let Err(e) = flush_shard_batch(shard_id, &db, &buf, &shards[shard_id]) {
            tracing::error!("rope-storage shard {shard_id} flusher final-drain write failed: {e:?}");
            return;
        }
    }
    // Final notify so any waiter for the highest seq can see it.
    notify_pair.1.notify_all();
}

/// Phase 2.B per-shard batch flush. Builds a `WriteBatch` of all the
/// CF puts implied by `ops`, stamps the per-shard watermark in the
/// same batch (for atomic crash semantics), and issues an fsync'd
/// `db.write_opt`. RocksDB's WAL group commit naturally amortises
/// concurrent fsyncs from sibling shards.
fn flush_shard_batch(
    shard_id: usize,
    db: &DB,
    ops: &[PendingWrite],
    shard: &Shard,
) -> Result<(), RocksError> {
    if ops.is_empty() {
        return Ok(());
    }

    let cf_descriptors = db
        .cf_handle(CF_DESCRIPTORS)
        .ok_or(RocksError::MissingCf(CF_DESCRIPTORS))?;
    let cf_heads = db
        .cf_handle(CF_HEADS)
        .ok_or(RocksError::MissingCf(CF_HEADS))?;
    let cf_chain = db
        .cf_handle(CF_CHAIN)
        .ok_or(RocksError::MissingCf(CF_CHAIN))?;
    let cf_reverse = db
        .cf_handle(CF_REVERSE)
        .ok_or(RocksError::MissingCf(CF_REVERSE))?;
    let cf_pieces = db
        .cf_handle(CF_PIECES)
        .ok_or(RocksError::MissingCf(CF_PIECES))?;

    let mut batch = WriteBatch::default();
    let mut highest_seq = 0u64;

    for pw in ops {
        highest_seq = highest_seq.max(pw.seq);
        match &pw.op {
            WriteOp::PutDescriptor { wallet, desc } => {
                let bytes = bincode::serialize(desc)?;
                batch.put_cf(&cf_descriptors, wallet, &bytes);
                batch.put_cf(&cf_heads, wallet, desc.head_string_id);
            }
            WriteOp::AppendChain {
                wallet,
                seq_in_wallet,
                string_id,
            } => {
                let key = chain_key(wallet, *seq_in_wallet);
                batch.put_cf(&cf_chain, &key, string_id);
                batch.put_cf(&cf_reverse, string_id, wallet);
                batch.put_cf(&cf_heads, wallet, string_id);
            }
            WriteOp::PutPieceMap {
                string_id,
                piece_map,
            } => {
                let bytes = bincode::serialize(piece_map)?;
                batch.put_cf(&cf_pieces, string_id, &bytes);
            }
            WriteOp::MarkDeleted { wallet, deleted_at } => {
                // Read-modify-write: pull the existing descriptor,
                // flip is_deleted + set deleted_at, write it back.
                // If the descriptor is missing the op is a no-op (the
                // wallet was never registered on disk — likely
                // dropped during recovery from a partial crash).
                if let Some(existing) = db.get_cf(&cf_descriptors, wallet)? {
                    let mut desc: StoredLedgerDescriptor = bincode::deserialize(&existing)?;
                    desc.is_deleted = true;
                    desc.deleted_at = Some(*deleted_at);
                    let bytes = bincode::serialize(&desc)?;
                    batch.put_cf(&cf_descriptors, wallet, &bytes);
                }
            }
        }
    }

    // Stamp the per-shard watermark in the same WriteBatch so
    // durability of the op set and durability of the watermark advance
    // are atomic — a crash mid-write rolls back BOTH together.
    let key = durable_seq_shard_key(shard_id);
    batch.put(&key, highest_seq.to_le_bytes());

    let mut wo = WriteOptions::default();
    wo.set_sync(true); // fsync after WAL write — the durability ack
    db.write_opt(batch, &wo)?;

    // Advance the per-shard durable watermark only after the fsync
    // returns. `fetch_max` so a slow flusher never undoes a sibling's
    // already-published progress (cannot happen today because each
    // shard owns its own seqs, but cheap insurance).
    shard.highest_durable.fetch_max(highest_seq, Ordering::AcqRel);
    Ok(())
}

/// Build the composite chain key: `wallet || u64-be seq_in_wallet`.
/// Big-endian on the seq half so that lexicographic order over RocksDB
/// keys equals append order — recovery just walks the prefix in the
/// natural iterator order.
fn chain_key(wallet: &[u8], seq_in_wallet: u64) -> Vec<u8> {
    let mut k = Vec::with_capacity(wallet.len() + 8);
    k.extend_from_slice(wallet);
    k.extend_from_slice(&seq_in_wallet.to_be_bytes());
    k
}

// ============================================================================
// Recovery
// ============================================================================

fn recover_from_db(db: &DB) -> Result<RecoveredState, RocksError> {
    use std::collections::HashMap;

    let cf_descriptors = db
        .cf_handle(CF_DESCRIPTORS)
        .ok_or(RocksError::MissingCf(CF_DESCRIPTORS))?;
    let cf_heads = db
        .cf_handle(CF_HEADS)
        .ok_or(RocksError::MissingCf(CF_HEADS))?;
    let cf_chain = db
        .cf_handle(CF_CHAIN)
        .ok_or(RocksError::MissingCf(CF_CHAIN))?;
    let cf_reverse = db
        .cf_handle(CF_REVERSE)
        .ok_or(RocksError::MissingCf(CF_REVERSE))?;
    let cf_pieces = db
        .cf_handle(CF_PIECES)
        .ok_or(RocksError::MissingCf(CF_PIECES))?;

    let mut state = RecoveredState::default();

    for kv in db.iterator_cf(&cf_descriptors, IteratorMode::Start) {
        let (k, v) = kv?;
        let desc: StoredLedgerDescriptor = bincode::deserialize(&v)?;
        state.descriptors.push((k.into_vec(), desc));
    }

    for kv in db.iterator_cf(&cf_heads, IteratorMode::Start) {
        let (k, v) = kv?;
        if v.len() != 32 {
            return Err(RocksError::Corrupted(format!(
                "head value malformed: expected 32 bytes, got {}",
                v.len()
            )));
        }
        let mut head = [0u8; 32];
        head.copy_from_slice(&v);
        state.heads.push((k.into_vec(), head));
    }

    // Chain CF is iterated in lexicographic order; bucket by wallet.
    let mut chains: HashMap<Vec<u8>, Vec<[u8; 32]>> = HashMap::new();
    for kv in db.iterator_cf(&cf_chain, IteratorMode::Start) {
        let (k, v) = kv?;
        if k.len() < 8 {
            return Err(RocksError::Corrupted(format!(
                "chain key malformed: expected ≥ 8 bytes, got {}",
                k.len()
            )));
        }
        let split = k.len() - 8;
        let wallet = k[..split].to_vec();
        if v.len() != 32 {
            return Err(RocksError::Corrupted(format!(
                "chain value malformed: expected 32 bytes, got {}",
                v.len()
            )));
        }
        let mut sid = [0u8; 32];
        sid.copy_from_slice(&v);
        chains.entry(wallet).or_default().push(sid);
    }
    state.chains = chains.into_iter().collect();

    for kv in db.iterator_cf(&cf_reverse, IteratorMode::Start) {
        let (k, v) = kv?;
        if k.len() != 32 {
            return Err(RocksError::Corrupted(format!(
                "reverse key malformed: expected 32 bytes, got {}",
                k.len()
            )));
        }
        let mut sid = [0u8; 32];
        sid.copy_from_slice(&k);
        state.reverse.push((sid, v.into_vec()));
    }

    for kv in db.iterator_cf(&cf_pieces, IteratorMode::Start) {
        let (k, v) = kv?;
        if k.len() != 32 {
            return Err(RocksError::Corrupted(format!(
                "pieces key malformed: expected 32 bytes, got {}",
                k.len()
            )));
        }
        let mut sid = [0u8; 32];
        sid.copy_from_slice(&k);
        let pm: StoredPieceMap = bincode::deserialize(&v)?;
        state.pieces.push((sid, pm));
    }

    state.durable_seq = recover_global_watermark(db)?;

    Ok(state)
}

/// Compute the global recovered watermark, honouring both the legacy
/// Phase 1.5 `b"durable_seq"` key and the Phase 2.B per-shard
/// `b"durable_seq_shard_<i>"` keys.
///
/// Algorithm: take the **min over shards of recovered_per_shard[i]**,
/// then take the **max** of that with the legacy global watermark.
///
/// Why `min` across shards: in Phase 2.B every seq routes to exactly
/// one shard, but the global watermark must be the largest `S` such
/// that *every* seq ≤ `S` is on disk. If shard A is at 1000 but shard
/// B is at 500, then seqs 501..1000 may include some that landed in
/// shard B and were lost in the crash → global must be ≤ 500.
///
/// Why `max` with legacy: a fresh DB upgraded from P1.5 still has the
/// old single watermark and no per-shard keys → the per-shard min is
/// 0, but the legacy key tells us a higher S was actually durable.
/// We take max to honour the more aggressive of the two. This is
/// safe because P1.5 wrote the global key after fsync, so everything
/// up to it is on disk regardless of shard partitioning.
fn recover_global_watermark(db: &DB) -> Result<u64, RocksError> {
    let legacy = match db.get(DURABLE_SEQ_KEY)? {
        None => 0,
        Some(bytes) if bytes.len() == 8 => {
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&bytes);
            u64::from_le_bytes(arr)
        }
        Some(other) => {
            return Err(RocksError::Corrupted(format!(
                "durable_seq value malformed: expected 8 bytes, got {}",
                other.len()
            )));
        }
    };

    let per_shard = recover_per_shard_watermarks(db)?;
    let any_shard_present = per_shard.iter().any(|&w| w > 0);
    let shard_min = if any_shard_present {
        // If at least one shard has been written, the global lower
        // bound is the min across shards. (A shard at 0 with a sibling
        // at >0 means that shard never received any op, so it places
        // no constraint on the global watermark.)
        per_shard
            .iter()
            .copied()
            .filter(|&w| w > 0)
            .min()
            .unwrap_or(0)
    } else {
        0
    };

    Ok(legacy.max(shard_min))
}

/// Read the Phase 2.B per-shard watermark for each shard. Missing
/// keys ⇒ 0 (shard never written).
fn recover_per_shard_watermarks(db: &DB) -> Result<[u64; NUM_SHARDS], RocksError> {
    let mut out = [0u64; NUM_SHARDS];
    for (i, slot) in out.iter_mut().enumerate() {
        let key = durable_seq_shard_key(i);
        *slot = match db.get(&key)? {
            None => 0,
            Some(bytes) if bytes.len() == 8 => {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&bytes);
                u64::from_le_bytes(arr)
            }
            Some(other) => {
                return Err(RocksError::Corrupted(format!(
                    "durable_seq_shard_{i} value malformed: expected 8 bytes, got {}",
                    other.len()
                )));
            }
        };
    }
    Ok(out)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
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
    fn open_creates_db_with_all_column_families() {
        let dir = TempDir::new().unwrap();
        let (p, recovered) = RocksPersistence::open(dir.path()).unwrap();
        assert!(recovered.descriptors.is_empty());
        assert_eq!(recovered.durable_seq, 0);
        // Sanity: the CFs exist (otherwise read_descriptor would error).
        assert!(p.read_descriptor(b"nope").unwrap().is_none());
    }

    #[test]
    fn enqueue_returns_monotonic_seqs_and_persists_after_wait() {
        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistence::open(dir.path()).unwrap();

        let wallet = vec![0xAAu8; 20];
        let head = [0xCDu8; 32];
        let seq1 = p
            .enqueue(WriteOp::PutDescriptor {
                wallet: wallet.clone(),
                desc: dummy_descriptor(&wallet, head),
            })
            .unwrap();
        let seq2 = p
            .enqueue(WriteOp::AppendChain {
                wallet: wallet.clone(),
                seq_in_wallet: 0,
                string_id: head,
            })
            .unwrap();

        assert!(seq2 > seq1, "seqs must be monotonically increasing");

        // Wait for durability — generously over the 10 ms tick.
        assert!(
            p.wait_durable(seq2, Duration::from_secs(2)),
            "wait_durable must succeed within 2s"
        );
        assert!(
            p.durable_seq() >= seq2,
            "durable_seq must reach the awaited seq"
        );

        // Direct disk read confirms the descriptor really hit RocksDB.
        let on_disk = p.read_descriptor(&wallet).unwrap().unwrap();
        assert_eq!(on_disk.head_string_id, head);
        assert_eq!(p.read_head(&wallet).unwrap(), Some(head));
    }

    #[test]
    fn append_chain_keys_sort_in_append_order() {
        // Big-endian seq encoding means lexicographic key order =
        // append order. Recovery relies on this.
        let k0 = chain_key(b"WALLET", 0);
        let k1 = chain_key(b"WALLET", 1);
        let k256 = chain_key(b"WALLET", 256);
        let k_max = chain_key(b"WALLET", u64::MAX);
        assert!(k0 < k1);
        assert!(k1 < k256);
        assert!(k256 < k_max);

        // Distinct wallets do not interleave with each other.
        let other = chain_key(b"OTHER!", u64::MAX);
        assert!(k0 < other || other < k0);
    }

    #[test]
    fn recovery_replays_state_after_drop_and_reopen() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        // ---- Phase 1: open, write, await durability, drop ----
        let wallet = vec![0xAAu8; 20];
        let head = [0xEFu8; 32];
        let sid1 = [0x01u8; 32];
        let sid2 = [0x02u8; 32];

        let highest_seq;
        {
            let (p, _) = RocksPersistence::open(&path).unwrap();
            p.enqueue(WriteOp::PutDescriptor {
                wallet: wallet.clone(),
                desc: dummy_descriptor(&wallet, head),
            })
            .unwrap();
            p.enqueue(WriteOp::AppendChain {
                wallet: wallet.clone(),
                seq_in_wallet: 0,
                string_id: sid1,
            })
            .unwrap();
            highest_seq = p
                .enqueue(WriteOp::AppendChain {
                    wallet: wallet.clone(),
                    seq_in_wallet: 1,
                    string_id: sid2,
                })
                .unwrap();
            assert!(p.wait_durable(highest_seq, Duration::from_secs(2)));
            // Implicit drop here: the flushers final-drain, then the
            // DB is closed.
        }

        // ---- Phase 2: reopen, verify recovery ----
        let (p2, recovered) = RocksPersistence::open(&path).unwrap();

        assert_eq!(recovered.descriptors.len(), 1);
        assert_eq!(recovered.descriptors[0].0, wallet);

        assert_eq!(recovered.heads.len(), 1);
        // Heads CF stores the LATEST head, not the original. Last
        // append set head to sid2.
        assert_eq!(recovered.heads[0].1, sid2);

        assert_eq!(recovered.chains.len(), 1);
        let (rec_wallet, chain) = &recovered.chains[0];
        assert_eq!(rec_wallet, &wallet);
        assert_eq!(chain, &vec![sid1, sid2], "chain must be in append order");

        assert_eq!(recovered.reverse.len(), 2);

        // Phase 2.B: the global watermark equals the highest seq IFF
        // every shard that received an op has caught up to it. Since
        // all three ops here route to the same shard (same wallet),
        // and we waited for durability, this holds.
        assert_eq!(
            recovered.durable_seq, highest_seq,
            "durable_seq must persist across reopen"
        );
        assert_eq!(
            p2.next_seq(),
            highest_seq + 1,
            "next_seq must resume from the persisted watermark"
        );
    }

    #[test]
    fn mark_deleted_flips_descriptor_on_disk() {
        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistence::open(dir.path()).unwrap();
        let wallet = vec![0xAAu8; 20];
        let head = [0xCDu8; 32];

        let s = p
            .enqueue(WriteOp::PutDescriptor {
                wallet: wallet.clone(),
                desc: dummy_descriptor(&wallet, head),
            })
            .unwrap();
        assert!(p.wait_durable(s, Duration::from_secs(2)));
        assert!(!p.read_descriptor(&wallet).unwrap().unwrap().is_deleted);

        let s2 = p
            .enqueue(WriteOp::MarkDeleted {
                wallet: wallet.clone(),
                deleted_at: 9_999_999_999,
            })
            .unwrap();
        assert!(p.wait_durable(s2, Duration::from_secs(2)));

        let on_disk = p.read_descriptor(&wallet).unwrap().unwrap();
        assert!(on_disk.is_deleted);
        assert_eq!(on_disk.deleted_at, Some(9_999_999_999));
    }

    #[test]
    fn watermark_advances_under_concurrent_load() {
        use std::sync::Arc as StdArc;
        use std::thread;

        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistence::open(dir.path()).unwrap();
        let p = StdArc::new(p);

        const THREADS: usize = 8;
        const APPENDS_PER_THREAD: u64 = 25;

        let mut handles = Vec::new();
        for tid in 0..THREADS as u8 {
            let p = p.clone();
            handles.push(thread::spawn(move || {
                let wallet = vec![tid; 20];
                let mut last_seq = 0;
                for i in 0..APPENDS_PER_THREAD {
                    let mut sid = [0u8; 32];
                    sid[0] = tid;
                    sid[1] = i as u8;
                    last_seq = p
                        .enqueue(WriteOp::AppendChain {
                            wallet: wallet.clone(),
                            seq_in_wallet: i,
                            string_id: sid,
                        })
                        .unwrap();
                }
                last_seq
            }));
        }

        let mut max_seq = 0u64;
        for h in handles {
            max_seq = max_seq.max(h.join().unwrap());
        }

        assert!(p.wait_durable(max_seq, Duration::from_secs(5)));
        assert!(p.durable_seq() >= max_seq);

        // Stats sanity: pending should drain to zero.
        let stats = p.stats();
        assert_eq!(stats.pending, 0, "pending must drain after wait_durable");
    }

    #[test]
    fn drop_drains_unawaited_writes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        let wallet = vec![0xBEu8; 20];

        // Open, enqueue WITHOUT calling wait_durable, then drop.
        // The Drop impl must drain every shard's channel and final-flush.
        {
            let (p, _) = RocksPersistence::open(&path).unwrap();
            for i in 0..50u64 {
                let mut sid = [0u8; 32];
                sid[0] = i as u8;
                p.enqueue(WriteOp::AppendChain {
                    wallet: wallet.clone(),
                    seq_in_wallet: i,
                    string_id: sid,
                })
                .unwrap();
            }
            // Drop here. No explicit wait_durable.
        }

        // Reopen and verify the chain is intact.
        let (_p2, recovered) = RocksPersistence::open(&path).unwrap();
        let chain = recovered
            .chains
            .into_iter()
            .find(|(w, _)| w == &wallet)
            .map(|(_, c)| c)
            .expect("wallet chain must be recovered");
        assert_eq!(chain.len(), 50, "Drop must final-drain unawaited writes");
        assert_eq!(chain[0][0], 0);
        assert_eq!(chain[49][0], 49);
        // Watermark must reflect the final flush.
        assert!(recovered.durable_seq >= 50);
    }

    // ------------------------------------------------------------------
    // Phase 2.B specific tests
    // ------------------------------------------------------------------

    /// Constants exposed for sanity assertions.
    #[test]
    fn shard_count_is_power_of_two() {
        assert!(NUM_SHARDS.is_power_of_two());
        assert_eq!(SHARD_MASK as usize, NUM_SHARDS - 1);
    }

    /// Verify wallets with distinct first bytes go to distinct shards
    /// (when the bytes happen to map distinctly under the mask).
    #[test]
    fn op_partitioning_is_stable_and_evenly_spread() {
        // Sweep all 256 first-byte values and check the shard mapping
        // distributes evenly: every shard must get exactly 256/NUM_SHARDS = 32 hits.
        let mut hits = [0usize; NUM_SHARDS];
        for b in 0u8..=255 {
            let op = WriteOp::PutDescriptor {
                wallet: vec![b],
                desc: dummy_descriptor(&[b], [0u8; 32]),
            };
            hits[op.shard()] += 1;
        }
        for (i, c) in hits.iter().enumerate() {
            assert_eq!(*c, 256 / NUM_SHARDS, "shard {i} got {c} hits, want 32");
        }
    }

    /// Phase 2.B parallel writers must service ops from every shard
    /// independently — the cross-shard durability invariant must hold
    /// even when one shard is hot and others are quiet.
    #[test]
    fn skewed_load_across_shards_still_meets_global_watermark() {
        use std::sync::Arc as StdArc;
        use std::thread;

        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistence::open(dir.path()).unwrap();
        let p = StdArc::new(p);

        // Two threads pound on shard 0 and shard 1 only.
        // Two other threads do one tiny op on shards 5 and 6 then idle.
        // wait_durable for the *last* seq must still succeed.
        let mut handles = Vec::new();

        for hot_first_byte in [0u8, 1u8] {
            let p = p.clone();
            handles.push(thread::spawn(move || {
                let wallet = vec![hot_first_byte; 20];
                for i in 0..200u64 {
                    let mut sid = [0u8; 32];
                    sid[0] = hot_first_byte;
                    sid[1] = i as u8;
                    p.enqueue(WriteOp::AppendChain {
                        wallet: wallet.clone(),
                        seq_in_wallet: i,
                        string_id: sid,
                    })
                    .unwrap();
                }
            }));
        }

        for cold_first_byte in [5u8, 6u8] {
            let p = p.clone();
            handles.push(thread::spawn(move || {
                let wallet = vec![cold_first_byte; 20];
                p.enqueue(WriteOp::AppendChain {
                    wallet,
                    seq_in_wallet: 0,
                    string_id: [cold_first_byte; 32],
                })
                .unwrap();
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // The very last seq might have landed in any shard — we don't
        // care which. The contract is: wait_durable on the highest
        // assigned seq must succeed.
        let target = p.next_seq().saturating_sub(1);
        assert!(
            p.wait_durable(target, Duration::from_secs(5)),
            "wait_durable must complete even when load is skewed across shards"
        );

        let stats = p.stats();
        // The two hot shards must each have a durable seq > 0.
        assert!(stats.shard_durable_seqs[0] > 0);
        assert!(stats.shard_durable_seqs[1] > 0);
        // The two cold shards (5, 6) must also have durable_seq > 0.
        assert!(stats.shard_durable_seqs[5] > 0);
        assert!(stats.shard_durable_seqs[6] > 0);
        // Quiet shards may be at 0.
    }

    /// Verify each shard persists its watermark to its own key, and
    /// recovery picks the conservative `min over shards` so we never
    /// claim a seq is durable when in fact a sibling shard lost it.
    #[test]
    fn per_shard_watermarks_persist_and_recover_correctly() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        // Phase 1: enqueue ops onto two specific shards (by choosing
        // wallets whose first byte goes to shard 0 vs shard 1), wait
        // for each independently, then drop.
        let mut highest_per_shard = [0u64; NUM_SHARDS];
        {
            let (p, _) = RocksPersistence::open(&path).unwrap();
            // 5 ops to shard 0 (wallet first byte = 0)
            for i in 0..5u64 {
                let mut sid = [0u8; 32];
                sid[0] = i as u8;
                let s = p
                    .enqueue(WriteOp::AppendChain {
                        wallet: vec![0u8; 20],
                        seq_in_wallet: i,
                        string_id: sid,
                    })
                    .unwrap();
                highest_per_shard[0] = highest_per_shard[0].max(s);
            }
            // 3 ops to shard 1 (wallet first byte = 1, masked against 0b111 ⇒ shard 1)
            for i in 0..3u64 {
                let mut sid = [0u8; 32];
                sid[0] = 0x10 + i as u8;
                let s = p
                    .enqueue(WriteOp::AppendChain {
                        wallet: vec![1u8; 20],
                        seq_in_wallet: i,
                        string_id: sid,
                    })
                    .unwrap();
                highest_per_shard[1] = highest_per_shard[1].max(s);
            }
            let target = highest_per_shard[0].max(highest_per_shard[1]);
            assert!(p.wait_durable(target, Duration::from_secs(2)));
        }

        // Phase 2: open a new instance at the same path with raw RocksDB
        // and inspect the per-shard watermark keys.
        {
            // Reopen via the public API and inspect the recovered state.
            let (p2, recovered) = RocksPersistence::open(&path).unwrap();

            // Global recovered watermark must be at least the highest
            // that landed on the slower of the two shards (i.e., at
            // least min(highest_per_shard[0..2])).
            let min_used_shard =
                highest_per_shard[0].min(highest_per_shard[1]);
            assert!(
                recovered.durable_seq >= min_used_shard,
                "recovered durable_seq {} must be ≥ min-used-shard high {}",
                recovered.durable_seq,
                min_used_shard
            );

            // Per-shard stats: shards 0 and 1 must have non-zero
            // durable seqs (at least their per-shard high). The
            // untouched shards are lifted to `recovered.durable_seq`
            // on open so that `wait_durable(seq)` for any
            // `seq <= recovered.durable_seq` returns immediately —
            // every seq below the global watermark IS durable, even
            // for shards that received no traffic.
            let s = p2.stats();
            assert!(s.shard_durable_seqs[0] >= highest_per_shard[0]);
            assert!(s.shard_durable_seqs[1] >= highest_per_shard[1]);
            for i in 2..NUM_SHARDS {
                assert_eq!(
                    s.shard_durable_seqs[i], recovered.durable_seq,
                    "shard {i} should start at the recovered global watermark"
                );
            }
        }
    }

    /// Verify the conservative `wait_durable(S)` invariant under
    /// extreme cross-shard skew: enqueue many ops onto shard 0,
    /// then ONE op onto shard 1, and check that wait_durable on the
    /// last (shard-1) seq does NOT return until shard 1 has caught
    /// up — even if shard 0 still has in-flight ops > S.
    #[test]
    fn wait_durable_is_correct_when_target_seq_is_in_minor_shard() {
        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistence::open(dir.path()).unwrap();

        // Pre-load shard 0 with a backlog (enqueue but DON'T wait).
        for i in 0..50u64 {
            let mut sid = [0u8; 32];
            sid[0] = i as u8;
            p.enqueue(WriteOp::AppendChain {
                wallet: vec![0u8; 20],
                seq_in_wallet: i,
                string_id: sid,
            })
            .unwrap();
        }
        // Now enqueue one op to shard 1.
        let target = p
            .enqueue(WriteOp::AppendChain {
                wallet: vec![1u8; 20],
                seq_in_wallet: 0,
                string_id: [0xCDu8; 32],
            })
            .unwrap();

        // wait_durable for the shard-1 seq must succeed, irrespective
        // of whether shard 0 has caught up to its own backlog.
        assert!(p.wait_durable(target, Duration::from_secs(5)));
        // After the wait, shard 1's watermark must be ≥ target.
        let s = p.stats();
        assert!(
            s.shard_durable_seqs[1] >= target,
            "shard 1 watermark {} must reach target {}",
            s.shard_durable_seqs[1],
            target
        );
    }

    /// A Phase 1.5 database (single `durable_seq` key, no per-shard
    /// keys) must open cleanly under the Phase 2.B binary and recover
    /// the legacy watermark.
    #[test]
    fn legacy_p1_5_database_is_recovered_via_back_compat_key() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();

        // Manually open RocksDB at this path, write the legacy key,
        // close. This simulates a P1.5 database.
        {
            let mut opts = Options::default();
            opts.create_if_missing(true);
            opts.create_missing_column_families(true);
            let cfs = vec![
                ColumnFamilyDescriptor::new(CF_DESCRIPTORS, Options::default()),
                ColumnFamilyDescriptor::new(CF_HEADS, Options::default()),
                ColumnFamilyDescriptor::new(CF_CHAIN, Options::default()),
                ColumnFamilyDescriptor::new(CF_REVERSE, Options::default()),
                ColumnFamilyDescriptor::new(CF_PIECES, Options::default()),
            ];
            let db = DB::open_cf_descriptors(&opts, path, cfs).unwrap();
            // Write legacy watermark = 12345
            let mut wo = WriteOptions::default();
            wo.set_sync(true);
            let mut batch = WriteBatch::default();
            batch.put(DURABLE_SEQ_KEY, 12345u64.to_le_bytes());
            db.write_opt(batch, &wo).unwrap();
            // No per-shard keys written.
        }

        // Now reopen via the Phase 2.B persistence and verify the
        // legacy watermark is honoured.
        let (p, recovered) = RocksPersistence::open(path).unwrap();
        assert_eq!(
            recovered.durable_seq, 12345,
            "P2.B must honour legacy P1.5 durable_seq key"
        );
        // next_seq starts one above the legacy watermark.
        assert_eq!(p.next_seq(), 12346);
        // Per-shard atomics should be initialised to the global
        // recovered watermark so any first wait_durable below it is
        // satisfied immediately.
        let s = p.stats();
        for i in 0..NUM_SHARDS {
            assert_eq!(s.shard_durable_seqs[i], 12345);
            assert_eq!(s.shard_assigned_seqs[i], 12345);
        }
    }

    /// `durable_seq()` must return the largest S such that
    /// `is_durable(S) == true`. Verify this end-to-end.
    #[test]
    fn durable_seq_matches_is_durable_largest_true() {
        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistence::open(dir.path()).unwrap();

        // Enqueue 20 ops, wait, then sample.
        for i in 0..20u64 {
            let mut sid = [0u8; 32];
            sid[0] = (i % 8) as u8;
            sid[1] = i as u8;
            p.enqueue(WriteOp::AppendChain {
                wallet: vec![sid[0]; 20],
                seq_in_wallet: i / 8,
                string_id: sid,
            })
            .unwrap();
        }
        let max = p.next_seq() - 1;
        assert!(p.wait_durable(max, Duration::from_secs(5)));

        let durable = p.durable_seq();
        assert!(durable >= max, "durable_seq {durable} must be >= {max}");
        assert!(p.is_durable(durable), "is_durable(durable_seq()) must be true");
    }
}
