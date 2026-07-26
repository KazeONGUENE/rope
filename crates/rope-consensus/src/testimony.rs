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

use crate::validator_registry::ValidatorRegistry;
use parking_lot::RwLock;
use rope_core::clock::LamportClock;
use rope_core::types::{AttestationType, NodeId, StringId};
use rope_crypto::batch::{BatchVerifyItem, BatchVerifyOutcome};
use rope_crypto::hybrid::{HybridSignature, HybridSigner, HybridVerifier};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

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

    /// Sign this testimony with a validator's hybrid keypair.
    ///
    /// Produces an Ed25519 + CRYSTALS-Dilithium3 signature over
    /// [`signing_data`](Self::signing_data). The `validator_id` MUST
    /// correspond to the signer's key (`NodeId == blake3(ed25519_pub)`);
    /// the collector re-derives the key from the registry keyed by
    /// `validator_id`, so signing with the wrong key produces a
    /// testimony that fails verification.
    pub fn sign_with(&mut self, signer: &HybridSigner) {
        let data = self.signing_data();
        let sig = signer.sign(&data);
        self.signature.ed25519 = sig.ed25519_sig;
        self.signature.dilithium = sig.dilithium_sig;
    }

    /// Reconstruct the crypto-layer [`HybridSignature`] from this
    /// testimony's stored signature bytes, for verification.
    pub fn hybrid_signature(&self) -> HybridSignature {
        HybridSignature {
            ed25519_sig: self.signature.ed25519.clone(),
            dilithium_sig: self.signature.dilithium.clone(),
        }
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

    /// Known validators (node ids). Kept for the finality-quorum size
    /// and duplicate/unknown checks. When a `registry` is attached, its
    /// active set is the authoritative validator universe; this list is
    /// kept in sync as validators register.
    validators: RwLock<Vec<NodeId>>,

    /// Validator public-key registry, used for real signature
    /// verification. When `None`, the collector runs in the legacy
    /// "trust signed testimonies" mode (only permitted when
    /// `config.verify_signatures == false`).
    registry: Option<Arc<ValidatorRegistry>>,

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
    /// Create new collector without a key registry.
    ///
    /// If `config.verify_signatures` is `true` this collector will
    /// reject every testimony with [`TestimonyError::MissingRegistry`]
    /// on the first submission that requires verification — callers
    /// that want signature enforcement MUST use
    /// [`with_registry`](Self::with_registry).
    pub fn new(config: TestimonyConfig) -> Self {
        Self {
            collections: RwLock::new(HashMap::new()),
            validators: RwLock::new(Vec::new()),
            registry: None,
            config,
        }
    }

    /// Create a collector backed by a validator key registry, enabling
    /// real hybrid-signature verification.
    pub fn with_registry(config: TestimonyConfig, registry: Arc<ValidatorRegistry>) -> Self {
        Self {
            collections: RwLock::new(HashMap::new()),
            validators: RwLock::new(registry.active_validators()),
            registry: Some(registry),
            config,
        }
    }

    /// Attach (or replace) the validator registry after construction.
    pub fn set_registry(&mut self, registry: Arc<ValidatorRegistry>) {
        {
            let mut v = self.validators.write();
            for id in registry.active_validators() {
                if !v.iter().any(|x| x.as_bytes() == id.as_bytes()) {
                    v.push(id);
                }
            }
        }
        self.registry = Some(registry);
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

    /// Validate a testimony.
    ///
    /// Enforces, in order:
    /// 1. The validator is a known member of the committee.
    /// 2. If `verify_signatures`, the testimony carries a signature.
    /// 3. If `verify_signatures`, the hybrid signature verifies against
    ///    the validator's registered public key over `signing_data()`.
    ///    A missing registry while `verify_signatures == true` is a
    ///    hard configuration error, not a silent pass.
    fn validate_testimony(&self, testimony: &Testimony) -> Result<(), TestimonyError> {
        // Check validator is known.
        {
            let validators = self.validators.read();
            if !validators
                .iter()
                .any(|v| v.as_bytes() == testimony.validator_id.as_bytes())
            {
                return Err(TestimonyError::UnknownValidator);
            }
        }

        if !self.config.verify_signatures {
            return Ok(());
        }

        // Signature must be present.
        if !testimony.is_signed() {
            return Err(TestimonyError::MissingSignature);
        }

        // A registry is mandatory for real verification. Refuse to
        // silently trust an unverifiable signature.
        let registry = self
            .registry
            .as_ref()
            .ok_or(TestimonyError::MissingRegistry)?;

        // The registering validator must be active.
        if !registry.is_active(&testimony.validator_id) {
            return Err(TestimonyError::UnknownValidator);
        }

        let public_key = registry
            .public_key(&testimony.validator_id)
            .ok_or(TestimonyError::UnknownValidator)?;

        let sig = testimony.hybrid_signature();
        let data = testimony.signing_data();
        match HybridVerifier::verify(&public_key, &data, &sig) {
            Ok(true) => Ok(()),
            Ok(false) => Err(TestimonyError::InvalidSignature),
            Err(e) => {
                tracing::warn!(
                    validator = ?testimony.validator_id,
                    "testimony signature verification errored: {e}"
                );
                Err(TestimonyError::InvalidSignature)
            }
        }
    }

    /// Submit and verify a batch of testimonies in one call, using
    /// rayon-parallel batch signature verification (Phase 2.C).
    ///
    /// This is the high-throughput entry point the consensus
    /// orchestrator uses when many testimonies for (possibly many)
    /// strings arrive together — e.g. an anchor round collecting the
    /// committee's attestations. Verification of the whole batch runs
    /// in parallel across the rayon pool with a process-wide parsed
    /// public-key cache.
    ///
    /// Returns, for each input testimony (parallel to the input slice),
    /// `Ok(true)` if it was accepted AND its target string reached
    /// finality as a result, `Ok(false)` if accepted but not yet
    /// final, and `Err(..)` if it was rejected (unknown validator,
    /// missing/invalid signature, etc). Rejected testimonies are NOT
    /// added to any collection.
    pub fn submit_testimonies_batch(
        &self,
        testimonies: Vec<Testimony>,
    ) -> Vec<Result<bool, TestimonyError>> {
        if testimonies.is_empty() {
            return Vec::new();
        }

        // Pre-flight non-crypto checks + resolve each validator's key.
        // `precheck[i]` is Ok(public_key_or_none) when the testimony is
        // structurally acceptable, Err(..) when it must be rejected
        // before any crypto work.
        let verify_sigs = self.config.verify_signatures;
        let mut prechecked: Vec<Result<Option<rope_crypto::hybrid::HybridPublicKey>, TestimonyError>> =
            Vec::with_capacity(testimonies.len());

        {
            let validators = self.validators.read();
            for t in &testimonies {
                let known = validators
                    .iter()
                    .any(|v| v.as_bytes() == t.validator_id.as_bytes());
                if !known {
                    prechecked.push(Err(TestimonyError::UnknownValidator));
                    continue;
                }
                if !verify_sigs {
                    prechecked.push(Ok(None));
                    continue;
                }
                if !t.is_signed() {
                    prechecked.push(Err(TestimonyError::MissingSignature));
                    continue;
                }
                match &self.registry {
                    None => prechecked.push(Err(TestimonyError::MissingRegistry)),
                    Some(reg) => {
                        if !reg.is_active(&t.validator_id) {
                            prechecked.push(Err(TestimonyError::UnknownValidator));
                        } else if let Some(pk) = reg.public_key(&t.validator_id) {
                            prechecked.push(Ok(Some(pk)));
                        } else {
                            prechecked.push(Err(TestimonyError::UnknownValidator));
                        }
                    }
                }
            }
        }

        // Build the crypto batch for those that passed pre-flight and
        // require verification. We must keep the owned messages,
        // signatures and public keys alive for the borrow-based
        // BatchVerifyItem API.
        let mut owned_msgs: Vec<Vec<u8>> = Vec::new();
        let mut owned_sigs: Vec<HybridSignature> = Vec::new();
        let mut owned_pks: Vec<rope_crypto::hybrid::HybridPublicKey> = Vec::new();
        // Map batch index -> testimony index.
        let mut batch_to_testimony: Vec<usize> = Vec::new();

        for (i, pc) in prechecked.iter().enumerate() {
            if let Ok(Some(pk)) = pc {
                owned_msgs.push(testimonies[i].signing_data());
                owned_sigs.push(testimonies[i].hybrid_signature());
                owned_pks.push(pk.clone());
                batch_to_testimony.push(i);
            }
        }

        let mut batch_results: Vec<bool> = Vec::new();
        if !owned_msgs.is_empty() {
            let items: Vec<BatchVerifyItem<'_>> = (0..owned_msgs.len())
                .map(|k| {
                    BatchVerifyItem::new(&owned_pks[k], &owned_msgs[k], &owned_sigs[k])
                })
                .collect();
            match HybridVerifier::verify_batch(&items) {
                Ok(BatchVerifyOutcome { results, .. }) => batch_results = results,
                Err(e) => {
                    tracing::error!("batch verification failed: {e}");
                    // On a batch-level error we cannot attribute per-item
                    // fault; reject every item that was in the batch.
                    batch_results = vec![false; owned_msgs.len()];
                }
            }
        }

        // Fold crypto results back onto the per-testimony verdict.
        let mut sig_ok: Vec<Result<(), TestimonyError>> = prechecked
            .iter()
            .map(|pc| match pc {
                Ok(_) => Ok(()),
                Err(e) => Err(e.clone()),
            })
            .collect();
        for (k, &ti) in batch_to_testimony.iter().enumerate() {
            if !batch_results[k] {
                sig_ok[ti] = Err(TestimonyError::InvalidSignature);
            }
        }

        // Insert accepted testimonies and compute finality.
        let validators_count = self.validators.read().len();
        let mut collections = self.collections.write();
        let mut out: Vec<Result<bool, TestimonyError>> = Vec::with_capacity(testimonies.len());
        for (i, t) in testimonies.into_iter().enumerate() {
            match &sig_ok[i] {
                Err(e) => out.push(Err(e.clone())),
                Ok(()) => {
                    let collection = collections
                        .entry(t.target_string_id)
                        .or_insert_with(|| TestimonyCollection::new(t.target_string_id));
                    collection.add(t);
                    let finality = collection.check_finality(validators_count);
                    out.push(Ok(finality));
                }
            }
        }
        out
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
    /// `verify_signatures` is enabled but no validator key registry is
    /// attached — the collector cannot verify and refuses to silently
    /// trust the testimony.
    MissingRegistry,
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
            TestimonyError::MissingRegistry => {
                write!(f, "signature verification enabled but no validator registry attached")
            }
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
    fn signed_testimony_verifies_against_registry() {
        use crate::validator_registry::ValidatorRegistry;
        use rope_crypto::hybrid::HybridSigner;

        let registry = Arc::new(ValidatorRegistry::new());
        let (signer, pk) = HybridSigner::generate();
        let validator_id = NodeId::new(pk.node_id());
        registry.register(validator_id, pk).unwrap();

        let collector =
            TestimonyCollector::with_registry(TestimonyConfig::default(), registry.clone());

        let string_id = StringId::from_content(b"real-sig string");
        let timestamp = LamportClock::new(validator_id);
        let mut testimony = Testimony::new(
            string_id,
            validator_id,
            AttestationType::Existence,
            timestamp,
            7,
        );
        testimony.sign_with(&signer);
        assert!(testimony.is_signed());

        // A genuinely signed testimony from a registered validator is
        // accepted (single validator => finality with 2f+1 = 1).
        let res = collector.submit_testimony(testimony);
        assert!(res.is_ok(), "expected accept, got {res:?}");
    }

    #[test]
    fn forged_testimony_is_rejected() {
        use crate::validator_registry::ValidatorRegistry;
        use rope_crypto::hybrid::HybridSigner;

        let registry = Arc::new(ValidatorRegistry::new());
        let (_signer, pk) = HybridSigner::generate();
        let validator_id = NodeId::new(pk.node_id());
        registry.register(validator_id, pk).unwrap();

        // Attacker signs with a DIFFERENT key but claims the victim's id.
        let (attacker_signer, _attacker_pk) = HybridSigner::generate();

        let collector =
            TestimonyCollector::with_registry(TestimonyConfig::default(), registry);

        let string_id = StringId::from_content(b"forged string");
        let timestamp = LamportClock::new(validator_id);
        let mut testimony = Testimony::new(
            string_id,
            validator_id,
            AttestationType::Existence,
            timestamp,
            7,
        );
        testimony.sign_with(&attacker_signer);

        let res = collector.submit_testimony(testimony);
        assert!(
            matches!(res, Err(TestimonyError::InvalidSignature)),
            "forged testimony must be rejected, got {res:?}"
        );
    }

    #[test]
    fn batch_submit_verifies_and_finalizes() {
        use crate::validator_registry::ValidatorRegistry;
        use rope_crypto::hybrid::HybridSigner;

        // 4 validators; f=(4-1)/3=1; threshold=2f+1=3.
        let registry = Arc::new(ValidatorRegistry::new());
        let mut signers = Vec::new();
        let mut ids = Vec::new();
        for _ in 0..4 {
            let (s, pk) = HybridSigner::generate();
            let id = NodeId::new(pk.node_id());
            registry.register(id, pk).unwrap();
            signers.push(s);
            ids.push(id);
        }

        let collector =
            TestimonyCollector::with_registry(TestimonyConfig::default(), registry);

        let string_id = StringId::from_content(b"batch string");
        let mut batch = Vec::new();
        for i in 0..3 {
            let ts = LamportClock::new(ids[i]);
            let mut t = Testimony::new(
                string_id,
                ids[i],
                AttestationType::Existence,
                ts,
                1,
            );
            t.sign_with(&signers[i]);
            batch.push(t);
        }

        let results = collector.submit_testimonies_batch(batch);
        assert_eq!(results.len(), 3);
        for r in &results {
            assert!(r.is_ok(), "each item must be accepted: {r:?}");
        }
        // The 3rd testimony reaches the 2f+1=3 threshold.
        assert_eq!(*results.last().unwrap().as_ref().unwrap(), true);
        assert!(collector.is_finalized(&string_id));
    }

    #[test]
    fn batch_submit_isolates_forged_item() {
        use crate::validator_registry::ValidatorRegistry;
        use rope_crypto::hybrid::HybridSigner;

        let registry = Arc::new(ValidatorRegistry::new());
        let mut signers = Vec::new();
        let mut ids = Vec::new();
        for _ in 0..4 {
            let (s, pk) = HybridSigner::generate();
            let id = NodeId::new(pk.node_id());
            registry.register(id, pk).unwrap();
            signers.push(s);
            ids.push(id);
        }
        let (attacker, _) = HybridSigner::generate();

        let collector =
            TestimonyCollector::with_registry(TestimonyConfig::default(), registry);
        let string_id = StringId::from_content(b"batch mix");

        let mut batch = Vec::new();
        // item 0 valid, item 1 forged, item 2 valid
        for i in 0..3 {
            let ts = LamportClock::new(ids[i]);
            let mut t = Testimony::new(
                string_id,
                ids[i],
                AttestationType::Existence,
                ts,
                1,
            );
            if i == 1 {
                t.sign_with(&attacker);
            } else {
                t.sign_with(&signers[i]);
            }
            batch.push(t);
        }

        let results = collector.submit_testimonies_batch(batch);
        assert!(results[0].is_ok());
        assert!(matches!(results[1], Err(TestimonyError::InvalidSignature)));
        assert!(results[2].is_ok());
    }

    #[test]
    fn verify_enabled_without_registry_is_hard_error() {
        // Default config has verify_signatures = true; new() has no
        // registry, so any signature-requiring submit must fail closed.
        let collector = TestimonyCollector::new(TestimonyConfig::default());
        let validator_id = NodeId::new([9u8; 32]);
        collector.register_validator(validator_id);
        let string_id = StringId::from_content(b"no registry");
        let ts = LamportClock::new(validator_id);
        let mut t = Testimony::new(
            string_id,
            validator_id,
            AttestationType::Existence,
            ts,
            1,
        );
        t.set_signature(vec![1u8; 64], vec![2u8; 3309]);
        let res = collector.submit_testimony(t);
        assert!(matches!(res, Err(TestimonyError::MissingRegistry)));
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
