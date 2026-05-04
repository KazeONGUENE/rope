//! `insurance-agent` — CLI binary for the Datachain Rope InsuranceAgent.
//!
//! ```text
//! insurance-agent serve \
//!     --rpc-url https://erpc.datachain.network \
//!     --tanastok-url https://tanastok.io/api/v1/tokenized-assets?limit=500 \
//!     --interval-secs 3600 \
//!     --reattest-after-secs 86400 \
//!     --agent-wallet 0x000000000000000000000000000000000000C003
//! ```
//!
//! Add `--once` to run a single pass and exit.

use clap::{Parser, Subcommand};
use std::sync::Arc;
use std::time::Duration;

use insurance_agent::{
    feeds::{naturaproof::NaturaProofStubFeed, tanastok::TanastokFeed, AssetFeed},
    InsuranceAgent, InsuranceAgentConfig,
};

#[derive(Parser, Debug)]
#[command(
    name = "insurance-agent",
    version,
    about = "Datachain Rope InsuranceAgent — parametric attestations against tokenized RWAs",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the agent loop. `--once` exits after one pass.
    Serve(ServeArgs),

    /// Print the version and exit.
    Version,
}

#[derive(Parser, Debug)]
struct ServeArgs {
    /// Datachain Rope JSON-RPC endpoint used to anchor attestations via
    /// `rope_appendToLedger`.
    #[arg(long, env = "INSURANCE_RPC_URL", default_value = insurance_agent::DEFAULT_RPC_URL)]
    rpc_url: String,

    /// Tanastok tokenized-assets endpoint.
    #[arg(long, env = "INSURANCE_TANASTOK_URL", default_value = insurance_agent::DEFAULT_TANASTOK_URL)]
    tanastok_url: String,

    /// Optional NaturaProof endpoint. Stored for telemetry only — the stub
    /// feed never hits the network. TODO(naturaproof): real impl pending.
    #[arg(long, env = "INSURANCE_NATURAPROOF_URL")]
    naturaproof_url: Option<String>,

    /// Wallet that owns the agent's string. Defaults to the canonical
    /// `0x...C003`.
    #[arg(long, env = "INSURANCE_AGENT_WALLET", default_value = insurance_agent::CANONICAL_AGENT_WALLET)]
    agent_wallet: String,

    /// Reserved for future signer-aware anchoring. Today the wallet is owned
    /// by the federation node running this CLI; OES-managed signing happens
    /// at the node layer when `rope_appendToLedger` lands. The path is not
    /// dereferenced.
    #[arg(long, env = "INSURANCE_KEY_PATH")]
    key_path: Option<String>,

    /// Refresh cadence for the asset list (seconds). Default: 1h.
    #[arg(long, env = "INSURANCE_INTERVAL_SECS", default_value_t = 3600)]
    interval_secs: u64,

    /// Skip an asset whose most recent attestation is younger than this
    /// (seconds). Default: 24h.
    #[arg(long, env = "INSURANCE_REATTEST_AFTER_SECS", default_value_t = 86_400)]
    reattest_after_secs: u64,

    /// HTTP timeout for both feed fetches and anchor calls (seconds).
    #[arg(long, env = "INSURANCE_HTTP_TIMEOUT_SECS", default_value_t = 30)]
    http_timeout_secs: u64,

    /// Validity window encoded into each attestation (seconds). Default: 7d.
    #[arg(long, env = "INSURANCE_ATTESTATION_VALIDITY_SECS", default_value_t = 7 * 86_400)]
    attestation_validity_secs: u64,

    /// Disable the Tanastok feed (useful for local dev when the public API
    /// is unreachable).
    #[arg(long, default_value_t = false)]
    no_tanastok: bool,

    /// Run a single pass and exit.
    #[arg(long, default_value_t = false)]
    once: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing();

    match cli.command {
        Command::Version => {
            println!("insurance-agent {}", insurance_agent::VERSION);
            Ok(())
        }
        Command::Serve(args) => serve(args).await,
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,insurance_agent=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init();
}

async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    let cfg = InsuranceAgentConfig {
        rpc_url: args.rpc_url.clone(),
        tanastok_url: args.tanastok_url.clone(),
        agent_wallet: args.agent_wallet.clone(),
        agent_id: insurance_agent::CANONICAL_AGENT_ID.to_string(),
        interval: Duration::from_secs(args.interval_secs),
        reattest_after: Duration::from_secs(args.reattest_after_secs),
        http_timeout: Duration::from_secs(args.http_timeout_secs),
        attestation_validity: Duration::from_secs(args.attestation_validity_secs),
        run_once: args.once,
    };

    if args.key_path.is_some() {
        tracing::warn!(
            "--key-path is currently unused: anchoring is performed via \
             rope_appendToLedger on the federation node owning the agent wallet. \
             Standalone signer support is tracked as future work."
        );
    }

    let mut feeds: Vec<Arc<dyn AssetFeed>> = Vec::new();
    if !args.no_tanastok {
        feeds.push(Arc::new(TanastokFeed::new(
            cfg.tanastok_url.clone(),
            cfg.http_timeout,
        )?));
    }
    feeds.push(Arc::new(match args.naturaproof_url {
        Some(url) => NaturaProofStubFeed::with_endpoint(url),
        None => NaturaProofStubFeed::new(),
    }));

    let agent = InsuranceAgent::from_config(cfg, feeds)?;
    agent.run().await
}
