//! RopeString - The fundamental unit of information in Datachain Rope
//!
//! String S = (σ, τ, π, ρ, μ)
//!
//! Where:
//! - σ (Sigma): Sequence - ordered nucleotides comprising content
//! - τ (Tau): Temporal Marker - Lamport clock timestamp
//! - π (Pi): Parentage - parent StringIds forming DAG
//! - ρ (Rho): Replication Factor - redundancy level (default: 5)
//! - μ (Mu): Mutability Class - erasure policy

use crate::clock::LamportClock;
use crate::nucleotide::NucleotideSequence;
use crate::types::{constants, MutabilityClass, NodeId, StringId};
use serde::{Deserialize, Serialize};
use serde_bytes;

/// Hybrid signature combining classical and post-quantum algorithms
/// Ed25519 + CRYSTALS-Dilithium3
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HybridSignature {
    /// Ed25519 signature (64 bytes) - stored as Vec for serde compatibility
    #[serde(with = "serde_bytes")]
    pub ed25519_sig: Vec<u8>,

    /// CRYSTALS-Dilithium3 signature (~2420 bytes)
    #[serde(with = "serde_bytes")]
    pub dilithium_sig: Vec<u8>,
}

impl HybridSignature {
    /// Create a new hybrid signature
    pub fn new(ed25519_sig: [u8; 64], dilithium_sig: Vec<u8>) -> Self {
        Self {
            ed25519_sig: ed25519_sig.to_vec(),
            dilithium_sig,
        }
    }

    /// Create empty/placeholder signature (for unsigned strings)
    pub fn empty() -> Self {
        Self {
            ed25519_sig: vec![0u8; 64],
            dilithium_sig: Vec::new(),
        }
    }

    /// Check if signature is empty
    pub fn is_empty(&self) -> bool {
        self.ed25519_sig.iter().all(|&b| b == 0) && self.dilithium_sig.is_empty()
    }
}

impl Default for HybridSignature {
    fn default() -> Self {
        Self::empty()
    }
}

/// OES Proof - Demonstrates string was created by a synchronized node
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OESProof {
    /// OES generation epoch
    pub generation: u64,

    /// Commitment to OES state
    pub state_commitment: [u8; 32],

    /// Merkle proof of inclusion
    pub merkle_proof: Vec<[u8; 32]>,

    /// Dilithium signature over the proof
    pub signature: Vec<u8>,
}

impl OESProof {
    /// Create empty proof (for testing/genesis)
    pub fn empty() -> Self {
        Self {
            generation: 0,
            state_commitment: [0u8; 32],
            merkle_proof: Vec::new(),
            signature: Vec::new(),
        }
    }
}

impl Default for OESProof {
    fn default() -> Self {
        Self::empty()
    }
}

/// Public key for string creator
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey {
    /// Ed25519 public key (32 bytes)
    pub ed25519: [u8; 32],

    /// CRYSTALS-Dilithium3 public key (~1952 bytes)
    pub dilithium: Vec<u8>,
}

impl PublicKey {
    pub fn new(ed25519: [u8; 32], dilithium: Vec<u8>) -> Self {
        Self { ed25519, dilithium }
    }

    /// Create from just Ed25519 (for backward compatibility)
    pub fn from_ed25519(ed25519: [u8; 32]) -> Self {
        Self {
            ed25519,
            dilithium: Vec::new(),
        }
    }

    /// Derive NodeId from this public key
    pub fn to_node_id(&self) -> NodeId {
        NodeId::from_public_key(&self.ed25519)
    }
}

/// RopeString - The fundamental unit of information
///
/// Named `RopeString` to avoid conflict with std::string::String
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RopeString {
    /// Unique identifier (computed from content hash)
    id: StringId,

    /// σ - Sequence: ordered nucleotides comprising content
    sequence: NucleotideSequence,

    /// τ - Temporal Marker: Lamport clock timestamp
    temporal_marker: LamportClock,

    /// π - Parentage: parent StringIds (forms DAG)
    parentage: Vec<StringId>,

    /// ρ - Replication Factor: redundancy level
    replication_factor: u32,

    /// μ - Mutability Class: erasure policy
    mutability_class: MutabilityClass,

    /// OES generation epoch marker
    oes_generation: u64,

    /// OES proof of synchronized creation
    oes_proof: OESProof,

    /// Hybrid quantum-resistant signature
    signature: HybridSignature,

    /// Creating node's public key
    creator: PublicKey,
}

impl RopeString {
    /// Create a new string builder
    pub fn builder() -> RopeStringBuilder {
        RopeStringBuilder::new()
    }

    /// Get the string ID
    pub fn id(&self) -> StringId {
        self.id
    }

    /// Get the content sequence (σ)
    pub fn sequence(&self) -> &NucleotideSequence {
        &self.sequence
    }

    /// Get raw content bytes
    pub fn content(&self) -> Vec<u8> {
        self.sequence.to_raw_bytes()
    }

    /// Get temporal marker (τ)
    pub fn temporal_marker(&self) -> &LamportClock {
        &self.temporal_marker
    }

    /// Get parentage (π)
    pub fn parentage(&self) -> &[StringId] {
        &self.parentage
    }

    /// Get replication factor (ρ)
    pub fn replication_factor(&self) -> u32 {
        self.replication_factor
    }

    /// Get mutability class (μ)
    pub fn mutability_class(&self) -> &MutabilityClass {
        &self.mutability_class
    }

    /// Get OES generation
    pub fn oes_generation(&self) -> u64 {
        self.oes_generation
    }

    /// Get OES proof
    pub fn oes_proof(&self) -> &OESProof {
        &self.oes_proof
    }

    /// Get signature
    pub fn signature(&self) -> &HybridSignature {
        &self.signature
    }

    /// Get creator's public key
    pub fn creator(&self) -> &PublicKey {
        &self.creator
    }

    /// Check if string is an anchor string
    pub fn is_anchor(&self) -> bool {
        // Anchor strings have special marker in mutability class
        matches!(self.mutability_class, MutabilityClass::Immutable) && !self.sequence.is_empty()
    }

    /// Compute the signing message (for signature verification)
    pub fn compute_signing_message(&self) -> Vec<u8> {
        let mut message = Vec::new();
        message.extend_from_slice(&self.sequence.to_bytes());
        message.extend_from_slice(&self.temporal_marker.to_bytes());
        for parent in &self.parentage {
            message.extend_from_slice(parent.as_bytes());
        }
        message.extend_from_slice(&self.replication_factor.to_be_bytes());
        // Mutability class would need serialization
        message.extend_from_slice(&self.oes_generation.to_be_bytes());
        message
    }

    /// Compute StringId from content
    fn compute_id(
        sequence: &NucleotideSequence,
        temporal_marker: &LamportClock,
        parentage: &[StringId],
        replication_factor: u32,
        mutability_class: &MutabilityClass,
    ) -> StringId {
        let mut content = Vec::new();
        content.extend_from_slice(&sequence.to_bytes());
        content.extend_from_slice(&temporal_marker.to_bytes());
        for parent in parentage {
            content.extend_from_slice(parent.as_bytes());
        }
        content.extend_from_slice(&replication_factor.to_be_bytes());
        // Add mutability class discriminant
        let mc_byte = match mutability_class {
            MutabilityClass::Immutable => 0u8,
            MutabilityClass::OwnerErasable => 1u8,
            MutabilityClass::TimeBound(_) => 2u8,
            MutabilityClass::ConditionalErasure(_) => 3u8,
            MutabilityClass::GDPRCompliant => 4u8,
        };
        content.push(mc_byte);

        StringId::from_content(&content)
    }

    /// Verify the sequence integrity
    pub fn verify_sequence(&self) -> bool {
        self.sequence.verify_all()
    }

    /// Get content size in bytes
    pub fn size(&self) -> usize {
        self.sequence.len() * 32
    }
}

/// Builder pattern for constructing RopeStrings
pub struct RopeStringBuilder {
    content: Option<Vec<u8>>,
    temporal_marker: Option<LamportClock>,
    parentage: Vec<StringId>,
    replication_factor: u32,
    mutability_class: MutabilityClass,
    oes_generation: u64,
    oes_proof: OESProof,
    signature: HybridSignature,
    creator: Option<PublicKey>,
}

impl RopeStringBuilder {
    pub fn new() -> Self {
        Self {
            content: None,
            temporal_marker: None,
            parentage: Vec::new(),
            replication_factor: constants::DEFAULT_REPLICATION_FACTOR,
            mutability_class: MutabilityClass::default(),
            oes_generation: 0,
            oes_proof: OESProof::empty(),
            signature: HybridSignature::empty(),
            creator: None,
        }
    }

    pub fn content(mut self, content: impl Into<Vec<u8>>) -> Self {
        self.content = Some(content.into());
        self
    }

    pub fn temporal_marker(mut self, marker: LamportClock) -> Self {
        self.temporal_marker = Some(marker);
        self
    }

    pub fn parentage(mut self, parents: Vec<StringId>) -> Self {
        self.parentage = parents;
        self
    }

    pub fn add_parent(mut self, parent: StringId) -> Self {
        self.parentage.push(parent);
        self
    }

    pub fn replication_factor(mut self, factor: u32) -> Self {
        self.replication_factor = factor.clamp(3, 10);
        self
    }

    pub fn mutability_class(mut self, class: MutabilityClass) -> Self {
        self.mutability_class = class;
        self
    }

    pub fn oes_generation(mut self, gen: u64) -> Self {
        self.oes_generation = gen;
        self
    }

    pub fn oes_proof(mut self, proof: OESProof) -> Self {
        self.oes_proof = proof;
        self
    }

    pub fn signature(mut self, sig: HybridSignature) -> Self {
        self.signature = sig;
        self
    }

    pub fn creator(mut self, creator: PublicKey) -> Self {
        self.creator = Some(creator);
        self
    }

    pub fn build(self) -> Result<RopeString, &'static str> {
        let content = self.content.ok_or("Content is required")?;
        let temporal_marker = self.temporal_marker.ok_or("Temporal marker is required")?;
        let creator = self.creator.ok_or("Creator is required")?;

        if content.len() > constants::MAX_STRING_SIZE {
            return Err("Content exceeds maximum size");
        }

        let sequence = NucleotideSequence::from_bytes(&content);

        let id = RopeString::compute_id(
            &sequence,
            &temporal_marker,
            &self.parentage,
            self.replication_factor,
            &self.mutability_class,
        );

        Ok(RopeString {
            id,
            sequence,
            temporal_marker,
            parentage: self.parentage,
            replication_factor: self.replication_factor,
            mutability_class: self.mutability_class,
            oes_generation: self.oes_generation,
            oes_proof: self.oes_proof,
            signature: self.signature,
            creator,
        })
    }
}

impl Default for RopeStringBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NodeId;

    fn make_test_creator() -> PublicKey {
        PublicKey::from_ed25519([0u8; 32])
    }

    fn make_test_clock() -> LamportClock {
        LamportClock::new(NodeId::new([0u8; 32]))
    }

    #[test]
    fn test_string_builder() {
        let string = RopeString::builder()
            .content(b"Hello, Datachain Rope!".to_vec())
            .temporal_marker(make_test_clock())
            .creator(make_test_creator())
            .build()
            .expect("Failed to build string");

        assert!(!string.id().as_bytes().iter().all(|&b| b == 0));
        assert_eq!(
            string.replication_factor(),
            constants::DEFAULT_REPLICATION_FACTOR
        );
        assert!(string.parentage().is_empty());
    }

    #[test]
    fn test_string_with_parents() {
        let parent_id = StringId::from_content(b"parent");

        let string = RopeString::builder()
            .content(b"Child string".to_vec())
            .temporal_marker(make_test_clock())
            .creator(make_test_creator())
            .add_parent(parent_id)
            .build()
            .expect("Failed to build string");

        assert_eq!(string.parentage().len(), 1);
        assert_eq!(string.parentage()[0], parent_id);
    }

    #[test]
    fn test_string_id_deterministic() {
        let clock = make_test_clock();
        let creator = make_test_creator();

        let string1 = RopeString::builder()
            .content(b"Same content".to_vec())
            .temporal_marker(clock.clone())
            .creator(creator.clone())
            .build()
            .unwrap();

        let string2 = RopeString::builder()
            .content(b"Same content".to_vec())
            .temporal_marker(clock)
            .creator(creator)
            .build()
            .unwrap();

        assert_eq!(string1.id(), string2.id());
    }

    #[test]
    fn test_string_sequence_verification() {
        let string = RopeString::builder()
            .content(b"Test content for verification".to_vec())
            .temporal_marker(make_test_clock())
            .creator(make_test_creator())
            .build()
            .unwrap();

        assert!(string.verify_sequence());
    }
}
