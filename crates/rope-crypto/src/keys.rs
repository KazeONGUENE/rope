//! Key management for Datachain Rope

// Keys management module

use crate::hybrid::{HybridPublicKey, HybridSecretKey, HybridSigner};

/// Re-export for convenience
pub use crate::hybrid::HybridPublicKey as PublicKey;

/// Secret key wrapper with zeroization
pub type SecretKey = HybridSecretKey;

/// Complete keypair for a node
pub struct KeyPair {
    /// Signer for creating signatures
    signer: HybridSigner,
    
    /// Public key for verification
    public_key: HybridPublicKey,
}

impl KeyPair {
    /// Generate a new random keypair
    pub fn generate() -> Self {
        let (signer, public_key) = HybridSigner::generate();
        Self { signer, public_key }
    }

    /// Get the public key
    pub fn public_key(&self) -> &HybridPublicKey {
        &self.public_key
    }

    /// Get the signer
    pub fn signer(&self) -> &HybridSigner {
        &self.signer
    }

    /// Sign a message
    pub fn sign(&self, message: &[u8]) -> crate::hybrid::HybridSignature {
        self.signer.sign(message)
    }

    /// Get node ID from public key
    pub fn node_id(&self) -> [u8; 32] {
        self.public_key.node_id()
    }
}

/// Keystore for managing multiple keys
pub struct KeyStore {
    /// Primary signing keypair
    primary: KeyPair,
    
    /// Key derivation seed
    seed: [u8; 32],
}

impl KeyStore {
    /// Create new keystore with random seed
    pub fn new() -> Self {
        let seed: [u8; 32] = rand::random();
        let primary = KeyPair::generate();
        Self { primary, seed }
    }

    /// Create from seed
    pub fn from_seed(seed: [u8; 32]) -> Self {
        // In production: derive keypair from seed deterministically
        let primary = KeyPair::generate();
        Self { primary, seed }
    }

    /// Get primary keypair
    pub fn primary(&self) -> &KeyPair {
        &self.primary
    }

    /// Derive a child key for specific purpose
    pub fn derive_key(&self, purpose: &str) -> [u8; 32] {
        crate::hash::derive_key(purpose, &self.seed)
    }
}

impl Default for KeyStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generation() {
        let keypair = KeyPair::generate();
        assert_ne!(keypair.public_key().ed25519, [0u8; 32]);
    }

    #[test]
    fn test_keypair_signing() {
        let keypair = KeyPair::generate();
        let message = b"Test message";
        
        let signature = keypair.sign(message);
        assert!(!signature.is_empty());
    }

    #[test]
    fn test_keystore() {
        let store = KeyStore::new();
        
        let key1 = store.derive_key("purpose1");
        let key2 = store.derive_key("purpose2");
        
        assert_ne!(key1, key2);
    }
}

