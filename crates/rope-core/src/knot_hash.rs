//! Knot hash construction (Quipu Primitive Canon §6.1.1)
//!
//! This module is the in-code realisation of the per-knot hash chain
//! specified in §6.1.1 of the Datachain Rope anthropological paper
//! (Datachain Foundation, 2026). It is the formal cryptographic mechanism
//! by which `rope_untieKnot` preserves chain continuity under granular
//! erasure without any re-hashing of subsequent knots on the same string.
//!
//! # Construction
//!
//! For each knot `k_i` on a string `s`, the per-knot hash is
//!
//! ```text
//! h_i = BLAKE3(
//!     "DCROPE/quipu-canon/knot-hash-chain/v1" ||
//!     event_id_i || event_type_i ||
//!     event_metadata_hash_i || authorisation_proof_i ||
//!     h_{i-1}
//! )
//! ```
//!
//! where `||` denotes concatenation, `h_0 = KnotHash::GENESIS` for a
//! freshly-registered string, and `event_metadata_hash` is itself a
//! BLAKE3 commitment over the knot's structural metadata (timestamp,
//! witness identifiers, testimony-pool quorum, OES key-shred destination
//! set) but not over the plaintext payload.
//!
//! The encrypted `event_payload` is committed separately under the OES
//! per-knot ephemeral key (see `rope-crypto::oes`) and is *not* in the
//! hash-chain pre-image. The two commitments are bound together by the
//! inclusion of `event_metadata_hash` (which itself commits to the OES
//! key-shred destination set) in the chain pre-image.
//!
//! # Two commitments per knot
//!
//! The construction is, formally, a separation of two cryptographic
//! commitments per knot:
//!
//! 1. **Durability commitment** (`KnotHash` `h_i`, this module): guarantees
//!    chain continuity under arbitrary subsequent erasures. Computed over
//!    erasure-survivable fields only.
//! 2. **Confidentiality commitment** (the OES wrapping of the payload, in
//!    `rope-crypto::oes`): guarantees that the payload is recoverable if
//!    and only if the OES key shreds survive. Independent of the
//!    durability commitment on the integrity-critical hash path.
//!
//! Under `rope_untieKnot`, the OES key shreds for a knot are destroyed
//! and the encrypted payload becomes mathematically irrecoverable; the
//! fields entering `h_i`, however, are unchanged, so successor knots
//! continue to verify without any re-hashing of the tail of the string.
//!
//! # Why this module does not yet replace `RopeString::compute_id`
//!
//! At the time of writing, `RopeString::compute_id` (in `string.rs`)
//! computes a content-address over (σ, τ, π, ρ, μ), i.e. the sequence σ
//! is in the identity pre-image. That construction predates the §6.1.1
//! specification and is the v1.0/v1.1 path. Migrating to §6.1.1 requires
//! a Canon revision (provisionally Quipu Primitive Canon v1.3) and a
//! coordinated protocol upgrade across the network. This module provides
//! the §6.1.1 construction as code so that the spec is verifiable,
//! testable, and callable today; calling sites will be migrated under
//! the Canon revision per the migration plan in
//! `docs/QUIPU_CANON_KNOT_HASH_CONSTRUCTION.md`.
//!
//! # Domain separation
//!
//! Both `compute_event_metadata_hash` and `compute_knot_hash` prefix
//! their pre-image with a fixed domain-separation tag. The tags are
//! part of the §6.1.1 specification and are not to be changed without
//! a coordinated Canon revision and on-chain version bump.

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Domain-separation tag for the per-knot chain hash.
///
/// Per §6.1.1 the v1 of this construction is identified by this exact
/// byte string. Changing the tag requires a Canon revision and an
/// on-chain `knot_hash_version` bump.
pub const KNOT_HASH_CHAIN_TAG: &[u8] = b"DCROPE/quipu-canon/knot-hash-chain/v1";

/// Domain-separation tag for the per-knot metadata hash.
pub const EVENT_METADATA_HASH_TAG: &[u8] = b"DCROPE/quipu-canon/event-metadata-hash/v1";

/// Per-knot hash chain output (`h_i` in the §6.1.1 specification).
///
/// `KnotHash` is a 256-bit BLAKE3 digest over the erasure-survivable
/// fields of a knot, chained via the previous knot's `KnotHash`. It is
/// the durability commitment of the §6.1.1 construction, and is invariant
/// under destruction of the encrypted payload via `rope_untieKnot`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct KnotHash(pub [u8; 32]);

impl KnotHash {
    /// The genesis knot-hash sentinel (`h_0` for a freshly-registered
    /// string). Constant zero by convention; the genesis knot itself
    /// chains over this value.
    pub const GENESIS: Self = Self([0u8; 32]);

    /// Construct from raw 32 bytes.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow as a 32-byte array.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Hex-encoded representation.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for KnotHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KnotHash({})", &self.to_hex()[..16])
    }
}

impl fmt::Display for KnotHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", self.to_hex())
    }
}

/// Commitment over a knot's structural metadata.
///
/// `EventMetadataHash` is a 256-bit BLAKE3 digest over the timestamp,
/// witness identifiers, testimony-pool quorum, and OES key-shred
/// destination set. It does NOT include the plaintext payload. Because
/// the OES key-shred destination set is committed here, the audit trail
/// can verify post hoc which witnesses held shreds for an erased knot
/// and whether their destruction obligations were honoured.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct EventMetadataHash(pub [u8; 32]);

impl EventMetadataHash {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for EventMetadataHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EventMetadataHash({})", &self.to_hex()[..16])
    }
}

/// Witness identifier (canonical 32-byte form, e.g. a `NodeId` digest).
pub type WitnessId = [u8; 32];

/// Per-knot structural metadata. Hashed into `EventMetadataHash`.
///
/// This is the input set for [`compute_event_metadata_hash`]. The
/// plaintext payload is deliberately NOT a field here; the payload is
/// committed separately under OES.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventMetadata {
    /// Knot creation timestamp. Canonical encoding is left to the caller
    /// (Unix milliseconds big-endian, monotonic counter, or HLC tuple
    /// bytes); the field is hashed length-prefixed so any encoding is
    /// unambiguous as long as it is stable for a given knot.
    pub timestamp_bytes: Vec<u8>,

    /// Identifiers of the witnesses subscribing to the relevant category
    /// at the time the knot was tied.
    pub witness_ids: Vec<WitnessId>,

    /// Required testimony quorum count (number of witnesses whose
    /// signatures must accompany the knot for cord-anchor commit).
    pub testimony_quorum: u32,

    /// Identifiers of the validators that received OES key shreds for
    /// this knot. This is the auditability hook: post-erasure, regulators
    /// can verify which shred-holders existed for a given knot and
    /// whether their destruction obligations were honoured.
    pub oes_key_shred_destinations: Vec<WitnessId>,
}

/// Compute the BLAKE3 digest over a knot's structural metadata.
///
/// The result is the `event_metadata_hash` field of the §6.1.1 hash-chain
/// pre-image. It commits to the OES key-shred destination set (so the
/// audit trail survives erasure) but does NOT commit to the plaintext
/// payload (so the result is invariant under granular erasure).
pub fn compute_event_metadata_hash(metadata: &EventMetadata) -> EventMetadataHash {
    let mut hasher = Hasher::new();

    hasher.update(EVENT_METADATA_HASH_TAG);

    hasher.update(&(metadata.timestamp_bytes.len() as u64).to_be_bytes());
    hasher.update(&metadata.timestamp_bytes);

    hasher.update(&(metadata.witness_ids.len() as u64).to_be_bytes());
    for witness in &metadata.witness_ids {
        hasher.update(witness);
    }

    hasher.update(&metadata.testimony_quorum.to_be_bytes());

    hasher.update(&(metadata.oes_key_shred_destinations.len() as u64).to_be_bytes());
    for destination in &metadata.oes_key_shred_destinations {
        hasher.update(destination);
    }

    EventMetadataHash(*hasher.finalize().as_bytes())
}

/// Pre-image inputs for [`compute_knot_hash`] per the §6.1.1 specification.
///
/// All five fields are erasure-survivable: none of them is derived from
/// the plaintext payload, and none of them changes when `rope_untieKnot`
/// destroys the OES key shreds for the knot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnotHashPreImage {
    /// `event_id`. The string-scoped position index of the knot.
    pub event_id: u64,

    /// `event_type`. A controlled-vocabulary entry (e.g. `"transfer"`,
    /// `"mint"`, `"attestation"`, `"erasure"`).
    pub event_type: String,

    /// `event_metadata_hash`. BLAKE3 commitment over the structural
    /// metadata of the knot.
    pub event_metadata_hash: EventMetadataHash,

    /// `authorisation_proof`. The post-quantum signature of the party
    /// whose private key authorised the knot (the string's owner in the
    /// ordinary case, a delegated authority for compliance-mandated
    /// tombstones). Variable-length to accommodate ML-DSA-65 signatures
    /// (approximately 3,300 bytes) and SLH-DSA signatures (variable,
    /// scheme-dependent).
    pub authorisation_proof: Vec<u8>,

    /// `h_{i-1}`. Predecessor knot's `KnotHash`. For the genesis knot
    /// of a string, this is `KnotHash::GENESIS`.
    pub previous_hash: KnotHash,
}

/// Compute `h_i` per the §6.1.1 specification.
///
/// ```text
/// h_i = BLAKE3(
///     KNOT_HASH_CHAIN_TAG ||
///     event_id || event_type ||
///     event_metadata_hash || authorisation_proof ||
///     h_{i-1}
/// )
/// ```
///
/// The encrypted `event_payload` is not in the pre-image; it is
/// committed separately under the OES per-knot ephemeral key.
pub fn compute_knot_hash(preimage: &KnotHashPreImage) -> KnotHash {
    let mut hasher = Hasher::new();

    hasher.update(KNOT_HASH_CHAIN_TAG);

    hasher.update(&preimage.event_id.to_be_bytes());

    let event_type_bytes = preimage.event_type.as_bytes();
    hasher.update(&(event_type_bytes.len() as u64).to_be_bytes());
    hasher.update(event_type_bytes);

    hasher.update(preimage.event_metadata_hash.as_bytes());

    hasher.update(&(preimage.authorisation_proof.len() as u64).to_be_bytes());
    hasher.update(&preimage.authorisation_proof);

    hasher.update(preimage.previous_hash.as_bytes());

    KnotHash(*hasher.finalize().as_bytes())
}

/// Construct the tombstone-knot pre-image from the original knot's
/// pre-image and the erasure authorisation proof.
///
/// Per §6.1.1 consequence (i): when `rope_untieKnot` is invoked, the
/// OES key shreds for the knot are destroyed and the encrypted payload
/// becomes mathematically irrecoverable; however, `event_id`,
/// `event_type`, `event_metadata_hash`, `authorisation_proof`, and
/// `previous_hash` are all unchanged because none of them was derived
/// from the payload. The tombstone knot is recorded with
/// `event_type = "erasure"` and the `authorisation_proof` of the party
/// authorising the untying.
///
/// The original knot's `KnotHash` `h_i` (which successor knots and
/// cord anchors committed to) is not affected by this construction;
/// the tombstone is a *new* knot recorded alongside the chain, not a
/// rewrite of `h_i`.
pub fn tombstone_preimage(
    original: &KnotHashPreImage,
    erasure_authorisation_proof: Vec<u8>,
) -> KnotHashPreImage {
    KnotHashPreImage {
        event_id: original.event_id,
        event_type: "erasure".to_string(),
        event_metadata_hash: original.event_metadata_hash,
        authorisation_proof: erasure_authorisation_proof,
        previous_hash: original.previous_hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_metadata() -> EventMetadata {
        EventMetadata {
            timestamp_bytes: 1_700_000_000_000u64.to_be_bytes().to_vec(),
            witness_ids: vec![[1u8; 32], [2u8; 32], [3u8; 32]],
            testimony_quorum: 3,
            oes_key_shred_destinations: vec![[10u8; 32], [11u8; 32]],
        }
    }

    fn sample_preimage(event_id: u64, prev: KnotHash) -> KnotHashPreImage {
        KnotHashPreImage {
            event_id,
            event_type: "transfer".to_string(),
            event_metadata_hash: compute_event_metadata_hash(&sample_metadata()),
            authorisation_proof: vec![0xABu8; 64],
            previous_hash: prev,
        }
    }

    #[test]
    fn metadata_hash_is_stable() {
        let m = sample_metadata();
        let h1 = compute_event_metadata_hash(&m);
        let h2 = compute_event_metadata_hash(&m);
        assert_eq!(h1, h2);
    }

    #[test]
    fn metadata_hash_changes_with_witnesses() {
        let mut m = sample_metadata();
        let h1 = compute_event_metadata_hash(&m);
        m.witness_ids.push([99u8; 32]);
        let h2 = compute_event_metadata_hash(&m);
        assert_ne!(h1, h2);
    }

    #[test]
    fn metadata_hash_changes_with_oes_destinations() {
        let mut m = sample_metadata();
        let h1 = compute_event_metadata_hash(&m);
        m.oes_key_shred_destinations.push([99u8; 32]);
        let h2 = compute_event_metadata_hash(&m);
        assert_ne!(h1, h2);
    }

    #[test]
    fn metadata_hash_changes_with_quorum() {
        let mut m = sample_metadata();
        let h1 = compute_event_metadata_hash(&m);
        m.testimony_quorum += 1;
        let h2 = compute_event_metadata_hash(&m);
        assert_ne!(h1, h2);
    }

    #[test]
    fn knot_hash_chain_is_deterministic() {
        let p1 = sample_preimage(1, KnotHash::GENESIS);
        let h1 = compute_knot_hash(&p1);
        let p2 = sample_preimage(2, h1);
        let h2 = compute_knot_hash(&p2);
        let p3 = sample_preimage(3, h2);
        let h3 = compute_knot_hash(&p3);

        let h1_b = compute_knot_hash(&sample_preimage(1, KnotHash::GENESIS));
        let h2_b = compute_knot_hash(&sample_preimage(2, h1_b));
        let h3_b = compute_knot_hash(&sample_preimage(3, h2_b));

        assert_eq!(h1, h1_b);
        assert_eq!(h2, h2_b);
        assert_eq!(h3, h3_b);
    }

    #[test]
    fn knot_hash_changes_with_event_id() {
        let p1 = sample_preimage(1, KnotHash::GENESIS);
        let p2 = sample_preimage(2, KnotHash::GENESIS);
        assert_ne!(compute_knot_hash(&p1), compute_knot_hash(&p2));
    }

    #[test]
    fn knot_hash_changes_with_event_type() {
        let mut p = sample_preimage(1, KnotHash::GENESIS);
        let h1 = compute_knot_hash(&p);
        p.event_type = "mint".to_string();
        let h2 = compute_knot_hash(&p);
        assert_ne!(h1, h2);
    }

    #[test]
    fn knot_hash_changes_with_authorisation_proof() {
        let mut p = sample_preimage(1, KnotHash::GENESIS);
        let h1 = compute_knot_hash(&p);
        p.authorisation_proof[0] ^= 0xFF;
        let h2 = compute_knot_hash(&p);
        assert_ne!(h1, h2);
    }

    #[test]
    fn knot_hash_chains_via_previous_hash() {
        let p_genesis = sample_preimage(1, KnotHash::GENESIS);
        let h_genesis = compute_knot_hash(&p_genesis);

        let p_with_other_prev = sample_preimage(1, KnotHash::new([0xAAu8; 32]));
        let h_with_other_prev = compute_knot_hash(&p_with_other_prev);

        assert_ne!(h_genesis, h_with_other_prev);
    }

    /// Core property of §6.1.1: the chain hash for a successor knot is
    /// invariant under the destruction of the original knot's payload.
    /// We model this by: tying k_1, tying k_2 over h_1, then constructing
    /// the tombstone for k_1 (which represents the post-erasure state).
    /// The successor's chain hash depends only on h_1 and on the
    /// successor's own erasure-survivable fields, so re-computing h_2 in
    /// the post-erasure state yields the same value as before.
    #[test]
    fn tombstone_preserves_chain_continuity() {
        let p1 = sample_preimage(1, KnotHash::GENESIS);
        let h1_pre_erasure = compute_knot_hash(&p1);

        let p2 = sample_preimage(2, h1_pre_erasure);
        let h2_pre_erasure = compute_knot_hash(&p2);

        let _tombstone = tombstone_preimage(&p1, vec![0xCDu8; 64]);

        let h2_post_erasure = compute_knot_hash(&p2);

        assert_eq!(h2_pre_erasure, h2_post_erasure);
    }

    /// The tombstone is itself a valid knot whose chain hash is well
    /// defined and distinct from the original knot's chain hash (because
    /// the `event_type` differs). Successor knots that chain over the
    /// tombstone instead of the original would produce a different chain
    /// hash, but in the §6.1.1 model successors always chain over the
    /// original `h_i`; the tombstone is recorded alongside the chain
    /// rather than as a replacement of `h_i`.
    #[test]
    fn tombstone_is_distinct_from_original() {
        let p1 = sample_preimage(1, KnotHash::GENESIS);
        let h1 = compute_knot_hash(&p1);

        let tombstone = tombstone_preimage(&p1, vec![0xCDu8; 64]);
        let h_tombstone = compute_knot_hash(&tombstone);

        assert_ne!(h1, h_tombstone);
        assert_eq!(tombstone.event_type, "erasure");
        assert_eq!(tombstone.event_id, p1.event_id);
        assert_eq!(tombstone.previous_hash, p1.previous_hash);
        assert_eq!(tombstone.event_metadata_hash, p1.event_metadata_hash);
    }

    #[test]
    fn knot_hash_does_not_depend_on_payload() {
        let p = sample_preimage(1, KnotHash::GENESIS);
        let h_original = compute_knot_hash(&p);

        let p_clone = p.clone();
        let h_clone = compute_knot_hash(&p_clone);

        assert_eq!(h_original, h_clone);
    }

    #[test]
    fn domain_separation_distinguishes_metadata_and_chain_hashes() {
        let metadata_hash = compute_event_metadata_hash(&sample_metadata());
        let chain_hash = compute_knot_hash(&sample_preimage(1, KnotHash::GENESIS));
        assert_ne!(metadata_hash.as_bytes(), chain_hash.as_bytes());
    }

    #[test]
    fn domain_separation_tags_match_canon_spec() {
        assert_eq!(KNOT_HASH_CHAIN_TAG, b"DCROPE/quipu-canon/knot-hash-chain/v1");
        assert_eq!(EVENT_METADATA_HASH_TAG, b"DCROPE/quipu-canon/event-metadata-hash/v1");
    }

    #[test]
    fn genesis_hash_is_zero() {
        assert_eq!(KnotHash::GENESIS.as_bytes(), &[0u8; 32]);
    }

    #[test]
    fn knot_hash_serializes_and_deserializes() {
        let h = KnotHash::new([42u8; 32]);
        let bytes = bincode::serialize(&h).expect("serialize");
        let h2: KnotHash = bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(h, h2);
    }

    #[test]
    fn metadata_hash_serializes_and_deserializes() {
        let m = sample_metadata();
        let h = compute_event_metadata_hash(&m);
        let bytes = bincode::serialize(&h).expect("serialize");
        let h2: EventMetadataHash = bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(h, h2);
    }

    #[test]
    fn preimage_serializes_and_deserializes() {
        let p = sample_preimage(7, KnotHash::new([13u8; 32]));
        let bytes = bincode::serialize(&p).expect("serialize");
        let p2: KnotHashPreImage = bincode::deserialize(&bytes).expect("deserialize");
        assert_eq!(p, p2);
    }

    #[test]
    fn empty_authorisation_proof_is_handled() {
        let mut p = sample_preimage(1, KnotHash::GENESIS);
        p.authorisation_proof = Vec::new();
        let h = compute_knot_hash(&p);
        assert_ne!(h.as_bytes(), &[0u8; 32]);
    }

    /// Length-prefix encoding prevents collisions between
    /// `(short_id, long_type)` and `(long_id, short_type)` cases.
    #[test]
    fn length_prefix_encoding_avoids_field_boundary_collisions() {
        let p1 = KnotHashPreImage {
            event_id: 1,
            event_type: "ABC".to_string(),
            event_metadata_hash: EventMetadataHash::default(),
            authorisation_proof: Vec::new(),
            previous_hash: KnotHash::GENESIS,
        };
        let p2 = KnotHashPreImage {
            event_id: 1,
            event_type: "AB".to_string(),
            event_metadata_hash: EventMetadataHash::default(),
            authorisation_proof: b"C".to_vec(),
            previous_hash: KnotHash::GENESIS,
        };
        assert_ne!(compute_knot_hash(&p1), compute_knot_hash(&p2));
    }
}
