//! EIP-191 `personal_sign` wallet authentication for Datachain ID.
//!
//! A Datawallet+ owner proves possession of their wallet private key by
//! signing a short, replay-bounded message:
//!
//! ```text
//! DATACHAIN-ID-AUTH\n{address_lowercase}\n{unix_timestamp}
//! ```
//!
//! The gateway recovers the signer from the 65-byte `r||s||v` signature
//! (same k256 construction as rope-node's Phase-2 destructive-RPC
//! verifier and rope-edc's stakeholder gateway), requires it to equal
//! the claimed address, and enforces a ±300 s freshness window. The
//! domain tag is distinct from the EDC console/stakeholder domains so a
//! signature captured on one surface can never be replayed on another.

use sha3::{Digest, Keccak256};

/// Freshness window for the signed timestamp (seconds).
pub const AUTH_WINDOW_SECS: i64 = 300;

/// Domain tag for Datachain ID wallet sign-in.
pub const DOMAIN: &str = "DATACHAIN-ID-AUTH";

/// The canonical message a wallet signs.
pub fn auth_message(address: &str, timestamp: i64) -> String {
    format!("{DOMAIN}\n{}\n{}", address.to_lowercase(), timestamp)
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
        Signature::try_from(&raw[..64]).map_err(|e| format!("signature parse: {e}"))?;

    let digest = eip191_digest(message);
    let pubkey = VerifyingKey::recover_from_prehash(&digest, &signature, recovery_id)
        .map_err(|e| format!("recover: {e}"))?;

    // Ethereum address = last 20 bytes of keccak256(uncompressed pubkey
    // without the 0x04 prefix byte).
    let encoded = pubkey.to_encoded_point(false);
    let pubkey_bytes = &encoded.as_bytes()[1..];
    let mut hasher = Keccak256::new();
    hasher.update(pubkey_bytes);
    let hash = hasher.finalize();
    Ok(format!("0x{}", hex::encode(&hash[12..])))
}

/// Full verification: freshness window + signature recovery + claimed
/// address equality. Returns the lowercase proven address.
pub fn verify_wallet_auth(
    address: &str,
    timestamp: i64,
    signature_hex: &str,
    now: i64,
) -> Result<String, String> {
    if (now - timestamp).abs() > AUTH_WINDOW_SECS {
        return Err(format!(
            "timestamp outside ±{AUTH_WINDOW_SECS}s freshness window"
        ));
    }
    let claimed = address.to_lowercase();
    if !claimed.starts_with("0x") || claimed.len() != 42 {
        return Err("address must be a 0x-prefixed 20-byte hex address".into());
    }
    let message = auth_message(&claimed, timestamp);
    let recovered = recover_signer(message.as_bytes(), signature_hex)?;
    if recovered != claimed {
        return Err("signature does not match the claimed address".into());
    }
    Ok(claimed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;

    /// Sign `message` (EIP-191 wrapped) with a raw secp256k1 key, the
    /// way MetaMask's `personal_sign` does.
    fn personal_sign(key: &SigningKey, message: &[u8]) -> String {
        let digest = eip191_digest(message);
        let (sig, rid) = key.sign_prehash_recoverable(&digest).expect("sign");
        let mut raw = sig.to_bytes().to_vec();
        raw.push(rid.to_byte() + 27);
        format!("0x{}", hex::encode(raw))
    }

    fn eth_address(key: &SigningKey) -> String {
        let encoded = key.verifying_key().to_encoded_point(false);
        let mut hasher = Keccak256::new();
        hasher.update(&encoded.as_bytes()[1..]);
        let hash = hasher.finalize();
        format!("0x{}", hex::encode(&hash[12..]))
    }

    #[test]
    fn roundtrip_recovery() {
        let key = SigningKey::from_bytes((&[0x42u8; 32]).into()).unwrap();
        let address = eth_address(&key);
        let now = 1_800_000_000;
        let message = auth_message(&address, now);
        let sig = personal_sign(&key, message.as_bytes());
        let proven = verify_wallet_auth(&address, now, &sig, now).expect("verify");
        assert_eq!(proven, address);
    }

    #[test]
    fn stale_timestamp_rejected() {
        let key = SigningKey::from_bytes((&[0x42u8; 32]).into()).unwrap();
        let address = eth_address(&key);
        let now = 1_800_000_000;
        let ts = now - AUTH_WINDOW_SECS - 1;
        let message = auth_message(&address, ts);
        let sig = personal_sign(&key, message.as_bytes());
        assert!(verify_wallet_auth(&address, ts, &sig, now).is_err());
    }

    #[test]
    fn wrong_address_rejected() {
        let key = SigningKey::from_bytes((&[0x42u8; 32]).into()).unwrap();
        let address = eth_address(&key);
        let now = 1_800_000_000;
        let message = auth_message(&address, now);
        let sig = personal_sign(&key, message.as_bytes());
        let other = "0x0000000000000000000000000000000000000001";
        assert!(verify_wallet_auth(other, now, &sig, now).is_err());
    }

    #[test]
    fn cross_domain_signature_rejected() {
        // A signature over the EDC console domain must not authenticate
        // against the Datachain ID domain.
        let key = SigningKey::from_bytes((&[0x42u8; 32]).into()).unwrap();
        let address = eth_address(&key);
        let now = 1_800_000_000;
        let edc_message = format!("EDC-CONSOLE-AUTH\n{}\n{}", address, now);
        let sig = personal_sign(&key, edc_message.as_bytes());
        assert!(verify_wallet_auth(&address, now, &sig, now).is_err());
    }
}
