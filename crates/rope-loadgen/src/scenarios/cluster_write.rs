//! `cluster-write` — Quipu Canon v2.0 Phase 2.D scenario.
//!
//! Drives synthetic appends through a multi-node `rope-cluster`
//! topology of in-process nodes backed by `LedgerStore`. Each node
//! runs in the same process — there is no real network — but the
//! cluster routing, partition lookup, endpoint dispatch, and
//! per-shard accounting are all real.
//!
//! Reports per-node and aggregate throughput so callers can verify
//! that adding nodes scales throughput close to linearly. The
//! single-node (`--nodes 1`) baseline matches the existing
//! `manager-write` ceiling once everything is wired through the
//! cluster, which is the point: P2.D adds NO single-node cost,
//! only routing.

use crate::cli::ClusterWriteArgs;
use crate::report::{throughput_ops_per_sec, ClusterWriteReport, LatencyStats, Report};
use rand::SeedableRng;
use rand::{Rng, RngCore};
use rand_chacha::ChaCha20Rng;
use rope_cluster::{
    endpoint::LocalHandler, ClusterClient, ClusterMembership, LocalShardEndpoint, NodeDescriptor,
    PartitionMap, ShardEndpoint, ShardOp, ShardOpKind,
};
use rope_core::types::NodeId;
use rope_storage::LedgerStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

#[derive(Serialize, Deserialize)]
struct AppendOp {
    wallet: Vec<u8>,
    string_id: [u8; 32],
}

fn make_node(node_byte: u8) -> (NodeId, NodeDescriptor, Arc<LedgerStore>, Arc<LocalShardEndpoint>) {
    let id = NodeId::new([node_byte; 32]);
    let desc = NodeDescriptor::new(id, format!("inproc-{node_byte}"));
    let store = Arc::new(LedgerStore::new());
    let store_for_handler = store.clone();
    let h: LocalHandler = Arc::new(move |op| match op.kind {
        ShardOpKind::AppendToLedger => {
            let payload: AppendOp =
                bincode::deserialize(&op.payload).map_err(|e| format!("decode: {e}"))?;
            store_for_handler.append_to_chain(&payload.wallet, payload.string_id);
            Ok(rope_cluster::ShardResult::empty())
        }
        other => Err(format!("cluster-write expects AppendToLedger, got {other:?}")),
    });
    let ep = LocalShardEndpoint::new(h);
    (id, desc, store, ep)
}

pub fn run(args: ClusterWriteArgs) -> Result<Report, String> {
    if args.nodes == 0 {
        return Err("--nodes must be ≥ 1".to_string());
    }
    if args.tasks == 0 {
        return Err("--tasks must be ≥ 1".to_string());
    }
    if args.wallets == 0 {
        return Err("--wallets must be ≥ 1".to_string());
    }
    if args.ops == 0 {
        return Err("--ops must be ≥ 1".to_string());
    }

    // Spin up `args.nodes` in-process nodes.
    let mut node_ids = Vec::with_capacity(args.nodes);
    let mut node_descs = Vec::with_capacity(args.nodes);
    let mut node_stores: Vec<Arc<LedgerStore>> = Vec::with_capacity(args.nodes);
    let mut node_endpoints: Vec<Arc<LocalShardEndpoint>> = Vec::with_capacity(args.nodes);
    for i in 0..args.nodes {
        let (id, desc, store, ep) = make_node(i as u8);
        node_ids.push(id);
        node_descs.push(desc);
        node_stores.push(store);
        node_endpoints.push(ep);
    }

    let mem = ClusterMembership::from_nodes(node_descs.clone());
    let pm = PartitionMap::round_robin(&node_ids);
    let client = ClusterClient::new(mem, pm);
    for (id, ep) in node_ids.iter().zip(node_endpoints.iter()) {
        client.register_endpoint(*id, ep.clone());
    }

    // Pre-generate the wallet pool. Each wallet has a uniformly
    // distributed first byte so the round-robin partition map
    // balances cleanly across nodes.
    let mut prng = ChaCha20Rng::seed_from_u64(args.seed);
    let mut wallets: Vec<Vec<u8>> = Vec::with_capacity(args.wallets);
    for _ in 0..args.wallets {
        let mut w = vec![0u8; 20];
        prng.fill_bytes(&mut w);
        wallets.push(w);
    }
    let wallets = Arc::new(wallets);

    // Fixed-size tokio runtime for the dispatch tasks. Using
    // current-thread for tiny workloads can hide contention; we use
    // a multi-thread runtime with `args.tasks` workers so wall
    // clock truly reflects parallel dispatch.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(args.tasks.max(1))
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime build failed: {e}"))?;

    let ops_per_task = args.ops / args.tasks;
    let total_ops = ops_per_task * args.tasks;

    // Per-task latency vectors (collected in nanoseconds, aggregated
    // after the join — never touch hdrhistogram on the hot path).
    let started = Instant::now();
    let join_handles: Vec<_> = (0..args.tasks)
        .map(|task_idx| {
            let client = client.clone();
            let wallets = wallets.clone();
            let seed_for_task = args.seed.wrapping_add(task_idx as u64);
            rt.spawn(async move {
                let mut prng = ChaCha20Rng::seed_from_u64(seed_for_task);
                let mut latencies: Vec<u64> = Vec::with_capacity(ops_per_task);
                for _ in 0..ops_per_task {
                    let widx: usize = prng.gen_range(0..wallets.len());
                    let wallet = wallets[widx].clone();
                    let mut sid = [0u8; 32];
                    prng.fill_bytes(&mut sid);
                    let payload = bincode::serialize(&AppendOp {
                        wallet: wallet.clone(),
                        string_id: sid,
                    })
                    .expect("static encode");
                    let op = ShardOp::new(wallet, ShardOpKind::AppendToLedger, payload);
                    let t = Instant::now();
                    let _ = client.dispatch(op).await;
                    latencies.push(t.elapsed().as_nanos() as u64);
                }
                latencies
            })
        })
        .collect();

    // Join all tasks, collect per-task latency vectors.
    let mut all_latencies: Vec<u64> = Vec::with_capacity(total_ops);
    rt.block_on(async {
        for h in join_handles {
            let task_latencies = h.await.expect("task panicked");
            all_latencies.extend(task_latencies);
        }
    });
    let elapsed = started.elapsed();

    // Per-node op counts: read each LocalShardEndpoint's counter.
    let per_node_ops: Vec<u64> = node_endpoints
        .iter()
        .map(|ep| ep.ops_executed())
        .collect();
    let min_ops = *per_node_ops.iter().min().unwrap_or(&0);
    let max_ops = *per_node_ops.iter().max().unwrap_or(&0);

    let throughput = throughput_ops_per_sec(total_ops, elapsed);
    let latency = LatencyStats::from_samples_ns(&all_latencies);

    let report = ClusterWriteReport {
        nodes: args.nodes,
        tasks: args.tasks,
        ops_total: total_ops,
        wallets: args.wallets,
        elapsed_ms: elapsed.as_secs_f64() * 1_000.0,
        throughput_ops_per_sec: throughput,
        mean_per_op_us: latency.mean_us,
        per_node_ops,
        min_node_ops: min_ops,
        max_node_ops: max_ops,
        seed: args.seed,
        latency,
    };

    // Sanity: total ops dispatched across nodes must equal total_ops.
    let summed: u64 = report.per_node_ops.iter().sum();
    if summed as usize != total_ops {
        tracing::warn!(
            summed,
            total_ops,
            "per-node op counts do not sum to total — endpoint failures or counter race"
        );
    }

    Ok(Report::ClusterWrite(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(nodes: usize, tasks: usize, ops: usize, wallets: usize) -> ClusterWriteArgs {
        ClusterWriteArgs {
            nodes,
            ops,
            wallets,
            tasks,
            seed: 42,
        }
    }

    #[test]
    fn one_node_smoke() {
        let r = run(args(1, 2, 100, 16)).expect("ok");
        match r {
            Report::ClusterWrite(rep) => {
                assert_eq!(rep.ops_total, 100);
                assert_eq!(rep.nodes, 1);
                assert_eq!(rep.per_node_ops.len(), 1);
                assert_eq!(rep.per_node_ops[0], 100);
            }
            _ => panic!("expected cluster-write report"),
        }
    }

    #[test]
    fn two_nodes_balance_load() {
        let r = run(args(2, 4, 800, 64)).expect("ok");
        match r {
            Report::ClusterWrite(rep) => {
                assert_eq!(rep.ops_total, 800);
                assert_eq!(rep.per_node_ops.len(), 2);
                let total: u64 = rep.per_node_ops.iter().sum();
                assert_eq!(total, 800);
                // Allow generous slack for the small workload.
                assert!(
                    rep.min_node_ops as f64 / rep.max_node_ops as f64 >= 0.6,
                    "two-node load must be roughly balanced (min={}, max={})",
                    rep.min_node_ops,
                    rep.max_node_ops
                );
            }
            _ => panic!("expected cluster-write report"),
        }
    }

    #[test]
    fn four_nodes_balance_load() {
        let r = run(args(4, 8, 4000, 256)).expect("ok");
        match r {
            Report::ClusterWrite(rep) => {
                assert_eq!(rep.per_node_ops.len(), 4);
                let total: u64 = rep.per_node_ops.iter().sum();
                assert_eq!(total, 4000);
                // Each node should hold ~ops/nodes; allow ±50%.
                let target = (rep.ops_total / rep.nodes) as u64;
                for c in &rep.per_node_ops {
                    assert!(
                        (target / 2..=target * 3 / 2).contains(c),
                        "per-node count {c} out of expected ~{target} (±50%)"
                    );
                }
            }
            _ => panic!("expected cluster-write report"),
        }
    }

    #[test]
    fn rejects_zero_nodes() {
        assert!(run(args(0, 1, 1, 1)).is_err());
    }

    #[test]
    fn rejects_zero_tasks() {
        assert!(run(args(1, 0, 1, 1)).is_err());
    }
}
