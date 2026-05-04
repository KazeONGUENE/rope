//! Shard endpoints — the per-node execution boundary.
//!
//! Every node in the cluster has exactly one [`ShardEndpoint`] —
//! the thing that takes a [`ShardOp`] and runs it. The trait has
//! two production-relevant implementations:
//!
//! - [`LocalShardEndpoint`] — runs the op in-process. Used for
//!   shards owned by the calling node.
//! - [`InMemoryRemoteEndpoint`] — a test harness that simulates a
//!   remote node by forwarding the op into another node's
//!   [`LocalShardEndpoint`]. Used by the multi-node integration
//!   tests in this crate and by the `rope-loadgen cluster-write`
//!   subcommand.
//!
//! Production deployments will swap [`InMemoryRemoteEndpoint`] for
//! a network-backed implementation (gRPC, libp2p
//! request-response, …). The trait surface is intentionally small
//! so the production transport only has to implement
//! [`ShardEndpoint::execute`].

use crate::error::{ClusterError, ClusterResult};
use crate::op::{ShardOp, ShardResult};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Tag describing which kind of endpoint we're holding. Useful for
/// dashboards and tests; does NOT influence routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShardEndpointKind {
    Local,
    Remote,
}

/// The execution boundary for one shard owner. Implementations are
/// `Send + Sync` so the routing layer can hold them in an `Arc` and
/// dispatch from many tokio worker threads at once.
#[async_trait]
pub trait ShardEndpoint: Send + Sync {
    /// What kind of endpoint this is — used by callers that want to
    /// short-circuit local dispatch (skip the wire format entirely
    /// when the target shard is local). Production transports may
    /// always return `Remote`.
    fn kind(&self) -> ShardEndpointKind;

    /// Execute the op against the owning node and return its
    /// result. Errors propagate back as
    /// [`ClusterError::EndpointFailed`] when they originate inside
    /// the shard owner; transport-level failures use the
    /// [`ClusterError::Transport`] variant.
    async fn execute(&self, op: ShardOp) -> ClusterResult<ShardResult>;

    /// Number of ops this endpoint has executed since creation.
    /// Cheap counter used for dashboards and tests.
    fn ops_executed(&self) -> u64;
}

// ----------------------------------------------------------------------
// LocalShardEndpoint — in-process executor
// ----------------------------------------------------------------------

/// Type-erased per-op handler. The receiving node implements this
/// closure to bridge the opaque payload into its concrete app
/// logic (typically `LedgerManager::*` calls). Returning `Err` is
/// surfaced to the caller as [`ClusterError::EndpointFailed`].
pub type LocalHandler = Arc<dyn Fn(ShardOp) -> Result<ShardResult, String> + Send + Sync>;

/// In-process shard executor. Holds a single handler closure that
/// the receiving node provides; the closure is responsible for
/// decoding the op's payload and executing the corresponding
/// `LedgerManager::*` (or other) method.
pub struct LocalShardEndpoint {
    handler: LocalHandler,
    ops: AtomicU64,
}

impl LocalShardEndpoint {
    pub fn new(handler: LocalHandler) -> Arc<Self> {
        Arc::new(Self {
            handler,
            ops: AtomicU64::new(0),
        })
    }

    /// Convenience builder for tests: a handler that always returns
    /// the supplied static payload, regardless of the op.
    pub fn echo(payload: Vec<u8>) -> Arc<Self> {
        let h: LocalHandler = Arc::new(move |_op| Ok(ShardResult::new(payload.clone())));
        Self::new(h)
    }
}

#[async_trait]
impl ShardEndpoint for LocalShardEndpoint {
    fn kind(&self) -> ShardEndpointKind {
        ShardEndpointKind::Local
    }

    async fn execute(&self, op: ShardOp) -> ClusterResult<ShardResult> {
        // The handler is sync — it's a function pointer into the
        // receiving node's app logic. For the in-process case
        // there's no benefit to spawning onto a tokio blocking pool
        // unless the closure does substantial I/O, so we just call
        // it directly. Production code that does real I/O should
        // wrap the call in `tokio::task::spawn_blocking`.
        let r = (self.handler)(op).map_err(|message| ClusterError::EndpointFailed {
            node: rope_core::types::NodeId::new([0u8; 32]),
            message,
        });
        if r.is_ok() {
            self.ops.fetch_add(1, Ordering::Relaxed);
        }
        r
    }

    fn ops_executed(&self) -> u64 {
        self.ops.load(Ordering::Relaxed)
    }
}

// ----------------------------------------------------------------------
// InMemoryRemoteEndpoint — test/sim transport
// ----------------------------------------------------------------------

/// Simulation harness for a "remote" node, used by integration
/// tests in this crate. Forwards the op into another
/// [`LocalShardEndpoint`] without touching any real network. Useful
/// for asserting routing correctness end-to-end without spinning up
/// real sockets.
///
/// The simulated peer is held by `Arc<dyn ShardEndpoint>`, which
/// means the test harness can chain endpoints arbitrarily — e.g.
/// `client → remote → another_remote → local` — and still get the
/// right routing decisions.
pub struct InMemoryRemoteEndpoint {
    target: Arc<dyn ShardEndpoint>,
    ops: AtomicU64,
    /// Optional fault injector. If set, any op for which the
    /// closure returns `Some(message)` fails with
    /// [`ClusterError::Transport`]. Used to test failover paths.
    fault: Mutex<Option<Arc<dyn Fn(&ShardOp) -> Option<String> + Send + Sync>>>,
}

impl InMemoryRemoteEndpoint {
    pub fn new(target: Arc<dyn ShardEndpoint>) -> Arc<Self> {
        Arc::new(Self {
            target,
            ops: AtomicU64::new(0),
            fault: Mutex::new(None),
        })
    }

    /// Install a fault-injection closure. `closure(op) → Some(msg)`
    /// causes that op to fail with the given message; `None` lets
    /// it through to the wrapped endpoint.
    pub fn install_fault<F>(&self, f: F)
    where
        F: Fn(&ShardOp) -> Option<String> + Send + Sync + 'static,
    {
        *self.fault.lock() = Some(Arc::new(f));
    }

    /// Remove the fault injector.
    pub fn clear_fault(&self) {
        *self.fault.lock() = None;
    }
}

#[async_trait]
impl ShardEndpoint for InMemoryRemoteEndpoint {
    fn kind(&self) -> ShardEndpointKind {
        ShardEndpointKind::Remote
    }

    async fn execute(&self, op: ShardOp) -> ClusterResult<ShardResult> {
        if let Some(f) = self.fault.lock().clone() {
            if let Some(msg) = f(&op) {
                return Err(ClusterError::Transport(msg));
            }
        }
        let r = self.target.execute(op).await;
        if r.is_ok() {
            self.ops.fetch_add(1, Ordering::Relaxed);
        }
        r
    }

    fn ops_executed(&self) -> u64 {
        self.ops.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::ShardOpKind;

    #[tokio::test]
    async fn local_endpoint_runs_handler_and_counts_ops() {
        let ep = LocalShardEndpoint::echo(b"pong".to_vec());
        let r = ep
            .execute(ShardOp::new(vec![0u8; 20], ShardOpKind::Custom, vec![]))
            .await
            .unwrap();
        assert_eq!(r.payload, b"pong");
        assert_eq!(ep.ops_executed(), 1);
    }

    #[tokio::test]
    async fn local_endpoint_propagates_handler_errors() {
        let h: LocalHandler = Arc::new(|_| Err("nope".to_string()));
        let ep = LocalShardEndpoint::new(h);
        let r = ep
            .execute(ShardOp::new(vec![0u8; 20], ShardOpKind::Custom, vec![]))
            .await;
        assert!(matches!(r, Err(ClusterError::EndpointFailed { .. })));
        assert_eq!(ep.ops_executed(), 0, "failed ops do not count");
    }

    #[tokio::test]
    async fn remote_endpoint_forwards_to_inner_local() {
        let inner = LocalShardEndpoint::echo(b"forwarded".to_vec());
        let remote = InMemoryRemoteEndpoint::new(inner.clone());
        let r = remote
            .execute(ShardOp::new(vec![0u8; 20], ShardOpKind::Custom, vec![]))
            .await
            .unwrap();
        assert_eq!(r.payload, b"forwarded");
        assert_eq!(remote.ops_executed(), 1);
        assert_eq!(inner.ops_executed(), 1);
    }

    #[tokio::test]
    async fn remote_endpoint_fault_injector_fires() {
        let inner = LocalShardEndpoint::echo(b"x".to_vec());
        let remote = InMemoryRemoteEndpoint::new(inner.clone());
        remote.install_fault(|_op| Some("simulated outage".to_string()));
        let r = remote
            .execute(ShardOp::new(vec![0u8; 20], ShardOpKind::Custom, vec![]))
            .await;
        assert!(matches!(r, Err(ClusterError::Transport(_))));
        assert_eq!(inner.ops_executed(), 0, "inner must not have run");
        // After clear, ops succeed again.
        remote.clear_fault();
        let r = remote
            .execute(ShardOp::new(vec![0u8; 20], ShardOpKind::Custom, vec![]))
            .await;
        assert!(r.is_ok());
    }
}
