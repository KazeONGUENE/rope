//! Shard-routable operations.
//!
//! [`ShardOp`] is the wire-shape every cluster client sends to a
//! shard owner. It carries the wallet identifier the
//! [`crate::router::ClusterClient`] uses to compute the target
//! shard, plus an opaque payload that the receiving node interprets.
//!
//! The opaque payload is intentionally `Vec<u8>` (not a typed enum)
//! so this crate doesn't need to depend on `rope-protocols` or
//! `rope-node`. Concrete payload encodings live in those crates and
//! are decoded after routing.

use serde::{Deserialize, Serialize};

/// Discriminator for the kind of operation being routed. Used by
/// dashboards / logs and (in a future patch) by the cross-shard
/// coordinator to decide which ops can be batched together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardOpKind {
    /// `LedgerManager::create_ledger`
    CreateLedger,
    /// `LedgerManager::append_to_ledger`
    AppendToLedger,
    /// `LedgerManager::get_chain` (read)
    GetChain,
    /// `LedgerManager::erase_personal_ledger`
    ErasePersonalLedger,
    /// `LedgerManager::untie_knot`
    UntieKnot,
    /// Catch-all for ops that don't fit the above (e.g. cluster
    /// admin pings, health checks). Routing still works; the
    /// receiving node decides what to do.
    Custom,
}

/// One routable op. Owns the wallet identifier (for routing) and an
/// opaque payload (for the shard owner to decode).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardOp {
    /// Wallet address. Routing key — its first byte selects the
    /// shard, see [`crate::partition::ShardId::for_wallet`].
    pub wallet: Vec<u8>,
    /// What kind of op this is. Used for telemetry and (future)
    /// batching decisions; does NOT influence routing.
    pub kind: ShardOpKind,
    /// Opaque payload — bincode-encoded by the caller, decoded by
    /// the receiving node.
    pub payload: Vec<u8>,
}

impl ShardOp {
    pub fn new(wallet: Vec<u8>, kind: ShardOpKind, payload: Vec<u8>) -> Self {
        Self {
            wallet,
            kind,
            payload,
        }
    }
}

/// Result of a shard op. Same opaque-payload pattern: the routing
/// layer doesn't interpret the bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardResult {
    /// Bincode-encoded result, decoded by the calling layer.
    pub payload: Vec<u8>,
}

impl ShardResult {
    pub fn new(payload: Vec<u8>) -> Self {
        Self { payload }
    }

    pub fn empty() -> Self {
        Self { payload: Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_op_round_trips_through_bincode() {
        let op = ShardOp::new(vec![0xAA; 20], ShardOpKind::AppendToLedger, b"hi".to_vec());
        let bytes = bincode::serialize(&op).unwrap();
        let back: ShardOp = bincode::deserialize(&bytes).unwrap();
        assert_eq!(op, back);
    }

    #[test]
    fn shard_result_round_trips() {
        let r = ShardResult::new(vec![1, 2, 3]);
        let bytes = bincode::serialize(&r).unwrap();
        let back: ShardResult = bincode::deserialize(&bytes).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn empty_result_serialises() {
        let r = ShardResult::empty();
        assert!(r.payload.is_empty());
        let _ = bincode::serialize(&r).unwrap();
    }
}
