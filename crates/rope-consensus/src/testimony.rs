//! # Testimony Protocol
//!
//! Cryptographic attestations for string validity in the Datachain Rope.
//!
//! ## Overview
//!
//! The Testimony Protocol extends Hashgraph's virtual voting with
//! accountable attestations. Validators provide explicit testimonies
//! that create a verifiable audit trail.
//!
//! ## Testimony Types
//!
//! 1. **Existence** - String exists and is valid
//! 2. **Ordering** - String follows causal ordering
//! 3. **Finality** - String has achieved finality
//! 4. **Erasure** - String has been validly erased
//!
//! ## Byzantine Fault Tolerance
//!
//! Requires 2f+1 testimonies where f = (n-1)/3 Byzantine validators.
//! For n=21 validators, need 15 testimonies (f=6).

use parking_lot::RwLock;
use rope_core::clock::LamportClock;
use rope_core::types::{AttestationType, NodeId, StringId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Testimony - Validator attestation confirming string validity
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Testimony {
    /// Unique testimony ID
    pub id: [u8; 32],

    /// Target string being attested
    pub target_string_id: StringId,

    /// Validator providing testimony
    pub validator_id: NodeId,

    /// Type of attestation
    pub attestation_type: AttestationType,

    /// Hybrid signature (Ed25519 + Dilithium)
    pub signature: TestimonySignature,

    /// Logical timestamp
    pub timestamp: LamportClock,

    /// OES generation when created
    pub oes_generation: u64,

    /// Additional metadata
    pub metadata: TestimonyMetadata,
}

/// Testimony signature (hybrid quantum-resistant)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestimonySignature {
    /// Ed25519 signature (64 bytes)
    pub ed25519: Vec<u8>,

    /// CRYSTALS-Dilithium3 signature (~2420 bytes)
    pub dilithium: Vec<u8>,
}

impl Default for TestimonySignature {
    fn default() -> Self {
        Self {
            ed25519: Vec::new(),
            dilithium: Vec::new(),
        }
    }
}

/// Testimony metadata
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TestimonyMetadata {
    /// Previous testimonies seen
    pub seen_testimonies: Vec<[u8; 32]>,

    /// Round number
    pub round: u64,

    /// Geographic region (for latency analysis)
    pub region: Option<String>,

    /// Additional attributes
    pub attributes: HashMap<String, String>,
}

/// Testimony type marker for string content
pub const TESTIMONY_TYPE_MARKER: u8 = 0x01;

impl Testimony {
    /// Create a new testimony
    pub fn new(
        target_string_id: StringId,
        validator_id: NodeId,
        attestation_type: AttestationType,
        timestamp: LamportClock,
        oes_generation: u64,
    ) -> Self {
        // Generate testimony ID
        let id = Self::generate_id(&target_string_id, &validator_id, &timestamp);

        Self {
            id,
            target_string_id,
            validator_id,
            attestation_type,
            signature: TestimonySignature::default(),
            timestamp,
            oes_generation,
            metadata: TestimonyMetadata::default(),
        }
    }

    /// Generate testimony ID
    fn generate_id(target: &StringId, validator: &NodeId, timestamp: &LamportClock) -> [u8; 32] {
        let mut data = Vec::new();
        data.extend_from_slice(target.as_bytes());
        data.extend_from_slice(validator.as_bytes());
        data.extend_from_slice(&timestamp.time().to_le_bytes());
        *blake3::hash(&data).as_bytes()
    }

    /// Get the data to be signed
    pub fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(self.target_string_id.as_bytes());
        data.extend_from_slice(self.validator_id.as_bytes());
        data.push(self.attestation_type.as_u8());
        data.extend_from_slice(&self.timestamp.time().to_le_bytes());
        data.extend_from_slice(&self.oes_generation.to_le_bytes());
        data
    }

    /// Check if testimony is signed
    pub fn is_signed(&self) -> bool {
        !self.signature.ed25519.is_empty() && !self.signature.dilithium.is_empty()
    }

    /// Set signature
    pub fn set_signature(&mut self, ed25519: Vec<u8>, dilithium: Vec<u8>) {
        self.signature.ed25519 = ed25519;
        self.signature.dilithium = dilithium;
    }

    // ========================================================================
    // Testimony as String (§6.1)
    // ========================================================================

    /// Serialize testimony content for lattice storage
    ///
    /// Per specification §6.1:
    /// "Critically, each testimony is itself a string that references other strings,
    /// creating a recursive structure where consensus evidence is preserved in the
    /// same data structure as the data being validated."
    pub fn serialize_content(&self) -> Vec<u8> {
        let mut content = Vec::with_capacity(256);

        // Type marker
        content.push(TESTIMONY_TYPE_MARKER);

        // Version (for future compatibility)
        content.push(0x01);

        // Target string ID (32 bytes)
        content.extend_from_slice(self.target_string_id.as_bytes());

        // Validator ID (32 bytes)
        content.extend_from_slice(self.validator_id.as_bytes());

        // Attestation type (1 byte)
        content.push(self.attestation_type.as_u8());

        // Timestamp (8 bytes)
        content.extend_from_slice(&self.timestamp.time().to_le_bytes());

        // OES generation (8 bytes)
        content.extend_from_slice(&self.oes_generation.to_le_bytes());

        // Round number (8 bytes)
        content.extend_from_slice(&self.metadata.round.to_le_bytes());

        // Signature lengths and data
        let ed25519_len = self.signature.ed25519.len() as u16;
        let dilithium_len = self.signature.dilithium.len() as u16;

        content.extend_from_slice(&ed25519_len.to_le_bytes());
        content.extend_from_slice(&self.signature.ed25519);

        content.extend_from_slice(&dilithium_len.to_le_bytes());
        content.extend_from_slice(&self.signature.dilithium);

        content
    }

    /// Parse testimony from serialized content
    pub fn from_content(content: &[u8]) -> Result<Self, TestimonyError> {
        if content.len() < 84 {
            return Err(TestimonyError::InvalidFormat(
                "Content too short".to_string(),
            ));
        }

        let mut pos = 0;

        // Check type marker
        if content[pos] != TESTIMONY_TYPE_MARKER {
            return Err(TestimonyError::InvalidFormat(
                "Invalid type marker".to_string(),
            ));
        }
        pos += 1;

        // Version
        let _version = content[pos];
        pos += 1;

        // Target string ID
        let target_bytes: [u8; 32] = content[pos..pos + 32]
            .try_into()
            .map_err(|_| TestimonyError::InvalidFormat("Invalid target ID".to_string()))?;
        let target_string_id = StringId::new(target_bytes);
        pos += 32;

        // Validator ID
        let validator_bytes: [u8; 32] = content[pos..pos + 32]
            .try_into()
            .map_err(|_| TestimonyError::InvalidFormat("Invalid validator ID".to_string()))?;
        let validator_id = NodeId::new(validator_bytes);
        pos += 32;

        // Attestation type
        let attestation_type =
            AttestationType::from_u8(content[pos]).ok_or(TestimonyError::InvalidAttestationType)?;
        pos += 1;

        // Timestamp
        let timestamp_val = u64::from_le_bytes(
            content[pos..pos + 8]
                .try_into()
                .map_err(|_| TestimonyError::InvalidFormat("Invalid timestamp".to_string()))?,
        );
        pos += 8;

        // OES generation
        let oes_generation =
            u64::from_le_bytes(content[pos..pos + 8].try_into().map_err(|_| {
                TestimonyError::InvalidFormat("Invalid OES generation".to_string())
            })?);
        pos += 8;

        // Round
        let round = u64::from_le_bytes(
            content[pos..pos + 8]
                .try_into()
                .map_err(|_| TestimonyError::InvalidFormat("Invalid round".to_string()))?,
        );
        pos += 8;

        // Signatures
        let ed25519_len =
            u16::from_le_bytes(content[pos..pos + 2].try_into().map_err(|_| {
                TestimonyError::InvalidFormat("Invalid signature length".to_string())
            })?) as usize;
        pos += 2;

        let ed25519 = if ed25519_len > 0 && pos + ed25519_len <= content.len() {
            content[pos..pos + ed25519_len].to_vec()
        } else {
            Vec::new()
        };
        pos += ed25519_len;

        let dilithium_len = if pos + 2 <= content.len() {
            u16::from_le_bytes(content[pos..pos + 2].try_into().unwrap_or([0, 0])) as usize
        } else {
            0
        };
        pos += 2;

        let dilithium = if dilithium_len > 0 && pos + dilithium_len <= content.len() {
            content[pos..pos + dilithium_len].to_vec()
        } else {
            Vec::new()
        };

        // Reconstruct timestamp (simplified - just use value as time)
        let mut timestamp = LamportClock::new(validator_id);
        for _ in 0..timestamp_val {
            timestamp.increment();
        }

        let mut testimony = Self::new(
            target_string_id,
            validator_id,
            attestation_type,
            timestamp,
            oes_generation,
        );

        testimony.signature = TestimonySignature { ed25519, dilithium };
        testimony.metadata.round = round;

        Ok(testimony)
    }

    /// Get string ID for this testimony when stored in lattice
    pub fn as_string_id(&self) -> StringId {
        StringId::new(self.id)
    }

    /// Get parent string IDs (references the target string)
    pub fn parent_strings(&self) -> Vec<StringId> {
        vec![self.target_string_id]
    }
}

/// Testimony collection for a string
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TestimonyCollection {
    /// Target string ID
    pub string_id: StringId,

    /// All testimonies for this string
    pub testimonies: Vec<Testimony>,

    /// Count by attestation type
    pub type_counts: HashMap<u8, usize>,

    /// Total validator weight
    pub total_weight: u64,

    /// Whether finality threshold is reached
    pub finality_reached: bool,
}

impl TestimonyCollection {
    /// Create new collection for a string
    pub fn new(string_id: StringId) -> Self {
        Self {
            string_id,
            testimonies: Vec::new(),
            type_counts: HashMap::new(),
            total_weight: 0,
            finality_reached: false,
        }
    }

    /// Add a testimony
    pub fn add(&mut self, testimony: Testimony) {
        // Check not duplicate
        if self.testimonies.iter().any(|t| t.id == testimony.id) {
            return;
        }

        // Update type counts
        let type_key = testimony.attestation_type.as_u8();
        *self.type_counts.entry(type_key).or_insert(0) += 1;

        // Add testimony
        self.testimonies.push(testimony);

        // Update weight (simplified - each validator has weight 1)
        self.total_weight += 1;
    }

    /// Count testimonies of a specific type
    pub fn count_type(&self, attestation_type: AttestationType) -> usize {
        self.type_counts
            .get(&attestation_type.as_u8())
            .copied()
            .unwrap_or(0)
    }

    /// Check if finality threshold is reached
    /// Requires 2f+1 existence testimonies where f = (n-1)/3
    pub fn check_finality(&mut self, total_validators: usize) -> bool {
        let f = (total_validators - 1) / 3;
        let threshold = 2 * f + 1;

        let existence_count = self.count_type(AttestationType::Existence);
        self.finality_reached = existence_count >= threshold;
        self.finality_reached
    }

    /// Get unique validators who testified
    pub fn unique_validators(&self) -> Vec<NodeId> {
        let mut validators: Vec<NodeId> = self.testimonies.iter().map(|t| t.validator_id).collect();
        validators.sort_by_key(|v| *v.as_bytes());
        validators.dedup_by_key(|v| *v.as_bytes());
        validators
    }
}

/// Testimony collector service
pub struct TestimonyCollector {
    /// Collections by string ID
    collections: RwLock<HashMap<StringId, TestimonyCollection>>,

    /// Known validators
    validators: RwLock<Vec<NodeId>>,

    /// Configuration
    config: TestimonyConfig,
}

/// Testimony configuration
#[derive(Clone, Debug)]
pub struct TestimonyConfig {
    /// Minimum testimonies for finality
    pub finality_threshold: usize,

    /// Maximum age of testimony (in Lamport ticks)
    pub max_testimony_age: u64,

    /// Enable signature verification
    pub verify_signatures: bool,
}

impl Default for TestimonyConfig {
    fn default() -> Self {
        Self {
            finality_threshold: 15, // 2f+1 for 21 validators
            max_testimony_age: 1000,
            verify_signatures: true,
        }
    }
}

impl TestimonyCollector {
    /// Create new collector
    pub fn new(config: TestimonyConfig) -> Self {
        Self {
            collections: RwLock::new(HashMap::new()),
            validators: RwLock::new(Vec::new()),
            config,
        }
    }

    /// Register a validator
    pub fn register_validator(&self, validator: NodeId) {
        let mut validators = self.validators.write();
        if !validators
            .iter()
            .any(|v| v.as_bytes() == validator.as_bytes())
        {
            validators.push(validator);
        }
    }

    /// Submit a testimony
    pub fn submit_testimony(&self, testimony: Testimony) -> Result<bool, TestimonyError> {
        // Validate testimony
        self.validate_testimony(&testimony)?;

        let mut collections = self.collections.write();
        let collection = collections
            .entry(testimony.target_string_id)
            .or_insert_with(|| TestimonyCollection::new(testimony.target_string_id));

        collection.add(testimony);

        // Check finality
        let validators_count = self.validators.read().len();
        let finality = collection.check_finality(validators_count);

        Ok(finality)
    }

    /// Validate a testimony
    fn validate_testimony(&self, testimony: &Testimony) -> Result<(), TestimonyError> {
        // Check validator is known
        let validators = self.validators.read();
        if !validators
            .iter()
            .any(|v| v.as_bytes() == testimony.validator_id.as_bytes())
        {
            return Err(TestimonyError::UnknownValidator);
        }

        // Check signature if required
        if self.config.verify_signatures && !testimony.is_signed() {
            return Err(TestimonyError::MissingSignature);
        }

        // Signature verification would happen here with rope-crypto
        // For now, we trust signed testimonies

        Ok(())
    }

    /// Get collection for a string
    pub fn get_collection(&self, string_id: &StringId) -> Option<TestimonyCollection> {
        self.collections.read().get(string_id).cloned()
    }

    /// Check if a string has reached finality
    pub fn is_finalized(&self, string_id: &StringId) -> bool {
        self.collections
            .read()
            .get(string_id)
            .map(|c| c.finality_reached)
            .unwrap_or(false)
    }

    /// Get finality progress
    pub fn finality_progress(&self, string_id: &StringId) -> FinalityProgress {
        let collections = self.collections.read();
        let validators_count = self.validators.read().len();

        if let Some(collection) = collections.get(string_id) {
            let f = (validators_count.saturating_sub(1)) / 3;
            let threshold = 2 * f + 1;
            let current = collection.count_type(AttestationType::Existence);

            FinalityProgress {
                current_testimonies: current,
                required_testimonies: threshold,
                finality_reached: collection.finality_reached,
                unique_validators: collection.unique_validators().len(),
                total_validators: validators_count,
            }
        } else {
            FinalityProgress {
                current_testimonies: 0,
                required_testimonies: self.config.finality_threshold,
                finality_reached: false,
                unique_validators: 0,
                total_validators: validators_count,
            }
        }
    }
}

impl Default for TestimonyCollector {
    fn default() -> Self {
        Self::new(TestimonyConfig::default())
    }
}

/// Finality progress report
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinalityProgress {
    pub current_testimonies: usize,
    pub required_testimonies: usize,
    pub finality_reached: bool,
    pub unique_validators: usize,
    pub total_validators: usize,
}

impl FinalityProgress {
    /// Get progress as percentage
    pub fn percentage(&self) -> f64 {
        if self.required_testimonies == 0 {
            return 0.0;
        }
        (self.current_testimonies as f64 / self.required_testimonies as f64 * 100.0).min(100.0)
    }
}

/// Testimony errors
#[derive(Clone, Debug)]
pub enum TestimonyError {
    UnknownValidator,
    MissingSignature,
    InvalidSignature,
    DuplicateTestimony,
    ExpiredTestimony,
    InvalidAttestationType,
    InvalidFormat(String),
}

impl std::fmt::Display for TestimonyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestimonyError::UnknownValidator => write!(f, "Unknown validator"),
            TestimonyError::MissingSignature => write!(f, "Missing signature"),
            TestimonyError::InvalidSignature => write!(f, "Invalid signature"),
            TestimonyError::DuplicateTestimony => write!(f, "Duplicate testimony"),
            TestimonyError::ExpiredTestimony => write!(f, "Expired testimony"),
            TestimonyError::InvalidAttestationType => write!(f, "Invalid attestation type"),
            TestimonyError::InvalidFormat(msg) => write!(f, "Invalid format: {}", msg),
        }
    }
}

impl std::error::Error for TestimonyError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_testimony_creation() {
        let string_id = StringId::from_content(b"test string");
        let validator_id = NodeId::new([1u8; 32]);
        let timestamp = LamportClock::new(validator_id);

        let testimony = Testimony::new(
            string_id,
            validator_id,
            AttestationType::Existence,
            timestamp,
            1,
        );

        assert_eq!(testimony.target_string_id, string_id);
        assert_eq!(testimony.validator_id, validator_id);
        assert!(!testimony.is_signed());
    }

    #[test]
    fn test_testimony_signing() {
        let string_id = StringId::from_content(b"test string");
        let validator_id = NodeId::new([1u8; 32]);
        let timestamp = LamportClock::new(validator_id);

        let mut testimony = Testimony::new(
            string_id,
            validator_id,
            AttestationType::Existence,
            timestamp,
            1,
        );

        assert!(!testimony.is_signed());

        testimony.set_signature(vec![0u8; 64], vec![0u8; 2420]);

        assert!(testimony.is_signed());
    }

    #[test]
    fn test_testimony_collection() {
        let string_id = StringId::from_content(b"test string");
        let mut collection = TestimonyCollection::new(string_id);

        // Add 10 testimonies
        for i in 0..10 {
            let validator_id = NodeId::new([i as u8; 32]);
            let timestamp = LamportClock::new(validator_id);

            let testimony = Testimony::new(
                string_id,
                validator_id,
                AttestationType::Existence,
                timestamp,
                1,
            );

            collection.add(testimony);
        }

        assert_eq!(collection.testimonies.len(), 10);
        assert_eq!(collection.count_type(AttestationType::Existence), 10);

        // Check finality for 21 validators (need 15)
        assert!(!collection.check_finality(21));

        // Add 5 more
        for i in 10..15 {
            let validator_id = NodeId::new([i as u8; 32]);
            let timestamp = LamportClock::new(validator_id);

            let testimony = Testimony::new(
                string_id,
                validator_id,
                AttestationType::Existence,
                timestamp,
                1,
            );

            collection.add(testimony);
        }

        // Now should have finality
        assert!(collection.check_finality(21));
    }

    #[test]
    fn test_testimony_collector() {
        let mut config = TestimonyConfig::default();
        config.verify_signatures = false; // Skip signature verification for test

        let collector = TestimonyCollector::new(config);
        let string_id = StringId::from_content(b"test string");

        // Register 21 validators
        for i in 0..21 {
            collector.register_validator(NodeId::new([i as u8; 32]));
        }

        // For 21 validators: f = (21-1)/3 = 6, threshold = 2*6+1 = 13
        // Submit 12 testimonies (not enough for finality)
        for i in 0..12 {
            let validator_id = NodeId::new([i as u8; 32]);
            let timestamp = LamportClock::new(validator_id);

            let testimony = Testimony::new(
                string_id,
                validator_id,
                AttestationType::Existence,
                timestamp,
                1,
            );

            let result = collector.submit_testimony(testimony);
            assert!(result.is_ok());
            assert!(!result.unwrap()); // Not finalized yet
        }

        // Check progress
        let progress = collector.finality_progress(&string_id);
        assert_eq!(progress.current_testimonies, 12);
        assert_eq!(progress.required_testimonies, 13); // 2f+1 = 13 for 21 validators
        assert!(!progress.finality_reached);

        // Submit 13th testimony (reaches threshold)
        let validator_id = NodeId::new([12u8; 32]);
        let timestamp = LamportClock::new(validator_id);
        let testimony = Testimony::new(
            string_id,
            validator_id,
            AttestationType::Existence,
            timestamp,
            1,
        );

        let result = collector.submit_testimony(testimony);
        assert!(result.is_ok());
        assert!(result.unwrap()); // Now finalized

        assert!(collector.is_finalized(&string_id));
    }
}
