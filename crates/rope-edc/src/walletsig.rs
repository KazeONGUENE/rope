//! Wallet-signature stakeholder authentication - spec v1.0 §6.3:
//! "Wallet-signature authentication is available for consumers who are
//! already identified on-chain, so a regulator's own DID can authenticate
//! without a separate credential to manage."
//!
//! Scheme: EIP-191 `personal_sign` over a short, replay-bounded message.
//! The stakeholder sends three headers on any gateway request:
//!
//! ```text
//! X-Edc-Wallet-Address: 0x…                       (the claimed signer)
//! X-Edc-Timestamp:      1783500000                (unix seconds, ±300 s)
//! X-Edc-Signature:      0x…                       (65-byte r||s||v hex)
//! ```
//!
//! The signed message is exactly:
//!
//! ```text
//! EDC-STAKEHOLDER-AUTH\n{address_lowercase}\n{timestamp}
//! ```
//!
//! Verification recovers the signer from the signature (same k256
//! recovery construction as rope-node's Phase-2 destructive-RPC
//! verifier) and requires it to equal the claimed address. The gateway
//! then resolves the address to an active `AccessGrant` whose grantee is
//! `kind == "wallet"` with a matching value. The ±300 s freshness window
//! bounds replay; because the gateway is read-only, a replay within the
//! window grants nothing beyond what the legitimate holder already has.

use sha3::{Digest, Keccak256};

/// Freshness window for the signed timestamp (seconds).
pub const AUTH_WINDOW_SECS: i64 = 300;

/// Domain tag for stakeholder-gateway authentication.
pub const DOMAIN_STAKEHOLDER: &str = "EDC-STAKEHOLDER-AUTH";
/// Domain tag for console (operator) sign-in. Distinct from the
/// stakeholder domain so a signature captured on one surface can never
/// be replayed against the other.
pub const DOMAIN_CONSOLE: &str = "EDC-CONSOLE-AUTH";

/// The canonical message a wallet signs for a given auth domain.
pub fn domain_auth_message(domain: &str, address: &str, timestamp: i64) -> String {
    format!("{domain}\n{}\n{}", address.to_lowercase(), timestamp)
}

/// The canonical message a stakeholder wallet signs.
pub fn auth_message(address: &str, timestamp: i64) -> String {
    domain_auth_message(DOMAIN_STAKEHOLDER, address, timestamp)
}

/// EIP-191 `personal_sign` digest:
/// `keccak256("\x19Ethereum Signed Message:\n" || len(msg) || msg)`.
fn eip191_digest(message: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(b"\x19Ethereum Signed Message:\n");
    hasher.update(message.len().to_string().as_bytes());
    hasher.update(message);
    let out = hasher.finalize();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&out);
    digest
}

/// Recover the Ethereum address (`0x…`, lowercase) that produced the
/// 65-byte `r||s||v` signature over `message` (EIP-191 wrapped).
pub fn recover_signer(message: &[u8], signature_hex: &str) -> Result<String, String> {
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

    let raw = hex::decode(signature_hex.trim_start_matches("0x").trim_start_matches("0X"))
        .map_err(|e| format!("signature hex: {e}"))?;
    if raw.len() != 65 {
        return Err(format!("signature must be 65 bytes, got {}", raw.len()));
    }
    let v = raw[64];
    let recovery_byte = match v {
        27 | 28 => v - 27,
        0 | 1 => v,
        other => return Err(format!("unexpected recovery id v={other}")),
    };
    let recovery_id =
        RecoveryId::try_from(recovery_byte).map_err(|e| format!("recovery id: {e}"))?;
    let signature =
        Signature::from_slice(&raw[..64]).map_err(|e| format!("signature encoding: {e}"))?;

    let digest = eip191_digest(message);
    let pubkey = VerifyingKey::recover_from_prehash(&digest, &signature, recovery_id)
        .map_err(|e| format!("recover: {e}"))?;

    let uncompressed = pubkey.to_encoded_point(false);
    let bytes = uncompressed.as_bytes();
    if bytes.len() != 65 || bytes[0] != 0x04 {
        return Err("uncompressed pubkey not 65 bytes".to_string());
    }
    let mut hasher = Keccak256::new();
    hasher.update(&bytes[1..]);
    let hash = hasher.finalize();
    Ok(format!("0x{}", hex::encode(&hash[12..32])))
}

/// Full verification for an arbitrary auth domain: signature is valid,
/// signer equals the claimed address, and the timestamp is within the
/// freshness window of `now`.
pub fn verify_domain(
    domain: &str,
    claimed_address: &str,
    timestamp: i64,
    signature_hex: &str,
    now: i64,
) -> Result<String, String> {
    if (now - timestamp).abs() > AUTH_WINDOW_SECS {
        return Err(format!(
            "timestamp outside ±{AUTH_WINDOW_SECS}s window (skew {}s)",
            (now - timestamp).abs()
        ));
    }
    let message = domain_auth_message(domain, claimed_address, timestamp);
    let recovered = recover_signer(message.as_bytes(), signature_hex)?;
    if recovered != claimed_address.to_lowercase() {
        return Err(format!(
            "recovered signer {recovered} does not match claimed {}",
            claimed_address.to_lowercase()
        ));
    }
    Ok(recovered)
}

/// Full verification on the stakeholder domain (spec v1.0 §6.3).
pub fn verify(
    claimed_address: &str,
    timestamp: i64,
    signature_hex: &str,
    now: i64,
) -> Result<String, String> {
    verify_domain(DOMAIN_STAKEHOLDER, claimed_address, timestamp, signature_hex, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;

    /// Sign the auth message the way a wallet (ethers / viem
    /// `personal_sign`) would, returning the 65-byte hex signature.
    fn sign(key: &SigningKey, address: &str, ts: i64) -> String {
        let message = auth_message(address, ts);
        let digest = eip191_digest(message.as_bytes());
        let (sig, rid) = key
            .sign_prehash_recoverable(&digest)
            .expect("signing cannot fail");
        let mut raw = sig.to_bytes().to_vec();
        raw.push(rid.to_byte() + 27);
        format!("0x{}", hex::encode(raw))
    }

    fn address_of(key: &SigningKey) -> String {
        let pubkey = key.verifying_key().to_encoded_point(false);
        let mut hasher = Keccak256::new();
        hasher.update(&pubkey.as_bytes()[1..]);
        let hash = hasher.finalize();
        format!("0x{}", hex::encode(&hash[12..32]))
    }

    fn test_key() -> SigningKey {
        // Deterministic test key (NOT a production key).
        SigningKey::from_slice(&[0x42u8; 32]).unwrap()
    }

    #[test]
    fn valid_signature_verifies() {
        let key = test_key();
        let addr = address_of(&key);
        let ts = 1_800_000_000;
        let sig = sign(&key, &addr, ts);
        let recovered = verify(&addr, ts, &sig, ts + 10).expect("must verify");
        assert_eq!(recovered, addr);
    }

    #[test]
    fn checksummed_address_accepted() {
        let key = test_key();
        let addr = address_of(&key);
        let checksummed: String = addr
            .char_indices()
            .map(|(i, c)| if i > 2 && i % 3 == 0 { c.to_ascii_uppercase() } else { c })
            .collect();
        let ts = 1_800_000_000;
        let sig = sign(&key, &checksummed, ts);
        assert!(verify(&checksummed, ts, &sig, ts).is_ok());
    }

    #[test]
    fn stale_timestamp_rejected() {
        let key = test_key();
        let addr = address_of(&key);
        let ts = 1_800_000_000;
        let sig = sign(&key, &addr, ts);
        assert!(verify(&addr, ts, &sig, ts + AUTH_WINDOW_SECS + 1).is_err());
        assert!(verify(&addr, ts, &sig, ts - AUTH_WINDOW_SECS - 1).is_err());
    }

    #[test]
    fn wrong_claimed_address_rejected() {
        let key = test_key();
        let addr = address_of(&key);
        let ts = 1_800_000_000;
        let sig = sign(&key, &addr, ts);
        let err = verify(
            "0x000000000000000000000000000000000000dead",
            ts,
            &sig,
            ts,
        );
        assert!(err.is_err());
    }

    #[test]
    fn tampered_signature_rejected() {
        let key = test_key();
        let addr = address_of(&key);
        let ts = 1_800_000_000;
        let mut sig = sign(&key, &addr, ts);
        // Flip one nibble in r.
        let flipped = if &sig[10..11] == "a" { "b" } else { "a" };
        sig.replace_range(10..11, flipped);
        assert!(verify(&addr, ts, &sig, ts).is_err());
    }

    #[test]
    fn malformed_signature_rejected() {
        assert!(recover_signer(b"m", "0x1234").is_err());
        assert!(recover_signer(b"m", "not-hex").is_err());
    }

    #[test]
    fn console_and_stakeholder_domains_are_not_interchangeable() {
        let key = test_key();
        let addr = address_of(&key);
        let ts = 1_800_000_000;
        // Sign on the console domain…
        let message = domain_auth_message(DOMAIN_CONSOLE, &addr, ts);
        let digest = eip191_digest(message.as_bytes());
        let (sig, rid) = key.sign_prehash_recoverable(&digest).unwrap();
        let mut raw = sig.to_bytes().to_vec();
        raw.push(rid.to_byte() + 27);
        let sig_hex = format!("0x{}", hex::encode(raw));
        // …verifies on console, rejected on stakeholder.
        assert!(verify_domain(DOMAIN_CONSOLE, &addr, ts, &sig_hex, ts).is_ok());
        assert!(verify_domain(DOMAIN_STAKEHOLDER, &addr, ts, &sig_hex, ts).is_err());
        assert!(verify(&addr, ts, &sig_hex, ts).is_err());
    }
}
