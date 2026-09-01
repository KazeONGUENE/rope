//! Ed25519-signed ecosystem tokens (JWT, `alg: EdDSA`) + JWKS.
//!
//! Every Datachain Rope platform can verify a Datachain ID token
//! offline: fetch `/.well-known/jwks.json` once, cache the OKP key,
//! and verify the compact JWT signature + `iss`/`aud`/`exp` claims.
//! No shared secrets, no callback to the gateway required (though
//! `/v1/auth/introspect` exists for platforms that prefer it).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::ROPE_CHAIN_ID;

/// A wallet linked to the Datawallet+ account, as embedded in the token.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WalletClaim {
    pub address: String,
    #[serde(rename = "type")]
    pub wallet_type: String,
    pub is_default: bool,
    pub verified: bool,
}

/// The full claim set of a Datachain ID ecosystem token.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Claims {
    /// Token issuer (`https://id.datachain.network`).
    pub iss: String,
    /// Datawallet+ auth user UUID.
    pub sub: String,
    /// Fixed audience: `datachain-ecosystem`.
    pub aud: String,
    pub iat: i64,
    pub exp: i64,
    pub email: String,
    #[serde(default)]
    pub name: String,
    /// Canonical DID — `did:web:datawallet.plus:<sub>` unless the
    /// account has an explicit DID registered.
    pub did: String,
    /// The wallet address platforms should treat as the user's primary
    /// on-chain identity (chain 271828). `None` when the account has no
    /// wallet bound yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_address: Option<String>,
    /// Every wallet linked to the account.
    #[serde(default)]
    pub wallets: Vec<WalletClaim>,
    /// Optional registered public key from the Datawallet+ profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_key: Option<String>,
    /// Authentication methods used: `["pwd"]` (credentials) or
    /// `["wallet_signature"]` (EIP-191 proof of key possession).
    pub amr: Vec<String>,
    /// Datachain Rope chain id the identity is scoped to.
    pub chain_id: u64,
}

#[derive(thiserror::Error, Debug)]
pub enum TokenError {
    #[error("malformed token")]
    Malformed,
    #[error("unsupported algorithm or key id")]
    UnsupportedHeader,
    #[error("signature verification failed")]
    BadSignature,
    #[error("token expired")]
    Expired,
    #[error("issuer mismatch")]
    IssuerMismatch,
    #[error("audience mismatch")]
    AudienceMismatch,
}

/// The gateway's signing identity. Loaded from (or generated into) a
/// 0600 key file at boot.
pub struct TokenSigner {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    /// Key id = first 16 hex chars of blake3(pubkey). Stable across
    /// restarts, changes only on key rotation.
    kid: String,
    issuer: String,
    /// SECURITY (2026-07-26 counter-audit, rope-node/rope-explorer backend
    /// sweep finding F1): `verify()` MUST reject a token whose `aud` claim
    /// doesn't match this value, exactly as every downstream ecosystem
    /// platform is told to do in `handover-datachain-id-sso-live-2026-07-07`
    /// (`jwtVerify(token, JWKS, { issuer, audience })`). Before this fix,
    /// `verify()` checked `iss`/`exp`/signature but never `aud` — harmless
    /// today because this gateway only ever mints the one fixed audience,
    /// but a silent gap in the one place (`/v1/auth/introspect` /
    /// `/v1/auth/userinfo`) that's supposed to be the reference
    /// implementation of the check every partner is told to perform.
    audience: String,
}

impl TokenSigner {
    pub fn new(signing_key: SigningKey, issuer: String, audience: String) -> Self {
        let verifying_key = signing_key.verifying_key();
        let kid = blake3::hash(verifying_key.as_bytes()).to_hex()[..16].to_string();
        Self {
            signing_key,
            verifying_key,
            kid,
            issuer,
            audience,
        }
    }

    /// Load the signing key from `path`, generating a fresh one (mode
    /// 0600) when the file does not exist yet.
    pub fn load_or_generate(path: &str, issuer: String, audience: String) -> anyhow::Result<Self> {
        use std::io::Write;
        let key_bytes: [u8; 32] = match std::fs::read_to_string(path) {
            Ok(content) => {
                let raw = hex::decode(content.trim())
                    .map_err(|e| anyhow::anyhow!("signing key file {path} is not hex: {e}"))?;
                raw.as_slice()
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("signing key file {path} must hold 32 bytes"))?
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = std::path::Path::new(path).parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut bytes = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut bytes);
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                let mut file = opts.open(path)?;
                file.write_all(hex::encode(bytes).as_bytes())?;
                tracing::info!(path, "generated new Ed25519 signing key");
                bytes
            }
            Err(err) => return Err(err.into()),
        };
        Ok(Self::new(SigningKey::from_bytes(&key_bytes), issuer, audience))
    }

    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The JWKS document served at `/.well-known/jwks.json`.
    pub fn jwks(&self) -> serde_json::Value {
        json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "alg": "EdDSA",
                "use": "sig",
                "kid": self.kid,
                "x": URL_SAFE_NO_PAD.encode(self.verifying_key.as_bytes()),
            }]
        })
    }

    /// Mint a compact JWT for the given claims.
    pub fn sign(&self, claims: &Claims) -> String {
        let header = json!({ "alg": "EdDSA", "typ": "JWT", "kid": self.kid });
        let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("header json"));
        let payload_b64 =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("claims json"));
        let signing_input = format!("{header_b64}.{payload_b64}");
        let signature: Signature = self.signing_key.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
    }

    /// Verify a compact JWT minted by this gateway and return its claims.
    pub fn verify(&self, token: &str, now: i64) -> Result<Claims, TokenError> {
        let mut parts = token.split('.');
        let (header_b64, payload_b64, sig_b64) = match (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) {
            (Some(h), Some(p), Some(s), None) => (h, p, s),
            _ => return Err(TokenError::Malformed),
        };

        let header_raw = URL_SAFE_NO_PAD
            .decode(header_b64)
            .map_err(|_| TokenError::Malformed)?;
        let header: serde_json::Value =
            serde_json::from_slice(&header_raw).map_err(|_| TokenError::Malformed)?;
        if header["alg"] != "EdDSA" {
            return Err(TokenError::UnsupportedHeader);
        }
        if let Some(kid) = header["kid"].as_str() {
            if kid != self.kid {
                return Err(TokenError::UnsupportedHeader);
            }
        }

        let sig_raw = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|_| TokenError::Malformed)?;
        let sig_bytes: [u8; 64] = sig_raw
            .as_slice()
            .try_into()
            .map_err(|_| TokenError::Malformed)?;
        let signature = Signature::from_bytes(&sig_bytes);
        let signing_input = format!("{header_b64}.{payload_b64}");
        self.verifying_key
            .verify(signing_input.as_bytes(), &signature)
            .map_err(|_| TokenError::BadSignature)?;

        let payload_raw = URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| TokenError::Malformed)?;
        let claims: Claims =
            serde_json::from_slice(&payload_raw).map_err(|_| TokenError::Malformed)?;
        if claims.exp <= now {
            return Err(TokenError::Expired);
        }
        if claims.iss != self.issuer {
            return Err(TokenError::IssuerMismatch);
        }
        if claims.aud != self.audience {
            return Err(TokenError::AudienceMismatch);
        }
        Ok(claims)
    }
}

/// Convenience constructor for a fresh claim set.
#[allow(clippy::too_many_arguments)]
pub fn build_claims(
    issuer: &str,
    audience: &str,
    ttl_secs: i64,
    now: i64,
    sub: String,
    email: String,
    name: String,
    did: String,
    primary_address: Option<String>,
    wallets: Vec<WalletClaim>,
    public_key: Option<String>,
    amr: Vec<String>,
) -> Claims {
    Claims {
        iss: issuer.to_string(),
        sub,
        aud: audience.to_string(),
        iat: now,
        exp: now + ttl_secs,
        email,
        name,
        did,
        primary_address,
        wallets,
        public_key,
        amr,
        chain_id: ROPE_CHAIN_ID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer() -> TokenSigner {
        TokenSigner::new(
            SigningKey::from_bytes(&[7u8; 32]),
            "https://id.datachain.network".into(),
            "datachain-ecosystem".into(),
        )
    }

    fn sample_claims(now: i64) -> Claims {
        build_claims(
            "https://id.datachain.network",
            "datachain-ecosystem",
            3600,
            now,
            "user-uuid".into(),
            "user@example.com".into(),
            "Test User".into(),
            "did:web:datawallet.plus:user-uuid".into(),
            Some("0xabc0000000000000000000000000000000000def".into()),
            vec![WalletClaim {
                address: "0xabc0000000000000000000000000000000000def".into(),
                wallet_type: "DATACHAIN".into(),
                is_default: true,
                verified: true,
            }],
            None,
            vec!["pwd".into()],
        )
    }

    #[test]
    fn sign_verify_roundtrip() {
        let s = signer();
        let now = 1_800_000_000;
        let token = s.sign(&sample_claims(now));
        let claims = s.verify(&token, now + 10).expect("verify");
        assert_eq!(claims.sub, "user-uuid");
        assert_eq!(claims.email, "user@example.com");
        assert_eq!(claims.chain_id, ROPE_CHAIN_ID);
        assert_eq!(claims.wallets.len(), 1);
    }

    #[test]
    fn expired_token_rejected() {
        let s = signer();
        let now = 1_800_000_000;
        let token = s.sign(&sample_claims(now));
        assert!(matches!(
            s.verify(&token, now + 3601),
            Err(TokenError::Expired)
        ));
    }

    #[test]
    fn tampered_payload_rejected() {
        let s = signer();
        let now = 1_800_000_000;
        let token = s.sign(&sample_claims(now));
        let mut parts: Vec<&str> = token.split('.').collect();
        let forged = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&sample_claims(now + 999_999)).unwrap(),
        );
        parts[1] = &forged;
        let forged_token = parts.join(".");
        assert!(matches!(
            s.verify(&forged_token, now + 10),
            Err(TokenError::BadSignature)
        ));
    }

    #[test]
    fn wrong_signer_rejected() {
        let s = signer();
        let other = TokenSigner::new(
            SigningKey::from_bytes(&[9u8; 32]),
            "https://id.datachain.network".into(),
            "datachain-ecosystem".into(),
        );
        let now = 1_800_000_000;
        let token = s.sign(&sample_claims(now));
        // Different key ⇒ kid mismatch surfaces as UnsupportedHeader.
        assert!(other.verify(&token, now + 10).is_err());
    }

    #[test]
    fn wrong_audience_rejected() {
        // A signer configured for a different audience must reject a
        // token minted for "datachain-ecosystem", even with a correctly
        // matching key/issuer/signature/expiry (finding F1, 2026-07-26).
        let s = TokenSigner::new(
            SigningKey::from_bytes(&[7u8; 32]),
            "https://id.datachain.network".into(),
            "some-other-audience".into(),
        );
        let now = 1_800_000_000;
        let token = s.sign(&sample_claims(now));
        assert!(matches!(
            s.verify(&token, now + 10),
            Err(TokenError::AudienceMismatch)
        ));
    }

    #[test]
    fn jwks_shape() {
        let s = signer();
        let jwks = s.jwks();
        assert_eq!(jwks["keys"][0]["kty"], "OKP");
        assert_eq!(jwks["keys"][0]["crv"], "Ed25519");
        assert_eq!(jwks["keys"][0]["kid"], s.kid());
        assert!(jwks["keys"][0]["x"].as_str().unwrap().len() >= 42);
    }
}
