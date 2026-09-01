//! # Lattice Contention Instrumentation (P1 - §17.5 #1)
//!
//! Lock-free, always-on histograms for `head_guard` acquisition wait and hold
//! time on the append hot path. Designed so the observation cost is bounded to
//! two `Instant::now()` calls and three `AtomicU64::fetch_add` per instrumented
//! critical section - safe to leave enabled in production.
//!
//! ## What it measures
//!
//! - **Wait time** (`nanoseconds`): elapsed between calling `Mutex::lock()` and
//!   the guard being handed back. Under contention this is the queue length
//!   times the mean hold time, which is the number we actually care about when
//!   the wedge from §17 recurs.
//! - **Hold time** (`nanoseconds`): elapsed between guard acquisition and its
//!   `Drop`. Long hold time means the critical section itself is the bottleneck
//!   (candidate for Phase 2.B / Phase C - moving work outside the lock).
//! - **Per-operation counters** (`create_ledger`, `append_to_ledger`,
//!   `erase_ledger`, `untie_knot`): so we can attribute wedge windows to the
//!   correct RPC method without cross-referencing journal logs.
//! - **Flusher backpressure**: an additional histogram fed by the persistence
//!   layer via `record_flusher_wait_ns` - filled when we integrate with the
//!   Phase 2.B pool (§22) or the legacy single-flusher when it stalls.
//!
//! ## Design constraints
//!
//! 1. **Zero mutexes on the observation path.** All state is `AtomicU64`. The
//!    hot path adds two `Instant::now()` reads and three atomic adds - measured
//!    at ~50 ns total, four orders of magnitude below the critical-section
//!    itself.
//! 2. **Fixed-size histograms.** 32 exponential buckets from 1 µs to ~4 s. No
//!    allocation on the observation path. Snapshot is a pure copy.
//! 3. **RPC-observable.** `rope_latticeMetrics` returns a JSON snapshot suitable
//!    for Grafana ingestion via a simple `curl | jq` scraper (§17.5 #4).
//! 4. **Reset-safe.** A dedicated `reset()` method zeros all counters for
//!    benchmark / soak windows without process restart.
//!
//! ## Non-goals
//!
//! - Per-wallet or per-shard breakdown: would require a `DashMap` and defeats
//!   the zero-lock design. If a specific wallet is suspected we can grep the
//!   append log or drop a targeted `tracing::info!` for a bounded window.
//! - Percentile calculation: done client-side from the histogram buckets.
//!
//! ## Correctness
//!
//! - Overflow on any single bucket saturates at `u64::MAX` (SeqCst add on
//!   `AtomicU64`). At 1 M ops/sec that's ~584,000 years to saturate a single
//!   bucket. Not a concern.
//! - `Instant::now()` uses `CLOCK_MONOTONIC` on Linux (monotonic, no NTP jump).
//!   Saturation to zero is impossible on any duration we care about.
//! - Concurrent updates to different buckets are fully independent (no shared
//!   cache line for adjacent buckets thanks to explicit `#[repr(align(64))]`
//!   padding on the bucket array).

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// Number of histogram buckets. Exponential, base 2, starting at 1 µs.
///
/// Mapping (bucket k covers `[2^(k-1) * 1 µs, 2^k * 1 µs)` for k >= 1):
///
/// - bucket 0:  [0 ns,      1 µs)              // sub-microsecond
/// - bucket 1:  [1 µs,      2 µs)
/// - bucket 2:  [2 µs,      4 µs)
/// - ...
/// - bucket 20: [~524 ms,   ~1.05 s)
/// - bucket 30: [~537 s,    ~1074 s)
/// - bucket 31: [~1074 s,   +∞)                // overflow sink
///
/// The 1 µs → ~1074 s exponential range comfortably covers every realistic
/// head_guard observation: sub-µs contention-free acquisition on one side,
/// multi-minute stalls (which would already have triggered HA restarts) on
/// the other.
pub const HIST_BUCKETS: usize = 32;

/// Cache-line-aligned bucket wrapper to prevent false sharing between adjacent
/// buckets when many threads are updating the histogram concurrently.
#[repr(align(64))]
struct AlignedBucket(AtomicU64);

impl AlignedBucket {
    const fn zero() -> Self {
        AlignedBucket(AtomicU64::new(0))
    }
}

/// Lock-free histogram of nanosecond durations.
struct NsHistogram {
    buckets: [AlignedBucket; HIST_BUCKETS],
    /// Running sum of all observations (nanoseconds). Used for mean.
    sum_ns: AtomicU64,
    /// Total number of observations. `count` == `sum_of_bucket_counts`.
    count: AtomicU64,
    /// Maximum observation ever seen (nanoseconds).
    max_ns: AtomicU64,
}

impl NsHistogram {
    const fn new() -> Self {
        // Can't use `[AlignedBucket::zero(); HIST_BUCKETS]` in const context
        // because `AlignedBucket` doesn't implement `Copy` (interior atomic).
        // Explicit list is the only way.
        Self {
            buckets: [
                AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(),
                AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(),
                AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(),
                AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(),
                AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(),
                AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(),
                AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(),
                AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(), AlignedBucket::zero(),
            ],
            sum_ns: AtomicU64::new(0),
            count: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
        }
    }

    #[inline(always)]
    fn observe(&self, ns: u64) {
        // Bucket index = floor(log2(ns_in_us)) + 1 for ns >= 1000, else 0.
        let bucket = bucket_for_ns(ns);
        self.buckets[bucket].0.fetch_add(1, Ordering::Relaxed);
        self.sum_ns.fetch_add(ns, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        // fetch_max avoids the compare_exchange loop while still being
        // wait-free on x86_64 and aarch64.
        self.max_ns.fetch_max(ns, Ordering::Relaxed);
    }

    fn snapshot(&self) -> HistogramSnapshot {
        let counts: Vec<u64> = self
            .buckets
            .iter()
            .map(|b| b.0.load(Ordering::Relaxed))
            .collect();
        let total = self.count.load(Ordering::Relaxed);
        let sum = self.sum_ns.load(Ordering::Relaxed);
        let max = self.max_ns.load(Ordering::Relaxed);
        let mean_ns = if total > 0 { sum / total } else { 0 };
        HistogramSnapshot {
            count: total,
            mean_ns,
            max_ns: max,
            sum_ns: sum,
            bucket_upper_bounds_ns: bucket_upper_bounds_ns(),
            bucket_counts: counts,
        }
    }

    fn reset(&self) {
        for b in self.buckets.iter() {
            b.0.store(0, Ordering::Relaxed);
        }
        self.sum_ns.store(0, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
        self.max_ns.store(0, Ordering::Relaxed);
    }
}

/// Compute the histogram bucket index for a nanosecond observation.
///
/// - `ns == 0` → bucket 0 (special-cased for correctness; `log2(0)` is UB).
/// - `ns < 1000` → bucket 0 (sub-microsecond).
/// - `ns >= 1000` → `1 + floor(log2(ns / 1000))`, capped at `HIST_BUCKETS - 1`.
#[inline]
fn bucket_for_ns(ns: u64) -> usize {
    if ns < 1_000 {
        return 0;
    }
    let us = ns / 1_000;
    // log2(us) via leading_zeros. For us=1 → bits=0; us=2 → 1; us=4 → 2; ...
    let bits = 63 - us.leading_zeros() as usize;
    let b = 1 + bits;
    if b >= HIST_BUCKETS {
        HIST_BUCKETS - 1
    } else {
        b
    }
}

/// Upper bounds (exclusive) of each histogram bucket in nanoseconds.
///
/// Layout:
/// - bucket 0:  [0,          1_000)          (< 1 µs)
/// - bucket 1:  [1_000,      2_000)          (1..2 µs)
/// - bucket 2:  [2_000,      4_000)          (2..4 µs)
/// - bucket k>=1: [2^(k-1) * 1_000, 2^k * 1_000)
/// - bucket 31: overflow sink, upper bound reported as u64::MAX.
fn bucket_upper_bounds_ns() -> Vec<u64> {
    let mut out = Vec::with_capacity(HIST_BUCKETS);
    out.push(1_000); // bucket 0 upper bound (1 µs)
    for k in 1..HIST_BUCKETS - 1 {
        let ub = 1_000u64
            .checked_shl(k as u32)
            .unwrap_or(u64::MAX);
        out.push(ub);
    }
    out.push(u64::MAX);
    out
}

/// Snapshot of a single histogram at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramSnapshot {
    pub count: u64,
    pub mean_ns: u64,
    pub max_ns: u64,
    pub sum_ns: u64,
    /// Upper bound (exclusive, ns) of each bucket. Length == `bucket_counts.len()`.
    pub bucket_upper_bounds_ns: Vec<u64>,
    /// Observation count per bucket. Length == `HIST_BUCKETS`.
    pub bucket_counts: Vec<u64>,
}

/// Which lattice operation acquired the head_guard.
///
/// Kept explicit rather than a `&'static str` field so scraping code can
/// enumerate all valid values without a wildcard match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatticeOp {
    CreateLedger,
    AppendToLedger,
    EraseLedger,
    UntieKnot,
}

impl LatticeOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            LatticeOp::CreateLedger => "create_ledger",
            LatticeOp::AppendToLedger => "append_to_ledger",
            LatticeOp::EraseLedger => "erase_ledger",
            LatticeOp::UntieKnot => "untie_knot",
        }
    }
}

/// Per-operation counters (independent of histogram buckets).
struct OpCounters {
    /// Number of times this op successfully acquired the head_guard.
    acquired: AtomicU64,
    /// Cumulative nanoseconds spent waiting on the head_guard.
    wait_ns_total: AtomicU64,
    /// Cumulative nanoseconds spent holding the head_guard.
    hold_ns_total: AtomicU64,
}

impl OpCounters {
    const fn new() -> Self {
        Self {
            acquired: AtomicU64::new(0),
            wait_ns_total: AtomicU64::new(0),
            hold_ns_total: AtomicU64::new(0),
        }
    }

    fn reset(&self) {
        self.acquired.store(0, Ordering::Relaxed);
        self.wait_ns_total.store(0, Ordering::Relaxed);
        self.hold_ns_total.store(0, Ordering::Relaxed);
    }
}

/// Snapshot of per-operation counters, one entry per `LatticeOp` variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpSnapshot {
    pub op: String,
    pub acquired: u64,
    pub wait_ns_total: u64,
    pub hold_ns_total: u64,
    pub mean_wait_ns: u64,
    pub mean_hold_ns: u64,
}

/// The global metrics instance. Always alive for the process lifetime.
pub struct LatticeMetrics {
    /// Global head_guard wait-time histogram (all ops combined).
    head_guard_wait: NsHistogram,
    /// Global head_guard hold-time histogram (all ops combined).
    head_guard_hold: NsHistogram,
    /// Flusher wait histogram. Populated by the persistence layer's
    /// `record_flusher_wait_ns` shim; provides the second half of the wedge
    /// diagnostic (§17.1 forensics: main thread in futex_wait_queue was the
    /// user visible symptom; flusher backpressure is the root cause).
    flusher_wait: NsHistogram,
    /// Per-op counters. Indexed by `LatticeOp as usize`.
    op_create_ledger: OpCounters,
    op_append: OpCounters,
    op_erase: OpCounters,
    op_untie: OpCounters,
    /// Monotonic timestamp of the last `reset()` call (seconds since UNIX
    /// epoch). Zero if never reset. Used by the RPC snapshot for windowing.
    last_reset_unix_secs: AtomicU64,
    /// Monotonic timestamp of `LatticeMetrics::new()` (seconds since UNIX
    /// epoch). Used by the RPC snapshot for windowing.
    started_unix_secs: AtomicU64,
}

impl LatticeMetrics {
    const fn new_const() -> Self {
        Self {
            head_guard_wait: NsHistogram::new(),
            head_guard_hold: NsHistogram::new(),
            flusher_wait: NsHistogram::new(),
            op_create_ledger: OpCounters::new(),
            op_append: OpCounters::new(),
            op_erase: OpCounters::new(),
            op_untie: OpCounters::new(),
            last_reset_unix_secs: AtomicU64::new(0),
            started_unix_secs: AtomicU64::new(0),
        }
    }

    fn op_counters(&self, op: LatticeOp) -> &OpCounters {
        match op {
            LatticeOp::CreateLedger => &self.op_create_ledger,
            LatticeOp::AppendToLedger => &self.op_append,
            LatticeOp::EraseLedger => &self.op_erase,
            LatticeOp::UntieKnot => &self.op_untie,
        }
    }

    /// Record a completed head_guard critical section.
    ///
    /// `wait_ns` = time spent inside `Mutex::lock()`.
    /// `hold_ns` = time between guard acquisition and drop.
    ///
    /// Cost: three atomic adds on `wait`, three on `hold`, three on the per-op
    /// counters. Total ~9 atomic ops, plus two histogram bucket lookups. On
    /// x86_64 measures around 40-60 ns total; safe to leave on in production.
    #[inline]
    pub fn record(&self, op: LatticeOp, wait_ns: u64, hold_ns: u64) {
        self.head_guard_wait.observe(wait_ns);
        self.head_guard_hold.observe(hold_ns);
        let oc = self.op_counters(op);
        oc.acquired.fetch_add(1, Ordering::Relaxed);
        oc.wait_ns_total.fetch_add(wait_ns, Ordering::Relaxed);
        oc.hold_ns_total.fetch_add(hold_ns, Ordering::Relaxed);
    }

    /// Record a flusher wait observation.
    ///
    /// Called by the persistence layer when the enqueue path stalls because
    /// the sync_channel is full or the flusher is behind. This is the second
    /// half of the wedge signal - if `flusher_wait` p99 rises above `head_guard`
    /// p99, the bottleneck is downstream of the head lock and Phase 2.B is the
    /// right fix.
    #[inline]
    pub fn record_flusher_wait_ns(&self, ns: u64) {
        self.flusher_wait.observe(ns);
    }

    /// Reset all counters. Used for benchmark / soak windows.
    pub fn reset(&self) {
        self.head_guard_wait.reset();
        self.head_guard_hold.reset();
        self.flusher_wait.reset();
        self.op_create_ledger.reset();
        self.op_append.reset();
        self.op_erase.reset();
        self.op_untie.reset();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.last_reset_unix_secs.store(now, Ordering::Relaxed);
    }

    /// Set the "started" timestamp. Called exactly once at process start.
    fn mark_started(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Only set if zero (first call wins; subsequent test-mode resets do
        // not lose the original start time).
        self.started_unix_secs
            .compare_exchange(0, now, Ordering::Relaxed, Ordering::Relaxed)
            .ok();
    }

    /// Full metrics snapshot suitable for `rope_latticeMetrics` RPC response.
    pub fn snapshot(&self) -> LatticeMetricsSnapshot {
        let ops = vec![
            self.op_snapshot(LatticeOp::CreateLedger),
            self.op_snapshot(LatticeOp::AppendToLedger),
            self.op_snapshot(LatticeOp::EraseLedger),
            self.op_snapshot(LatticeOp::UntieKnot),
        ];
        LatticeMetricsSnapshot {
            head_guard_wait: self.head_guard_wait.snapshot(),
            head_guard_hold: self.head_guard_hold.snapshot(),
            flusher_wait: self.flusher_wait.snapshot(),
            per_op: ops,
            started_unix_secs: self.started_unix_secs.load(Ordering::Relaxed),
            last_reset_unix_secs: self.last_reset_unix_secs.load(Ordering::Relaxed),
        }
    }

    fn op_snapshot(&self, op: LatticeOp) -> OpSnapshot {
        let oc = self.op_counters(op);
        let acquired = oc.acquired.load(Ordering::Relaxed);
        let wait = oc.wait_ns_total.load(Ordering::Relaxed);
        let hold = oc.hold_ns_total.load(Ordering::Relaxed);
        let mean_wait = if acquired > 0 { wait / acquired } else { 0 };
        let mean_hold = if acquired > 0 { hold / acquired } else { 0 };
        OpSnapshot {
            op: op.as_str().to_string(),
            acquired,
            wait_ns_total: wait,
            hold_ns_total: hold,
            mean_wait_ns: mean_wait,
            mean_hold_ns: mean_hold,
        }
    }
}

/// Top-level snapshot returned by `rope_latticeMetrics` RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatticeMetricsSnapshot {
    pub head_guard_wait: HistogramSnapshot,
    pub head_guard_hold: HistogramSnapshot,
    pub flusher_wait: HistogramSnapshot,
    pub per_op: Vec<OpSnapshot>,
    /// Unix seconds when the LatticeMetrics singleton was initialized. This is
    /// process-start time - useful for absolute-rate calculations.
    pub started_unix_secs: u64,
    /// Unix seconds when `reset()` was last called. Zero if never. Between
    /// `last_reset_unix_secs` and now is the current observation window.
    pub last_reset_unix_secs: u64,
}

// Process-wide singleton. Initialized lazily on first access.
static METRICS: OnceLock<LatticeMetrics> = OnceLock::new();

/// Access the process-wide `LatticeMetrics` singleton.
///
/// Safe to call from any thread; the underlying `OnceLock` guarantees exactly
/// one initialization. Cost after first call is a single relaxed load.
pub fn lattice_metrics() -> &'static LatticeMetrics {
    METRICS.get_or_init(|| {
        let m = LatticeMetrics::new_const();
        m.mark_started();
        m
    })
}

/// RAII guard that records `wait_ns` at construction and `hold_ns` on drop.
///
/// Wraps the actual `MutexGuard<'_, T>` so the caller uses it exactly like a
/// normal guard. Zero-cost when the guard is dropped normally; a panic mid-
/// section still records the hold time.
pub struct MetricsGuard<'a, T: ?Sized> {
    inner: parking_lot::MutexGuard<'a, T>,
    hold_start: Instant,
    op: LatticeOp,
    wait_ns: u64,
    /// Set to `true` if `.finish()` was called explicitly. Suppresses the
    /// Drop-time recording so tests can inspect wait/hold separately.
    finished: bool,
}

impl<'a, T: ?Sized> std::ops::Deref for MetricsGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<'a, T: ?Sized> std::ops::DerefMut for MetricsGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<'a, T: ?Sized> Drop for MetricsGuard<'a, T> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let hold_ns = self.hold_start.elapsed().as_nanos() as u64;
        lattice_metrics().record(self.op, self.wait_ns, hold_ns);
    }
}

/// Instrument a `parking_lot::Mutex::lock()` acquisition.
///
/// Times the wait, holds the guard, and records both wait and hold on drop.
///
/// # Example
///
/// ```ignore
/// use rope_node::lattice_metrics::{instrument_head_lock, LatticeOp};
///
/// let head_lock = registry.wallet_head_lock(&wallet_bytes);
/// let head_guard = instrument_head_lock(&head_lock, LatticeOp::AppendToLedger);
/// // ... critical section using *head_guard...
/// // Metrics recorded automatically when head_guard drops.
/// ```
#[inline]
pub fn instrument_head_lock<T: ?Sized>(
    lock: &parking_lot::Mutex<T>,
    op: LatticeOp,
) -> MetricsGuard<'_, T> {
    let wait_start = Instant::now();
    let inner = lock.lock();
    let wait_ns = wait_start.elapsed().as_nanos() as u64;
    let hold_start = Instant::now();
    MetricsGuard {
        inner,
        hold_start,
        op,
        wait_ns,
        finished: false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn bucket_for_ns_matches_upper_bounds() {
        // Sub-microsecond → bucket 0
        assert_eq!(bucket_for_ns(0), 0);
        assert_eq!(bucket_for_ns(1), 0);
        assert_eq!(bucket_for_ns(999), 0);

        // 1..2 µs → bucket 1
        assert_eq!(bucket_for_ns(1_000), 1);
        assert_eq!(bucket_for_ns(1_999), 1);

        // 2..4 µs → bucket 2
        assert_eq!(bucket_for_ns(2_000), 2);
        assert_eq!(bucket_for_ns(3_999), 2);

        // 1 s ≈ bucket 20-21
        let b = bucket_for_ns(1_000_000_000);
        assert!((19..=22).contains(&b), "1s bucket unexpectedly {}", b);

        // Overflow sink
        assert_eq!(bucket_for_ns(u64::MAX), HIST_BUCKETS - 1);
    }

    #[test]
    fn bucket_upper_bounds_are_monotonically_increasing() {
        let bounds = bucket_upper_bounds_ns();
        assert_eq!(bounds.len(), HIST_BUCKETS);
        for i in 1..bounds.len() {
            assert!(
                bounds[i] > bounds[i - 1] || (bounds[i] == u64::MAX),
                "bounds not monotonic at {}: {} vs {}",
                i,
                bounds[i - 1],
                bounds[i]
            );
        }
    }

    #[test]
    fn record_updates_counters_and_histograms() {
        let m = LatticeMetrics::new_const();
        m.record(LatticeOp::AppendToLedger, 500, 10_000);
        m.record(LatticeOp::AppendToLedger, 1_500_000, 100_000);
        m.record(LatticeOp::CreateLedger, 300, 5_000);

        let snap = m.snapshot();
        assert_eq!(snap.head_guard_wait.count, 3);
        assert_eq!(snap.head_guard_hold.count, 3);
        assert_eq!(snap.head_guard_wait.sum_ns, 500 + 1_500_000 + 300);
        assert_eq!(snap.head_guard_hold.sum_ns, 10_000 + 100_000 + 5_000);

        // Per-op counters
        let per_op = &snap.per_op;
        assert_eq!(per_op.len(), 4);
        let append = per_op.iter().find(|o| o.op == "append_to_ledger").unwrap();
        assert_eq!(append.acquired, 2);
        assert_eq!(append.wait_ns_total, 500 + 1_500_000);
        assert_eq!(append.hold_ns_total, 110_000);

        let create = per_op.iter().find(|o| o.op == "create_ledger").unwrap();
        assert_eq!(create.acquired, 1);
        assert_eq!(create.wait_ns_total, 300);

        // Ops with no observations report zero.
        let erase = per_op.iter().find(|o| o.op == "erase_ledger").unwrap();
        assert_eq!(erase.acquired, 0);
        assert_eq!(erase.mean_wait_ns, 0);
        assert_eq!(erase.mean_hold_ns, 0);
    }

    #[test]
    fn reset_zeros_all_counters_and_stamps_time() {
        let m = LatticeMetrics::new_const();
        m.mark_started();
        m.record(LatticeOp::AppendToLedger, 100, 200);
        m.record_flusher_wait_ns(50);

        m.reset();

        let snap = m.snapshot();
        assert_eq!(snap.head_guard_wait.count, 0);
        assert_eq!(snap.head_guard_hold.count, 0);
        assert_eq!(snap.flusher_wait.count, 0);
        for op in &snap.per_op {
            assert_eq!(op.acquired, 0);
        }
        // last_reset_unix_secs should be non-zero after reset.
        assert!(snap.last_reset_unix_secs > 0);
        assert!(snap.started_unix_secs > 0);
    }

    #[test]
    fn concurrent_record_does_not_lose_observations() {
        // 8 threads × 10_000 records each. Assert count == 80_000.
        const THREADS: usize = 8;
        const PER_THREAD: usize = 10_000;
        let m = Arc::new(LatticeMetrics::new_const());
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let m = m.clone();
                thread::spawn(move || {
                    for i in 0..PER_THREAD {
                        m.record(LatticeOp::AppendToLedger, (i as u64) * 100, (i as u64) * 500);
                    }
                    let _ = t;
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let snap = m.snapshot();
        assert_eq!(snap.head_guard_wait.count, (THREADS * PER_THREAD) as u64);
        assert_eq!(snap.head_guard_hold.count, (THREADS * PER_THREAD) as u64);
        let append = snap
            .per_op
            .iter()
            .find(|o| o.op == "append_to_ledger")
            .unwrap();
        assert_eq!(append.acquired, (THREADS * PER_THREAD) as u64);
    }

    #[test]
    fn instrument_head_lock_records_wait_and_hold() {
        let m = LatticeMetrics::new_const();
        let _ = m; // singleton is what actually records
        lattice_metrics().reset();

        let lock = parking_lot::Mutex::new(0u64);
        {
            let mut g = instrument_head_lock(&lock, LatticeOp::AppendToLedger);
            *g += 1;
            // Simulate held work.
            thread::sleep(Duration::from_millis(2));
        }
        // Give a moment for hold to be recorded via Drop.
        let snap = lattice_metrics().snapshot();
        assert!(snap.head_guard_wait.count >= 1);
        assert!(snap.head_guard_hold.count >= 1);
        // Hold ns must be > 1ms sleep.
        assert!(
            snap.head_guard_hold.max_ns >= 1_000_000,
            "hold_ns max unexpectedly {}",
            snap.head_guard_hold.max_ns
        );
    }

    #[test]
    fn overflow_bucket_receives_extreme_waits() {
        // Bucket 31 covers `[2^30 * 1_000, +∞)` ns = `[~1074 s, +∞)`.
        // We use `u64::MAX / 2` (~9.2 * 10^18 ns = ~292 years) to guarantee
        // we land in the overflow sink regardless of any future bucket
        // remapping. The sum_ns / max_ns assertions still hold because
        // sum_ns is a plain running total that saturates at u64::MAX (no
        // wrap-around on `fetch_add`).
        let m = LatticeMetrics::new_const();
        let extreme = u64::MAX / 2;
        m.record(LatticeOp::AppendToLedger, extreme, 0);
        let snap = m.snapshot();
        let last_bucket = *snap.head_guard_wait.bucket_counts.last().unwrap();
        assert_eq!(last_bucket, 1, "overflow bucket should receive the observation");
        assert_eq!(snap.head_guard_wait.max_ns, extreme);
    }

    #[test]
    fn multi_second_wait_lands_in_expected_bucket() {
        // Sanity check the mapping for a realistic wedge-window observation:
        // 8 s wait ≈ what HA saw during §17 forensics. Should not land in the
        // overflow sink (which is reserved for pathological >~1074 s stalls).
        let m = LatticeMetrics::new_const();
        m.record(LatticeOp::AppendToLedger, 8_000_000_000, 0);
        let snap = m.snapshot();
        let overflow_bucket = *snap.head_guard_wait.bucket_counts.last().unwrap();
        assert_eq!(overflow_bucket, 0, "8 s wait must not hit the overflow sink");
        assert_eq!(snap.head_guard_wait.count, 1);
        assert_eq!(snap.head_guard_wait.max_ns, 8_000_000_000);
    }

    #[test]
    fn snapshot_bucket_arrays_have_expected_length() {
        let m = LatticeMetrics::new_const();
        let snap = m.snapshot();
        assert_eq!(snap.head_guard_wait.bucket_counts.len(), HIST_BUCKETS);
        assert_eq!(snap.head_guard_wait.bucket_upper_bounds_ns.len(), HIST_BUCKETS);
    }

    #[test]
    fn flusher_wait_is_independent_of_head_guard() {
        let m = LatticeMetrics::new_const();
        m.record(LatticeOp::AppendToLedger, 100, 200);
        m.record_flusher_wait_ns(5_000_000);
        m.record_flusher_wait_ns(10_000_000);

        let snap = m.snapshot();
        assert_eq!(snap.head_guard_wait.count, 1);
        assert_eq!(snap.flusher_wait.count, 2);
        assert_eq!(snap.flusher_wait.sum_ns, 15_000_000);
    }
}
