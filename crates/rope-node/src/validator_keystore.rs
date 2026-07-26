//! # Validator Keystore — Quipu Canon v2.0 Phase 2
//!
//! Persists the node's hybrid consensus signing key (Ed25519 +
//! CRYSTALS-Dilithium3, plus X25519 + Kyber768 for KEM) to disk so the
//! node keeps the same consensus identity across restarts.
//!
//! ## Why persistence matters
//!
//! A validator's consensus id is `blake3(ed25519_pubkey)`. If the key
//! were regenerated on every boot, the node's id would change, its
//! testimonies would be signed by an id no peer recognises, and the
//! committee would treat every restart as a brand-new (unknown)
//! validator. Persisting the keypair binds the identity for the life of
//! the data directory.
//!
//! ## On-disk format (`validator_key.bin`)
//!
//! ```text
//! magic         "RVK1"                     (4 bytes)
//! ed25519_sk    32 bytes
//! x25519_sk     32 bytes
//! u32 le        dilithium_sk length
//! dilithium_sk  <len> bytes
//! u32 le        kyber_sk length
//! kyber_sk      <len> bytes
//! u32 le        public_key length
//! public_key    HybridPublicKey::to_bytes()  (<len> bytes)
//! ```
//!
//! The full public key is stored alongside the secret material because
//! Dilithium public keys are NOT a plain suffix of the secret key, so we
//! cannot reliably re-derive the public key from the secret alone. The
//! signer signs using only the secret key; the stored public key is the
//! one registered in the [`ValidatorRegistry`], guaranteeing the sk/pk
//! pair matches the pair originally produced by `HybridSigner::generate`.
//!
//! ## Permissions
//!
//! The key file is written with `0o600` on Unix. Operators SHOULD keep
//! `validator_key.bin` out of any backup that leaves the host and should
//! treat it with the same care as any validator signing key.

use anyhow::{anyhow, Context, Result};
use rope_consensus::{ValidatorRegistry, ValidatorSetSnapshot};
use rope_core::types::NodeId;
use rope_crypto::hybrid::{HybridPublicKey, HybridSecretKey, HybridSigner};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAGIC: &[u8; 4] = b"RVK1";

/// The materialised validator identity for this node.
pub struct ValidatorIdentity {
    pub signer: Arc<HybridSigner>,
    pub public_key: HybridPublicKey,
    pub node_id: NodeId,
}

impl ValidatorIdentity {
    fn from_signer(signer: HybridSigner, public_key: HybridPublicKey) -> Self {
        let node_id = NodeId::new(public_key.node_id());
        Self {
            signer: Arc::new(signer),
            public_key,
            node_id,
        }
    }
}

/// Load the validator key from `data_dir/validator_key.bin`, or generate
/// and persist a fresh one if it does not exist.
pub fn load_or_create(data_dir: &Path) -> Result<ValidatorIdentity> {
    let path = key_path(data_dir);
    if path.exists() {
        load(&path).with_context(|| format!("loading validator key from {}", path.display()))
    } else {
        create(&path).with_context(|| format!("creating validator key at {}", path.display()))
    }
}

fn key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("validator_key.bin")
}

fn create(path: &Path) -> Result<ValidatorIdentity> {
    let (signer, public_key) = HybridSigner::generate();
    let secret = signer.secret_key();
    let bytes = encode(&secret, &public_key);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    write_private(path, &bytes)?;
    tracing::info!(
        "Generated new validator consensus key → {} (node_id {})",
        path.display(),
        short_hex(public_key.node_id())
    );
    Ok(ValidatorIdentity::from_signer(signer, public_key))
}

fn load(path: &Path) -> Result<ValidatorIdentity> {
    let bytes = std::fs::read(path)?;
    let (secret, public_key) = decode(&bytes)?;
    let signer = HybridSigner::from_secret_key(&secret)
        .map_err(|e| anyhow!("reconstructing signer from stored secret key: {e}"))?;
    tracing::info!(
        "Loaded validator consensus key from {} (node_id {})",
        path.display(),
        short_hex(public_key.node_id())
    );
    Ok(ValidatorIdentity::from_signer(signer, public_key))
}

fn encode(secret: &HybridSecretKey, public_key: &HybridPublicKey) -> Vec<u8> {
    let mut out = Vec::with_capacity(256 + secret.dilithium_bytes().len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(secret.ed25519_bytes());
    out.extend_from_slice(secret.x25519_bytes());
    put_lv(&mut out, secret.dilithium_bytes());
    put_lv(&mut out, secret.kyber_bytes());
    let pk = public_key.to_bytes();
    put_lv(&mut out, &pk);
    out
}

fn decode(bytes: &[u8]) -> Result<(HybridSecretKey, HybridPublicKey)> {
    let mut pos = 0usize;
    if bytes.len() < 4 + 32 + 32 || &bytes[0..4] != MAGIC {
        return Err(anyhow!("validator key file: bad magic or too short"));
    }
    pos += 4;

    let ed25519: [u8; 32] = bytes[pos..pos + 32]
        .try_into()
        .map_err(|_| anyhow!("validator key file: ed25519 field"))?;
    pos += 32;

    let x25519: [u8; 32] = bytes[pos..pos + 32]
        .try_into()
        .map_err(|_| anyhow!("validator key file: x25519 field"))?;
    pos += 32;

    let dilithium = get_lv(bytes, &mut pos).context("dilithium sk")?;
    let kyber = get_lv(bytes, &mut pos).context("kyber sk")?;
    let pk_bytes = get_lv(bytes, &mut pos).context("public key")?;

    let secret = HybridSecretKey::new(ed25519, x25519, dilithium, kyber);
    let public_key = HybridPublicKey::from_bytes(&pk_bytes)
        .map_err(|e| anyhow!("validator key file: public key decode: {e}"))?;

    Ok((secret, public_key))
}

fn put_lv(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
}

fn get_lv(bytes: &[u8], pos: &mut usize) -> Result<Vec<u8>> {
    if *pos + 4 > bytes.len() {
        return Err(anyhow!("truncated length prefix"));
    }
    let len = u32::from_le_bytes(bytes[*pos..*pos + 4].try_into().unwrap()) as usize;
    *pos += 4;
    if *pos + len > bytes.len() {
        return Err(anyhow!("truncated value ({} bytes expected)", len));
    }
    let v = bytes[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(v)
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    f.flush()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.flush()?;
    Ok(())
}

/// Build the node's [`ValidatorRegistry`], seeded with this node's own
/// public key and, if present, the committee snapshot in
/// `data_dir/validator_set.json`.
///
/// The `validator_set.json` file is the operator-distributed committee
/// roster: a JSON [`ValidatorSetSnapshot`]. Every node in the committee
/// ships the same file, so all nodes agree on who may testify. This
/// node's own key is always inserted (idempotently), so a single-node
/// deployment needs no roster file at all.
pub fn build_registry(
    data_dir: &Path,
    identity: &ValidatorIdentity,
) -> Result<Arc<ValidatorRegistry>> {
    let registry = Arc::new(ValidatorRegistry::new());

    // Load committee roster if present.
    let roster = data_dir.join("validator_set.json");
    if roster.exists() {
        let text = std::fs::read_to_string(&roster)
            .with_context(|| format!("reading {}", roster.display()))?;
        let snapshot: ValidatorSetSnapshot = serde_json::from_str(&text)
            .with_context(|| format!("parsing {} as ValidatorSetSnapshot", roster.display()))?;
        let mut n = 0usize;
        for rec in &snapshot.validators {
            // register_weighted enforces identity binding + PQ presence,
            // rejecting any tampered roster entry.
            match registry.register_weighted(rec.node_id, rec.public_key.clone(), rec.weight) {
                Ok(()) => n += 1,
                Err(e) => tracing::warn!("skipping invalid roster entry: {e}"),
            }
        }
        tracing::info!("Loaded {} validators from {}", n, roster.display());
    }

    // Always include self (idempotent — replaces any stale roster entry
    // for our own id with the live key).
    registry
        .register(identity.node_id, identity.public_key.clone())
        .map_err(|e| anyhow!("registering self in validator registry: {e}"))?;

    tracing::info!(
        "Validator registry ready: {} active validator(s)",
        registry.active_count()
    );
    Ok(registry)
}

fn short_hex(bytes: [u8; 32]) -> String {
    let mut s = String::with_capacity(16);
    for b in bytes.iter().take(8) {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_then_load_roundtrips_identity() {
        let dir = tempdir().unwrap();
        let id1 = load_or_create(dir.path()).unwrap();
        let id2 = load_or_create(dir.path()).unwrap();
        // Same node id across "restarts".
        assert_eq!(id1.node_id, id2.node_id);
        assert_eq!(id1.public_key.ed25519, id2.public_key.ed25519);
        assert_eq!(id1.public_key.dilithium, id2.public_key.dilithium);
    }

    #[test]
    fn reloaded_signer_produces_verifiable_signatures() {
        use rope_crypto::hybrid::HybridVerifier;
        let dir = tempdir().unwrap();
        let created = load_or_create(dir.path()).unwrap();
        let loaded = load_or_create(dir.path()).unwrap();

        let msg = b"quipu canon v2 phase 2 testimony";
        let sig = loaded.signer.sign(msg);
        // Verify against the ORIGINAL public key: proves the reloaded
        // secret key is the genuine pair of the persisted public key.
        let ok = HybridVerifier::verify(&created.public_key, msg, &sig).unwrap();
        assert!(ok, "reloaded signer must produce signatures that verify against the stored pubkey");
    }

    #[test]
    fn build_registry_includes_self() {
        let dir = tempdir().unwrap();
        let id = load_or_create(dir.path()).unwrap();
        let reg = build_registry(dir.path(), &id).unwrap();
        assert!(reg.is_active(&id.node_id));
        assert_eq!(reg.active_count(), 1);
    }
}
