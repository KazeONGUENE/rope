//! HTTP surface of the Datachain ID gateway.
//!
//! | Method | Path                       | Purpose                                     |
//! |--------|----------------------------|---------------------------------------------|
//! | GET    | `/`                        | Service descriptor                          |
//! | GET    | `/healthz`                 | Liveness                                    |
//! | GET    | `/.well-known/jwks.json`   | Ed25519 verification key (JWKS)             |
//! | POST   | `/v1/auth/login`           | Datawallet+ email + password → token        |
//! | POST   | `/v1/auth/wallet`          | EIP-191 wallet signature → token            |
//! | GET    | `/v1/auth/userinfo`        | Bearer token → claims                       |
//! | POST   | `/v1/auth/introspect`      | Server-side token introspection             |

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::config::Config;
use crate::identity;
use crate::jwt::{build_claims, TokenSigner};
use crate::rate::RateLimiter;
use crate::supabase::{SupabaseClient, SupabaseError};
use crate::walletsig;

pub struct AppState {
    pub config: Config,
    pub signer: TokenSigner,
    pub supabase: SupabaseClient,
    /// Login attempts per source IP.
    pub login_ip_limiter: RateLimiter,
    /// Login attempts per target email (credential-stuffing guard).
    pub login_email_limiter: RateLimiter,
    /// Wallet-signature attempts per source IP.
    pub wallet_ip_limiter: RateLimiter,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(descriptor))
        .route("/healthz", get(healthz))
        .route("/.well-known/jwks.json", get(jwks))
        .route("/v1/auth/login", post(login))
        .route("/v1/auth/wallet", post(wallet_login))
        .route("/v1/auth/userinfo", get(userinfo))
        .route("/v1/auth/introspect", post(introspect))
        .layer(middleware::from_fn(cors))
        .with_state(state)
}

/// Permissive CORS: the gateway is a public identity endpoint consumed
/// by browser apps across the whole ecosystem (dcscan.io, dcswap.net,
/// tanastok.io, …). Tokens are bearer credentials returned in the
/// response body — no cookies — so `*` is safe here.
async fn cors(req: axum::extract::Request, next: Next) -> Response {
    let is_preflight = req.method() == Method::OPTIONS;
    let mut response = if is_preflight {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(req).await
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type, authorization"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );
    response
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Client IP for rate limiting: first `X-Forwarded-For` hop (set by
/// nginx) or the socket peer.
fn client_ip(headers: &HeaderMap, peer: &SocketAddr) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| peer.ip().to_string())
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

async fn descriptor(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(json!({
        "service": "Datachain ID",
        "description": "Ecosystem identity gateway for Datachain Rope (chainId 271828). Verifies Datawallet+ credentials and EIP-191 wallet signatures, mints Ed25519-signed ecosystem tokens verifiable via JWKS.",
        "issuer": state.config.issuer,
        "audience": state.config.audience,
        "chain_id": crate::config::ROPE_CHAIN_ID,
        "jwks_uri": format!("{}/.well-known/jwks.json", state.config.issuer),
        "kid": state.signer.kid(),
        "endpoints": {
            "login":      { "method": "POST", "path": "/v1/auth/login",      "body": { "email": "string", "password": "string" } },
            "wallet":     { "method": "POST", "path": "/v1/auth/wallet",     "body": { "address": "0x…", "timestamp": "unix seconds", "signature": "0x… (65-byte personal_sign)" }, "message_format": "DATACHAIN-ID-AUTH\\n{address_lowercase}\\n{timestamp}" },
            "userinfo":   { "method": "GET",  "path": "/v1/auth/userinfo",   "auth": "Bearer <token>" },
            "introspect": { "method": "POST", "path": "/v1/auth/introspect", "body": { "token": "string" } }
        }
    }))
}

async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn jwks(State(state): State<Arc<AppState>>) -> Response {
    let mut response = Json(state.signer.jwks()).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    response
}

#[derive(Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

async fn login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    let email = body.email.trim().to_lowercase();
    if email.is_empty() || body.password.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "missing_fields",
            "email and password are required",
        );
    }

    let ts = now();
    let ip = client_ip(&headers, &peer);
    if !state.login_ip_limiter.allow(&format!("ip:{ip}"), ts)
        || !state.login_email_limiter.allow(&format!("email:{email}"), ts)
    {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many attempts; retry later",
        );
    }

    let grant = match state.supabase.verify_password(&email, &body.password).await {
        Ok(g) => g,
        Err(SupabaseError::InvalidCredentials) => {
            tracing::info!(ip, "login rejected (invalid credentials)");
            return error_response(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "email or password is incorrect",
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "gotrue verification failed");
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "identity backend unavailable",
            );
        }
    };

    issue_token(&state, &grant.user, vec!["pwd".into()], ts).await
}

#[derive(Deserialize)]
struct WalletLoginRequest {
    address: String,
    timestamp: i64,
    signature: String,
}

async fn wallet_login(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<WalletLoginRequest>,
) -> Response {
    let ts = now();
    let ip = client_ip(&headers, &peer);
    if !state.wallet_ip_limiter.allow(&format!("ip:{ip}"), ts) {
        return error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many attempts; retry later",
        );
    }

    let proven = match walletsig::verify_wallet_auth(&body.address, body.timestamp, &body.signature, ts)
    {
        Ok(addr) => addr,
        Err(reason) => {
            tracing::info!(ip, reason, "wallet login rejected");
            return error_response(StatusCode::UNAUTHORIZED, "invalid_signature", &reason);
        }
    };

    // The proven key must belong to a registered Datawallet+ wallet.
    let wallet_row = match state.supabase.wallet_by_address(&proven).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "wallet_not_registered",
                "this wallet is not linked to any Datawallet+ account",
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "wallet lookup failed");
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "identity backend unavailable",
            );
        }
    };

    // wallets.user_id → tanastok_users.id → auth_user_id → GoTrue user.
    let app_user_id = match wallet_row.user_id {
        Some(id) => id,
        None => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "wallet_not_registered",
                "this wallet has no owning account",
            );
        }
    };
    let auth_user_id = match state.supabase.app_user_by_id(&app_user_id).await {
        Ok(Some(app)) => match app.auth_user_id {
            Some(id) => id,
            None => {
                return error_response(
                    StatusCode::UNAUTHORIZED,
                    "wallet_not_registered",
                    "wallet owner has no auth account",
                )
            }
        },
        Ok(None) => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "wallet_not_registered",
                "wallet owner not found",
            )
        }
        Err(e) => {
            tracing::error!(error = %e, "app user lookup failed");
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "identity backend unavailable",
            );
        }
    };

    let user = match state.supabase.admin_user(&auth_user_id).await {
        Ok(u) => u,
        Err(e) => {
            tracing::error!(error = %e, "admin user lookup failed");
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "identity backend unavailable",
            );
        }
    };

    issue_token(&state, &user, vec!["wallet_signature".into()], ts).await
}

/// Shared tail of both login paths: resolve identity, mint the token,
/// shape the response.
async fn issue_token(
    state: &Arc<AppState>,
    user: &crate::supabase::GoTrueUser,
    amr: Vec<String>,
    ts: i64,
) -> Response {
    let identity = match identity::resolve(&state.supabase, user).await {
        Ok(i) => i,
        Err(e) => {
            tracing::error!(error = %e, "identity resolution failed");
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "identity backend unavailable",
            );
        }
    };

    let claims = build_claims(
        &state.config.issuer,
        &state.config.audience,
        state.config.token_ttl_secs,
        ts,
        identity.sub.clone(),
        identity.email.clone(),
        identity.name.clone(),
        identity.did.clone(),
        identity.primary_address.clone(),
        identity.wallets.clone(),
        identity.public_key.clone(),
        amr,
    );
    let token = state.signer.sign(&claims);

    tracing::info!(sub = %identity.sub, amr = ?claims.amr, "token issued");

    (
        StatusCode::OK,
        Json(json!({
            "token": token,
            "token_type": "Bearer",
            "expires_in": state.config.token_ttl_secs,
            "user": {
                "id": identity.sub,
                "email": identity.email,
                "name": identity.name,
                "did": identity.did,
                "primary_address": identity.primary_address,
                "wallets": identity.wallets,
                "public_key": identity.public_key,
                "chain_id": crate::config::ROPE_CHAIN_ID,
            }
        })),
    )
        .into_response()
}

async fn userinfo(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or("");
    if token.is_empty() {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "missing_token",
            "Authorization: Bearer <token> required",
        );
    }
    match state.signer.verify(token, now()) {
        Ok(claims) => (StatusCode::OK, Json(serde_json::to_value(claims).unwrap())).into_response(),
        Err(e) => error_response(StatusCode::UNAUTHORIZED, "invalid_token", &e.to_string()),
    }
}

#[derive(Deserialize)]
struct IntrospectRequest {
    token: String,
}

async fn introspect(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IntrospectRequest>,
) -> Response {
    match state.signer.verify(&body.token, now()) {
        Ok(claims) => {
            let mut value = serde_json::to_value(&claims).unwrap();
            value["active"] = json!(true);
            (StatusCode::OK, Json(value)).into_response()
        }
        Err(_) => (StatusCode::OK, Json(json!({ "active": false }))).into_response(),
    }
}
