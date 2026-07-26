//! Runtime configuration for the Datachain ID gateway.
//!
//! Everything is read from the environment once at boot. Secrets
//! (`SUPABASE_SERVICE_ROLE_KEY`) live in `/etc/rope-idp.env` (mode 0600)
//! and are injected by systemd via `EnvironmentFile=`.

use anyhow::{bail, Context, Result};

/// Datachain Rope mainnet chain id — the ecosystem context every token
/// is scoped to.
pub const ROPE_CHAIN_ID: u64 = 271_828;

#[derive(Clone, Debug)]
pub struct Config {
    /// Socket the HTTP server binds to. Default `127.0.0.1:9096` —
    /// only nginx should reach the plaintext listener.
    pub listen: String,
    /// Supabase project base URL (the Datawallet+ backend).
    pub supabase_url: String,
    /// Supabase anon (publishable) key — used for GoTrue password
    /// verification so RLS/MFA policies stay in effect.
    pub supabase_anon_key: String,
    /// Supabase service-role key — used for identity enrichment
    /// (wallet + DID + profile lookups) only. Never exposed to callers.
    pub supabase_service_key: String,
    /// `iss` claim of every minted token.
    pub issuer: String,
    /// `aud` claim of every minted token.
    pub audience: String,
    /// Token lifetime in seconds.
    pub token_ttl_secs: i64,
    /// Path of the Ed25519 signing-key file (32-byte hex). Created with
    /// mode 0600 on first boot when absent.
    pub key_file: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let supabase_url = std::env::var("SUPABASE_URL")
            .context("SUPABASE_URL is required (Datawallet+ Supabase project base URL)")?;
        let supabase_anon_key = std::env::var("SUPABASE_ANON_KEY")
            .context("SUPABASE_ANON_KEY is required")?;
        let supabase_service_key = std::env::var("SUPABASE_SERVICE_ROLE_KEY")
            .context("SUPABASE_SERVICE_ROLE_KEY is required")?;
        if supabase_url.is_empty() || supabase_anon_key.is_empty() || supabase_service_key.is_empty()
        {
            bail!("Supabase configuration must not be empty");
        }
        Ok(Self {
            listen: std::env::var("IDP_LISTEN").unwrap_or_else(|_| "127.0.0.1:9096".into()),
            supabase_url: supabase_url.trim_end_matches('/').to_string(),
            supabase_anon_key,
            supabase_service_key,
            issuer: std::env::var("IDP_ISSUER")
                .unwrap_or_else(|_| "https://id.datachain.network".into()),
            audience: std::env::var("IDP_AUDIENCE")
                .unwrap_or_else(|_| "datachain-ecosystem".into()),
            token_ttl_secs: std::env::var("IDP_TOKEN_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(86_400),
            key_file: std::env::var("IDP_KEY_FILE")
                .unwrap_or_else(|_| "/var/lib/rope-idp/signing-key.hex".into()),
        })
    }
}
