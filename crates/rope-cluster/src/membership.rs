//! Cluster membership snapshot — the set of nodes that exist *right
//! now*. Membership changes (joins, leaves, failures) produce a
//! brand-new immutable snapshot rather than mutating in-place. This
//! keeps the routing decision strictly serialisable: every op sees
//! exactly one membership.
//!
//! Production deployments will obtain membership via a coordinator
//! (libp2p Kademlia DHT, Consul, etc.). For unit tests and the
//! in-process integration harness, [`ClusterMembership::from_nodes`]
//! is enough.

use rope_core::types::NodeId;
use serde::{Deserialize, Serialize};

/// Per-node descriptor — what a routing client needs to know to
/// deliver an op to a node. `address` is opaque from the point of
/// view of this crate; the production transport interprets it
/// (e.g. as an `https://host:port` URL or libp2p multiaddr).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDescriptor {
    pub id: NodeId,
    pub address: String,
}

impl NodeDescriptor {
    pub fn new(id: NodeId, address: impl Into<String>) -> Self {
        Self {
            id,
            address: address.into(),
        }
    }
}

/// Immutable snapshot of cluster membership. Cheap to clone (one
/// `Vec<NodeDescriptor>`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterMembership {
    nodes: Vec<NodeDescriptor>,
}

impl ClusterMembership {
    pub fn from_nodes(nodes: Vec<NodeDescriptor>) -> Self {
        let mut nodes = nodes;
        nodes.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
        Self { nodes }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &NodeDescriptor> {
        self.nodes.iter()
    }

    pub fn ids(&self) -> Vec<NodeId> {
        self.nodes.iter().map(|n| n.id).collect()
    }

    pub fn lookup(&self, id: &NodeId) -> Option<&NodeDescriptor> {
        self.nodes.iter().find(|n| n.id == *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(byte: u8, addr: &str) -> NodeDescriptor {
        NodeDescriptor::new(NodeId::new([byte; 32]), addr)
    }

    #[test]
    fn sorted_by_id_for_deterministic_iteration() {
        let m = ClusterMembership::from_nodes(vec![
            n(3, "addr3"),
            n(1, "addr1"),
            n(2, "addr2"),
        ]);
        let bytes: Vec<u8> = m.iter().map(|nd| nd.id.as_bytes()[0]).collect();
        assert_eq!(bytes, vec![1, 2, 3]);
    }

    #[test]
    fn lookup_returns_descriptor() {
        let m = ClusterMembership::from_nodes(vec![n(1, "addr1"), n(2, "addr2")]);
        let nd = m.lookup(&NodeId::new([1u8; 32])).expect("found");
        assert_eq!(nd.address, "addr1");
        assert!(m.lookup(&NodeId::new([99u8; 32])).is_none());
    }

    #[test]
    fn ids_returns_in_sorted_order() {
        let m = ClusterMembership::from_nodes(vec![n(5, "x"), n(1, "y"), n(3, "z")]);
        assert_eq!(
            m.ids(),
            vec![
                NodeId::new([1u8; 32]),
                NodeId::new([3u8; 32]),
                NodeId::new([5u8; 32])
            ]
        );
    }
}
