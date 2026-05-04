//! Worker pool + LedgerStore lifecycle helpers.
//!
//! The hot path is intentionally allocation-free per op: each thread
//! pre-allocates its latency buffer, generates its sid via a per-thread
//! ChaCha8 PRNG (no syscalls), and indexes into a shared wallet pool
//! by integer.

use crate::cli::{CommonWorkloadArgs, Mode, Scenario};
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rope_storage::ledger_db::{LedgerStore, StoredLedgerDescriptor};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

/// A pool of synthetic 20-byte wallet addresses.
///
/// Wallet `i` has the seed-derived deterministic bytes, so every run
/// with the same seed produces the same wallets — useful for
/// `store-recover` tests against a previously-generated database.
#[derive(Clone)]
pub struct WalletPool {
    wallets: Arc<Vec<Vec<u8>>>,
}

impl WalletPool {
    pub fn new(count: usize, seed: u64) -> Self {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut wallets = Vec::with_capacity(count);
        for _ in 0..count {
            let mut bytes = [0u8; 20];
            rng.fill_bytes(&mut bytes);
            wallets.push(bytes.to_vec());
        }
        Self {
            wallets: Arc::new(wallets),
        }
    }

    pub fn len(&self) -> usize {
        self.wallets.len()
    }

    pub fn get(&self, idx: usize) -> &[u8] {
        &self.wallets[idx]
    }

    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        self.wallets.iter().map(|v| v.as_slice())
    }
}

/// Owns the underlying `LedgerStore` plus the `TempDir` (if any) so
/// the on-disk DB lives exactly as long as the harness.
///
/// `mode` and `db_path` are kept on the handle so structured logs
/// emitted by the runner can attribute their telemetry to the right
/// configuration. They are intentionally `pub` so a downstream caller
/// (e.g. a CI script that hooks the runner into a larger harness)
/// can introspect them, even though the current scenarios don't read
/// them after construction.
pub struct StoreHandle {
    pub store: Arc<LedgerStore>,
    // `mode` and `db_path` are exposed for callers that want to attribute
    // their own telemetry to the store configuration; the harness itself
    // does not read them after construction. Allow dead_code is the
    // canonical way to express "intentionally part of the public surface
    // even though no internal call site exercises them".
    #[allow(dead_code)]
    pub mode: Mode,
    #[allow(dead_code)]
    pub db_path: Option<PathBuf>,
    /// Owned tempdir — `Some` iff `--db-path` was not specified for
    /// rocksdb mode. Dropped on `StoreHandle::drop`, which removes the
    /// directory.
    _tempdir: Option<TempDir>,
}

impl StoreHandle {
    pub fn create(args: &CommonWorkloadArgs) -> Result<Self, String> {
        match args.mode {
            Mode::Memory => {
                tracing::debug!(target: "rope_loadgen::runner", "creating in-memory LedgerStore");
                Ok(Self {
                    store: Arc::new(LedgerStore::new()),
                    mode: Mode::Memory,
                    db_path: None,
                    _tempdir: None,
                })
            }
            Mode::Rocksdb => {
                let (path, tempdir) = match &args.db_path {
                    Some(p) => (p.clone(), None),
                    None => {
                        let td = TempDir::new()
                            .map_err(|e| format!("creating tempdir for rocksdb: {e}"))?;
                        let p = td.path().to_path_buf();
                        (p, Some(td))
                    }
                };
                tracing::debug!(target: "rope_loadgen::runner", path = %path.display(), "opening rocksdb-backed LedgerStore");
                let store = LedgerStore::open(&path)
                    .map_err(|e| format!("opening rocksdb at {}: {e}", path.display()))?;
                Ok(Self {
                    store: Arc::new(store),
                    mode: Mode::Rocksdb,
                    db_path: Some(path),
                    _tempdir: tempdir,
                })
            }
        }
    }

    /// Returns `true` if every write enqueued so far has hit disk
    /// (vacuously true in memory mode). Always returns immediately
    /// in memory mode.
    pub fn await_durable(&self, timeout: Duration) -> (bool, Duration) {
        let start = Instant::now();
        let ok = self.store.await_all_durable(timeout);
        (ok, start.elapsed())
    }
}

/// Wallet-index assignment for a given thread + op index, per
/// `Scenario`. Returned indices are always in range `[0, pool.len())`.
#[inline]
pub fn pick_wallet(
    scenario: Scenario,
    pool: &WalletPool,
    thread_id: usize,
    threads: usize,
    op_idx: usize,
    rng: &mut ChaCha8Rng,
) -> usize {
    let n = pool.len();
    debug_assert!(n > 0);
    match scenario {
        Scenario::Same => 0,
        Scenario::Partitioned => {
            // Each thread owns a contiguous slice of [thread_id*span, (thread_id+1)*span).
            // Span ≥ 1 because we cap below.
            let span = (n / threads).max(1);
            let start = (thread_id * span) % n;
            let within = op_idx % span;
            (start + within) % n
        }
        Scenario::Random => (rng.next_u64() as usize) % n,
    }
}

/// Pre-create one descriptor per wallet so subsequent `append_to_chain`
/// calls have a chain to grow. Untimed.
pub fn populate_descriptors(store: &LedgerStore, pool: &WalletPool) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(1_700_000_000);
    for wallet in pool.iter() {
        let mut head = [0u8; 32];
        head[..wallet.len().min(32)].copy_from_slice(&wallet[..wallet.len().min(32)]);
        store.put_descriptor(
            wallet,
            StoredLedgerDescriptor {
                wallet_address: wallet.to_vec(),
                genesis_string_id: head,
                head_string_id: head,
                entry_count: 0,
                total_size_bytes: 0,
                oes_generation_at_creation: 0,
                current_oes_generation: 0,
                created_at: now,
                last_appended_at: now,
                is_deleted: false,
                deleted_at: None,
                replication_factor: 5,
            },
        );
    }
}

/// Spawn `n` worker threads, each running `body(thread_id, ops_for_this_thread)`,
/// collect their per-thread latency buffers (in ns), and return the
/// total wall-clock duration of the timed phase plus the merged samples.
///
/// `body` MUST push exactly `ops_for_this_thread` samples into the
/// `Vec` it receives. The runner does no per-op work itself — that's
/// the caller's job — which keeps measurement overhead off the hot
/// path.
pub fn run_threads<F>(
    threads: usize,
    ops_total: usize,
    seed_base: u64,
    body: F,
) -> (Duration, Vec<u64>)
where
    F: Fn(usize, usize, &mut ChaCha8Rng, &mut Vec<u64>) + Send + Sync + 'static,
{
    let body = Arc::new(body);
    let per_thread = ops_total / threads.max(1);
    let remainder = ops_total - per_thread * threads;

    // Start gun: a parking_lot::Barrier would be ideal but std::sync's
    // Barrier is in std and works the same way for our 1-shot use.
    let barrier = Arc::new(std::sync::Barrier::new(threads + 1));

    let mut handles = Vec::with_capacity(threads);
    for tid in 0..threads {
        // Distribute the remainder across the first `remainder` threads,
        // so total ops always exactly equals `ops_total`.
        let n_for_this = per_thread + if tid < remainder { 1 } else { 0 };
        let body = body.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            let mut rng = ChaCha8Rng::seed_from_u64(seed_base.wrapping_add(tid as u64));
            let mut samples = Vec::with_capacity(n_for_this);
            // Wait for the parent to release us simultaneously.
            barrier.wait();
            body(tid, n_for_this, &mut rng, &mut samples);
            samples
        }));
    }

    // Release all workers at once and start the wall clock.
    barrier.wait();
    let start = Instant::now();
    let mut all_samples = Vec::with_capacity(ops_total);
    for h in handles {
        let mut s = h.join().expect("worker thread panicked");
        all_samples.append(&mut s);
    }
    let elapsed = start.elapsed();
    (elapsed, all_samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wallet_pool_is_deterministic() {
        let a = WalletPool::new(64, 42);
        let b = WalletPool::new(64, 42);
        let c = WalletPool::new(64, 43);
        assert_eq!(a.len(), 64);
        assert_eq!(a.get(7), b.get(7));
        assert_ne!(
            a.get(7),
            c.get(7),
            "different seeds must produce different wallets"
        );
    }

    #[test]
    fn pick_wallet_same_always_returns_zero() {
        let pool = WalletPool::new(100, 1);
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        for tid in 0..16 {
            for op in 0..100 {
                assert_eq!(pick_wallet(Scenario::Same, &pool, tid, 16, op, &mut rng), 0);
            }
        }
    }

    #[test]
    fn pick_wallet_partitioned_no_overlap() {
        // 4 threads × 100 wallets ⇒ 25-wallet slice per thread, no overlap.
        let pool = WalletPool::new(100, 1);
        let mut sets = vec![std::collections::HashSet::new(); 4];
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        for (tid, set) in sets.iter_mut().enumerate() {
            for op in 0..50 {
                let idx = pick_wallet(Scenario::Partitioned, &pool, tid, 4, op, &mut rng);
                set.insert(idx);
            }
        }
        // Sets must be pairwise disjoint.
        for i in 0..4 {
            for j in (i + 1)..4 {
                let intersect: Vec<_> = sets[i].intersection(&sets[j]).collect();
                assert!(
                    intersect.is_empty(),
                    "threads {i} and {j} overlap on wallets {intersect:?}"
                );
            }
        }
    }

    #[test]
    fn pick_wallet_random_stays_in_range() {
        let pool = WalletPool::new(50, 1);
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        for op in 0..10_000 {
            let idx = pick_wallet(Scenario::Random, &pool, 0, 1, op, &mut rng);
            assert!(idx < 50);
        }
    }

    #[test]
    fn run_threads_collects_exact_op_count() {
        let (_dur, samples) = run_threads(4, 100, 42, |_tid, n, _rng, samples| {
            for i in 0..n {
                samples.push(i as u64);
            }
        });
        assert_eq!(samples.len(), 100);
    }

    #[test]
    fn run_threads_distributes_remainder() {
        // 7 ops / 4 threads = 1 each + 3 remainder = (2,2,2,1) total 7.
        let (_dur, samples) = run_threads(4, 7, 0, |_tid, n, _rng, samples| {
            for i in 0..n {
                samples.push(i as u64);
            }
        });
        assert_eq!(samples.len(), 7);
    }
}
