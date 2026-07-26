//! DCScan API keys — self-service issuance for authenticated users.
//!
//! Any user authenticated through **Datachain ID** (`id.datachain.network`,
//! Datawallet+ credentials or EIP-191 wallet signature) can mint API keys
//! for the DCScan REST API. Management endpoints require a
//! `Authorization: Bearer <Datachain ID token>` header; the token is
//! verified server-side against the gateway's introspection endpoint
//! (`POST /v1/auth/introspect`) so DCScan holds no signing secrets.
//!
//! | Method | Path                  | Auth                | Purpose                          |
//! |--------|-----------------------|---------------------|----------------------------------|
//! | POST   | `/api/v1/keys`        | Bearer ID token     | Mint a key (returned once)       |
//! | GET    | `/api/v1/keys`        | Bearer ID token     | List caller's keys (masked)      |
//! | DELETE | `/api/v1/keys/:id`    | Bearer ID token     | Revoke one of the caller's keys  |
//! | GET    | `/api/v1/keys/verify` | `X-API-Key` header  | Validate a key + usage counters  |
//!
//! Keys look like `dcsk_<64 hex chars>`. Only the BLAKE3 hash of the full
//! key is stored; the plaintext is shown exactly once at mint time. Every
//! `/api/*` request that carries a valid `X-API-Key` header is counted
//! against the key (see [`track_usage`]), which gives keyed consumers an
//! auditable usage trail (`request_count`, `last_used_at`) and gives
//! operations a per-consumer dial for future rate-limit tiers.

use std::collections::HashMap;

use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::RwLock;

/// Maximum number of active (non-revoked) keys per identity.
const MAX_ACTIVE_KEYS_PER_OWNER: usize = 5;

/// How long a successful introspection result is reused (seconds).
const INTROSPECT_CACHE_TTL_SECS: i64 = 60;

/// Datachain ID gateway base URL (override with `DCSCAN_IDP_URL`).
fn idp_url() -> String {
    std::env::var("DCSCAN_IDP_URL").unwrap_or_else(|_| "https://id.datachain.network".to_string())
}

fn store_path() -> std::path::PathBuf {
    std::env::var("DCSCAN_API_KEYS_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/var/lib/dc-explorer/api_keys.json"))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// One issued API key. The plaintext key is never stored — `key_hash`
/// is `blake3(full_key)` in hex.
#[derive(Clone, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    pub id: String,
    pub key_hash: String,
    /// Display form: `dcsk_ab12cd34…` (first 8 chars of the secret).
    pub prefix: String,
    pub label: String,
    /// Datachain ID subject (stable user UUID) — ownership anchor.
    pub owner_sub: String,
    pub owner_email: Option<String>,
    pub owner_name: Option<String>,
    pub owner_address: Option<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub request_count: u64,
    pub revoked: bool,
}

impl ApiKeyRecord {
    fn masked(&self) -> serde_json::Value {
        json!({
            "id": self.id,
            "prefix": self.prefix,
            "label": self.label,
            "created_at": self.created_at,
            "last_used_at": self.last_used_at,
            "request_count": self.request_count,
            "revoked": self.revoked,
        })
    }
}

#[derive(Default, Serialize, Deserialize)]
struct KeyFile {
    keys: Vec<ApiKeyRecord>,
}

/// Verified caller identity, as returned by the Datachain ID gateway.
#[derive(Clone)]
pub struct IdpIdentity {
    pub sub: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub primary_address: Option<String>,
}

/// In-process store: key records (persisted to disk) + a short-lived
/// introspection cache so bursts of management calls don't hammer the
/// identity gateway.
pub struct ApiKeyStore {
    /// key-id → record
    keys: RwLock<HashMap<String, ApiKeyRecord>>,
    /// blake3(bearer token) → (expires_at, identity)
    introspect_cache: RwLock<HashMap<String, (i64, IdpIdentity)>>,
}

impl ApiKeyStore {
    pub fn load() -> Self {
        let path = store_path();
        let keys = match std::fs::read_to_string(&path) {
            Ok(s) => match serde_json::from_str::<KeyFile>(&s) {
                Ok(f) => {
                    tracing::info!(
                        "ApiKeyStore resumed from {}: {} keys",
                        path.display(),
                        f.keys.len()
                    );
                    f.keys.into_iter().map(|k| (k.id.clone(), k)).collect()
                }
                Err(e) => {
                    tracing::warn!(
                        "ApiKeyStore parse failed at {} ({}); starting fresh",
                        path.display(),
                        e
                    );
                    HashMap::new()
                }
            },
            Err(_) => {
                tracing::info!(
                    "ApiKeyStore: no persisted store at {}; starting fresh",
                    path.display()
                );
                HashMap::new()
            }
        };
        Self {
            keys: RwLock::new(keys),
            introspect_cache: RwLock::new(HashMap::new()),
        }
    }

    pub async fn persist(&self) {
        let file = {
            let keys = self.keys.read().await;
            KeyFile {
                keys: keys.values().cloned().collect(),
            }
        };
        let path = store_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("json.tmp");
        match serde_json::to_string_pretty(&file) {
            Ok(s) => {
                if let Err(e) = std::fs::write(&tmp, s).and_then(|_| std::fs::rename(&tmp, &path)) {
                    tracing::error!("ApiKeyStore persist failed at {}: {}", path.display(), e);
                }
            }
            Err(e) => tracing::error!("ApiKeyStore serialize failed: {}", e),
        }
    }

    /// Verify a Datachain ID bearer token via the gateway's introspection
    /// endpoint, with a short local cache keyed on the token hash.
    async fn verify_bearer(
        &self,
        http: &reqwest::Client,
        token: &str,
    ) -> Result<IdpIdentity, (StatusCode, &'static str, String)> {
        let cache_key = blake3::hash(token.as_bytes()).to_hex().to_string();
        let ts = now();
        if let Some((expires, identity)) = self.introspect_cache.read().await.get(&cache_key) {
            if *expires > ts {
                return Ok(identity.clone());
            }
        }

        let url = format!("{}/v1/auth/introspect", idp_url());
        let res = http
            .post(&url)
            .json(&json!({ "token": token }))
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                (
                    StatusCode::BAD_GATEWAY,
                    "idp_unreachable",
                    format!("identity gateway unreachable: {e}"),
                )
            })?;
        let body: serde_json::Value = res.json().await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                "idp_bad_response",
                format!("identity gateway returned malformed response: {e}"),
            )
        })?;

        if body.get("active").and_then(|v| v.as_bool()) != Some(true) {
            return Err((
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "token is expired or invalid; sign in again via id.datachain.network".to_string(),
            ));
        }
        let sub = body
            .get("sub")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if sub.is_empty() {
            return Err((
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "token carries no subject".to_string(),
            ));
        }
        let identity = IdpIdentity {
            sub,
            email: body
                .get("email")
                .and_then(|v| v.as_str())
                .map(String::from),
            name: body.get("name").and_then(|v| v.as_str()).map(String::from),
            primary_address: body
                .get("primary_address")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
        };
        self.introspect_cache
            .write()
            .await
            .insert(cache_key, (ts + INTROSPECT_CACHE_TTL_SECS, identity.clone()));
        Ok(identity)
    }

    /// Look up a presented `X-API-Key` value and, when valid, record the
    /// usage. Returns the key id on success.
    pub async fn touch(&self, presented: &str) -> Option<String> {
        if !presented.starts_with("dcsk_") {
            return None;
        }
        let hash = blake3::hash(presented.as_bytes()).to_hex().to_string();
        let mut keys = self.keys.write().await;
        let record = keys
            .values_mut()
            .find(|k| k.key_hash == hash && !k.revoked)?;
        record.request_count += 1;
        record.last_used_at = Some(now());
        Some(record.id.clone())
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn error_json(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

async fn authenticate(
    state: &crate::AppState,
    headers: &HeaderMap,
) -> Result<IdpIdentity, Response> {
    let token = match bearer_token(headers) {
        Some(t) => t,
        None => {
            return Err(error_json(
                StatusCode::UNAUTHORIZED,
                "missing_token",
                "Authorization: Bearer <Datachain ID token> required — sign in at id.datachain.network (Datawallet+ credentials or wallet signature)",
            ))
        }
    };
    state
        .api_keys
        .verify_bearer(&state.http_client, token)
        .await
        .map_err(|(status, code, msg)| error_json(status, code, &msg))
}

#[derive(Deserialize, Default)]
pub struct CreateKeyRequest {
    #[serde(default)]
    pub label: String,
}

/// POST /api/v1/keys — mint a new API key for the authenticated identity.
pub async fn create_key(
    State(state): State<std::sync::Arc<crate::AppState>>,
    headers: HeaderMap,
    body: Option<Json<CreateKeyRequest>>,
) -> Response {
    let identity = match authenticate(&state, &headers).await {
        Ok(i) => i,
        Err(resp) => return resp,
    };

    let label = body
        .map(|Json(b)| b.label.trim().chars().take(64).collect::<String>())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "default".to_string());

    // 32 bytes of CSPRNG → 64 hex chars of secret.
    let mut secret = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut secret);
    let full_key = format!("dcsk_{}", hex::encode(secret));
    let record = ApiKeyRecord {
        id: uuid::Uuid::new_v4().to_string(),
        key_hash: blake3::hash(full_key.as_bytes()).to_hex().to_string(),
        prefix: format!("dcsk_{}…", &hex::encode(secret)[..8]),
        label,
        owner_sub: identity.sub.clone(),
        owner_email: identity.email.clone(),
        owner_name: identity.name.clone(),
        owner_address: identity.primary_address.clone(),
        created_at: now(),
        last_used_at: None,
        request_count: 0,
        revoked: false,
    };

    {
        let mut keys = state.api_keys.keys.write().await;
        let active = keys
            .values()
            .filter(|k| k.owner_sub == identity.sub && !k.revoked)
            .count();
        if active >= MAX_ACTIVE_KEYS_PER_OWNER {
            return error_json(
                StatusCode::CONFLICT,
                "key_limit_reached",
                &format!(
                    "you already have {MAX_ACTIVE_KEYS_PER_OWNER} active keys — revoke one first"
                ),
            );
        }
        keys.insert(record.id.clone(), record.clone());
    }
    state.api_keys.persist().await;

    tracing::info!(sub = %identity.sub, key_id = %record.id, "API key minted");

    // Security notice to the account owner (fire-and-forget; the key
    // itself is never emailed — only the masked prefix).
    if let Some(email) = identity.email.clone() {
        state.mailer.send_background(
            email,
            "DCScan API key created".to_string(),
            format!(
                "A new DCScan API key was just created on your Datachain ID account.\n\n\
                 Label:   {}\n\
                 Prefix:  {}\n\
                 Created: {}\n\n\
                 Manage your keys at https://dcscan.io/apis or https://datachain.network/docs#api-keys.\n\
                 If you did not create this key, revoke it immediately and change your Datawallet+ password.\n\n\
                 — Datachain Foundation",
                record.label,
                record.prefix,
                chrono::Utc::now().to_rfc3339(),
            ),
        );
    }
    (
        StatusCode::CREATED,
        Json(json!({
            "id": record.id,
            "key": full_key,
            "prefix": record.prefix,
            "label": record.label,
            "created_at": record.created_at,
            "note": "Store this key now — it is shown only once. Present it as an X-API-Key header.",
        })),
    )
        .into_response()
}

/// GET /api/v1/keys — list the caller's keys (masked).
pub async fn list_keys(
    State(state): State<std::sync::Arc<crate::AppState>>,
    headers: HeaderMap,
) -> Response {
    let identity = match authenticate(&state, &headers).await {
        Ok(i) => i,
        Err(resp) => return resp,
    };
    let keys = state.api_keys.keys.read().await;
    let mut mine: Vec<&ApiKeyRecord> = keys
        .values()
        .filter(|k| k.owner_sub == identity.sub)
        .collect();
    mine.sort_by_key(|k| std::cmp::Reverse(k.created_at));
    Json(json!({
        "keys": mine.iter().map(|k| k.masked()).collect::<Vec<_>>(),
        "active": mine.iter().filter(|k| !k.revoked).count(),
        "max_active": MAX_ACTIVE_KEYS_PER_OWNER,
    }))
    .into_response()
}

/// DELETE /api/v1/keys/:id — revoke one of the caller's keys.
pub async fn revoke_key(
    State(state): State<std::sync::Arc<crate::AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let identity = match authenticate(&state, &headers).await {
        Ok(i) => i,
        Err(resp) => return resp,
    };
    let revoked = {
        let mut keys = state.api_keys.keys.write().await;
        match keys.get_mut(&id) {
            Some(k) if k.owner_sub == identity.sub => {
                k.revoked = true;
                true
            }
            _ => false,
        }
    };
    if !revoked {
        return error_json(
            StatusCode::NOT_FOUND,
            "key_not_found",
            "no key with that id belongs to you",
        );
    }
    state.api_keys.persist().await;
    tracing::info!(sub = %identity.sub, key_id = %id, "API key revoked");
    Json(json!({ "revoked": true, "id": id })).into_response()
}

/// GET /api/v1/keys/verify — validate an `X-API-Key` header. Counts as a
/// use (it exercises the exact production path consumers will use).
pub async fn verify_key(
    State(state): State<std::sync::Arc<crate::AppState>>,
    headers: HeaderMap,
) -> Response {
    let presented = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if presented.is_empty() {
        return error_json(
            StatusCode::BAD_REQUEST,
            "missing_key",
            "X-API-Key header required",
        );
    }
    match state.api_keys.touch(presented).await {
        Some(id) => {
            state.api_keys.persist().await;
            let keys = state.api_keys.keys.read().await;
            let k = keys.get(&id).expect("touched key exists");
            Json(json!({ "valid": true, "key": k.masked() })).into_response()
        }
        None => error_json(
            StatusCode::UNAUTHORIZED,
            "invalid_key",
            "unknown or revoked API key",
        ),
    }
}

/// Axum middleware: attribute any `/api/*` request carrying a valid
/// `X-API-Key` header to its key (usage counter + last-used timestamp).
/// Unkeyed requests pass through untouched — the public API stays public.
pub async fn track_usage(
    State(state): State<std::sync::Arc<crate::AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    // /api/v1/keys/verify already counts itself — avoid double counting.
    let is_verify = request.uri().path() == "/api/v1/keys/verify";
    if let Some(key) = presented {
        if !is_verify && request.uri().path().starts_with("/api/") {
            state.api_keys.touch(&key).await;
        }
    }
    next.run(request).await
}
