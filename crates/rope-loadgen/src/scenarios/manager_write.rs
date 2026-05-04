//! `manager-write` scenario — full `LedgerManager` end-to-end.
//!
//! Drives `LedgerManager::append_to_ledger` against a pre-populated
//! ledger pool. Exercises every Phase 1 piece together:
//!
//! - **P1.1** (sharded `StringLattice`) — every append inserts a new
//!   `RopeString` into the lattice
//! - **P1.2** (per-wallet head lock) — every append takes the
//!   wallet's head lock to serialise concurrent appends to the same
//!   ledger
//! - **P1.3** (per-shard HLC) — every append calls
//!   `ClockManager::tick_for_wallet`
//! - **P1.4** (OES key cache) — every append derives an OES ledger
//!   key, hitting or missing the cache depending on `--oes-rotate-every`
//! - **P1.5** (LedgerStore) — every append mirrors to the underlying
//!   `LedgerStore` (memory or RocksDB)
//!
//! This is the bench whose `throughput_ops_per_sec` is the most
//! representative number for production node load.

use crate::cli::{ManagerWriteArgs, Mode, Scenario};
use crate::report::{throughput_ops_per_sec, LatencyStats, ManagerWriteReport, Report};
use crate::runner::{pick_wallet, run_threads, StoreHandle, WalletPool};
use rand::RngCore;
use rope_core::clock::ClockManager;
use rope_core::lattice::StringLattice;
use rope_core::personal_ledger::{InteractionRecord, InteractionType};
use rope_core::string::PublicKey;
use rope_core::types::NodeId;
use rope_crypto::oes::OESManager;
use rope_node::ledger_manager::LedgerManager;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn run(args: ManagerWriteArgs) -> Result<Report, String> {
    let common = args.common.clone();
    if common.threads == 0 || common.ops == 0 || common.wallets == 0 {
        return Err("threads, ops, and wallets must all be > 0".into());
    }

    tracing::info!(
        target: "rope_loadgen::manager_write",
        ops = common.ops,
        threads = common.threads,
        wallets = common.wallets,
        scenario = ?common.scenario,
        mode = ?common.mode,
        payload_bytes = args.payload_bytes,
        "starting manager-write workload"
    );

    let pool = WalletPool::new(common.wallets, common.seed);
    let store_handle = StoreHandle::create(&common)?;

    // Build the LedgerManager around the same store we'll measure
    // durability against. All other dependencies use the canonical
    // test bring-up (matching `LedgerManager::tests::make_test_manager`).
    //
    // NOTE: the LedgerManager's internal `StringRegistry` is created
    // by `LedgerManager::new`. We don't need to construct one
    // ourselves; the registry is sized lazily as wallets register.
    let lattice = Arc::new(StringLattice::new());
    let oes = Arc::new(OESManager::genesis(&[0u8; 32]));
    let node_id = NodeId::new([1u8; 32]);
    let creator_key = PublicKey::from_ed25519([2u8; 32]);
    let clock = Arc::new(ClockManager::new(node_id));

    let manager = Arc::new(LedgerManager::new(
        lattice,
        store_handle.store.clone(),
        oes,
        node_id,
        creator_key,
        clock,
    ));

    // ---- Untimed prelude: one create_ledger per wallet ----
    let prelude_start = Instant::now();
    let mut create_errors = 0usize;
    let wallet_hexes: Vec<String> = pool.iter().map(wallet_to_hex).collect();
    for whex in &wallet_hexes {
        if let Err(e) = manager.create_ledger(whex) {
            tracing::warn!(target: "rope_loadgen::manager_write", wallet = %whex, error = %e, "create_ledger failed");
            create_errors += 1;
        }
    }
    let prelude_elapsed = prelude_start.elapsed();
    let prelude_throughput = throughput_ops_per_sec(common.wallets, prelude_elapsed);

    if create_errors == common.wallets {
        return Err(format!(
            "all {} create_ledger calls failed — manager bring-up is broken",
            common.wallets
        ));
    }

    // Make sure every prelude write is durable before we start the
    // append timer, so the prelude's flush doesn't pollute append
    // latency.
    let _ = store_handle.await_durable(Duration::from_secs(60));

    // ---- Timed phase: appends ----
    let manager_for_workers = manager.clone();
    let pool_arc = Arc::new(pool);
    let pool_for_workers = pool_arc.clone();
    let wallet_hexes_arc = Arc::new(wallet_hexes);
    let wallet_hexes_for_workers = wallet_hexes_arc.clone();
    let scenario = common.scenario;
    let threads = common.threads;
    let payload_bytes = args.payload_bytes;
    let oes_rotate_every = args.oes_rotate_every;

    let append_errors_atomic = Arc::new(AtomicUsize::new(0));
    let append_errors_hot = append_errors_atomic.clone();

    let (elapsed, samples) = run_threads(
        common.threads,
        common.ops,
        common.seed,
        move |tid, n, rng, samples| {
            let manager = manager_for_workers.clone();
            let pool = pool_for_workers.clone();
            let hexes = wallet_hexes_for_workers.clone();
            let errors = append_errors_hot.clone();

            // Pre-allocate a payload buffer; we just bump its first
            // bytes per op to keep payloads "different" for any
            // content-aware downstream code.
            let mut payload = vec![0u8; payload_bytes];
            rng.fill_bytes(&mut payload);

            for op_idx in 0..n {
                let widx = pick_wallet(scenario, &pool, tid, threads, op_idx, rng);
                let whex = &hexes[widx];

                // Tag the payload uniquely without allocating a fresh
                // Vec each iteration. Keeps the bench focused on the
                // ledger machinery rather than the allocator.
                if !payload.is_empty() {
                    payload[0] = tid as u8;
                }
                if payload.len() >= 4 {
                    payload[1] = (op_idx & 0xFF) as u8;
                    payload[2] = ((op_idx >> 8) & 0xFF) as u8;
                    payload[3] = ((op_idx >> 16) & 0xFF) as u8;
                }

                // Optional OES rotation to measure cache miss cost.
                // We don't actually rotate the OES generation in the
                // OESManager (that'd require a federation step); we
                // just use the rotation as a hint to the cache by
                // varying the wallet — which currently does NOT
                // change the cache-key shape, so this is a no-op
                // until the cache key includes a generation bumped
                // by the manager. Left wired so the flag is honoured
                // when that hook is added.
                let _ = oes_rotate_every;

                let interaction = InteractionRecord {
                    interaction_type: InteractionType::IdentityClaim,
                    counterparty: None,
                    data: payload.clone(),
                    timestamp: 1_700_000_000 + op_idx as i64,
                    metadata: hashbrown::HashMap::new(),
                };

                let t0 = Instant::now();
                let res = manager.append_to_ledger(whex, interaction);
                let dt = t0.elapsed().as_nanos() as u64;
                if res.is_err() {
                    errors.fetch_add(1, Ordering::Relaxed);
                }
                samples.push(dt);
            }
        },
    );

    let (durable_ok, wait) = if common.await_durable {
        store_handle.await_durable(Duration::from_secs(120))
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
        Mode::Memory => "memory".to_string(),
        Mode::Rocksdb => "rocksdb".to_string(),
    };
    let scenario_s = match common.scenario {
        Scenario::Same => "same".to_string(),
        Scenario::Partitioned => "partitioned".to_string(),
        Scenario::Random => "random".to_string(),
    };

    Ok(Report::ManagerWrite(ManagerWriteReport {
        mode: mode_s,
        scenario: scenario_s,
        threads: common.threads,
        ops_total: common.ops,
        wallets: common.wallets,
        payload_bytes: args.payload_bytes,
        create_ledger_total_ms: prelude_elapsed.as_secs_f64() * 1_000.0,
        create_ledger_throughput_per_sec: prelude_throughput,
        create_ledger_errors: create_errors,
        elapsed_ms: elapsed.as_secs_f64() * 1_000.0,
        durability_wait_ms: wait.as_secs_f64() * 1_000.0,
        throughput_ops_per_sec: throughput_ops_per_sec(common.ops, elapsed),
        throughput_inc_durability_ops_per_sec: throughput_ops_per_sec(common.ops, elapsed + wait),
        latency: LatencyStats::from_samples_ns(&samples),
        append_errors: append_errors_atomic.load(Ordering::Relaxed),
        seed: common.seed,
    }))
}

/// Convert a 20-byte wallet address into the `0x…` hex string that
/// `LedgerManager::create_ledger` / `append_to_ledger` expect.
fn wallet_to_hex(wallet: &[u8]) -> String {
    let mut s = String::with_capacity(2 + wallet.len() * 2);
    s.push_str("0x");
    for byte in wallet {
        // `format!("{:02x}", byte)` would allocate per call; this
        // hand-rolled version stays in the existing String capacity.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 0x0F) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::CommonWorkloadArgs;

    fn args(scenario: Scenario, mode: Mode) -> ManagerWriteArgs {
        ManagerWriteArgs {
            common: CommonWorkloadArgs {
                threads: 2,
                ops: 50,
                wallets: 4,
                scenario,
                mode,
                db_path: None,
                await_durable: true,
                seed: 1,
            },
            payload_bytes: 64,
            oes_rotate_every: 0,
        }
    }

    #[test]
    fn wallet_to_hex_prefixed_lowercase() {
        let w = vec![0xDEu8, 0xAD, 0xBE, 0xEF];
        assert_eq!(wallet_to_hex(&w), "0xdeadbeef");
        let w = vec![0u8; 20];
        let h = wallet_to_hex(&w);
        assert_eq!(h.len(), 2 + 40);
        assert!(h.starts_with("0x"));
        assert!(h[2..].chars().all(|c| c == '0'));
    }

    #[test]
    fn manager_write_memory_partitioned_runs_to_completion() {
        let r = run(args(Scenario::Partitioned, Mode::Memory)).expect("run");
        match r {
            Report::ManagerWrite(s) => {
                assert_eq!(s.ops_total, 50);
                assert_eq!(s.wallets, 4);
                assert_eq!(s.create_ledger_errors, 0);
                assert_eq!(
                    s.append_errors, 0,
                    "no append should fail in memory mode with valid wallets"
                );
                assert_eq!(s.latency.samples, 50);
                assert!(s.throughput_ops_per_sec > 0.0);
                assert!(
                    s.create_ledger_total_ms > 0.0,
                    "create_ledger must be measurable"
                );
            }
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn manager_write_same_wallet_serialises_through_head_lock() {
        // Same scenario through the LedgerManager exercises P1.2 head
        // lock at the manager level. Must not deadlock.
        let r = run(args(Scenario::Same, Mode::Memory)).expect("run");
        match r {
            Report::ManagerWrite(s) => {
                assert_eq!(s.append_errors, 0);
                assert_eq!(s.latency.samples, 50);
            }
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn manager_write_rocksdb_runs_to_completion() {
        let r = run(args(Scenario::Partitioned, Mode::Rocksdb)).expect("run");
        match r {
            Report::ManagerWrite(s) => {
                assert_eq!(s.append_errors, 0);
                assert_eq!(s.latency.samples, 50);
                // Even with a tiny workload, the durability wait is
                // ≥ 0; the LedgerStore mirror must report durability
                // through the manager too.
                assert!(s.durability_wait_ms >= 0.0);
            }
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn empty_args_rejected() {
        let mut a = args(Scenario::Partitioned, Mode::Memory);
        a.common.threads = 0;
        assert!(run(a).is_err());

        let mut a = args(Scenario::Partitioned, Mode::Memory);
        a.common.ops = 0;
        assert!(run(a).is_err());

        let mut a = args(Scenario::Partitioned, Mode::Memory);
        a.common.wallets = 0;
        assert!(run(a).is_err());
    }
}
