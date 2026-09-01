//! `store-mixed` scenario — interleaved put_descriptor / append /
//! mark_deleted / get_chain workload to model real-world load.

use crate::cli::StoreMixedArgs;
use crate::report::{
    throughput_ops_per_sec, LatencyStats, OpCounts, Report, StoreMixedReport, WeightsBreakdown,
};
use crate::runner::{pick_wallet, populate_descriptors, run_threads, StoreHandle, WalletPool};
use parking_lot::Mutex;
use rand::RngCore;
use rope_storage::ledger_db::StoredLedgerDescriptor;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn run(args: StoreMixedArgs) -> Result<Report, String> {
    let common = args.common.clone();
    if common.threads == 0 || common.ops == 0 || common.wallets == 0 {
        return Err("threads, ops, and wallets must all be > 0".into());
    }
    let total_weight = args.weight_append
        + args.weight_put_descriptor
        + args.weight_mark_deleted
        + args.weight_get_chain;
    if total_weight <= 0.0 {
        return Err("at least one weight must be > 0".into());
    }

    tracing::info!(
        target: "rope_loadgen::store_mixed",
        ops = common.ops,
        threads = common.threads,
        wallets = common.wallets,
        scenario = ?common.scenario,
        mode = ?common.mode,
        "starting store-mixed workload"
    );

    let pool = WalletPool::new(common.wallets, common.seed);
    let handle = StoreHandle::create(&common)?;

    // Always pre-populate so get_chain / mark_deleted have something
    // to operate on.
    populate_descriptors(&handle.store, &pool);
    let _ = handle.await_durable(Duration::from_secs(60));

    // Normalised cumulative weights for op selection.
    let cum_append = args.weight_append / total_weight;
    let cum_put = cum_append + args.weight_put_descriptor / total_weight;
    let cum_mark = cum_put + args.weight_mark_deleted / total_weight;
    // get_chain implicitly takes the remainder.

    let store = handle.store.clone();
    let pool_arc = Arc::new(pool);
    let scenario = common.scenario;
    let threads = common.threads;
    let pool_for_workers = pool_arc.clone();

    // Op counters — atomics so per-thread increments don't take a
    // mutex on the hot path.
    let cnt_append = Arc::new(AtomicUsize::new(0));
    let cnt_put = Arc::new(AtomicUsize::new(0));
    let cnt_mark = Arc::new(AtomicUsize::new(0));
    let cnt_get = Arc::new(AtomicUsize::new(0));

    // Black-hole sink for `get_chain` results so the optimiser can't
    // remove the call. Mutex<usize> totals up the cumulative chain
    // length seen — small contention but only on the get path.
    let read_sink = Arc::new(Mutex::new(0usize));

    let cnt_append_hot = cnt_append.clone();
    let cnt_put_hot = cnt_put.clone();
    let cnt_mark_hot = cnt_mark.clone();
    let cnt_get_hot = cnt_get.clone();
    let read_sink_hot = read_sink.clone();

    let (elapsed, samples) = run_threads(
        common.threads,
        common.ops,
        common.seed,
        move |tid, n, rng, samples| {
            let store = store.clone();
            let pool = pool_for_workers.clone();
            let cnt_append = cnt_append_hot.clone();
            let cnt_put = cnt_put_hot.clone();
            let cnt_mark = cnt_mark_hot.clone();
            let cnt_get = cnt_get_hot.clone();
            let sink = read_sink_hot.clone();

            for op_idx in 0..n {
                let widx = pick_wallet(scenario, &pool, tid, threads, op_idx, rng);
                let wallet = pool.get(widx);

                // Pick op type by cumulative weight.
                // ChaCha gives us a uniform u64; map to [0, 1).
                let r = (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;

                let t0 = Instant::now();
                if r < cum_append {
                    let mut sid = [0u8; 32];
                    sid[0] = tid as u8;
                    sid[1] = (op_idx & 0xFF) as u8;
                    sid[2] = ((op_idx >> 8) & 0xFF) as u8;
                    let _ = store.append_to_chain(wallet, sid);
                    cnt_append.fetch_add(1, Ordering::Relaxed);
                } else if r < cum_put {
                    let mut head = [0u8; 32];
                    head[..wallet.len().min(32)].copy_from_slice(&wallet[..wallet.len().min(32)]);
                    head[31] = (op_idx & 0xFF) as u8;
                    let _ = store.put_descriptor(
                        wallet,
                        StoredLedgerDescriptor {
                            wallet_address: wallet.to_vec(),
                            genesis_string_id: head,
                            head_string_id: head,
                            entry_count: op_idx as u64,
                            total_size_bytes: 0,
                            oes_generation_at_creation: 0,
                            current_oes_generation: 0,
                            created_at: 1_700_000_000,
                            last_appended_at: 1_700_000_000,
                            is_deleted: false,
                            deleted_at: None,
                            replication_factor: 5,
                        },
                    );
                    cnt_put.fetch_add(1, Ordering::Relaxed);
                } else if r < cum_mark {
                    let _ = store.mark_deleted(wallet);
                    cnt_mark.fetch_add(1, Ordering::Relaxed);
                } else {
                    let chain = store.get_chain(wallet);
                    // Sink accumulates lengths so the call cannot be DCE'd.
                    *sink.lock() += chain.len();
                    cnt_get.fetch_add(1, Ordering::Relaxed);
                }
                let dt = t0.elapsed().as_nanos() as u64;
                samples.push(dt);
            }
        },
    );

    let (durable_ok, wait) = if common.await_durable {
        handle.await_durable(Duration::from_secs(120))
    } else {
        (true, Duration::ZERO)
    };
    if !durable_ok {
        return Err(format!(
            "await_all_durable timed out after {:.2}s",
            wait.as_secs_f64()
        ));
    }

    let mode_s = match common.mode {
        crate::cli::Mode::Memory => "memory".to_string(),
        crate::cli::Mode::Rocksdb => "rocksdb".to_string(),
    };
    let scenario_s = match common.scenario {
        crate::cli::Scenario::Same => "same".to_string(),
        crate::cli::Scenario::Partitioned => "partitioned".to_string(),
        crate::cli::Scenario::Random => "random".to_string(),
    };

    Ok(Report::StoreMixed(StoreMixedReport {
        mode: mode_s,
        scenario: scenario_s,
        threads: common.threads,
        ops_total: common.ops,
        wallets: common.wallets,
        weights: WeightsBreakdown {
            append: args.weight_append / total_weight,
            put_descriptor: args.weight_put_descriptor / total_weight,
            mark_deleted: args.weight_mark_deleted / total_weight,
            get_chain: args.weight_get_chain / total_weight,
        },
        elapsed_ms: elapsed.as_secs_f64() * 1_000.0,
        durability_wait_ms: wait.as_secs_f64() * 1_000.0,
        throughput_ops_per_sec: throughput_ops_per_sec(common.ops, elapsed),
        throughput_inc_durability_ops_per_sec: throughput_ops_per_sec(common.ops, elapsed + wait),
        latency: LatencyStats::from_samples_ns(&samples),
        op_counts: OpCounts {
            append: cnt_append.load(Ordering::Relaxed),
            put_descriptor: cnt_put.load(Ordering::Relaxed),
            mark_deleted: cnt_mark.load(Ordering::Relaxed),
            get_chain: cnt_get.load(Ordering::Relaxed),
        },
        seed: common.seed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{CommonWorkloadArgs, Mode, Scenario};

    fn args() -> StoreMixedArgs {
        StoreMixedArgs {
            common: CommonWorkloadArgs {
                threads: 2,
                ops: 200,
                wallets: 8,
                scenario: Scenario::Partitioned,
                mode: Mode::Memory,
                db_path: None,
                await_durable: true,
                seed: 1,
            },
            weight_append: 0.7,
            weight_put_descriptor: 0.1,
            weight_mark_deleted: 0.05,
            weight_get_chain: 0.15,
        }
    }

    #[test]
    fn mixed_runs_to_completion_and_counts_match() {
        let r = run(args()).expect("run");
        match r {
            Report::StoreMixed(s) => {
                assert_eq!(s.ops_total, 200);
                let total_executed = s.op_counts.append
                    + s.op_counts.put_descriptor
                    + s.op_counts.mark_deleted
                    + s.op_counts.get_chain;
                assert_eq!(total_executed, 200, "sum of op counts must equal ops_total");
            }
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn weights_normalise() {
        let mut a = args();
        a.weight_append = 7.0;
        a.weight_put_descriptor = 1.0;
        a.weight_mark_deleted = 0.5;
        a.weight_get_chain = 1.5;
        let r = run(a).expect("run");
        match r {
            Report::StoreMixed(s) => {
                let total = s.weights.append
                    + s.weights.put_descriptor
                    + s.weights.mark_deleted
                    + s.weights.get_chain;
                assert!(
                    (total - 1.0).abs() < 1e-6,
                    "weights should normalise to 1.0, got {}",
                    total
                );
            }
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn all_zero_weights_rejected() {
        let mut a = args();
        a.weight_append = 0.0;
        a.weight_put_descriptor = 0.0;
        a.weight_mark_deleted = 0.0;
        a.weight_get_chain = 0.0;
        assert!(run(a).is_err());
    }
}
