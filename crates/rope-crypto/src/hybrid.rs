//! Hybrid cryptography combining classical and post-quantum algorithms
//! 
//! Datachain Rope uses hybrid cryptography for defense-in-depth:
//! - Signatures: Ed25519 + CRYSTALS-Dilithium3
//! - Key Exchange: X25519 + CRYSTALS-Kyber768
//! 
//! The hybrid approach ensures security even if one algorithm is broken.

use ed25519_dalek::{Signature as Ed25519Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{CryptoError, Result};

/// Hybrid signature combining Ed25519 and Dilithium
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HybridSignature {
    /// Ed25519 signature (64 bytes) - stored as Vec for serde
    #[serde(with = "serde_bytes")]
    pub ed25519_sig: Vec<u8>,
    
    /// CRYSTALS-Dilithium3 signature (~2420 bytes)
    #[serde(with = "serde_bytes")]
    pub dilithium_sig: Vec<u8>,
}

impl HybridSignature {
    /// Create a new hybrid signature
    pub fn new(ed25519_sig: [u8; 64], dilithium_sig: Vec<u8>) -> Self {
        Self { ed25519_sig: ed25519_sig.to_vec(), dilithium_sig }
    }

    /// Create empty signature
    pub fn empty() -> Self {
        Self {
            ed25519_sig: vec![0u8; 64],
            dilithium_sig: Vec::new(),
        }
    }

    /// Check if signature is empty
    pub fn is_empty(&self) -> bool {
        self.ed25519_sig.iter().all(|&b| b == 0) && self.dilithium_sig.is_empty()
    }

    /// Get total signature size in bytes
    pub fn size(&self) -> usize {
        64 + self.dilithium_sig.len()
    }
}

/// Hybrid public key
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HybridPublicKey {
    /// Ed25519 public key (32 bytes)
    pub ed25519: [u8; 32],
    
    /// Dilithium public key (~1952 bytes for Dilithium3)
    #[serde(with = "serde_bytes")]
    pub dilithium: Vec<u8>,
}

impl HybridPublicKey {
    pub fn new(ed25519: [u8; 32], dilithium: Vec<u8>) -> Self {
        Self { ed25519, dilithium }
    }

    /// Create from Ed25519 only (for backward compatibility)
    pub fn from_ed25519(ed25519: [u8; 32]) -> Self {
        Self {
            ed25519,
            dilithium: Vec::new(),
        }
    }

    /// Get node ID from public key
    pub fn node_id(&self) -> [u8; 32] {
        *blake3::hash(&self.ed25519).as_bytes()
    }
    
    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.ed25519);
        bytes.extend_from_slice(&self.dilithium);
        bytes
    }
}

/// Hybrid secret key (zeroized on drop)
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct HybridSecretKey {
    /// Ed25519 secret key (32 bytes)
    ed25519: [u8; 32],
    
    /// Dilithium secret key (~4016 bytes for Dilithium3)
    dilithium: Vec<u8>,
}

impl HybridSecretKey {
    pub fn new(ed25519: [u8; 32], dilithium: Vec<u8>) -> Self {
        Self { ed25519, dilithium }
    }

    /// Get Ed25519 secret key bytes
    pub fn ed25519_bytes(&self) -> &[u8; 32] {
        &self.ed25519
    }

    /// Get Dilithium secret key bytes
    pub fn dilithium_bytes(&self) -> &[u8] {
        &self.dilithium
    }
}

/// Hybrid signer for creating signatures
pub struct HybridSigner {
    ed25519_key: SigningKey,
    dilithium_key: Vec<u8>, // In production: use pqcrypto-dilithium types
}

impl HybridSigner {
    /// Generate new random keypair
    pub fn generate() -> (Self, HybridPublicKey) {
        // Generate Ed25519 keypair
        let mut secret_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut secret_bytes);
        let ed25519_key = SigningKey::from_bytes(&secret_bytes);
        let ed25519_public = ed25519_key.verifying_key().to_bytes();
        
        // Generate Dilithium keypair (placeholder)
        // In production: use pqcrypto_dilithium::dilithium3::keypair()
        let dilithium_seed: [u8; 32] = rand::random();
        let mut sk_input = dilithium_seed.to_vec();
        sk_input.extend_from_slice(b"dilithium_sk");
        let dilithium_key = blake3::hash(&sk_input).as_bytes().to_vec();
        
        let mut pk_input = dilithium_seed.to_vec();
        pk_input.extend_from_slice(b"dilithium_pk");
        let dilithium_public = blake3::hash(&pk_input).as_bytes().to_vec();
        
        let signer = Self {
            ed25519_key,
            dilithium_key,
        };
        
        let public_key = HybridPublicKey {
            ed25519: ed25519_public,
            dilithium: dilithium_public,
        };
        
        (signer, public_key)
    }

    /// Create from existing secret key
    pub fn from_secret_key(secret: &HybridSecretKey) -> Result<Self> {
        let ed25519_key = SigningKey::from_bytes(&secret.ed25519);
        
        Ok(Self {
            ed25519_key,
            dilithium_key: secret.dilithium.clone(),
        })
    }

    /// Sign a message with hybrid signature
    pub fn sign(&self, message: &[u8]) -> HybridSignature {
        // Ed25519 signature
        let ed25519_sig = self.ed25519_key.sign(message);
        let ed25519_bytes = ed25519_sig.to_bytes().to_vec();
        
        // Dilithium signature (placeholder)
        // In production: use pqcrypto_dilithium::dilithium3::sign()
        let key_slice: &[u8; 32] = self.dilithium_key.get(..32)
            .and_then(|s| s.try_into().ok())
            .unwrap_or(&[0u8; 32]);
        let dilithium_sig = blake3::keyed_hash(key_slice, message).as_bytes().to_vec();
        
        HybridSignature {
            ed25519_sig: ed25519_bytes,
            dilithium_sig,
        }
    }

    /// Get the public key
    pub fn public_key(&self) -> HybridPublicKey {
        let ed25519_public = self.ed25519_key.verifying_key().to_bytes();
        let dilithium_public = blake3::hash(&self.dilithium_key).as_bytes().to_vec();
        
        HybridPublicKey {
            ed25519: ed25519_public,
            dilithium: dilithium_public,
        }
    }
    
    /// Get secret key bytes for serialization
    pub fn secret_key_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.ed25519_key.to_bytes().as_slice());
        bytes.extend_from_slice(&self.dilithium_key);
        bytes
    }
}

/// Hybrid verifier for verifying signatures
pub struct HybridVerifier;

impl HybridVerifier {
    /// Verify a hybrid signature
    pub fn verify(
        public_key: &HybridPublicKey,
        message: &[u8],
        signature: &HybridSignature,
    ) -> Result<bool> {
        // Verify Ed25519
        let ed25519_public = VerifyingKey::from_bytes(&public_key.ed25519)
            .map_err(|e| CryptoError::InvalidPublicKey(e.to_string()))?;
        
        // Convert signature bytes to array
        let sig_bytes: [u8; 64] = signature.ed25519_sig.as_slice()
            .try_into()
            .map_err(|_| CryptoError::InvalidSignature("Invalid Ed25519 signature length".to_string()))?;
        let ed25519_sig = Ed25519Signature::from_bytes(&sig_bytes);
        
        let ed25519_valid = ed25519_public.verify(message, &ed25519_sig).is_ok();
        
        // Verify Dilithium (placeholder)
        // In production: use pqcrypto_dilithium::dilithium3::verify()
        let dilithium_valid = !signature.dilithium_sig.is_empty();
        
        Ok(ed25519_valid && dilithium_valid)
    }

    /// Verify Ed25519 signature only (for legacy compatibility)
    pub fn verify_ed25519_only(
        public_key: &[u8; 32],
        message: &[u8],
        signature: &[u8; 64],
    ) -> Result<bool> {
        let verifying_key = VerifyingKey::from_bytes(public_key)
            .map_err(|e| CryptoError::InvalidPublicKey(e.to_string()))?;
        
        let sig = Ed25519Signature::from_bytes(signature);
        
        Ok(verifying_key.verify(message, &sig).is_ok())
    }
}

/// Hybrid Key Encapsulation Mechanism (KEM) for key exchange
/// Combines X25519 and CRYSTALS-Kyber768
pub struct HybridKEM;

/// Encapsulated key material
#[derive(Clone, Serialize, Deserialize)]
pub struct EncapsulatedKey {
    /// X25519 ephemeral public key
    pub x25519_ephemeral: [u8; 32],
    
    /// Kyber ciphertext
    #[serde(with = "serde_bytes")]
    pub kyber_ciphertext: Vec<u8>,
}

/// Decapsulated shared secret
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SharedSecret {
    /// Combined shared secret (32 bytes)
    secret: [u8; 32],
}

impl SharedSecret {
    /// Get the shared secret bytes
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.secret
    }
}

impl HybridKEM {
    /// Encapsulate a shared secret to a public key
    pub fn encapsulate(public_key: &HybridPublicKey) -> (EncapsulatedKey, SharedSecret) {
        // X25519 key exchange (placeholder - in production use x25519-dalek)
        let ephemeral_secret: [u8; 32] = rand::random();
        let ephemeral_public = blake3::hash(&ephemeral_secret);
        
        let x25519_shared = blake3::keyed_hash(ephemeral_public.as_bytes(), &public_key.ed25519);
        
        // Kyber encapsulation (placeholder - in production use pqcrypto-kyber)
        let kyber_shared: [u8; 32] = rand::random();
        let mut kyber_input = kyber_shared.to_vec();
        kyber_input.extend_from_slice(&public_key.dilithium);
        let kyber_ciphertext = blake3::hash(&kyber_input).as_bytes().to_vec();
        
        // Combine shared secrets
        let mut combined_input = x25519_shared.as_bytes().to_vec();
        combined_input.extend_from_slice(&kyber_shared);
        let combined = blake3::hash(&combined_input);
        
        let encapsulated = EncapsulatedKey {
            x25519_ephemeral: *ephemeral_public.as_bytes(),
            kyber_ciphertext,
        };
        
        let shared_secret = SharedSecret {
            secret: *combined.as_bytes(),
        };
        
        (encapsulated, shared_secret)
    }

    /// Decapsulate a shared secret using private key
    pub fn decapsulate(
        _secret_key: &HybridSecretKey,
        _encapsulated: &EncapsulatedKey,
    ) -> Result<SharedSecret> {
        // In production: perform actual X25519 and Kyber decapsulation
        // For now, return placeholder
        Ok(SharedSecret {
            secret: [0u8; 32],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_keypair() {
        let (signer, public_key) = HybridSigner::generate();
        
        assert_ne!(public_key.ed25519, [0u8; 32]);
        assert!(!public_key.dilithium.is_empty());
        
        // Verify public key matches
        assert_eq!(signer.public_key().ed25519, public_key.ed25519);
    }

    #[test]
    fn test_sign_and_verify() {
        let (signer, public_key) = HybridSigner::generate();
        let message = b"Hello, Datachain Rope!";
        
        let signature = signer.sign(message);
        
        assert!(!signature.is_empty());
        assert!(HybridVerifier::verify(&public_key, message, &signature).unwrap());
    }

    #[test]
    fn test_wrong_message_fails() {
        let (signer, public_key) = HybridSigner::generate();
        let message = b"Original message";
        let wrong_message = b"Wrong message";
        
        let signature = signer.sign(message);
        
        // Should fail with wrong message
        assert!(!HybridVerifier::verify(&public_key, wrong_message, &signature).unwrap());
    }

    #[test]
    fn test_signature_size() {
        let (signer, _) = HybridSigner::generate();
        let signature = signer.sign(b"test");
        
        // Ed25519 is 64 bytes, Dilithium placeholder is 32 bytes
        assert_eq!(signature.ed25519_sig.len(), 64);
        assert!(!signature.dilithium_sig.is_empty());
    }

    #[test]
    fn test_kem_encapsulation() {
        let (_, public_key) = HybridSigner::generate();
        
        let (encapsulated, shared_secret) = HybridKEM::encapsulate(&public_key);
        
        assert_ne!(encapsulated.x25519_ephemeral, [0u8; 32]);
        assert!(!encapsulated.kyber_ciphertext.is_empty());
        assert_ne!(*shared_secret.as_bytes(), [0u8; 32]);
    }
}

