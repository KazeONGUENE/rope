//! `store-write` scenario — synthetic appends, throughput + latency.

use crate::cli::StoreWriteArgs;
use crate::report::{throughput_ops_per_sec, LatencyStats, Report, StoreWriteReport};
use crate::runner::{pick_wallet, populate_descriptors, run_threads, StoreHandle, WalletPool};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn run(args: StoreWriteArgs) -> Result<Report, String> {
    let common = args.common.clone();
    if common.threads == 0 {
        return Err("threads must be > 0".into());
    }
    if common.ops == 0 {
        return Err("ops must be > 0".into());
    }
    if common.wallets == 0 {
        return Err("wallets must be > 0".into());
    }

    tracing::info!(
        target: "rope_loadgen::store_write",
        ops = common.ops,
        threads = common.threads,
        wallets = common.wallets,
        scenario = ?common.scenario,
        mode = ?common.mode,
        "starting store-write workload"
    );

    let pool = WalletPool::new(common.wallets, common.seed);
    let handle = StoreHandle::create(&common)?;

    if args.prelude_descriptors {
        populate_descriptors(&handle.store, &pool);
        // Make sure prelude writes are durable before the timed phase
        // so they don't pollute the latency histogram.
        let _ = handle.await_durable(Duration::from_secs(60));
    }

    let store = handle.store.clone();
    let pool_arc = Arc::new(pool);

    // ---- Timed phase ----
    let scenario = common.scenario;
    let threads = common.threads;
    let pool_for_workers = pool_arc.clone();

    let (elapsed, samples) = run_threads(
        common.threads,
        common.ops,
        common.seed,
        move |tid, n, rng, samples| {
            let store = store.clone();
            let pool = pool_for_workers.clone();
            for op_idx in 0..n {
                let widx = pick_wallet(scenario, &pool, tid, threads, op_idx, rng);
                let wallet = pool.get(widx);

                // Synthetic sid — first byte tags the thread, second
                // tags the op modulo, the rest is filler. Uniqueness
                // matters only insofar as appended sids collide on
                // disk; the chain CF stores them under composite
                // (wallet, seq_in_wallet) keys, so duplicates are
                // legal — but we still keep them distinct to model
                // the realistic case.
                let mut sid = [0u8; 32];
                sid[0] = tid as u8;
                sid[1] = (op_idx & 0xFF) as u8;
                sid[2] = ((op_idx >> 8) & 0xFF) as u8;
                sid[3] = ((op_idx >> 16) & 0xFF) as u8;

                let t0 = Instant::now();
                store.append_to_chain(wallet, sid);
                let dt = t0.elapsed().as_nanos() as u64;
                samples.push(dt);
            }
        },
    );

    // ---- Durability flush (timed separately) ----
    let (durable_ok, wait) = if common.await_durable {
        handle.await_durable(Duration::from_secs(120))
    } else {
        (true, Duration::ZERO)
    };
    if !durable_ok {
        return Err(format!(
            "await_all_durable timed out after {:.2}s — flusher likely stuck",
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

    let report = StoreWriteReport {
        mode: mode_s,
        scenario: scenario_s,
        threads: common.threads,
        ops_total: common.ops,
        wallets: common.wallets,
        elapsed_ms: elapsed.as_secs_f64() * 1_000.0,
        durability_wait_ms: wait.as_secs_f64() * 1_000.0,
        throughput_ops_per_sec: throughput_ops_per_sec(common.ops, elapsed),
        throughput_inc_durability_ops_per_sec: throughput_ops_per_sec(common.ops, elapsed + wait),
        latency: LatencyStats::from_samples_ns(&samples),
        prelude_descriptors: args.prelude_descriptors,
        seed: common.seed,
    };
    Ok(Report::StoreWrite(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{CommonWorkloadArgs, Mode, Scenario};

    fn args(mode: Mode, scenario: Scenario) -> StoreWriteArgs {
        StoreWriteArgs {
            common: CommonWorkloadArgs {
                threads: 2,
                ops: 200,
                wallets: 8,
                scenario,
                mode,
                db_path: None,
                await_durable: true,
                seed: 1,
            },
            prelude_descriptors: false,
        }
    }

    #[test]
    fn memory_partitioned_runs_to_completion() {
        let r = run(args(Mode::Memory, Scenario::Partitioned)).expect("run");
        match r {
            Report::StoreWrite(s) => {
                assert_eq!(s.ops_total, 200);
                assert_eq!(s.threads, 2);
                assert_eq!(s.latency.samples, 200);
                assert!(s.throughput_ops_per_sec > 0.0);
                // In memory mode, durability wait should be ~0.
                assert!(
                    s.durability_wait_ms < 5.0,
                    "memory mode durability wait was {} ms",
                    s.durability_wait_ms
                );
            }
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn rocksdb_partitioned_runs_to_completion() {
        let r = run(args(Mode::Rocksdb, Scenario::Partitioned)).expect("run");
        match r {
            Report::StoreWrite(s) => {
                assert_eq!(s.ops_total, 200);
                assert_eq!(s.latency.samples, 200);
                // Disk mode should successfully await durability.
                assert!(s.durability_wait_ms >= 0.0);
            }
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn same_wallet_scenario_serialises_through_head_lock() {
        // The Same scenario is the worst case for the per-wallet
        // head-string lock (P1.2). It must still complete and not
        // deadlock.
        let r = run(args(Mode::Memory, Scenario::Same)).expect("run");
        match r {
            Report::StoreWrite(s) => assert_eq!(s.latency.samples, 200),
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn empty_args_are_rejected() {
        let mut a = args(Mode::Memory, Scenario::Partitioned);
        a.common.threads = 0;
        assert!(run(a).is_err());

        let mut a = args(Mode::Memory, Scenario::Partitioned);
        a.common.ops = 0;
        assert!(run(a).is_err());

        let mut a = args(Mode::Memory, Scenario::Partitioned);
        a.common.wallets = 0;
        assert!(run(a).is_err());
    }
}
