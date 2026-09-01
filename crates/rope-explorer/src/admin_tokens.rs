//! Dynamic, email-bound admin-token generator for Datachain Rope.
//!
//! Replaces the static `PROJECTS_ADMIN_TOKEN` / `NODE_REQUESTS_ADMIN_TOKEN` /
//! `ECOSYSTEM_ADMIN_TOKEN` / `INTEGRATION_REQUESTS_ADMIN_TOKEN` env-var
//! escape hatches with a self-service issuance flow whose blast radius is
//! bounded to a single email address, a single role, and a 7-day TTL.
//!
//! # Wire format
//!
//! Tokens are 32 bytes of CSPRNG output, URL-safe base64-encoded to 43
//! characters, and prefixed with a role tag so a leaked token can be
//! recognised at a glance in log lines:
//!
//! * `dcrope_pa_<43>` — project admin
//! * `dcrope_na_<43>` — node admin
//! * `dcrope_ma_<43>` — multi-role (holder is eligible for **both**
//!   project and node admin actions; issued when the same email is
//!   eligible for both roles at request time)
//!
//! Only the SHA-256 hash of the raw token is ever persisted; the raw
//! token exists on disk exactly zero times. If the operator loses their
//! token they must request a new one.
//!
//! # Storage
//!
//! * `/opt/datachain-rope/admin-tokens.jsonl` (override with
//!   `ADMIN_TOKENS_PATH`) — append-only JSON-lines log.  Each line is
//!   either an `issued` record or a `revoked` record.  The runtime map
//!   is rebuilt from this file at startup, so a corrupt or partial
//!   write cannot brick admin access.
//! * `0x000000000000000000000000000000000000d005` (override with
//!   `ADMIN_TOKENS_LEDGER_WALLET`) — dedicated on-chain ledger.  Every
//!   issue/revoke event is anchored as an `AdminTokenIssued` /
//!   `AdminTokenRevoked` knot via `rope_appendToLedger`, mirroring the
//!   `NODE_REQUESTS_LEDGER_WALLET` pattern.
//!
//! # Auth surface migration (2026-08-14)
//!
//! Per the `migrate_now` decision, every existing admin gate that read
//! a static env var now calls [`require_role`] instead.  The env vars
//! are no longer honoured, so an unknown or expired token yields the
//! canonical `403 admin_token_invalid` response regardless of what the
//! operator has in their systemd unit file.
//!
//! # Bootstrap
//!
//! The very first token per role is minted via `POST
//! /api/v1/admin-tokens/bootstrap` with an Ed25519 signature from a
//! founder key listed in `deploy/config/master-nodes.toml`.  Subsequent
//! tokens for the same email can be self-serve via `POST
//! /api/v1/admin-tokens/request` as long as the email is in the
//! built-in allowlist or has been declared through one of the node /
//! project / databox / EDC deploy channels.
//!
//! # Domain policy
//!
//! * `PROJECT_ADMIN_ALLOWED_DOMAINS` (built-in default): `onguene.com,
//!   onguene.org, datachain.one, datachain.network, databox.network,
//!   xn--databx-yta.com` (the last one is `databØx.com` in Punycode).
//! * `NODE_ADMIN_EXTRA_DOMAINS`: same list + any email that appears in
//!   `node-requests.jsonl` or `projects.jsonl` (any status).
//!
//! # Auto-renewal
//!
//! A single background task (`spawn_renewal_loop`) wakes up every
//! `ADMIN_TOKENS_RENEWAL_TICK_SECS` (default 3600 s) and issues a fresh
//! token for any active record whose `expires_at` is within 24 h.  The
//! new token is emailed to the same address that received the previous
//! one; the old token is revoked in the same transaction.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use tokio::sync::RwLock;

/// Role a token carries.  `MultiRole` is granted when the requesting
/// email is eligible for both `ProjectAdmin` and `NodeAdmin` at the
/// moment of issuance; downgrading to a single-role token is not
/// supported (the operator revokes and requests a new one).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    ProjectAdmin,
    NodeAdmin,
    /// One token, both roles.  Emitted when the requester's email is
    /// eligible for both roles at request time.
    MultiRole,
}

impl Role {
    pub fn prefix(self) -> &'static str {
        match self {
            Role::ProjectAdmin => "dcrope_pa_",
            Role::NodeAdmin => "dcrope_na_",
            Role::MultiRole => "dcrope_ma_",
        }
    }

    /// Does this role grant `wanted`?  MultiRole grants both concrete
    /// roles; the concrete roles do not grant each other.
    pub fn grants(self, wanted: Role) -> bool {
        match (self, wanted) {
            (Role::MultiRole, _) => true,
            (a, b) if a == b => true,
            _ => false,
        }
    }
}

/// One durable record persisted to `admin-tokens.jsonl`.  Only the
/// SHA-256 hash of the raw token is stored; the raw token is emailed
/// to the recipient and never touches disk again.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenRecord {
    /// Opaque record id (`at-<yyyymmddhhmmss>-<8-hex>`).  Used in
    /// on-chain anchor metadata so an ops audit can cross-check a
    /// specific issuance without exposing the token hash.
    pub id: String,
    /// SHA-256 of the raw token, lower-case hex, exactly 64 chars.
    /// This is what [`verify_and_role`] compares against.
    pub token_sha256: String,
    /// Which admin surface(s) this token grants access to.
    pub role: Role,
    /// Email that received the raw token.  Lower-cased for the domain
    /// check; the original casing is preserved for the audit trail.
    pub email: String,
    /// Same email as `email` but forced to lowercase — used for the
    /// domain-allowlist check.  Cached to avoid re-normalising on
    /// every verify call.
    pub email_lc: String,
    /// Bootstrap issuance never has a `previous_token_sha256`.  Every
    /// subsequent issuance for the same email points back at the
    /// previous record for that email so a chain of custody is
    /// reconstructable from the JSONL alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_token_sha256: Option<String>,
    /// Unix seconds when this record was written.
    pub issued_at: i64,
    /// Unix seconds after which [`verify_and_role`] rejects the token.
    /// Set to `issued_at + ADMIN_TOKEN_TTL_SECS`.
    pub expires_at: i64,
    /// `None` means the token is still active.  `Some(unix_secs)` is
    /// the moment a revoke record was appended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<i64>,
    /// Why the token was revoked (e.g. `"rotated"`, `"admin_request"`,
    /// `"email_bounced"`).  Absent for still-active records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoke_reason: Option<String>,
    /// How the token got its authority — `"bootstrap"` for the first
    /// token per role, `"self_serve"` for allowlist-domain requests,
    /// `"eligibility_snapshot"` for node-admins declared through a
    /// deploy channel, `"auto_renewal"` for tokens minted by the
    /// background task.
    pub source: String,
    /// Free-form snapshot of the eligibility evidence at issuance
    /// time.  For allowlist domains this is the matched domain; for
    /// eligibility-snapshot tokens it lists the JSONL records that
    /// declared the email.  Never used at verify time; kept for
    /// audit and for the `/api/v1/admin-tokens` (super-admin) list.
    pub eligibility: Value,
}

impl TokenRecord {
    fn is_active(&self, now: i64) -> bool {
        self.revoked_at.is_none() && self.expires_at > now
    }
}

/// One line of the JSONL log.  Split so we can serialise a plain
/// "revoke" line without carrying the full record shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LogEntry {
    Issued(TokenRecord),
    Revoked {
        /// Same as `TokenRecord::token_sha256` — the raw hash we're
        /// tombstoning.
        token_sha256: String,
        revoked_at: i64,
        reason: String,
    },
}

/// Thread-safe in-memory index for the token store.  Keyed by the
/// SHA-256 hash so verify is O(1).  Cloneable via `Arc` for the
/// background renewal loop.
#[derive(Debug, Default)]
pub struct AdminTokenStore {
    /// Records indexed by their token hash.  Revoked records stay in
    /// the map so replay attempts can be logged with the original
    /// issuance metadata.
    inner: RwLock<HashMap<String, TokenRecord>>,
    /// Absolute path of the JSONL log the map was rebuilt from.
    path: PathBuf,
    /// Founder Ed25519 verifying keys loaded from
    /// `deploy/config/master-nodes.toml` at startup.  Empty vec ⇒
    /// bootstrap endpoint is disabled and answers 501.
    founder_keys: Vec<VerifyingKey>,
    /// Domains that mint self-serve project-admin tokens without any
    /// eligibility snapshot.  See `project_admin_domains()`.
    project_admin_domains: Vec<String>,
}

impl AdminTokenStore {
    /// Load or create the store at `path`.  Missing files are
    /// tolerated (returns an empty store); a corrupt line short-
    /// circuits with an error so the operator sees the problem before
    /// admin access silently falls open.
    pub fn load(path: PathBuf, founder_keys: Vec<VerifyingKey>) -> std::io::Result<Self> {
        let mut inner: HashMap<String, TokenRecord> = HashMap::new();
        if path.exists() {
            let contents = std::fs::read_to_string(&path)?;
            for (line_no, line) in contents.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let entry: LogEntry = serde_json::from_str(line).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("admin-tokens.jsonl:{}: {}", line_no + 1, e),
                    )
                })?;
                match entry {
                    LogEntry::Issued(rec) => {
                        inner.insert(rec.token_sha256.clone(), rec);
                    }
                    LogEntry::Revoked {
                        token_sha256,
                        revoked_at,
                        reason,
                    } => {
                        if let Some(rec) = inner.get_mut(&token_sha256) {
                            rec.revoked_at = Some(revoked_at);
                            rec.revoke_reason = Some(reason);
                        }
                    }
                }
            }
        }
        Ok(Self {
            inner: RwLock::new(inner),
            path,
            founder_keys,
            project_admin_domains: project_admin_domains(),
        })
    }

    /// Constant-time-ish verify: hash the provided token, then look up
    /// the resulting hash in the map.  The map lookup itself is not
    /// constant time (HashMap), but the leaked timing side-channel is
    /// merely "does a token with this hash exist" which is not a
    /// secret — the token *value* is.
    pub async fn verify_and_role(&self, raw_token: &str) -> Option<Role> {
        let hash = hash_token(raw_token);
        let now = chrono::Utc::now().timestamp();
        let map = self.inner.read().await;
        let rec = map.get(&hash)?;
        if !rec.is_active(now) {
            return None;
        }
        // Prefix must match role — this catches a rotated-role attack
        // where a stored MultiRole hash is presented with a `pa_`
        // wrapper (the wrapper is decoration; only the hash matters
        // for auth, but we reject the mismatch to make audit logs
        // readable).
        let prefix = rec.role.prefix();
        if !raw_token.starts_with(prefix) {
            tracing::warn!(
                "admin-token verify: prefix mismatch (record={}, got={})",
                prefix,
                raw_token.chars().take(11).collect::<String>()
            );
            return None;
        }
        Some(rec.role)
    }

    /// Append `entry` to the JSONL log, then update the in-memory map
    /// on success.  Best-effort append/update discipline: if the map
    /// mutation panics, the JSONL is the source of truth (a restart
    /// rebuilds the correct map).
    async fn append(&self, entry: LogEntry) -> std::io::Result<()> {
        let line = serde_json::to_string(&entry).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        let path = self.path.clone();
        let line_owned = format!("{line}\n");
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            use std::io::Write;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            f.write_all(line_owned.as_bytes())?;
            f.flush()?;
            Ok(())
        })
        .await
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))??;

        let mut map = self.inner.write().await;
        match entry {
            LogEntry::Issued(rec) => {
                map.insert(rec.token_sha256.clone(), rec);
            }
            LogEntry::Revoked {
                token_sha256,
                revoked_at,
                reason,
            } => {
                if let Some(rec) = map.get_mut(&token_sha256) {
                    rec.revoked_at = Some(revoked_at);
                    rec.revoke_reason = Some(reason);
                }
            }
        }
        Ok(())
    }

    /// Enumerate active records (used by super-admin GET
    /// `/api/v1/admin-tokens` and by the auto-renewal loop).  The
    /// concrete `role` here is the record's stored role, not what
    /// `wanted` filters — we return every active record because the
    /// renewal loop and the audit list need everything.
    pub async fn active_records(&self) -> Vec<TokenRecord> {
        let now = chrono::Utc::now().timestamp();
        let map = self.inner.read().await;
        map.values()
            .filter(|r| r.is_active(now))
            .cloned()
            .collect()
    }

    /// Check whether a MultiRole (or the given `role`) record already
    /// exists for `email_lc`.  Callers use this to decide whether the
    /// bootstrap endpoint should be gated.
    pub async fn has_active_for_email(&self, email_lc: &str) -> bool {
        let now = chrono::Utc::now().timestamp();
        let map = self.inner.read().await;
        map.values().any(|r| r.email_lc == email_lc && r.is_active(now))
    }

    /// Convenience constructor that reads `ADMIN_TOKENS_PATH` and
    /// `ADMIN_TOKENS_FOUNDER_KEYS_PATH` from the process environment,
    /// falling back to `/opt/datachain-rope/admin-tokens.jsonl` and
    /// `/opt/datachain-rope/config/master-nodes.toml` respectively.
    /// A missing or unreadable JSONL yields an empty store (bootstrap
    /// endpoint is the only way in from that state); a missing
    /// founder-keys file yields `founder_keys = []` which disables the
    /// bootstrap endpoint with a 501 response.
    pub fn from_env() -> Self {
        let path = default_store_path();
        let founder_keys = load_founder_keys(&default_founder_keys_path());
        match Self::load(path.clone(), founder_keys.clone()) {
            Ok(store) => store,
            Err(e) => {
                tracing::error!(
                    "admin-tokens: failed to load JSONL at {}: {} (starting with empty store)",
                    path.display(),
                    e
                );
                Self {
                    inner: RwLock::new(HashMap::new()),
                    path,
                    founder_keys,
                    project_admin_domains: project_admin_domains(),
                }
            }
        }
    }
}

/// SHA-256 hex of `raw_token` — the canonical index key.
fn hash_token(raw_token: &str) -> String {
    let digest = sha2::Sha256::digest(raw_token.as_bytes());
    hex::encode(digest)
}

/// Constant-time equality on two hex strings of equal length.  Used
/// only in the founder-key comparison; token hashes go through the
/// HashMap which is not constant-time (but the map key is a public
/// hash, not a secret).
fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Domains that mint project-admin tokens without any additional
/// eligibility snapshot.  Founder-controlled by `master-nodes.toml`
/// authority — the operator can add domains through
/// `PROJECT_ADMIN_ALLOWED_DOMAINS` (comma-separated) but the built-in
/// baseline is always honoured.
fn project_admin_domains() -> Vec<String> {
    let mut v = vec![
        "onguene.com".to_string(),
        "onguene.org".to_string(),
        "datachain.one".to_string(),
        "datachain.network".to_string(),
        "databox.network".to_string(),
        // `databØx.com` — the `Ø` is U+00D8.  Encoded in Punycode
        // (RFC 3492) via the `idna` crate; the on-wire form email
        // clients hand us is already lowercased ASCII IDN form.  The
        // Punycode for `databØx` is `xn--databx-yta`.
        "xn--databx-yta.com".to_string(),
    ];
    if let Ok(extra) = std::env::var("PROJECT_ADMIN_ALLOWED_DOMAINS") {
        for d in extra.split(',') {
            let d = d.trim().to_lowercase();
            if !d.is_empty() && !v.contains(&d) {
                v.push(d);
            }
        }
    }
    v
}

/// Load founder Ed25519 verifying keys from `master-nodes.toml`.
/// Bootstrap is disabled when no keys are configured.
pub fn load_founder_keys(master_nodes_path: &std::path::Path) -> Vec<VerifyingKey> {
    let contents = match std::fs::read_to_string(master_nodes_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "admin-tokens: master-nodes.toml not readable at {}: {} (bootstrap disabled)",
                master_nodes_path.display(),
                e
            );
            return Vec::new();
        }
    };
    let parsed: toml::Value = match toml::from_str(&contents) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "admin-tokens: master-nodes.toml parse error: {} (bootstrap disabled)",
                e
            );
            return Vec::new();
        }
    };
    // The canonical layout puts `founder_keys` under a `[founder]`
    // section (matches how `rope-node` reads the same file — see
    // `crates/rope-node/src/governance.rs::FounderAuthority`).  We also
    // accept a flat top-level `founder_keys = [...]` fixture so unit
    // tests and small dev configs keep working; if both are present the
    // nested form wins.
    let nested = parsed
        .get("founder")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("founder_keys"))
        .and_then(|v| v.as_array())
        .cloned();
    let keys_raw = match nested {
        Some(arr) => arr,
        None => parsed
            .get("founder_keys")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
    };
    let mut out = Vec::new();
    for k in keys_raw {
        if let Some(hex_str) = k.as_str() {
            let bytes = match hex::decode(hex_str.trim()) {
                Ok(b) if b.len() == 32 => b,
                _ => {
                    tracing::warn!("admin-tokens: skipping non-32-byte founder key {:?}", hex_str);
                    continue;
                }
            };
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            match VerifyingKey::from_bytes(&arr) {
                Ok(vk) => out.push(vk),
                Err(e) => {
                    tracing::warn!("admin-tokens: invalid founder key {:?}: {}", hex_str, e);
                }
            }
        }
    }
    tracing::info!("admin-tokens: {} founder key(s) loaded", out.len());
    out
}

/// Default JSONL location.  Override with `ADMIN_TOKENS_PATH`.
pub fn default_store_path() -> PathBuf {
    std::env::var("ADMIN_TOKENS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/datachain-rope/admin-tokens.jsonl"))
}

/// Default founder-key file.  Override with `ADMIN_TOKENS_FOUNDER_KEYS_PATH`.
pub fn default_founder_keys_path() -> PathBuf {
    std::env::var("ADMIN_TOKENS_FOUNDER_KEYS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/datachain-rope/config/master-nodes.toml"))
}

/// Dedicated on-chain wallet.  Every issue/revoke event is anchored
/// as a knot on this wallet's personal-ledger string.  Override with
/// `ADMIN_TOKENS_LEDGER_WALLET`.
pub fn ledger_wallet() -> String {
    std::env::var("ADMIN_TOKENS_LEDGER_WALLET")
        .unwrap_or_else(|_| "0x000000000000000000000000000000000000d005".to_string())
}

/// TTL for freshly-issued tokens (default 7 days).  The auto-renewal
/// task fires when a token is within one renewal-window (default
/// 24 h) of expiry.
pub fn ttl_secs() -> i64 {
    std::env::var("ADMIN_TOKEN_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n >= 3600)
        .unwrap_or(7 * 86_400)
}

/// How close to expiry (in seconds) the auto-renewal loop should
/// treat a token as "due".  Default 24 h.
pub fn renewal_window_secs() -> i64 {
    std::env::var("ADMIN_TOKEN_RENEWAL_WINDOW_SECS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n >= 300)
        .unwrap_or(86_400)
}

/// Domain of `email`, lowercased.  `None` if the email is malformed.
fn email_domain_lc(email: &str) -> Option<String> {
    let (_local, domain) = email.split_once('@')?;
    if domain.is_empty() {
        return None;
    }
    Some(domain.trim().to_lowercase())
}

/// Very small email-shape validator; strict enough that a random
/// string in the wrong field is caught before we hit the JSONL log
/// with garbage.
fn looks_like_email(s: &str) -> bool {
    let s = s.trim();
    if s.len() < 5 || s.len() > 254 {
        return false;
    }
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    if !domain.contains('.') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_graphic() || c == ' ')
        .then_some(true)
        .unwrap_or(false)
}

/// Mint a new random token for `role` — 32 bytes of CSPRNG → base64url.
/// Returns `(raw_token, sha256_hex)` so the caller can email the raw
/// value and persist the hash in the same transaction.
fn mint_token(role: Role) -> (String, String) {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let b64 =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let raw = format!("{}{}", role.prefix(), b64);
    let hash = hash_token(&raw);
    (raw, hash)
}

/// Result of a per-request eligibility check.  Cheap to construct
/// even when the answer is "no" so the caller can log the reason.
#[derive(Clone, Debug)]
pub struct EligibilitySnapshot {
    pub roles: Vec<Role>,
    pub evidence: Value,
}

impl EligibilitySnapshot {
    fn best_role(&self) -> Option<Role> {
        if self.roles.contains(&Role::ProjectAdmin) && self.roles.contains(&Role::NodeAdmin) {
            Some(Role::MultiRole)
        } else if self.roles.contains(&Role::ProjectAdmin) {
            Some(Role::ProjectAdmin)
        } else if self.roles.contains(&Role::NodeAdmin) {
            Some(Role::NodeAdmin)
        } else {
            None
        }
    }
}

/// Compute what `email_lc` is eligible for, right now, from the local
/// JSONL data.  This is intentionally coarse — anyone who has ever
/// submitted a node-request or a project submission gets node-admin
/// eligibility.  Domain-allowlisted emails also get project-admin.
pub fn compute_eligibility(store: &AdminTokenStore, email_lc: &str) -> EligibilitySnapshot {
    let mut roles = Vec::new();
    let mut evidence = json!({});

    // (a) Built-in / operator-extended project-admin domain allowlist.
    if let Some(domain) = email_domain_lc(email_lc) {
        for allowed in &store.project_admin_domains {
            if domain == *allowed {
                roles.push(Role::ProjectAdmin);
                roles.push(Role::NodeAdmin); // domain allowlist grants both
                evidence["project_admin_domain"] = json!(allowed);
                break;
            }
        }
    }

    // (b) Node-admin eligibility from declared JSONL sources.  Any
    // email that already appears in a node-request or project
    // submission is eligible for a node-admin token.  This covers the
    // "Deploy a Node" form, the "Submit Your Project" form, the
    // `ropectl deploy-node` CLI (which writes into the same JSONL),
    // and the EDC console (which now writes through the same rope
    // ledger — mirrored back into `projects.jsonl` on cache refresh).
    let mut evidence_sources: Vec<Value> = Vec::new();
    for jsonl in [
        std::env::var("NODE_REQUESTS_PATH")
            .unwrap_or_else(|_| "/opt/datachain-rope/node-requests.jsonl".into()),
        std::env::var("PROJECTS_PATH")
            .unwrap_or_else(|_| "/opt/datachain-rope/projects.jsonl".into()),
    ] {
        if let Ok(contents) = std::fs::read_to_string(&jsonl) {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(rec) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let match_email = |field: &str| -> bool {
                    rec.get(field)
                        .and_then(|v| v.as_str())
                        .map(|e| e.trim().to_lowercase() == email_lc)
                        .unwrap_or(false)
                };
                if match_email("email")
                    || match_email("submitter_email")
                    || match_email("contact_email")
                {
                    let entry = json!({
                        "source": jsonl,
                        "record_id": rec.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                        "status": rec.get("status").and_then(|v| v.as_str()).unwrap_or(""),
                    });
                    if !evidence_sources.iter().any(|e| e == &entry) {
                        evidence_sources.push(entry);
                    }
                }
            }
        }
    }
    if !evidence_sources.is_empty() {
        if !roles.contains(&Role::NodeAdmin) {
            roles.push(Role::NodeAdmin);
        }
        evidence["node_admin_declared_in"] = json!(evidence_sources);
    }

    EligibilitySnapshot { roles, evidence }
}

/// Verify an Ed25519 signature over the canonical bootstrap message
/// against the loaded founder key set.  Returns `Some(pubkey_hex)` on
/// success so the audit trail records which key authorised the mint.
fn verify_founder_signature(
    store: &AdminTokenStore,
    email_lc: &str,
    roles_csv: &str,
    timestamp: i64,
    signature_hex: &str,
    signer_hex: &str,
) -> Option<String> {
    // Freshness — reject anything older than 10 minutes or more than 2
    // minutes in the future (allow small clock skew).
    let now = chrono::Utc::now().timestamp();
    if timestamp < now - 600 || timestamp > now + 120 {
        return None;
    }
    let sig_bytes = hex::decode(signature_hex.trim()).ok()?;
    if sig_bytes.len() != 64 {
        return None;
    }
    let signer_bytes = hex::decode(signer_hex.trim()).ok()?;
    if signer_bytes.len() != 32 {
        return None;
    }
    // Signer must be in the founder set — comparison in constant time
    // over the hex encoding.
    let signer_hex_lc = signer_hex.trim().to_lowercase();
    let mut matched = None;
    for key in &store.founder_keys {
        let key_hex = hex::encode(key.to_bytes());
        if ct_eq(&key_hex, &signer_hex_lc) {
            matched = Some(key);
            break;
        }
    }
    let vk = matched?;
    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(&sig_bytes);
    let sig = Signature::from_bytes(&sig_arr);
    let msg = bootstrap_message(email_lc, roles_csv, timestamp);
    vk.verify(msg.as_bytes(), &sig).ok()?;
    Some(signer_hex_lc)
}

/// Canonical bootstrap signing message.  Domain-tagged so a signature
/// minted for another Datachain Rope surface cannot be replayed here.
fn bootstrap_message(email_lc: &str, roles_csv: &str, timestamp: i64) -> String {
    format!("DCROPE-ADMIN-TOKEN-BOOTSTRAP\n{email_lc}\n{roles_csv}\n{timestamp}")
}

// ---------------------------------------------------------------------------
// Anchor helpers (`rope_appendToLedger`).  Best-effort — the JSONL
// write is durable regardless of anchor success, and any missed
// anchor can be reconciled from the JSONL by a manual replay.
// ---------------------------------------------------------------------------

async fn anchor_issued(state: &crate::AppState, record: &TokenRecord) {
    let wallet = ledger_wallet();
    let rpc = state.rpc_url_active().to_string();
    let create = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "rope_createPersonalLedger",
        "params": [wallet],
    });
    let _ = state.http_client.post(&rpc).json(&create).send().await;
    let append = json!({
        "jsonrpc": "2.0", "id": 2,
        "method": "rope_appendToLedger",
        "params": [wallet, {
            "interaction_type": "AdminTokenIssued",
            "description": json!({
                "id": record.id,
                "role": record.role,
                "email_lc": record.email_lc,
                "token_sha256_prefix": record.token_sha256.chars().take(12).collect::<String>(),
                "expires_at": record.expires_at,
                "source": record.source,
                "previous_token_sha256_prefix": record
                    .previous_token_sha256
                    .as_ref()
                    .map(|h| h.chars().take(12).collect::<String>()),
            }).to_string(),
            "metadata": {
                "record_id": record.id,
                "role": format!("{:?}", record.role),
                "email_lc": record.email_lc,
                "expires_at": record.expires_at,
                "source": record.source.clone(),
            }
        }],
    });
    if let Err(e) = state.http_client.post(&rpc).json(&append).send().await {
        tracing::warn!("admin-token anchor (issued) failed: {}", e);
    }
}

async fn anchor_revoked(state: &crate::AppState, hash: &str, reason: &str) {
    let wallet = ledger_wallet();
    let rpc = state.rpc_url_active().to_string();
    let append = json!({
        "jsonrpc": "2.0", "id": 3,
        "method": "rope_appendToLedger",
        "params": [wallet, {
            "interaction_type": "AdminTokenRevoked",
            "description": json!({
                "token_sha256_prefix": hash.chars().take(12).collect::<String>(),
                "reason": reason,
            }).to_string(),
            "metadata": {
                "token_sha256_prefix": hash.chars().take(12).collect::<String>(),
                "reason": reason,
            }
        }],
    });
    if let Err(e) = state.http_client.post(&rpc).json(&append).send().await {
        tracing::warn!("admin-token anchor (revoked) failed: {}", e);
    }
}

// ---------------------------------------------------------------------------
// Public auth helper — every migrated call site funnels through here.
// ---------------------------------------------------------------------------

/// Extract the presented token from the standard `X-Admin-Token`
/// header, hash it, and check that the resulting record is (a) still
/// active and (b) grants `wanted`.  Returns a canonical error tuple
/// on any failure so handler call sites are a single `?` away.
///
/// The env-var escape hatches (`PROJECTS_ADMIN_TOKEN`,
/// `NODE_REQUESTS_ADMIN_TOKEN`, `ECOSYSTEM_ADMIN_TOKEN`,
/// `INTEGRATION_REQUESTS_ADMIN_TOKEN`) are **not** consulted here.
/// Per the `migrate_now` decision, only dynamic tokens are honoured.
pub async fn require_role(
    store: &AdminTokenStore,
    headers: &HeaderMap,
    wanted: Role,
) -> Result<(), (StatusCode, Json<Value>)> {
    let raw = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(raw) = raw else {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "admin_token_missing",
                "message": "X-Admin-Token header is required",
            })),
        ));
    };
    match store.verify_and_role(raw).await {
        Some(role) if role.grants(wanted) => Ok(()),
        Some(role) => Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "admin_token_role_mismatch",
                "message": format!(
                    "token grants {:?}, endpoint requires {:?}",
                    role, wanted
                ),
            })),
        )),
        None => Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "admin_token_invalid",
                "message": "unknown or expired admin token",
            })),
        )),
    }
}

// ---------------------------------------------------------------------------
// HTTP handlers.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RequestBody {
    email: String,
    /// Anti-spam honeypot.  If a client fills this in we silently
    /// return 202 as if success — same pattern as the integration-
    /// request submit handler.
    #[serde(default)]
    website: String,
}

/// `POST /api/v1/admin-tokens/request` — self-serve mint for eligible
/// emails.  Domain-allowlisted addresses receive a MultiRole token
/// immediately; other addresses are checked against the JSONL
/// declaration channels for node-admin eligibility.  Non-eligible
/// requesters get a canonical `403 not_eligible`.
pub async fn request_token(
    State(state): State<Arc<crate::AppState>>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<RequestBody>,
) -> (StatusCode, Json<Value>) {
    let _peer = peer; // reserved for future per-IP rate limiting
    if !body.website.is_empty() {
        // Silent honeypot drop.
        return (
            StatusCode::ACCEPTED,
            Json(json!({ "success": true, "message": "queued" })),
        );
    }
    let email = body.email.trim();
    if !looks_like_email(email) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "error": "invalid_email",
                "message": "email is malformed",
            })),
        );
    }
    let email_lc = email.to_lowercase();
    let eligibility = compute_eligibility(&state.admin_tokens, &email_lc);
    let Some(role) = eligibility.best_role() else {
        tracing::info!("admin-token request rejected: {} not eligible", email_lc);
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "not_eligible",
                "message":
                    "email is not in the allowlist and has not been declared through any deploy channel",
            })),
        );
    };
    mint_and_email(
        &state,
        email,
        &email_lc,
        role,
        "self_serve",
        eligibility.evidence,
    )
    .await
}

#[derive(Deserialize)]
pub struct BootstrapBody {
    email: String,
    /// One of `project_admin`, `node_admin`, `multi_role`.
    role: String,
    /// Unix seconds at signing time — used verbatim in the canonical
    /// signing message, checked for freshness.
    timestamp: i64,
    signature: String,
    signer: String,
}

/// `POST /api/v1/admin-tokens/bootstrap` — mint a token for `email`
/// with a founder-signed Ed25519 payload.  Available only when the
/// server has founder keys loaded; otherwise answers 501.
pub async fn bootstrap_token(
    State(state): State<Arc<crate::AppState>>,
    Json(body): Json<BootstrapBody>,
) -> (StatusCode, Json<Value>) {
    if state.admin_tokens.founder_keys.is_empty() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "success": false,
                "error": "bootstrap_disabled",
                "message": "no founder keys configured (see ADMIN_TOKENS_FOUNDER_KEYS_PATH)",
            })),
        );
    }
    let email = body.email.trim();
    if !looks_like_email(email) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "invalid_email" })),
        );
    }
    let email_lc = email.to_lowercase();
    let role = match body.role.trim().to_lowercase().as_str() {
        "project_admin" => Role::ProjectAdmin,
        "node_admin" => Role::NodeAdmin,
        "multi_role" => Role::MultiRole,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "success": false,
                    "error": "invalid_role",
                    "message": format!("unknown role {other:?}"),
                })),
            );
        }
    };
    let roles_csv = match role {
        Role::ProjectAdmin => "project_admin",
        Role::NodeAdmin => "node_admin",
        Role::MultiRole => "project_admin,node_admin",
    };
    let signer_hex = verify_founder_signature(
        &state.admin_tokens,
        &email_lc,
        roles_csv,
        body.timestamp,
        &body.signature,
        &body.signer,
    );
    let Some(signer_hex) = signer_hex else {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": "bad_signature",
                "message": "signature did not verify against any configured founder key or timestamp out of window",
            })),
        );
    };
    let evidence = json!({
        "bootstrap": {
            "signer_pubkey": signer_hex,
            "timestamp": body.timestamp,
        }
    });
    mint_and_email(&state, email, &email_lc, role, "bootstrap", evidence).await
}

#[derive(Deserialize)]
pub struct RevokeBody {
    /// Full raw token — same as the one presented in `X-Admin-Token`
    /// on other endpoints.  Never travels over any of the browser-
    /// exposed audit endpoints; sent only to `/revoke`.
    token: String,
    #[serde(default)]
    reason: String,
}

/// `POST /api/v1/admin-tokens/revoke` — invalidate a token.  Auth
/// requires the caller to present a `project_admin` (or MultiRole)
/// token in `X-Admin-Token`.  A holder revoking their own token can
/// pass their token in both fields.
pub async fn revoke_token(
    State(state): State<Arc<crate::AppState>>,
    headers: HeaderMap,
    Json(body): Json<RevokeBody>,
) -> (StatusCode, Json<Value>) {
    if let Err(rejection) = require_role(&state.admin_tokens, &headers, Role::ProjectAdmin).await {
        return rejection;
    }
    let raw = body.token.trim();
    if raw.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "success": false, "error": "token_required" })),
        );
    }
    let hash = hash_token(raw);
    let reason = if body.reason.trim().is_empty() {
        "admin_request".to_string()
    } else {
        body.reason.trim().to_string()
    };
    let now = chrono::Utc::now().timestamp();
    let entry = LogEntry::Revoked {
        token_sha256: hash.clone(),
        revoked_at: now,
        reason: reason.clone(),
    };
    if let Err(e) = state.admin_tokens.append(entry).await {
        tracing::error!("admin-token revoke: JSONL append failed: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "persist_failed" })),
        );
    }
    anchor_revoked(&state, &hash, &reason).await;
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "revoked": true,
            "token_sha256_prefix": hash.chars().take(12).collect::<String>(),
        })),
    )
}

/// `GET /api/v1/admin-tokens` — super-admin audit list.  Returns
/// active records only, with token hashes truncated for readability.
/// Requires a `project_admin` (or MultiRole) token in `X-Admin-Token`.
pub async fn list_tokens(
    State(state): State<Arc<crate::AppState>>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if let Err(rejection) = require_role(&state.admin_tokens, &headers, Role::ProjectAdmin).await {
        return rejection;
    }
    let records = state.admin_tokens.active_records().await;
    let items: Vec<Value> = records
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "role": r.role,
                "email_lc": r.email_lc,
                "issued_at": r.issued_at,
                "expires_at": r.expires_at,
                "source": r.source,
                "token_sha256_prefix": r.token_sha256.chars().take(12).collect::<String>(),
                "eligibility": r.eligibility,
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "count": items.len(),
            "records": items,
        })),
    )
}

// ---------------------------------------------------------------------------
// Shared mint pipeline.  Both `request_token` and `bootstrap_token`
// funnel here so persistence, anchoring, and email dispatch stay in
// one place.
// ---------------------------------------------------------------------------

async fn mint_and_email(
    state: &Arc<crate::AppState>,
    email_original: &str,
    email_lc: &str,
    role: Role,
    source: &'static str,
    eligibility_evidence: Value,
) -> (StatusCode, Json<Value>) {
    // Revoke any active record for the same email (rotation).  We
    // want at most one active token per email so the audit surface
    // stays small; the caller can always request another after the
    // previous is revoked, but we do it eagerly here.
    let now = chrono::Utc::now().timestamp();
    let mut previous_hash: Option<String> = None;
    for existing in state.admin_tokens.active_records().await {
        if existing.email_lc == email_lc {
            previous_hash = Some(existing.token_sha256.clone());
            let revoke_entry = LogEntry::Revoked {
                token_sha256: existing.token_sha256.clone(),
                revoked_at: now,
                reason: "rotated".to_string(),
            };
            if let Err(e) = state.admin_tokens.append(revoke_entry).await {
                tracing::error!("admin-token mint: revoke of previous {} failed: {}", existing.id, e);
            }
            anchor_revoked(state, &existing.token_sha256, "rotated").await;
        }
    }

    let (raw_token, token_hash) = mint_token(role);
    let issued_at = chrono::Utc::now().timestamp();
    let expires_at = issued_at + ttl_secs();
    let id = format!(
        "at-{}-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S"),
        &token_hash[..8]
    );

    let record = TokenRecord {
        id: id.clone(),
        token_sha256: token_hash.clone(),
        role,
        email: email_original.to_string(),
        email_lc: email_lc.to_string(),
        previous_token_sha256: previous_hash,
        issued_at,
        expires_at,
        revoked_at: None,
        revoke_reason: None,
        source: source.to_string(),
        eligibility: eligibility_evidence,
    };

    if let Err(e) = state
        .admin_tokens
        .append(LogEntry::Issued(record.clone()))
        .await
    {
        tracing::error!("admin-token mint: JSONL append failed: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "error": "persist_failed" })),
        );
    }
    anchor_issued(state, &record).await;

    // Email the raw token to the recipient.  Fire-and-forget so a
    // SendGrid outage doesn't fail the HTTP response — the operator
    // can always revoke and re-request if the mail was lost.
    let subject = format!("Your Datachain Rope admin token ({:?})", role);
    let text = format!(
        "Hello,\n\n\
         A new Datachain Rope admin token has been issued for {email}.\n\n\
         Role       : {role:?}\n\
         Issued     : {issued}\n\
         Expires    : {expires} (auto-renewed if you use it before then)\n\
         Record id  : {id}\n\n\
         Token (present this as X-Admin-Token on protected endpoints):\n\n    {token}\n\n\
         Keep it private. If you did not request this token, revoke it \
         immediately by contacting the Datachain Foundation at \
         contact@onguene.com.\n\n\
         Any previously-issued token for {email} has been revoked by this \
         rotation.\n\n\
         - Datachain Rope Foundation\n",
        email = email_original,
        role = role,
        issued = chrono::DateTime::from_timestamp(issued_at, 0)
            .map(|d| d.to_rfc3339())
            .unwrap_or_else(|| issued_at.to_string()),
        expires = chrono::DateTime::from_timestamp(expires_at, 0)
            .map(|d| d.to_rfc3339())
            .unwrap_or_else(|| expires_at.to_string()),
        id = id,
        token = raw_token,
    );
    state
        .mailer
        .send_background(email_original.to_string(), subject, text);

    (
        StatusCode::CREATED,
        Json(json!({
            "success": true,
            "id": id,
            "role": role,
            "email": email_original,
            "expires_at": expires_at,
            "message":
                "Token dispatched by email. Present it as X-Admin-Token on protected endpoints.",
        })),
    )
}

// ---------------------------------------------------------------------------
// Background auto-renewal loop.
// ---------------------------------------------------------------------------

/// Spawn a tokio task that ticks every `ADMIN_TOKENS_RENEWAL_TICK_SECS`
/// (default 3600 s), scans active tokens, and rotates any within
/// `ADMIN_TOKEN_RENEWAL_WINDOW_SECS` of expiry.  Each rotation
/// preserves the original role and evidence — the requester does not
/// re-prove eligibility every seven days.
pub fn spawn_renewal_loop(state: Arc<crate::AppState>) {
    let tick = std::env::var("ADMIN_TOKENS_RENEWAL_TICK_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n >= 60)
        .unwrap_or(3600);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(tick));
        // Skip the first immediate fire; wait one tick so a boot
        // storm doesn't race the initial JSONL load.
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(e) = renew_due_tokens(&state).await {
                tracing::warn!("admin-token renewal loop: {}", e);
            }
        }
    });
}

async fn renew_due_tokens(state: &Arc<crate::AppState>) -> anyhow::Result<()> {
    let window = renewal_window_secs();
    let now = chrono::Utc::now().timestamp();
    let due: Vec<TokenRecord> = state
        .admin_tokens
        .active_records()
        .await
        .into_iter()
        .filter(|r| r.expires_at - now <= window && r.expires_at > now)
        .collect();
    for record in due {
        tracing::info!(
            "admin-token auto-renewal: rotating {} ({:?}) — {} s to expiry",
            record.id,
            record.role,
            record.expires_at - now
        );
        let evidence = json!({
            "auto_renewal_of": record.id,
            "previous_source": record.source,
        });
        let (status, _body) = mint_and_email(
            state,
            &record.email,
            &record.email_lc,
            record.role,
            "auto_renewal",
            evidence,
        )
        .await;
        if status.is_success() {
            tracing::info!(
                "admin-token auto-renewal: {} rotated successfully",
                record.email_lc
            );
        } else {
            tracing::warn!(
                "admin-token auto-renewal: {} returned {}",
                record.email_lc,
                status
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests.  Cover the invariants that would silently break admin auth
// if we regressed them.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store(path: PathBuf) -> AdminTokenStore {
        AdminTokenStore::load(path, Vec::new()).expect("store must load")
    }

    #[test]
    fn role_prefixes_are_unique() {
        assert_ne!(Role::ProjectAdmin.prefix(), Role::NodeAdmin.prefix());
        assert_ne!(Role::ProjectAdmin.prefix(), Role::MultiRole.prefix());
        assert_ne!(Role::NodeAdmin.prefix(), Role::MultiRole.prefix());
    }

    #[test]
    fn multi_role_grants_both() {
        assert!(Role::MultiRole.grants(Role::ProjectAdmin));
        assert!(Role::MultiRole.grants(Role::NodeAdmin));
        assert!(Role::MultiRole.grants(Role::MultiRole));
        assert!(!Role::ProjectAdmin.grants(Role::NodeAdmin));
        assert!(!Role::NodeAdmin.grants(Role::ProjectAdmin));
    }

    #[test]
    fn hash_token_is_deterministic_and_hex() {
        let a = hash_token("dcrope_pa_abc");
        let b = hash_token("dcrope_pa_abc");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn mint_tokens_are_unique_and_prefixed() {
        let (a, _) = mint_token(Role::ProjectAdmin);
        let (b, _) = mint_token(Role::ProjectAdmin);
        assert_ne!(a, b);
        assert!(a.starts_with("dcrope_pa_"));
        assert!(b.starts_with("dcrope_pa_"));
        assert_eq!(a.len(), "dcrope_pa_".len() + 43);
    }

    #[test]
    fn load_founder_keys_accepts_nested_and_flat_shapes() {
        use std::io::Write;
        // Canonical nested shape (matches `deploy/config/master-nodes.toml`
        // and `rope-node`'s `governance.rs::FounderAuthority`).
        let hex_key = "0e6aa71f8e8161ec7448eca9b04f2e2205b4ef8783810f66cc5c94e4292a77ef";
        let nested_toml = format!(
            "[founder]\nname = \"Test\"\nfounder_keys = [\"{hex_key}\"]\n"
        );
        let tmp = tempfile::NamedTempFile::new().expect("tmpfile");
        write!(tmp.as_file(), "{}", nested_toml).unwrap();
        let keys = load_founder_keys(tmp.path());
        assert_eq!(keys.len(), 1, "nested [founder].founder_keys must parse");
        // Flat top-level shape (backward-compat).
        let flat_toml = format!("founder_keys = [\"{hex_key}\"]\n");
        let tmp2 = tempfile::NamedTempFile::new().expect("tmpfile");
        write!(tmp2.as_file(), "{}", flat_toml).unwrap();
        let keys2 = load_founder_keys(tmp2.path());
        assert_eq!(keys2.len(), 1, "flat top-level founder_keys must parse");
    }

    #[test]
    fn project_admin_domains_include_all_required() {
        let d = project_admin_domains();
        for required in [
            "onguene.com",
            "onguene.org",
            "datachain.one",
            "datachain.network",
            "databox.network",
            "xn--databx-yta.com",
        ] {
            assert!(
                d.iter().any(|x| x == required),
                "domain allowlist missing {required}"
            );
        }
    }

    #[test]
    fn ct_eq_smoke() {
        assert!(ct_eq("abcd", "abcd"));
        assert!(!ct_eq("abcd", "abce"));
        assert!(!ct_eq("abcd", "abc"));
    }

    #[test]
    fn looks_like_email_smoke() {
        assert!(looks_like_email("kaze@onguene.com"));
        assert!(!looks_like_email("nope"));
        assert!(!looks_like_email("nope@nowhere"));
        assert!(!looks_like_email(""));
        assert!(!looks_like_email("@onguene.com"));
    }

    #[test]
    fn record_is_active_respects_ttl_and_revoke() {
        let now = 1_000_000;
        let mut r = TokenRecord {
            id: "t".into(),
            token_sha256: "0".repeat(64),
            role: Role::ProjectAdmin,
            email: "a@b.com".into(),
            email_lc: "a@b.com".into(),
            previous_token_sha256: None,
            issued_at: now - 10,
            expires_at: now + 100,
            revoked_at: None,
            revoke_reason: None,
            source: "test".into(),
            eligibility: Value::Null,
        };
        assert!(r.is_active(now));
        r.expires_at = now - 1;
        assert!(!r.is_active(now));
        r.expires_at = now + 100;
        r.revoked_at = Some(now - 5);
        assert!(!r.is_active(now));
    }

    #[test]
    fn store_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin-tokens.jsonl");
        let now = chrono::Utc::now().timestamp();
        let rec = TokenRecord {
            id: "at-x".into(),
            token_sha256: hash_token("dcrope_pa_test"),
            role: Role::ProjectAdmin,
            email: "kaze@onguene.com".into(),
            email_lc: "kaze@onguene.com".into(),
            previous_token_sha256: None,
            issued_at: now,
            expires_at: now + 3600,
            revoked_at: None,
            revoke_reason: None,
            source: "test".into(),
            eligibility: json!({"project_admin_domain": "onguene.com"}),
        };
        std::fs::write(&path, format!("{}\n", serde_json::to_string(&LogEntry::Issued(rec.clone())).unwrap())).unwrap();
        let store = make_store(path);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert_eq!(
                store.verify_and_role("dcrope_pa_test").await,
                Some(Role::ProjectAdmin)
            );
            assert!(store.has_active_for_email("kaze@onguene.com").await);
            let recs = store.active_records().await;
            assert_eq!(recs.len(), 1);
            assert_eq!(recs[0].id, "at-x");
        });
    }

    #[test]
    fn store_revocation_disables_verify() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin-tokens.jsonl");
        let now = chrono::Utc::now().timestamp();
        let hash = hash_token("dcrope_pa_revoked");
        let rec = TokenRecord {
            id: "at-r".into(),
            token_sha256: hash.clone(),
            role: Role::ProjectAdmin,
            email: "a@onguene.com".into(),
            email_lc: "a@onguene.com".into(),
            previous_token_sha256: None,
            issued_at: now,
            expires_at: now + 3600,
            revoked_at: None,
            revoke_reason: None,
            source: "test".into(),
            eligibility: Value::Null,
        };
        let issued = format!("{}\n", serde_json::to_string(&LogEntry::Issued(rec)).unwrap());
        let revoked = format!(
            "{}\n",
            serde_json::to_string(&LogEntry::Revoked {
                token_sha256: hash.clone(),
                revoked_at: now + 10,
                reason: "test".into(),
            })
            .unwrap()
        );
        std::fs::write(&path, format!("{issued}{revoked}")).unwrap();
        let store = make_store(path);
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert_eq!(store.verify_and_role("dcrope_pa_revoked").await, None);
        });
    }

    #[test]
    fn compute_eligibility_domain_hit_grants_both_roles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin-tokens.jsonl");
        let store = make_store(path);
        let s = compute_eligibility(&store, "kaze@onguene.com");
        assert_eq!(s.best_role(), Some(Role::MultiRole));
        assert!(s.roles.contains(&Role::ProjectAdmin));
        assert!(s.roles.contains(&Role::NodeAdmin));
    }

    #[test]
    fn compute_eligibility_rejects_unknown_domain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin-tokens.jsonl");
        let store = make_store(path);
        // Point the JSONL discovery at empty temp paths so unrelated
        // production files can't leak into the test result.
        std::env::set_var("NODE_REQUESTS_PATH", dir.path().join("nr.jsonl"));
        std::env::set_var("PROJECTS_PATH", dir.path().join("proj.jsonl"));
        let s = compute_eligibility(&store, "randy@example.com");
        assert_eq!(s.best_role(), None);
        assert!(s.roles.is_empty());
    }

    #[test]
    fn compute_eligibility_node_admin_from_node_request_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("admin-tokens.jsonl");
        let store = make_store(path);
        let nr = dir.path().join("nr.jsonl");
        let proj = dir.path().join("proj.jsonl");
        std::fs::write(
            &nr,
            r#"{"id":"nr-1","email":"declared@example.com","status":"pending"}
"#,
        )
        .unwrap();
        std::env::set_var("NODE_REQUESTS_PATH", &nr);
        std::env::set_var("PROJECTS_PATH", &proj);
        let s = compute_eligibility(&store, "declared@example.com");
        assert_eq!(s.best_role(), Some(Role::NodeAdmin));
        assert!(!s.roles.contains(&Role::ProjectAdmin));
    }

    #[test]
    fn bootstrap_message_is_stable() {
        assert_eq!(
            bootstrap_message("kaze@onguene.com", "project_admin,node_admin", 1_700_000_000),
            "DCROPE-ADMIN-TOKEN-BOOTSTRAP\nkaze@onguene.com\nproject_admin,node_admin\n1700000000"
        );
    }
}
