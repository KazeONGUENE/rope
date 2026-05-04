//! [`ClusterClient`] — the dispatch API every caller uses.
//!
//! Given a [`crate::op::ShardOp`], the client:
//!
//!   1. Computes the target shard via
//!      [`crate::partition::ShardId::for_wallet`].
//!   2. Looks up the owning node in the active
//!      [`crate::partition::PartitionMap`].
//!   3. Looks up the [`crate::endpoint::ShardEndpoint`] registered
//!      for that node id.
//!   4. Forwards the op via [`crate::endpoint::ShardEndpoint::execute`]
//!      and returns the result to the caller.
//!
//! Steps 1–3 are O(1). Step 4's cost depends on the endpoint kind:
//! local endpoints execute in-process; remote endpoints incur the
//! transport's cost (negligible in the in-memory test harness, real
//! over a production network).
//!
//! Topology changes — adding/removing nodes, swapping a
//! [`PartitionMap`] — are handled by replacing the
//! [`ClusterClient`]'s internal `Arc<PartitionMap>` atomically. All
//! in-flight ops finish against the previous map; new ops see the
//! new map. There is no half-applied state.

use crate::endpoint::ShardEndpoint;
use crate::error::{ClusterError, ClusterResult};
use crate::membership::ClusterMembership;
use crate::op::{ShardOp, ShardResult};
use crate::partition::{PartitionMap, ShardId};
use parking_lot::RwLock;
use rope_core::types::NodeId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Cluster dispatch client. Cheap to clone (one `Arc<Inner>`).
#[derive(Clone)]
pub struct ClusterClient {
    inner: Arc<Inner>,
}

struct Inner {
    membership: RwLock<ClusterMembership>,
    partitions: RwLock<Arc<PartitionMap>>,
    endpoints: RwLock<HashMap<NodeId, Arc<dyn ShardEndpoint>>>,
    /// Counters per shard for local observability. Indexed by
    /// `ShardId.0`. Always exactly [`crate::partition::NUM_SHARDS`]
    /// long; not serialised, so a `Vec` here is fine.
    shard_op_counters: Vec<AtomicU64>,
    /// Counters per kind: hits to a local endpoint vs hits to a
    /// remote endpoint. Useful for capacity planning.
    local_dispatch_count: AtomicU64,
    remote_dispatch_count: AtomicU64,
}

impl ClusterClient {
    pub fn new(membership: ClusterMembership, partitions: PartitionMap) -> Self {
        let counters: Vec<AtomicU64> = (0..crate::partition::NUM_SHARDS)
            .map(|_| AtomicU64::new(0))
            .collect();
        Self {
            inner: Arc::new(Inner {
                membership: RwLock::new(membership),
                partitions: RwLock::new(Arc::new(partitions)),
                endpoints: RwLock::new(HashMap::new()),
                shard_op_counters: counters,
                local_dispatch_count: AtomicU64::new(0),
                remote_dispatch_count: AtomicU64::new(0),
            }),
        }
    }

    /// Register the endpoint that handles ops for `node`. Replaces
    /// any existing registration.
    pub fn register_endpoint(&self, node: NodeId, endpoint: Arc<dyn ShardEndpoint>) {
        self.inner.endpoints.write().insert(node, endpoint);
    }

    /// Remove the endpoint for `node`. Subsequent ops routed to that
    /// node will fail with [`ClusterError::EndpointNotFound`].
    pub fn unregister_endpoint(&self, node: &NodeId) {
        self.inner.endpoints.write().remove(node);
    }

    /// Atomically swap the partition map. In-flight ops finish
    /// against the old map.
    pub fn swap_partitions(&self, partitions: PartitionMap) {
        *self.inner.partitions.write() = Arc::new(partitions);
    }

    /// Atomically swap the membership snapshot.
    pub fn swap_membership(&self, membership: ClusterMembership) {
        *self.inner.membership.write() = membership;
    }

    /// Read-side: a snapshot of the active partition map.
    pub fn partitions(&self) -> Arc<PartitionMap> {
        self.inner.partitions.read().clone()
    }

    /// Read-side: a snapshot of the active membership.
    pub fn membership(&self) -> ClusterMembership {
        self.inner.membership.read().clone()
    }

    /// How many ops have been dispatched against shard `s` since
    /// this client was created.
    pub fn shard_op_count(&self, s: ShardId) -> u64 {
        self.inner.shard_op_counters[s.as_usize()].load(Ordering::Relaxed)
    }

    /// Total ops dispatched to local endpoints.
    pub fn local_dispatch_count(&self) -> u64 {
        self.inner.local_dispatch_count.load(Ordering::Relaxed)
    }

    /// Total ops dispatched to remote endpoints.
    pub fn remote_dispatch_count(&self) -> u64 {
        self.inner.remote_dispatch_count.load(Ordering::Relaxed)
    }

    /// Resolve the owning node for a wallet without dispatching.
    /// Useful for callers that want to build batch sends grouped by
    /// owner.
    pub fn owner_for_wallet(&self, wallet: &[u8]) -> NodeId {
        self.inner.partitions.read().owner_for_wallet(wallet)
    }

    /// Group a batch of wallets by owning node. The returned map's
    /// keys are node ids; the values are the indices into `wallets`.
    /// Useful for the future cross-shard batch coordinator.
    pub fn group_wallets_by_owner(&self, wallets: &[Vec<u8>]) -> HashMap<NodeId, Vec<usize>> {
        let pm = self.inner.partitions.read().clone();
        let mut groups: HashMap<NodeId, Vec<usize>> = HashMap::new();
        for (i, w) in wallets.iter().enumerate() {
            groups.entry(pm.owner_for_wallet(w)).or_default().push(i);
        }
        groups
    }

    /// Dispatch one op. Steps:
    ///
    ///   1. Validate the op carries a wallet (routing key).
    ///   2. Compute the target shard.
    ///   3. Look up the owning node and its endpoint.
    ///   4. Bump per-shard / per-kind counters.
    ///   5. Forward to the endpoint and return its result.
    pub async fn dispatch(&self, op: ShardOp) -> ClusterResult<ShardResult> {
        if op.wallet.is_empty() {
            return Err(ClusterError::UnroutableOp);
        }

        let shard = ShardId::for_wallet(&op.wallet);
        let owner = self.inner.partitions.read().owner(shard);

        // Validate the owner is still in the membership; mismatched
        // assignments surface as a clear error rather than silent
        // dispatch to a missing endpoint.
        if self.inner.membership.read().lookup(&owner).is_none() {
            return Err(ClusterError::OwnerNotInMembership { shard, node: owner });
        }

        let endpoint = self
            .inner
            .endpoints
            .read()
            .get(&owner)
            .cloned()
            .ok_or(ClusterError::EndpointNotFound { node: owner })?;

        // Counters BEFORE dispatch so even failed ops register —
        // they are still load on the cluster.
        self.inner.shard_op_counters[shard.as_usize()].fetch_add(1, Ordering::Relaxed);
        match endpoint.kind() {
            crate::endpoint::ShardEndpointKind::Local => {
                self.inner.local_dispatch_count.fetch_add(1, Ordering::Relaxed);
            }
            crate::endpoint::ShardEndpointKind::Remote => {
                self.inner.remote_dispatch_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        endpoint.execute(op).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{InMemoryRemoteEndpoint, LocalShardEndpoint};
    use crate::membership::NodeDescriptor;
    use crate::op::ShardOpKind;

    fn make_two_node_cluster() -> (ClusterClient, NodeId, NodeId) {
        let n1 = NodeId::new([1u8; 32]);
        let n2 = NodeId::new([2u8; 32]);
        let mem = ClusterMembership::from_nodes(vec![
            NodeDescriptor::new(n1, "addr1"),
            NodeDescriptor::new(n2, "addr2"),
        ]);
        let pm = PartitionMap::round_robin(&[n1, n2]);
        (ClusterClient::new(mem, pm), n1, n2)
    }

    #[tokio::test]
    async fn dispatch_routes_to_correct_owner() {
        let (client, n1, n2) = make_two_node_cluster();

        // Two distinct local endpoints, one per node.
        let ep1 = LocalShardEndpoint::echo(b"node1".to_vec());
        let ep2 = LocalShardEndpoint::echo(b"node2".to_vec());
        client.register_endpoint(n1, ep1.clone());
        client.register_endpoint(n2, ep2.clone());

        // Wallet 0 → shard 0 → round-robin → node 0 (n1).
        let r = client
            .dispatch(ShardOp::new(
                vec![0u8; 20],
                ShardOpKind::AppendToLedger,
                vec![],
            ))
            .await
            .unwrap();
        assert_eq!(r.payload, b"node1");
        assert_eq!(ep1.ops_executed(), 1);
        assert_eq!(ep2.ops_executed(), 0);

        // Wallet whose first byte is 1 → shard 1 → round-robin → n2.
        let mut w = vec![0u8; 20];
        w[0] = 1;
        let r = client
            .dispatch(ShardOp::new(w, ShardOpKind::AppendToLedger, vec![]))
            .await
            .unwrap();
        assert_eq!(r.payload, b"node2");
        assert_eq!(ep1.ops_executed(), 1);
        assert_eq!(ep2.ops_executed(), 1);
    }

    #[tokio::test]
    async fn dispatch_to_remote_endpoint_works_end_to_end() {
        // Same as above, but n2's endpoint is wrapped in an
        // InMemoryRemoteEndpoint so we exercise the "remote" path.
        let (client, n1, n2) = make_two_node_cluster();
        let local_n2 = LocalShardEndpoint::echo(b"via-remote".to_vec());
        let remote = InMemoryRemoteEndpoint::new(local_n2.clone());
        client.register_endpoint(n1, LocalShardEndpoint::echo(b"node1".to_vec()));
        client.register_endpoint(n2, remote.clone());

        let mut w = vec![0u8; 20];
        w[0] = 1; // → shard 1 → n2
        let r = client
            .dispatch(ShardOp::new(w, ShardOpKind::AppendToLedger, vec![]))
            .await
            .unwrap();
        assert_eq!(r.payload, b"via-remote");
        assert_eq!(remote.ops_executed(), 1);
        assert_eq!(local_n2.ops_executed(), 1);
        assert_eq!(client.remote_dispatch_count(), 1);
        // Only one dispatch happened (the one to n2 via the remote
        // endpoint). n1's local endpoint was registered but never
        // hit, so local_dispatch_count is 0.
        assert_eq!(client.local_dispatch_count(), 0);
    }

    #[tokio::test]
    async fn dispatch_errors_on_missing_endpoint() {
        let (client, n1, _n2) = make_two_node_cluster();
        client.register_endpoint(n1, LocalShardEndpoint::echo(b"a".to_vec()));
        // n2 has no endpoint — dispatching a wallet that owns under
        // n2 must fail with EndpointNotFound, not panic.
        let mut w = vec![0u8; 20];
        w[0] = 1;
        let r = client
            .dispatch(ShardOp::new(w, ShardOpKind::AppendToLedger, vec![]))
            .await;
        assert!(matches!(r, Err(ClusterError::EndpointNotFound { .. })));
    }

    #[tokio::test]
    async fn dispatch_errors_on_empty_wallet() {
        let (client, n1, _n2) = make_two_node_cluster();
        client.register_endpoint(n1, LocalShardEndpoint::echo(b"x".to_vec()));
        let r = client
            .dispatch(ShardOp::new(vec![], ShardOpKind::AppendToLedger, vec![]))
            .await;
        assert!(matches!(r, Err(ClusterError::UnroutableOp)));
    }

    #[tokio::test]
    async fn swap_partitions_redirects_subsequent_ops() {
        let (client, n1, n2) = make_two_node_cluster();
        let ep1 = LocalShardEndpoint::echo(b"to-n1".to_vec());
        let ep2 = LocalShardEndpoint::echo(b"to-n2".to_vec());
        client.register_endpoint(n1, ep1.clone());
        client.register_endpoint(n2, ep2.clone());

        // Wallet 0 → shard 0 → n1 under round-robin.
        let r = client
            .dispatch(ShardOp::new(
                vec![0u8; 20],
                ShardOpKind::AppendToLedger,
                vec![],
            ))
            .await
            .unwrap();
        assert_eq!(r.payload, b"to-n1");

        // Swap to a partition map that gives EVERYTHING to n2.
        let pm_all_n2 = PartitionMap::from_assignments(
            &(0..crate::partition::NUM_SHARDS)
                .map(|s| crate::partition::ShardOwnership {
                    shard: ShardId(s as u8),
                    owner: n2,
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        client.swap_partitions(pm_all_n2);

        // Same wallet now routes to n2.
        let r = client
            .dispatch(ShardOp::new(
                vec![0u8; 20],
                ShardOpKind::AppendToLedger,
                vec![],
            ))
            .await
            .unwrap();
        assert_eq!(r.payload, b"to-n2");
    }

    #[tokio::test]
    async fn group_wallets_by_owner_partitions_correctly() {
        let (client, n1, n2) = make_two_node_cluster();
        client.register_endpoint(n1, LocalShardEndpoint::echo(b"a".to_vec()));
        client.register_endpoint(n2, LocalShardEndpoint::echo(b"b".to_vec()));

        let mut wallets: Vec<Vec<u8>> = Vec::new();
        for i in 0u8..32 {
            let mut w = vec![0u8; 20];
            w[0] = i;
            wallets.push(w);
        }
        let groups = client.group_wallets_by_owner(&wallets);
        // Even shard ids (0, 2, …) → n1; odd → n2 under round-robin
        // with [n1, n2].
        let n1_indices: &Vec<usize> = groups.get(&n1).unwrap();
        let n2_indices: &Vec<usize> = groups.get(&n2).unwrap();
        assert_eq!(n1_indices.len(), 16);
        assert_eq!(n2_indices.len(), 16);
        for &i in n1_indices {
            assert!(i % 2 == 0);
        }
        for &i in n2_indices {
            assert!(i % 2 == 1);
        }
    }

    #[tokio::test]
    async fn shard_op_counters_track_dispatch() {
        let (client, n1, n2) = make_two_node_cluster();
        client.register_endpoint(n1, LocalShardEndpoint::echo(b"a".to_vec()));
        client.register_endpoint(n2, LocalShardEndpoint::echo(b"b".to_vec()));

        for i in 0u8..32 {
            let mut w = vec![0u8; 20];
            w[0] = i;
            let _ = client
                .dispatch(ShardOp::new(w, ShardOpKind::AppendToLedger, vec![]))
                .await
                .unwrap();
        }
        for i in 0u8..32 {
            assert_eq!(client.shard_op_count(ShardId(i)), 1);
        }
        // Untouched shards stay at 0.
        for i in 32u8..64 {
            assert_eq!(client.shard_op_count(ShardId(i)), 0);
        }
        assert_eq!(client.local_dispatch_count(), 32);
    }

    #[tokio::test]
    async fn owner_not_in_membership_is_a_clear_error() {
        // Build a partition map referencing a node that membership
        // doesn't know about. Subsequent dispatch must error
        // explicitly rather than dispatching into the void.
        let n1 = NodeId::new([1u8; 32]);
        let phantom = NodeId::new([99u8; 32]);
        let mem = ClusterMembership::from_nodes(vec![NodeDescriptor::new(n1, "a")]);
        // Hand-craft a map where shard 0 → phantom and the rest → n1.
        let mut assigns: Vec<crate::partition::ShardOwnership> = (0..crate::partition::NUM_SHARDS)
            .map(|s| crate::partition::ShardOwnership {
                shard: ShardId(s as u8),
                owner: n1,
            })
            .collect();
        assigns[0] = crate::partition::ShardOwnership {
            shard: ShardId(0),
            owner: phantom,
        };
        let pm = PartitionMap::from_assignments(&assigns).unwrap();
        let client = ClusterClient::new(mem, pm);
        client.register_endpoint(n1, LocalShardEndpoint::echo(b"x".to_vec()));

        let r = client
            .dispatch(ShardOp::new(
                vec![0u8; 20], // shard 0
                ShardOpKind::AppendToLedger,
                vec![],
            ))
            .await;
        assert!(matches!(r, Err(ClusterError::OwnerNotInMembership { .. })));
    }
}
