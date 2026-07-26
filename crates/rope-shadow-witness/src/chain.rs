//! Shadow chain core. Translates observed canonical knots into
//! §6.1.1 v2 chain entries.
//!
//! The transformation is deterministic from observable inputs, so any
//! independent witness operating against the same upstream rope-node
//! produces an identical v2 chain. This is the property that gives
//! the off-chain witness pattern its trust model: cross-witness
//! agreement is by construction, not by trust in any single operator.

use std::sync::Arc;

use chrono::Utc;
use tracing::{debug, info, warn};

use rope_core::knot_hash::{
    compute_event_metadata_hash, compute_knot_hash, tombstone_preimage, EventMetadata, KnotHash,
    KnotHashPreImage,
};

use crate::error::{ShadowWitnessError, ShadowWitnessResult};
use crate::store::{parse_string_id_hex, ShadowChainStore};
use crate::{ShadowChainEntry, ShadowChainHead};

/// One observed knot from the canonical chain.
///
/// Constructed by [`crate::client::RpcClient::get_string_with_knots`]
/// from the JSON-RPC response of `rope_getStringWithKnots`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedKnot {
    /// Canonical string identifier (hex `0x...` form).
    pub string_id: String,
    /// `knot_index` from the RPC response: the position of this knot
    /// in the string. Becomes `event_id` in the v2 chain.
    pub knot_index: u64,
    /// Whether this knot is a tombstone at the time of observation.
    pub is_tombstone: bool,
    /// `untied_at` UNIX seconds (only set when `is_tombstone == true`).
    pub tombstone_untied_at: Option<i64>,
    /// `audit_hash` hex (only set when `is_tombstone == true`).
    pub tombstone_audit_hash_hex: Option<String>,
    /// Reason text for the tombstone, if any.
    pub tombstone_reason: Option<String>,
}

/// The shadow chain. Holds an `Arc<ShadowChainStore>` and applies
/// observed knots and tombstones to the persistent v2 chain.
pub struct ShadowChain {
    store: Arc<ShadowChainStore>,
}

impl ShadowChain {
    pub fn new(store: Arc<ShadowChainStore>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &Arc<ShadowChainStore> {
        &self.store
    }

    /// Apply an observed knot to the v2 chain.
    ///
    /// The new entry's `knot_hash` chains over the predecessor's
    /// `knot_hash` from the heads CF, falling back to
    /// [`KnotHash::GENESIS`] if no predecessor is recorded yet.
    ///
    /// Returns `Ok(true)` if a new entry was written, `Ok(false)` if
    /// the entry was already present at this `event_id` (idempotent
    /// behaviour for repeated observations of the same canonical
    /// state).
    pub fn apply_observed(&self, observed: &ObservedKnot) -> ShadowWitnessResult<bool> {
        let string_id_bytes = parse_string_id_hex(&observed.string_id)?;

        if let Some(existing) = self.store.get_entry(&string_id_bytes, observed.knot_index)? {
            if existing.is_tombstone == observed.is_tombstone {
                debug!(
                    string_id = %observed.string_id,
                    event_id = observed.knot_index,
                    "shadow chain: entry already present, skipping"
                );
                return Ok(false);
            }
            if existing.is_tombstone {
                warn!(
                    string_id = %observed.string_id,
                    event_id = observed.knot_index,
                    "shadow chain: refusing to overwrite tombstone with active observation; \
                     this would only happen if the upstream walked back a tombstone, which the \
                     canonical Quipu Canon does not permit"
                );
                return Ok(false);
            }
            info!(
                string_id = %observed.string_id,
                event_id = observed.knot_index,
                "shadow chain: knot newly tombstoned; applying tombstone preimage"
            );
            return self.apply_tombstone(observed, &existing);
        }

        let head = self.store.get_head(&string_id_bytes)?;
        let previous_hash = match &head {
            Some(h) if h.latest_event_id + 1 == observed.knot_index => h.latest_knot_hash,
            Some(h) if h.latest_event_id >= observed.knot_index => {
                return Err(ShadowWitnessError::Internal(format!(
                    "out-of-order observation: head at event_id {} but observed event_id {}",
                    h.latest_event_id, observed.knot_index
                )));
            }
            Some(h) => {
                return Err(ShadowWitnessError::Internal(format!(
                    "gap in observation: head at event_id {} but observed event_id {} (expected {})",
                    h.latest_event_id, observed.knot_index, h.latest_event_id + 1
                )));
            }
            None if observed.knot_index == 0 => KnotHash::GENESIS,
            None => {
                return Err(ShadowWitnessError::Internal(format!(
                    "first observation of string {} but event_id is {} (expected 0)",
                    observed.string_id, observed.knot_index
                )));
            }
        };

        let event_type = if observed.is_tombstone {
            "erasure".to_string()
        } else {
            "append".to_string()
        };

        let metadata = build_event_metadata(observed);
        let event_metadata_hash = compute_event_metadata_hash(&metadata);

        let authorisation_proof = build_authorisation_proof(observed)?;

        let preimage = KnotHashPreImage {
            event_id: observed.knot_index,
            event_type: event_type.clone(),
            event_metadata_hash,
            authorisation_proof,
            previous_hash,
        };

        let knot_hash = compute_knot_hash(&preimage);

        let now = Utc::now().timestamp();
        let entry = ShadowChainEntry {
            string_id: observed.string_id.clone(),
            event_id: observed.knot_index,
            event_type,
            event_metadata_hash,
            knot_hash,
            previous_hash,
            is_tombstone: observed.is_tombstone,
            observed_at_unix: now,
        };
        let new_head = ShadowChainHead {
            latest_event_id: observed.knot_index,
            latest_knot_hash: knot_hash,
            updated_at_unix: now,
        };

        self.store
            .put_entry_and_advance_head(&string_id_bytes, &entry, &new_head)?;

        info!(
            string_id = %observed.string_id,
            event_id = observed.knot_index,
            event_type = %entry.event_type,
            knot_hash = %knot_hash,
            "shadow chain: tied new knot"
        );

        Ok(true)
    }

    /// Apply a tombstone observation in place of an existing active
    /// entry. Per §6.1.1 the tombstone is *recorded as a new knot
    /// alongside the chain*, but in the v0.1 RPC-poll model we
    /// observe the same `knot_index` flipping from active to
    /// tombstone; we therefore upgrade the existing entry's
    /// `is_tombstone` flag and emit a chained tombstone hash via
    /// [`tombstone_preimage`]. The original entry's `knot_hash` is
    /// retained as the chain anchor; the tombstone pre-image's hash is
    /// stored in a sibling field for audit, but the §6.1.1 chain head
    /// remains the original `knot_hash` to preserve continuity for
    /// successor knots.
    fn apply_tombstone(
        &self,
        observed: &ObservedKnot,
        existing: &ShadowChainEntry,
    ) -> ShadowWitnessResult<bool> {
        let string_id_bytes = parse_string_id_hex(&observed.string_id)?;

        let original_preimage = KnotHashPreImage {
            event_id: existing.event_id,
            event_type: existing.event_type.clone(),
            event_metadata_hash: existing.event_metadata_hash,
            authorisation_proof: Vec::new(),
            previous_hash: existing.previous_hash,
        };

        let erasure_proof = build_authorisation_proof(observed)?;
        let tombstone_pre = tombstone_preimage(&original_preimage, erasure_proof);
        let tombstone_hash = compute_knot_hash(&tombstone_pre);

        let now = Utc::now().timestamp();
        let updated = ShadowChainEntry {
            string_id: observed.string_id.clone(),
            event_id: existing.event_id,
            event_type: "erasure".to_string(),
            event_metadata_hash: existing.event_metadata_hash,
            knot_hash: tombstone_hash,
            previous_hash: existing.previous_hash,
            is_tombstone: true,
            observed_at_unix: now,
        };

        let head = self.store.get_head(&string_id_bytes)?;
        let preserved_head = match head {
            Some(h) if h.latest_event_id == existing.event_id => ShadowChainHead {
                latest_event_id: existing.event_id,
                latest_knot_hash: existing.knot_hash,
                updated_at_unix: now,
            },
            Some(h) => h,
            None => ShadowChainHead {
                latest_event_id: existing.event_id,
                latest_knot_hash: existing.knot_hash,
                updated_at_unix: now,
            },
        };

        self.store
            .put_entry_and_advance_head(&string_id_bytes, &updated, &preserved_head)?;

        info!(
            string_id = %observed.string_id,
            event_id = existing.event_id,
            tombstone_hash = %tombstone_hash,
            "shadow chain: tied tombstone for previously-active knot"
        );

        Ok(true)
    }
}

/// Build the v0.1 [`EventMetadata`] from an observed knot.
///
/// See `lib.rs` § "v0.1 fidelity scope" for the proxy mapping.
fn build_event_metadata(observed: &ObservedKnot) -> EventMetadata {
    let timestamp_bytes = match observed.tombstone_untied_at {
        Some(ts) => ts.to_be_bytes().to_vec(),
        None => Vec::new(),
    };
    EventMetadata {
        timestamp_bytes,
        witness_ids: Vec::new(),
        testimony_quorum: 0,
        oes_key_shred_destinations: Vec::new(),
    }
}

/// Build the v0.1 `authorisation_proof` from an observed knot.
fn build_authorisation_proof(observed: &ObservedKnot) -> ShadowWitnessResult<Vec<u8>> {
    if observed.is_tombstone {
        if let Some(ah) = observed.tombstone_audit_hash_hex.as_ref() {
            let stripped = ah.strip_prefix("0x").unwrap_or(ah);
            let bytes = hex::decode(stripped)?;
            return Ok(bytes);
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ShadowChainStore;

    fn fresh_chain() -> (tempfile::TempDir, ShadowChain) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ShadowChainStore::open(dir.path()).unwrap());
        (dir, ShadowChain::new(store))
    }

    fn fake_string_id() -> String {
        "0x".to_string() + &"ab".repeat(32)
    }

    fn observed_active(event_id: u64) -> ObservedKnot {
        ObservedKnot {
            string_id: fake_string_id(),
            knot_index: event_id,
            is_tombstone: false,
            tombstone_untied_at: None,
            tombstone_audit_hash_hex: None,
            tombstone_reason: None,
        }
    }

    fn observed_tombstone(event_id: u64) -> ObservedKnot {
        ObservedKnot {
            string_id: fake_string_id(),
            knot_index: event_id,
            is_tombstone: true,
            tombstone_untied_at: Some(1700000123),
            tombstone_audit_hash_hex: Some("0x".to_string() + &"cd".repeat(32)),
            tombstone_reason: Some("OwnerRequest".to_string()),
        }
    }

    #[test]
    fn first_observation_chains_over_genesis() {
        let (_d, chain) = fresh_chain();
        let added = chain.apply_observed(&observed_active(0)).unwrap();
        assert!(added);
        let id_bytes = parse_string_id_hex(&fake_string_id()).unwrap();
        let head = chain.store().get_head(&id_bytes).unwrap().unwrap();
        assert_eq!(head.latest_event_id, 0);
        let entry = chain.store().get_entry(&id_bytes, 0).unwrap().unwrap();
        assert_eq!(entry.previous_hash, KnotHash::GENESIS);
    }

    #[test]
    fn second_observation_chains_over_first() {
        let (_d, chain) = fresh_chain();
        chain.apply_observed(&observed_active(0)).unwrap();
        chain.apply_observed(&observed_active(1)).unwrap();
        let id_bytes = parse_string_id_hex(&fake_string_id()).unwrap();
        let e0 = chain.store().get_entry(&id_bytes, 0).unwrap().unwrap();
        let e1 = chain.store().get_entry(&id_bytes, 1).unwrap().unwrap();
        assert_eq!(e1.previous_hash, e0.knot_hash);
    }

    #[test]
    fn repeated_observation_is_idempotent() {
        let (_d, chain) = fresh_chain();
        let added1 = chain.apply_observed(&observed_active(0)).unwrap();
        let added2 = chain.apply_observed(&observed_active(0)).unwrap();
        assert!(added1);
        assert!(!added2);
    }

    #[test]
    fn tombstone_upgrade_preserves_chain_head() {
        let (_d, chain) = fresh_chain();
        chain.apply_observed(&observed_active(0)).unwrap();
        chain.apply_observed(&observed_active(1)).unwrap();
        chain.apply_observed(&observed_active(2)).unwrap();

        let id_bytes = parse_string_id_hex(&fake_string_id()).unwrap();
        let head_before = chain.store().get_head(&id_bytes).unwrap().unwrap();
        assert_eq!(head_before.latest_event_id, 2);
        let original_e2_hash = head_before.latest_knot_hash;

        let added = chain.apply_observed(&observed_tombstone(1)).unwrap();
        assert!(added);

        let head_after = chain.store().get_head(&id_bytes).unwrap().unwrap();
        assert_eq!(head_after.latest_event_id, 2);
        assert_eq!(head_after.latest_knot_hash, original_e2_hash);

        let e1_after = chain.store().get_entry(&id_bytes, 1).unwrap().unwrap();
        assert!(e1_after.is_tombstone);
        assert_eq!(e1_after.event_type, "erasure");
    }

    #[test]
    fn gap_in_observation_is_rejected() {
        let (_d, chain) = fresh_chain();
        chain.apply_observed(&observed_active(0)).unwrap();
        let err = chain.apply_observed(&observed_active(2)).unwrap_err();
        match err {
            ShadowWitnessError::Internal(_) => {}
            other => panic!("expected Internal error, got {:?}", other),
        }
    }

    #[test]
    fn deterministic_across_two_chains() {
        let (_d1, chain1) = fresh_chain();
        let (_d2, chain2) = fresh_chain();
        for i in 0..5 {
            chain1.apply_observed(&observed_active(i)).unwrap();
            chain2.apply_observed(&observed_active(i)).unwrap();
        }
        let id_bytes = parse_string_id_hex(&fake_string_id()).unwrap();
        for i in 0..5 {
            let e1 = chain1.store().get_entry(&id_bytes, i).unwrap().unwrap();
            let e2 = chain2.store().get_entry(&id_bytes, i).unwrap().unwrap();
            assert_eq!(e1.knot_hash, e2.knot_hash);
            assert_eq!(e1.event_metadata_hash, e2.event_metadata_hash);
        }
    }
}
