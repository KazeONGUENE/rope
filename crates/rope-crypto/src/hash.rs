//! BLAKE3 hashing utilities for Datachain Rope
//!
//! All hashing in Datachain Rope uses BLAKE3 with 256-bit output.
//! BLAKE3 provides:
//! - Speed: 3-4x faster than SHA-256
//! - Security: 256-bit security level
//! - Keyed hashing: For MACs and KDFs
//! - Extensible output: For key derivation

/// Hash data using BLAKE3 (256-bit output)
pub fn hash_blake3(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

/// Keyed hash using BLAKE3
pub fn hash_keyed(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    *blake3::keyed_hash(key, data).as_bytes()
}

/// Hash multiple items together
pub fn hash_concat(items: &[&[u8]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for item in items {
        hasher.update(item);
    }
    *hasher.finalize().as_bytes()
}

/// Derive key material from seed
pub fn derive_key(context: &str, key_material: &[u8]) -> [u8; 32] {
    *blake3::derive_key(context, key_material)
        .as_slice()
        .try_into()
        .unwrap_or(&[0u8; 32])
}

/// Incremental hasher for large data
pub struct IncrementalHasher {
    hasher: blake3::Hasher,
}

impl IncrementalHasher {
    /// Create new incremental hasher
    pub fn new() -> Self {
        Self {
            hasher: blake3::Hasher::new(),
        }
    }

    /// Create keyed incremental hasher
    pub fn new_keyed(key: &[u8; 32]) -> Self {
        Self {
            hasher: blake3::Hasher::new_keyed(key),
        }
    }

    /// Update with data
    pub fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }

    /// Finalize and get hash
    pub fn finalize(self) -> [u8; 32] {
        *self.hasher.finalize().as_bytes()
    }

    /// Finalize with extended output
    pub fn finalize_xof(self, output: &mut [u8]) {
        let mut reader = self.hasher.finalize_xof();
        reader.fill(output);
    }
}

impl Default for IncrementalHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// Merkle tree utilities for proofs
pub mod merkle {
    use super::*;

    /// Compute Merkle root from leaves
    pub fn compute_root(leaves: &[[u8; 32]]) -> [u8; 32] {
        if leaves.is_empty() {
            return [0u8; 32];
        }

        if leaves.len() == 1 {
            return leaves[0];
        }

        let mut current_level = leaves.to_vec();

        while current_level.len() > 1 {
            let mut next_level = Vec::with_capacity(current_level.len().div_ceil(2));

            for chunk in current_level.chunks(2) {
                if chunk.len() == 2 {
                    next_level.push(hash_concat(&[&chunk[0], &chunk[1]]));
                } else {
                    next_level.push(chunk[0]);
                }
            }

            current_level = next_level;
        }

        current_level[0]
    }

    /// Generate Merkle proof for a leaf at given index
    pub fn generate_proof(leaves: &[[u8; 32]], index: usize) -> Vec<[u8; 32]> {
        if leaves.is_empty() || index >= leaves.len() {
            return Vec::new();
        }

        let mut proof = Vec::new();
        let mut current_level = leaves.to_vec();
        let mut current_index = index;

        while current_level.len() > 1 {
            let sibling_index = if current_index.is_multiple_of(2) {
                current_index + 1
            } else {
                current_index - 1
            };

            if sibling_index < current_level.len() {
                proof.push(current_level[sibling_index]);
            }

            // Move to next level
            let mut next_level = Vec::with_capacity(current_level.len().div_ceil(2));
            for chunk in current_level.chunks(2) {
                if chunk.len() == 2 {
                    next_level.push(hash_concat(&[&chunk[0], &chunk[1]]));
                } else {
                    next_level.push(chunk[0]);
                }
            }

            current_level = next_level;
            current_index /= 2;
        }

        proof
    }

    /// Verify Merkle proof
    pub fn verify_proof(leaf: [u8; 32], proof: &[[u8; 32]], index: usize, root: [u8; 32]) -> bool {
        let mut current = leaf;
        let mut current_index = index;

        for sibling in proof {
            if current_index.is_multiple_of(2) {
                current = hash_concat(&[&current, sibling]);
            } else {
                current = hash_concat(&[sibling, &current]);
            }
            current_index /= 2;
        }

        current == root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_blake3() {
        let data = b"Hello, Datachain Rope!";
        let hash = hash_blake3(data);

        assert_eq!(hash.len(), 32);

        // Same data should give same hash
        let hash2 = hash_blake3(data);
        assert_eq!(hash, hash2);

        // Different data should give different hash
        let hash3 = hash_blake3(b"Different data");
        assert_ne!(hash, hash3);
    }

    #[test]
    fn test_keyed_hash() {
        let key = [42u8; 32];
        let data = b"Test data";

        let hash = hash_keyed(&key, data);

        // Different key should give different hash
        let key2 = [43u8; 32];
        let hash2 = hash_keyed(&key2, data);
        assert_ne!(hash, hash2);
    }

    #[test]
    fn test_incremental_hasher() {
        let data = b"Hello, World!";

        // Full hash
        let full_hash = hash_blake3(data);

        // Incremental hash
        let mut hasher = IncrementalHasher::new();
        hasher.update(b"Hello, ");
        hasher.update(b"World!");
        let incremental_hash = hasher.finalize();

        assert_eq!(full_hash, incremental_hash);
    }

    #[test]
    fn test_merkle_root() {
        let leaves = [
            hash_blake3(b"leaf1"),
            hash_blake3(b"leaf2"),
            hash_blake3(b"leaf3"),
            hash_blake3(b"leaf4"),
        ];

        let root = merkle::compute_root(&leaves);

        // Root should be deterministic
        let root2 = merkle::compute_root(&leaves);
        assert_eq!(root, root2);
    }

    #[test]
    fn test_merkle_proof() {
        let leaves = [
            hash_blake3(b"leaf1"),
            hash_blake3(b"leaf2"),
            hash_blake3(b"leaf3"),
            hash_blake3(b"leaf4"),
        ];

        let root = merkle::compute_root(&leaves);

        // Generate and verify proof for each leaf
        for (i, leaf) in leaves.iter().enumerate() {
            let proof = merkle::generate_proof(&leaves, i);
            assert!(merkle::verify_proof(*leaf, &proof, i, root));
        }
    }

    #[test]
    fn test_merkle_proof_wrong_leaf() {
        let leaves = [hash_blake3(b"leaf1"), hash_blake3(b"leaf2")];

        let root = merkle::compute_root(&leaves);
        let proof = merkle::generate_proof(&leaves, 0);

        // Wrong leaf should fail verification
        let wrong_leaf = hash_blake3(b"wrong");
        assert!(!merkle::verify_proof(wrong_leaf, &proof, 0, root));
    }
}
