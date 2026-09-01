// Reference Rust client for Phase-2 signed destructive RPC.
//
// Run as a one-off binary with:
//
//   cargo run --release \
//     --manifest-path datachain-rope/examples/phase2-signed-rpc/Cargo.toml \
//     -- 0x<priv-key-hex> [rpc-url] [chain-id]
//
// or copy the helper functions into your own crate. They have no dependency
// on rope-node; they re-implement the canonical-message construction so the
// example is self-contained for partner integrations.
//
// Chain scoping (Phase 0, 2026-08-30):
//   The domain-separation tag is derived from `chain_id`. Mainnet (271828)
//   keeps the fixed legacy tag for backward compatibility with every Phase-2
//   client already in production. Every other chain gets a tag of the shape
//   `DCROPE/destructive-rpc/v1/{chain_id}\0`, so a signature minted for
//   testnet (271829) cannot be replayed against mainnet and vice versa.
//   Byte-for-byte parity with rope-node is enforced by
//   `crates/rope-node/src/rpc_signature.rs::chain_domain_tag`.
//
// See `docs/PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md` for the spec.

use std::time::{SystemTime, UNIX_EPOCH};

use k256::ecdsa::{RecoveryId, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use rand::RngCore;
use serde_json::{json, Value};
use sha3::{Digest, Keccak256};

/// Mainnet's frozen v1 domain tag. Kept as a fixed byte string so every
/// signature minted before Phase 0 (2026-08-30) still verifies. Do NOT
/// change these bytes: every DCSwap `quipuEmitter.ts` signature, the
/// nonce store, and every cached client-side pre-image assumes this
/// value on mainnet.
const MAINNET_DOMAIN_TAG: &[u8] = b"DCROPE/destructive-rpc/v1\0";
const MAINNET_CHAIN_ID: u64 = 271828;
const NONCE_LEN: usize = 16;

/// Build the domain-separation tag for `chain_id`.
///
/// * Mainnet (271828) returns [`MAINNET_DOMAIN_TAG`] verbatim.
/// * Every other chain returns `DCROPE/destructive-rpc/v1/{chain_id}\0`
///   as UTF-8 bytes with a trailing NUL, matching the tag emitted by
///   `crates/rope-node/src/rpc_signature.rs::chain_domain_tag`.
fn chain_domain_tag(chain_id: u64) -> Vec<u8> {
    if chain_id == MAINNET_CHAIN_ID {
        MAINNET_DOMAIN_TAG.to_vec()
    } else {
        let mut tag = format!("DCROPE/destructive-rpc/v1/{chain_id}").into_bytes();
        tag.push(0);
        tag
    }
}

fn canonical_message(
    chain_id: u64,
    method: &str,
    params_without_auth: &Value,
    signed_at: u64,
    nonce: &[u8; NONCE_LEN],
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(256);
    let tag = chain_domain_tag(chain_id);
    buf.extend_from_slice(&tag);
    let m = method.as_bytes();
    buf.extend_from_slice(&(m.len() as u32).to_be_bytes());
    buf.extend_from_slice(m);
    let p = serde_json::to_vec(params_without_auth).expect("serialize params");
    buf.extend_from_slice(&(p.len() as u32).to_be_bytes());
    buf.extend_from_slice(&p);
    buf.extend_from_slice(&signed_at.to_be_bytes());
    buf.extend_from_slice(nonce);
    buf
}

fn eip191_digest(canonical: &[u8]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(b"\x19Ethereum Signed Message:\n");
    h.update(canonical.len().to_string().as_bytes());
    h.update(canonical);
    let out = h.finalize();
    let mut d = [0u8; 32];
    d.copy_from_slice(&out);
    d
}

fn sign_eip191(sk: &SigningKey, canonical: &[u8]) -> [u8; 65] {
    let digest = eip191_digest(canonical);
    let (sig, recid): (k256::ecdsa::Signature, RecoveryId) =
        sk.sign_prehash_recoverable(&digest).expect("sign");
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = u8::from(recid) + 27;
    out
}

fn eth_address_for(sk: &SigningKey) -> String {
    let pk = VerifyingKey::from(sk);
    let pt = pk.to_encoded_point(false);
    let raw = &pt.as_bytes()[1..];
    let mut h = Keccak256::new();
    h.update(raw);
    let d = h.finalize();
    format!("0x{}", hex::encode(&d[12..]))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pk_hex = std::env::args().nth(1).ok_or(
        "usage: sign_phase2_rpc <0x-prefixed-secp256k1-priv-key-hex> [rpc-url] [chain-id]",
    )?;
    let rpc_url = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "https://erpc.datachain.network".to_string());
    // Third positional arg overrides the chain id. Defaults to mainnet so
    // existing invocations keep working with zero changes. Set to 271829
    // (or any other chain) to sign under a chain-scoped tag.
    let chain_id: u64 = std::env::args()
        .nth(3)
        .as_deref()
        .map(|s| s.parse().expect("chain-id must be a u64"))
        .unwrap_or(MAINNET_CHAIN_ID);

    let key_bytes = hex::decode(pk_hex.trim_start_matches("0x"))?;
    let sk = SigningKey::from_slice(&key_bytes)?;
    let addr = eth_address_for(&sk);
    println!("signer address: {addr}");
    println!("chain id      : {chain_id}");
    println!(
        "domain tag    : {}",
        String::from_utf8_lossy(chain_domain_tag(chain_id).trim_ascii_end())
    );

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);

    let method = "rope_appendToLedger";
    let params_without_auth = json!([
        addr,
        {
            "interaction_type": "TestimonyAttestation",
            "description": "phase-2 reference client",
            "metadata": { "client": "examples/phase2-signed-rpc" }
        }
    ]);

    let canonical = canonical_message(chain_id, method, &params_without_auth, now, &nonce);
    let sig65 = sign_eip191(&sk, &canonical);

    let auth = json!({
        "auth": {
            "scheme": "secp256k1-eip191",
            "signed_at": now,
            "nonce": format!("0x{}", hex::encode(nonce)),
            "signature": format!("0x{}", hex::encode(sig65))
        }
    });

    let mut params = params_without_auth.as_array().unwrap().clone();
    params.push(auth);

    let payload = json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1
    });

    println!("submitting to {rpc_url} ...");
    let resp = reqwest::Client::new()
        .post(&rpc_url)
        .json(&payload)
        .send()
        .await?
        .text()
        .await?;
    println!("response: {resp}");
    Ok(())
}
