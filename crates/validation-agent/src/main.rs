//! `validation-agent` binary entry point.
//!
//! Drives [`validation_agent::ValidationAgent`] from a clap-based CLI.
//! Designed to be run on the same host as a rope-node, talking to the
//! local `127.0.0.1:8545` JSON-RPC by default. See `--help` for the
//! full surface.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::{fmt, EnvFilter};

use validation_agent::config::ValidationAgentConfig;
use validation_agent::{ValidationAgent, VALIDATION_AGENT_WALLET};

#[derive(Debug, Parser)]
#[command(
    name = "validation-agent",
    version,
    about = "Datachain Rope ValidationAgent — post-quantum knot signature verifier and \
             cord-anchor witness. One of the five canonical AI testimony agents."
)]
struct Cli {
    /// JSON-RPC URL of the local rope-node.
    #[arg(long, default_value = validation_agent::config::DEFAULT_RPC_URL, env = "ROPE_RPC_URL")]
    rpc_url: String,

    /// Polling interval (seconds).
    #[arg(
        long,
        default_value_t = validation_agent::config::DEFAULT_POLL_INTERVAL_SECS,
        env = "ROPE_VALIDATION_POLL_SECS"
    )]
    poll_interval_secs: u64,

    /// Maximum cord anchor knots scanned per poll tick.
    #[arg(
        long,
        default_value_t = validation_agent::config::DEFAULT_MAX_KNOTS_PER_TICK,
        env = "ROPE_VALIDATION_MAX_PER_TICK"
    )]
    max_knots_per_tick: u64,

    /// HTTP request timeout for individual RPC calls (seconds).
    #[arg(
        long,
        default_value_t = validation_agent::config::DEFAULT_RPC_TIMEOUT_SECS,
        env = "ROPE_VALIDATION_RPC_TIMEOUT_SECS"
    )]
    rpc_timeout_secs: u64,

    /// Path to the agent's signing key (32-byte BLAKE3 seed). When
    /// absent, an ephemeral in-memory key is generated — this is
    /// fine for dev and CI, NOT for production.
    #[arg(long, env = "ROPE_VALIDATION_KEY_PATH")]
    key_path: Option<PathBuf>,

    /// Wallet address used as the testimony submitter.
    #[arg(long, default_value = VALIDATION_AGENT_WALLET, env = "ROPE_VALIDATION_WALLET")]
    wallet_address: String,

    /// Only validate cord anchor knots. (default true; pass
    /// `--no-anchor-only` to attempt entity-string scanning, which
    /// in v0.1 logs a warning and falls through to anchor-only
    /// behaviour — see crate-level scope note.)
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    anchor_only: bool,

    /// Run a single tick and exit (CI smoke test).
    #[arg(long)]
    single_tick: bool,
}

impl From<Cli> for ValidationAgentConfig {
    fn from(cli: Cli) -> Self {
        Self {
            rpc_url: cli.rpc_url,
            poll_interval: Duration::from_secs(cli.poll_interval_secs),
            max_knots_per_tick: cli.max_knots_per_tick,
            rpc_timeout: Duration::from_secs(cli.rpc_timeout_secs),
            key_path: cli.key_path,
            wallet_address: cli.wallet_address,
            anchor_only: cli.anchor_only,
            single_tick: cli.single_tick,
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("RUST_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,validation_agent=debug"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .try_init();
}

fn load_signer(key_path: Option<&PathBuf>) -> Result<Arc<rope_crypto::hybrid::HybridSigner>> {
    use rope_crypto::hybrid::HybridSigner;
    let signer = match key_path {
        Some(path) => {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading agent key file {}", path.display()))?;
            anyhow::ensure!(
                bytes.len() == 32,
                "agent key file {} must be exactly 32 bytes (a BLAKE3 seed); got {}",
                path.display(),
                bytes.len()
            );
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&bytes);
            let (signer, _pk) = HybridSigner::from_seed(&seed);
            tracing::info!(
                target: "validation_agent::main",
                path = %path.display(),
                "loaded agent signing key from disk",
            );
            signer
        }
        None => {
            tracing::warn!(
                target: "validation_agent::main",
                "no --key-path provided; using ephemeral in-memory hybrid key. \
                 Testimony pubkey will not persist across restarts. \
                 Provide --key-path for production.",
            );
            HybridSigner::generate().0
        }
    };
    Ok(Arc::new(signer))
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let signer = load_signer(cli.key_path.as_ref())?;
    let config = ValidationAgentConfig::from(cli);

    let rpc = Arc::new(validation_agent::rpc::HttpRpcClient::new(
        config.rpc_url.clone(),
        config.rpc_timeout,
    )?);

    let agent = ValidationAgent::new(config, rpc, signer);
    agent.run().await
}
