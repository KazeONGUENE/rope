//! Nucleotide - Individual information unit within a string
//!
//! Analogous to DNA bases (A, T, G, C), nucleotides are the atomic units
//! of information in Datachain Rope. Each nucleotide carries:
//! - 256-bit data chunk
//! - Position in sequence
//! - Parity bits for error detection

use serde::{Deserialize, Serialize};

/// Nucleotide - Fundamental unit of information within a string
///
/// Like DNA nucleotides that carry genetic information through base pairs,
/// Rope nucleotides carry data with built-in error detection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Nucleotide {
    /// 256-bit (32-byte) data chunk
    value: [u8; 32],

    /// Position in the sequence (0-indexed)
    position: u64,

    /// Parity bits for error detection (CRC32)
    parity: [u8; 4],
}

impl Nucleotide {
    /// Create a new nucleotide from raw data
    pub fn new(value: [u8; 32], position: u64) -> Self {
        let parity = Self::compute_parity(&value, position);
        Self {
            value,
            position,
            parity,
        }
    }

    /// Create from a slice (will be zero-padded if less than 32 bytes)
    pub fn from_slice(data: &[u8], position: u64) -> Self {
        let mut value = [0u8; 32];
        let len = data.len().min(32);
        value[..len].copy_from_slice(&data[..len]);
        Self::new(value, position)
    }

    /// Get the value bytes
    pub fn value(&self) -> &[u8; 32] {
        &self.value
    }

    /// Get the position
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Get parity bytes
    pub fn parity(&self) -> &[u8; 4] {
        &self.parity
    }

    /// Verify parity matches content
    pub fn verify(&self) -> bool {
        let expected = Self::compute_parity(&self.value, self.position);
        self.parity == expected
    }

    /// Compute CRC32 parity for error detection
    fn compute_parity(value: &[u8; 32], position: u64) -> [u8; 4] {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        position.hash(&mut hasher);
        let hash = hasher.finish();

        // Take lower 32 bits as parity
        (hash as u32).to_be_bytes()
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(44);
        bytes.extend_from_slice(&self.value);
        bytes.extend_from_slice(&self.position.to_be_bytes());
        bytes.extend_from_slice(&self.parity);
        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 44 {
            return None;
        }

        let mut value = [0u8; 32];
        value.copy_from_slice(&bytes[0..32]);

        let position = u64::from_be_bytes(bytes[32..40].try_into().ok()?);

        let mut parity = [0u8; 4];
        parity.copy_from_slice(&bytes[40..44]);

        Some(Self {
            value,
            position,
            parity,
        })
    }
}

/// A sequence of nucleotides forming string content (σ - Sigma)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NucleotideSequence {
    nucleotides: Vec<Nucleotide>,
}

impl NucleotideSequence {
    /// Create empty sequence
    pub fn new() -> Self {
        Self {
            nucleotides: Vec::new(),
        }
    }

    /// Create sequence from raw bytes
    pub fn from_bytes(data: &[u8]) -> Self {
        let nucleotides: Vec<Nucleotide> = data
            .chunks(32)
            .enumerate()
            .map(|(i, chunk)| Nucleotide::from_slice(chunk, i as u64))
            .collect();

        Self { nucleotides }
    }

    /// Add a nucleotide to the sequence
    pub fn push(&mut self, nucleotide: Nucleotide) {
        self.nucleotides.push(nucleotide);
    }

    /// Get nucleotide at position
    pub fn get(&self, position: usize) -> Option<&Nucleotide> {
        self.nucleotides.get(position)
    }

    /// Get mutable nucleotide at position
    pub fn get_mut(&mut self, position: usize) -> Option<&mut Nucleotide> {
        self.nucleotides.get_mut(position)
    }

    /// Get all nucleotides
    pub fn iter(&self) -> impl Iterator<Item = &Nucleotide> {
        self.nucleotides.iter()
    }

    /// Number of nucleotides
    pub fn len(&self) -> usize {
        self.nucleotides.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.nucleotides.is_empty()
    }

    /// Verify all nucleotides have valid parity
    pub fn verify_all(&self) -> bool {
        self.nucleotides.iter().all(|n| n.verify())
    }

    /// Find corrupted nucleotides
    pub fn find_corrupted(&self) -> Vec<u64> {
        self.nucleotides
            .iter()
            .filter(|n| !n.verify())
            .map(|n| n.position())
            .collect()
    }

    /// Convert back to raw bytes
    pub fn to_raw_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.nucleotides.len() * 32);
        for nucleotide in &self.nucleotides {
            bytes.extend_from_slice(nucleotide.value());
        }
        bytes
    }

    /// Serialize entire sequence
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(self.nucleotides.len() as u64).to_be_bytes());
        for nucleotide in &self.nucleotides {
            bytes.extend_from_slice(&nucleotide.to_bytes());
        }
        bytes
    }
}

impl Default for NucleotideSequence {
    fn default() -> Self {
        Self::new()
    }
}

impl From<&[u8]> for NucleotideSequence {
    fn from(data: &[u8]) -> Self {
        Self::from_bytes(data)
    }
}

impl From<Vec<u8>> for NucleotideSequence {
    fn from(data: Vec<u8>) -> Self {
        Self::from_bytes(&data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nucleotide_creation() {
        let value = [0xAB; 32];
        let nucleotide = Nucleotide::new(value, 0);

        assert_eq!(nucleotide.value(), &value);
        assert_eq!(nucleotide.position(), 0);
        assert!(nucleotide.verify());
    }

    #[test]
    fn test_nucleotide_from_slice() {
        let data = b"Hello, Datachain Rope!";
        let nucleotide = Nucleotide::from_slice(data, 0);

        assert!(nucleotide.verify());
        assert!(nucleotide.value().starts_with(data));
    }

    #[test]
    fn test_nucleotide_corruption_detection() {
        let mut nucleotide = Nucleotide::new([0xAB; 32], 0);
        assert!(nucleotide.verify());

        // Corrupt the value
        nucleotide.value[0] = 0xFF;
        assert!(!nucleotide.verify());
    }

    #[test]
    fn test_sequence_from_bytes() {
        let data = vec![0u8; 100];
        let sequence = NucleotideSequence::from_bytes(&data);

        // 100 bytes = 4 nucleotides (32 bytes each, last one padded)
        assert_eq!(sequence.len(), 4);
        assert!(sequence.verify_all());
    }

    #[test]
    fn test_sequence_roundtrip() {
        let original = b"This is test data for the Datachain Rope nucleotide sequence";
        let sequence = NucleotideSequence::from_bytes(original);
        let recovered = sequence.to_raw_bytes();

        // Should start with original data (may have padding at end)
        assert!(recovered.starts_with(original));
    }
}
