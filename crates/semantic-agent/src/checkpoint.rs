//! Deterministic merkle root over the indexed knot set.
//!
//! ## Why merkle, not just a single hash?
//!
//! A simple `BLAKE3(concat(sorted_ids))` would already be deterministic
//! and unforgeable, but it has two operational problems:
//!
//! 1. It does not produce inclusion proofs, so a third party that
//!    suspects "agent claims to have indexed knot X but has not" cannot
//!    challenge the agent without re-indexing the entire chain.
//! 2. It conflates *order* and *membership*: the agent could anchor a
//!    perfectly correct concat-hash even with a few off-by-one errors
//!    in its index, because any subset of identity-equivalent knots
//!    hashes the same as the full set.
//!
//! A binary merkle tree over `entry.identity_digest()` solves both:
//! the root commits to the full membership, and any individual knot's
//! inclusion can be proved with O(log N) sibling hashes.
//!
//! The tree uses domain-separated leaf and internal hashes, per
//! [BIP340/Tagged-hashes-style] best practice, so a leaf hash can
//! never be confused with an internal hash even when the same digest
//! happens to appear in both positions.

use crate::config::{AgentConfig, AgentIdentity};
use crate::search::SearchService;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const LEAF_TAG: &[u8] = b"dcr-semantic-agent/merkle-leaf/v1";
const NODE_TAG: &[u8] = b"dcr-semantic-agent/merkle-node/v1";
const EMPTY_TAG: &[u8] = b"dcr-semantic-agent/merkle-empty/v1";

/// 32-byte BLAKE3 digest committing to the full sorted set of indexed
/// knots. `Default` is the well-known root of the empty tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MerkleRoot(#[serde(with = "hex_serde")] pub [u8; 32]);

impl MerkleRoot {
    pub fn empty() -> Self {
        Self(*blake3::hash(EMPTY_TAG).as_bytes())
    }

    pub fn to_hex(&self) -> String {
        format!("0x{}", hex::encode(self.0))
    }
}

impl Default for MerkleRoot {
    fn default() -> Self {
        Self::empty()
    }
}

mod hex_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("0x{}", hex::encode(bytes)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s: String = String::deserialize(d)?;
        let stripped = s.trim_start_matches("0x");
        let v = hex::decode(stripped).map_err(serde::de::Error::custom)?;
        if v.len() != 32 {
            return Err(serde::de::Error::custom(format!(
                "expected 32 bytes, got {}",
                v.len()
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&v);
        Ok(out)
    }
}

/// Compute the merkle root of a sorted list of `(knot_id, identity_digest)`
/// tuples. Pure function — no I/O. Caller is responsible for
/// pre-sorting (the search service does so for us).
pub fn merkle_root_of_identity_digests(sorted: &[(String, [u8; 32])]) -> MerkleRoot {
    if sorted.is_empty() {
        return MerkleRoot::empty();
    }
    // Leaf layer.
    let mut layer: Vec<[u8; 32]> = sorted
        .iter()
        .map(|(_, d)| {
            let mut h = blake3::Hasher::new();
            h.update(LEAF_TAG);
            h.update(d);
            *h.finalize().as_bytes()
        })
        .collect();
    // Reduce to root. Standard "pad-with-self" rule on odd layers.
    while layer.len() > 1 {
        let mut next: Vec<[u8; 32]> = Vec::with_capacity(layer.len() / 2 + 1);
        for chunk in layer.chunks(2) {
            let left = chunk[0];
            let right = if chunk.len() == 2 { chunk[1] } else { chunk[0] };
            let mut h = blake3::Hasher::new();
            h.update(NODE_TAG);
            h.update(&left);
            h.update(&right);
            next.push(*h.finalize().as_bytes());
        }
        layer = next;
    }
    MerkleRoot(layer[0])
}

/// On-chain testimony emitted every checkpoint cycle.
///
/// Field shape mirrors the `interaction.metadata` shape consumed by
/// `rope_appendToLedger` so the on-chain knot's metadata round-trips
/// 1:1 with this struct.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexCheckpointTestimony {
    /// Always `"IndexCheckpointTestimony/v1"`. Lets future versions of
    /// the agent (or other subscribers) discriminate testimony shapes.
    pub event_type: String,
    /// Canonical SemanticAgent ID (`"semantic"`).
    pub agent_id: String,
    /// Wallet hex (the agent's canonical wallet by default).
    pub agent_wallet: String,
    /// `MerkleRoot` over the indexed knot set, as 0x-prefixed hex.
    pub merkle_root: String,
    /// Total number of knots committed by `merkle_root`.
    pub total_indexed: u64,
    /// Largest `string_id` the agent has scanned at the time of the
    /// checkpoint. Operators use this to bisect reindex ranges.
    pub last_string_id: Option<String>,
    /// Schema version — bump on incompatible changes.
    pub schema_version: u32,
    /// Wall-clock checkpoint timestamp (Unix seconds).
    pub checkpoint_at: i64,
}

impl IndexCheckpointTestimony {
    /// Convert to the `interaction` JSON shape expected by
    /// `rope_appendToLedger`. The agent_id and merkle_root are echoed
    /// in the description for human-readable explorers.
    pub fn to_interaction(&self) -> serde_json::Value {
        serde_json::json!({
            "interaction_type": "TestimonySubmission",
            "description": format!(
                "{} merkle_root={} total_indexed={}",
                self.event_type, self.merkle_root, self.total_indexed
            ),
            "metadata": {
                "event_type": self.event_type,
                "agent_id": self.agent_id,
                "agent_wallet": self.agent_wallet,
                "merkle_root": self.merkle_root,
                "total_indexed": self.total_indexed.to_string(),
                "last_string_id": self.last_string_id.clone().unwrap_or_default(),
                "schema_version": self.schema_version.to_string(),
                "checkpoint_at": self.checkpoint_at.to_string(),
            }
        })
    }
}

/// Stateless builder that snapshots a [`SearchService`] and produces an
/// [`IndexCheckpointTestimony`]. Cheap to clone.
#[derive(Clone)]
pub struct CheckpointBuilder {
    config: Arc<AgentConfig>,
    search: Arc<SearchService>,
}

impl CheckpointBuilder {
    pub fn new(config: Arc<AgentConfig>, search: Arc<SearchService>) -> Self {
        Self { config, search }
    }

    pub fn identity(&self) -> &AgentIdentity {
        &self.config.identity
    }

    /// Build a fresh checkpoint reflecting the index state at the
    /// instant of the call. Pure read — does not mutate the index.
    pub fn build(
        &self,
        last_string_id: Option<String>,
    ) -> anyhow::Result<(IndexCheckpointTestimony, MerkleRoot)> {
        let snapshot = self.search.snapshot_identity_tuples()?;
        let total_indexed = snapshot.len() as u64;
        let root = merkle_root_of_identity_digests(&snapshot);
        let testimony = IndexCheckpointTestimony {
            event_type: "IndexCheckpointTestimony/v1".to_string(),
            agent_id: self.config.identity.agent_id.clone(),
            agent_wallet: self.config.identity.wallet.clone(),
            merkle_root: root.to_hex(),
            total_indexed,
            last_string_id,
            schema_version: 1,
            checkpoint_at: chrono::Utc::now().timestamp(),
        };
        Ok((testimony, root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KnotIndexEntry;

    fn entry(id: &str, kind: &str) -> KnotIndexEntry {
        KnotIndexEntry {
            knot_id: id.to_string(),
            string_id: "0xowner".into(),
            string_kind: kind.to_string(),
            event_type: "Transfer".to_string(),
            knot_index: 1,
            status: "active".to_string(),
            indexed_at: 0,
            knot_timestamp: 0,
            payload_text: String::new(),
            payload_size: 0,
        }
    }

    fn snap(entries: &[KnotIndexEntry]) -> Vec<(String, [u8; 32])> {
        let mut v: Vec<(String, [u8; 32])> = entries
            .iter()
            .map(|e| (e.knot_id.clone(), e.identity_digest()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    #[test]
    fn empty_root_matches_constant() {
        let r = merkle_root_of_identity_digests(&[]);
        assert_eq!(r, MerkleRoot::empty());
    }

    #[test]
    fn root_is_deterministic_across_calls() {
        let entries = [
            entry("0x01", "wallet"),
            entry("0x02", "wallet"),
            entry("0x03", "asset"),
        ];
        let s1 = snap(&entries);
        let s2 = snap(&entries);
        assert_eq!(
            merkle_root_of_identity_digests(&s1),
            merkle_root_of_identity_digests(&s2)
        );
    }

    #[test]
    fn root_changes_when_set_changes() {
        let s1 = snap(&[entry("0x01", "wallet"), entry("0x02", "wallet")]);
        let s2 = snap(&[entry("0x01", "wallet"), entry("0x99", "wallet")]);
        assert_ne!(
            merkle_root_of_identity_digests(&s1),
            merkle_root_of_identity_digests(&s2)
        );
    }

    #[test]
    fn root_is_order_invariant_after_sort() {
        let s1 = snap(&[entry("0x01", "wallet"), entry("0x02", "wallet")]);
        let s2 = snap(&[entry("0x02", "wallet"), entry("0x01", "wallet")]);
        assert_eq!(
            merkle_root_of_identity_digests(&s1),
            merkle_root_of_identity_digests(&s2)
        );
    }

    #[test]
    fn root_handles_odd_layers() {
        // 5 leaves → 3 internal → 2 → 1 (each odd layer pads with self)
        let entries: Vec<_> = (0..5)
            .map(|i| entry(&format!("0x0{i}"), "wallet"))
            .collect();
        let r = merkle_root_of_identity_digests(&snap(&entries));
        assert_ne!(r, MerkleRoot::empty());
    }

    #[test]
    fn checkpoint_builder_round_trips_through_interaction() {
        let testimony = IndexCheckpointTestimony {
            event_type: "IndexCheckpointTestimony/v1".into(),
            agent_id: "semantic".into(),
            agent_wallet: crate::CANONICAL_AGENT_WALLET.into(),
            merkle_root: "0x00".into(),
            total_indexed: 7,
            last_string_id: Some("0xabc".into()),
            schema_version: 1,
            checkpoint_at: 1_700_000_000,
        };
        let v = testimony.to_interaction();
        assert_eq!(v["interaction_type"], "TestimonySubmission");
        assert_eq!(v["metadata"]["agent_id"], "semantic");
        assert_eq!(v["metadata"]["total_indexed"], "7");
    }

    #[test]
    fn merkle_root_hex_serde_round_trip() {
        let r = MerkleRoot([0x42u8; 32]);
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("0x4242"));
        let back: MerkleRoot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }
}
