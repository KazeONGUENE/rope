//! Datawallet+ backend client (Supabase GoTrue + PostgREST).
//!
//! Two privilege levels:
//!
//! * **anon key** — used exclusively for `signInWithPassword`
//!   (`/auth/v1/token?grant_type=password`) so Supabase auth policies
//!   (rate limits, MFA, banned users) stay in effect.
//! * **service-role key** — used for read-only identity enrichment:
//!   wallet rows, DID, profile. The correct linkage chain, verified
//!   against the live schema on 2026-07-07, is:
//!
//!   ```text
//!   auth.users.id  ──►  tanastok_users.auth_user_id
//!   tanastok_users.id  ──►  wallets.user_id
//!   auth.users.id  ──►  profiles.user_id
//!   ```
//!
//!   (The historical `careaway-auth-verify` edge function queried
//!   `wallets.user_id = auth.users.id`, which never matches — this
//!   gateway resolves through `tanastok_users` and therefore actually
//!   returns the user's wallets.)

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone)]
pub struct SupabaseClient {
    http: reqwest::Client,
    base_url: String,
    anon_key: String,
    service_key: String,
}

#[derive(thiserror::Error, Debug)]
pub enum SupabaseError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("upstream request failed: {0}")]
    Transport(String),
    #[error("unexpected upstream response: {0}")]
    Unexpected(String),
}

/// GoTrue user object subset we need.
#[derive(Debug, Deserialize)]
pub struct GoTrueUser {
    pub id: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub user_metadata: Value,
}

/// Result of a successful password verification.
#[derive(Debug)]
pub struct PasswordGrant {
    pub user: GoTrueUser,
}

/// A wallet row from the Datawallet+ `wallets` table.
#[derive(Debug, Deserialize, Clone)]
pub struct WalletRow {
    pub address: String,
    #[serde(rename = "type", default)]
    pub wallet_type: Option<String>,
    #[serde(default)]
    pub is_default: Option<bool>,
    #[serde(default)]
    pub verified: Option<bool>,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// A `tanastok_users` row (the app-level user record).
#[derive(Debug, Deserialize)]
pub struct AppUserRow {
    pub id: String,
    #[serde(default)]
    pub auth_user_id: Option<String>,
    #[serde(default)]
    pub did: Option<String>,
}

/// A `profiles` row subset.
#[derive(Debug, Deserialize, Default)]
pub struct ProfileRow {
    #[serde(default)]
    pub did: Option<String>,
    #[serde(default)]
    pub public_key: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
}

impl SupabaseClient {
    pub fn new(base_url: String, anon_key: String, service_key: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base_url,
            anon_key,
            service_key,
        }
    }

    /// Verify email + password against GoTrue. Any auth failure maps to
    /// `InvalidCredentials` so callers never leak whether the email
    /// exists.
    pub async fn verify_password(
        &self,
        email: &str,
        password: &str,
    ) -> Result<PasswordGrant, SupabaseError> {
        let url = format!("{}/auth/v1/token?grant_type=password", self.base_url);
        let resp = self
            .http
            .post(&url)
            .header("apikey", &self.anon_key)
            .json(&serde_json::json!({ "email": email, "password": password }))
            .send()
            .await
            .map_err(|e| SupabaseError::Transport(e.to_string()))?;

        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| SupabaseError::Unexpected(e.to_string()))?;

        if !status.is_success() {
            // 400/401/422 from GoTrue = bad credentials (or banned /
            // unconfirmed user). Log the class server-side, return a
            // generic error to the caller.
            tracing::debug!(status = %status, code = %body["error_code"], "gotrue rejection");
            return Err(SupabaseError::InvalidCredentials);
        }

        let user: GoTrueUser = serde_json::from_value(body["user"].clone())
            .map_err(|e| SupabaseError::Unexpected(format!("user object: {e}")))?;
        Ok(PasswordGrant { user })
    }

    async fn rest_get(&self, path_and_query: &str) -> Result<Value, SupabaseError> {
        let url = format!("{}/rest/v1/{}", self.base_url, path_and_query);
        let resp = self
            .http
            .get(&url)
            .header("apikey", &self.service_key)
            .header("Authorization", format!("Bearer {}", self.service_key))
            .send()
            .await
            .map_err(|e| SupabaseError::Transport(e.to_string()))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| SupabaseError::Unexpected(e.to_string()))?;
        if !status.is_success() {
            return Err(SupabaseError::Unexpected(format!(
                "PostgREST {status}: {body}"
            )));
        }
        Ok(body)
    }

    /// `tanastok_users` row for a GoTrue auth user id.
    pub async fn app_user_by_auth_id(
        &self,
        auth_user_id: &str,
    ) -> Result<Option<AppUserRow>, SupabaseError> {
        let body = self
            .rest_get(&format!(
                "tanastok_users?auth_user_id=eq.{auth_user_id}&select=id,auth_user_id,did&limit=1"
            ))
            .await?;
        Ok(parse_first(body))
    }

    /// `tanastok_users` row by primary key (used for reverse wallet lookup).
    pub async fn app_user_by_id(&self, id: &str) -> Result<Option<AppUserRow>, SupabaseError> {
        let body = self
            .rest_get(&format!(
                "tanastok_users?id=eq.{id}&select=id,auth_user_id,did&limit=1"
            ))
            .await?;
        Ok(parse_first(body))
    }

    /// Every wallet linked to an app-level user id.
    pub async fn wallets_by_app_user(
        &self,
        app_user_id: &str,
    ) -> Result<Vec<WalletRow>, SupabaseError> {
        let body = self
            .rest_get(&format!(
                "wallets?user_id=eq.{app_user_id}&select=address,type,is_default,verified,user_id&order=created_at.asc"
            ))
            .await?;
        serde_json::from_value(body).map_err(|e| SupabaseError::Unexpected(e.to_string()))
    }

    /// Reverse lookup: find the wallet row owning `address`
    /// (case-insensitive).
    pub async fn wallet_by_address(
        &self,
        address: &str,
    ) -> Result<Option<WalletRow>, SupabaseError> {
        // Addresses are plain hex, safe to interpolate into the query.
        let addr = address
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == 'x')
            .collect::<String>();
        let body = self
            .rest_get(&format!(
                "wallets?address=ilike.{addr}&select=address,type,is_default,verified,user_id&limit=1"
            ))
            .await?;
        Ok(parse_first(body))
    }

    /// `profiles` row for a GoTrue auth user id.
    pub async fn profile_by_auth_id(
        &self,
        auth_user_id: &str,
    ) -> Result<Option<ProfileRow>, SupabaseError> {
        let body = self
            .rest_get(&format!(
                "profiles?user_id=eq.{auth_user_id}&select=did,public_key,display_name,username&limit=1"
            ))
            .await?;
        Ok(parse_first(body))
    }

    /// Admin lookup of a GoTrue user (wallet-signature login path,
    /// where no password grant happened).
    pub async fn admin_user(&self, auth_user_id: &str) -> Result<GoTrueUser, SupabaseError> {
        let url = format!("{}/auth/v1/admin/users/{auth_user_id}", self.base_url);
        let resp = self
            .http
            .get(&url)
            .header("apikey", &self.service_key)
            .header("Authorization", format!("Bearer {}", self.service_key))
            .send()
            .await
            .map_err(|e| SupabaseError::Transport(e.to_string()))?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| SupabaseError::Unexpected(e.to_string()))?;
        if !status.is_success() {
            return Err(SupabaseError::Unexpected(format!(
                "admin user lookup {status}"
            )));
        }
        serde_json::from_value(body).map_err(|e| SupabaseError::Unexpected(e.to_string()))
    }
}

fn parse_first<T: serde::de::DeserializeOwned>(body: Value) -> Option<T> {
    body.as_array()
        .and_then(|rows| rows.first().cloned())
        .and_then(|row| serde_json::from_value(row).ok())
}
