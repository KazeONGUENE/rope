//! # Validator Registry — Quipu Canon v2.0 Phase 2
//!
//! Maps each consensus [`NodeId`] to the [`HybridPublicKey`] the node uses
//! to sign testimonies, plus a stake weight. This is the piece that turns
//! testimony signature *checking* from "trust that a signature is present"
//! (the pre-Phase-2 behaviour) into real cryptographic verification against
//! a known key.
//!
//! ## Why a registry
//!
//! `TestimonyCollector::validate_testimony` needs the validator's public
//! key to verify the hybrid signature over `Testimony::signing_data()`.
//! Testimonies carry only the `NodeId` (32 bytes), not the full ~2 KB
//! hybrid public key, so the collector resolves `NodeId → HybridPublicKey`
//! through this registry.
//!
//! ## Identity binding
//!
//! A validator's `NodeId` MUST equal `blake3(ed25519_pubkey)` — the same
//! derivation [`HybridPublicKey::node_id`] uses. `register` enforces this
//! so a validator cannot register a key for a `NodeId` it does not control.
//! This binds the consensus identity to the signing key with no separate
//! certificate step.
//!
//! ## Stake weight
//!
//! Each validator carries a `weight` (default 1). Finality is computed by
//! `TestimonyCollection` on validator *count* today (2f+1), but the
//! registry already tracks weight so a future stake-weighted quorum rule
//! is a drop-in change rather than a schema migration.

use parking_lot::RwLock;
use rope_core::types::NodeId;
use rope_crypto::hybrid::HybridPublicKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// One validator's on-registry record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorRecord {
    /// The validator's consensus id. Equals `blake3(ed25519_pubkey)`.
    pub node_id: NodeId,
    /// The hybrid public key (Ed25519 + Dilithium3, and optionally
    /// X25519 + Kyber768) the validator signs testimonies with.
    pub public_key: HybridPublicKey,
    /// Stake weight. Defaults to 1. Reserved for stake-weighted quorum.
    pub weight: u64,
    /// Whether the validator is currently active in the committee.
    /// Inactive validators keep their record (for historical
    /// verification of old testimonies) but do not count toward the
    /// live quorum size.
    pub active: bool,
}

/// Errors surfaced by [`ValidatorRegistry`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// The supplied `NodeId` does not equal `blake3(ed25519_pubkey)`.
    /// Prevents a node from registering a key it does not control.
    IdentityMismatch { expected: NodeId, supplied: NodeId },
    /// The public key has no Dilithium component. Consensus mandates
    /// hybrid PQ keys — an Ed25519-only key is rejected.
    MissingPostQuantumKey,
}

fn short_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes.iter().take(8) {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::IdentityMismatch { expected, supplied } => write!(
                f,
                "validator identity mismatch: node_id {} does not match blake3(ed25519)={}",
                short_hex(supplied.as_bytes()),
                short_hex(expected.as_bytes()),
            ),
            RegistryError::MissingPostQuantumKey => {
                write!(f, "validator public key has no Dilithium component")
            }
        }
    }
}

impl std::error::Error for RegistryError {}

/// Serializable snapshot of the whole committee. Used to ship the
/// validator set in node config, gossip it to peers, and load it at
/// startup.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ValidatorSetSnapshot {
    pub validators: Vec<ValidatorRecord>,
}

/// Thread-safe registry of validators keyed by [`NodeId`].
///
/// Cheap to share (`Arc<ValidatorRegistry>`); all mutation goes through
/// a single `RwLock<HashMap>`. Reads (the hot path — one lookup per
/// testimony verification) take a read lock only.
pub struct ValidatorRegistry {
    validators: RwLock<HashMap<NodeId, ValidatorRecord>>,
}

impl ValidatorRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            validators: RwLock::new(HashMap::new()),
        }
    }

    /// Build a registry from a snapshot (config load / gossip).
    /// Records are inserted verbatim (identity is assumed already
    /// validated when the snapshot was produced). Returns the count.
    pub fn from_snapshot(snapshot: &ValidatorSetSnapshot) -> Self {
        let mut map = HashMap::with_capacity(snapshot.validators.len());
        for rec in &snapshot.validators {
            map.insert(rec.node_id, rec.clone());
        }
        Self {
            validators: RwLock::new(map),
        }
    }

    /// Register a validator with weight 1.
    ///
    /// Enforces `node_id == blake3(ed25519_pubkey)` and that the key
    /// carries a Dilithium component. Replaces any existing record for
    /// the same `node_id`.
    pub fn register(
        &self,
        node_id: NodeId,
        public_key: HybridPublicKey,
    ) -> Result<(), RegistryError> {
        self.register_weighted(node_id, public_key, 1)
    }

    /// Register a validator with an explicit stake weight.
    pub fn register_weighted(
        &self,
        node_id: NodeId,
        public_key: HybridPublicKey,
        weight: u64,
    ) -> Result<(), RegistryError> {
        // Identity binding: the node_id must be the BLAKE3 of the
        // Ed25519 public key. This is the same derivation
        // HybridPublicKey::node_id uses, so a node cannot claim an
        // id it does not hold the key for.
        let derived = NodeId::new(public_key.node_id());
        if derived != node_id {
            return Err(RegistryError::IdentityMismatch {
                expected: derived,
                supplied: node_id,
            });
        }

        // Consensus mandates hybrid PQ keys.
        if !public_key.has_pq_keys() {
            return Err(RegistryError::MissingPostQuantumKey);
        }

        self.validators.write().insert(
            node_id,
            ValidatorRecord {
                node_id,
                public_key,
                weight,
                active: true,
            },
        );
        Ok(())
    }

    /// Look up a validator's public key.
    pub fn public_key(&self, node_id: &NodeId) -> Option<HybridPublicKey> {
        self.validators
            .read()
            .get(node_id)
            .map(|r| r.public_key.clone())
    }

    /// Look up a validator's stake weight (0 if unknown).
    pub fn weight(&self, node_id: &NodeId) -> u64 {
        self.validators
            .read()
            .get(node_id)
            .map(|r| r.weight)
            .unwrap_or(0)
    }

    /// True if the node is a known, active validator.
    pub fn is_active(&self, node_id: &NodeId) -> bool {
        self.validators
            .read()
            .get(node_id)
            .map(|r| r.active)
            .unwrap_or(false)
    }

    /// True if the node has a record (active or not).
    pub fn contains(&self, node_id: &NodeId) -> bool {
        self.validators.read().contains_key(node_id)
    }

    /// Mark a validator active/inactive without removing its key
    /// (so historical testimonies remain verifiable).
    pub fn set_active(&self, node_id: &NodeId, active: bool) {
        if let Some(rec) = self.validators.write().get_mut(node_id) {
            rec.active = active;
        }
    }

    /// Number of active validators — the `n` used for the 2f+1 quorum.
    pub fn active_count(&self) -> usize {
        self.validators
            .read()
            .values()
            .filter(|r| r.active)
            .count()
    }

    /// Total records (active + inactive).
    pub fn len(&self) -> usize {
        self.validators.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.validators.read().is_empty()
    }

    /// Sum of weights over active validators.
    pub fn total_active_weight(&self) -> u64 {
        self.validators
            .read()
            .values()
            .filter(|r| r.active)
            .map(|r| r.weight)
            .sum()
    }

    /// The list of active validator node ids.
    pub fn active_validators(&self) -> Vec<NodeId> {
        self.validators
            .read()
            .values()
            .filter(|r| r.active)
            .map(|r| r.node_id)
            .collect()
    }

    /// Export a serializable snapshot of the full set.
    pub fn snapshot(&self) -> ValidatorSetSnapshot {
        ValidatorSetSnapshot {
            validators: self.validators.read().values().cloned().collect(),
        }
    }
}

impl Default for ValidatorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience: wrap in an `Arc` for sharing across the collector, the
/// orchestrator, and the RPC layer.
pub fn shared() -> Arc<ValidatorRegistry> {
    Arc::new(ValidatorRegistry::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rope_crypto::hybrid::HybridSigner;

    fn validator() -> (NodeId, HybridPublicKey) {
        let (_signer, pk) = HybridSigner::generate();
        let node_id = NodeId::new(pk.node_id());
        (node_id, pk)
    }

    #[test]
    fn register_and_lookup_roundtrips() {
        let reg = ValidatorRegistry::new();
        let (id, pk) = validator();
        reg.register(id, pk.clone()).unwrap();
        assert!(reg.contains(&id));
        assert!(reg.is_active(&id));
        assert_eq!(reg.public_key(&id).unwrap().ed25519, pk.ed25519);
        assert_eq!(reg.active_count(), 1);
        assert_eq!(reg.weight(&id), 1);
    }

    #[test]
    fn identity_mismatch_is_rejected() {
        let reg = ValidatorRegistry::new();
        let (_id, pk) = validator();
        // Claim a wrong node id.
        let wrong = NodeId::new([0xAB; 32]);
        let r = reg.register(wrong, pk);
        assert!(matches!(r, Err(RegistryError::IdentityMismatch { .. })));
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn ed25519_only_key_is_rejected() {
        let reg = ValidatorRegistry::new();
        let (_signer, full) = HybridSigner::generate();
        let ed_only = HybridPublicKey::from_ed25519(full.ed25519);
        let id = NodeId::new(ed_only.node_id());
        let r = reg.register(id, ed_only);
        assert!(matches!(r, Err(RegistryError::MissingPostQuantumKey)));
    }

    #[test]
    fn deactivation_preserves_key_but_drops_from_quorum() {
        let reg = ValidatorRegistry::new();
        let (id, pk) = validator();
        reg.register(id, pk).unwrap();
        assert_eq!(reg.active_count(), 1);
        reg.set_active(&id, false);
        assert_eq!(reg.active_count(), 0);
        // Key still resolvable for historical verification.
        assert!(reg.public_key(&id).is_some());
        assert!(reg.contains(&id));
    }

    #[test]
    fn snapshot_roundtrips() {
        let reg = ValidatorRegistry::new();
        for _ in 0..5 {
            let (id, pk) = validator();
            reg.register(id, pk).unwrap();
        }
        let snap = reg.snapshot();
        assert_eq!(snap.validators.len(), 5);
        let reg2 = ValidatorRegistry::from_snapshot(&snap);
        assert_eq!(reg2.len(), 5);
        assert_eq!(reg2.active_count(), 5);
    }

    #[test]
    fn weighted_registration_tracks_total_weight() {
        let reg = ValidatorRegistry::new();
        let (id1, pk1) = validator();
        let (id2, pk2) = validator();
        reg.register_weighted(id1, pk1, 10).unwrap();
        reg.register_weighted(id2, pk2, 25).unwrap();
        assert_eq!(reg.total_active_weight(), 35);
    }
}
