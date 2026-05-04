//! Knot representation used by the validation pipeline.
//!
//! Two flavours of knot land in the agent:
//!
//! 1. **Cord anchor knots** — the EVM-shaped knots produced by the
//!    Reth-backed cord. These come from `rope_getKnotByIndex`. Today
//!    they do not yet carry a `HybridSignature` (see Quipu Canon v2.0
//!    Phase 2 — real consensus turned on with batched signatures);
//!    the agent reports them as `skipped` until Phase 2 lands.
//!
//! 2. **Hybrid-signed knots** — knots that carry a real Ed25519 +
//!    Dilithium3 hybrid signature plus their creator's public key.
//!    These are produced by personal-ledger appends (in v2.0 Phase 2,
//!    by witness anchors). The verification code path is real and is
//!    exercised by the unit tests against synthesized payloads.
//!
//! The agent normalizes both into a single [`Knot`] envelope so the
//! verifier and witness modules don't need to know which RPC produced
//! the data.

use serde::{Deserialize, Serialize};

use rope_crypto::hybrid::{HybridPublicKey, HybridSignature};

/// A knot identifier — opaque hex string, prefixed `0x`, length
/// dependent on the underlying source. We do not constrain the length
/// here because both cord-anchor block hashes (32 bytes) and personal
/// ledger string ids (32 bytes) currently use the same encoding, but
/// future quipu canon revisions are free to lengthen them.
pub type KnotId = String;

/// Source layer that produced this knot. Used purely for logging /
/// metrics — the verification logic is identical for all sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnotSource {
    /// Cord anchor knot retrieved via `rope_getKnotByIndex`.
    CordAnchor,
    /// Per-entity (wallet / contract / asset / did) string knot
    /// retrieved via `rope_listStrings` + `rope_getStringWithKnots`.
    EntityString,
    /// Test fixture — only produced inside `#[cfg(test)]` paths.
    Test,
}

impl KnotSource {
    /// Lower-snake-case identifier used in logs and metrics.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CordAnchor => "cord_anchor",
            Self::EntityString => "entity_string",
            Self::Test => "test",
        }
    }
}

/// Normalized knot envelope fed to the verifier.
///
/// `signature` and `creator` are `None` when the underlying source did
/// not (or does not yet) populate them. The verifier interprets a
/// missing signature as `skipped` rather than `rejected` — see
/// [`crate::verify::VerificationOutcome`].
#[derive(Debug, Clone)]
pub struct Knot {
    /// Hex-encoded knot id (block hash for cord anchors, string id for
    /// entity strings).
    pub knot_id: KnotId,

    /// Lattice or chain index of this knot at the time we observed it.
    /// `0` for genesis.
    pub knot_index: u64,

    /// Where we read this knot from.
    pub source: KnotSource,

    /// Bytes that the creator signed. For cord anchor knots, this is
    /// the canonical anchor preimage (currently the block hash bytes
    /// when we have nothing better — see scope note in
    /// `verify::KnotVerifier::verify`). For RopeStrings, this would be
    /// the result of `RopeString::compute_signing_message()`.
    pub signing_message: Vec<u8>,

    /// The creator's hybrid public key, if available. `None` for
    /// EVM-shape anchors that do not yet expose it on the wire.
    pub creator: Option<HybridPublicKey>,

    /// The knot's hybrid signature, if available. `None` for EVM-shape
    /// anchors today.
    pub signature: Option<HybridSignature>,

    /// Wall-clock observation timestamp (Unix seconds). Used in
    /// testimony metadata.
    pub observed_at: i64,
}

impl Knot {
    /// Convenience constructor for tests and the witness module.
    pub fn new(
        knot_id: impl Into<String>,
        knot_index: u64,
        source: KnotSource,
        signing_message: Vec<u8>,
    ) -> Self {
        Self {
            knot_id: knot_id.into(),
            knot_index,
            source,
            signing_message,
            creator: None,
            signature: None,
            observed_at: chrono::Utc::now().timestamp(),
        }
    }

    /// Attach a creator public key and a hybrid signature.
    pub fn with_signature(mut self, creator: HybridPublicKey, signature: HybridSignature) -> Self {
        self.creator = Some(creator);
        self.signature = Some(signature);
        self
    }

    /// Returns `true` if this knot exposes both a creator public key
    /// AND a non-empty signature.
    pub fn has_signature_material(&self) -> bool {
        match (&self.creator, &self.signature) {
            (Some(_), Some(sig)) => !sig.is_empty(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knot_source_string_mapping_is_stable() {
        assert_eq!(KnotSource::CordAnchor.as_str(), "cord_anchor");
        assert_eq!(KnotSource::EntityString.as_str(), "entity_string");
        assert_eq!(KnotSource::Test.as_str(), "test");
    }

    #[test]
    fn fresh_knot_has_no_signature_material() {
        let k = Knot::new("0xabc", 1, KnotSource::Test, b"msg".to_vec());
        assert!(!k.has_signature_material());
        assert!(k.creator.is_none());
        assert!(k.signature.is_none());
    }

    #[test]
    fn with_signature_marks_material_present() {
        let (signer, pk) = rope_crypto::hybrid::HybridSigner::generate();
        let sig = signer.sign(b"msg");
        let k = Knot::new("0xabc", 1, KnotSource::Test, b"msg".to_vec()).with_signature(pk, sig);
        assert!(k.has_signature_material());
    }
}
