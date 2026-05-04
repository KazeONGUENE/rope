//! Cluster-routing errors.

use thiserror::Error;

use crate::partition::ShardId;
use rope_core::types::NodeId;

pub type ClusterResult<T> = Result<T, ClusterError>;

#[derive(Debug, Error)]
pub enum ClusterError {
    /// The partition map references a node that is not in the
    /// membership snapshot.
    #[error("shard {shard:?} is owned by node {node:?} which is not in the cluster membership")]
    OwnerNotInMembership { shard: ShardId, node: NodeId },

    /// No endpoint registered for the given node id.
    #[error("no endpoint registered for node {node:?}")]
    EndpointNotFound { node: NodeId },

    /// The endpoint refused or failed to execute the op.
    #[error("endpoint for node {node:?} failed: {message}")]
    EndpointFailed { node: NodeId, message: String },

    /// The op cannot be routed because it carries no wallet
    /// identifier (cluster routing is wallet-keyed).
    #[error("op carries no wallet identifier; cannot route")]
    UnroutableOp,

    /// Cluster topology change in flight; routing is paused.
    #[error("cluster topology change in flight; retry after rebalance")]
    TopologyChanging,

    /// Generic catch-all so the production transport can pass
    /// through wire-level errors without losing fidelity.
    #[error("transport error: {0}")]
    Transport(String),
}
