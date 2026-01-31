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
    pub(crate) signer: HybridSigner,

    /// Public key for verification
    pub(crate) public_key: HybridPublicKey,
}

impl KeyPair {
    /// Generate a new random keypair (Ed25519 only)
    pub fn generate() -> anyhow::Result<Self> {
        let (signer, public_key) = HybridSigner::generate();
        Ok(Self { signer, public_key })
    }

    /// Generate hybrid quantum-resistant keypair (Ed25519 + Dilithium3)
    pub fn generate_hybrid() -> anyhow::Result<Self> {
        let (signer, public_key) = HybridSigner::generate();
        Ok(Self { signer, public_key })
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

    /// Export private key bytes
    pub fn private_key_bytes(&self) -> Vec<u8> {
        self.signer.secret_key_bytes()
    }

    /// Export public key bytes
    pub fn public_key_bytes(&self) -> Vec<u8> {
        self.public_key.to_bytes()
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
    ///
    /// This generates a cryptographically random seed and derives the primary
    /// keypair from it. The seed is stored for deriving child keys.
    pub fn new() -> Self {
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        Self::from_seed(seed)
    }

    /// Create from seed (deterministic key generation)
    ///
    /// The primary keypair is derived deterministically from the seed.
    /// This allows for key recovery if the seed is backed up.
    ///
    /// # Security Note
    /// The seed MUST be cryptographically random and kept secret.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        // Derive primary key seed from master seed
        let primary_seed = {
            let mut input = seed.to_vec();
            input.extend_from_slice(b"primary_keypair");
            *blake3::hash(&input).as_bytes()
        };

        let (signer, public_key) = HybridSigner::from_seed(&primary_seed);
        let primary = KeyPair { signer, public_key };

        Self { primary, seed }
    }

    /// Get primary keypair
    pub fn primary(&self) -> &KeyPair {
        &self.primary
    }

    /// Get the master seed (for backup purposes)
    ///
    /// # Security Warning
    /// This returns the master seed. Handle with extreme care.
    /// Anyone with this seed can derive all keys.
    pub fn seed(&self) -> &[u8; 32] {
        &self.seed
    }

    /// Derive a child key for specific purpose
    pub fn derive_key(&self, purpose: &str) -> [u8; 32] {
        crate::hash::derive_key(purpose, &self.seed)
    }

    /// Derive a child keypair for specific purpose
    ///
    /// This creates a deterministic keypair for a specific purpose,
    /// allowing for key hierarchies.
    pub fn derive_keypair(&self, purpose: &str) -> KeyPair {
        let child_seed = self.derive_key(purpose);
        let (signer, public_key) = HybridSigner::from_seed(&child_seed);
        KeyPair { signer, public_key }
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
        let keypair = KeyPair::generate().unwrap();
        assert_ne!(keypair.public_key().ed25519, [0u8; 32]);
    }

    #[test]
    fn test_keypair_signing() {
        let keypair = KeyPair::generate().unwrap();
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
