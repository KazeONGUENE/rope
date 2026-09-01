//! Certification-provider registry.
//!
//! Closes finding **C8** of `docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md`:
//! `POST /api/v1/verify/certify` used to accept an arbitrary `provider_id`
//! string from any caller with **zero authentication**. That meant anyone
//! on the internet could inject a fabricated "CertiK audited this
//! contract" (or any other trusted auditor's name) certification, which
//! would then render on the public `/address` page next to a real
//! contract - a direct rug-pull / impersonation vector against users who
//! trust the certification badge.
//!
//! This module gives each onboarded third-party certification provider
//! (an auditing firm, a compliance vendor, ...) its own dedicated bearer
//! secret, so a certification can only be recorded under a given
//! `provider_id` by the party that holds that specific provider's secret.
//! Minting a *generic* self-service DCScan API key (`api_keys.rs`) is
//! deliberately **not** sufficient here - that would only prove "some
//! Datachain ID account made this call", not "the real CertiK made this
//! call". Provider identity has to be provisioned out-of-band by the
//! Datachain Foundation once a real due-diligence relationship exists
//! with that auditor, exactly like the manufacturer/carrier API-key
//! provisioning pattern used elsewhere in the ecosystem (TangibleDC,
//! Mapstore).
//!
//! Providers are configured via the `DCSCAN_CERTIFICATION_PROVIDERS` env
//! var: a JSON array of `{ "provider_id": "...", "display_name": "...",
//! "secret": "..." }` objects, loaded once at boot. Only the BLAKE3 hash
//! of each secret is ever kept in memory or on disk - the plaintext lives
//! only in the operator's secret manager / env file.
//!
//! **No-stubs guarantee:** when the env var is unset or empty (the
//! out-of-the-box state - no auditor has been onboarded yet), the
//! registry is empty and every certification submission is honestly
//! refused with `501 Not Implemented`, never silently accepted. This
//! matches the "honest empty/501 over fabricated success" directive
//! applied to the databox network endpoints in the same audit pass.

use std::collections::HashMap;

use serde::Deserialize;

/// One onboarded certification provider.
#[derive(Clone)]
pub struct ProviderRecord {
    pub display_name: String,
    secret_hash: String,
}

/// In-memory registry, loaded once at boot from `DCSCAN_CERTIFICATION_PROVIDERS`.
pub struct CertificationProviderRegistry {
    providers: HashMap<String, ProviderRecord>,
}

#[derive(Deserialize)]
struct RawProvider {
    provider_id: String,
    display_name: String,
    secret: String,
}

impl CertificationProviderRegistry {
    pub fn load() -> Self {
        let raw = std::env::var("DCSCAN_CERTIFICATION_PROVIDERS").unwrap_or_default();
        if raw.trim().is_empty() {
            tracing::info!(
                "CertificationProviderRegistry: DCSCAN_CERTIFICATION_PROVIDERS not set - \
                 POST /api/v1/verify/certify will fail closed (501) for every submission until \
                 at least one provider is onboarded. This is intentional (see finding C8 of \
                 SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md) - it replaces the previous \
                 behaviour of silently accepting an unauthenticated certification under any \
                 claimed provider name."
            );
            return Self {
                providers: HashMap::new(),
            };
        }
        let parsed: Vec<RawProvider> = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    "CertificationProviderRegistry: DCSCAN_CERTIFICATION_PROVIDERS is set but \
                     failed to parse as a JSON array of {{provider_id,display_name,secret}} \
                     objects ({e}) - starting with ZERO providers (fail closed, not fail open)"
                );
                Vec::new()
            }
        };
        let mut providers = HashMap::new();
        for p in parsed {
            let id = p.provider_id.trim().to_lowercase();
            let secret = p.secret.trim();
            if id.is_empty() || secret.is_empty() {
                tracing::warn!(
                    "CertificationProviderRegistry: skipping malformed provider entry \
                     (empty provider_id or secret)"
                );
                continue;
            }
            providers.insert(
                id,
                ProviderRecord {
                    display_name: p.display_name,
                    secret_hash: blake3::hash(secret.as_bytes()).to_hex().to_string(),
                },
            );
        }
        tracing::info!(
            "CertificationProviderRegistry: loaded {} onboarded certification provider(s)",
            providers.len()
        );
        Self { providers }
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Authenticate a certification submission. The caller must present
    /// the exact secret provisioned for the claimed `provider_id`
    /// (case-insensitive id match, constant-time secret comparison).
    pub fn authenticate(&self, provider_id: &str, presented_secret: &str) -> Option<ProviderRecord> {
        if presented_secret.trim().is_empty() {
            return None;
        }
        let id = provider_id.trim().to_lowercase();
        let record = self.providers.get(&id)?;
        let presented_hash = blake3::hash(presented_secret.trim().as_bytes())
            .to_hex()
            .to_string();
        if constant_time_eq(record.secret_hash.as_bytes(), presented_hash.as_bytes()) {
            Some(record.clone())
        } else {
            None
        }
    }
}

/// Constant-time byte comparison - avoids a timing side-channel on the
/// hash comparison (defense in depth; the hash itself already collapses
/// most of the signal, but there is no reason to accept any leak).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests that mutate `DCSCAN_CERTIFICATION_PROVIDERS` must not run
    // concurrently with each other (Rust runs `#[test]` fns on multiple
    // threads sharing one process env) - same pattern as
    // `rope_auth::tests::with_env`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn registry_with(id: &str, secret: &str) -> CertificationProviderRegistry {
        let mut providers = HashMap::new();
        providers.insert(
            id.to_string(),
            ProviderRecord {
                display_name: id.to_string(),
                secret_hash: blake3::hash(secret.as_bytes()).to_hex().to_string(),
            },
        );
        CertificationProviderRegistry { providers }
    }

    #[test]
    fn empty_registry_when_env_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("DCSCAN_CERTIFICATION_PROVIDERS");
        let reg = CertificationProviderRegistry::load();
        assert!(reg.is_empty());
        assert!(reg.authenticate("certik", "anything").is_none());
    }

    #[test]
    fn empty_registry_on_malformed_json() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("DCSCAN_CERTIFICATION_PROVIDERS", "{ not valid json");
        let reg = CertificationProviderRegistry::load();
        std::env::remove_var("DCSCAN_CERTIFICATION_PROVIDERS");
        assert!(reg.is_empty());
    }

    #[test]
    fn authenticates_matching_secret_case_insensitive_id() {
        let reg = registry_with("certik", "s3cr3t-value");
        assert!(reg.authenticate("CertiK", "s3cr3t-value").is_some());
        assert!(reg.authenticate("certik", "wrong-secret").is_none());
        assert!(reg.authenticate("unknown-provider", "s3cr3t-value").is_none());
    }

    #[test]
    fn rejects_empty_secret() {
        let reg = registry_with("certik", "s3cr3t-value");
        assert!(reg.authenticate("certik", "").is_none());
        assert!(reg.authenticate("certik", "   ").is_none());
    }

    #[test]
    fn skips_malformed_entries_but_keeps_valid_ones() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(
            "DCSCAN_CERTIFICATION_PROVIDERS",
            r#"[{"provider_id":"","display_name":"Bad","secret":"x"},
                {"provider_id":"good","display_name":"Good Auditor","secret":"realsecret"}]"#,
        );
        let reg = CertificationProviderRegistry::load();
        std::env::remove_var("DCSCAN_CERTIFICATION_PROVIDERS");
        assert!(!reg.is_empty());
        assert!(reg.authenticate("good", "realsecret").is_some());
        assert!(reg.authenticate("", "x").is_none());
    }
}
