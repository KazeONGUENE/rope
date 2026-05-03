//! `rope-deployer` binary — Datachain Foundation cloud provisioning service.
//!
//! Phase D MVP: prints the configured provider matrix and serves an
//! offline dry-run dispatcher. Wiring into `axum` (HTTP) and into the
//! `rope` CLI happens in a follow-up commit; the surface is already
//! defined in [`rope_deployer::api`].

use std::sync::Arc;

use rope_deployer::providers::digitalocean::DigitalOceanProvider;
use rope_deployer::providers::exoscale::ExoscaleProvider;
use rope_deployer::providers::local::LocalProvider;
use rope_deployer::{AppState, ProviderRegistry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let registry = ProviderRegistry::new();
    registry.register(Arc::new(LocalProvider::new()));
    registry.register(Arc::new(ExoscaleProvider::from_env()));
    registry.register(Arc::new(DigitalOceanProvider::from_env()));

    let state = Arc::new(AppState::new(registry));

    let snapshot = state.providers.snapshot();
    tracing::info!("rope-deployer ready");
    for (name, live) in &snapshot {
        tracing::info!(
            target: "rope_deployer",
            provider = name.as_str(),
            live = live,
            "provider registered"
        );
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&rope_deployer::api::providers(&state))?
    );
    println!(
        "rope-deployer MVP ready. HTTP API surface defined in `rope_deployer::api` \
         — see deploy/EXOSCALE_AS_A_SERVICE.md for the live integration plan."
    );

    Ok(())
}
