//! End-to-end demonstration that the cluster routes real
//! `LedgerStore` operations to the correct node.
//!
//! Two in-memory `LedgerStore`s play the role of two physical
//! nodes. The cluster is configured with a round-robin partition
//! map across the two node ids. We then issue 256 appends — one
//! per shard — and verify:
//!
//!   1. Each store contains exactly the wallets whose shard rounds
//!      to that node (n1 owns shards 0, 2, 4, …; n2 owns 1, 3, 5,
//!      …).
//!   2. Reads via the cluster client return the same chain that
//!      direct reads against the owning store would.
//!   3. Per-shard counters in `ClusterClient` reflect the dispatch
//!      pattern.
//!
//! This is intentionally synchronous from the test's point of view
//! — the cluster API is `async`, but in the in-memory case every
//! call resolves on the spot. Production deployments will use a
//! real network transport that will surface actual `await` points.

use rope_cluster::{
    ClusterClient, ClusterMembership, LocalShardEndpoint, NodeDescriptor, PartitionMap, ShardId,
    ShardOp, ShardOpKind, NUM_SHARDS,
};
use rope_core::types::NodeId;
use rope_storage::LedgerStore;
use std::sync::Arc;

/// Payload encoding for the test ops. `kind` is set by the caller;
/// these structs are the bytes inside `ShardOp.payload`.
mod proto {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    pub struct AppendOp {
        pub wallet: Vec<u8>,
        pub string_id: [u8; 32],
    }

    #[derive(Serialize, Deserialize)]
    pub struct GetChainOp {
        pub wallet: Vec<u8>,
    }

    #[derive(Serialize, Deserialize)]
    pub struct ChainResult {
        pub ids: Vec<[u8; 32]>,
    }
}

/// Build a `LocalShardEndpoint` that decodes the test ops and runs
/// them against a `LedgerStore`. One per node.
fn ledger_endpoint(store: Arc<LedgerStore>) -> Arc<LocalShardEndpoint> {
    let h: rope_cluster::endpoint::LocalHandler = Arc::new(move |op| {
        match op.kind {
            ShardOpKind::AppendToLedger => {
                let payload: proto::AppendOp = bincode::deserialize(&op.payload)
                    .map_err(|e| format!("decode AppendOp: {e}"))?;
                store.append_to_chain(&payload.wallet, payload.string_id);
                Ok(rope_cluster::ShardResult::empty())
            }
            ShardOpKind::GetChain => {
                let payload: proto::GetChainOp = bincode::deserialize(&op.payload)
                    .map_err(|e| format!("decode GetChainOp: {e}"))?;
                let ids = store.get_chain(&payload.wallet);
                let result = proto::ChainResult { ids };
                let bytes =
                    bincode::serialize(&result).map_err(|e| format!("encode ChainResult: {e}"))?;
                Ok(rope_cluster::ShardResult::new(bytes))
            }
            other => Err(format!("test endpoint does not handle {other:?}")),
        }
    });
    LocalShardEndpoint::new(h)
}

#[tokio::test]
async fn two_node_cluster_routes_appends_to_correct_owner() {
    // Two in-memory ledger stores, one per "node".
    let store_a = Arc::new(LedgerStore::new());
    let store_b = Arc::new(LedgerStore::new());

    let n_a = NodeId::new([0xAA; 32]);
    let n_b = NodeId::new([0xBB; 32]);
    let mem = ClusterMembership::from_nodes(vec![
        NodeDescriptor::new(n_a, "node-a"),
        NodeDescriptor::new(n_b, "node-b"),
    ]);

    // Round-robin so even shards → first node *in the membership
    // ID-sorted order*; we use the same node order to keep the
    // mental model trivial.
    let pm = PartitionMap::round_robin(&[n_a, n_b]);
    let client = ClusterClient::new(mem, pm);
    client.register_endpoint(n_a, ledger_endpoint(store_a.clone()));
    client.register_endpoint(n_b, ledger_endpoint(store_b.clone()));

    // Issue one append per shard. Wallet bytes encode the shard
    // explicitly so we can audit routing later.
    let mut wallets: Vec<Vec<u8>> = Vec::with_capacity(NUM_SHARDS);
    for s in 0..NUM_SHARDS {
        let mut w = vec![0u8; 20];
        w[0] = s as u8; // first byte = shard id
        wallets.push(w);
    }

    for w in &wallets {
        let mut sid = [0u8; 32];
        sid[0..20].copy_from_slice(w);
        let payload = bincode::serialize(&proto::AppendOp {
            wallet: w.clone(),
            string_id: sid,
        })
        .unwrap();
        let r = client
            .dispatch(ShardOp::new(w.clone(), ShardOpKind::AppendToLedger, payload))
            .await
            .expect("append must succeed");
        assert!(r.payload.is_empty());
    }

    // Direct inspection of each store: a wallet should land in
    // exactly one store, the one its shard routes to.
    for w in &wallets {
        let s = w[0] as usize;
        let owner = if s % 2 == 0 { n_a } else { n_b };
        let expected_store = if owner == n_a { &store_a } else { &store_b };
        let other_store = if owner == n_a { &store_b } else { &store_a };

        let chain = expected_store.get_chain(w);
        assert_eq!(
            chain.len(),
            1,
            "wallet shard {s} should have 1 string in its owning store, got {}",
            chain.len()
        );
        assert!(
            other_store.get_chain(w).is_empty(),
            "wallet shard {s} must NOT appear in the other node's store"
        );
    }

    // Read back through the cluster — same answer.
    for w in &wallets {
        let payload =
            bincode::serialize(&proto::GetChainOp { wallet: w.clone() }).unwrap();
        let r = client
            .dispatch(ShardOp::new(w.clone(), ShardOpKind::GetChain, payload))
            .await
            .expect("get_chain must succeed");
        let result: proto::ChainResult = bincode::deserialize(&r.payload).unwrap();
        assert_eq!(result.ids.len(), 1, "every wallet has exactly 1 entry");
    }

    // Per-shard counters: each shard processed exactly 2 ops
    // (1 append + 1 get_chain).
    for s in ShardId::all() {
        assert_eq!(
            client.shard_op_count(s),
            2,
            "shard {s:?} should have processed exactly 2 ops"
        );
    }
    // 256 shards × 2 ops = 512 dispatches, all to LOCAL endpoints.
    assert_eq!(client.local_dispatch_count(), 512);
    assert_eq!(client.remote_dispatch_count(), 0);
}

#[tokio::test]
async fn cluster_balances_load_evenly_across_two_nodes() {
    // Sanity: across many shards the per-node load distribution
    // must be balanced — this is the whole point of P2.D.
    let store_a = Arc::new(LedgerStore::new());
    let store_b = Arc::new(LedgerStore::new());
    let n_a = NodeId::new([0xAA; 32]);
    let n_b = NodeId::new([0xBB; 32]);
    let mem = ClusterMembership::from_nodes(vec![
        NodeDescriptor::new(n_a, "a"),
        NodeDescriptor::new(n_b, "b"),
    ]);
    let pm = PartitionMap::round_robin(&[n_a, n_b]);
    let client = ClusterClient::new(mem, pm);
    client.register_endpoint(n_a, ledger_endpoint(store_a.clone()));
    client.register_endpoint(n_b, ledger_endpoint(store_b.clone()));

    // 1024 random-looking wallets (BLAKE3 of an index).
    for i in 0..1024u32 {
        let h = blake3::hash(&i.to_le_bytes());
        let mut w = vec![0u8; 20];
        w.copy_from_slice(&h.as_bytes()[..20]);
        let mut sid = [0u8; 32];
        sid[..4].copy_from_slice(&i.to_le_bytes());
        let payload = bincode::serialize(&proto::AppendOp {
            wallet: w.clone(),
            string_id: sid,
        })
        .unwrap();
        client
            .dispatch(ShardOp::new(w, ShardOpKind::AppendToLedger, payload))
            .await
            .unwrap();
    }

    // Tally how many appends each store actually received via its
    // mirror's wallet count. The shard distribution is uniform so
    // each node should land in [40%, 60%] of total.
    let count_a: usize = (0..1024)
        .filter(|&i| {
            let h = blake3::hash(&(i as u32).to_le_bytes());
            !store_a.get_chain(&h.as_bytes()[..20]).is_empty()
        })
        .count();
    let count_b: usize = (0..1024)
        .filter(|&i| {
            let h = blake3::hash(&(i as u32).to_le_bytes());
            !store_b.get_chain(&h.as_bytes()[..20]).is_empty()
        })
        .count();

    assert_eq!(count_a + count_b, 1024);
    assert!(
        (410..=614).contains(&count_a),
        "node A should hold ~512 wallets, got {count_a}"
    );
    assert!(
        (410..=614).contains(&count_b),
        "node B should hold ~512 wallets, got {count_b}"
    );
}

#[tokio::test]
async fn cluster_handles_endpoint_failure_without_corrupting_neighbours() {
    // If endpoint A returns an error, that error surfaces to the
    // caller cleanly; endpoint B's state is unaffected.
    let store_b = Arc::new(LedgerStore::new());
    let n_a = NodeId::new([0xAA; 32]);
    let n_b = NodeId::new([0xBB; 32]);
    let mem = ClusterMembership::from_nodes(vec![
        NodeDescriptor::new(n_a, "a"),
        NodeDescriptor::new(n_b, "b"),
    ]);
    let pm = PartitionMap::round_robin(&[n_a, n_b]);
    let client = ClusterClient::new(mem, pm);

    // Endpoint A always fails.
    let h: rope_cluster::endpoint::LocalHandler =
        Arc::new(|_op| Err("simulated node-A outage".to_string()));
    client.register_endpoint(n_a, LocalShardEndpoint::new(h));
    client.register_endpoint(n_b, ledger_endpoint(store_b.clone()));

    // Even-shard wallet → routes to A → must error.
    let mut w = vec![0u8; 20];
    w[0] = 0; // shard 0 → A
    let payload = bincode::serialize(&proto::AppendOp {
        wallet: w.clone(),
        string_id: [0u8; 32],
    })
    .unwrap();
    let r = client
        .dispatch(ShardOp::new(w, ShardOpKind::AppendToLedger, payload))
        .await;
    assert!(r.is_err(), "expected endpoint failure to propagate");

    // Odd-shard wallet → routes to B → succeeds, B's store gets the
    // append. A's failure did not leak.
    let mut w = vec![0u8; 20];
    w[0] = 1; // shard 1 → B
    let mut sid = [0u8; 32];
    sid[0] = 1;
    let payload = bincode::serialize(&proto::AppendOp {
        wallet: w.clone(),
        string_id: sid,
    })
    .unwrap();
    let r = client
        .dispatch(ShardOp::new(w.clone(), ShardOpKind::AppendToLedger, payload))
        .await;
    assert!(r.is_ok(), "B-bound op should succeed despite A's outage");
    assert_eq!(store_b.get_chain(&w).len(), 1);
}
