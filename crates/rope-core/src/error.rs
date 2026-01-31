//! Error types for Datachain Rope core operations

use crate::types::StringId;
use thiserror::Error;

/// Result type alias for Rope operations
pub type Result<T> = std::result::Result<T, RopeError>;

/// Errors that can occur in Datachain Rope core operations
#[derive(Error, Debug, Clone)]
pub enum RopeError {
    // === String Operations ===
    /// String not found in the lattice
    #[error("String not found: {0}")]
    StringNotFound(StringId),

    /// String has been erased
    #[error("String has been erased: {0}")]
    StringErased(StringId),

    /// Missing parent reference in DAG
    #[error("Missing parent string: {0}")]
    MissingParent(StringId),

    /// Parent string was erased
    #[error("Parent string was erased: {0}")]
    ParentErased(StringId),

    /// Content exceeds maximum size
    #[error("Content exceeds maximum size of {max} bytes")]
    ContentTooLarge { max: usize },

    // === Complement Operations ===
    /// Complement not found for string
    #[error("Complement not found for string: {0}")]
    ComplementNotFound(StringId),

    /// Complement verification failed
    #[error("Complement verification failed for string: {0}")]
    ComplementVerificationFailed(StringId),

    /// Entanglement proof invalid
    #[error("Invalid entanglement proof for string: {0}")]
    InvalidEntanglementProof(StringId),

    // === Regeneration ===
    /// Regeneration failed
    #[error("Failed to regenerate string: {0}")]
    RegenerationFailed(StringId),

    /// Insufficient sources for regeneration
    #[error("Insufficient sources for regeneration: need {required}, have {available}")]
    InsufficientSources { required: usize, available: usize },

    /// Regeneration blocked (string was erased)
    #[error("Regeneration blocked for erased string: {0}")]
    RegenerationBlocked(StringId),

    // === Cryptographic Errors ===
    /// Invalid signature
    #[error("Invalid signature on string")]
    InvalidSignature,

    /// Invalid OES generation
    #[error("OES generation {generation} is outside acceptable window")]
    InvalidOESGeneration { generation: u64 },

    /// OES evolution failed
    #[error("OES evolution failed: {0}")]
    OESEvolutionFailed(String),

    // === Consensus Errors ===
    /// Testimony verification failed
    #[error("Testimony verification failed")]
    TestimonyVerificationFailed,

    /// Quorum not met
    #[error("Quorum not met: need {required}, have {received}")]
    QuorumNotMet { required: u32, received: u32 },

    /// Invalid anchor string
    #[error("Invalid anchor string: {0}")]
    InvalidAnchor(String),

    // === Erasure Errors ===
    /// Unauthorized erasure attempt
    #[error("Unauthorized erasure attempt on string: {0}")]
    UnauthorizedErasure(StringId),

    /// Erasure not allowed for immutable string
    #[error("Cannot erase immutable string: {0}")]
    ImmutableString(StringId),

    /// Erasure already in progress
    #[error("Erasure already in progress for string: {0}")]
    ErasureInProgress(StringId),

    // === Network Errors ===
    /// Node not found
    #[error("Node not found: {0}")]
    NodeNotFound(String),

    /// Connection failed
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    /// Rate limit exceeded
    #[error("Rate limit exceeded: {limit} per second")]
    RateLimitExceeded { limit: u32 },

    // === Storage Errors ===
    /// Storage error
    #[error("Storage error: {0}")]
    StorageError(String),

    /// Serialization error
    #[error("Serialization error: {0}")]
    SerializationError(String),

    // === General Errors ===
    /// Invalid input
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Error codes matching the API specification
impl RopeError {
    /// Get the error code for API responses
    pub fn code(&self) -> u32 {
        match self {
            Self::StringNotFound(_) => 1001,
            Self::StringErased(_) => 1002,
            Self::MissingParent(_) | Self::ParentErased(_) => 1003,
            Self::InvalidOESGeneration { .. } => 1004,
            Self::InvalidSignature => 1005,
            Self::UnauthorizedErasure(_) | Self::ImmutableString(_) => 1006,
            Self::RegenerationFailed(_) | Self::InsufficientSources { .. } => 1007,
            Self::QuorumNotMet { .. } => 1008,
            _ => 9999,
        }
    }

    /// Check if error is recoverable
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            Self::RateLimitExceeded { .. }
                | Self::QuorumNotMet { .. }
                | Self::InsufficientSources { .. }
                | Self::ConnectionFailed(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        let err = RopeError::StringNotFound(StringId::from_content(b"test"));
        assert_eq!(err.code(), 1001);

        let err = RopeError::InvalidSignature;
        assert_eq!(err.code(), 1005);
    }

    #[test]
    fn test_error_display() {
        let id = StringId::from_content(b"test");
        let err = RopeError::StringNotFound(id);

        let msg = format!("{}", err);
        assert!(msg.contains("String not found"));
    }

    #[test]
    fn test_recoverable_errors() {
        assert!(RopeError::RateLimitExceeded { limit: 1000 }.is_recoverable());
        assert!(!RopeError::InvalidSignature.is_recoverable());
    }
}
