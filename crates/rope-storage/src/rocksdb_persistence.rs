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
//! ## Schema
//!
//! | CF              | Key shape                                | Value shape                       |
//! |-----------------|------------------------------------------|-----------------------------------|
//! | `descriptors`   | `wallet_bytes`                           | `bincode(StoredLedgerDescriptor)` |
//! | `heads`         | `wallet_bytes`                           | `[u8; 32]` (head_string_id)       |
//! | `chain`         | `wallet_bytes \|\| u64-be seq_in_wallet` | `[u8; 32]` (string_id)            |
//! | `reverse`       | `[u8; 32]` (string_id)                   | `wallet_bytes`                    |
//! | `pieces`        | `[u8; 32]` (string_id)                   | `bincode(StoredPieceMap)`         |
//! | `strings`       | `[u8; 32]` (string_id)                   | `bincode(RopeString)` blob        |
//! | `tombstones`    | `[u8; 32]` (string_id)                   | `bincode(StoredTombstone)`        |
//! | default         | `b"durable_seq"`                         | `u64-le` (latest fsync'd seq)     |
//!
//! Quipu Canon v2.0 **Phase 1.6** adds the `strings` and `tombstones`
//! CFs so that the actual knot payloads (serialised `RopeString`s) and
//! the canon v1.1 §4.2 untie-tombstones survive a node restart. The
//! `strings` CF is disk-only — the read hot path stays in the
//! `StringLattice`; blobs are read back exactly once at open time to
//! rebuild the lattice. GDPR erasure deletes the blob from disk in the
//! same fsync'd batch that records the tombstone, so cryptographic
//! erasure remains true across restarts.
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
/// Phase 1.6 — serialised `RopeString` knot payloads, keyed by string_id.
pub const CF_STRINGS: &str = "strings";
/// Phase 1.6 — canon v1.1 §4.2 untie-tombstones, keyed by string_id.
pub const CF_TOMBSTONES: &str = "tombstones";

/// Default-CF key holding the latest fsync'd global sequence number.
const DURABLE_SEQ_KEY: &[u8] = b"durable_seq";

/// How long the background flusher waits for new ops before flushing
/// what it has. 10 ms matches the architecture spec §3.5 ack window.
const FLUSH_INTERVAL: Duration = Duration::from_millis(10);

/// Hard cap on ops drained per flush. Bounds the memory footprint of
/// a single `WriteBatch`; with the default 64 MB ROC `wb` and ~80 B
/// per chain entry this leaves plenty of headroom.
const MAX_BATCH_OPS: usize = 4096;

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
    /// Phase 1.6 — persist an opaque serialised `RopeString` blob so
    /// the knot payload survives restarts. The blob is produced by
    /// `bincode::serialize(&RopeString)` at the call site (rope-node);
    /// rope-storage treats it as opaque bytes to avoid a circular
    /// dependency on rope-core's concrete types.
    PutStringBlob {
        string_id: [u8; 32],
        blob: Vec<u8>,
    },
    /// Phase 1.6 — cryptographic erasure on disk. Deletes the string
    /// blob (whole-string erase pathway AND the payload-destruction
    /// half of a per-knot untie). GDPR Art. 17 correctness across
    /// restarts depends on this landing in the same fsync'd batch
    /// wave as the tombstone that records the erasure.
    DeleteStringBlob { string_id: [u8; 32] },
    /// Phase 1.6 — persist a canon v1.1 §4.2 untie-tombstone.
    PutTombstone {
        string_id: [u8; 32],
        tombstone: StoredTombstone,
    },
}

/// Phase 1.6 — on-disk shape of a canon v1.1 §4.2 knot tombstone.
/// Mirrors `rope_core::lattice::KnotTombstone` field-for-field, plus
/// the untied knot's parent edges (`parents`) which the lattice needs
/// to hop past the tombstone when walking a string after a restart
/// (the live `RopeString` — and with it the in-payload parentage — is
/// destroyed at untie time, so the edge must be carried here). Kept
/// as an independent type so rope-storage does not depend on the
/// lattice module's concrete types.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StoredTombstone {
    pub untied_at: i64,
    pub audit_hash: [u8; 32],
    pub reason: String,
    /// Parent string-ids of the untied knot, genesis-sentinel included
    /// as all-zero when the knot was a genesis (never true in practice
    /// — genesis knots cannot be untied).
    pub parents: Vec<[u8; 32]>,
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
    /// Phase 1.6 — serialised `RopeString` blobs (opaque to rope-storage;
    /// rope-node deserialises them back into the lattice at boot).
    pub string_blobs: Vec<([u8; 32], Vec<u8>)>,
    /// Phase 1.6 — untie-tombstones to replay into the lattice at boot.
    pub tombstones: Vec<([u8; 32], StoredTombstone)>,
    pub durable_seq: u64,
}

/// Disk-backed persistence engine for [`crate::ledger_db::LedgerStore`].
///
/// One handle per RocksDB instance. Cloning is via `Arc`; the engine is
/// `Send + Sync`. Drop signals the background flusher to drain the
/// channel and exit cleanly.
pub struct RocksPersistence {
    db: Arc<DB>,
    write_tx: Mutex<Option<Sender<PendingWrite>>>,
    next_seq: AtomicU64,
    durable_seq: Arc<AtomicU64>,
    /// Set true on Drop; flusher checks it as a belt-and-braces exit.
    shutdown: Arc<AtomicBool>,
    /// Joined in Drop. Outer `Mutex<Option<…>>` so Drop can `take()`.
    flusher: Mutex<Option<JoinHandle<()>>>,
    /// Notified after each successful flush — wakes [`wait_durable`].
    notify_pair: Arc<(Mutex<()>, Condvar)>,
}

/// Stats snapshot for metrics endpoints.
#[derive(Clone, Debug, Default)]
pub struct PersistenceStats {
    pub next_seq: u64,
    pub durable_seq: u64,
    pub pending: u64,
    pub shutdown: bool,
}

impl RocksPersistence {
    /// Open or create a RocksDB instance at `path` and start the
    /// background flusher. Returns the handle plus a [`RecoveredState`]
    /// snapshot that the caller (typically `LedgerStore::open`) uses to
    /// rebuild its in-memory mirror.
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
        // Background workers: keep small; the bulk of throughput comes
        // from our own batched flush, not from internal compactions.
        db_opts.increase_parallelism(2);

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

        // ---- Recovery: scan every CF and read back the watermark ----
        let recovered = recover_from_db(&db)?;

        // ---- Set up background flusher ----
        let (write_tx, write_rx) = mpsc::channel::<PendingWrite>();
        let durable_seq = Arc::new(AtomicU64::new(recovered.durable_seq));
        let shutdown = Arc::new(AtomicBool::new(false));
        let notify_pair = Arc::new((Mutex::new(()), Condvar::new()));

        let flusher = {
            let db = db.clone();
            let durable_seq = durable_seq.clone();
            let shutdown = shutdown.clone();
            let notify_pair = notify_pair.clone();
            thread::Builder::new()
                .name("rope-storage-flusher".to_string())
                .spawn(move || flusher_loop(db, write_rx, durable_seq, shutdown, notify_pair))
                .expect("spawn flusher thread")
        };

        // `next_seq` resumes from the durable watermark. Pre-Drop
        // crashes may have lost in-flight ops above the watermark; that
        // is the documented best-effort behaviour for ops whose caller
        // never invoked `wait_durable`.
        let next_seq = AtomicU64::new(recovered.durable_seq + 1);

        Ok((
            Arc::new(Self {
                db,
                write_tx: Mutex::new(Some(write_tx)),
                next_seq,
                durable_seq,
                shutdown,
                flusher: Mutex::new(Some(flusher)),
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
    /// has been dropped or the flusher panicked. In normal operation
    /// this never errors.
    pub fn enqueue(&self, op: WriteOp) -> Result<u64, RocksError> {
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel);
        let guard = self.write_tx.lock();
        let tx = guard.as_ref().ok_or(RocksError::FlusherStopped)?;
        tx.send(PendingWrite { seq, op })
            .map_err(|_| RocksError::FlusherStopped)?;
        Ok(seq)
    }

    /// Block until the given sequence number is on durable disk, or
    /// `timeout` expires. Returns `true` iff the watermark reached
    /// `seq` before the deadline.
    pub fn wait_durable(&self, seq: u64, timeout: Duration) -> bool {
        let start = Instant::now();
        let (lock, cvar) = &*self.notify_pair;
        let mut guard = lock.lock();
        loop {
            if self.durable_seq.load(Ordering::Acquire) >= seq {
                return true;
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return false;
            }
            let remaining = timeout - elapsed;
            // `wait_for` returns a `WaitTimeoutResult` indicating
            // whether the timeout elapsed. Either way we re-check the
            // watermark at the top of the loop.
            let _ = cvar.wait_for(&mut guard, remaining);
        }
    }

    /// Latest durable (fsync'd) sequence number.
    pub fn durable_seq(&self) -> u64 {
        self.durable_seq.load(Ordering::Acquire)
    }

    /// Highest sequence number assigned so far. The gap
    /// `next_seq() - durable_seq() - 1` is the in-flight queue depth.
    pub fn next_seq(&self) -> u64 {
        self.next_seq.load(Ordering::Acquire)
    }

    /// Snapshot stats for metrics endpoints.
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
        // 1. Flag shutdown so the flusher exits its idle loop quickly.
        self.shutdown.store(true, Ordering::Release);
        // 2. Drop the sender → flusher's `recv()` returns Disconnected
        //    on its next iteration, and it exits after one final
        //    drain+flush.
        {
            let mut guard = self.write_tx.lock();
            *guard = None;
        }
        // 3. Wait for the flusher to finish its final batch and exit.
        if let Some(handle) = self.flusher.lock().take() {
            let _ = handle.join();
        }
        // 4. Best-effort one more sync just in case the flusher
        //    panicked mid-flight.
        let _ = self.db.flush();
    }
}

// ============================================================================
// Background flusher
// ============================================================================

fn flusher_loop(
    db: Arc<DB>,
    rx: mpsc::Receiver<PendingWrite>,
    durable_seq: Arc<AtomicU64>,
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

        if let Err(e) = flush_batch(&db, &buf, &durable_seq) {
            // RocksDB write failures are fatal for durability — log and
            // bail out of the loop so the next enqueue sees
            // `FlusherStopped`. The OS will surface the underlying
            // disk-full / EROFS / etc.
            tracing::error!(
                "rope-storage flusher write failed: {e:?} (op_count={}); flusher exiting",
                buf.len()
            );
            return;
        }

        // Wake everyone parked in `wait_durable`.
        notify_pair.1.notify_all();

        buf.clear();
    }

    // Final drain + flush on shutdown — pull any remaining ops from
    // the channel, write them, advance the watermark, and notify.
    while let Ok(op) = rx.try_recv() {
        buf.push(op);
        if buf.len() >= MAX_BATCH_OPS {
            if let Err(e) = flush_batch(&db, &buf, &durable_seq) {
                tracing::error!("rope-storage flusher final-drain write failed: {e:?}");
                return;
            }
            buf.clear();
        }
    }
    if !buf.is_empty() {
        if let Err(e) = flush_batch(&db, &buf, &durable_seq) {
            tracing::error!("rope-storage flusher final-drain write failed: {e:?}");
            return;
        }
    }
    // Final notify so any waiter for the highest seq can see it.
    notify_pair.1.notify_all();
}

fn flush_batch(db: &DB, ops: &[PendingWrite], durable_seq: &AtomicU64) -> Result<(), RocksError> {
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

    // Stamp the watermark in the same WriteBatch so durability of the
    // op set and durability of the watermark advance are atomic — a
    // crash mid-write will roll back BOTH together.
    batch.put(DURABLE_SEQ_KEY, highest_seq.to_le_bytes());

    let mut wo = WriteOptions::default();
    wo.set_sync(true); // fsync after WAL write — the durability ack
    db.write_opt(batch, &wo)?;

    durable_seq.store(highest_seq, Ordering::Release);
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

    state.durable_seq = match db.get(DURABLE_SEQ_KEY)? {
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
            // Implicit drop here: the flusher final-drains, then the
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
        // The Drop impl must drain the channel and final-flush.
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
}
