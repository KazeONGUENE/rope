//! # `rope-cluster` — Quipu Canon v2.0 Phase 2.D
//!
//! Horizontal node sharding for Datachain Rope. With Phases 1.1–2.C.1
//! the in-process bottlenecks (sharded lattice, parallel WriteBatch,
//! signature batch verification, lattice finality watermark) are gone
//! and a single node sustains ~190 k durable signed-knot ops/s on
//! commodity hardware. The next ceiling is the box itself —
//! CPU/disk/network of one machine. P2.D scales OUT across machines:
//! independent wallets execute on independent nodes in parallel.
//!
//! ## Sharding scheme
//!
//! - The keyspace is partitioned into [`NUM_SHARDS`] (256) shards
//!   keyed by [`ShardId`] = `wallet_address[0]`. This matches the
//!   intra-node sharding axis used by `rope-core::lattice` and
//!   `rope-core::clock`, so once the cluster routes an op to the
//!   correct node, that node's existing per-shard data structures
//!   handle it without further routing.
//! - A [`PartitionMap`] assigns each shard to exactly one
//!   [`NodeId`]. The default assignment is round-robin
//!   (`shard_id % node_count`) but the trait permits any
//!   deterministic function.
//! - [`ShardEndpoint`] is the per-shard execution boundary. The
//!   [`LocalShardEndpoint`] runs ops in-process; the
//!   [`InMemoryRemoteEndpoint`] is a simulation harness for
//!   integration tests; production deployments will swap in a
//!   real network-backed implementation (gRPC, libp2p
//!   request-response, etc.).
//! - [`ClusterClient`] is the dispatch API every caller uses. Given
//!   any [`ShardOp`], it computes the target shard, looks up the
//!   owning node via [`PartitionMap`], and forwards to the right
//!   [`ShardEndpoint`].
//!
//! ## What this crate does NOT do (yet)
//!
//! - **Topology changes.** Adding/removing nodes today requires a
//!   coordinated [`PartitionMap`] swap across all callers. A
//!   future Phase 2.D.1 will add lease-based shard ownership and
//!   incremental rebalance.
//! - **Cross-shard transactions.** All current ops are wallet-keyed
//!   (so they fit in a single shard) or already global (anchors,
//!   testimonies, broadcast). A future Phase 2.D.2 will add a 2PC
//!   coordinator for ops that touch two distinct wallets atomically
//!   (e.g. cross-wallet credit transfers in `rope-smartchain`).
//! - **Replication.** Each shard has exactly one owner here. Phase
//!   2.D.3 will add quorum replication of the per-shard log.
//!
//! These are deliberately separate steps so each can be reviewed,
//! benchmarked, and rolled back independently — the same
//! incremental discipline used by Phases 1.1–2.C.1.

pub mod endpoint;
pub mod error;
pub mod membership;
pub mod op;
pub mod partition;
pub mod router;

pub use endpoint::{InMemoryRemoteEndpoint, LocalShardEndpoint, ShardEndpoint, ShardEndpointKind};
pub use error::{ClusterError, ClusterResult};
pub use membership::{ClusterMembership, NodeDescriptor};
pub use op::{ShardOp, ShardOpKind, ShardResult};
pub use partition::{PartitionMap, ShardId, ShardOwnership, NUM_SHARDS};
pub use router::ClusterClient;
