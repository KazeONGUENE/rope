//! Core type definitions for Datachain Rope
//!
//! This module defines the fundamental types used throughout the protocol,
//! following the mathematical formalization from the specification.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

/// StringId - Unique identifier for strings computed from BLAKE3 hash
///
/// StringId = BLAKE3(σ || τ || π || ρ || μ)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct StringId {
    /// 256-bit BLAKE3 hash
    hash: [u8; 32],
}

impl StringId {
    /// Create a new StringId from raw bytes
    pub fn new(hash: [u8; 32]) -> Self {
        Self { hash }
    }

    /// Create StringId from content using BLAKE3
    pub fn from_content(content: &[u8]) -> Self {
        let hash = blake3::hash(content);
        Self {
            hash: *hash.as_bytes(),
        }
    }

    /// Get the raw bytes
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.hash
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.hash)
    }

    /// Parse from hex string
    pub fn from_hex(s: &str) -> Result<Self, hex::FromHexError> {
        let bytes = hex::decode(s)?;
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&bytes);
        Ok(Self { hash })
    }

    /// Zero/null StringId (for genesis)
    pub const ZERO: Self = Self { hash: [0u8; 32] };
}

impl fmt::Debug for StringId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StringId({})", &self.to_hex()[..16])
    }
}

impl fmt::Display for StringId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", &self.to_hex()[..16])
    }
}

/// NodeId - Unique identifier for network nodes
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId {
    /// Derived from public key hash
    id: [u8; 32],
}

impl NodeId {
    pub fn new(id: [u8; 32]) -> Self {
        Self { id }
    }

    pub fn from_public_key(public_key: &[u8]) -> Self {
        let hash = blake3::hash(public_key);
        Self {
            id: *hash.as_bytes(),
        }
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.id
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.id)
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NodeId({})", &self.to_hex()[..12])
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", &self.to_hex()[..12])
    }
}

/// MutabilityClass - Erasure policy governing modification/deletion permissions
///
/// μ (Mu) from the String formal definition S = (σ, τ, π, ρ, μ)
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutabilityClass {
    /// Cannot be erased (system strings, genesis)
    Immutable,

    /// Owner can initiate erasure
    #[default]
    OwnerErasable,

    /// Auto-erases after specified duration
    TimeBound(Duration),

    /// Erases when condition is met
    ConditionalErasure(ErasureCondition),

    /// Subject to GDPR right-to-be-forgotten requests
    GDPRCompliant,
}

/// Condition for conditional erasure
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErasureCondition {
    /// Erase after N references
    AfterReferences(u64),

    /// Erase after specific timestamp
    AfterTimestamp(i64),

    /// Erase on specific event
    OnEvent(String),

    /// Custom condition with script
    Custom(Vec<u8>),
}

/// Attestation types for Testimony consensus
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttestationType {
    /// String exists and is valid
    Existence,

    /// String is valid (deprecated - use Existence)
    Validity,

    /// Confirms ordering position
    Ordering,

    /// Confirms finality achieved
    Finality,

    /// Confirms erasure completion
    Erasure,

    /// Confirms regeneration success
    Regeneration,
}

impl AttestationType {
    /// Convert to u8 for hashing/storage
    pub fn as_u8(&self) -> u8 {
        match self {
            AttestationType::Existence => 0,
            AttestationType::Validity => 1,
            AttestationType::Ordering => 2,
            AttestationType::Finality => 3,
            AttestationType::Erasure => 4,
            AttestationType::Regeneration => 5,
        }
    }

    /// Create from u8
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(AttestationType::Existence),
            1 => Some(AttestationType::Validity),
            2 => Some(AttestationType::Ordering),
            3 => Some(AttestationType::Finality),
            4 => Some(AttestationType::Erasure),
            5 => Some(AttestationType::Regeneration),
            _ => None,
        }
    }
}

/// Geographic zone for federation distribution
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Copy)]
pub enum GeoZone {
    NorthAmerica,
    Europe,
    AsiaPacific,
    LatinAmerica,
    Africa,
    MiddleEast,
}

/// System constants as per specification
pub mod constants {
    use std::time::Duration;

    /// Maximum string creation rate per node (1000/sec)
    pub const STRING_RATE_LIMIT: u32 = 1000;

    /// Target interval between anchor strings (~3 seconds)
    pub const ANCHOR_INTERVAL: Duration = Duration::from_secs(3);

    /// Anchor strings required for finality
    pub const FINALITY_ANCHORS: u32 = 3;

    /// Minimum active validator set
    pub const MIN_VALIDATORS: u32 = 21;

    /// Maximum validator set size
    pub const MAX_VALIDATORS: u32 = 100;

    /// Default copies maintained for regeneration
    pub const DEFAULT_REPLICATION_FACTOR: u32 = 5;

    /// Confirmations for erasure completion (2f + 1)
    pub const ERASURE_THRESHOLD_FACTOR: f32 = 2.0 / 3.0;

    /// Organic encryption state evolution interval (anchors)
    pub const OES_EVOLUTION_INTERVAL: u64 = 100;

    /// OES genome dimension in bytes
    pub const GENOME_DIMENSION: usize = 992;

    /// OES mutation rate (10%)
    pub const MUTATION_RATE: f64 = 0.1;

    /// Valid OES generations window
    pub const GENERATION_WINDOW: u64 = 10;

    /// Maximum string content size (10MB)
    pub const MAX_STRING_SIZE: usize = 10 * 1024 * 1024;

    /// Default piece size for RDP transfer (256KB)
    pub const RDP_PIECE_SIZE: usize = 256 * 1024;

    /// DHT replication factor
    pub const DHT_REPLICATION: u32 = 20;

    /// Gossip fanout (nodes to gossip to per round)
    pub const GOSSIP_FANOUT: u32 = 10;

    /// Maximum strings per gossip message
    pub const MAX_GOSSIP_BATCH: usize = 1000;
}

/// Finality status for a string
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinalityStatus {
    /// Whether the string has achieved finality
    pub is_final: bool,

    /// Number of anchor confirmations
    pub anchor_confirmations: u32,

    /// Required confirmations for finality
    pub required_confirmations: u32,

    /// Estimated time until finality (if not final)
    pub estimated_finality_time: Option<Duration>,
}

impl FinalityStatus {
    pub fn finalized(anchor_confirmations: u32) -> Self {
        Self {
            is_final: true,
            anchor_confirmations,
            required_confirmations: constants::FINALITY_ANCHORS,
            estimated_finality_time: None,
        }
    }

    pub fn pending(anchor_confirmations: u32, estimated_time: Duration) -> Self {
        Self {
            is_final: false,
            anchor_confirmations,
            required_confirmations: constants::FINALITY_ANCHORS,
            estimated_finality_time: Some(estimated_time),
        }
    }
}

/// Damage types for regeneration (DNA-inspired)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DamageType {
    /// Single corrupted byte (analogous to BER - Base Excision Repair)
    SingleNucleotide,

    /// Corrupted segment (analogous to NER - Nucleotide Excision Repair)
    SegmentCorruption,

    /// Transmission error (analogous to MMR - Mismatch Repair)
    MismatchError,

    /// Major damage (analogous to DSB repair - Double Strand Break)
    SevereCorruption,

    /// String lost entirely
    CompleteLoss,
}

/// Erasure reason for CEP (Controlled Erasure Protocol)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErasureReason {
    /// Data owner initiated
    OwnerRequest,

    /// GDPR right-to-be-forgotten
    GDPRRequest,

    /// Automatic expiration (TimeBound)
    TimeBoundExpiry,

    /// Condition met (ConditionalErasure)
    ConditionalTrigger,

    /// Legal requirement
    CourtOrder,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_id_creation() {
        let content = b"test content";
        let id = StringId::from_content(content);

        assert_ne!(id, StringId::ZERO);
        assert_eq!(id.as_bytes().len(), 32);
    }

    #[test]
    fn test_string_id_hex_roundtrip() {
        let content = b"test content";
        let id = StringId::from_content(content);
        let hex = id.to_hex();
        let parsed = StringId::from_hex(&hex).unwrap();

        assert_eq!(id, parsed);
    }

    #[test]
    fn test_node_id_from_public_key() {
        let public_key = [0u8; 32];
        let node_id = NodeId::from_public_key(&public_key);

        assert_eq!(node_id.as_bytes().len(), 32);
    }
}
