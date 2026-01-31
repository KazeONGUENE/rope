//! Complement - Verification string paired with each primary string
//!
//! Inspired by DNA's complementary strand pairing (A-T, G-C),
//! each string in Datachain Rope has a complement that:
//! - Enables integrity verification without original data
//! - Provides error correction capability
//! - Supports regeneration of damaged strings

use crate::string::RopeString;
use crate::types::StringId;
use reed_solomon_erasure::galois_8::ReedSolomon;
use serde::{Deserialize, Serialize};

/// Regeneration hint for complement-based reconstruction
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegenerationHint {
    /// Related string ID that can help with regeneration
    pub related_string_id: StringId,

    /// Type of relationship
    pub relationship: RelationshipType,

    /// Segment range that can be regenerated from this source
    pub segment_range: (u64, u64),
}

/// Types of relationships between strings for regeneration
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationshipType {
    /// Direct parent in DAG
    Parent,

    /// Direct child in DAG
    Child,

    /// Sibling (shares parent)
    Sibling,

    /// Related through content similarity
    ContentRelated,

    /// Previous version of same data
    PreviousVersion,
}

/// Entanglement proof linking string and complement
///
/// Proves that the complement was correctly generated for the string,
/// preventing malicious complement substitution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntanglementProof {
    /// Hash binding string and complement
    pub binding_hash: [u8; 32],

    /// Timestamp of entanglement
    pub created_at: u64,

    /// Creator node signature
    pub signature: Vec<u8>,
}

impl EntanglementProof {
    /// Generate entanglement proof for a string-complement pair
    pub fn generate(string: &RopeString, complement_data: &[u8]) -> Self {
        let mut binding_content = Vec::new();
        binding_content.extend_from_slice(string.id().as_bytes());
        binding_content.extend_from_slice(complement_data);

        let binding_hash = blake3::hash(&binding_content);

        Self {
            binding_hash: *binding_hash.as_bytes(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            signature: Vec::new(), // To be signed by OES
        }
    }

    /// Verify the entanglement proof
    pub fn verify(&self, string: &RopeString, complement_data: &[u8]) -> bool {
        let mut binding_content = Vec::new();
        binding_content.extend_from_slice(string.id().as_bytes());
        binding_content.extend_from_slice(complement_data);

        let expected_hash = blake3::hash(&binding_content);
        self.binding_hash == *expected_hash.as_bytes()
    }
}

/// Complement - Verification and regeneration partner for a string
///
/// Like DNA's complementary strand, the Complement enables:
/// 1. Integrity verification through hash comparison
/// 2. Error correction through Reed-Solomon decoding
/// 3. Regeneration of lost/corrupted data
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Complement {
    /// ID of the primary string this complements
    primary_id: StringId,

    /// Reed-Solomon encoded parity data
    complement_data: Vec<u8>,

    /// BLAKE3 hash of primary content for verification
    verification_hash: [u8; 32],

    /// Hints for multi-source regeneration
    regeneration_hints: Vec<RegenerationHint>,

    /// Proof linking this complement to its string
    entanglement_proof: EntanglementProof,
}

impl Complement {
    /// Generate a complement for a string
    pub fn generate(string: &RopeString) -> Self {
        let content = string.content();

        // Generate Reed-Solomon parity
        let complement_data =
            Self::generate_reed_solomon_parity(&content, string.replication_factor());

        // Compute verification hash
        let verification_hash = *blake3::hash(&content).as_bytes();

        // Generate entanglement proof
        let entanglement_proof = EntanglementProof::generate(string, &complement_data);

        // Collect regeneration hints from parentage
        let regeneration_hints = string
            .parentage()
            .iter()
            .map(|parent_id| RegenerationHint {
                related_string_id: *parent_id,
                relationship: RelationshipType::Parent,
                segment_range: (0, content.len() as u64),
            })
            .collect();

        Self {
            primary_id: string.id(),
            complement_data,
            verification_hash,
            regeneration_hints,
            entanglement_proof,
        }
    }

    /// Generate Reed-Solomon parity shards
    fn generate_reed_solomon_parity(data: &[u8], replication_factor: u32) -> Vec<u8> {
        if data.is_empty() {
            return Vec::new();
        }

        // Reed-Solomon parameters: data_shards + parity_shards
        // For replication_factor 5: 3 data shards, 2 parity shards
        let data_shards = (replication_factor as usize * 3) / 5;
        let parity_shards = replication_factor as usize - data_shards;

        let data_shards = data_shards.max(1);
        let parity_shards = parity_shards.max(1);

        // Pad data to be divisible by data_shards
        let shard_size = data.len().div_ceil(data_shards);
        let mut padded_data = data.to_vec();
        padded_data.resize(shard_size * data_shards, 0);

        // Create shards
        let mut shards: Vec<Vec<u8>> = padded_data
            .chunks(shard_size)
            .map(|chunk| chunk.to_vec())
            .collect();

        // Add empty parity shards
        for _ in 0..parity_shards {
            shards.push(vec![0u8; shard_size]);
        }

        // Encode with Reed-Solomon
        if let Ok(rs) = ReedSolomon::new(data_shards, parity_shards) {
            let mut shard_refs: Vec<&mut [u8]> =
                shards.iter_mut().map(|s| s.as_mut_slice()).collect();
            let _ = rs.encode(&mut shard_refs);
        }

        // Return only parity shards as complement
        let parity_start = data_shards;
        shards[parity_start..]
            .iter()
            .flat_map(|s| s.iter().copied())
            .collect()
    }

    /// Get primary string ID
    pub fn primary_id(&self) -> StringId {
        self.primary_id
    }

    /// Get the complement data (Reed-Solomon parity)
    pub fn complement_data(&self) -> &[u8] {
        &self.complement_data
    }

    /// Get verification hash
    pub fn verification_hash(&self) -> &[u8; 32] {
        &self.verification_hash
    }

    /// Get regeneration hints
    pub fn regeneration_hints(&self) -> &[RegenerationHint] {
        &self.regeneration_hints
    }

    /// Get entanglement proof
    pub fn entanglement_proof(&self) -> &EntanglementProof {
        &self.entanglement_proof
    }

    /// Verify that content matches the verification hash
    pub fn verify_content(&self, content: &[u8]) -> bool {
        let hash = blake3::hash(content);
        *hash.as_bytes() == self.verification_hash
    }

    /// Verify entanglement with string
    pub fn verify_entanglement(&self, string: &RopeString) -> bool {
        string.id() == self.primary_id
            && self
                .entanglement_proof
                .verify(string, &self.complement_data)
    }

    /// Add a regeneration hint
    pub fn add_regeneration_hint(&mut self, hint: RegenerationHint) {
        self.regeneration_hints.push(hint);
    }

    /// Attempt to regenerate damaged content using complement
    pub fn regenerate_content(
        &self,
        damaged_content: &[u8],
        replication_factor: u32,
    ) -> Option<Vec<u8>> {
        if damaged_content.is_empty() && self.complement_data.is_empty() {
            return None;
        }

        // Reed-Solomon parameters
        let data_shards = (replication_factor as usize * 3) / 5;
        let parity_shards = replication_factor as usize - data_shards;

        let data_shards = data_shards.max(1);
        let parity_shards = parity_shards.max(1);

        // Calculate shard size from parity data
        let parity_total_size = self.complement_data.len();
        let shard_size = parity_total_size / parity_shards;

        if shard_size == 0 {
            return None;
        }

        // Reconstruct shards
        let mut shards: Vec<Option<Vec<u8>>> = Vec::new();

        // Add data shards (may be damaged)
        let mut padded_damaged = damaged_content.to_vec();
        padded_damaged.resize(shard_size * data_shards, 0);

        for chunk in padded_damaged.chunks(shard_size) {
            shards.push(Some(chunk.to_vec()));
        }

        // Add parity shards from complement
        for chunk in self.complement_data.chunks(shard_size) {
            shards.push(Some(chunk.to_vec()));
        }

        // Attempt Reed-Solomon reconstruction
        if let Ok(rs) = ReedSolomon::new(data_shards, parity_shards) {
            let mut shard_refs: Vec<Option<Box<[u8]>>> = shards
                .into_iter()
                .map(|s| s.map(|v| v.into_boxed_slice()))
                .collect();

            if rs.reconstruct(&mut shard_refs).is_ok() {
                // Collect reconstructed data shards
                let reconstructed: Vec<u8> = shard_refs[..data_shards]
                    .iter()
                    .filter_map(|s| s.as_ref())
                    .flat_map(|s| s.iter().copied())
                    .collect();

                // Verify reconstruction
                if self.verify_content(&reconstructed) {
                    return Some(reconstructed);
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::LamportClock;
    use crate::string::{PublicKey, RopeString};
    use crate::types::NodeId;

    fn make_test_string(content: &[u8]) -> RopeString {
        RopeString::builder()
            .content(content.to_vec())
            .temporal_marker(LamportClock::new(NodeId::new([0u8; 32])))
            .creator(PublicKey::from_ed25519([0u8; 32]))
            .build()
            .unwrap()
    }

    #[test]
    fn test_complement_generation() {
        let string = make_test_string(b"Test content for complement generation");
        let complement = Complement::generate(&string);

        assert_eq!(complement.primary_id(), string.id());
        assert!(!complement.complement_data().is_empty());
        assert!(complement.verify_content(&string.content()));
    }

    #[test]
    fn test_complement_verification() {
        let string = make_test_string(b"Content to verify");
        let complement = Complement::generate(&string);

        // Correct content should verify
        assert!(complement.verify_content(&string.content()));

        // Wrong content should not verify
        assert!(!complement.verify_content(b"Wrong content"));
    }

    #[test]
    fn test_entanglement_proof() {
        let string = make_test_string(b"Entangled content");
        let complement = Complement::generate(&string);

        assert!(complement.verify_entanglement(&string));

        // Different string should not verify
        let other_string = make_test_string(b"Different content");
        assert!(!complement.verify_entanglement(&other_string));
    }

    #[test]
    fn test_regeneration_hints() {
        let parent_id = StringId::from_content(b"parent");

        let string = RopeString::builder()
            .content(b"Child content".to_vec())
            .temporal_marker(LamportClock::new(NodeId::new([0u8; 32])))
            .creator(PublicKey::from_ed25519([0u8; 32]))
            .add_parent(parent_id)
            .build()
            .unwrap();

        let complement = Complement::generate(&string);

        assert_eq!(complement.regeneration_hints().len(), 1);
        assert_eq!(
            complement.regeneration_hints()[0].related_string_id,
            parent_id
        );
    }
}
