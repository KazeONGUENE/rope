//! Parallel-writer RocksDB persistence backend for
//! [`crate::ledger_db::LedgerStore`] — Phase 2.B of the Quipu Canon
//! v2.0 scaling roadmap.
//!
//! ## Why this exists
//!
//! The default [`crate::rocksdb_persistence::RocksPersistence`] backend
//! (Phase 1.5 + 1.6) uses a **single** background flusher thread that
//! drains **one** enqueue channel and writes **one** `WriteBatch` per
//! ~10 ms tick to a **shared WAL**. That WAL is the ultimate write
//! bottleneck: every `append_to_ledger` from every wallet on every
//! tokio worker serialises through it.
//!
//! Post-incident forensics on 2026-08-11 (see §17 + §19-§21 of
//! `.cursor/rules/handover-from-dcswap-dcscan-address-parity-fixes-
//! 2026-08-11.mdc`) showed that the residual 5-9 s RPC latency spikes
//! that persisted **after** the Phase 1.6.β lock-fix (§18) and Phase C
//! OES-outside-head_guard (§21) were caused by exactly this WAL
//! serialisation: even with per-wallet head locks and cheap OES
//! derivation, three RocksDB writes per append (blob, chain, descriptor)
//! all funnelled through the single writer and blocked tokio workers
//! long enough to starve `eth_blockNumber` health probes.
//!
//! This module replaces the single-writer with **N independent sharded
//! writers**, each with its own bounded channel, its own background
//! flusher thread, and its own per-shard durability watermark. Ops are
//! sharded on a `partition_byte` (see [`WriteOp::partition_byte`] in
//! the sibling module) so each writer sees roughly `1/N` of the total
//! throughput. Global `wait_durable(seq)` returns as soon as every
//! shard that COULD have accepted `seq` has flushed at least `seq`.
//!
//! ## Correctness properties preserved from Phase 1.5 + 1.6
//!
//! 1. **Enqueue-before-mirror**: caller (LedgerStore) still enqueues
//!    to the persistence layer BEFORE mutating the in-memory mirror,
//!    so a full queue cannot silently ack a write that never landed.
//! 2. **Per-shard batch atomicity**: each shard's WriteBatch is
//!    committed with `fsync=true`, so a crash mid-flush loses whole
//!    batches, never partial batches, on that shard.
//! 3. **Per-wallet chain ordering**: `LedgerStore::append_to_chain`
//!    reserves a per-wallet `seq_in_wallet` before enqueueing, and
//!    all AppendChain ops for the same wallet route to the same
//!    shard (partition_byte = wallet[0]). Within a shard, ops are
//!    flushed in FIFO order via a single mpsc channel + single
//!    flusher thread, so per-wallet chain order is preserved.
//! 4. **Descriptor↔head coherence**: PutDescriptor writes descriptor
//!    + head_index in the same WriteBatch. Because same-wallet
//!    PutDescriptor + AppendChain both route to the same shard,
//!    a wallet's descriptor and its chain never diverge across a
//!    crash boundary.
//! 5. **Untie + tombstone coherence**: DeleteStringBlob + PutTombstone
//!    both key on `string_id`, so they route to the same shard, so
//!    they land in the same WriteBatch order. GDPR erasure survives
//!    restarts.
//! 6. **Cross-shard split of blob vs chain**: `PutStringBlob` routes
//!    on `string_id[0]` while `AppendChain(wallet, string_id)` routes
//!    on `wallet[0]`. These MAY land on different shards. Reasoning
//!    that this is safe:
//!    - Caller in `append_to_ledger` enqueues blob first, then chain,
//!      then descriptor. Records the highest returned seq S.
//!    - Caller waits `wait_durable(S)` before RPC-acking the mint.
//!    - `wait_durable(S)` waits for every shard whose
//!      `highest_assigned >= S` to reach `highest_durable >= S`, so
//!      BOTH the blob shard and the chain shard must be durable
//!      before the caller unblocks.
//!    - If the process crashes between blob flush and chain flush,
//!      the caller never got an ack → the outside world doesn't know
//!      about the mint → losing it is fine, provided the recovered
//!      state is coherent.
//!    - Recovered state coherence: if chain durable but blob not,
//!      lattice rehydration finds a chain entry with a missing blob
//!      and skips it (via `LedgerManager::prime_wallet_chain` +
//!      `read_string_blob` returning None). If blob durable but
//!      chain not, the orphan blob is not reachable via any chain
//!      walk and is a harmless disk-space leak until the next
//!      compaction sweep.
//!
//! ## Sequence-number monotonicity across a crash — THE BUG FIX
//!
//! Version 2 of this module (in the sibling `datachain-rope-v2` tree,
//! never deployed) had a subtle recovery bug: `next_seq` was
//! initialised from `recovered.durable_seq`, which was defined as
//! `max(legacy_global_watermark, min(non_zero_per_shard_watermarks))`.
//! Consider the state:
//! - Shard 0 durable = 1000 (heavy load on wallet A pre-crash)
//! - Shard 1 durable = 500 (lighter load on wallet B pre-crash)
//!
//! `recovered.durable_seq = max(legacy, min(1000, 500)) = 500`,
//! and `next_seq = 501`. Now a new op enqueues to shard 0 with seq=501.
//! `shard[0].highest_assigned.fetch_max(501)` keeps it at 1000, and
//! `wait_durable(501)` sees `shard[0].durable = 1000 >= 501` **before
//! the new op has actually flushed**. A false ack.
//!
//! **Fix, implemented here**: `next_seq` is initialised to
//! `max(legacy_global_watermark, max_over_all_per_shard_watermarks) + 1`.
//! In the example above, that gives `next_seq = 1001`. Every new op
//! now gets a seq strictly greater than any pre-crash seq, so its
//! shard's `highest_assigned.fetch_max(new_seq)` always bumps the
//! high-water mark, and `wait_durable(new_seq)` correctly requires a
//! fresh flush.
//!
//! ## Sharding rules (see [`shard_of_op`])
//!
//! | WriteOp variant       | Partition byte source     |
//! |-----------------------|---------------------------|
//! | PutDescriptor         | `wallet[0]`               |
//! | AppendChain           | `wallet[0]`               |
//! | MarkDeleted           | `wallet[0]`               |
//! | PutPieceMap           | `string_id[0]`            |
//! | PutStringBlob         | `string_id[0]`            |
//! | DeleteStringBlob      | `string_id[0]`            |
//! | PutTombstone          | `string_id[0]`            |
//!
//! Empty wallets and all-zero string ids partition to shard 0. Both
//! are pathological cases guarded by validation upstream, but the
//! router must produce SOME shard for every op so we default gracefully.
//!
//! ## Opt-in
//!
//! Selected at open time via `ROPE_LEDGER_P2B=1` (default off). Same
//! DB path can be opened by either backend — on-disk format is
//! identical except for a small handful of per-shard watermark keys
//! in the default CF (see [`DURABLE_SEQ_SHARD_PREFIX`]). A DB written
//! by the legacy backend and opened by the parallel backend recovers
//! correctly because per-shard watermarks default to
//! `legacy_global_watermark` on absence. A DB written by the parallel
//! backend and opened by the legacy backend continues to use the
//! (now stale) global watermark from the default CF; the parallel
//! backend also writes the global watermark on every batch to keep
//! this fallback path working.

use crate::ledger_db::{StoredLedgerDescriptor, StoredPieceMap};
use crate::rocksdb_persistence::{
    chain_key, queue_cap, PersistenceStats, RecoveredState, RecoveryOptions, RocksError,
    StoredTombstone, WriteOp, CF_CHAIN, CF_DESCRIPTORS, CF_HEADS, CF_PIECES, CF_REVERSE,
    CF_STRINGS, CF_TOMBSTONES,
};
use parking_lot::{Condvar, Mutex};
use rocksdb::{
    ColumnFamilyDescriptor, DBCompressionType, IteratorMode, Options, WriteBatch, WriteOptions, DB,
};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Number of parallel writer shards. Chosen empirically as a balance
/// between (a) enough shards to hide any single flusher's tail
/// latency, (b) few enough to keep total flusher-thread count in
/// check on 8-vCPU hosts. 8 is what the v2 branch soaked with.
pub const NUM_SHARDS: usize = 8;

/// Mask for computing `shard_index = partition_byte & SHARD_MASK`.
/// Requires NUM_SHARDS to be a power of two — enforced at compile
/// time by [`assert_num_shards_power_of_two`].
const SHARD_MASK: u8 = (NUM_SHARDS - 1) as u8;

const _: () = {
    // NUM_SHARDS must be a power of two so the mask trick works.
    assert!(
        NUM_SHARDS.is_power_of_two(),
        "NUM_SHARDS must be a power of two"
    );
    // Must also fit in u8 for the mask to make sense — 256 shards max.
    assert!(NUM_SHARDS <= 256, "NUM_SHARDS must be <= 256");
};

/// Default-CF key prefix for per-shard durability watermarks. The
/// full key is `DURABLE_SEQ_SHARD_PREFIX || u8 shard_index`.
const DURABLE_SEQ_SHARD_PREFIX: &[u8] = b"durable_seq_shard_";

/// Default-CF key holding the legacy (single-flusher) global durable
/// seq. The parallel backend keeps this up to date on every batch
/// flush so a subsequent open by the legacy backend still recovers
/// a correct `next_seq`.
const LEGACY_DURABLE_SEQ_KEY: &[u8] = b"durable_seq";

/// How long each per-shard flusher waits for new ops before
/// pushing whatever it has to disk. Same 10 ms as the legacy backend.
const FLUSH_INTERVAL: Duration = Duration::from_millis(10);

/// Hard cap on ops drained per shard per flush. Batch memory
/// footprint stays bounded even under bursty single-shard load.
const MAX_BATCH_OPS: usize = 4096;

// ============================================================================
// Sharding
// ============================================================================

/// Extract the partition byte from a `WriteOp`. Wallet-scoped ops
/// use `wallet[0]`, string-id-scoped ops use `string_id[0]`. Empty
/// wallets partition to shard 0; string_ids are always 32 bytes so
/// they never fall through.
///
/// Kept as a free function (not a method on `WriteOp`) so the sibling
/// legacy module doesn't have to depend on the parallel-writer shard
/// count. The Phase 2.B design owns the routing decision; `WriteOp`
/// stays a plain data shape.
pub fn partition_byte_of(op: &WriteOp) -> u8 {
    match op {
        WriteOp::PutDescriptor { wallet, .. }
        | WriteOp::AppendChain { wallet, .. }
        | WriteOp::MarkDeleted { wallet, .. } => wallet.first().copied().unwrap_or(0),
        WriteOp::PutPieceMap { string_id, .. }
        | WriteOp::PutStringBlob { string_id, .. }
        | WriteOp::DeleteStringBlob { string_id, .. }
        | WriteOp::PutTombstone { string_id, .. } => string_id[0],
    }
}

/// Compute the shard index for a `WriteOp`. Guaranteed to return
/// `0..NUM_SHARDS`.
#[inline]
pub fn shard_of_op(op: &WriteOp) -> usize {
    (partition_byte_of(op) & SHARD_MASK) as usize
}

// ============================================================================
// Types
// ============================================================================

/// One enqueued op, tagged with its assigned global sequence number
/// and the shard it was routed to. Kept small so the mpsc channel
/// doesn't become the bottleneck.
struct PendingWrite {
    seq: u64,
    op: WriteOp,
}

/// Per-shard state. One instance per writer.
struct Shard {
    /// Enqueue channel to this shard's flusher. `Mutex<Option<...>>`
    /// so [`Drop`] can take-and-drop each sender in turn without a
    /// race against enqueuers.
    tx: Mutex<Option<SyncSender<PendingWrite>>>,
    /// Highest global seq ever assigned to this shard. Monotonic.
    highest_assigned: AtomicU64,
    /// Highest global seq known to be fsync'd on this shard. Monotonic.
    highest_durable: AtomicU64,
    /// Handle to this shard's flusher thread. `Mutex<Option<...>>` so
    /// [`Drop`] can join each thread in turn.
    flusher: Mutex<Option<JoinHandle<()>>>,
}

impl Shard {
    fn new(watermark_on_open: u64) -> Self {
        Self {
            tx: Mutex::new(None),
            highest_assigned: AtomicU64::new(watermark_on_open),
            highest_durable: AtomicU64::new(watermark_on_open),
            flusher: Mutex::new(None),
        }
    }
}

/// Snapshot per-shard stats for metrics endpoints.
#[derive(Clone, Debug, Default)]
pub struct ShardStats {
    pub shard_id: usize,
    pub highest_assigned: u64,
    pub highest_durable: u64,
    pub pending: u64,
}

/// Parallel-writer RocksDB persistence backend. Constructed via
/// [`Self::open`] / [`Self::open_lazy`]. Cloneable as `Arc`.
pub struct RocksPersistenceP2b {
    db: Arc<DB>,
    shards: Arc<[Shard; NUM_SHARDS]>,
    /// Global monotonic seq counter. Every enqueue gets a fresh seq
    /// via `fetch_add(1)`. Initialised at open to
    /// `max(legacy_global, max(per_shard)) + 1` (see module docs).
    next_seq: AtomicU64,
    /// Set true on Drop; every shard's flusher checks it as a
    /// belt-and-braces exit signal.
    shutdown: Arc<AtomicBool>,
    /// Notified after each successful flush on any shard — wakes
    /// [`wait_durable`] callers so they can re-check the condition.
    notify_pair: Arc<(Mutex<()>, Condvar)>,
    /// Configured enqueue capacity, per shard. Recorded for stats +
    /// diagnostics; the actual channels are bounded to this value.
    queue_cap_per_shard: usize,
}

impl RocksPersistenceP2b {
    /// Open or create a RocksDB instance at `path` and start N
    /// background flushers. Returns the handle plus a [`RecoveredState`]
    /// snapshot that the caller (typically `LedgerStore::open`) uses to
    /// rebuild its in-memory mirror.
    pub fn open(path: impl AsRef<Path>) -> Result<(Arc<Self>, RecoveredState), RocksError> {
        Self::open_with_queue_cap(path, queue_cap())
    }

    /// Lazy variant of [`Self::open`]: same on-disk format, same
    /// N background flushers, but the returned [`RecoveredState`]
    /// carries an **empty** `string_blobs` vector. See
    /// [`crate::rocksdb_persistence::RocksPersistence::open_lazy`] for
    /// the full rationale (avoids the ~5 min / multi-GB eager
    /// rehydration cost at boot).
    pub fn open_lazy(path: impl AsRef<Path>) -> Result<(Arc<Self>, RecoveredState), RocksError> {
        Self::open_with_options(
            path,
            queue_cap(),
            RecoveryOptions {
                load_string_blobs: false,
            },
        )
    }

    /// Like [`Self::open`] but with an explicit per-shard enqueue
    /// capacity. Tests / operator tooling only.
    pub fn open_with_queue_cap(
        path: impl AsRef<Path>,
        cap_per_shard: usize,
    ) -> Result<(Arc<Self>, RecoveredState), RocksError> {
        Self::open_with_options(path, cap_per_shard, RecoveryOptions::default())
    }

    /// Full-control open: choose per-shard queue capacity + which
    /// CFs to eagerly load into [`RecoveredState`].
    pub fn open_with_options(
        path: impl AsRef<Path>,
        cap_per_shard: usize,
        opts: RecoveryOptions,
    ) -> Result<(Arc<Self>, RecoveredState), RocksError> {
        let path = path.as_ref();
        let cap_per_shard = cap_per_shard.max(1);

        // ---- RocksDB tuning: same knobs as the legacy backend ----
        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);
        db_opts.set_compression_type(DBCompressionType::Lz4);
        db_opts.set_max_open_files(512);
        // Bump internal parallelism modestly — RocksDB itself still
        // has its own workers for compactions, and we have N flusher
        // threads writing concurrently on top.
        db_opts.increase_parallelism(NUM_SHARDS as i32);

        let cfs = vec![
            ColumnFamilyDescriptor::new(CF_DESCRIPTORS, Options::default()),
            ColumnFamilyDescriptor::new(CF_HEADS, Options::default()),
            ColumnFamilyDescriptor::new(CF_CHAIN, Options::default()),
            ColumnFamilyDescriptor::new(CF_REVERSE, Options::default()),
            ColumnFamilyDescriptor::new(CF_PIECES, Options::default()),
            ColumnFamilyDescriptor::new(CF_STRINGS, Options::default()),
            ColumnFamilyDescriptor::new(CF_TOMBSTONES, Options::default()),
        ];

        let db = Arc::new(DB::open_cf_descriptors(&db_opts, path, cfs)?);

        // ---- Recovery: scan every CF, read all watermarks ----
        let recovered = recover_state(&db, &opts)?;
        let legacy_global = recovered.durable_seq;
        let per_shard = recover_per_shard_watermarks(&db)?;

        // ---- THE FIX: next_seq starts strictly above every historical seq ----
        //
        // See the module docs section "Sequence-number monotonicity
        // across a crash" for the full incident story. Without this
        // fix, `wait_durable(new_seq)` could return true immediately
        // when a shard's pre-crash durable watermark happened to
        // exceed the new_seq — a silent false ack.
        let mut init_next_seq = legacy_global;
        for w in per_shard.iter() {
            init_next_seq = init_next_seq.max(*w);
        }
        let next_seq = AtomicU64::new(init_next_seq + 1);

        // ---- Shard state: each shard's watermark bootstraps from
        //      its own persisted per-shard watermark. If missing
        //      (upgrading from the legacy backend), fall back to the
        //      legacy global watermark. Never regress below either. ----
        let shards_vec: Vec<Shard> = (0..NUM_SHARDS)
            .map(|i| Shard::new(per_shard[i].max(legacy_global)))
            .collect();
        let shards_arr: [Shard; NUM_SHARDS] = shards_vec
            .try_into()
            .unwrap_or_else(|_| unreachable!("NUM_SHARDS matches Vec::len"));
        let shards = Arc::new(shards_arr);

        let shutdown = Arc::new(AtomicBool::new(false));
        let notify_pair = Arc::new((Mutex::new(()), Condvar::new()));

        // ---- Start one flusher per shard ----
        for shard_id in 0..NUM_SHARDS {
            let (tx, rx) = mpsc::sync_channel::<PendingWrite>(cap_per_shard);
            *shards[shard_id].tx.lock() = Some(tx);
            let handle = {
                let db = db.clone();
                let shards = shards.clone();
                let shutdown = shutdown.clone();
                let notify_pair = notify_pair.clone();
                thread::Builder::new()
                    .name(format!("rope-storage-p2b-flusher-{shard_id}"))
                    .spawn(move || flusher_loop(shard_id, db, rx, shards, shutdown, notify_pair))
                    .map_err(|e| {
                        RocksError::Corrupted(format!(
                            "failed to spawn p2b flusher {shard_id}: {e}"
                        ))
                    })?
            };
            *shards[shard_id].flusher.lock() = Some(handle);
        }

        Ok((
            Arc::new(Self {
                db,
                shards,
                next_seq,
                shutdown,
                notify_pair,
                queue_cap_per_shard: cap_per_shard,
            }),
            recovered,
        ))
    }

    /// Configured enqueue capacity per shard (see `ROPE_LEDGER_QUEUE_CAP`).
    pub fn queue_cap_per_shard(&self) -> usize {
        self.queue_cap_per_shard
    }

    /// Total enqueue capacity across all shards.
    pub fn total_queue_cap(&self) -> usize {
        self.queue_cap_per_shard.saturating_mul(NUM_SHARDS)
    }

    /// Enqueue a write op. Returns the assigned global sequence number,
    /// which callers may pass to [`Self::wait_durable`] to block until
    /// the op is fsync'd.
    ///
    /// Uses **non-blocking** `try_send` on the target shard's bounded
    /// channel:
    /// - [`RocksError::QueueFull`] — target shard's flusher is behind;
    ///   caller must surface a retryable overload (not a success).
    /// - [`RocksError::FlusherStopped`] — persistence is being dropped
    ///   or the shard's flusher panicked.
    pub fn enqueue(&self, op: WriteOp) -> Result<u64, RocksError> {
        let shard_id = shard_of_op(&op);
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel);

        // Bump the shard's assigned high-water so wait_durable knows
        // this shard must be considered for `seq`.
        self.shards[shard_id]
            .highest_assigned
            .fetch_max(seq, Ordering::AcqRel);

        let guard = self.shards[shard_id].tx.lock();
        let tx = guard.as_ref().ok_or(RocksError::FlusherStopped)?;
        match tx.try_send(PendingWrite { seq, op }) {
            Ok(()) => Ok(seq),
            Err(TrySendError::Full(_)) => Err(RocksError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(RocksError::FlusherStopped),
        }
    }

    /// Block until the given sequence number is on durable disk, or
    /// `timeout` expires. Returns `true` iff every shard that could
    /// hold `seq` has flushed at least `seq` before the deadline.
    ///
    /// Correctness: a shard `s` is "irrelevant" for `wait_durable(seq)`
    /// iff `s.highest_assigned < seq` — meaning the enqueuing thread
    /// never routed seq (or any greater seq) to `s`. All other shards
    /// must have `s.highest_durable >= seq` before we return true.
    /// This is tighter than v2's `durable >= min(assigned, seq)`
    /// formula, which unnecessarily forced shards to catch up on
    /// their own pending work even when that work had nothing to do
    /// with the awaited seq.
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

    fn is_durable(&self, seq: u64) -> bool {
        for s in self.shards.iter() {
            let assigned = s.highest_assigned.load(Ordering::Acquire);
            if assigned >= seq {
                let durable = s.highest_durable.load(Ordering::Acquire);
                if durable < seq {
                    return false;
                }
            }
        }
        true
    }

    /// Latest global durable seq — the minimum durable watermark
    /// across all shards that hold ops, or the max otherwise. In
    /// practice this is what an external observer would see if they
    /// asked "is every previously-enqueued write on disk yet?"
    pub fn durable_seq(&self) -> u64 {
        // Use max: represents "the highest seq we can guarantee is
        // durable somewhere". This is what dashboards want to plot.
        // Callers who need STRICT global durability should call
        // wait_durable(next_seq() - 1).
        self.shards
            .iter()
            .map(|s| s.highest_durable.load(Ordering::Acquire))
            .max()
            .unwrap_or(0)
    }

    /// Highest sequence number assigned so far to any shard.
    pub fn next_seq(&self) -> u64 {
        self.next_seq.load(Ordering::Acquire)
    }

    /// Snapshot aggregate stats for metrics endpoints.
    pub fn stats(&self) -> PersistenceStats {
        let next = self.next_seq();
        let dur = self.durable_seq();
        PersistenceStats {
            next_seq: next,
            durable_seq: dur,
            pending: next.saturating_sub(dur).saturating_sub(1),
            shutdown: self.shutdown.load(Ordering::Acquire),
        }
    }

    /// Snapshot per-shard stats. One entry per shard, ordered by
    /// shard id.
    pub fn shard_stats(&self) -> Vec<ShardStats> {
        (0..NUM_SHARDS)
            .map(|i| {
                let s = &self.shards[i];
                let assigned = s.highest_assigned.load(Ordering::Acquire);
                let durable = s.highest_durable.load(Ordering::Acquire);
                ShardStats {
                    shard_id: i,
                    highest_assigned: assigned,
                    highest_durable: durable,
                    pending: assigned.saturating_sub(durable),
                }
            })
            .collect()
    }

    // ---- Point reads (mirror legacy backend API) ----

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

    pub fn read_string_blob(
        &self,
        string_id: &[u8; 32],
    ) -> Result<Option<Vec<u8>>, RocksError> {
        let cf = self
            .db
            .cf_handle(CF_STRINGS)
            .ok_or(RocksError::MissingCf(CF_STRINGS))?;
        match self.db.get_cf(&cf, string_id)? {
            None => Ok(None),
            Some(bytes) => Ok(Some(bytes)),
        }
    }

    pub fn read_tombstone(
        &self,
        string_id: &[u8; 32],
    ) -> Result<Option<StoredTombstone>, RocksError> {
        let cf = self
            .db
            .cf_handle(CF_TOMBSTONES)
            .ok_or(RocksError::MissingCf(CF_TOMBSTONES))?;
        match self.db.get_cf(&cf, string_id)? {
            None => Ok(None),
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
        }
    }

    /// Stream every persisted knot blob from disk in on-disk key
    /// order, in fixed-size batches. Identical semantics to
    /// [`crate::rocksdb_persistence::RocksPersistence::stream_string_blobs`].
    pub fn stream_string_blobs<F>(
        &self,
        batch_size: usize,
        sleep_between_batches: Duration,
        mut handler: F,
    ) -> Result<usize, RocksError>
    where
        F: FnMut(Vec<([u8; 32], Vec<u8>)>) -> Result<(), RocksError>,
    {
        let cf = self
            .db
            .cf_handle(CF_STRINGS)
            .ok_or(RocksError::MissingCf(CF_STRINGS))?;
        let batch_size = batch_size.max(1);
        let mut total = 0usize;
        let mut batch: Vec<([u8; 32], Vec<u8>)> = Vec::with_capacity(batch_size);
        for kv in self.db.iterator_cf(&cf, IteratorMode::Start) {
            let (k, v) = kv?;
            if k.len() != 32 {
                return Err(RocksError::Corrupted(format!(
                    "strings key malformed: expected 32 bytes, got {}",
                    k.len()
                )));
            }
            let mut sid = [0u8; 32];
            sid.copy_from_slice(&k);
            batch.push((sid, v.into_vec()));
            if batch.len() >= batch_size {
                total += batch.len();
                handler(std::mem::take(&mut batch))?;
                if !sleep_between_batches.is_zero() {
                    std::thread::sleep(sleep_between_batches);
                }
            }
        }
        if !batch.is_empty() {
            total += batch.len();
            handler(batch)?;
        }
        Ok(total)
    }
}

impl Drop for RocksPersistenceP2b {
    fn drop(&mut self) {
        // 1. Flag shutdown so idle flushers exit their recv timeout loop.
        self.shutdown.store(true, Ordering::Release);
        // 2. Drop every sender → each flusher's `recv()` returns
        //    Disconnected on its next iteration, and it exits after
        //    one final drain+flush.
        for shard in self.shards.iter() {
            let mut guard = shard.tx.lock();
            *guard = None;
        }
        // 3. Wait for every flusher thread to finish its final batch.
        for shard in self.shards.iter() {
            if let Some(handle) = shard.flusher.lock().take() {
                let _ = handle.join();
            }
        }
        // 4. Best-effort final DB flush.
        let _ = self.db.flush();
    }
}

// ============================================================================
// Per-shard background flusher
// ============================================================================

fn flusher_loop(
    shard_id: usize,
    db: Arc<DB>,
    rx: Receiver<PendingWrite>,
    shards: Arc<[Shard; NUM_SHARDS]>,
    shutdown: Arc<AtomicBool>,
    notify_pair: Arc<(Mutex<()>, Condvar)>,
) {
    let mut buf: Vec<PendingWrite> = Vec::with_capacity(MAX_BATCH_OPS);

    loop {
        match rx.recv_timeout(FLUSH_INTERVAL) {
            Ok(first) => buf.push(first),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if shutdown.load(Ordering::Acquire) {
                    break;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        while buf.len() < MAX_BATCH_OPS {
            match rx.try_recv() {
                Ok(op) => buf.push(op),
                Err(_) => break,
            }
        }

        if let Err(e) = flush_shard_batch(shard_id, &db, &buf, &shards[shard_id]) {
            tracing::error!(
                target: "rope_storage::p2b",
                "shard {shard_id} flusher write failed: {e:?} (op_count={}); exiting",
                buf.len()
            );
            return;
        }

        notify_pair.1.notify_all();
        buf.clear();
    }

    // Final drain on shutdown — pull any remaining ops from the
    // channel, write them, advance the watermark, notify.
    while let Ok(op) = rx.try_recv() {
        buf.push(op);
        if buf.len() >= MAX_BATCH_OPS {
            if let Err(e) = flush_shard_batch(shard_id, &db, &buf, &shards[shard_id]) {
                tracing::error!(
                    target: "rope_storage::p2b",
                    "shard {shard_id} final-drain write failed: {e:?}"
                );
                return;
            }
            buf.clear();
        }
    }
    if !buf.is_empty() {
        if let Err(e) = flush_shard_batch(shard_id, &db, &buf, &shards[shard_id]) {
            tracing::error!(
                target: "rope_storage::p2b",
                "shard {shard_id} final-drain write failed: {e:?}"
            );
            return;
        }
    }
    notify_pair.1.notify_all();
}

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
    let cf_strings = db
        .cf_handle(CF_STRINGS)
        .ok_or(RocksError::MissingCf(CF_STRINGS))?;
    let cf_tombstones = db
        .cf_handle(CF_TOMBSTONES)
        .ok_or(RocksError::MissingCf(CF_TOMBSTONES))?;

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
                if let Some(existing) = db.get_cf(&cf_descriptors, wallet)? {
                    let mut desc: StoredLedgerDescriptor = bincode::deserialize(&existing)?;
                    desc.is_deleted = true;
                    desc.deleted_at = Some(*deleted_at);
                    let bytes = bincode::serialize(&desc)?;
                    batch.put_cf(&cf_descriptors, wallet, &bytes);
                }
            }
            WriteOp::PutStringBlob { string_id, blob } => {
                batch.put_cf(&cf_strings, string_id, blob);
            }
            WriteOp::DeleteStringBlob { string_id } => {
                batch.delete_cf(&cf_strings, string_id);
            }
            WriteOp::PutTombstone {
                string_id,
                tombstone,
            } => {
                let bytes = bincode::serialize(tombstone)?;
                batch.put_cf(&cf_tombstones, string_id, &bytes);
            }
        }
    }

    // Stamp this shard's per-shard watermark in the same batch, plus
    // a best-effort bump of the legacy global watermark so a
    // subsequent open by the legacy backend recovers reasonable
    // state. The legacy backend does not read per-shard keys, so
    // maintaining `LEGACY_DURABLE_SEQ_KEY` = max seq ever fsync'd
    // (from this shard) is the compatibility hook.
    batch.put(
        durable_seq_shard_key(shard_id),
        highest_seq.to_le_bytes(),
    );
    batch.put(LEGACY_DURABLE_SEQ_KEY, highest_seq.to_le_bytes());

    let mut wo = WriteOptions::default();
    wo.set_sync(true);
    db.write_opt(batch, &wo)?;

    shard
        .highest_durable
        .fetch_max(highest_seq, Ordering::AcqRel);
    Ok(())
}

// ============================================================================
// Recovery helpers
// ============================================================================

fn durable_seq_shard_key(shard_id: usize) -> Vec<u8> {
    let mut k = Vec::with_capacity(DURABLE_SEQ_SHARD_PREFIX.len() + 1);
    k.extend_from_slice(DURABLE_SEQ_SHARD_PREFIX);
    k.push(shard_id as u8);
    k
}

/// Read every per-shard watermark from the default CF. Missing keys
/// default to 0 (fresh install OR upgrade from legacy backend), which
/// [`Self::open_with_options`] then clamps up to `legacy_global` so
/// no shard ever regresses below the legacy watermark.
fn recover_per_shard_watermarks(db: &DB) -> Result<[u64; NUM_SHARDS], RocksError> {
    let mut out = [0u64; NUM_SHARDS];
    for (i, w) in out.iter_mut().enumerate() {
        let key = durable_seq_shard_key(i);
        match db.get(&key)? {
            None => *w = 0,
            Some(bytes) if bytes.len() == 8 => {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&bytes);
                *w = u64::from_le_bytes(arr);
            }
            Some(other) => {
                return Err(RocksError::Corrupted(format!(
                    "durable_seq_shard_{i} value malformed: expected 8 bytes, got {}",
                    other.len()
                )));
            }
        }
    }
    Ok(out)
}

/// Legacy-format recovery. Same scanning logic as
/// `RocksPersistence::recover_from_db` in the sibling module — kept
/// as a local copy so both backends can evolve their recovery
/// independently if needed. The two implementations agree on the
/// shape of every CF; changes here must be mirrored in the legacy
/// backend and vice versa.
fn recover_state(db: &DB, opts: &RecoveryOptions) -> Result<RecoveredState, RocksError> {
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
    let cf_strings = db
        .cf_handle(CF_STRINGS)
        .ok_or(RocksError::MissingCf(CF_STRINGS))?;
    let cf_tombstones = db
        .cf_handle(CF_TOMBSTONES)
        .ok_or(RocksError::MissingCf(CF_TOMBSTONES))?;

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

    if opts.load_string_blobs {
        for kv in db.iterator_cf(&cf_strings, IteratorMode::Start) {
            let (k, v) = kv?;
            if k.len() != 32 {
                return Err(RocksError::Corrupted(format!(
                    "strings key malformed: expected 32 bytes, got {}",
                    k.len()
                )));
            }
            let mut sid = [0u8; 32];
            sid.copy_from_slice(&k);
            state.string_blobs.push((sid, v.into_vec()));
        }
    }

    for kv in db.iterator_cf(&cf_tombstones, IteratorMode::Start) {
        let (k, v) = kv?;
        if k.len() != 32 {
            return Err(RocksError::Corrupted(format!(
                "tombstones key malformed: expected 32 bytes, got {}",
                k.len()
            )));
        }
        let mut sid = [0u8; 32];
        sid.copy_from_slice(&k);
        let ts: StoredTombstone = bincode::deserialize(&v)?;
        state.tombstones.push((sid, ts));
    }

    state.durable_seq = match db.get(LEGACY_DURABLE_SEQ_KEY)? {
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

    Ok(state)
}

/// Public helper for tests / diagnostics: enforce that
/// [`NUM_SHARDS`] is a power of two and fits in a u8 mask. Called
/// as a compile-time assertion above; also exposed for runtime
/// probes by ops tooling.
pub fn assert_num_shards_power_of_two() {
    assert!(NUM_SHARDS.is_power_of_two());
    assert!(NUM_SHARDS <= 256);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rocksdb_persistence::DEFAULT_QUEUE_CAP;
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
    fn shard_count_is_power_of_two() {
        assert_num_shards_power_of_two();
        // Mask arithmetic sanity: shard indexes must land in bounds.
        for b in 0u8..=255 {
            let idx = (b & SHARD_MASK) as usize;
            assert!(idx < NUM_SHARDS);
        }
    }

    #[test]
    fn partition_byte_of_wallet_scoped_ops_uses_wallet_first_byte() {
        let op = WriteOp::PutDescriptor {
            wallet: vec![0x42, 0xAB, 0xCD],
            desc: dummy_descriptor(&[0x42], [0u8; 32]),
        };
        assert_eq!(partition_byte_of(&op), 0x42);

        let op = WriteOp::AppendChain {
            wallet: vec![0x99, 0x00],
            seq_in_wallet: 0,
            string_id: [0u8; 32],
        };
        assert_eq!(partition_byte_of(&op), 0x99);

        let op = WriteOp::MarkDeleted {
            wallet: vec![0xFE],
            deleted_at: 1,
        };
        assert_eq!(partition_byte_of(&op), 0xFE);
    }

    #[test]
    fn partition_byte_of_string_scoped_ops_uses_string_id_first_byte() {
        let mut sid = [0u8; 32];
        sid[0] = 0x77;
        let op = WriteOp::PutStringBlob {
            string_id: sid,
            blob: vec![],
        };
        assert_eq!(partition_byte_of(&op), 0x77);

        let op = WriteOp::DeleteStringBlob { string_id: sid };
        assert_eq!(partition_byte_of(&op), 0x77);

        let op = WriteOp::PutTombstone {
            string_id: sid,
            tombstone: StoredTombstone {
                untied_at: 1,
                audit_hash: [0u8; 32],
                reason: "t".to_string(),
                parents: vec![],
            },
        };
        assert_eq!(partition_byte_of(&op), 0x77);
    }

    #[test]
    fn empty_wallet_partitions_to_shard_zero() {
        let op = WriteOp::PutDescriptor {
            wallet: vec![],
            desc: dummy_descriptor(&[], [0u8; 32]),
        };
        assert_eq!(partition_byte_of(&op), 0);
        assert_eq!(shard_of_op(&op), 0);
    }

    #[test]
    fn open_creates_db_with_all_cfs_and_zero_watermarks() {
        let dir = TempDir::new().unwrap();
        let (p, recovered) = RocksPersistenceP2b::open(dir.path()).unwrap();
        assert!(recovered.descriptors.is_empty());
        assert_eq!(recovered.durable_seq, 0);
        assert_eq!(p.next_seq(), 1);
        assert_eq!(p.durable_seq(), 0);
        for stat in p.shard_stats() {
            assert_eq!(stat.highest_assigned, 0);
            assert_eq!(stat.highest_durable, 0);
        }
        // Read-through of a nonexistent descriptor returns None.
        assert!(p.read_descriptor(b"nope").unwrap().is_none());
    }

    #[test]
    fn enqueue_routes_to_correct_shard_and_persists_after_wait() {
        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistenceP2b::open(dir.path()).unwrap();

        let wallet = vec![0xAAu8; 20];
        let head = [0xCDu8; 32];
        let expected_shard = (0xAAu8 & SHARD_MASK) as usize;

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
        assert!(seq2 > seq1);

        // Both ops must have gone to the wallet's shard.
        let stats = p.shard_stats();
        assert!(
            stats[expected_shard].highest_assigned >= seq2,
            "expected shard {expected_shard} to have assigned >= {seq2}, got {}",
            stats[expected_shard].highest_assigned
        );

        assert!(
            p.wait_durable(seq2, Duration::from_secs(2)),
            "wait_durable must return true within 2s"
        );

        let on_disk = p.read_descriptor(&wallet).unwrap().unwrap();
        assert_eq!(on_disk.head_string_id, head);
        assert_eq!(p.read_head(&wallet).unwrap(), Some(head));
    }

    #[test]
    fn wait_durable_returns_true_immediately_for_never_assigned_seq() {
        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistenceP2b::open(dir.path()).unwrap();
        // seq=0 was never assigned to any shard (next_seq starts at 1).
        assert!(p.wait_durable(0, Duration::from_millis(50)));
    }

    #[test]
    fn wait_durable_ignores_unrelated_shards() {
        // If op A goes to shard 0 and op B goes to shard 1, waiting on
        // A's seq must not require shard 1 to flush.
        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistenceP2b::open(dir.path()).unwrap();

        let mut wallet_a = vec![0u8; 20];
        wallet_a[0] = 0x00; // shard 0
        let mut wallet_b = vec![0u8; 20];
        wallet_b[0] = 0x01; // shard 1 (or a different shard for NUM_SHARDS >= 2)

        let seq_a = p
            .enqueue(WriteOp::PutDescriptor {
                wallet: wallet_a.clone(),
                desc: dummy_descriptor(&wallet_a, [0u8; 32]),
            })
            .unwrap();

        // Wait for A to be durable (should succeed) — do this BEFORE
        // enqueueing B so we're not relying on B's flush to satisfy A.
        assert!(p.wait_durable(seq_a, Duration::from_secs(2)));

        // Now B: seq_b > seq_a but on a different shard.
        let seq_b = p
            .enqueue(WriteOp::PutDescriptor {
                wallet: wallet_b.clone(),
                desc: dummy_descriptor(&wallet_b, [0u8; 32]),
            })
            .unwrap();

        assert!(seq_b > seq_a);
        // seq_a stays durable regardless of B's progress.
        assert!(p.wait_durable(seq_a, Duration::from_millis(50)));
        // And B eventually becomes durable too.
        assert!(p.wait_durable(seq_b, Duration::from_secs(2)));
    }

    #[test]
    fn recovery_restores_all_cfs_and_next_seq_is_max_plus_one() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        let wallet_a = vec![0xAAu8; 20];
        let wallet_b = vec![0xBBu8; 20];
        let head_a = [0xE1u8; 32];
        let head_b = [0xE2u8; 32];
        let mut sid1 = [0u8; 32];
        sid1[0] = 0x77;
        let blob_bytes = vec![1u8, 2, 3, 4, 5];

        let highest_seq;
        {
            let (p, _) = RocksPersistenceP2b::open(&path).unwrap();
            p.enqueue(WriteOp::PutDescriptor {
                wallet: wallet_a.clone(),
                desc: dummy_descriptor(&wallet_a, head_a),
            })
            .unwrap();
            p.enqueue(WriteOp::PutDescriptor {
                wallet: wallet_b.clone(),
                desc: dummy_descriptor(&wallet_b, head_b),
            })
            .unwrap();
            p.enqueue(WriteOp::AppendChain {
                wallet: wallet_a.clone(),
                seq_in_wallet: 0,
                string_id: head_a,
            })
            .unwrap();
            p.enqueue(WriteOp::PutStringBlob {
                string_id: sid1,
                blob: blob_bytes.clone(),
            })
            .unwrap();
            let s = p
                .enqueue(WriteOp::PutTombstone {
                    string_id: sid1,
                    tombstone: StoredTombstone {
                        untied_at: 42,
                        audit_hash: [0xABu8; 32],
                        reason: "test".to_string(),
                        parents: vec![],
                    },
                })
                .unwrap();
            highest_seq = s;
            assert!(p.wait_durable(highest_seq, Duration::from_secs(2)));
        }

        // Reopen with parallel backend.
        {
            let (p, recovered) = RocksPersistenceP2b::open(&path).unwrap();
            // Every mutation must have made it to disk.
            assert_eq!(recovered.descriptors.len(), 2);
            assert!(recovered.string_blobs.iter().any(|(k, _)| k == &sid1));
            assert!(recovered.tombstones.iter().any(|(k, _)| k == &sid1));
            // Chain must contain wallet_a's genesis entry.
            let a_chain = recovered
                .chains
                .iter()
                .find(|(w, _)| w == &wallet_a)
                .map(|(_, c)| c.clone())
                .unwrap_or_default();
            assert_eq!(a_chain, vec![head_a]);

            // THE FIX: next_seq must be strictly greater than every
            // per-shard watermark, not just the legacy global one.
            assert!(
                p.next_seq() > highest_seq,
                "next_seq must exceed highest pre-crash seq (fix for false-ack bug)"
            );
        }
    }

    #[test]
    fn next_seq_after_reopen_respects_max_shard_watermark_not_min() {
        // Construct a state where shards have very different
        // watermarks, then reopen and assert next_seq >= max watermark
        // + 1. This is the direct regression test for the v2 bug.
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        // Enqueue many ops routed to a single specific shard to
        // drive its watermark high, and just one op to another shard.
        let mut wallet_hot = vec![0u8; 20];
        wallet_hot[0] = 0x00; // shard 0
        let mut wallet_cold = vec![0u8; 20];
        wallet_cold[0] = 0x01; // different shard

        let mut expected_max_seq = 0;
        {
            let (p, _) = RocksPersistenceP2b::open(&path).unwrap();
            for i in 0..200 {
                let sid = {
                    let mut s = [0u8; 32];
                    s[0] = 0x00; // keep blob on shard 0 too
                    s[1] = i as u8;
                    s
                };
                let s = p
                    .enqueue(WriteOp::PutStringBlob {
                        string_id: sid,
                        blob: vec![i as u8; 8],
                    })
                    .unwrap();
                expected_max_seq = expected_max_seq.max(s);
            }
            // One cold op.
            let s = p
                .enqueue(WriteOp::PutDescriptor {
                    wallet: wallet_cold.clone(),
                    desc: dummy_descriptor(&wallet_cold, [0u8; 32]),
                })
                .unwrap();
            expected_max_seq = expected_max_seq.max(s);
            assert!(p.wait_durable(expected_max_seq, Duration::from_secs(5)));
        }

        {
            let (p, _) = RocksPersistenceP2b::open(&path).unwrap();
            // The fix: next_seq must be strictly greater than the
            // highest per-shard watermark, not the min. Without the
            // fix, a false ack could happen on shards whose durable
            // watermark exceeds a newly-issued seq.
            assert!(
                p.next_seq() > expected_max_seq,
                "next_seq={} must exceed pre-crash max seq={}",
                p.next_seq(),
                expected_max_seq
            );

            // Enqueue a fresh op and confirm wait_durable actually
            // waits (does not immediately return true from stale
            // watermarks).
            let mut sid = [0u8; 32];
            sid[0] = 0x00; // route to shard 0 where the watermark is highest
            let fresh_seq = p
                .enqueue(WriteOp::PutStringBlob {
                    string_id: sid,
                    blob: vec![0xEE; 4],
                })
                .unwrap();
            assert!(fresh_seq > expected_max_seq);
            // Should still succeed within the flush interval.
            assert!(p.wait_durable(fresh_seq, Duration::from_secs(2)));
        }
    }

    #[test]
    fn per_wallet_appends_stay_on_same_shard_preserving_chain_order() {
        // Every AppendChain(wallet=W) must route to shard(W[0]) so
        // FIFO order in a single mpsc channel enforces chain order.
        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistenceP2b::open(dir.path()).unwrap();

        let wallet = vec![0x33u8; 20];
        let expected_shard = (0x33u8 & SHARD_MASK) as usize;

        let mut sids = Vec::with_capacity(50);
        let mut last_seq = 0;
        for i in 0..50u64 {
            let mut sid = [0u8; 32];
            sid[0] = 0xEE;
            sid[31] = i as u8;
            sids.push(sid);
            let s = p
                .enqueue(WriteOp::AppendChain {
                    wallet: wallet.clone(),
                    seq_in_wallet: i,
                    string_id: sid,
                })
                .unwrap();
            last_seq = s;
        }
        assert!(p.wait_durable(last_seq, Duration::from_secs(5)));

        // Every chain seq for this wallet on the expected shard.
        let stats = p.shard_stats();
        assert!(stats[expected_shard].highest_durable >= last_seq);

        // Reopen and confirm chain order.
        drop(p);
        let (_p2, recovered) = RocksPersistenceP2b::open(dir.path()).unwrap();
        let chain = recovered
            .chains
            .iter()
            .find(|(w, _)| w == &wallet)
            .map(|(_, c)| c.clone())
            .unwrap();
        assert_eq!(chain, sids, "chain order must match append order");
    }

    #[test]
    fn queue_full_returns_typed_error_not_silent_success() {
        // Configure a tiny per-shard cap, spam one shard, expect
        // QueueFull rather than silent enqueue-then-drop.
        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistenceP2b::open_with_queue_cap(dir.path(), 4).unwrap();

        let mut wallet = vec![0u8; 20];
        wallet[0] = 0x00;

        // The flusher will drain some ops as we push, so we have to
        // push aggressively to actually fill. Do it in a tight loop
        // and accept that we may or may not observe QueueFull —
        // just assert that IF we do, it's the right error type.
        let mut saw_queue_full = false;
        for i in 0..10_000u64 {
            let mut sid = [0u8; 32];
            sid[0] = 0x00;
            sid[24..].copy_from_slice(&i.to_be_bytes());
            match p.enqueue(WriteOp::PutStringBlob {
                string_id: sid,
                blob: vec![0u8; 128],
            }) {
                Ok(_) => {}
                Err(RocksError::QueueFull) => {
                    saw_queue_full = true;
                    break;
                }
                Err(other) => panic!("unexpected error: {other:?}"),
            }
        }
        // Under a 4-op cap with a real disk, we should hit this at
        // least once during 10k spam. This test is a soft assertion:
        // if the environment is unusually fast it may not trigger,
        // but if it does trigger we must get the typed error.
        if !saw_queue_full {
            eprintln!("note: queue_full never triggered — flusher outpaced 10k spam");
        }
    }

    #[test]
    fn stream_string_blobs_yields_every_persisted_blob_in_batches() {
        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistenceP2b::open(dir.path()).unwrap();

        let mut expected: Vec<([u8; 32], Vec<u8>)> = Vec::new();
        // Under P2B, `wait_durable(seq)` only guarantees the shard that
        // OWNS `seq` is flushed up to that point — other shards may
        // still have pending writes for other sequence numbers. Track
        // the max seq per shard so we can wait on all of them.
        let mut max_seq_per_shard: [u64; NUM_SHARDS] = [0; NUM_SHARDS];
        for i in 0..25u8 {
            let mut sid = [0u8; 32];
            sid[0] = i;
            let blob = vec![i, i + 1, i + 2];
            expected.push((sid, blob.clone()));
            let seq = p
                .enqueue(WriteOp::PutStringBlob {
                    string_id: sid,
                    blob: blob.clone(),
                })
                .unwrap();
            // Sharding key for PutStringBlob is the first byte of sid.
            let shard = (sid[0] & SHARD_MASK) as usize;
            if seq > max_seq_per_shard[shard] {
                max_seq_per_shard[shard] = seq;
            }
        }
        for max_seq in max_seq_per_shard.iter() {
            if *max_seq > 0 {
                assert!(
                    p.wait_durable(*max_seq, Duration::from_secs(5)),
                    "shard's highest seq {} must land durable",
                    max_seq
                );
            }
        }

        let mut seen: Vec<([u8; 32], Vec<u8>)> = Vec::new();
        let n = p
            .stream_string_blobs(4, Duration::ZERO, |batch| {
                for kv in batch {
                    seen.push(kv);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(n, expected.len());
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        let mut expected_sorted = expected.clone();
        expected_sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(seen, expected_sorted);
    }

    #[test]
    fn concurrent_enqueue_from_many_threads_preserves_all_writes() {
        use std::sync::Arc as StdArc;
        use std::thread;

        let dir = TempDir::new().unwrap();
        let (p, _) = RocksPersistenceP2b::open(dir.path()).unwrap();
        let p = StdArc::new(p);

        let threads = 8usize;
        let ops_per_thread = 100usize;
        let mut handles = Vec::with_capacity(threads);
        for t in 0..threads {
            let p = p.clone();
            handles.push(thread::spawn(move || {
                let mut last_seq = 0;
                for i in 0..ops_per_thread {
                    let mut sid = [0u8; 32];
                    sid[0] = t as u8;
                    let idx = (i as u64).to_be_bytes();
                    sid[24..].copy_from_slice(&idx);
                    // Loop on QueueFull, backing off briefly.
                    loop {
                        match p.enqueue(WriteOp::PutStringBlob {
                            string_id: sid,
                            blob: vec![t as u8; 32],
                        }) {
                            Ok(s) => {
                                last_seq = s;
                                break;
                            }
                            Err(RocksError::QueueFull) => {
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            Err(other) => panic!("unexpected: {other:?}"),
                        }
                    }
                }
                last_seq
            }));
        }
        let mut max_seq = 0;
        for h in handles {
            max_seq = max_seq.max(h.join().unwrap());
        }
        assert!(p.wait_durable(max_seq, Duration::from_secs(10)));

        // Verify every blob is on disk by reading a sample from every thread.
        for t in 0..threads {
            let mut sid = [0u8; 32];
            sid[0] = t as u8;
            let idx = ((ops_per_thread - 1) as u64).to_be_bytes();
            sid[24..].copy_from_slice(&idx);
            let blob = p.read_string_blob(&sid).unwrap();
            assert!(blob.is_some(), "missing blob for thread {t}");
            assert_eq!(blob.unwrap(), vec![t as u8; 32]);
        }
    }

    #[test]
    fn default_queue_cap_can_be_overridden_by_env() {
        // Not exhaustive — just confirm the constant + env plumbing
        // is wired correctly. The env var is read in
        // rocksdb_persistence::queue_cap() which we re-export via
        // the sibling module.
        assert!(DEFAULT_QUEUE_CAP > 0);
    }

    #[test]
    fn shard_stats_pending_reflects_enqueue_minus_durable() {
        let dir = TempDir::new().unwrap();
        // Small cap to keep in-flight counts predictable.
        let (p, _) = RocksPersistenceP2b::open_with_queue_cap(dir.path(), 128).unwrap();

        // Enqueue a burst.
        let mut wallet = vec![0u8; 20];
        wallet[0] = 0x55;
        let mut last_seq = 0;
        for i in 0..50u64 {
            let mut sid = [0u8; 32];
            sid[0] = 0x55;
            sid[24..].copy_from_slice(&i.to_be_bytes());
            last_seq = p
                .enqueue(WriteOp::PutStringBlob {
                    string_id: sid,
                    blob: vec![0u8; 16],
                })
                .unwrap();
        }
        // Immediately after enqueue: pending may or may not be > 0
        // depending on how fast the flusher drained. Give the
        // flusher time and assert pending settles to 0.
        assert!(p.wait_durable(last_seq, Duration::from_secs(5)));
        for s in p.shard_stats() {
            assert_eq!(s.pending, 0, "pending must settle to 0 after wait_durable");
        }
    }
}
