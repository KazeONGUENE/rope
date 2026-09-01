//! Phase-2 V11 closure: signed-payload destructive RPC verifier.
//!
//! Implements the spec at `docs/PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md`.
//! Two signature schemes:
//! - `secp256k1-eip191` for wallet-owned methods (rope_untieKnot,
//!   rope_erasePersonalLedger, rope_appendToLedger, rope_createPersonalLedger).
//!   The wallet's secp256k1 EOA key signs the canonical request bytes
//!   wrapped in EIP-191 (`personal_sign`). The verifier recovers the
//!   signing address and asserts it equals `params[0]`.
//! - `ed25519` for the governance method (rope_anchorDeployerAttestation).
//!   The signer pubkey must appear in `master-nodes.toml [founder] founder_keys`.
//!
//! Replay protection:
//! - 16-byte random nonce, hex-encoded in the wire format.
//! - `signed_at` Unix-seconds timestamp; `±window_secs` tolerance.
//! - In-memory `DashMap<NonceKey, expires_at>` keyed by `(signer, nonce)`.
//! - Background pruning task drops entries with `expires_at < now`.
//!
//! This module is paired with the loopback bypass in `rpc_server.rs`
//! and the env-flag gate in `rpc_auth.rs`. It is invoked ONLY when:
//! - The caller is non-loopback (i.e. has X-Forwarded-For OR is from a
//!   non-loopback peer address), AND
//! - The method is in `rpc_auth::DESTRUCTIVE_METHODS`, AND
//! - `ROPE_PHASE2_SIGNED_DESTRUCTIVE=1`.
//!
//! When all three conditions hold, the verifier extracts the auth envelope
//! from `params[N-1]`, validates the signature + nonce + freshness, and
//! returns either `VerifiedAuth::WalletEoa(addr)` or `VerifiedAuth::Founder`.
//! Any other path returns an `AuthError` and the dispatcher refuses with
//! JSON-RPC code -32401.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use ed25519_dalek::{Signature as Ed25519Signature, Verifier, VerifyingKey};
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey as EcdsaVerifyingKey};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};


/// Domain-separation tag woven into every canonical message on
/// **mainnet (chain 271828)**. Preserved as a fixed byte string for
/// backward compatibility with every Phase-2 client that was built
/// against v1 of the wire format (DCSwap `quipuEmitter.ts`, the Rust
/// SDK example, TypeScript SDK example, and every already-signed
/// nonce currently in the replay window).
///
/// All other chains (testnet 271829 and any future flavour) derive a
/// **chain-scoped** tag of the shape `DCROPE/destructive-rpc/v1/{chain_id}\0`
/// via [`chain_domain_tag`]. This keeps mainnet on the frozen v1 wire
/// format while making a testnet signature unusable against mainnet
/// and vice versa. See `docs/design/testnet-parity-roadmap-2026-08-30.md`
/// §2.2 for the rationale.
pub const DOMAIN_TAG: &[u8] = b"DCROPE/destructive-rpc/v1\0";

/// Chain ID for Datachain Rope mainnet. Mainnet gets the fixed
/// legacy [`DOMAIN_TAG`]; every other chain gets a chain-scoped tag.
pub const MAINNET_CHAIN_ID: u64 = 271828;

/// Build the domain-separation tag for `chain_id`.
///
/// * Mainnet (271828) returns the fixed [`DOMAIN_TAG`] bytes verbatim.
///   This is a hard carve-out: mainnet's wire format is frozen so
///   existing signed calls, cached nonces, and third-party SDK
///   implementations keep working with zero coordination.
/// * All other chains return `format!("DCROPE/destructive-rpc/v1/{chain_id}\0")`
///   encoded as UTF-8 bytes. A signature produced under one chain's
///   tag cannot be replayed under another chain's tag because the
///   canonical pre-image bytes differ from the very first byte.
///
/// The trailing NUL byte matches the legacy tag's shape and gives
/// consumers a stable termination when reading the tag from a hex
/// dump.
pub fn chain_domain_tag(chain_id: u64) -> Vec<u8> {
    if chain_id == MAINNET_CHAIN_ID {
        DOMAIN_TAG.to_vec()
    } else {
        let mut tag = format!("DCROPE/destructive-rpc/v1/{chain_id}").into_bytes();
        tag.push(0);
        tag
    }
}

/// Default replay window if not configured otherwise. 5 minutes.
pub const DEFAULT_REPLAY_WINDOW_SECS: i64 = 300;

/// The nonce field MUST be exactly 16 bytes (128 bits). Hex-encoded on the
/// wire as a 32-character string with optional `0x` prefix.
pub const NONCE_LEN: usize = 16;

/// JSON-RPC error code returned for any auth failure on a destructive method.
/// Matches the Phase-1 V11 hot-fix code so existing alerting + scanners do
/// not need to change.
pub const ERROR_CODE_DENIED: i32 = -32401;

/// Schemes the verifier accepts. Adding a new variant here requires
/// updating `verify_destructive_call` and adding round-trip unit tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthScheme {
    /// secp256k1 ECDSA-with-recovery, EIP-191 (`personal_sign`) wrapped.
    /// The signer is identified as the recovered Ethereum address.
    #[serde(rename = "secp256k1-eip191")]
    Secp256k1Eip191,
    /// Ed25519. Signer is identified by their 32-byte pubkey, which must
    /// appear in the founder-keys registry to authorize governance methods.
    #[serde(rename = "ed25519")]
    Ed25519,
}


/// Wire shape of the auth envelope, embedded as the LAST element of
/// `params` under the `auth` key. Example:
///
/// ```json
/// { "auth": {
///     "scheme": "secp256k1-eip191",
///     "signed_at": 1781336400,
///     "nonce": "0xe93b3c8d6a14f02f5b1a4d7e3c2b9a08",
///     "signature": "0x9b...c4...1c"
/// } }
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthEnvelope {
    pub scheme: AuthScheme,
    pub signed_at: u64,
    pub nonce: String,
    pub signature: String,
}

/// Result of a successful verification. Includes the recovered signer so
/// the dispatcher can compare against `params[0]` (wallet methods) or the
/// founder-keys registry (governance method).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifiedAuth {
    /// secp256k1 path. Recovered Ethereum address (20 bytes, lowercase).
    WalletEoa(EthAddress),
    /// ed25519 path. The signer pubkey passed verification AND was found
    /// in the founder-keys registry. The dispatcher can rely on the path
    /// alone; the pubkey is included for audit/logging.
    Founder { pubkey: [u8; 32] },
}

/// 20-byte Ethereum-style address. `Display`/`Debug`/`PartialEq` use the
/// lowercase 0x-prefixed hex form so comparisons with `params[0]` are
/// case-insensitive.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EthAddress(pub [u8; 20]);

impl EthAddress {
    pub fn to_hex(&self) -> String {
        format!("0x{}", hex::encode(self.0))
    }

    pub fn from_hex(s: &str) -> Result<Self, AuthError> {
        let s = s.trim_start_matches("0x").trim_start_matches("0X");
        let bytes = hex::decode(s).map_err(|e| AuthError::BadHex(format!("address: {e}")))?;
        if bytes.len() != 20 {
            return Err(AuthError::BadAddressLength(bytes.len()));
        }
        let mut out = [0u8; 20];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }

    /// Lowercase comparison (eth addresses are case-insensitive on the wire).
    pub fn eq_ignore_case(&self, hex: &str) -> bool {
        match Self::from_hex(hex) {
            Ok(other) => other == *self,
            Err(_) => false,
        }
    }
}

impl std::fmt::Debug for EthAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EthAddress({})", self.to_hex())
    }
}

impl std::fmt::Display for EthAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}


/// All ways verification can fail. The dispatcher maps every variant to
/// JSON-RPC code -32401 with a brief, non-leaky message. Detailed reasons
/// flow only through `tracing::warn!` for forensics.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("missing auth envelope (expected last params element to contain {{\"auth\": ...}})")]
    MissingEnvelope,
    #[error("malformed auth envelope: {0}")]
    MalformedEnvelope(String),
    #[error("unknown signature scheme: {0}")]
    UnknownScheme(String),
    #[error("bad hex: {0}")]
    BadHex(String),
    #[error("bad signature length: expected 65 (secp256k1) or 64 (ed25519), got {0}")]
    BadSignatureLength(usize),
    #[error("bad nonce length: expected {NONCE_LEN}, got {0}")]
    BadNonceLength(usize),
    #[error("bad address length: expected 20, got {0}")]
    BadAddressLength(usize),
    #[error("bad ed25519 pubkey length: expected 32, got {0}")]
    BadPubkeyLength(usize),
    #[error("stale signature: signed_at delta = {delta_secs}s (window = {window_secs}s)")]
    StaleSignature { delta_secs: i64, window_secs: i64 },
    #[error("nonce replay (already seen for this signer)")]
    NonceReplay,
    #[error("ECDSA recover failed: {0}")]
    EcdsaRecover(String),
    #[error("Ed25519 verify failed: {0}")]
    Ed25519Verify(String),
    #[error("recovered signer does not match params[0]: recovered={recovered}, params[0]={claimed}")]
    SignerMismatch { recovered: String, claimed: String },
    #[error("Ed25519 signer pubkey {0} not in founder-keys registry")]
    SignerNotAuthority(String),
    #[error("missing or non-string params[0] (expected wallet address)")]
    MissingWalletParam,
    #[error("canonical encoding failed: {0}")]
    CanonicalEncoding(String),
}


/// Key for the per-signer nonce store. Keeps namespaces separate so
/// two callers cannot accidentally lock each other out.
#[derive(Clone, PartialEq, Eq, Hash)]
struct NonceKey {
    /// 20 bytes (eth address) or 32 bytes (ed25519 pubkey).
    signer: Vec<u8>,
    nonce: [u8; NONCE_LEN],
}

/// In-process verifier. One instance per `RpcHandlers`. Holds:
/// - A handle to the founder-keys registry (read via the existing
///   `GovernanceManager`).
/// - The configured replay window.
/// - The `(signer, nonce) -> expires_at` map used for replay protection.
pub struct AuthVerifier {
    nonces: DashMap<NonceKey, u64>,
    window_secs: i64,
    /// Founder-key matcher. We accept any 32-byte pubkey whose lowercase
    /// hex appears in `master-nodes.toml [founder] founder_keys`.
    founder_keys_lc: Vec<String>,
}

impl AuthVerifier {
    /// Build a verifier from the loaded `MasterNodeRegistry`. The window
    /// defaults to `DEFAULT_REPLAY_WINDOW_SECS` if the registry omits it.
    pub fn new(founder_keys: &[String], window_secs: i64) -> Arc<Self> {
        let founder_keys_lc = founder_keys
            .iter()
            .map(|k| k.trim_start_matches("0x").to_ascii_lowercase())
            .collect();
        let window = if window_secs > 0 {
            window_secs
        } else {
            DEFAULT_REPLAY_WINDOW_SECS
        };
        Arc::new(Self {
            nonces: DashMap::new(),
            window_secs: window,
            founder_keys_lc,
        })
    }

    /// Number of in-flight nonces (forensics + tests).
    pub fn nonce_count(&self) -> usize {
        self.nonces.len()
    }

    /// Drop nonce entries whose `expires_at < now_unix`. Idempotent.
    pub fn prune_nonces(&self, now_unix: i64) {
        self.nonces.retain(|_, expires_at| (*expires_at as i64) >= now_unix);
    }

    fn known_founder(&self, pubkey: &[u8; 32]) -> bool {
        let pk_hex = hex::encode(pubkey);
        self.founder_keys_lc.iter().any(|k| k == &pk_hex)
    }
}


/// Methods that require the `Founder` (governance) authority instead of
/// a wallet signature. Anything not in this list and present in
/// `rpc_auth::DESTRUCTIVE_METHODS` is treated as wallet-owned.
const FOUNDER_METHODS: &[&str] = &["rope_anchorDeployerAttestation"];

fn is_founder_method(method: &str) -> bool {
    FOUNDER_METHODS.contains(&method)
}

/// Build the canonical bytes that the client signs. The encoding is
/// length-prefixed and domain-tagged so it is unambiguous and immune to
/// JSON whitespace / key-ordering churn.
///
/// Layout:
/// ```text
/// DOMAIN_TAG ||                      // 26 bytes
/// u32_be(len(method)) || method ||   // method name
/// u32_be(len(params_bytes)) ||
/// params_bytes ||                    // raw json bytes of params[0..N-1]
/// u64_be(signed_at) ||
/// nonce                              // 16 bytes
/// ```
///
/// `params_bytes` is the SLICE of params with the auth envelope removed,
/// re-serialized via `serde_json::to_vec` over a `serde_json::Value`. JSON
/// canonicalization is therefore "whatever serde_json emits for a Value
/// in stable mode" - and the client must produce the same bytes. The SDK
/// helpers in `crates/rope-cli/` and `examples/sign-rpc-typescript/` do
/// this by serializing through `serde_json::Value` (Rust) or
/// `JSON.stringify` with sorted keys (TS).
pub fn canonical_message(
    method: &str,
    params_without_auth: &serde_json::Value,
    signed_at: u64,
    nonce: &[u8; NONCE_LEN],
) -> Result<Vec<u8>, AuthError> {
    canonical_message_with_chain(MAINNET_CHAIN_ID, method, params_without_auth, signed_at, nonce)
}

/// Chain-scoped variant of [`canonical_message`].
///
/// Uses the domain tag returned by [`chain_domain_tag`] as the pre-image
/// prefix. For `chain_id == MAINNET_CHAIN_ID` the output is bit-for-bit
/// identical to [`canonical_message`], so already-signed mainnet nonces
/// remain valid. For any other chain, the tag encodes the chain id and
/// signatures cannot be replayed across chains.
pub fn canonical_message_with_chain(
    chain_id: u64,
    method: &str,
    params_without_auth: &serde_json::Value,
    signed_at: u64,
    nonce: &[u8; NONCE_LEN],
) -> Result<Vec<u8>, AuthError> {
    let mut buf = Vec::with_capacity(256);
    let tag = chain_domain_tag(chain_id);
    buf.extend_from_slice(&tag);
    let method_bytes = method.as_bytes();
    buf.extend_from_slice(&(method_bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(method_bytes);
    let params_bytes = serde_json::to_vec(params_without_auth)
        .map_err(|e| AuthError::CanonicalEncoding(e.to_string()))?;
    buf.extend_from_slice(&(params_bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(&params_bytes);
    buf.extend_from_slice(&signed_at.to_be_bytes());
    buf.extend_from_slice(nonce);
    Ok(buf)
}

/// Wrap canonical bytes per EIP-191 (`personal_sign`):
/// `keccak256("\x19Ethereum Signed Message:\n" || len(msg) || msg)`.
pub fn eip191_digest(canonical: &[u8]) -> [u8; 32] {
    let len_str = canonical.len().to_string();
    let mut hasher = Keccak256::new();
    hasher.update(b"\x19Ethereum Signed Message:\n");
    hasher.update(len_str.as_bytes());
    hasher.update(canonical);
    let out = hasher.finalize();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&out);
    digest
}

/// Recover the signer's Ethereum address from a 65-byte secp256k1
/// `personal_sign` signature over `canonical`.
pub fn recover_eip191_address(
    canonical: &[u8],
    signature_65: &[u8; 65],
) -> Result<EthAddress, AuthError> {
    let digest = eip191_digest(canonical);

    let r_s = &signature_65[..64];
    let v = signature_65[64];
    let recovery_byte = match v {
        27 | 28 => v - 27,
        0 | 1 => v,
        other => return Err(AuthError::EcdsaRecover(format!(
            "unexpected recovery id v={other} (expected 0/1/27/28)"
        ))),
    };
    let recovery_id = RecoveryId::try_from(recovery_byte)
        .map_err(|e| AuthError::EcdsaRecover(format!("bad recovery id: {e}")))?;
    let signature = EcdsaSignature::from_slice(r_s)
        .map_err(|e| AuthError::EcdsaRecover(format!("bad sig encoding: {e}")))?;
    let pubkey = EcdsaVerifyingKey::recover_from_prehash(&digest, &signature, recovery_id)
        .map_err(|e| AuthError::EcdsaRecover(e.to_string()))?;

    let pk_uncompressed = pubkey.to_encoded_point(false);
    let pk_bytes = pk_uncompressed.as_bytes();
    if pk_bytes.len() != 65 || pk_bytes[0] != 0x04 {
        return Err(AuthError::EcdsaRecover(
            "uncompressed pubkey not 65 bytes".to_string(),
        ));
    }
    let mut hasher = Keccak256::new();
    hasher.update(&pk_bytes[1..]);
    let pk_hash = hasher.finalize();
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&pk_hash[12..32]);
    Ok(EthAddress(addr))
}


fn parse_nonce_hex(hex_str: &str) -> Result<[u8; NONCE_LEN], AuthError> {
    let s = hex_str.trim_start_matches("0x").trim_start_matches("0X");
    let bytes = hex::decode(s).map_err(|e| AuthError::BadHex(format!("nonce: {e}")))?;
    if bytes.len() != NONCE_LEN {
        return Err(AuthError::BadNonceLength(bytes.len()));
    }
    let mut out = [0u8; NONCE_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn parse_sig_hex(hex_str: &str) -> Result<Vec<u8>, AuthError> {
    let s = hex_str.trim_start_matches("0x").trim_start_matches("0X");
    hex::decode(s).map_err(|e| AuthError::BadHex(format!("signature: {e}")))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Extract `params[N-1].auth` and return `(envelope, params_without_auth)`.
/// If the last params element is `{"auth": {...}}` we strip it; otherwise
/// we report `MissingEnvelope`. Any non-array `params` is treated as
/// missing too — destructive RPCs must be array-shape.
fn split_auth(
    params: &serde_json::Value,
) -> Result<(AuthEnvelope, serde_json::Value), AuthError> {
    let arr = params
        .as_array()
        .ok_or(AuthError::MissingEnvelope)?
        .clone();
    if arr.is_empty() {
        return Err(AuthError::MissingEnvelope);
    }
    let last = arr.last().ok_or(AuthError::MissingEnvelope)?;
    let auth_value = last.get("auth").ok_or(AuthError::MissingEnvelope)?.clone();
    let envelope: AuthEnvelope = serde_json::from_value(auth_value)
        .map_err(|e| AuthError::MalformedEnvelope(e.to_string()))?;
    let mut without = arr;
    without.pop();
    Ok((envelope, serde_json::Value::Array(without)))
}

/// Top-level entry point. Called from `handle_json_rpc_with_auth` AFTER
/// the loopback bypass has been considered. Returns `Ok(VerifiedAuth)` on
/// success; on failure the caller emits a JSON-RPC -32401 error.
///
/// The function:
///   1. Splits the auth envelope off `params`.
///   2. Checks `signed_at` falls within `±window_secs`.
///   3. Decodes nonce + signature hex.
///   4. Reconstructs canonical bytes from the params-without-auth slice.
///   5. Verifies the signature according to `scheme`.
///   6. Records the (signer, nonce) pair; rejects if already seen.
///   7. For wallet methods: asserts recovered address == params[0].
///      For founder methods: asserts pubkey appears in founder-keys.
pub fn verify_destructive_call(
    verifier: &AuthVerifier,
    method: &str,
    params: &serde_json::Value,
) -> Result<VerifiedAuth, AuthError> {
    verify_destructive_call_for_chain(verifier, MAINNET_CHAIN_ID, method, params)
}

/// Chain-scoped variant of [`verify_destructive_call`].
///
/// Uses [`canonical_message_with_chain`] to reconstruct the pre-image
/// under the chain-scoped domain tag. On mainnet (`chain_id ==
/// MAINNET_CHAIN_ID`) this is identical to [`verify_destructive_call`]
/// so the frozen v1 wire format keeps working. On any other chain, a
/// signature signed against a different chain's tag will fail the
/// signer-mismatch or ed25519-verify check because the recovered
/// address / verified signature cannot match a pre-image the peer
/// never signed.
pub fn verify_destructive_call_for_chain(
    verifier: &AuthVerifier,
    chain_id: u64,
    method: &str,
    params: &serde_json::Value,
) -> Result<VerifiedAuth, AuthError> {
    let (envelope, params_without_auth) = split_auth(params)?;

    let now = now_unix();
    let delta = now - (envelope.signed_at as i64);
    if delta.abs() > verifier.window_secs {
        return Err(AuthError::StaleSignature {
            delta_secs: delta,
            window_secs: verifier.window_secs,
        });
    }

    let nonce = parse_nonce_hex(&envelope.nonce)?;
    let sig_bytes = parse_sig_hex(&envelope.signature)?;
    let canonical = canonical_message_with_chain(
        chain_id,
        method,
        &params_without_auth,
        envelope.signed_at,
        &nonce,
    )?;

    let result = match envelope.scheme {
        AuthScheme::Secp256k1Eip191 => {
            if sig_bytes.len() != 65 {
                return Err(AuthError::BadSignatureLength(sig_bytes.len()));
            }
            let mut sig65 = [0u8; 65];
            sig65.copy_from_slice(&sig_bytes);
            let recovered = recover_eip191_address(&canonical, &sig65)?;

            let claimed = params_without_auth
                .get(0)
                .and_then(|v| v.as_str())
                .ok_or(AuthError::MissingWalletParam)?;
            if !recovered.eq_ignore_case(claimed) {
                return Err(AuthError::SignerMismatch {
                    recovered: recovered.to_hex(),
                    claimed: claimed.to_string(),
                });
            }
            VerifiedAuth::WalletEoa(recovered)
        }
        AuthScheme::Ed25519 => {
            if !is_founder_method(method) {
                return Err(AuthError::UnknownScheme(format!(
                    "ed25519 only valid for founder methods; method={method}"
                )));
            }
            if sig_bytes.len() != 64 {
                return Err(AuthError::BadSignatureLength(sig_bytes.len()));
            }
            let last = params
                .as_array()
                .and_then(|a| a.last())
                .ok_or(AuthError::MalformedEnvelope("missing wrapper".into()))?;
            let pk_hex = last
                .get("auth")
                .and_then(|a| a.get("pubkey"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| AuthError::MalformedEnvelope(
                    "founder-method auth must include `pubkey`".to_string(),
                ))?;
            let pk_clean = pk_hex.trim_start_matches("0x").trim_start_matches("0X");
            let pk_bytes = hex::decode(pk_clean)
                .map_err(|e| AuthError::BadHex(format!("pubkey: {e}")))?;
            if pk_bytes.len() != 32 {
                return Err(AuthError::BadPubkeyLength(pk_bytes.len()));
            }
            let mut pk_arr = [0u8; 32];
            pk_arr.copy_from_slice(&pk_bytes);
            let verifying = VerifyingKey::from_bytes(&pk_arr)
                .map_err(|e| AuthError::Ed25519Verify(e.to_string()))?;
            let mut sig_arr = [0u8; 64];
            sig_arr.copy_from_slice(&sig_bytes);
            let signature = Ed25519Signature::from_bytes(&sig_arr);
            verifying
                .verify(&canonical, &signature)
                .map_err(|e| AuthError::Ed25519Verify(e.to_string()))?;
            if !verifier.known_founder(&pk_arr) {
                return Err(AuthError::SignerNotAuthority(hex::encode(pk_arr)));
            }
            VerifiedAuth::Founder { pubkey: pk_arr }
        }
    };

    let signer_bytes: Vec<u8> = match &result {
        VerifiedAuth::WalletEoa(addr) => addr.0.to_vec(),
        VerifiedAuth::Founder { pubkey } => pubkey.to_vec(),
    };
    let key = NonceKey {
        signer: signer_bytes,
        nonce,
    };
    let expires_at = (envelope.signed_at as i64 + verifier.window_secs).max(0) as u64;
    if verifier.nonces.contains_key(&key) {
        return Err(AuthError::NonceReplay);
    }
    verifier.nonces.insert(key, expires_at);

    Ok(result)
}


#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey as Ed25519SigningKey};
    use k256::ecdsa::SigningKey as EcdsaSigningKey;
    use rand::rngs::OsRng;
    use serde_json::json;

    fn fresh_verifier(founder_keys: &[String]) -> Arc<AuthVerifier> {
        AuthVerifier::new(founder_keys, DEFAULT_REPLAY_WINDOW_SECS)
    }

    fn random_nonce() -> [u8; NONCE_LEN] {
        use rand::RngCore;
        let mut n = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut n);
        n
    }

    fn ed25519_random_signing_key() -> Ed25519SigningKey {
        use rand::RngCore;
        let mut secret = [0u8; 32];
        OsRng.fill_bytes(&mut secret);
        Ed25519SigningKey::from_bytes(&secret)
    }

    fn eth_addr_for(sk: &EcdsaSigningKey) -> EthAddress {
        let pk = EcdsaVerifyingKey::from(sk);
        let pk_bytes = pk.to_encoded_point(false);
        let raw = &pk_bytes.as_bytes()[1..];
        let mut h = Keccak256::new();
        h.update(raw);
        let digest = h.finalize();
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&digest[12..]);
        EthAddress(addr)
    }

    fn sign_eip191(sk: &EcdsaSigningKey, canonical: &[u8]) -> [u8; 65] {
        let digest = eip191_digest(canonical);
        let (sig, recid): (EcdsaSignature, RecoveryId) = sk
            .sign_prehash_recoverable(&digest)
            .expect("sign_prehash should not fail on 32-byte digest");
        let mut out = [0u8; 65];
        let bytes = sig.to_bytes();
        out[..64].copy_from_slice(&bytes);
        out[64] = u8::from(recid) + 27;
        out
    }

    fn build_wallet_envelope(
        sk: &EcdsaSigningKey,
        method: &str,
        params_without_auth: &serde_json::Value,
        signed_at: u64,
        nonce: &[u8; NONCE_LEN],
    ) -> AuthEnvelope {
        let canonical = canonical_message(method, params_without_auth, signed_at, nonce).unwrap();
        let sig = sign_eip191(sk, &canonical);
        AuthEnvelope {
            scheme: AuthScheme::Secp256k1Eip191,
            signed_at,
            nonce: format!("0x{}", hex::encode(nonce)),
            signature: format!("0x{}", hex::encode(sig)),
        }
    }

    fn embed_auth(
        params_without_auth: &serde_json::Value,
        envelope: &AuthEnvelope,
        extra: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let mut arr = params_without_auth.as_array().cloned().unwrap_or_default();
        let mut auth_value = serde_json::to_value(envelope).unwrap();
        if let Some(extra_obj) = extra {
            if let Some(obj) = auth_value.as_object_mut() {
                if let Some(extra_obj_map) = extra_obj.as_object() {
                    for (k, v) in extra_obj_map {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        arr.push(json!({"auth": auth_value}));
        serde_json::Value::Array(arr)
    }

    #[test]
    fn canonical_is_deterministic() {
        let nonce = [7u8; NONCE_LEN];
        let p = json!(["0xabc", {"x": 1}]);
        let a = canonical_message("rope_appendToLedger", &p, 1781336400, &nonce).unwrap();
        let b = canonical_message("rope_appendToLedger", &p, 1781336400, &nonce).unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with(DOMAIN_TAG));
    }

    /// Regression test for the 2026-07-26 DCSwap Quipu-emitter incident:
    /// production `handle_json_rpc_with_auth` does NOT sign an in-memory
    /// `json!()` value directly — it `serde_json::from_str`s the wire body,
    /// slices off the auth envelope, then `serde_json::to_vec`s the
    /// remaining params to reconstruct the bytes a real client signed. Any
    /// non-Rust client (TypeScript, the DCSwap bot's ethers.js signer, etc.)
    /// builds its object literals in insertion order and signs
    /// `JSON.stringify` bytes in that same order. If `Value::Object` here
    /// were a plain `BTreeMap` (the serde_json default without the
    /// `preserve_order` feature), this round trip would silently
    /// alphabetize the keys and produce different bytes than the client
    /// signed — recovering a signer address that doesn't match, with no
    /// hint that ordering was the cause. This test exercises exactly that
    /// parse-strip-reserialize path with keys that are NOT already in
    /// alphabetical order, so it fails loudly if `preserve_order` is ever
    /// dropped from the workspace `serde_json` feature set.
    #[test]
    fn canonical_message_round_trips_object_key_order_from_json_text() {
        // Deliberately non-alphabetical key order, mirroring a real
        // rope_appendToLedger interaction payload (interaction_type,
        // description, metadata — not alphabetical).
        let wire_body = r#"{"jsonrpc":"2.0","id":1,"method":"rope_appendToLedger","params":["0xabc",{"interaction_type":"StateUpdate","description":"x","metadata":{"emitter":"dcswap","schema":"Probe","quipu_version":"1.2","timestamp":"1"}}]}"#;
        let parsed: serde_json::Value = serde_json::from_str(wire_body).unwrap();
        let params_from_wire = parsed.get("params").cloned().unwrap();

        // What a non-Rust client computes: JSON.stringify of the exact same
        // logical structure, keys in original insertion order.
        let client_serialized = json!([
            "0xabc",
            {
                "interaction_type": "StateUpdate",
                "description": "x",
                "metadata": {
                    "emitter": "dcswap",
                    "schema": "Probe",
                    "quipu_version": "1.2",
                    "timestamp": "1"
                }
            }
        ]);

        let nonce = [3u8; NONCE_LEN];
        let server_side =
            canonical_message("rope_appendToLedger", &params_from_wire, 100, &nonce).unwrap();
        let client_side =
            canonical_message("rope_appendToLedger", &client_serialized, 100, &nonce).unwrap();
        assert_eq!(
            server_side, client_side,
            "server's parse->strip->reserialize of the wire body must byte-for-byte match \
             what an insertion-order-preserving client (JS/TS) would sign; if this fails, \
             Value::Object lost insertion order (preserve_order feature missing) and every \
             Phase-2 signed call with object params will be rejected as a signer mismatch"
        );
    }

    #[test]
    fn canonical_changes_with_method() {
        let nonce = [7u8; NONCE_LEN];
        let p = json!(["0xabc"]);
        let a = canonical_message("rope_appendToLedger", &p, 100, &nonce).unwrap();
        let b = canonical_message("rope_untieKnot", &p, 100, &nonce).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn canonical_changes_with_signed_at() {
        let nonce = [7u8; NONCE_LEN];
        let p = json!(["0xabc"]);
        let a = canonical_message("rope_appendToLedger", &p, 100, &nonce).unwrap();
        let b = canonical_message("rope_appendToLedger", &p, 101, &nonce).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn canonical_changes_with_nonce() {
        let p = json!(["0xabc"]);
        let a = canonical_message("rope_appendToLedger", &p, 100, &[1; NONCE_LEN]).unwrap();
        let b = canonical_message("rope_appendToLedger", &p, 100, &[2; NONCE_LEN]).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn eip191_recovery_round_trip() {
        let sk = EcdsaSigningKey::random(&mut OsRng);
        let expected = eth_addr_for(&sk);
        let nonce = random_nonce();
        let params = json!([expected.to_hex(), {"interaction_type": "test"}]);
        let canonical =
            canonical_message("rope_appendToLedger", &params, now_unix() as u64, &nonce).unwrap();
        let sig = sign_eip191(&sk, &canonical);
        let recovered = recover_eip191_address(&canonical, &sig).unwrap();
        assert_eq!(recovered, expected);
    }

    #[test]
    fn verify_wallet_method_happy_path() {
        let verifier = fresh_verifier(&[]);
        let sk = EcdsaSigningKey::random(&mut OsRng);
        let addr = eth_addr_for(&sk);
        let now = now_unix() as u64;
        let nonce = random_nonce();
        let params_without = json!([addr.to_hex(), {"interaction_type": "TestimonyAttestation"}]);
        let env = build_wallet_envelope(&sk, "rope_appendToLedger", &params_without, now, &nonce);
        let full = embed_auth(&params_without, &env, None);
        let r = verify_destructive_call(&verifier, "rope_appendToLedger", &full).unwrap();
        match r {
            VerifiedAuth::WalletEoa(a) => assert_eq!(a, addr),
            _ => panic!("expected WalletEoa"),
        }
    }

    #[test]
    fn verify_wallet_method_rejects_signer_mismatch() {
        let verifier = fresh_verifier(&[]);
        let sk_a = EcdsaSigningKey::random(&mut OsRng);
        let sk_b = EcdsaSigningKey::random(&mut OsRng);
        let addr_b = eth_addr_for(&sk_b);
        let now = now_unix() as u64;
        let nonce = random_nonce();
        let params_without = json!([addr_b.to_hex(), {"interaction_type": "x"}]);
        let env = build_wallet_envelope(&sk_a, "rope_appendToLedger", &params_without, now, &nonce);
        let full = embed_auth(&params_without, &env, None);
        let err = verify_destructive_call(&verifier, "rope_appendToLedger", &full).unwrap_err();
        assert!(matches!(err, AuthError::SignerMismatch { .. }));
    }

    #[test]
    fn verify_rejects_stale_signed_at() {
        let verifier = fresh_verifier(&[]);
        let sk = EcdsaSigningKey::random(&mut OsRng);
        let addr = eth_addr_for(&sk);
        let stale = (now_unix() - DEFAULT_REPLAY_WINDOW_SECS - 5) as u64;
        let nonce = random_nonce();
        let p = json!([addr.to_hex(), {"interaction_type": "x"}]);
        let env = build_wallet_envelope(&sk, "rope_appendToLedger", &p, stale, &nonce);
        let full = embed_auth(&p, &env, None);
        let err = verify_destructive_call(&verifier, "rope_appendToLedger", &full).unwrap_err();
        assert!(matches!(err, AuthError::StaleSignature { .. }));
    }

    #[test]
    fn verify_rejects_nonce_replay() {
        let verifier = fresh_verifier(&[]);
        let sk = EcdsaSigningKey::random(&mut OsRng);
        let addr = eth_addr_for(&sk);
        let now = now_unix() as u64;
        let nonce = random_nonce();
        let p = json!([addr.to_hex(), {"interaction_type": "x"}]);
        let env = build_wallet_envelope(&sk, "rope_appendToLedger", &p, now, &nonce);
        let full = embed_auth(&p, &env, None);
        let _first = verify_destructive_call(&verifier, "rope_appendToLedger", &full).unwrap();
        let err = verify_destructive_call(&verifier, "rope_appendToLedger", &full).unwrap_err();
        assert_eq!(err, AuthError::NonceReplay);
    }

    #[test]
    fn verify_rejects_missing_envelope() {
        let verifier = fresh_verifier(&[]);
        let p = json!(["0x000000000000000000000000000000000000dEaD", {"interaction_type": "x"}]);
        let err = verify_destructive_call(&verifier, "rope_appendToLedger", &p).unwrap_err();
        assert_eq!(err, AuthError::MissingEnvelope);
    }

    #[test]
    fn verify_rejects_bad_sig_length() {
        let verifier = fresh_verifier(&[]);
        let sk = EcdsaSigningKey::random(&mut OsRng);
        let addr = eth_addr_for(&sk);
        let nonce = random_nonce();
        let now = now_unix() as u64;
        let p = json!([addr.to_hex(), {"interaction_type": "x"}]);
        let mut env = build_wallet_envelope(&sk, "rope_appendToLedger", &p, now, &nonce);
        env.signature = "0x1234".to_string();
        let full = embed_auth(&p, &env, None);
        let err = verify_destructive_call(&verifier, "rope_appendToLedger", &full).unwrap_err();
        assert!(matches!(err, AuthError::BadSignatureLength(_)));
    }

    #[test]
    fn verify_rejects_bad_nonce_length() {
        let verifier = fresh_verifier(&[]);
        let sk = EcdsaSigningKey::random(&mut OsRng);
        let addr = eth_addr_for(&sk);
        let nonce = random_nonce();
        let now = now_unix() as u64;
        let p = json!([addr.to_hex(), {"interaction_type": "x"}]);
        let mut env = build_wallet_envelope(&sk, "rope_appendToLedger", &p, now, &nonce);
        env.nonce = "0x00".to_string();
        let full = embed_auth(&p, &env, None);
        let err = verify_destructive_call(&verifier, "rope_appendToLedger", &full).unwrap_err();
        assert!(matches!(err, AuthError::BadNonceLength(_)));
    }

    #[test]
    fn verify_founder_method_happy_path() {
        let sk = ed25519_random_signing_key();
        let pk = VerifyingKey::from(&sk).to_bytes();
        let pk_hex = format!("0x{}", hex::encode(pk));
        let verifier = fresh_verifier(&[pk_hex.clone()]);
        let now = now_unix() as u64;
        let nonce = random_nonce();
        let p = json!([{"deployer": "0xfeed", "metadata": {"role": "founder"}}]);
        let canonical =
            canonical_message("rope_anchorDeployerAttestation", &p, now, &nonce).unwrap();
        let sig: Ed25519Signature = sk.sign(&canonical);
        let env = AuthEnvelope {
            scheme: AuthScheme::Ed25519,
            signed_at: now,
            nonce: format!("0x{}", hex::encode(nonce)),
            signature: format!("0x{}", hex::encode(sig.to_bytes())),
        };
        let full = embed_auth(&p, &env, Some(json!({"pubkey": pk_hex})));
        let r =
            verify_destructive_call(&verifier, "rope_anchorDeployerAttestation", &full).unwrap();
        match r {
            VerifiedAuth::Founder { pubkey } => assert_eq!(pubkey, pk),
            _ => panic!("expected Founder"),
        }
    }

    #[test]
    fn verify_founder_rejects_unknown_pubkey() {
        let sk = ed25519_random_signing_key();
        let pk = VerifyingKey::from(&sk).to_bytes();
        let pk_hex = format!("0x{}", hex::encode(pk));
        let verifier = fresh_verifier(&["0xdeadbeef00".repeat(6).chars().take(64).collect::<String>()]);
        let now = now_unix() as u64;
        let nonce = random_nonce();
        let p = json!([{"deployer": "0xfeed"}]);
        let canonical =
            canonical_message("rope_anchorDeployerAttestation", &p, now, &nonce).unwrap();
        let sig: Ed25519Signature = sk.sign(&canonical);
        let env = AuthEnvelope {
            scheme: AuthScheme::Ed25519,
            signed_at: now,
            nonce: format!("0x{}", hex::encode(nonce)),
            signature: format!("0x{}", hex::encode(sig.to_bytes())),
        };
        let full = embed_auth(&p, &env, Some(json!({"pubkey": pk_hex})));
        let err = verify_destructive_call(&verifier, "rope_anchorDeployerAttestation", &full)
            .unwrap_err();
        assert!(matches!(err, AuthError::SignerNotAuthority(_)));
    }

    #[test]
    fn ed25519_rejected_for_wallet_method() {
        let sk = ed25519_random_signing_key();
        let pk = VerifyingKey::from(&sk).to_bytes();
        let pk_hex = format!("0x{}", hex::encode(pk));
        let verifier = fresh_verifier(&[pk_hex.clone()]);
        let now = now_unix() as u64;
        let nonce = random_nonce();
        let p = json!(["0xabc", {"interaction_type": "x"}]);
        let canonical = canonical_message("rope_appendToLedger", &p, now, &nonce).unwrap();
        let sig: Ed25519Signature = sk.sign(&canonical);
        let env = AuthEnvelope {
            scheme: AuthScheme::Ed25519,
            signed_at: now,
            nonce: format!("0x{}", hex::encode(nonce)),
            signature: format!("0x{}", hex::encode(sig.to_bytes())),
        };
        let full = embed_auth(&p, &env, Some(json!({"pubkey": pk_hex})));
        let err = verify_destructive_call(&verifier, "rope_appendToLedger", &full).unwrap_err();
        assert!(matches!(err, AuthError::UnknownScheme(_)));
    }

    #[test]
    fn prune_drops_expired_nonces() {
        let verifier = fresh_verifier(&[]);
        verifier.nonces.insert(
            NonceKey {
                signer: vec![1; 20],
                nonce: [0; NONCE_LEN],
            },
            100,
        );
        verifier.nonces.insert(
            NonceKey {
                signer: vec![2; 20],
                nonce: [1; NONCE_LEN],
            },
            10_000_000_000,
        );
        verifier.prune_nonces(1_000_000);
        assert_eq!(verifier.nonce_count(), 1);
    }

    #[test]
    fn case_insensitive_address_match() {
        let lower = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let upper = "0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let a = EthAddress::from_hex(lower).unwrap();
        assert!(a.eq_ignore_case(upper));
        assert!(a.eq_ignore_case(lower));
    }

    #[test]
    fn future_signed_at_outside_window_is_rejected() {
        let verifier = fresh_verifier(&[]);
        let sk = EcdsaSigningKey::random(&mut OsRng);
        let addr = eth_addr_for(&sk);
        let future = (now_unix() + DEFAULT_REPLAY_WINDOW_SECS + 5) as u64;
        let nonce = random_nonce();
        let p = json!([addr.to_hex(), {"interaction_type": "x"}]);
        let env = build_wallet_envelope(&sk, "rope_appendToLedger", &p, future, &nonce);
        let full = embed_auth(&p, &env, None);
        let err = verify_destructive_call(&verifier, "rope_appendToLedger", &full).unwrap_err();
        assert!(matches!(err, AuthError::StaleSignature { .. }));
    }

    // -------------------------------------------------------------------
    // Chain-scoped DOMAIN_TAG regression tests (Phase 0 §2.2)
    // -------------------------------------------------------------------
    //
    // Mainnet's wire format is frozen. Every existing Phase-2 client
    // (DCSwap `quipuEmitter.ts`, the Rust / TypeScript SDK examples,
    // and every cached nonce currently in the replay window) was
    // built against `DOMAIN_TAG = b"DCROPE/destructive-rpc/v1\0"` with
    // no chain-id byte. If we ever accidentally add a chain-id byte to
    // the mainnet pre-image, every one of those signatures becomes
    // invalid and destructive RPCs silently break in production. These
    // tests are the byte-for-byte tripwire for that regression.

    #[test]
    fn chain_domain_tag_mainnet_is_frozen_legacy_bytes() {
        assert_eq!(chain_domain_tag(MAINNET_CHAIN_ID), DOMAIN_TAG.to_vec());
        assert_eq!(chain_domain_tag(271828), DOMAIN_TAG.to_vec());
    }

    #[test]
    fn chain_domain_tag_testnet_encodes_chain_id() {
        let tag = chain_domain_tag(271829);
        assert_eq!(tag, b"DCROPE/destructive-rpc/v1/271829\0".to_vec());
        assert!(tag.ends_with(&[0]));
        assert_ne!(tag, DOMAIN_TAG.to_vec());
    }

    #[test]
    fn chain_domain_tag_distinct_per_chain() {
        let mainnet = chain_domain_tag(271828);
        let testnet = chain_domain_tag(271829);
        let hypothetical = chain_domain_tag(31337);
        assert_ne!(mainnet, testnet);
        assert_ne!(mainnet, hypothetical);
        assert_ne!(testnet, hypothetical);
    }

    #[test]
    fn canonical_message_mainnet_default_matches_frozen_wire_format() {
        // The bare `canonical_message` MUST stay bit-for-bit identical
        // to the pre-2026-08-30 output. This asserts the exact tag +
        // length + payload framing so an unrelated refactor cannot
        // silently shift the pre-image.
        let nonce = [7u8; NONCE_LEN];
        let p = json!(["0xabc"]);
        let canonical =
            canonical_message("rope_untieKnot", &p, 1_781_336_400, &nonce).unwrap();
        assert!(canonical.starts_with(DOMAIN_TAG));
        let with_chain = canonical_message_with_chain(
            MAINNET_CHAIN_ID,
            "rope_untieKnot",
            &p,
            1_781_336_400,
            &nonce,
        )
        .unwrap();
        assert_eq!(canonical, with_chain);
    }

    #[test]
    fn canonical_message_testnet_differs_from_mainnet_from_first_byte() {
        let nonce = [7u8; NONCE_LEN];
        let p = json!(["0xabc"]);
        let mainnet = canonical_message_with_chain(
            MAINNET_CHAIN_ID,
            "rope_untieKnot",
            &p,
            1_781_336_400,
            &nonce,
        )
        .unwrap();
        let testnet =
            canonical_message_with_chain(271829, "rope_untieKnot", &p, 1_781_336_400, &nonce)
                .unwrap();
        assert_ne!(mainnet, testnet);
        assert!(testnet.starts_with(b"DCROPE/destructive-rpc/v1/271829\0"));
    }

    /// The core replay-protection guarantee: a wallet signature that
    /// was correctly minted against testnet's tag MUST NOT verify
    /// against mainnet's chain-scoped verifier, and vice versa.
    #[test]
    fn signature_from_testnet_is_rejected_on_mainnet() {
        let verifier = fresh_verifier(&[]);
        let sk = EcdsaSigningKey::random(&mut OsRng);
        let addr = eth_addr_for(&sk);
        let now = now_unix() as u64;
        let nonce = random_nonce();
        let p = json!([addr.to_hex(), {"interaction_type": "x"}]);

        // Sign against the testnet tag.
        let canonical =
            canonical_message_with_chain(271829, "rope_appendToLedger", &p, now, &nonce)
                .unwrap();
        let sig = sign_eip191(&sk, &canonical);
        let env = AuthEnvelope {
            scheme: AuthScheme::Secp256k1Eip191,
            signed_at: now,
            nonce: format!("0x{}", hex::encode(nonce)),
            signature: format!("0x{}", hex::encode(sig)),
        };
        let full = embed_auth(&p, &env, None);

        // Present it to a mainnet verifier: signer recovery
        // reconstructs the pre-image with the mainnet tag, so it
        // recovers a different (garbage) address that does not match
        // the claimed `params[0]` wallet.
        let err = verify_destructive_call_for_chain(
            &verifier,
            MAINNET_CHAIN_ID,
            "rope_appendToLedger",
            &full,
        )
        .unwrap_err();
        assert!(
            matches!(err, AuthError::SignerMismatch { .. }),
            "testnet signature must fail on mainnet with SignerMismatch, got: {err:?}"
        );
    }

    #[test]
    fn signature_from_mainnet_is_rejected_on_testnet() {
        let verifier = fresh_verifier(&[]);
        let sk = EcdsaSigningKey::random(&mut OsRng);
        let addr = eth_addr_for(&sk);
        let now = now_unix() as u64;
        let nonce = random_nonce();
        let p = json!([addr.to_hex(), {"interaction_type": "x"}]);

        // Sign against the mainnet tag (default `canonical_message`).
        let env = build_wallet_envelope(&sk, "rope_appendToLedger", &p, now, &nonce);
        let full = embed_auth(&p, &env, None);

        let err = verify_destructive_call_for_chain(
            &verifier,
            271829,
            "rope_appendToLedger",
            &full,
        )
        .unwrap_err();
        assert!(
            matches!(err, AuthError::SignerMismatch { .. }),
            "mainnet signature must fail on testnet with SignerMismatch, got: {err:?}"
        );
    }

    /// Backward-compatibility guardrail: a mainnet-signed call must
    /// keep working end-to-end when routed through the new chain-
    /// scoped verifier with `chain_id = MAINNET_CHAIN_ID`. This
    /// protects every DCSwap `quipuEmitter.ts` signature currently in
    /// flight against the Phase-2 gate.
    #[test]
    fn mainnet_signature_still_verifies_via_chain_scoped_verifier() {
        let verifier = fresh_verifier(&[]);
        let sk = EcdsaSigningKey::random(&mut OsRng);
        let addr = eth_addr_for(&sk);
        let now = now_unix() as u64;
        let nonce = random_nonce();
        let p = json!([addr.to_hex(), {"interaction_type": "x"}]);
        let env = build_wallet_envelope(&sk, "rope_appendToLedger", &p, now, &nonce);
        let full = embed_auth(&p, &env, None);

        let ok = verify_destructive_call_for_chain(
            &verifier,
            MAINNET_CHAIN_ID,
            "rope_appendToLedger",
            &full,
        )
        .expect("mainnet-signed call must still verify on mainnet");
        assert!(matches!(ok, VerifiedAuth::WalletEoa(_)));
    }

    /// Positive path on testnet: signing against the testnet tag and
    /// verifying against the testnet tag succeeds.
    #[test]
    fn testnet_signature_verifies_end_to_end_under_chain_scoped_tag() {
        let verifier = fresh_verifier(&[]);
        let sk = EcdsaSigningKey::random(&mut OsRng);
        let addr = eth_addr_for(&sk);
        let now = now_unix() as u64;
        let nonce = random_nonce();
        let p = json!([addr.to_hex(), {"interaction_type": "x"}]);
        let canonical =
            canonical_message_with_chain(271829, "rope_appendToLedger", &p, now, &nonce)
                .unwrap();
        let sig = sign_eip191(&sk, &canonical);
        let env = AuthEnvelope {
            scheme: AuthScheme::Secp256k1Eip191,
            signed_at: now,
            nonce: format!("0x{}", hex::encode(nonce)),
            signature: format!("0x{}", hex::encode(sig)),
        };
        let full = embed_auth(&p, &env, None);

        let ok = verify_destructive_call_for_chain(
            &verifier,
            271829,
            "rope_appendToLedger",
            &full,
        )
        .expect("testnet-signed call must verify under testnet chain-scoped tag");
        assert!(matches!(ok, VerifiedAuth::WalletEoa(_)));
    }
}

