//! rope-idp binary — Datachain ID, the ecosystem identity gateway.
//!
//! One service, one issuer (`https://id.datachain.network`), consumed by
//! every Datachain Rope platform:
//!
//! * `POST /v1/auth/login`  — Datawallet+ email + password (verified
//!   against the Datawallet+ Supabase GoTrue backend) → ecosystem JWT.
//! * `POST /v1/auth/wallet` — EIP-191 `personal_sign` proof of wallet
//!   key possession (wallet must be registered in Datawallet+) →
//!   ecosystem JWT.
//! * `GET /.well-known/jwks.json` — Ed25519 public key so platforms can
//!   verify tokens fully offline.
//!
//! Tokens carry the user's email, display name, DID, every linked
//! wallet, and the primary on-chain address for chain 271828.

use std::net::SocketAddr;
use std::sync::Arc;

mod config;
mod identity;
mod jwt;
mod rate;
mod routes;
mod supabase;
mod walletsig;

use config::Config;
use jwt::TokenSigner;
use rate::RateLimiter;
use routes::AppState;
use supabase::SupabaseClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rope_idp=info,info".into()),
        )
        .init();

    let config = Config::from_env()?;
    let signer = TokenSigner::load_or_generate(&config.key_file, config.issuer.clone())?;
    tracing::info!(kid = signer.kid(), issuer = %config.issuer, "signing key loaded");

    let supabase = SupabaseClient::new(
        config.supabase_url.clone(),
        config.supabase_anon_key.clone(),
        config.supabase_service_key.clone(),
    );

    let state = Arc::new(AppState {
        // 10 login attempts / 5 min / IP; 5 / 5 min / email;
        // 20 wallet-signature attempts / 5 min / IP.
        login_ip_limiter: RateLimiter::new(300, 10),
        login_email_limiter: RateLimiter::new(300, 5),
        wallet_ip_limiter: RateLimiter::new(300, 20),
        signer,
        supabase,
        config: config.clone(),
    });

    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    tracing::info!(listen = %config.listen, "Datachain ID gateway listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
