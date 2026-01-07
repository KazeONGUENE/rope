//! Testimony - Cryptographic attestations for string validity

use rope_core::types::{AttestationType, NodeId, StringId};
use rope_core::clock::LamportClock;
use serde::{Deserialize, Serialize};

/// Testimony - Validator attestation confirming string validity
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Testimony {
    /// Target string being attested
    pub target_string_id: StringId,
    
    /// Validator providing testimony
    pub validator_id: NodeId,
    
    /// Type of attestation
    pub attestation_type: AttestationType,
    
    /// Hybrid signature
    pub signature: Vec<u8>,
    
    /// Logical timestamp
    pub timestamp: LamportClock,
    
    /// OES generation when created
    pub oes_generation: u64,
}

impl Testimony {
    /// Create a new testimony
    pub fn new(
        target_string_id: StringId,
        validator_id: NodeId,
        attestation_type: AttestationType,
        timestamp: LamportClock,
        oes_generation: u64,
    ) -> Self {
        Self {
            target_string_id,
            validator_id,
            attestation_type,
            signature: Vec::new(),
            timestamp,
            oes_generation,
        }
    }
}

