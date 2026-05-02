//! # Ledger Encryption — OES-Derived Symmetric Encryption for Personal Strings
//!
//! Provides the encrypt-before-slice pipeline for the personal ledger model.
//! Each wallet's String content (σ) is encrypted with a ChaCha20-Poly1305
//! symmetric key derived from the OES state, scoped to the wallet address.
//!
//! ## Key Hierarchy
//!
//! ```text
//! OES State (genome + chaotic subsystems)
//!     │
//!     ├─► derive_key(32, "ledger:{wallet_hex}:{generation}")
//!     │       │
//!     │       └─► ChaCha20-Poly1305 Key (256-bit)
//!     │               │
//!     │               ├─► Encrypt σ (nucleotide sequence content)
//!     │               └─► Decrypt σ (on repatriation)
//!     │
//!     └─► derive_key(32, "ledger_nonce:{wallet_hex}:{generation}:{sequence}")
//!             │
//!             └─► Nonce (96-bit, derived deterministically per entry)
//! ```
//!
//! ## Security Properties
//!
//! - **Forward secrecy**: Each OES generation produces a different key.
//!   Destroying the OES state for a generation makes all content encrypted
//!   under that generation permanently unrecoverable.
//! - **Wallet binding**: Keys are scoped to a specific wallet address.
//!   Two wallets on the same node derive completely different keys.
//! - **Authenticated encryption**: ChaCha20-Poly1305 provides both
//!   confidentiality and integrity. Tampered ciphertext is detected.
//! - **Deterministic nonces**: Derived from wallet + generation + sequence
//!   counter. Never reused for the same key (guaranteed by unique sequence).

use blake3;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Wallet address — 20-byte Ethereum-compatible or 32-byte native
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WalletAddress {
    bytes: Vec<u8>,
}

impl WalletAddress {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
        }
    }

    pub fn from_hex(hex_str: &str) -> Result<Self, LedgerCryptoError> {
        let clean = hex_str.strip_prefix("0x").unwrap_or(hex_str);
        let bytes = hex::decode(clean).map_err(|_| LedgerCryptoError::InvalidWalletAddress)?;
        if bytes.len() != 20 && bytes.len() != 32 {
            return Err(LedgerCryptoError::InvalidWalletAddress);
        }
        Ok(Self { bytes })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn to_hex(&self) -> String {
        format!("0x{}", hex::encode(&self.bytes))
    }
}

impl std::fmt::Display for WalletAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{}", hex::encode(&self.bytes))
    }
}

/// Ledger encryption key — zeroized on drop
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct LedgerKey {
    key: [u8; 32],
}

impl LedgerKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.key
    }
}

impl std::fmt::Debug for LedgerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LedgerKey([REDACTED])")
    }
}

/// Encrypted ledger entry with all metadata needed for decryption
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedLedgerEntry {
    pub ciphertext: Vec<u8>,
    pub nonce: [u8; 12],
    pub tag: [u8; 16],
    pub oes_generation: u64,
    pub sequence_number: u64,
    pub wallet_address: WalletAddress,
    pub content_hash: [u8; 32],
}

/// Versioned envelope wrapping encrypted content within a RopeString σ field.
/// The version byte lets nodes distinguish plaintext (v0) from encrypted (v1+).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerEnvelope {
    pub version: u8,
    pub payload: LedgerEnvelopePayload,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LedgerEnvelopePayload {
    Plaintext(Vec<u8>),
    EncryptedV1(EncryptedLedgerEntry),
}

impl LedgerEnvelope {
    pub fn plaintext(data: Vec<u8>) -> Self {
        Self {
            version: 0,
            payload: LedgerEnvelopePayload::Plaintext(data),
        }
    }

    pub fn encrypted_v1(entry: EncryptedLedgerEntry) -> Self {
        Self {
            version: 1,
            payload: LedgerEnvelopePayload::EncryptedV1(entry),
        }
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(self.version);
        match &self.payload {
            LedgerEnvelopePayload::Plaintext(data) => {
                out.extend_from_slice(data);
            }
            LedgerEnvelopePayload::EncryptedV1(entry) => {
                let encoded = bincode::serialize(entry).unwrap_or_default();
                out.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
                out.extend_from_slice(&encoded);
            }
        }
        out
    }

    pub fn deserialize(data: &[u8]) -> Result<Self, LedgerCryptoError> {
        if data.is_empty() {
            return Err(LedgerCryptoError::InvalidEnvelope);
        }

        let version = data[0];
        match version {
            0 => Ok(Self::plaintext(data[1..].to_vec())),
            1 => {
                if data.len() < 5 {
                    return Err(LedgerCryptoError::InvalidEnvelope);
                }
                let len = u32::from_be_bytes(data[1..5].try_into().unwrap()) as usize;
                if data.len() < 5 + len {
                    return Err(LedgerCryptoError::InvalidEnvelope);
                }
                let entry: EncryptedLedgerEntry = bincode::deserialize(&data[5..5 + len])
                    .map_err(|_| LedgerCryptoError::DeserializationFailed)?;
                Ok(Self::encrypted_v1(entry))
            }
            _ => Err(LedgerCryptoError::UnsupportedVersion(version)),
        }
    }
}

/// Errors specific to ledger encryption
#[derive(Debug, Clone, thiserror::Error)]
pub enum LedgerCryptoError {
    #[error("invalid wallet address")]
    InvalidWalletAddress,
    #[error("encryption failed: {0}")]
    EncryptionFailed(String),
    #[error("decryption failed: authentication tag mismatch")]
    DecryptionFailed,
    #[error("invalid envelope format")]
    InvalidEnvelope,
    #[error("unsupported envelope version: {0}")]
    UnsupportedVersion(u8),
    #[error("deserialization failed")]
    DeserializationFailed,
    #[error("OES state unavailable for generation {0}")]
    OesStateUnavailable(u64),
    #[error("ledger not found for wallet {0}")]
    LedgerNotFound(String),
    #[error("ledger already exists for wallet {0}")]
    LedgerAlreadyExists(String),
    #[error("unauthorized: caller does not own this ledger")]
    Unauthorized,
}

/// Derive a ledger encryption key from OES state for a specific wallet and generation.
///
/// This is the core key derivation function. Given an OES `derive_key` function
/// (which incorporates genome, Lorenz, cellular, fractal, quantum, and anchor state),
/// we produce a wallet-scoped symmetric key.
pub fn derive_ledger_key(
    oes_derive: &dyn Fn(usize, &str) -> Vec<u8>,
    wallet: &WalletAddress,
    generation: u64,
) -> LedgerKey {
    let purpose = format!("ledger:{}:{}", wallet.to_hex(), generation);
    let raw = oes_derive(32, &purpose);
    let mut key = [0u8; 32];
    key.copy_from_slice(&raw[..32]);
    LedgerKey { key }
}

/// Derive a deterministic nonce for a specific entry in the ledger.
///
/// Nonces must never repeat for the same key. Since each (wallet, generation)
/// pair produces a unique key, and each entry within that generation has a
/// unique sequence_number, the nonce is guaranteed unique.
fn derive_nonce(wallet: &WalletAddress, generation: u64, sequence_number: u64) -> [u8; 12] {
    let mut input = Vec::new();
    input.extend_from_slice(b"ledger_nonce:");
    input.extend_from_slice(wallet.as_bytes());
    input.extend_from_slice(&generation.to_le_bytes());
    input.extend_from_slice(&sequence_number.to_le_bytes());

    let hash = blake3::hash(&input);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&hash.as_bytes()[..12]);
    nonce
}

/// Encrypt plaintext content for a ledger entry.
///
/// Uses ChaCha20-Poly1305 (RFC 8439) via the `ring` crate's AEAD.
pub fn encrypt_ledger_content(
    key: &LedgerKey,
    plaintext: &[u8],
    wallet: &WalletAddress,
    generation: u64,
    sequence_number: u64,
) -> Result<EncryptedLedgerEntry, LedgerCryptoError> {
    let nonce = derive_nonce(wallet, generation, sequence_number);
    let content_hash = *blake3::hash(plaintext).as_bytes();

    let aad = build_aad(wallet, generation, sequence_number);

    let (ciphertext, tag) = chacha20_poly1305_encrypt(key.as_bytes(), &nonce, plaintext, &aad)
        .map_err(|e| LedgerCryptoError::EncryptionFailed(e))?;

    Ok(EncryptedLedgerEntry {
        ciphertext,
        nonce,
        tag,
        oes_generation: generation,
        sequence_number,
        wallet_address: wallet.clone(),
        content_hash,
    })
}

/// Decrypt a ledger entry back to plaintext.
pub fn decrypt_ledger_content(
    key: &LedgerKey,
    entry: &EncryptedLedgerEntry,
) -> Result<Vec<u8>, LedgerCryptoError> {
    let aad = build_aad(
        &entry.wallet_address,
        entry.oes_generation,
        entry.sequence_number,
    );

    let plaintext = chacha20_poly1305_decrypt(
        key.as_bytes(),
        &entry.nonce,
        &entry.ciphertext,
        &entry.tag,
        &aad,
    )
    .map_err(|_| LedgerCryptoError::DecryptionFailed)?;

    let hash = *blake3::hash(&plaintext).as_bytes();
    if hash != entry.content_hash {
        return Err(LedgerCryptoError::DecryptionFailed);
    }

    Ok(plaintext)
}

/// Build Additional Authenticated Data binding the ciphertext to its context.
fn build_aad(wallet: &WalletAddress, generation: u64, sequence: u64) -> Vec<u8> {
    let mut aad = Vec::with_capacity(64);
    aad.extend_from_slice(b"DCR-LEDGER-V1:");
    aad.extend_from_slice(wallet.as_bytes());
    aad.extend_from_slice(&generation.to_le_bytes());
    aad.extend_from_slice(&sequence.to_le_bytes());
    aad
}

// ============================================================================
// ChaCha20-Poly1305 Implementation (using ring)
// ============================================================================

fn chacha20_poly1305_encrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<(Vec<u8>, [u8; 16]), String> {
    use ring::aead;

    let unbound_key =
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(|e| format!("{}", e))?;
    let sealing_key = aead::LessSafeKey::new(unbound_key);

    let ring_nonce = aead::Nonce::try_assume_unique_for_key(nonce).map_err(|e| format!("{}", e))?;
    let ring_aad = aead::Aad::from(aad);

    let mut in_out = plaintext.to_vec();
    sealing_key
        .seal_in_place_append_tag(ring_nonce, ring_aad, &mut in_out)
        .map_err(|e| format!("{}", e))?;

    let tag_start = in_out.len() - 16;
    let mut tag = [0u8; 16];
    tag.copy_from_slice(&in_out[tag_start..]);
    in_out.truncate(tag_start);

    Ok((in_out, tag))
}

fn chacha20_poly1305_decrypt(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
    tag: &[u8; 16],
    aad: &[u8],
) -> Result<Vec<u8>, String> {
    use ring::aead;

    let unbound_key =
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(|e| format!("{}", e))?;
    let opening_key = aead::LessSafeKey::new(unbound_key);

    let ring_nonce = aead::Nonce::try_assume_unique_for_key(nonce).map_err(|e| format!("{}", e))?;
    let ring_aad = aead::Aad::from(aad);

    let mut in_out = Vec::with_capacity(ciphertext.len() + 16);
    in_out.extend_from_slice(ciphertext);
    in_out.extend_from_slice(tag);

    let plaintext = opening_key
        .open_in_place(ring_nonce, ring_aad, &mut in_out)
        .map_err(|e| format!("{}", e))?;

    Ok(plaintext.to_vec())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_oes_derive(length: usize, purpose: &str) -> Vec<u8> {
        let hash = blake3::hash(purpose.as_bytes());
        let mut key = hash.as_bytes().to_vec();
        while key.len() < length {
            key.extend_from_slice(blake3::hash(&key).as_bytes());
        }
        key.truncate(length);
        key
    }

    fn test_wallet() -> WalletAddress {
        WalletAddress::from_hex("0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195").unwrap()
    }

    #[test]
    fn test_wallet_address_roundtrip() {
        let addr = test_wallet();
        let hex = addr.to_hex();
        let parsed = WalletAddress::from_hex(&hex).unwrap();
        assert_eq!(addr, parsed);
    }

    #[test]
    fn test_key_derivation_deterministic() {
        let wallet = test_wallet();
        let k1 = derive_ledger_key(&mock_oes_derive, &wallet, 0);
        let k2 = derive_ledger_key(&mock_oes_derive, &wallet, 0);
        assert_eq!(k1.key, k2.key);
    }

    #[test]
    fn test_key_derivation_different_generations() {
        let wallet = test_wallet();
        let k1 = derive_ledger_key(&mock_oes_derive, &wallet, 0);
        let k2 = derive_ledger_key(&mock_oes_derive, &wallet, 1);
        assert_ne!(k1.key, k2.key);
    }

    #[test]
    fn test_key_derivation_different_wallets() {
        let w1 = test_wallet();
        let w2 = WalletAddress::from_hex("0x297Ba84d8c69Fb845Dab7C45FF7494dD561fF44c").unwrap();
        let k1 = derive_ledger_key(&mock_oes_derive, &w1, 0);
        let k2 = derive_ledger_key(&mock_oes_derive, &w2, 0);
        assert_ne!(k1.key, k2.key);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let wallet = test_wallet();
        let key = derive_ledger_key(&mock_oes_derive, &wallet, 0);
        let plaintext = b"Hello, Datachain Rope personal ledger!";

        let entry = encrypt_ledger_content(&key, plaintext, &wallet, 0, 0).unwrap();
        assert_ne!(entry.ciphertext, plaintext.to_vec());

        let decrypted = decrypt_ledger_content(&key, &entry).unwrap();
        assert_eq!(decrypted, plaintext.to_vec());
    }

    #[test]
    fn test_wrong_key_fails_decryption() {
        let wallet = test_wallet();
        let key = derive_ledger_key(&mock_oes_derive, &wallet, 0);
        let wrong_key = derive_ledger_key(&mock_oes_derive, &wallet, 999);

        let entry = encrypt_ledger_content(&key, b"secret data", &wallet, 0, 0).unwrap();
        let result = decrypt_ledger_content(&wrong_key, &entry);
        assert!(result.is_err());
    }

    #[test]
    fn test_tampered_ciphertext_fails() {
        let wallet = test_wallet();
        let key = derive_ledger_key(&mock_oes_derive, &wallet, 0);

        let mut entry = encrypt_ledger_content(&key, b"secure content", &wallet, 0, 0).unwrap();
        if !entry.ciphertext.is_empty() {
            entry.ciphertext[0] ^= 0xFF;
        }

        let result = decrypt_ledger_content(&key, &entry);
        assert!(result.is_err());
    }

    #[test]
    fn test_envelope_plaintext_roundtrip() {
        let env = LedgerEnvelope::plaintext(b"raw data".to_vec());
        let serialized = env.serialize();
        let deserialized = LedgerEnvelope::deserialize(&serialized).unwrap();
        assert_eq!(deserialized.version, 0);
        if let LedgerEnvelopePayload::Plaintext(data) = deserialized.payload {
            assert_eq!(data, b"raw data");
        } else {
            panic!("Expected Plaintext payload");
        }
    }

    #[test]
    fn test_envelope_encrypted_roundtrip() {
        let wallet = test_wallet();
        let key = derive_ledger_key(&mock_oes_derive, &wallet, 0);
        let entry = encrypt_ledger_content(&key, b"secret", &wallet, 0, 0).unwrap();

        let env = LedgerEnvelope::encrypted_v1(entry.clone());
        let serialized = env.serialize();
        let deserialized = LedgerEnvelope::deserialize(&serialized).unwrap();
        assert_eq!(deserialized.version, 1);

        if let LedgerEnvelopePayload::EncryptedV1(recovered) = deserialized.payload {
            let plaintext = decrypt_ledger_content(&key, &recovered).unwrap();
            assert_eq!(plaintext, b"secret");
        } else {
            panic!("Expected EncryptedV1 payload");
        }
    }

    #[test]
    fn test_nonce_uniqueness() {
        let wallet = test_wallet();
        let n1 = derive_nonce(&wallet, 0, 0);
        let n2 = derive_nonce(&wallet, 0, 1);
        let n3 = derive_nonce(&wallet, 1, 0);
        assert_ne!(n1, n2);
        assert_ne!(n1, n3);
        assert_ne!(n2, n3);
    }
}
