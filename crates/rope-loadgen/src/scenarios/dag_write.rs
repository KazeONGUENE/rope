//! `dag-write` — Quipu Canon v2.0 Phase 2.E scenario.
//!
//! Drives concurrent appends to per-wallet `KnotDag` instances and
//! reports throughput, per-wallet tip-set growth, and latency
//! percentiles. Designed to demonstrate that the DAG canon lifts
//! the per-wallet head-lock ceiling that Phase 1.2 still enforced.
//!
//! ## Two scenarios
//!
//! - **`--single-wallet`**: every thread targets the SAME wallet.
//!   Worst case for the linear chain (full head-lock contention),
//!   best case for the DAG canon (every append commits without
//!   blocking; the wallet ends up with a fan-out tip set).
//! - default: each op picks a uniformly random wallet from the
//!   pool. Realistic mixed shape.

use crate::cli::DagWriteArgs;
use crate::report::{throughput_ops_per_sec, DagWriteReport, LatencyStats, Report};
use rand::SeedableRng;
use rand::{Rng, RngCore};
use rand_chacha::ChaCha20Rng;
use rope_core::{KnotDag, KnotDagRegistry, StringId};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

pub fn run(args: DagWriteArgs) -> Result<Report, String> {
    if args.threads == 0 {
        return Err("--threads must be ≥ 1".to_string());
    }
    if args.wallets == 0 {
        return Err("--wallets must be ≥ 1".to_string());
    }
    if args.ops == 0 {
        return Err("--ops must be ≥ 1".to_string());
    }

    let registry = Arc::new(KnotDagRegistry::new());

    // Pre-generate the wallet pool. With `--single-wallet`, the
    // first wallet is the only one used.
    let mut prng = ChaCha20Rng::seed_from_u64(args.seed);
    let mut wallets: Vec<Vec<u8>> = Vec::with_capacity(args.wallets);
    for _ in 0..args.wallets {
        let mut w = vec![0u8; 20];
        prng.fill_bytes(&mut w);
        wallets.push(w);
    }
    let wallets = Arc::new(wallets);

    // Seed each wallet with a genesis knot. Untimed.
    for (i, w) in wallets.iter().enumerate() {
        let mut bytes = [0u8; 32];
        bytes[0..2].copy_from_slice(&(i as u16).to_le_bytes());
        bytes[2] = 0xFE; // genesis marker
        let g = StringId::new(bytes);
        registry.append(w, g, &[]).expect("genesis append");
    }

    let ops_per_thread = args.ops / args.threads;
    let total_ops = ops_per_thread * args.threads;

    let started = Instant::now();
    let mut handles = Vec::with_capacity(args.threads);
    for t in 0..args.threads {
        let registry = registry.clone();
        let wallets = wallets.clone();
        let seed = args.seed.wrapping_add(t as u64);
        let single_wallet = args.single_wallet;
        handles.push(thread::spawn(move || {
            let mut prng = ChaCha20Rng::seed_from_u64(seed);
            let mut latencies: Vec<u64> = Vec::with_capacity(ops_per_thread);
            let mut nonce: u64 = (t as u64) << 32;
            for _ in 0..ops_per_thread {
                let widx = if single_wallet {
                    0
                } else {
                    prng.gen_range(0..wallets.len())
                };
                let wallet = &wallets[widx];
                // Build a unique knot id: thread bits in the high
                // half, per-thread monotonic nonce in the low half.
                let mut bytes = [0u8; 32];
                bytes[0..8].copy_from_slice(&nonce.to_le_bytes());
                bytes[8] = t as u8;
                let new_id = StringId::new(bytes);
                nonce = nonce.wrapping_add(1);

                // Read current tips, append referencing them.
                let dag: Arc<KnotDag> = registry.dag_for(wallet);
                let parents = dag.tips();
                let s = Instant::now();
                // Best-effort: a duplicate id means another thread
                // raced us to the same nonce — extremely unlikely
                // but counted as a no-op latency-wise.
                let _ = dag.add_knot(new_id, &parents);
                latencies.push(s.elapsed().as_nanos() as u64);
            }
            latencies
        }));
    }

    let mut all_latencies: Vec<u64> = Vec::with_capacity(total_ops);
    for h in handles {
        all_latencies.extend(h.join().expect("worker panicked"));
    }
    let elapsed = started.elapsed();

    // Aggregate per-wallet stats.
    let distinct = registry.wallet_count();
    let mut sum_size = 0usize;
    let mut max_tips = 0usize;
    let mut sum_tips: u64 = 0;
    for w in wallets.iter() {
        let dag = registry.dag_for(w);
        sum_size += dag.len();
        let t = dag.tips().len();
        if t > max_tips {
            max_tips = t;
        }
        sum_tips += t as u64;
    }
    let mean_tips = if distinct > 0 {
        sum_tips as f64 / distinct as f64
    } else {
        0.0
    };

    let throughput = throughput_ops_per_sec(total_ops, elapsed);
    let latency = LatencyStats::from_samples_ns(&all_latencies);

    let report = DagWriteReport {
        mode: if args.single_wallet {
            "single-wallet".to_string()
        } else {
            "random-wallet".to_string()
        },
        wallets: args.wallets,
        threads: args.threads,
        ops_total: total_ops,
        elapsed_ms: elapsed.as_secs_f64() * 1_000.0,
        throughput_ops_per_sec: throughput,
        mean_per_op_us: latency.mean_us,
        distinct_wallets_touched: distinct,
        sum_dag_size: sum_size,
        max_tip_count: max_tips,
        mean_tip_count: mean_tips,
        seed: args.seed,
        latency,
    };

    Ok(Report::DagWrite(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(threads: usize, ops: usize, wallets: usize, single: bool) -> DagWriteArgs {
        DagWriteArgs {
            wallets,
            ops,
            threads,
            single_wallet: single,
            seed: 42,
        }
    }

    #[test]
    fn random_wallet_smoke() {
        let r = run(args(4, 1000, 32, false)).unwrap();
        match r {
            Report::DagWrite(rep) => {
                // 32 genesis + 1000 ops × 1 knot each = 1032
                assert_eq!(rep.sum_dag_size, 32 + 1000);
                assert_eq!(rep.distinct_wallets_touched, 32);
            }
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn single_wallet_high_concurrency_succeeds() {
        // The whole point of the DAG canon: many threads slamming
        // the same wallet must NOT lose appends. The tip-set fan-out
        // depends on race timing — under fast deterministic
        // single-machine runs the tip set may converge back to 1
        // by end of run (each new tip immediately demotes the
        // previous one) — what matters here is that NO appends
        // were dropped because of head-lock contention.
        let r = run(args(8, 4000, 4, true)).unwrap();
        match r {
            Report::DagWrite(rep) => {
                // Genesis × 4 + 4000 timed ops on wallet[0].
                assert_eq!(rep.sum_dag_size, 4 + 4000);
                assert_eq!(rep.distinct_wallets_touched, 4);
                // The tip count is ≥ 1 always — the concrete value
                // depends on how often threads' `tips()` reads
                // raced with each other's `add_knot` writes.
                assert!(rep.max_tip_count >= 1);
            }
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn single_wallet_with_barrier_produces_fanout() {
        // Stronger guarantee: with an explicit barrier so threads
        // genuinely race each other on the SAME read of `tips()`,
        // the tip set MUST grow above 1.
        use rope_core::{KnotDag, KnotDagRegistry, StringId};
        use std::sync::{Arc, Barrier};
        use std::thread;

        let registry = Arc::new(KnotDagRegistry::new());
        let wallet = vec![0xAA; 20];
        let g = StringId::new([0u8; 32]);
        registry.append(&wallet, g, &[]).unwrap();

        const N: usize = 16;
        let barrier = Arc::new(Barrier::new(N));
        let mut handles = Vec::with_capacity(N);
        for t in 0..N {
            let registry = registry.clone();
            let wallet = wallet.clone();
            let barrier = barrier.clone();
            handles.push(thread::spawn(move || {
                let dag: Arc<KnotDag> = registry.dag_for(&wallet);
                let parents = dag.tips();
                // All threads pause until everyone has read the same
                // tip set, then race to append concurrently.
                barrier.wait();
                let mut bytes = [0u8; 32];
                bytes[0] = 0xC0;
                bytes[1] = t as u8;
                let id = StringId::new(bytes);
                dag.add_knot(id, &parents).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let dag = registry.dag_for(&wallet);
        // Genesis + N siblings = N + 1 knots.
        assert_eq!(dag.len(), N + 1);
        // All N siblings must be tips (none has children yet).
        assert_eq!(
            dag.tips().len(),
            N,
            "barrier-coordinated fan-out must produce N concurrent tips"
        );
        // Genesis must NOT be a tip.
        assert!(!dag.is_tip(&g));
    }

    #[test]
    fn rejects_zero_threads() {
        assert!(run(args(0, 1, 1, false)).is_err());
    }

    #[test]
    fn rejects_zero_wallets() {
        assert!(run(args(1, 1, 0, false)).is_err());
    }
}
