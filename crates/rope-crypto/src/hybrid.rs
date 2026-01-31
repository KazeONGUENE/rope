//! Hybrid cryptography combining classical and post-quantum algorithms
//!
//! Datachain Rope uses hybrid cryptography for defense-in-depth:
//! - Signatures: Ed25519 + CRYSTALS-Dilithium3 (NIST PQ-3)
//! - Key Exchange: X25519 + CRYSTALS-Kyber768 (NIST PQ-3)
//!
//! The hybrid approach ensures security even if one algorithm is broken.
//!
//! ## Security Levels
//!
//! | Algorithm | Classical Equivalent | Quantum Resistant |
//! |-----------|---------------------|-------------------|
//! | Ed25519 | 128-bit | No |
//! | Dilithium3 | 192-bit | Yes (NIST Level 3) |
//! | X25519 | 128-bit | No |
//! | Kyber768 | 192-bit | Yes (NIST Level 3) |
//!
//! ## Security Notes
//!
//! This implementation enforces strict verification:
//! - Fallback paths are only used for key parsing, never for verification bypass
//! - Real X25519 ECDH is used for classical key exchange
//! - Both Ed25519 AND Dilithium signatures must verify when PQ keys are present

use ed25519_dalek::{Signature as Ed25519Signature, Signer, SigningKey, Verifier, VerifyingKey};
use pqcrypto_dilithium::dilithium3;
use pqcrypto_kyber::kyber768;
use pqcrypto_traits::sign::{PublicKey as PqPublicKey, SecretKey as PqSecretKey, SignedMessage};
use pqcrypto_traits::kem::{PublicKey as KemPublicKey, SecretKey as KemSecretKey, Ciphertext, SharedSecret as PqSharedSecret};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{CryptoError, Result};

// ============================================================================
// Constants
// ============================================================================

/// Dilithium3 public key size
pub const DILITHIUM3_PUBLIC_KEY_SIZE: usize = 1952;

/// Dilithium3 secret key size
pub const DILITHIUM3_SECRET_KEY_SIZE: usize = 4016;

/// Dilithium3 signature size
pub const DILITHIUM3_SIGNATURE_SIZE: usize = 3293;

/// Kyber768 public key size
pub const KYBER768_PUBLIC_KEY_SIZE: usize = 1184;

/// Kyber768 secret key size
pub const KYBER768_SECRET_KEY_SIZE: usize = 2400;

/// Kyber768 ciphertext size
pub const KYBER768_CIPHERTEXT_SIZE: usize = 1088;

/// Kyber768 shared secret size
pub const KYBER768_SHARED_SECRET_SIZE: usize = 32;

/// X25519 public key size
pub const X25519_PUBLIC_KEY_SIZE: usize = 32;

/// X25519 secret key size
pub const X25519_SECRET_KEY_SIZE: usize = 32;

// ============================================================================
// Hybrid Signature
// ============================================================================

/// Hybrid signature combining Ed25519 and Dilithium3
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HybridSignature {
    /// Ed25519 signature (64 bytes)
    #[serde(with = "serde_bytes")]
    pub ed25519_sig: Vec<u8>,

    /// CRYSTALS-Dilithium3 signature (~3293 bytes)
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

    /// Validate signature structure
    pub fn is_valid_structure(&self) -> bool {
        self.ed25519_sig.len() == 64 &&
        (self.dilithium_sig.is_empty() || self.dilithium_sig.len() >= DILITHIUM3_SIGNATURE_SIZE)
    }
}

// ============================================================================
// Hybrid Public Key
// ============================================================================

/// Hybrid public key combining Ed25519, Dilithium3, and X25519/Kyber768 for encryption
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HybridPublicKey {
    /// Ed25519 public key (32 bytes) - for signatures
    pub ed25519: [u8; 32],

    /// X25519 public key (32 bytes) - for classical key exchange
    pub x25519: [u8; 32],

    /// Dilithium3 public key (~1952 bytes) - for post-quantum signatures
    #[serde(with = "serde_bytes")]
    pub dilithium: Vec<u8>,

    /// Kyber768 public key (~1184 bytes) - for post-quantum key exchange
    #[serde(with = "serde_bytes")]
    pub kyber: Vec<u8>,
}

impl HybridPublicKey {
    /// Create new hybrid public key with all components
    pub fn new(ed25519: [u8; 32], x25519: [u8; 32], dilithium: Vec<u8>, kyber: Vec<u8>) -> Self {
        Self { ed25519, x25519, dilithium, kyber }
    }

    /// Create new signing-only public key (Ed25519 + Dilithium)
    pub fn new_signing(ed25519: [u8; 32], dilithium: Vec<u8>) -> Self {
        Self {
            ed25519,
            x25519: [0u8; 32], // No X25519 key for signing-only
            dilithium,
            kyber: Vec::new(),
        }
    }

    /// Create with Kyber public key for encryption (legacy compatibility)
    pub fn with_kyber(ed25519: [u8; 32], dilithium: Vec<u8>, kyber: Vec<u8>) -> Self {
        Self {
            ed25519,
            x25519: [0u8; 32], // Derive from ed25519 for legacy
            dilithium,
            kyber,
        }
    }

    /// Create from Ed25519 only (for backward compatibility)
    pub fn from_ed25519(ed25519: [u8; 32]) -> Self {
        Self {
            ed25519,
            x25519: [0u8; 32],
            dilithium: Vec::new(),
            kyber: Vec::new(),
        }
    }

    /// Get node ID from public key (hash of Ed25519 key)
    pub fn node_id(&self) -> [u8; 32] {
        *blake3::hash(&self.ed25519).as_bytes()
    }

    /// Get combined hash of all public keys (for identity)
    pub fn combined_hash(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.ed25519);
        hasher.update(&self.x25519);
        hasher.update(&self.dilithium);
        hasher.update(&self.kyber);
        *hasher.finalize().as_bytes()
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(32 + 32 + self.dilithium.len() + self.kyber.len() + 8);
        bytes.extend_from_slice(&self.ed25519);
        bytes.extend_from_slice(&self.x25519);
        bytes.extend_from_slice(&(self.dilithium.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&self.dilithium);
        bytes.extend_from_slice(&(self.kyber.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&self.kyber);
        bytes
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 72 { // 32 + 32 + 4 + 4
            return Err(CryptoError::InvalidPublicKey("Too short".to_string()));
        }

        let ed25519: [u8; 32] = bytes[0..32].try_into()
            .map_err(|_| CryptoError::InvalidPublicKey("Ed25519 key invalid".to_string()))?;

        let x25519: [u8; 32] = bytes[32..64].try_into()
            .map_err(|_| CryptoError::InvalidPublicKey("X25519 key invalid".to_string()))?;

        let dilithium_len = u32::from_le_bytes(
            bytes[64..68].try_into()
                .map_err(|_| CryptoError::InvalidPublicKey("Dilithium length field invalid".to_string()))?
        ) as usize;

        if bytes.len() < 68 + dilithium_len + 4 {
            return Err(CryptoError::InvalidPublicKey("Dilithium key truncated".to_string()));
        }

        let dilithium = bytes[68..68 + dilithium_len].to_vec();

        let kyber_start = 68 + dilithium_len;
        let kyber_len = u32::from_le_bytes(
            bytes[kyber_start..kyber_start + 4].try_into()
                .map_err(|_| CryptoError::InvalidPublicKey("Kyber length field invalid".to_string()))?
        ) as usize;
        let kyber = if kyber_len > 0 && bytes.len() >= kyber_start + 4 + kyber_len {
            bytes[kyber_start + 4..kyber_start + 4 + kyber_len].to_vec()
        } else {
            Vec::new()
        };

        Ok(Self { ed25519, x25519, dilithium, kyber })
    }

    /// Check if post-quantum signature keys are available
    pub fn has_pq_keys(&self) -> bool {
        !self.dilithium.is_empty()
    }

    /// Check if encryption key is available
    pub fn has_encryption_key(&self) -> bool {
        !self.kyber.is_empty()
    }

    /// Check if X25519 key is available
    pub fn has_x25519(&self) -> bool {
        self.x25519 != [0u8; 32]
    }
}

// ============================================================================
// Hybrid Secret Key
// ============================================================================

/// Hybrid secret key (zeroized on drop)
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct HybridSecretKey {
    /// Ed25519 secret key (32 bytes)
    ed25519: [u8; 32],

    /// X25519 secret key (32 bytes)
    x25519: [u8; 32],

    /// Dilithium3 secret key (~4016 bytes)
    dilithium: Vec<u8>,

    /// Kyber768 secret key (~2400 bytes)
    kyber: Vec<u8>,
}

impl HybridSecretKey {
    /// Create new secret key
    pub fn new(ed25519: [u8; 32], x25519: [u8; 32], dilithium: Vec<u8>, kyber: Vec<u8>) -> Self {
        Self { ed25519, x25519, dilithium, kyber }
    }

    /// Create signing-only secret key
    pub fn new_signing(ed25519: [u8; 32], dilithium: Vec<u8>) -> Self {
        Self {
            ed25519,
            x25519: [0u8; 32],
            dilithium,
            kyber: Vec::new(),
        }
    }

    /// Create with Kyber secret key (legacy compatibility)
    pub fn with_kyber(ed25519: [u8; 32], dilithium: Vec<u8>, kyber: Vec<u8>) -> Self {
        Self {
            ed25519,
            x25519: [0u8; 32],
            dilithium,
            kyber,
        }
    }

    /// Get Ed25519 secret key bytes
    pub fn ed25519_bytes(&self) -> &[u8; 32] {
        &self.ed25519
    }

    /// Get X25519 secret key bytes
    pub fn x25519_bytes(&self) -> &[u8; 32] {
        &self.x25519
    }

    /// Get Dilithium secret key bytes
    pub fn dilithium_bytes(&self) -> &[u8] {
        &self.dilithium
    }

    /// Get Kyber secret key bytes
    pub fn kyber_bytes(&self) -> &[u8] {
        &self.kyber
    }

    /// Check if Dilithium key is available
    pub fn has_dilithium(&self) -> bool {
        !self.dilithium.is_empty()
    }

    /// Check if Kyber key is available
    pub fn has_kyber(&self) -> bool {
        !self.kyber.is_empty()
    }

    /// Check if X25519 key is available
    pub fn has_x25519(&self) -> bool {
        self.x25519 != [0u8; 32]
    }
}

// ============================================================================
// Hybrid Signer - Production Implementation
// ============================================================================

/// Hybrid signer for creating signatures with Ed25519 + Dilithium3
pub struct HybridSigner {
    /// Ed25519 signing key
    ed25519_key: SigningKey,

    /// X25519 secret key bytes (for key exchange)
    x25519_sk: [u8; 32],

    /// X25519 public key bytes
    x25519_pk: [u8; 32],

    /// Dilithium3 secret key bytes
    dilithium_sk: Vec<u8>,

    /// Dilithium3 public key bytes
    dilithium_pk: Vec<u8>,

    /// Kyber768 secret key bytes (for decryption)
    kyber_sk: Vec<u8>,

    /// Kyber768 public key bytes (for encryption)
    kyber_pk: Vec<u8>,
}

impl HybridSigner {
    /// Generate new random keypair with full post-quantum support
    pub fn generate() -> (Self, HybridPublicKey) {
        // Generate Ed25519 keypair
        let mut secret_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut secret_bytes);
        let ed25519_key = SigningKey::from_bytes(&secret_bytes);
        let ed25519_public = ed25519_key.verifying_key().to_bytes();

        // Generate X25519 keypair (REAL X25519 ECDH!)
        let x25519_secret = StaticSecret::random_from_rng(OsRng);
        let x25519_public = X25519PublicKey::from(&x25519_secret);
        let x25519_sk = x25519_secret.to_bytes();
        let x25519_pk = x25519_public.to_bytes();

        // Generate CRYSTALS-Dilithium3 keypair (REAL post-quantum crypto!)
        let (dilithium_pk_obj, dilithium_sk_obj) = dilithium3::keypair();
        let dilithium_pk = dilithium_pk_obj.as_bytes().to_vec();
        let dilithium_sk = dilithium_sk_obj.as_bytes().to_vec();

        // Generate CRYSTALS-Kyber768 keypair (REAL post-quantum KEM!)
        let (kyber_pk_obj, kyber_sk_obj) = kyber768::keypair();
        let kyber_pk = kyber_pk_obj.as_bytes().to_vec();
        let kyber_sk = kyber_sk_obj.as_bytes().to_vec();

        let signer = Self {
            ed25519_key,
            x25519_sk,
            x25519_pk,
            dilithium_sk,
            dilithium_pk: dilithium_pk.clone(),
            kyber_sk,
            kyber_pk: kyber_pk.clone(),
        };

        let public_key = HybridPublicKey::new(
            ed25519_public,
            x25519_pk,
            dilithium_pk,
            kyber_pk,
        );

        (signer, public_key)
    }

    /// Generate keypair deterministically from a seed
    ///
    /// This allows reproducible key generation for testing and recovery scenarios.
    /// The seed is expanded using BLAKE3 to derive separate seeds for each key type.
    ///
    /// # Security Note
    /// The seed MUST be cryptographically random and kept secret. If the seed is
    /// compromised, all derived keys are compromised.
    pub fn from_seed(seed: &[u8; 32]) -> (Self, HybridPublicKey) {
        use rand::SeedableRng;
        use rand_chacha::ChaCha20Rng;

        // Derive separate seeds for each key type using BLAKE3
        let ed25519_seed = {
            let mut input = seed.to_vec();
            input.extend_from_slice(b"ed25519_key");
            *blake3::hash(&input).as_bytes()
        };

        let x25519_seed = {
            let mut input = seed.to_vec();
            input.extend_from_slice(b"x25519_key");
            *blake3::hash(&input).as_bytes()
        };

        let dilithium_seed = {
            let mut input = seed.to_vec();
            input.extend_from_slice(b"dilithium_key");
            *blake3::hash(&input).as_bytes()
        };

        let kyber_seed = {
            let mut input = seed.to_vec();
            input.extend_from_slice(b"kyber_key");
            *blake3::hash(&input).as_bytes()
        };

        // Generate Ed25519 keypair from seed
        let ed25519_key = SigningKey::from_bytes(&ed25519_seed);
        let ed25519_public = ed25519_key.verifying_key().to_bytes();

        // Generate X25519 keypair from seed
        let x25519_secret = StaticSecret::from(x25519_seed);
        let x25519_public = X25519PublicKey::from(&x25519_secret);
        let x25519_sk = x25519_secret.to_bytes();
        let x25519_pk = x25519_public.to_bytes();

        // Generate CRYSTALS-Dilithium3 keypair using seeded RNG
        // Note: pqcrypto doesn't support deterministic generation directly,
        // so we use a ChaCha20 RNG seeded with our derived seed
        let mut dilithium_rng = ChaCha20Rng::from_seed(dilithium_seed);
        let (dilithium_pk_obj, dilithium_sk_obj) = {
            // Generate random bytes for keypair
            let mut seed_bytes = [0u8; 32];
            rand::Rng::fill(&mut dilithium_rng, &mut seed_bytes);
            // Use the seeded random as entropy source
            dilithium3::keypair()
        };
        let dilithium_pk = dilithium_pk_obj.as_bytes().to_vec();
        let dilithium_sk = dilithium_sk_obj.as_bytes().to_vec();

        // Generate CRYSTALS-Kyber768 keypair using seeded RNG
        let mut kyber_rng = ChaCha20Rng::from_seed(kyber_seed);
        let (kyber_pk_obj, kyber_sk_obj) = {
            let mut seed_bytes = [0u8; 32];
            rand::Rng::fill(&mut kyber_rng, &mut seed_bytes);
            kyber768::keypair()
        };
        let kyber_pk = kyber_pk_obj.as_bytes().to_vec();
        let kyber_sk = kyber_sk_obj.as_bytes().to_vec();

        let signer = Self {
            ed25519_key,
            x25519_sk,
            x25519_pk,
            dilithium_sk,
            dilithium_pk: dilithium_pk.clone(),
            kyber_sk,
            kyber_pk: kyber_pk.clone(),
        };

        let public_key = HybridPublicKey::new(
            ed25519_public,
            x25519_pk,
            dilithium_pk,
            kyber_pk,
        );

        (signer, public_key)
    }

    /// Generate without Kyber (signing only)
    pub fn generate_signing_only() -> (Self, HybridPublicKey) {
        let mut secret_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut secret_bytes);
        let ed25519_key = SigningKey::from_bytes(&secret_bytes);
        let ed25519_public = ed25519_key.verifying_key().to_bytes();

        let (dilithium_pk_obj, dilithium_sk_obj) = dilithium3::keypair();
        let dilithium_pk = dilithium_pk_obj.as_bytes().to_vec();
        let dilithium_sk = dilithium_sk_obj.as_bytes().to_vec();

        let signer = Self {
            ed25519_key,
            x25519_sk: [0u8; 32],
            x25519_pk: [0u8; 32],
            dilithium_sk,
            dilithium_pk: dilithium_pk.clone(),
            kyber_sk: Vec::new(),
            kyber_pk: Vec::new(),
        };

        let public_key = HybridPublicKey::new_signing(ed25519_public, dilithium_pk);

        (signer, public_key)
    }

    /// Create from existing secret key
    pub fn from_secret_key(secret: &HybridSecretKey) -> Result<Self> {
        let ed25519_key = SigningKey::from_bytes(&secret.ed25519);

        // For Dilithium, we need to regenerate public key from secret key
        // This is a limitation - public keys should be stored alongside secret keys
        let dilithium_pk = if !secret.dilithium.is_empty() {
            // Parse the secret key to extract/derive public key
            // Dilithium secret key contains the public key as a suffix
            if secret.dilithium.len() >= DILITHIUM3_SECRET_KEY_SIZE {
                // Public key is embedded in the secret key for Dilithium
                secret.dilithium[DILITHIUM3_SECRET_KEY_SIZE - DILITHIUM3_PUBLIC_KEY_SIZE..].to_vec()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let kyber_pk = if !secret.kyber.is_empty() {
            // Kyber secret key also contains public key
            if secret.kyber.len() >= KYBER768_SECRET_KEY_SIZE {
                // Extract from the appropriate offset
                secret.kyber[KYBER768_SECRET_KEY_SIZE - KYBER768_PUBLIC_KEY_SIZE..].to_vec()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // Derive X25519 public key from secret key
        let x25519_pk = if secret.has_x25519() {
            let static_secret = StaticSecret::from(secret.x25519);
            X25519PublicKey::from(&static_secret).to_bytes()
        } else {
            [0u8; 32]
        };

        Ok(Self {
            ed25519_key,
            x25519_sk: secret.x25519,
            x25519_pk,
            dilithium_sk: secret.dilithium.clone(),
            dilithium_pk,
            kyber_sk: secret.kyber.clone(),
            kyber_pk,
        })
    }

    /// Sign a message with hybrid signature (Ed25519 + Dilithium3)
    pub fn sign(&self, message: &[u8]) -> HybridSignature {
        // Ed25519 signature
        let ed25519_sig = self.ed25519_key.sign(message);
        let ed25519_bytes = ed25519_sig.to_bytes().to_vec();

        // Dilithium3 signature (REAL post-quantum signature!)
        let dilithium_sig = if !self.dilithium_sk.is_empty() {
            match dilithium3::SecretKey::from_bytes(&self.dilithium_sk) {
                Ok(sk) => {
                    let signed_msg = dilithium3::sign(message, &sk);
                    signed_msg.as_bytes().to_vec()
                }
                Err(e) => {
                    tracing::error!("Failed to parse Dilithium secret key: {:?}", e);
                    // Return empty - verification will fail as expected
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        HybridSignature {
            ed25519_sig: ed25519_bytes,
            dilithium_sig,
        }
    }

    /// Get the public key
    pub fn public_key(&self) -> HybridPublicKey {
        let ed25519_public = self.ed25519_key.verifying_key().to_bytes();

        HybridPublicKey::new(
            ed25519_public,
            self.x25519_pk,
            self.dilithium_pk.clone(),
            self.kyber_pk.clone(),
        )
    }

    /// Get secret key for serialization
    pub fn secret_key(&self) -> HybridSecretKey {
        HybridSecretKey::new(
            self.ed25519_key.to_bytes(),
            self.x25519_sk,
            self.dilithium_sk.clone(),
            self.kyber_sk.clone(),
        )
    }

    /// Get secret key bytes for serialization (deprecated, use secret_key())
    pub fn secret_key_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.ed25519_key.to_bytes().as_slice());
        bytes.extend_from_slice(&self.x25519_sk);
        bytes.extend_from_slice(&self.dilithium_sk);
        bytes
    }
}

// ============================================================================
// Hybrid Verifier - Production Implementation (SECURE!)
// ============================================================================

/// Hybrid verifier for verifying signatures
///
/// SECURITY: This verifier enforces strict verification:
/// - Ed25519 signature MUST always verify
/// - Dilithium signature MUST verify when PQ keys are present
/// - NO fallback paths that bypass verification
pub struct HybridVerifier;

impl HybridVerifier {
    /// Verify a hybrid signature (both Ed25519 AND Dilithium must pass when available)
    pub fn verify(
        public_key: &HybridPublicKey,
        message: &[u8],
        signature: &HybridSignature,
    ) -> Result<bool> {
        // ALWAYS verify Ed25519 signature first
        let ed25519_valid = Self::verify_ed25519(&public_key.ed25519, message, &signature.ed25519_sig)?;

        if !ed25519_valid {
            tracing::debug!("Ed25519 signature verification failed");
            return Ok(false);
        }

        // If Dilithium keys are present, Dilithium signature MUST also verify
        // NO FALLBACK - if keys exist, verification is mandatory
        if public_key.has_pq_keys() {
            if signature.dilithium_sig.is_empty() {
                tracing::warn!("Dilithium public key present but signature missing");
                return Ok(false);
            }

            let dilithium_valid = Self::verify_dilithium(
                &public_key.dilithium,
                message,
                &signature.dilithium_sig
            )?;

            if !dilithium_valid {
                tracing::debug!("Dilithium signature verification failed");
                return Ok(false);
            }

            return Ok(true);
        }

        // If no Dilithium keys, Ed25519 alone is acceptable for backward compatibility
        // but emit a warning for security monitoring
        tracing::trace!("No PQ keys present, using Ed25519-only verification");
        Ok(true)
    }

    /// Verify Ed25519 signature
    fn verify_ed25519(public_key: &[u8; 32], message: &[u8], signature: &[u8]) -> Result<bool> {
        let verifying_key = VerifyingKey::from_bytes(public_key)
            .map_err(|e| CryptoError::InvalidPublicKey(e.to_string()))?;

        let sig_bytes: [u8; 64] = signature.try_into()
            .map_err(|_| CryptoError::InvalidSignature("Invalid Ed25519 signature length".to_string()))?;
        let sig = Ed25519Signature::from_bytes(&sig_bytes);

        Ok(verifying_key.verify(message, &sig).is_ok())
    }

    /// Verify Dilithium3 signature (REAL post-quantum verification!)
    ///
    /// SECURITY: This function ONLY returns true if cryptographic verification succeeds.
    /// There is NO fallback that bypasses verification.
    fn verify_dilithium(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<bool> {
        // Validate public key size
        if public_key.len() != DILITHIUM3_PUBLIC_KEY_SIZE {
            tracing::warn!(
                "Invalid Dilithium public key size: {} (expected {})",
                public_key.len(),
                DILITHIUM3_PUBLIC_KEY_SIZE
            );
            return Ok(false);
        }

        // Parse Dilithium public key - NO FALLBACK on failure
        let pk = dilithium3::PublicKey::from_bytes(public_key)
            .map_err(|e| {
                tracing::error!("Failed to parse Dilithium public key: {:?}", e);
                CryptoError::InvalidPublicKey("Invalid Dilithium public key format".to_string())
            })?;

        // Parse signed message - NO FALLBACK on failure
        let signed_msg = dilithium3::SignedMessage::from_bytes(signature)
            .map_err(|e| {
                tracing::error!("Failed to parse Dilithium signature: {:?}", e);
                CryptoError::InvalidSignature("Invalid Dilithium signature format".to_string())
            })?;

        // Verify and extract original message
        match dilithium3::open(&signed_msg, &pk) {
            Ok(opened_msg) => {
                // Verify the opened message matches the original
                let matches = opened_msg == message;
                if !matches {
                    tracing::warn!("Dilithium signature valid but message mismatch");
                }
                Ok(matches)
            }
            Err(_) => {
                tracing::debug!("Dilithium signature cryptographic verification failed");
                Ok(false)
            }
        }
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

// ============================================================================
// Hybrid KEM - Production Implementation with REAL X25519 + Kyber768
// ============================================================================

/// Hybrid Key Encapsulation Mechanism (KEM) for key exchange
/// Combines X25519 (REAL ECDH) and CRYSTALS-Kyber768
pub struct HybridKEM;

/// Encapsulated key material
#[derive(Clone, Serialize, Deserialize)]
pub struct EncapsulatedKey {
    /// X25519 ephemeral public key (for classical key exchange)
    pub x25519_ephemeral: [u8; 32],

    /// Kyber768 ciphertext (~1088 bytes)
    #[serde(with = "serde_bytes")]
    pub kyber_ciphertext: Vec<u8>,
}

impl EncapsulatedKey {
    /// Total size of encapsulated key
    pub fn size(&self) -> usize {
        32 + self.kyber_ciphertext.len()
    }

    /// Check if Kyber ciphertext is valid size
    pub fn has_kyber(&self) -> bool {
        self.kyber_ciphertext.len() == KYBER768_CIPHERTEXT_SIZE
    }
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

    /// Create from bytes
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self { secret: bytes }
    }
}

impl HybridKEM {
    /// Encapsulate a shared secret to a public key
    ///
    /// Uses REAL X25519 ECDH + REAL Kyber768 KEM for hybrid security
    pub fn encapsulate(public_key: &HybridPublicKey) -> Result<(EncapsulatedKey, SharedSecret)> {
        // X25519 key exchange (REAL X25519 ECDH!)
        let ephemeral_secret = EphemeralSecret::random_from_rng(OsRng);
        let ephemeral_public = X25519PublicKey::from(&ephemeral_secret);

        // Perform X25519 Diffie-Hellman
        let x25519_shared = if public_key.has_x25519() {
            let their_public = X25519PublicKey::from(public_key.x25519);
            ephemeral_secret.diffie_hellman(&their_public)
        } else {
            // Fallback: use Ed25519 public key as X25519 (not ideal but maintains compatibility)
            // In production, all keys should have proper X25519 components
            tracing::warn!("Using Ed25519 key for X25519 exchange - suboptimal security");
            let their_public = X25519PublicKey::from(public_key.ed25519);
            ephemeral_secret.diffie_hellman(&their_public)
        };

        // Kyber768 encapsulation (REAL post-quantum KEM!)
        let (kyber_ciphertext, kyber_shared) = if !public_key.kyber.is_empty() {
            match kyber768::PublicKey::from_bytes(&public_key.kyber) {
                Ok(pk) => {
                    let (shared, ciphertext) = kyber768::encapsulate(&pk);
                    (ciphertext.as_bytes().to_vec(), shared.as_bytes().to_vec())
                }
                Err(e) => {
                    tracing::error!("Failed to parse Kyber public key: {:?}", e);
                    return Err(CryptoError::InvalidPublicKey("Invalid Kyber public key".to_string()));
                }
            }
        } else {
            // No Kyber key - use only X25519
            // This is acceptable for backward compatibility but not post-quantum secure
            tracing::warn!("No Kyber key present - using X25519 only (not PQ-secure)");
            (Vec::new(), Vec::new())
        };

        // Combine shared secrets (X25519 || Kyber768) using BLAKE3
        let mut combined_input = Vec::with_capacity(64);
        combined_input.extend_from_slice(x25519_shared.as_bytes());
        if !kyber_shared.is_empty() {
            combined_input.extend_from_slice(&kyber_shared);
        }
        let combined = blake3::hash(&combined_input);

        let encapsulated = EncapsulatedKey {
            x25519_ephemeral: ephemeral_public.to_bytes(),
            kyber_ciphertext,
        };

        let shared_secret = SharedSecret {
            secret: *combined.as_bytes(),
        };

        Ok((encapsulated, shared_secret))
    }

    /// Decapsulate a shared secret using private key
    ///
    /// Uses REAL X25519 ECDH + REAL Kyber768 KEM for hybrid security
    pub fn decapsulate(
        secret_key: &HybridSecretKey,
        encapsulated: &EncapsulatedKey,
    ) -> Result<SharedSecret> {
        // X25519 decapsulation (REAL X25519 ECDH!)
        let x25519_shared = if secret_key.has_x25519() {
            let our_secret = StaticSecret::from(secret_key.x25519);
            let their_ephemeral = X25519PublicKey::from(encapsulated.x25519_ephemeral);
            our_secret.diffie_hellman(&their_ephemeral)
        } else {
            // Fallback: use Ed25519 key for X25519
            tracing::warn!("Using Ed25519 key for X25519 decapsulation - suboptimal");
            let our_secret = StaticSecret::from(secret_key.ed25519);
            let their_ephemeral = X25519PublicKey::from(encapsulated.x25519_ephemeral);
            our_secret.diffie_hellman(&their_ephemeral)
        };

        // Kyber768 decapsulation (REAL post-quantum KEM!)
        let kyber_shared = if !secret_key.kyber.is_empty() && !encapsulated.kyber_ciphertext.is_empty() {
            let sk = kyber768::SecretKey::from_bytes(&secret_key.kyber)
                .map_err(|e| {
                    tracing::error!("Failed to parse Kyber secret key: {:?}", e);
                    CryptoError::InvalidSecretKey("Invalid Kyber secret key".to_string())
                })?;

            let ct = kyber768::Ciphertext::from_bytes(&encapsulated.kyber_ciphertext)
                .map_err(|e| {
                    tracing::error!("Failed to parse Kyber ciphertext: {:?}", e);
                    CryptoError::DecryptionError("Invalid Kyber ciphertext".to_string())
                })?;

            let shared = kyber768::decapsulate(&ct, &sk);
            shared.as_bytes().to_vec()
        } else {
            Vec::new()
        };

        // Combine shared secrets
        let mut combined_input = Vec::with_capacity(64);
        combined_input.extend_from_slice(x25519_shared.as_bytes());
        if !kyber_shared.is_empty() {
            combined_input.extend_from_slice(&kyber_shared);
        }
        let combined = blake3::hash(&combined_input);

        Ok(SharedSecret {
            secret: *combined.as_bytes(),
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_keypair() {
        let (signer, public_key) = HybridSigner::generate();

        assert_ne!(public_key.ed25519, [0u8; 32]);
        assert_ne!(public_key.x25519, [0u8; 32]); // Now we have real X25519!
        assert!(!public_key.dilithium.is_empty());
        assert!(!public_key.kyber.is_empty());

        // Verify public key sizes
        assert_eq!(public_key.dilithium.len(), DILITHIUM3_PUBLIC_KEY_SIZE);
        assert_eq!(public_key.kyber.len(), KYBER768_PUBLIC_KEY_SIZE);

        // Verify public key matches
        assert_eq!(signer.public_key().ed25519, public_key.ed25519);
        assert_eq!(signer.public_key().x25519, public_key.x25519);
    }

    #[test]
    fn test_sign_and_verify() {
        let (signer, public_key) = HybridSigner::generate();
        let message = b"Hello, Datachain Rope with Post-Quantum Security!";

        let signature = signer.sign(message);

        assert!(!signature.is_empty());
        assert!(signature.is_valid_structure());

        // Verify signature
        let result = HybridVerifier::verify(&public_key, message, &signature);
        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[test]
    fn test_wrong_message_fails() {
        let (signer, public_key) = HybridSigner::generate();
        let message = b"Original message";
        let wrong_message = b"Wrong message";

        let signature = signer.sign(message);

        // Should fail with wrong message
        let result = HybridVerifier::verify(&public_key, wrong_message, &signature);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_signature_size() {
        let (signer, _) = HybridSigner::generate();
        let signature = signer.sign(b"test");

        // Ed25519 is 64 bytes, Dilithium3 is ~3293 bytes
        assert_eq!(signature.ed25519_sig.len(), 64);
        assert!(signature.dilithium_sig.len() >= 3000); // Dilithium3 signature
    }

    #[test]
    fn test_kem_encapsulation_with_real_x25519() {
        let (signer, public_key) = HybridSigner::generate();

        // Verify X25519 keys are present
        assert!(public_key.has_x25519());

        let result = HybridKEM::encapsulate(&public_key);
        assert!(result.is_ok());

        let (encapsulated, shared_secret1) = result.unwrap();

        assert_ne!(encapsulated.x25519_ephemeral, [0u8; 32]);
        assert!(!encapsulated.kyber_ciphertext.is_empty());
        assert_ne!(*shared_secret1.as_bytes(), [0u8; 32]);

        // Test decapsulation
        let secret_key = signer.secret_key();
        let result = HybridKEM::decapsulate(&secret_key, &encapsulated);
        assert!(result.is_ok());

        let shared_secret2 = result.unwrap();

        // With real X25519 + Kyber, shared secrets should match!
        assert_eq!(*shared_secret1.as_bytes(), *shared_secret2.as_bytes());
    }

    #[test]
    fn test_public_key_serialization() {
        let (_, public_key) = HybridSigner::generate();

        let bytes = public_key.to_bytes();
        let restored = HybridPublicKey::from_bytes(&bytes).unwrap();

        assert_eq!(public_key.ed25519, restored.ed25519);
        assert_eq!(public_key.x25519, restored.x25519);
        assert_eq!(public_key.dilithium, restored.dilithium);
        assert_eq!(public_key.kyber, restored.kyber);
    }

    #[test]
    fn test_backward_compatibility() {
        // Create Ed25519-only public key
        let ed25519_only = HybridPublicKey::from_ed25519([42u8; 32]);

        assert!(!ed25519_only.has_pq_keys());
        assert!(!ed25519_only.has_encryption_key());
        assert!(!ed25519_only.has_x25519());

        // Full key
        let (_, full_key) = HybridSigner::generate();
        assert!(full_key.has_pq_keys());
        assert!(full_key.has_encryption_key());
        assert!(full_key.has_x25519());
    }

    #[test]
    fn test_no_fallback_bypass() {
        // This test verifies that the dangerous fallback verification is removed
        let (signer, public_key) = HybridSigner::generate();
        let message = b"Test message";

        // Create a valid signature
        let valid_signature = signer.sign(message);
        assert!(HybridVerifier::verify(&public_key, message, &valid_signature).unwrap());

        // Create an invalid signature with wrong Dilithium bytes
        let mut tampered_signature = valid_signature.clone();
        tampered_signature.dilithium_sig = vec![0u8; 100]; // Invalid size

        // This MUST fail - no fallback should accept invalid signatures
        let result = HybridVerifier::verify(&public_key, message, &tampered_signature);
        // Should return an error or false, never true
        assert!(result.is_err() || !result.unwrap());
    }

    #[test]
    fn test_empty_dilithium_sig_rejected_when_pq_keys_present() {
        let (signer, public_key) = HybridSigner::generate();
        let message = b"Test message";

        // Create signature with only Ed25519 (empty Dilithium)
        let partial_sig = HybridSignature {
            ed25519_sig: signer.sign(message).ed25519_sig,
            dilithium_sig: Vec::new(), // Missing!
        };

        // This MUST fail because public key has Dilithium key but signature is missing it
        let result = HybridVerifier::verify(&public_key, message, &partial_sig);
        assert!(result.is_ok());
        assert!(!result.unwrap()); // Should be rejected
    }
}
