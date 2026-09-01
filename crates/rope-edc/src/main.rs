//! rope-edc binary - serves the console UI, the console API, the
//! stakeholder gateway, and the public directory on one port.
//!
//! Environment:
//!
//! | Variable | Default | Purpose |
//! |---|---|---|
//! | `EDC_LISTEN` | `0.0.0.0:9095` | Bind address |
//! | `EDC_DATA_DIR` | `./edc-data` | Store + journals |
//! | `EDC_STATIC` | `./crates/rope-edc/static` | Console UI files |
//! | `EDC_ROPE_RPC` | `http://127.0.0.1:8545` | Loopback rope-node RPC |
//! | `EDC_REGISTRY_WALLET` | `0x…ec01` | Public directory wallet |
//! | `EDC_PUBLIC_URL` | `http://127.0.0.1:9095` | Stakeholder base URL |
//! | `EDC_CONSOLE_TOKEN` | unset | Optional shared console token |
//! | `EDC_CONSOLE_REQUIRE_SIGNATURE` | unset | `1` = console requires EIP-191 sign-in (hosted mode) |
//! | `EDC_SESSION_TTL_SECS` | `86400` | Console session lifetime |
//! | `EDC_TIMELOCK_DELAY_SECS` | `3600` | Regulator/public grant delay |
//! | `EDC_AI_DISABLE` | unset | `1` = deterministic engine only |
//! | `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `EDC_OLLAMA_ENDPOINT` | unset | AI providers |
//! | `EDC_CLOUD_PROVIDER` | auto | `exoscale`/`digitalocean`/`local` node provisioning target |
//! | `EXOSCALE_API_KEY`+`EXOSCALE_API_SECRET` / `DIGITALOCEAN_TOKEN` | unset | Cloud credentials |
//! | `EDC_SSH_PUBKEY` | unset | SSH key authorised on provisioned nodes |
//! | `EDC_SIM_TICK_SECS` | `60` | Simulation-project synthetic tick interval |

use std::sync::Arc;

use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
    services::ServeDir,
};

use rope_edc::ai::AiAnalytics;
use rope_edc::api::{self, AppState};
use rope_edc::registry::Registry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let data_dir =
        std::env::var("EDC_DATA_DIR").unwrap_or_else(|_| "./edc-data".to_string());
    let static_dir = std::env::var("EDC_STATIC")
        .unwrap_or_else(|_| "./crates/rope-edc/static".to_string());
    let listen = std::env::var("EDC_LISTEN").unwrap_or_else(|_| "0.0.0.0:9095".to_string());

    let registry = Registry::open(&data_dir)?;
    let ai = Arc::new(AiAnalytics::from_env());
    tracing::info!(
        "EDC starting: data_dir={data_dir} static={static_dir} ai_engine={}",
        ai.engine_label()
    );

    // Background schedulers (spec v1.0 §6.3/§6.4):
    // scheduled bulk exports, scheduled reports, simulation ticker.
    tokio::spawn(rope_edc::export::run_export_scheduler(registry.clone()));
    tokio::spawn(rope_edc::reports::run_report_scheduler(registry.clone()));
    tokio::spawn(rope_edc::simulation::run_simulation_ticker(registry.clone()));

    // Build the shared cloud-provider registry once at startup. Live
    // providers (DigitalOcean) read their credentials + state cache
    // path from the environment here; dry-run providers (Exoscale)
    // report `is_live=false` and stash dry-run instances in the same
    // state directory.
    let deployer = rope_edc::provision::default_provider_registry();
    for (provider, live) in deployer.snapshot() {
        tracing::info!("cloud provider: {} (live={})", provider.as_str(), live);
    }

    let state = Arc::new(AppState {
        registry,
        ai,
        deployer,
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = api::router(state)
        .nest_service("/console", ServeDir::new(&static_dir))
        .route(
            "/healthz",
            axum::routing::get(|| async { axum::Json(serde_json::json!({"ok": true})) }),
        )
        .layer(cors)
        .layer(CompressionLayer::new());

    let listener = tokio::net::TcpListener::bind(&listen).await?;
    tracing::info!("EDC listening on {listen} - console at /console/");
    axum::serve(listener, app).await?;
    Ok(())
}
