//! `rope-shadow-witness` daemon entry point.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};

use rope_shadow_witness::chain::ShadowChain;
use rope_shadow_witness::client::RpcClient;
use rope_shadow_witness::config::ShadowWitnessConfig;
use rope_shadow_witness::observer::Observer;
use rope_shadow_witness::server::Server;
use rope_shadow_witness::store::ShadowChainStore;

#[derive(Parser, Debug)]
#[command(name = "rope-shadow-witness", version)]
struct Cli {
    /// Path to the TOML config file.
    #[arg(short, long, default_value = "/etc/rope-shadow-witness/config.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).with_target(true).init();

    let cli = Cli::parse();
    let config = ShadowWitnessConfig::from_path(&cli.config)?;
    info!(
        config_path = %cli.config.display(),
        upstream = %config.upstream_rpc_url,
        bind = %config.bind_addr,
        data_dir = %config.data_dir.display(),
        witness_tag = %config.witness_tag,
        "shadow witness: loaded config"
    );

    std::fs::create_dir_all(&config.data_dir)?;

    // Stable "first-install" timestamp for the soak gate.
    //
    // The gate measures the soak window as `now - first_install_at_unix`.
    // We write `data_dir/.first-install-at-unix` once on initial bring-up
    // and never overwrite it, so the value survives binary refresh,
    // process restart, and clock skew. The store-derived
    // `first_observed_at_unix` (RocksDB heads minimum) drifts forward when
    // every head has been refreshed with a new knot; this file does not.
    let install_marker = config.data_dir.join(".first-install-at-unix");
    if !install_marker.exists() {
        let now = chrono::Utc::now().timestamp();
        std::fs::write(&install_marker, now.to_string())?;
        info!(install_marker = %install_marker.display(), unix_ts = now,
              "shadow witness: created first-install marker");
    }

    let store = Arc::new(ShadowChainStore::open(&config.data_dir)?);
    let chain = Arc::new(ShadowChain::new(store));
    let client = Arc::new(RpcClient::new(&config.upstream_rpc_url)?);

    let observer = Arc::new(Observer::new(client, chain.clone(), config.clone()));
    let server = Arc::new(Server::new(chain, config));

    let observer_task = tokio::spawn({
        let me = observer.clone();
        async move { me.run().await }
    });
    let server_task = tokio::spawn({
        let me = server.clone();
        async move {
            if let Err(e) = me.serve().await {
                error!(error = %e, "shadow witness: rpc server exited");
            }
        }
    });

    tokio::select! {
        _ = observer_task => error!("shadow witness: observer task exited"),
        _ = server_task => error!("shadow witness: server task exited"),
        _ = tokio::signal::ctrl_c() => info!("shadow witness: ctrl-c received, shutting down"),
    }

    Ok(())
}
