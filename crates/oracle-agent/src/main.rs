//! `oracle-agent` — CLI binary entry point.
//!
//! Spins the [`OracleAgent`] control loop. CLI flags override environment
//! variables override defaults (per [`clap`]'s `env` feature).

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use oracle_agent::{
    AgentConfig, OracleAgent, SigningMode, TestimonySigner, ORACLE_AGENT_NAME,
    ORACLE_TESTIMONY_SCHEMA,
};

#[derive(Debug, Parser)]
#[command(
    name = "oracle-agent",
    about = "Datachain Rope OracleAgent — DC FAT and stablecoin price testimony anchoring",
    long_about = None,
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Polling interval in seconds.
    #[arg(
        long,
        env = "ORACLE_INTERVAL_SECS",
        default_value_t = oracle_agent::config::DEFAULT_INTERVAL_SECS
    )]
    interval_secs: u64,

    /// Local rope-node JSON-RPC URL.
    #[arg(
        long,
        env = "ORACLE_RPC_URL",
        default_value = oracle_agent::config::DEFAULT_RPC_URL
    )]
    rpc_url: String,

    /// Canonical price feed URL (defaults to dcswap.net).
    #[arg(
        long,
        env = "ORACLE_FEED_URL",
        default_value = oracle_agent::config::DEFAULT_FEED_URL
    )]
    feed_url: String,

    /// Wallet hex (0x-prefixed) the OracleAgent anchors testimonies on.
    #[arg(
        long,
        env = "ORACLE_WALLET_HEX",
        default_value = oracle_agent::config::DEFAULT_WALLET_HEX
    )]
    wallet_hex: String,

    /// Path to a 32-byte raw key seed file. When omitted an ephemeral
    /// keypair is generated at startup (NOT recommended for production).
    #[arg(long, env = "ORACLE_KEY_PATH")]
    key_path: Option<PathBuf>,

    /// Signing mode (`hybrid` or `ed25519-only`). `hybrid` is required for
    /// production.
    #[arg(long, env = "ORACLE_SIGNING_MODE", default_value_t = SigningMode::Hybrid)]
    signing_mode: SigningMode,

    /// Disable the startup `rope_createPersonalLedger` call (for use when
    /// the ledger is known to exist already).
    #[arg(long, env = "ORACLE_NO_AUTO_CREATE_LEDGER")]
    no_auto_create_ledger: bool,

    /// HTTP request timeout for the feed fetch, in seconds.
    #[arg(
        long,
        env = "ORACLE_FEED_TIMEOUT_SECS",
        default_value_t = oracle_agent::config::DEFAULT_FEED_TIMEOUT_SECS
    )]
    feed_timeout_secs: u64,

    /// HTTP request timeout for the JSON-RPC anchor call, in seconds.
    #[arg(
        long,
        env = "ORACLE_RPC_TIMEOUT_SECS",
        default_value_t = oracle_agent::config::DEFAULT_RPC_TIMEOUT_SECS
    )]
    rpc_timeout_secs: u64,

    /// Maximum retries (per cycle) for both the feed fetch and the anchor.
    #[arg(long, env = "ORACLE_MAX_RETRIES", default_value_t = 4)]
    max_retries: u32,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate a new 32-byte agent key seed and write it to `path`.
    /// The seed is the input to `KeyStore::from_seed` — losing it loses the
    /// agent's identity.
    InitKey {
        #[arg(long)]
        path: PathBuf,
    },

    /// Run a single anchor cycle against the configured feed + RPC and
    /// exit. Useful for smoke-testing in CI.
    RunOnce,

    /// Print the agent's stable identity (Ed25519 pk + node id) without
    /// touching the network.
    Whoami,
}

fn build_config(cli: &Cli) -> AgentConfig {
    AgentConfig {
        interval: Duration::from_secs(cli.interval_secs.max(1)),
        rpc_url: cli.rpc_url.clone(),
        feed_url: cli.feed_url.clone(),
        wallet_hex: cli.wallet_hex.clone(),
        key_path: cli.key_path.clone(),
        signing_mode: cli.signing_mode,
        auto_create_ledger: !cli.no_auto_create_ledger,
        feed_timeout: Duration::from_secs(cli.feed_timeout_secs.max(1)),
        rpc_timeout: Duration::from_secs(cli.rpc_timeout_secs.max(1)),
        max_retries: cli.max_retries,
        ..AgentConfig::default()
    }
}

fn build_signer(cfg: &AgentConfig) -> anyhow::Result<TestimonySigner> {
    if let Some(path) = &cfg.key_path {
        let signer = TestimonySigner::from_seed_file(path, cfg.signing_mode)?;
        tracing::info!(
            target: "oracle_agent::main",
            key_path = %path.display(),
            ed25519_pk = %signer.ed25519_public_key_hex(),
            "loaded persistent key"
        );
        Ok(signer)
    } else {
        let signer = TestimonySigner::ephemeral(cfg.signing_mode);
        tracing::warn!(
            target: "oracle_agent::main",
            ed25519_pk = %signer.ed25519_public_key_hex(),
            "no --key-path supplied; using ephemeral keypair (testimonies will not survive restart)"
        );
        Ok(signer)
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_env("ORACLE_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info,oracle_agent=info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true).with_level(true))
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();

    if let Some(Command::InitKey { path }) = &cli.command {
        let signer = TestimonySigner::generate_seed_file(path, cli.signing_mode)?;
        println!(
            "wrote new {} seed to {}\nagent ed25519 pk: 0x{}\nagent node id:    0x{}",
            cli.signing_mode,
            path.display(),
            signer.ed25519_public_key_hex(),
            signer.node_id_hex()
        );
        return Ok(());
    }

    let cfg = build_config(&cli);
    cfg.validate()?;
    let signer = build_signer(&cfg)?;

    if let Some(Command::Whoami) = &cli.command {
        println!(
            "{} ({})\n  schema:        {}\n  wallet:        {}\n  ed25519 pk:    0x{}\n  node id:       0x{}\n  signing mode:  {}",
            ORACLE_AGENT_NAME,
            cfg.wallet_hex,
            ORACLE_TESTIMONY_SCHEMA,
            cfg.wallet_hex,
            signer.ed25519_public_key_hex(),
            signer.node_id_hex(),
            cfg.signing_mode
        );
        return Ok(());
    }

    let agent = OracleAgent::new(cfg, signer)?;

    if let Some(Command::RunOnce) = &cli.command {
        if let Err(e) = agent.ensure_ledger().await {
            tracing::warn!(target: "oracle_agent::main", error = %e, "ensure_ledger failed (continuing)");
        }
        let outcome = agent.run_once().await?;
        println!(
            "anchored knot {} ({} pieces) — fat=${:.6} mech={}",
            outcome.anchor.knot_string_id,
            outcome.anchor.piece_count,
            outcome.testimony.fat_price_usd,
            outcome.testimony.mechanism_version
        );
        return Ok(());
    }

    let cancel = build_shutdown_signal();
    agent.run(Box::pin(cancel)).await;
    Ok(())
}

/// Compose SIGINT + SIGTERM into a single future the loop can `select!` on.
async fn build_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT, shutting down");
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, shutting down");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("received Ctrl+C, shutting down");
    }
}
