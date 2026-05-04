//! Testimony signer.
//!
//! Builds a [`SignedTestimony`] from canonical bytes using `rope-crypto`'s
//! [`HybridSigner`](rope_crypto::HybridSigner). The signer supports two modes:
//!
//! * **Hybrid** — Ed25519 (32-byte pubkey, 64-byte sig) + CRYSTALS-Dilithium3
//!   (~1952-byte pubkey, ~3293-byte sig). This is the production mode and the
//!   default. The Dilithium part is what gives Datachain Rope its NIST PQ-3
//!   security level (per Quipu Primitive Canon §4 — every knot's
//!   `event_signature` SHOULD be ML-DSA-65 / Dilithium3 by default).
//! * **Ed25519-only** — the testimony's Dilithium fields are left empty. Only
//!   suitable for local development where the signing cost matters more than
//!   the security level. The agent emits a WARN at startup in this mode.
//!
//! ## Key persistence
//!
//! [`TestimonySigner::from_seed_file`] reads a 32-byte raw seed from disk and
//! deterministically derives the keypair via
//! [`KeyStore::from_seed`](rope_crypto::KeyStore::from_seed). This means the
//! same seed always produces the same agent identity — which is what an
//! operator running the OracleAgent in a systemd unit needs for restart
//! continuity. [`TestimonySigner::ephemeral`] generates a fresh in-memory
//! keypair for tests.

use std::path::{Path, PathBuf};

use rope_crypto::{HybridSignature, KeyStore, PublicKey};
use serde::{Deserialize, Serialize};

use crate::config::SigningMode;

/// Errors emitted by the signer.
#[derive(Debug, thiserror::Error)]
pub enum SignerError {
    #[error("failed to read key seed from {path:?}: {source}")]
    ReadSeed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "key seed file at {path:?} must be exactly 32 bytes (got {got}); a 32-byte seed is the \
         input to KeyStore::from_seed"
    )]
    SeedLength { path: PathBuf, got: usize },
    #[error("failed to write key seed to {path:?}: {source}")]
    WriteSeed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A canonical, signed testimony envelope. Used for tests and as the
/// reference shape; in [`crate::OracleAgent`] the same fields are inlined into
/// the [`crate::OraclePriceTestimony`] payload that gets anchored on-chain.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct SignedTestimony {
    /// The exact bytes that were signed (the canonical JSON of the
    /// testimony's pre-signature payload).
    pub canonical: Vec<u8>,
    /// BLAKE3 hash of `canonical`, hex-encoded.
    pub payload_hash: String,
    /// 32-byte Ed25519 public key, hex-encoded.
    pub ed25519_public_key_hex: String,
    /// 64-byte Ed25519 signature, hex-encoded.
    pub ed25519_signature_hex: String,
    /// Dilithium3 public key, hex-encoded. Empty in Ed25519-only mode.
    pub dilithium_public_key_hex: String,
    /// Dilithium3 signature (signed-message format), hex-encoded.
    /// Empty in Ed25519-only mode.
    pub dilithium_signature_hex: String,
    /// Signing mode used to produce the signature.
    pub signing_mode: String,
}

impl SignedTestimony {
    /// Recompute and verify the BLAKE3 hash of `canonical` matches
    /// `payload_hash`. Used in tests and by anyone receiving a testimony to
    /// validate the envelope did not get tampered with in transit.
    pub fn payload_hash_matches(&self) -> bool {
        let h = blake3::hash(&self.canonical);
        hex::encode(h.as_bytes()) == self.payload_hash
    }
}

/// Signs canonical testimony bytes with a Datachain-Rope-compatible keypair.
///
/// The signer holds a `KeyStore` so that the keypair lives as long as the
/// signer. Drop semantics: when the signer is dropped the underlying
/// `HybridSecretKey` zeroizes itself (per `rope-crypto::HybridSecretKey:
/// ZeroizeOnDrop`).
pub struct TestimonySigner {
    store: KeyStore,
    mode: SigningMode,
}

impl std::fmt::Debug for TestimonySigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never log the secret key. Only the mode + the public key fingerprint.
        f.debug_struct("TestimonySigner")
            .field("mode", &self.mode)
            .field("ed25519_pk", &self.ed25519_public_key_hex())
            .finish_non_exhaustive()
    }
}

impl TestimonySigner {
    /// Build a signer with a freshly-generated random keypair. Used in tests
    /// and as a fallback when no `--key-path` is supplied.
    pub fn ephemeral(mode: SigningMode) -> Self {
        Self {
            store: KeyStore::new(),
            mode,
        }
    }

    /// Build a signer from a 32-byte raw seed (deterministic).
    pub fn from_seed_bytes(seed: [u8; 32], mode: SigningMode) -> Self {
        Self {
            store: KeyStore::from_seed(seed),
            mode,
        }
    }

    /// Read a 32-byte seed from `path` and build a deterministic signer.
    pub fn from_seed_file(path: &Path, mode: SigningMode) -> Result<Self, SignerError> {
        let bytes = std::fs::read(path).map_err(|source| SignerError::ReadSeed {
            path: path.to_path_buf(),
            source,
        })?;
        if bytes.len() != 32 {
            return Err(SignerError::SeedLength {
                path: path.to_path_buf(),
                got: bytes.len(),
            });
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        Ok(Self::from_seed_bytes(seed, mode))
    }

    /// Write a freshly-generated 32-byte seed to `path` and return the
    /// matching signer. Useful for `oracle-agent --init-key path/to/seed.bin`.
    pub fn generate_seed_file(path: &Path, mode: SigningMode) -> Result<Self, SignerError> {
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        std::fs::write(path, seed).map_err(|source| SignerError::WriteSeed {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self::from_seed_bytes(seed, mode))
    }

    /// Active signing mode.
    pub fn mode(&self) -> SigningMode {
        self.mode
    }

    /// View the agent's public key (Ed25519 + Dilithium when present).
    pub fn public_key(&self) -> &PublicKey {
        self.store.primary().public_key()
    }

    /// Hex-encoded 32-byte Ed25519 public key. Useful as a stable agent
    /// identifier in logs, in dashboards, and in the testimony envelope.
    pub fn ed25519_public_key_hex(&self) -> String {
        hex::encode(self.public_key().ed25519)
    }

    /// 32-byte node id (BLAKE3 of the Ed25519 pubkey, per
    /// `HybridPublicKey::node_id`).
    pub fn node_id_hex(&self) -> String {
        hex::encode(self.public_key().node_id())
    }

    /// Sign the canonical bytes of a testimony. The returned envelope embeds
    /// the canonical bytes verbatim — verifiers re-hash them to check
    /// `payload_hash` and re-run `HybridVerifier::verify` against the
    /// signatures.
    pub fn sign(&self, canonical: &[u8]) -> SignedTestimony {
        let raw = self.store.primary().sign(canonical);
        let payload_hash = hex::encode(blake3::hash(canonical).as_bytes());
        let pk = self.public_key();

        let (dilithium_pk_hex, dilithium_sig_hex) = match self.mode {
            SigningMode::Hybrid => (hex::encode(&pk.dilithium), hex::encode(&raw.dilithium_sig)),
            SigningMode::Ed25519Only => (String::new(), String::new()),
        };

        SignedTestimony {
            canonical: canonical.to_vec(),
            payload_hash,
            ed25519_public_key_hex: hex::encode(pk.ed25519),
            ed25519_signature_hex: hex::encode(&raw.ed25519_sig),
            dilithium_public_key_hex: dilithium_pk_hex,
            dilithium_signature_hex: dilithium_sig_hex,
            signing_mode: self.mode.to_string(),
        }
    }

    /// Returns the raw [`HybridSignature`] for `canonical`. This is the
    /// format the consensus layer expects in
    /// `rope_core::personal_ledger::HybridSignature` fields. The testimony
    /// envelope above is the JSON-friendly hex-encoded variant.
    pub fn sign_raw(&self, canonical: &[u8]) -> HybridSignature {
        self.store.primary().sign(canonical)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rope_crypto::HybridVerifier;

    #[test]
    fn ephemeral_signer_signs_and_verifies_hybrid() {
        let signer = TestimonySigner::ephemeral(SigningMode::Hybrid);
        let canonical = br#"{"hello":"world"}"#;
        let signed = signer.sign(canonical);

        assert_eq!(signed.canonical, canonical);
        assert_eq!(signed.signing_mode, "hybrid");
        assert!(signed.payload_hash_matches());
        assert!(!signed.ed25519_signature_hex.is_empty());
        assert!(!signed.dilithium_signature_hex.is_empty());

        let raw = signer.sign_raw(canonical);
        assert!(HybridVerifier::verify(signer.public_key(), canonical, &raw).unwrap());
    }

    #[test]
    fn ephemeral_signer_in_ed25519_only_mode_blanks_dilithium_fields() {
        let signer = TestimonySigner::ephemeral(SigningMode::Ed25519Only);
        let signed = signer.sign(b"payload");

        assert_eq!(signed.signing_mode, "ed25519-only");
        assert!(signed.dilithium_public_key_hex.is_empty());
        assert!(signed.dilithium_signature_hex.is_empty());
        // Ed25519 fields must still be present and non-zero
        assert!(!signed.ed25519_signature_hex.is_empty());
        assert!(!signed.ed25519_public_key_hex.is_empty());
    }

    #[test]
    fn signed_testimony_detects_tampered_canonical_bytes() {
        let signer = TestimonySigner::ephemeral(SigningMode::Hybrid);
        let mut signed = signer.sign(b"original");
        signed.canonical[0] ^= 0xFF;
        assert!(
            !signed.payload_hash_matches(),
            "tampered canonical bytes must invalidate payload hash"
        );
    }

    #[test]
    fn deterministic_seed_gives_stable_public_key() {
        let seed = [7u8; 32];
        let a = TestimonySigner::from_seed_bytes(seed, SigningMode::Hybrid);
        let b = TestimonySigner::from_seed_bytes(seed, SigningMode::Hybrid);
        assert_eq!(a.ed25519_public_key_hex(), b.ed25519_public_key_hex());
        assert_eq!(a.node_id_hex(), b.node_id_hex());
    }

    #[test]
    fn distinct_seeds_give_distinct_public_keys() {
        let a = TestimonySigner::from_seed_bytes([1u8; 32], SigningMode::Hybrid);
        let b = TestimonySigner::from_seed_bytes([2u8; 32], SigningMode::Hybrid);
        assert_ne!(a.ed25519_public_key_hex(), b.ed25519_public_key_hex());
    }

    #[test]
    fn from_seed_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oracle.seed");
        let signer1 = TestimonySigner::generate_seed_file(&path, SigningMode::Hybrid).unwrap();
        let pk1 = signer1.ed25519_public_key_hex();

        let signer2 = TestimonySigner::from_seed_file(&path, SigningMode::Hybrid).unwrap();
        assert_eq!(pk1, signer2.ed25519_public_key_hex());
    }

    #[test]
    fn from_seed_file_rejects_bad_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oracle.bad-seed");
        std::fs::write(&path, b"too short").unwrap();
        let err = TestimonySigner::from_seed_file(&path, SigningMode::Hybrid)
            .expect_err("9-byte seed must be rejected");
        assert!(matches!(err, SignerError::SeedLength { got: 9, .. }));
    }
}
