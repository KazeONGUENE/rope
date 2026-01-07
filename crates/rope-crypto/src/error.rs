//! Cryptographic error types

use thiserror::Error;

/// Result type for cryptographic operations
pub type Result<T> = std::result::Result<T, CryptoError>;

/// Errors in cryptographic operations
#[derive(Error, Debug, Clone)]
pub enum CryptoError {
    /// Invalid public key
    #[error("Invalid public key: {0}")]
    InvalidPublicKey(String),
    
    /// Invalid secret key
    #[error("Invalid secret key: {0}")]
    InvalidSecretKey(String),
    
    /// Invalid signature
    #[error("Invalid signature: {0}")]
    InvalidSignature(String),
    
    /// Signature verification failed
    #[error("Signature verification failed")]
    VerificationFailed,
    
    /// Key derivation failed
    #[error("Key derivation failed: {0}")]
    KeyDerivationFailed(String),
    
    /// OES error
    #[error("OES error: {0}")]
    OESError(String),
    
    /// Encapsulation failed
    #[error("Key encapsulation failed: {0}")]
    EncapsulationFailed(String),
    
    /// Decapsulation failed
    #[error("Key decapsulation failed: {0}")]
    DecapsulationFailed(String),
    
    /// Random number generation failed
    #[error("RNG failed: {0}")]
    RNGFailed(String),
}

