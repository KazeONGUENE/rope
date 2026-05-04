//! `store-recover` scenario — open an existing RocksDB-backed store
//! and time the cold-recovery snapshot rebuild.

use crate::cli::StoreRecoverArgs;
use crate::report::{Report, StoreRecoverReport};
use rope_storage::{ledger_db::LedgerStore, rocksdb_persistence::RocksPersistence};
use std::time::Instant;

pub fn run(args: StoreRecoverArgs) -> Result<Report, String> {
    if args.iterations == 0 {
        return Err("iterations must be > 0".into());
    }
    if !args.db_path.exists() {
        return Err(format!(
            "db path does not exist: {}",
            args.db_path.display()
        ));
    }

    tracing::info!(
        target: "rope_loadgen::store_recover",
        path = %args.db_path.display(),
        iterations = args.iterations,
        "starting store-recover workload"
    );

    let mut times_ms: Vec<f64> = Vec::with_capacity(args.iterations);

    // Track the recovery contents on the LAST iteration only (so we
    // don't pay the conversion cost N times). All iterations recover
    // identical state since we never write.
    let mut last_descriptors = 0usize;
    let mut last_chain_entries = 0usize;
    let mut last_pieces = 0usize;
    let mut last_durable_seq = 0u64;

    for i in 0..args.iterations {
        let t0 = Instant::now();
        let (persistence, recovered) = RocksPersistence::open(&args.db_path)
            .map_err(|e| format!("opening rocksdb at {}: {e}", args.db_path.display()))?;
        // Build the store from the recovered snapshot — this is what a
        // node startup actually does, and it's the cost the operator
        // sees as "node startup time".
        let store = LedgerStore::from_recovered(persistence, {
            // Fan out the snapshot for both the store builder and our
            // metric capture without cloning the heavy fields twice.
            // We pull cheap counts BEFORE handing the snapshot to
            // `from_recovered`.
            last_descriptors = recovered.descriptors.len();
            last_chain_entries = recovered.chains.iter().map(|(_, c)| c.len()).sum();
            last_pieces = recovered.pieces.len();
            last_durable_seq = recovered.durable_seq;
            recovered
        });
        let dt_ms = t0.elapsed().as_secs_f64() * 1_000.0;

        // Sanity: prevent the optimiser dropping the store entirely.
        std::hint::black_box(&store);

        times_ms.push(dt_ms);
        tracing::info!(
            target: "rope_loadgen::store_recover",
            iter = i,
            elapsed_ms = dt_ms,
            descriptors = last_descriptors,
            chain_entries = last_chain_entries,
            "iteration complete"
        );

        // Drop the store between iterations so the next open is truly cold.
        drop(store);
    }

    let mean_ms = times_ms.iter().sum::<f64>() / times_ms.len() as f64;
    let mut sorted = times_ms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50_ms = percentile(&sorted, 0.50);
    let p95_ms = percentile(&sorted, 0.95);
    let max_ms = sorted.last().copied().unwrap_or(0.0);

    Ok(Report::StoreRecover(StoreRecoverReport {
        db_path: args.db_path.display().to_string(),
        iterations: args.iterations,
        recovered_descriptors: last_descriptors,
        recovered_chain_entries: last_chain_entries,
        recovered_pieces: last_pieces,
        durable_seq: last_durable_seq,
        iteration_ms: times_ms,
        mean_ms,
        p50_ms,
        p95_ms,
        max_ms,
    }))
}

/// Linear-interpolation percentile over a pre-sorted slice. With our
/// expected `iterations ∈ [1, 50]`, HDR-histogram would be overkill;
/// a sorted-slice percentile gives exact rather than bucketed answers.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{CommonWorkloadArgs, Mode, Scenario, StoreWriteArgs};
    use crate::scenarios::store_write;
    use tempfile::TempDir;

    #[test]
    fn percentile_basic() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&v, 0.0), 1.0);
        assert_eq!(percentile(&v, 1.0), 5.0);
        assert_eq!(percentile(&v, 0.5), 3.0);
    }

    #[test]
    fn missing_db_path_errors() {
        let r = run(StoreRecoverArgs {
            db_path: "/nonexistent/path/abcdef".into(),
            iterations: 1,
        });
        assert!(r.is_err());
    }

    #[test]
    fn zero_iterations_errors() {
        let dir = TempDir::new().unwrap();
        let r = run(StoreRecoverArgs {
            db_path: dir.path().to_path_buf(),
            iterations: 0,
        });
        assert!(r.is_err());
    }

    #[test]
    fn recover_after_write_returns_recovered_state_counts() {
        // 1) Write some state to a fresh DB.
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        let r = store_write::run(StoreWriteArgs {
            common: CommonWorkloadArgs {
                threads: 2,
                ops: 50,
                wallets: 5,
                scenario: Scenario::Partitioned,
                mode: Mode::Rocksdb,
                db_path: Some(path.clone()),
                await_durable: true,
                seed: 7,
            },
            prelude_descriptors: true,
        });
        assert!(r.is_ok(), "store-write prelude failed: {:?}", r.err());

        // 2) Now run store-recover against the same path.
        let r = run(StoreRecoverArgs {
            db_path: path,
            iterations: 2,
        })
        .expect("recover");
        match r {
            Report::StoreRecover(s) => {
                assert_eq!(s.iterations, 2);
                assert_eq!(s.iteration_ms.len(), 2);
                assert_eq!(s.recovered_descriptors, 5);
                assert_eq!(s.recovered_chain_entries, 50);
                assert!(s.durable_seq > 0);
                assert!(s.mean_ms > 0.0);
            }
            _ => panic!("wrong report variant"),
        }
    }
}
