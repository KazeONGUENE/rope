//! Partition map — deterministic mapping from shard to owning node.
//!
//! ## Why 256 shards
//!
//! All intra-node parallelism in Datachain Rope (the lattice in
//! `rope-core::lattice`, the per-shard HLC in `rope-core::clock`,
//! the per-wallet head locks in `rope-core::personal_ledger`, and
//! the parallel WriteBatch consumers in
//! `rope-storage::rocksdb_persistence`) is keyed by the first byte
//! of an addr-derived hash. P2.D adopts the same 256-shard
//! granularity so that **once an op is routed to its owning node,
//! the existing per-shard primitives handle it without any further
//! routing**.
//!
//! ## Why first-byte sharding
//!
//! All wallet addresses are 20-byte secp256k1-derived public-key
//! hashes (or, for synthetic test wallets, 20-byte random bytes
//! with the same shape). The first byte is uniformly distributed,
//! so first-byte sharding gives a near-ideal balance across nodes
//! without any rehash. Identical sharding axis to the lattice.

use rope_core::types::NodeId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Number of shards in the cluster keyspace. Matches the per-shard
/// granularity used by `rope-core::lattice::NUM_SHARDS` and
/// `rope-core::clock::NUM_SHARDS`.
pub const NUM_SHARDS: usize = 256;

/// One partition of the keyspace. Wraps `u8` so it's literally
/// `wallet_address[0]`, matching the lattice/clock sharding axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ShardId(pub u8);

impl ShardId {
    /// Compute the shard for a wallet address (first byte). Cheapest
    /// possible derivation — no hash, no allocation.
    #[inline]
    pub fn for_wallet(wallet: &[u8]) -> Self {
        ShardId(wallet.first().copied().unwrap_or(0))
    }

    /// Compute the shard for a 32-byte string id (first byte —
    /// matches `lattice::shard_for_string_id`).
    #[inline]
    pub fn for_string_id(string_id: &[u8; 32]) -> Self {
        ShardId(string_id[0])
    }

    /// Iterate over all 256 shards in ascending order. Useful for
    /// building partition maps and for tests.
    pub fn all() -> impl Iterator<Item = ShardId> {
        (0u8..=255).map(ShardId)
    }

    /// Index into a length-256 array.
    #[inline]
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

impl From<u8> for ShardId {
    fn from(b: u8) -> Self {
        ShardId(b)
    }
}

/// Per-shard ownership record. A shard is owned by exactly one node
/// at any point in time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardOwnership {
    pub shard: ShardId,
    pub owner: NodeId,
}

/// Static partition map: a fixed assignment of all 256 shards to
/// member nodes. Cheap to clone (one `Vec<NodeId>` of length 256).
///
/// `PartitionMap` is intentionally immutable. Topology changes
/// produce a brand-new `PartitionMap` that the cluster client can
/// swap atomically; in-flight ops always see a single consistent
/// snapshot.
///
/// Storage is `Vec<NodeId>` (always length [`NUM_SHARDS`]) rather
/// than `Box<[NodeId; 256]>` because serde's `Serialize` /
/// `Deserialize` derive only supports fixed-size arrays up to 32
/// elements. The wrapper keeps lookups O(1) and the length
/// invariant is enforced in every constructor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionMap {
    /// Owner per shard, indexed by `ShardId.0`. Always exactly
    /// [`NUM_SHARDS`] long.
    owners: Vec<NodeId>,
}

impl PartitionMap {
    /// Build a partition map by spreading shards across `nodes` in
    /// round-robin order. Node `i` owns shards `i, i+|nodes|, …`.
    /// Any node count `>= 1` is valid; with one node, every shard
    /// resolves to that node (single-node deployments).
    ///
    /// Panics if `nodes` is empty — a cluster with zero nodes has
    /// no defined ownership.
    pub fn round_robin(nodes: &[NodeId]) -> Self {
        assert!(!nodes.is_empty(), "PartitionMap requires at least one node");
        let mut owners: Vec<NodeId> = vec![nodes[0]; NUM_SHARDS];
        for (i, slot) in owners.iter_mut().enumerate() {
            *slot = nodes[i % nodes.len()];
        }
        Self { owners }
    }

    /// Build a partition map by hashing each shard id with BLAKE3
    /// and assigning to the highest-hash node according to a
    /// rendezvous-hashing (HRW) scheme. Slightly more expensive than
    /// round-robin but produces minimal disruption when a node
    /// joins or leaves: only `1/N` of the shards move.
    pub fn rendezvous(nodes: &[NodeId]) -> Self {
        assert!(!nodes.is_empty(), "PartitionMap requires at least one node");
        let mut owners: Vec<NodeId> = vec![nodes[0]; NUM_SHARDS];
        for s in 0..NUM_SHARDS {
            // Pick the node whose blake3(s || node_id) is largest.
            let mut best: Option<(NodeId, [u8; 32])> = None;
            for n in nodes {
                let mut hasher = blake3::Hasher::new();
                hasher.update(&[s as u8]);
                hasher.update(n.as_bytes());
                let h = *hasher.finalize().as_bytes();
                if best.map_or(true, |(_, b)| h > b) {
                    best = Some((*n, h));
                }
            }
            owners[s] = best.unwrap().0;
        }
        Self { owners }
    }

    /// Build a partition map from an explicit assignment. Every
    /// shard must be present exactly once.
    pub fn from_assignments(assignments: &[ShardOwnership]) -> Result<Self, String> {
        if assignments.len() != NUM_SHARDS {
            return Err(format!(
                "expected exactly {NUM_SHARDS} assignments, got {}",
                assignments.len()
            ));
        }
        // Use the first assignment's owner as the placeholder so
        // we can detect missing shards by checking every slot was
        // overwritten exactly once.
        let mut owners: Vec<NodeId> = vec![assignments[0].owner; NUM_SHARDS];
        let mut seen = [false; NUM_SHARDS];
        for ShardOwnership { shard, owner } in assignments {
            let idx = shard.as_usize();
            if seen[idx] {
                return Err(format!("shard {idx} assigned twice"));
            }
            seen[idx] = true;
            owners[idx] = *owner;
        }
        for (idx, present) in seen.iter().enumerate() {
            if !*present {
                return Err(format!("shard {idx} has no owner"));
            }
        }
        Ok(Self { owners })
    }

    /// Look up the owner for a shard. O(1).
    #[inline]
    pub fn owner(&self, shard: ShardId) -> NodeId {
        // SAFETY-equivalent invariant: `owners.len() == NUM_SHARDS`
        // is enforced in every constructor and `PartitionMap` is
        // immutable, so the index is in-bounds.
        self.owners[shard.as_usize()]
    }

    /// Look up the owner for a wallet (convenience wrapper).
    #[inline]
    pub fn owner_for_wallet(&self, wallet: &[u8]) -> NodeId {
        self.owner(ShardId::for_wallet(wallet))
    }

    /// Snapshot of the full assignment, useful for serialisation
    /// and tests.
    pub fn assignments(&self) -> Vec<ShardOwnership> {
        self.owners
            .iter()
            .enumerate()
            .map(|(i, &owner)| ShardOwnership {
                shard: ShardId(i as u8),
                owner,
            })
            .collect()
    }

    /// Group shards by owner — useful for batching ops by target
    /// node before dispatch.
    pub fn shards_by_owner(&self) -> HashMap<NodeId, Vec<ShardId>> {
        let mut by_owner: HashMap<NodeId, Vec<ShardId>> = HashMap::new();
        for (i, &owner) in self.owners.iter().enumerate() {
            by_owner.entry(owner).or_default().push(ShardId(i as u8));
        }
        by_owner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(byte: u8) -> NodeId {
        NodeId::new([byte; 32])
    }

    #[test]
    fn shard_for_wallet_uses_first_byte() {
        let w = [0xAB, 0x12, 0x34];
        assert_eq!(ShardId::for_wallet(&w), ShardId(0xAB));
    }

    #[test]
    fn shard_for_empty_wallet_is_zero() {
        assert_eq!(ShardId::for_wallet(&[]), ShardId(0));
    }

    #[test]
    fn round_robin_balances_evenly_for_power_of_two() {
        let nodes = (0..4u8).map(n).collect::<Vec<_>>();
        let pm = PartitionMap::round_robin(&nodes);
        let groups = pm.shards_by_owner();
        // 256 / 4 = 64 each.
        for nd in &nodes {
            assert_eq!(groups.get(nd).map(|v| v.len()).unwrap_or(0), 64);
        }
    }

    #[test]
    fn round_robin_handles_uneven_node_count() {
        let nodes = (0..3u8).map(n).collect::<Vec<_>>();
        let pm = PartitionMap::round_robin(&nodes);
        let groups = pm.shards_by_owner();
        // 256 / 3 = 85 + 86 + 85.
        let counts: Vec<usize> = nodes
            .iter()
            .map(|n| groups.get(n).map(|v| v.len()).unwrap_or(0))
            .collect();
        assert_eq!(counts.iter().sum::<usize>(), NUM_SHARDS);
        for c in &counts {
            assert!((*c as i32 - 86).abs() <= 1, "shard count {c} unbalanced");
        }
    }

    #[test]
    fn rendezvous_balances_reasonably_for_8_nodes() {
        let nodes = (0..8u8).map(n).collect::<Vec<_>>();
        let pm = PartitionMap::rendezvous(&nodes);
        let groups = pm.shards_by_owner();
        // 256 / 8 = 32. Allow ±50% drift on a small key space —
        // rendezvous is uniform in expectation but variance is
        // higher than round-robin.
        for nd in &nodes {
            let c = groups.get(nd).map(|v| v.len()).unwrap_or(0);
            assert!(
                (16..=64).contains(&c),
                "node {nd:?} got {c} shards, expected ~32"
            );
        }
    }

    #[test]
    fn rendezvous_only_moves_one_n_th_when_a_node_leaves() {
        // Build with 4 nodes, then with 3; count how many shards changed
        // owner. With rendezvous hashing the expected disruption is
        // |moved| ≈ 1/4 of shards.
        let nodes_4 = (0..4u8).map(n).collect::<Vec<_>>();
        let nodes_3 = (0..3u8).map(n).collect::<Vec<_>>();
        let pm4 = PartitionMap::rendezvous(&nodes_4);
        let pm3 = PartitionMap::rendezvous(&nodes_3);
        let mut moved = 0usize;
        for s in ShardId::all() {
            if pm4.owner(s) != pm3.owner(s) {
                moved += 1;
            }
        }
        // Generous bounds (1/4 ± 25%) — the test is a regression
        // guard, not a hash-quality assertion.
        let expected = NUM_SHARDS / 4;
        assert!(
            moved >= expected / 2 && moved <= expected * 2,
            "rendezvous moved {moved} shards (expected ~{expected})"
        );
    }

    #[test]
    fn round_robin_panics_on_empty_node_list() {
        let r = std::panic::catch_unwind(|| PartitionMap::round_robin(&[]));
        assert!(r.is_err());
    }

    #[test]
    fn from_assignments_rejects_missing_shards() {
        let nodes: Vec<_> = (0..2u8).map(n).collect();
        let mut assigns: Vec<_> = (0..255u16)
            .map(|s| ShardOwnership {
                shard: ShardId(s as u8),
                owner: nodes[(s as usize) % 2],
            })
            .collect();
        // Missing shard 255.
        assert!(PartitionMap::from_assignments(&assigns).is_err());
        assigns.push(ShardOwnership {
            shard: ShardId(255),
            owner: nodes[1],
        });
        assert!(PartitionMap::from_assignments(&assigns).is_ok());
    }

    #[test]
    fn from_assignments_rejects_duplicates() {
        let nodes: Vec<_> = (0..2u8).map(n).collect();
        let mut assigns: Vec<_> = (0..NUM_SHARDS)
            .map(|s| ShardOwnership {
                shard: ShardId(s as u8),
                owner: nodes[s % 2],
            })
            .collect();
        // Duplicate shard 7 (replace shard 0's slot then re-add).
        assigns[0] = ShardOwnership {
            shard: ShardId(7),
            owner: nodes[0],
        };
        assert!(PartitionMap::from_assignments(&assigns).is_err());
    }

    #[test]
    fn owner_for_wallet_resolves_to_correct_node() {
        let nodes: Vec<_> = (0..4u8).map(n).collect();
        let pm = PartitionMap::round_robin(&nodes);
        // Wallet whose first byte is 7 → shard 7 → node 7 % 4 = 3.
        let wallet = [0x07, 0xFF, 0xFF, 0xFF];
        assert_eq!(pm.owner_for_wallet(&wallet), nodes[3]);
    }
}
