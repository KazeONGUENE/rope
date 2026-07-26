//! Console sign-in sessions — EIP-191 wallet-signature login for the
//! operator console.
//!
//! Why this exists: the console API identifies its caller by wallet
//! address. On a self-hosted, single-operator instance a bare
//! `X-Edc-Wallet` header is acceptable (the instance is on the owner's
//! own network, optionally behind `EDC_CONSOLE_TOKEN`). On a publicly
//! hosted console (console.datachain.network) a bare header would allow
//! anyone to impersonate any wallet. This module closes that hole:
//!
//! 1. The browser signs `EDC-CONSOLE-AUTH\n{address}\n{timestamp}` with
//!    the operator's wallet (MetaMask `personal_sign`).
//! 2. `POST /api/v1/ecosystem/auth/session` verifies the signature and
//!    issues an opaque session token (`edc_sess_…`, 32 random bytes).
//! 3. Subsequent console requests carry `X-Edc-Session`; the server
//!    resolves it to the proven wallet. Only the blake3 digest of the
//!    token is stored.
//!
//! With `EDC_CONSOLE_REQUIRE_SIGNATURE=1` (the hosted-console setting)
//! the bare `X-Edc-Wallet` path is disabled entirely and every console
//! call must present a session token or a fresh per-request signature.

use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::RwLock;
use rand::RngCore;

/// Default session lifetime: 24 h. Override with `EDC_SESSION_TTL_SECS`.
const DEFAULT_TTL_SECS: i64 = 86_400;

struct SessionEntry {
    wallet: String,
    expires_at: i64,
}

fn store() -> &'static RwLock<HashMap<String, SessionEntry>> {
    static STORE: OnceLock<RwLock<HashMap<String, SessionEntry>>> = OnceLock::new();
    STORE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn ttl_secs() -> i64 {
    std::env::var("EDC_SESSION_TTL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TTL_SECS)
}

fn digest(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

/// Issue a new session for a signature-proven wallet. Returns the
/// plaintext token (shown once to the client) and its expiry.
pub fn create(wallet: &str, now: i64) -> (String, i64) {
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    let token = format!("edc_sess_{}", hex::encode(raw));
    let expires_at = now + ttl_secs();
    let mut map = store().write();
    // Opportunistic prune so the map never grows unbounded.
    map.retain(|_, e| e.expires_at > now);
    map.insert(
        digest(&token),
        SessionEntry {
            wallet: wallet.to_lowercase(),
            expires_at,
        },
    );
    (token, expires_at)
}

/// Resolve a presented session token to its wallet, if still valid.
pub fn resolve(token: &str, now: i64) -> Option<String> {
    let map = store().read();
    map.get(&digest(token))
        .filter(|e| e.expires_at > now)
        .map(|e| e.wallet.clone())
}

/// Revoke a session (sign-out). Returns true when the session existed.
pub fn revoke(token: &str) -> bool {
    store().write().remove(&digest(token)).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_resolve_roundtrip() {
        let (token, exp) = create("0xABCDEF0000000000000000000000000000000001", 1_000);
        assert!(exp > 1_000);
        assert_eq!(
            resolve(&token, 1_001).as_deref(),
            Some("0xabcdef0000000000000000000000000000000001")
        );
    }

    #[test]
    fn expired_session_rejected() {
        let (token, exp) = create("0x0000000000000000000000000000000000000002", 1_000);
        assert!(resolve(&token, exp + 1).is_none());
    }

    #[test]
    fn unknown_token_rejected() {
        assert!(resolve("edc_sess_deadbeef", 0).is_none());
    }

    #[test]
    fn revoke_kills_session() {
        let (token, _) = create("0x0000000000000000000000000000000000000003", 1_000);
        assert!(revoke(&token));
        assert!(resolve(&token, 1_001).is_none());
        assert!(!revoke(&token));
    }
}
