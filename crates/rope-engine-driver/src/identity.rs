//! Per-node quorum identity — reuses the SAME `validator_key.bin` every
//! node already generates for the native Quipu Canon v2.0 Phase-2 String
//! Lattice consensus (`rope-node/src/validator_keystore.rs`), instead of
//! minting a second, parallel set of "fake" per-node keys. Confirmed by
//! direct inspection: BLUE, GREEN, DO-rpc-1 and DO-rpc-2 each already have
//! a *distinct* `~/.rope/validator_key.bin` (different sha256 per node),
//! i.e. these are real, independent, per-machine secrets — not one key
//! copied to four boxes.
//!
//! We only need the Ed25519 component for the lighter-weight EVM
//! block-quorum protocol (the full hybrid Ed25519+Dilithium3 scheme is
//! what the native knot layer uses; signing every 4.2s EVM round with a
//! ~2.4KB Dilithium3 signature is unnecessary weight for this sub-protocol
//! and can be layered in later without changing the wire format below,
//! since the file already carries both).
//!
//! On-disk format (from `validator_keystore.rs`, kept in sync deliberately):
//! ```text
//! magic         "RVK1"      (4 bytes)
//! ed25519_sk    32 bytes
//! x25519_sk     32 bytes
//! ... (dilithium/kyber/pubkey, length-prefixed, ignored here)
//! ```

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use std::path::Path;

const MAGIC: &[u8; 4] = b"RVK1";

pub struct NodeIdentity {
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

impl NodeIdentity {
    pub fn pubkey_hex(&self) -> String {
        hex::encode(self.verifying_key.to_bytes())
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing_key.sign(msg).to_bytes()
    }
}

/// Load the node's Ed25519 keypair from the same `validator_key.bin` the
/// native consensus layer uses. Does NOT create one — `rope-node` owns
/// key creation (`validator_keystore::load_or_create`); this driver is a
/// read-only consumer of an identity that must already exist, so a
/// missing file is a hard configuration error rather than something we
/// silently paper over by minting a throwaway key nobody else recognises.
pub fn load_from_validator_keystore(path: &Path) -> Result<NodeIdentity> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading validator key at {} (has rope-node run at least once on this box to create it?)", path.display()))?;

    if bytes.len() < 4 + 32 + 32 || &bytes[0..4] != MAGIC {
        bail!(
            "{}: bad magic or too short ({} bytes) — not a valid validator_key.bin",
            path.display(),
            bytes.len()
        );
    }

    let ed25519_sk: [u8; 32] = bytes[4..36]
        .try_into()
        .map_err(|_| anyhow!("{}: malformed ed25519 secret field", path.display()))?;

    let signing_key = SigningKey::from_bytes(&ed25519_sk);
    let verifying_key = signing_key.verifying_key();

    Ok(NodeIdentity {
        signing_key,
        verifying_key,
    })
}

pub fn verify(pubkey_hex: &str, msg: &[u8], sig_hex: &str) -> Result<bool> {
    let pk_bytes = hex::decode(pubkey_hex).context("pubkey hex")?;
    let pk_arr: [u8; 32] = pk_bytes
        .try_into()
        .map_err(|_| anyhow!("pubkey must be 32 bytes"))?;
    let vk = VerifyingKey::from_bytes(&pk_arr).context("invalid ed25519 pubkey")?;

    let sig_bytes = hex::decode(sig_hex).context("signature hex")?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| anyhow!("signature must be 64 bytes"))?;
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);

    Ok(vk.verify(msg, &sig).is_ok())
}

/// Canonical message signed/verified for one quorum round. Domain-separated
/// so a signature from this protocol can never be replayed as a signature
/// for a different Datachain Rope protocol that happens to reuse the same
/// validator key (the native testimony layer, Phase-2 signed RPCs, etc.).
pub fn attest_message(round: u64, block_number: u64, block_hash_hex: &str) -> Vec<u8> {
    let mut msg = b"DCROPE/evm-quorum-attest/v1".to_vec();
    msg.extend_from_slice(&round.to_le_bytes());
    msg.extend_from_slice(&block_number.to_le_bytes());
    msg.extend_from_slice(block_hash_hex.as_bytes());
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_fixture_key(dir: &Path, seed: u8) -> std::path::PathBuf {
        let sk_bytes = [seed; 32];
        let signing_key = SigningKey::from_bytes(&sk_bytes);
        let vk = signing_key.verifying_key();

        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&sk_bytes);
        out.extend_from_slice(&[0u8; 32]); // x25519 placeholder
        // dilithium (empty, length-prefixed)
        out.extend_from_slice(&0u32.to_le_bytes());
        // kyber (empty, length-prefixed)
        out.extend_from_slice(&0u32.to_le_bytes());
        // public key blob (not needed by our loader, but keep shape realistic)
        let pk = vk.to_bytes();
        out.extend_from_slice(&(pk.len() as u32).to_le_bytes());
        out.extend_from_slice(&pk);

        let path = dir.join("validator_key.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&out).unwrap();
        path
    }

    #[test]
    fn test_load_roundtrips_pubkey() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture_key(dir.path(), 7);
        let identity = load_from_validator_keystore(&path).unwrap();
        assert_eq!(identity.pubkey_hex().len(), 64);
    }

    #[test]
    fn test_sign_then_verify_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture_key(dir.path(), 9);
        let identity = load_from_validator_keystore(&path).unwrap();

        let msg = attest_message(1, 100, "0xabc123");
        let sig = identity.sign(&msg);
        let ok = verify(&identity.pubkey_hex(), &msg, &hex::encode(sig)).unwrap();
        assert!(ok);
    }

    #[test]
    fn test_verify_rejects_tampered_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture_key(dir.path(), 11);
        let identity = load_from_validator_keystore(&path).unwrap();

        let msg = attest_message(1, 100, "0xabc123");
        let sig = identity.sign(&msg);
        let tampered = attest_message(1, 100, "0xdeadbeef");
        let ok = verify(&identity.pubkey_hex(), &tampered, &hex::encode(sig)).unwrap();
        assert!(!ok);
    }

    #[test]
    fn test_verify_rejects_wrong_key() {
        let dir = tempfile::tempdir().unwrap();
        let path_a = write_fixture_key(dir.path(), 13);
        let identity_a = load_from_validator_keystore(&path_a).unwrap();

        let dir_b = tempfile::tempdir().unwrap();
        let path_b = write_fixture_key(dir_b.path(), 17);
        let identity_b = load_from_validator_keystore(&path_b).unwrap();

        let msg = attest_message(2, 50, "0x1");
        let sig = identity_a.sign(&msg);
        let ok = verify(&identity_b.pubkey_hex(), &msg, &hex::encode(sig)).unwrap();
        assert!(!ok);
    }

    #[test]
    fn test_load_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.bin");
        assert!(load_from_validator_keystore(&missing).is_err());
    }

    #[test]
    fn test_load_bad_magic_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("validator_key.bin");
        std::fs::write(&path, [0u8; 100]).unwrap();
        assert!(load_from_validator_keystore(&path).is_err());
    }
}
