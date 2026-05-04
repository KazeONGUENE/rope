//! Report types — JSON-serialisable for CI consumption, with a
//! human-readable summary printed to stderr.
//!
//! Latencies are captured per op into per-thread `Vec<u64>` (nanoseconds)
//! during the timed phase, then aggregated into an `hdrhistogram::Histogram`
//! at report time. We deliberately do NOT touch the histogram on the hot
//! path — `record()` calls under contention skew measurements significantly.

use hdrhistogram::Histogram;
use serde::Serialize;
use std::time::Duration;

/// Top-level report serialised to JSON on stdout.
///
/// Variants are flattened into a single JSON object via `#[serde(tag = ...)]`
/// so CI scripts can `jq -r '.scenario_kind'` regardless of subcommand.
#[derive(Serialize, Debug)]
#[serde(tag = "scenario_kind", rename_all = "kebab-case")]
pub enum Report {
    StoreWrite(StoreWriteReport),
    StoreRecover(StoreRecoverReport),
    StoreMixed(StoreMixedReport),
    ManagerWrite(ManagerWriteReport),
    VerifyBatch(VerifyBatchReport),
}

impl Report {
    pub fn human_summary(&self) -> String {
        match self {
            Report::StoreWrite(r) => r.human_summary(),
            Report::StoreRecover(r) => r.human_summary(),
            Report::StoreMixed(r) => r.human_summary(),
            Report::ManagerWrite(r) => r.human_summary(),
            Report::VerifyBatch(r) => r.human_summary(),
        }
    }
}

#[derive(Serialize, Debug)]
pub struct StoreWriteReport {
    pub mode: String,
    pub scenario: String,
    pub threads: usize,
    pub ops_total: usize,
    pub wallets: usize,
    pub elapsed_ms: f64,
    pub durability_wait_ms: f64,
    pub throughput_ops_per_sec: f64,
    pub throughput_inc_durability_ops_per_sec: f64,
    pub latency: LatencyStats,
    pub prelude_descriptors: bool,
    pub seed: u64,
}

impl StoreWriteReport {
    pub fn human_summary(&self) -> String {
        format!(
            "store-write summary\n\
             ===================\n  mode               : {mode}\n  scenario           : {scenario}\n  \
             threads            : {threads}\n  wallets            : {wallets}\n  ops total          : {ops}\n  \
             elapsed            : {elapsed:>10.2} ms\n  durability wait    : {wait:>10.2} ms\n  \
             throughput (work)  : {tput:>14.0} ops/s\n  throughput (+wait) : {tput_d:>14.0} ops/s\n  \
             latency p50        : {p50:>10.2} µs\n  latency p95        : {p95:>10.2} µs\n  \
             latency p99        : {p99:>10.2} µs\n  latency p99.9      : {p999:>10.2} µs\n  \
             latency max        : {pmax:>10.2} µs",
            mode = self.mode,
            scenario = self.scenario,
            threads = self.threads,
            wallets = self.wallets,
            ops = self.ops_total,
            elapsed = self.elapsed_ms,
            wait = self.durability_wait_ms,
            tput = self.throughput_ops_per_sec,
            tput_d = self.throughput_inc_durability_ops_per_sec,
            p50 = self.latency.p50_us,
            p95 = self.latency.p95_us,
            p99 = self.latency.p99_us,
            p999 = self.latency.p999_us,
            pmax = self.latency.max_us,
        )
    }
}

#[derive(Serialize, Debug)]
pub struct StoreRecoverReport {
    pub db_path: String,
    pub iterations: usize,
    pub recovered_descriptors: usize,
    pub recovered_chain_entries: usize,
    pub recovered_pieces: usize,
    pub durable_seq: u64,
    /// Per-iteration cold-open elapsed time (ms).
    pub iteration_ms: Vec<f64>,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

impl StoreRecoverReport {
    pub fn human_summary(&self) -> String {
        format!(
            "store-recover summary\n\
             =====================\n  db path            : {path}\n  iterations         : {n}\n  \
             descriptors        : {desc}\n  chain entries      : {chain}\n  piece maps         : {pieces}\n  \
             durable_seq        : {seq}\n  recover mean       : {mean:>10.2} ms\n  \
             recover p50        : {p50:>10.2} ms\n  recover p95        : {p95:>10.2} ms\n  \
             recover max        : {max:>10.2} ms",
            path = self.db_path,
            n = self.iterations,
            desc = self.recovered_descriptors,
            chain = self.recovered_chain_entries,
            pieces = self.recovered_pieces,
            seq = self.durable_seq,
            mean = self.mean_ms,
            p50 = self.p50_ms,
            p95 = self.p95_ms,
            max = self.max_ms,
        )
    }
}

#[derive(Serialize, Debug)]
pub struct StoreMixedReport {
    pub mode: String,
    pub scenario: String,
    pub threads: usize,
    pub ops_total: usize,
    pub wallets: usize,
    pub weights: WeightsBreakdown,
    pub elapsed_ms: f64,
    pub durability_wait_ms: f64,
    pub throughput_ops_per_sec: f64,
    pub throughput_inc_durability_ops_per_sec: f64,
    pub latency: LatencyStats,
    pub op_counts: OpCounts,
    pub seed: u64,
}

impl StoreMixedReport {
    pub fn human_summary(&self) -> String {
        format!(
            "store-mixed summary\n\
             ===================\n  mode               : {mode}\n  scenario           : {scenario}\n  \
             threads            : {threads}\n  wallets            : {wallets}\n  ops total          : {ops}\n  \
             weights            : append={wa:.2} put={wp:.2} mark={wm:.2} get={wg:.2}\n  \
             ops executed       : append={ca} put={cp} mark={cm} get={cg}\n  \
             elapsed            : {elapsed:>10.2} ms\n  durability wait    : {wait:>10.2} ms\n  \
             throughput (work)  : {tput:>14.0} ops/s\n  throughput (+wait) : {tput_d:>14.0} ops/s\n  \
             latency p50        : {p50:>10.2} µs\n  latency p95        : {p95:>10.2} µs\n  \
             latency p99        : {p99:>10.2} µs\n  latency p99.9      : {p999:>10.2} µs\n  \
             latency max        : {pmax:>10.2} µs",
            mode = self.mode,
            scenario = self.scenario,
            threads = self.threads,
            wallets = self.wallets,
            ops = self.ops_total,
            wa = self.weights.append,
            wp = self.weights.put_descriptor,
            wm = self.weights.mark_deleted,
            wg = self.weights.get_chain,
            ca = self.op_counts.append,
            cp = self.op_counts.put_descriptor,
            cm = self.op_counts.mark_deleted,
            cg = self.op_counts.get_chain,
            elapsed = self.elapsed_ms,
            wait = self.durability_wait_ms,
            tput = self.throughput_ops_per_sec,
            tput_d = self.throughput_inc_durability_ops_per_sec,
            p50 = self.latency.p50_us,
            p95 = self.latency.p95_us,
            p99 = self.latency.p99_us,
            p999 = self.latency.p999_us,
            pmax = self.latency.max_us,
        )
    }
}

#[derive(Serialize, Debug)]
pub struct ManagerWriteReport {
    pub mode: String,
    pub scenario: String,
    pub threads: usize,
    pub ops_total: usize,
    pub wallets: usize,
    pub payload_bytes: usize,
    /// Time spent in the untimed prelude (one `create_ledger` per wallet)
    /// — useful for understanding cold-start cost vs steady-state append
    /// throughput.
    pub create_ledger_total_ms: f64,
    pub create_ledger_throughput_per_sec: f64,
    /// Errors during create_ledger (e.g. address parse failure). Should
    /// be 0 on a healthy run.
    pub create_ledger_errors: usize,
    pub elapsed_ms: f64,
    pub durability_wait_ms: f64,
    pub throughput_ops_per_sec: f64,
    pub throughput_inc_durability_ops_per_sec: f64,
    pub latency: LatencyStats,
    /// Errors during the timed phase. Reported but do NOT affect the
    /// throughput numerator — they ARE counted as ops attempted.
    pub append_errors: usize,
    pub seed: u64,
}

impl ManagerWriteReport {
    pub fn human_summary(&self) -> String {
        format!(
            "manager-write summary\n\
             =====================\n  mode               : {mode}\n  scenario           : {scenario}\n  \
             threads            : {threads}\n  wallets            : {wallets}\n  payload bytes      : {bytes}\n  \
             ops total          : {ops}\n  create_ledger      : {clt:>10.2} ms total ({cthroughput:>10.0}/s)\n  \
             create errors      : {cerr}\n  elapsed (append)   : {elapsed:>10.2} ms\n  \
             durability wait    : {wait:>10.2} ms\n  throughput (work)  : {tput:>14.0} ops/s\n  \
             throughput (+wait) : {tput_d:>14.0} ops/s\n  append errors      : {aerr}\n  \
             latency p50        : {p50:>10.2} µs\n  latency p95        : {p95:>10.2} µs\n  \
             latency p99        : {p99:>10.2} µs\n  latency p99.9      : {p999:>10.2} µs\n  \
             latency max        : {pmax:>10.2} µs",
            mode = self.mode,
            scenario = self.scenario,
            threads = self.threads,
            wallets = self.wallets,
            bytes = self.payload_bytes,
            ops = self.ops_total,
            clt = self.create_ledger_total_ms,
            cthroughput = self.create_ledger_throughput_per_sec,
            cerr = self.create_ledger_errors,
            elapsed = self.elapsed_ms,
            wait = self.durability_wait_ms,
            tput = self.throughput_ops_per_sec,
            tput_d = self.throughput_inc_durability_ops_per_sec,
            aerr = self.append_errors,
            p50 = self.latency.p50_us,
            p95 = self.latency.p95_us,
            p99 = self.latency.p99_us,
            p999 = self.latency.p999_us,
            pmax = self.latency.max_us,
        )
    }
}

#[derive(Serialize, Debug, Default)]
pub struct WeightsBreakdown {
    pub append: f64,
    pub put_descriptor: f64,
    pub mark_deleted: f64,
    pub get_chain: f64,
}

#[derive(Serialize, Debug, Default)]
pub struct OpCounts {
    pub append: usize,
    pub put_descriptor: usize,
    pub mark_deleted: usize,
    pub get_chain: usize,
}

// ============================================================================
// Quipu Canon v2.0 Phase 2.C — verify-batch report
// ============================================================================

#[derive(Serialize, Debug)]
pub struct VerifyBatchReport {
    pub items: usize,
    pub keys: usize,
    pub payload_bytes: usize,
    pub iterations: usize,
    pub cold_cache: bool,
    pub seed: u64,
    /// Detected logical CPUs at run time (drives parallel speedup).
    pub logical_cpus: usize,
    /// Mean wall-clock of one full serial (verify-loop) pass.
    pub serial_elapsed_ms: f64,
    /// Mean wall-clock of one full batch (verify_batch) pass.
    pub batch_elapsed_ms: f64,
    /// Verifications per second on the serial path.
    pub serial_throughput_ops_per_sec: f64,
    /// Verifications per second on the batch path.
    pub batch_throughput_ops_per_sec: f64,
    /// Speedup factor batch/serial; > 1 means batch is faster.
    pub batch_speedup_x: f64,
    /// Mean per-item µs on the serial path.
    pub serial_per_item_us: f64,
    /// Mean per-item µs on the batch path.
    pub batch_per_item_us: f64,
}

impl VerifyBatchReport {
    pub fn human_summary(&self) -> String {
        format!(
            "verify-batch summary (Quipu Canon v2.0 Phase 2.C)\n\
             ==================================================\n  \
             items              : {items}\n  keys               : {keys}\n  payload bytes      : {payload}\n  \
             iterations         : {iters}\n  cold cache         : {cold}\n  logical cpus       : {cpus}\n  \
             serial elapsed     : {serial:>10.2} ms\n  batch  elapsed     : {batch:>10.2} ms\n  \
             serial per-item    : {sp_item:>10.2} µs\n  batch  per-item    : {bp_item:>10.2} µs\n  \
             serial throughput  : {sp_tput:>14.0} verify/s\n  batch  throughput  : {bp_tput:>14.0} verify/s\n  \
             speedup            : {speedup:>14.2}x\n",
            items = self.items,
            keys = self.keys,
            payload = self.payload_bytes,
            iters = self.iterations,
            cold = self.cold_cache,
            cpus = self.logical_cpus,
            serial = self.serial_elapsed_ms,
            batch = self.batch_elapsed_ms,
            sp_item = self.serial_per_item_us,
            bp_item = self.batch_per_item_us,
            sp_tput = self.serial_throughput_ops_per_sec,
            bp_tput = self.batch_throughput_ops_per_sec,
            speedup = self.batch_speedup_x,
        )
    }
}

/// Latency percentiles in microseconds.
///
/// Computed from raw nanosecond samples via `hdrhistogram` with 3
/// significant digits — i.e. ≤ 0.1% relative error across the full
/// 1 ns – 1 minute range.
#[derive(Serialize, Debug, Default)]
pub struct LatencyStats {
    pub samples: usize,
    pub mean_us: f64,
    pub p50_us: f64,
    pub p90_us: f64,
    pub p95_us: f64,
    pub p99_us: f64,
    pub p999_us: f64,
    pub max_us: f64,
}

impl LatencyStats {
    /// Aggregate raw nanosecond samples into percentile stats.
    ///
    /// The histogram tracks values from 1 ns up to 60 s, which more
    /// than covers any plausible per-op latency for the LedgerStore
    /// (typical: 1-50 µs in-memory, 5-200 µs RocksDB).
    pub fn from_samples_ns(samples: &[u64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }

        // 1 ns lower bound, 60 s upper bound, 3 sig-digits precision.
        // hdrhistogram errors only on truly absurd config (e.g. high < low).
        let mut hist: Histogram<u64> = Histogram::new_with_bounds(1, 60_000_000_000, 3)
            .expect("static histogram bounds are valid");

        for &s in samples {
            // Clamp to the upper bound — anything > 60 s is recorded
            // at the top bucket rather than dropped, which preserves
            // the count for the throughput maths.
            let clamped = s.clamp(1, 60_000_000_000);
            hist.record(clamped)
                .expect("clamped value is within histogram bounds");
        }

        Self {
            samples: samples.len(),
            mean_us: hist.mean() / 1_000.0,
            p50_us: hist.value_at_quantile(0.50) as f64 / 1_000.0,
            p90_us: hist.value_at_quantile(0.90) as f64 / 1_000.0,
            p95_us: hist.value_at_quantile(0.95) as f64 / 1_000.0,
            p99_us: hist.value_at_quantile(0.99) as f64 / 1_000.0,
            p999_us: hist.value_at_quantile(0.999) as f64 / 1_000.0,
            max_us: hist.max() as f64 / 1_000.0,
        }
    }
}

/// Throughput helpers — kept here so callers don't reinvent the
/// duration-to-rate formula and accidentally divide by zero.
pub fn throughput_ops_per_sec(ops: usize, dur: Duration) -> f64 {
    if dur.as_nanos() == 0 {
        return 0.0;
    }
    ops as f64 / dur.as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_stats_from_empty_is_zeroed() {
        let s = LatencyStats::from_samples_ns(&[]);
        assert_eq!(s.samples, 0);
        assert_eq!(s.p50_us, 0.0);
        assert_eq!(s.max_us, 0.0);
    }

    #[test]
    fn latency_stats_basic_percentiles() {
        // 100 samples, 1 µs to 100 µs in 1 µs steps.
        let samples: Vec<u64> = (1..=100).map(|i| i * 1_000).collect();
        let s = LatencyStats::from_samples_ns(&samples);
        assert_eq!(s.samples, 100);
        // p50 of 1..=100 µs sits around 50 µs (HDR rounding allows ~0.1%).
        assert!(
            (40.0..=60.0).contains(&s.p50_us),
            "p50 was {}, expected ~50",
            s.p50_us
        );
        assert!(s.p99_us >= 99.0, "p99 was {}, expected >= 99", s.p99_us);
        assert!(s.max_us >= 100.0, "max was {}, expected >= 100", s.max_us);
    }

    #[test]
    fn latency_stats_handles_outliers() {
        // 99 small + 1 huge — p99 small, max huge.
        let mut samples = vec![1_000u64; 99];
        samples.push(10_000_000_000); // 10 s outlier
        let s = LatencyStats::from_samples_ns(&samples);
        assert!(s.p50_us <= 2.0, "p50 was {}", s.p50_us);
        assert!(
            s.max_us >= 1_000_000.0,
            "max was {} µs, expected ≥ 1 s",
            s.max_us
        );
    }

    #[test]
    fn throughput_zero_duration_does_not_panic() {
        assert_eq!(throughput_ops_per_sec(100, Duration::ZERO), 0.0);
    }

    #[test]
    fn throughput_basic() {
        let t = throughput_ops_per_sec(1_000_000, Duration::from_secs(2));
        assert!(
            (499_999.0..=500_001.0).contains(&t),
            "throughput was {}, expected ~500_000",
            t
        );
    }
}
