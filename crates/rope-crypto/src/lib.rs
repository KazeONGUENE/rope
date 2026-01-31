//! # Datachain Rope Cryptography
//!
//! Cryptographic primitives for Datachain Rope including:
//! - Organic Encryption System (OES) - Self-evolving post-quantum crypto
//! - Hybrid signatures (Ed25519 + CRYSTALS-Dilithium3)
//! - Hybrid key exchange (X25519 + CRYSTALS-Kyber768)
//! - BLAKE3 hashing utilities
//!
//! ## Security Model
//!
//! Datachain Rope implements hybrid classical/post-quantum cryptography
//! to ensure security against both current and future quantum threats.
//!
//! | Function | Algorithm | Security Level |
//! |----------|-----------|----------------|
//! | Signatures | Ed25519 + Dilithium3 | 256-bit + NIST PQ-3 |
//! | Hashing | BLAKE3 | 256-bit |
//! | Key Exchange | X25519 + Kyber768 | 256-bit + NIST PQ-3 |

pub mod error;
pub mod hash;
pub mod hybrid;
pub mod keys;
pub mod oes;

pub use error::*;
pub use hash::*;
pub use hybrid::*;
pub use keys::*;
pub use oes::*;

/// Cryptographic prelude
pub mod prelude {
    pub use crate::error::{CryptoError, Result};
    pub use crate::hash::{hash_blake3, hash_keyed};
    pub use crate::hybrid::{HybridKEM, HybridSigner, HybridVerifier};
    pub use crate::keys::{KeyPair, PublicKey, SecretKey};
    pub use crate::oes::{OESProof, OrganicEncryptionState};
}
